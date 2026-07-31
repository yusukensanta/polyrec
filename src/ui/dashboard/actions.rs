use super::widgets::audio_device_icon;
use super::{App, HighlightSaveState};
use crate::capture::audio::{enumerate_app_audio_sessions, enumerate_audio_devices};
use crate::config::Config;
use crate::i18n::Strings;
use crate::session::{EncodeSettings, state::SessionAction};
use crate::sources::enumerate_sources;
use crate::types::{AppAudioSource, AudioDevice, CaptureSource};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

/// The Applications audio list is entirely curated via `Config::register_app_audio`
/// / `unregister_app_audio` ("+ Add app", or unchecking a row -- see
/// `render_audio_popup`) -- unlike the SYSTEM device list, it does NOT
/// auto-populate from whatever happens to be making sound right now. A
/// fresh install (nothing registered yet) shows nothing here but the "+ Add
/// app" button, regardless of how many apps are currently producing audio.
///
/// For each registered app, looks up its current live session (if any) to
/// get real `process_ids` and an up-to-date icon; a registered app that
/// isn't currently running gets a synthetic `process_ids: vec![]` entry
/// (icon re-extracted from the path it was registered with) so it still
/// shows and can still be un-registered even while idle.
pub(super) fn build_app_audio_sources(config: &Config) -> Vec<AppAudioSource> {
    let live = enumerate_app_audio_sessions().unwrap_or_default();
    config
        .registered_app_audio
        .iter()
        .map(|reg| {
            if let Some(matched) = live.iter().find(|s| s.exe_name == reg.exe_name) {
                matched.clone()
            } else {
                AppAudioSource {
                    process_ids: Vec::new(),
                    exe_name: reg.exe_name.clone(),
                    display_name: crate::sources::display_name_from_exe_name(&reg.exe_name),
                    icon_rgba: crate::sources::extract_exe_icon_rgba(&reg.exe_path),
                }
            }
        })
        .collect()
}

/// Which of `devices`' checkboxes should start checked -- `config`'s saved
/// selection (`Config::selected_audio_device_ids`) if the user has ever
/// changed one, matched by the device's stable WASAPI endpoint id so a
/// reconnected device (unplugged mic, sleeping Bluetooth headset) keeps
/// its remembered state; otherwise falls back to the original
/// loopback-only default (see the call sites' doc comments for why that's
/// the right first-run default). Shared by `App::new` (initial load) and
/// `refresh_sources_and_audio_if_due` (device set changed) so both apply
/// the exact same precedence.
pub(super) fn resolve_selected_audio(devices: &[AudioDevice], config: &Config) -> Vec<bool> {
    match &config.selected_audio_device_ids {
        Some(ids) => devices.iter().map(|d| ids.contains(&d.id)).collect(),
        None => devices.iter().map(|d| d.is_loopback).collect(),
    }
}

/// Order-independent fingerprint of an app-audio list, for deciding whether
/// `refresh_sources_and_audio_if_due` needs to rebuild anything -- comparing
/// just the set of exe names misses a registered app transitioning from
/// idle (`process_ids: []`) to live (real pids) or back, since its exe name
/// doesn't change either way but its `process_ids` do.
fn app_audio_fingerprint(sources: &[AppAudioSource]) -> Vec<(String, Vec<u32>)> {
    let mut v: Vec<(String, Vec<u32>)> = sources
        .iter()
        .map(|s| {
            let mut pids = s.process_ids.clone();
            pids.sort_unstable();
            (s.exe_name.clone(), pids)
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

/// Display label for each audio track a recording with these sources will
/// produce, in the same order `session::start_capture` assigns track ids
/// (devices first, then app-audio sources) -- see
/// `App::last_recording_audio_labels`'s field doc for why the export dialog
/// needs this captured at record-start time instead of reading it back from
/// the live device list.
///
/// One app source can expand to several tracks: `session::start_capture`
/// spawns one capture task (and track id) per entry in
/// `AppAudioSource::process_ids`, not one per source -- two genuinely
/// independent top-level process trees of the same exe (e.g. two separate
/// Chrome windows/profiles launched independently) each get their own track.
/// A single app's own parent/child helper processes (e.g. Discord's
/// GPU/renderer/utility processes) are already collapsed to one entry by
/// `capture::audio::enumerate_app_audio_sessions`'s canonical_root_pid, so
/// they don't multiply this list. Repeating the display
/// name `process_ids.len()` times (zero for a registered-but-inactive
/// source) keeps this list's length and order matching the real tracks
/// exactly; emitting one label per source regardless of process count would
/// desync every label after the first multi-process source from its actual
/// track.
fn build_audio_labels(devices: &[AudioDevice], app_sources: &[AppAudioSource]) -> Vec<String> {
    devices
        .iter()
        .map(|dev| format!("{} {}", audio_device_icon(dev), dev.name))
        .chain(
            app_sources.iter().flat_map(|src| {
                std::iter::repeat_n(src.display_name.clone(), src.process_ids.len())
            }),
        )
        .collect()
}

impl App {
    pub(super) fn handle_pause_button(&mut self) {
        if self.session.is_recording() {
            self.session.pause_capture();
        } else if self.session.is_paused() {
            self.session.resume_capture();
        }
    }

    pub(super) fn handle_rec_button(&mut self, is_recording: bool) {
        let is_paused = self.session.is_paused();
        if is_recording || is_paused {
            self.stop_recording();
        } else if let Some(idx) = self.selected_source {
            let Some(source) = self.sources.get(idx).cloned() else {
                self.selected_source = None;
                return;
            };
            self.start_recording_with_source(source);
        }
    }

    /// F9 (or whatever start/stop hotkey is configured) works globally, without the
    /// PolyRec window needing focus — so unlike the REC button, it can't rely on a
    /// manual source-list selection. It captures whatever window is currently in the
    /// foreground instead, so "press hotkey, record what I'm doing right now" works
    /// while alt-tabbed into a game with the dashboard never brought to front.
    pub(super) fn handle_hotkey_start_stop(&mut self, is_recording: bool) {
        let is_paused = self.session.is_paused();
        if is_recording || is_paused {
            self.stop_recording();
        } else if let Some(source) = foreground_capture_source() {
            self.start_recording_with_source(source);
        } else {
            tracing::warn!(
                "start/stop hotkey pressed but the foreground window isn't capturable (or it's PolyRec itself)"
            );
        }
    }

    /// Refreshes `free_space_bytes` for `config.output_dir`'s volume, at most
    /// every `FREE_SPACE_CHECK_INTERVAL` -- or immediately if the output dir
    /// itself changed since the last check (typing a new path, or Browse...),
    /// so switching drives doesn't show a stale number from the old one.
    pub(super) fn refresh_free_space(&mut self) {
        const FREE_SPACE_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

        let dir_changed =
            self.free_space_checked_dir.as_deref() != Some(self.config.output_dir.as_path());
        let due = self
            .free_space_checked_at
            .is_none_or(|t| t.elapsed() >= FREE_SPACE_CHECK_INTERVAL);
        if !dir_changed && !due {
            return;
        }

        self.free_space_bytes = crate::disk_space::free_bytes(&self.config.output_dir).ok();
        self.free_space_checked_at = Some(Instant::now());
        self.free_space_checked_dir = Some(self.config.output_dir.clone());
    }

    /// Keeps the source/audio-device lists live without a manual Refresh
    /// button, at most every `SOURCES_CHECK_INTERVAL`. Two things this
    /// deliberately does NOT do, both found while designing this:
    ///
    /// - Replace `self.sources` on every check, unconditionally. Window
    ///   Z-order shifts every time the user alt-tabs, even when nothing
    ///   opened or closed -- unconditionally replacing the list would make
    ///   entries visibly reorder while the user is trying to click one. Only
    ///   replace it (and only then clear/reload icon textures) when the SET
    ///   of window handles actually changed.
    /// - Reset `selected_audio` on every check. The old manual Refresh
    ///   button did this unconditionally (fine for a deliberate click); doing
    ///   it silently every couple of seconds would stomp on audio tracks the
    ///   user just manually checked. Only reset it when the SET of device
    ///   IDs actually changed (a device was plugged/unplugged).
    ///
    /// Both preserve the current selection by hwnd, same as the old Refresh
    /// button did.
    pub(super) fn refresh_sources_and_audio_if_due(&mut self) {
        const SOURCES_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
        if self
            .sources_checked_at
            .is_some_and(|t| t.elapsed() < SOURCES_CHECK_INTERVAL)
        {
            return;
        }
        self.sources_checked_at = Some(Instant::now());

        let new_sources = enumerate_sources();
        let current_hwnds: std::collections::HashSet<usize> =
            self.sources.iter().map(|s| s.hwnd).collect();
        let new_hwnds: std::collections::HashSet<usize> =
            new_sources.iter().map(|s| s.hwnd).collect();
        if new_hwnds != current_hwnds {
            let previously_selected_hwnd = self
                .selected_source
                .and_then(|i| self.sources.get(i))
                .map(|src| src.hwnd);
            self.sources = new_sources;
            self.source_icon_textures.clear();
            self.selected_source = previously_selected_hwnd
                .and_then(|hwnd| self.sources.iter().position(|src| src.hwnd == hwnd));
        }

        let new_audio_devices = enumerate_audio_devices().unwrap_or_default();
        let current_ids: std::collections::HashSet<&str> =
            self.audio_devices.iter().map(|d| d.id.as_str()).collect();
        let new_ids: std::collections::HashSet<&str> =
            new_audio_devices.iter().map(|d| d.id.as_str()).collect();
        if new_ids != current_ids {
            self.selected_audio = resolve_selected_audio(&new_audio_devices, &self.config);
            self.audio_devices = new_audio_devices;
            // export_track_selection is NOT reset here -- same reasoning as the
            // old Refresh button: it's tied to the last finished recording's
            // actually-probed track count, not the live device list.
        }

        // Every entry the list can ever contain is a registered app (see
        // `build_app_audio_sources`'s doc comment), so there's no separate
        // "checked" state to preserve across a rebuild the way SYSTEM
        // devices have -- being in the list at all already means "record
        // this", always. Only the live process_ids (idle vs running) can
        // change between refreshes.
        let new_app_audio_sources = build_app_audio_sources(&self.config);
        if app_audio_fingerprint(&new_app_audio_sources)
            != app_audio_fingerprint(&self.app_audio_sources)
        {
            self.selected_app_audio = vec![true; new_app_audio_sources.len()];
            self.app_audio_sources = new_app_audio_sources;
            self.app_audio_icon_textures.clear();
        }
    }

    /// Saves the SYSTEM device checkboxes' current state to `config` so it
    /// survives an app restart -- called right after a checkbox toggle in
    /// the Audio popup, same "checkbox click is the persistence trigger"
    /// pattern as `Config::register_app_audio`/`unregister_app_audio` for
    /// the Applications list, just keyed by device-id membership in
    /// `Config::selected_audio_device_ids` instead of a separate list, since
    /// every device (not just ones the user cares about) is always present
    /// in `self.audio_devices`.
    pub(super) fn persist_selected_audio_devices(&mut self, s: &'static Strings) {
        let ids: Vec<String> = self
            .audio_devices
            .iter()
            .zip(self.selected_audio.iter())
            .filter(|&(_, &selected)| selected)
            .map(|(d, _)| d.id.clone())
            .collect();
        self.config.selected_audio_device_ids = Some(ids);
        if let Err(e) = self.config.save() {
            tracing::error!("failed to save config: {e}");
            self.error_message = Some(format!("{}{e}", s.config_save_failed_prefix));
        }
    }

    /// Rebuilds the Applications audio list from `self.config` immediately
    /// -- called right after a register/unregister action so the popup
    /// reflects it the same frame, instead of waiting for
    /// `refresh_sources_and_audio_if_due`'s next timer tick (which also
    /// wouldn't notice a registration change on its own: unregistering an
    /// app that's still running doesn't change its live `process_ids`, the
    /// thing that poll's fingerprint actually watches for).
    pub(super) fn rebuild_app_audio_sources_now(&mut self) {
        self.app_audio_sources = build_app_audio_sources(&self.config);
        self.selected_app_audio = vec![true; self.app_audio_sources.len()];
        self.app_audio_icon_textures.clear();
    }

    pub(super) fn stop_recording(&mut self) {
        let path = self.session.active.as_ref().map(|a| a.output_path.clone());
        if let Some(p) = &path {
            tracing::info!("recording stop requested, finalizing: {}", p.display());
        }
        let disk_full = self
            .session
            .active
            .as_ref()
            .map(|a| Arc::clone(&a.disk_full_flag));
        self.session.apply(SessionAction::Stop);
        self.finalizing_handle = self.session.stop_capture();
        self.finalizing_path = path;
        self.finalizing_disk_full = disk_full;
        self.recording_start = None;
        self.frame_count.store(0, Ordering::Relaxed);
        // export_track_selection/export_available_tracks are set once finalize
        // actually succeeds and the file can be probed (poll_background_work) --
        // not here, since what's selected pre-recording isn't a guarantee of
        // what ends up in the file.
    }

    pub(super) fn start_recording_with_source(&mut self, source: CaptureSource) {
        let source_title = source.window_title.clone();
        let app_name = crate::session::app_name_from_exe(&source.exe_name);
        let mut selected_devices: Vec<_> = self
            .audio_devices
            .iter()
            .zip(self.selected_audio.iter())
            .filter(|&(_, &sel)| sel)
            .map(|(dev, _)| dev.clone())
            .collect();
        // The MP4 container's physical track order doesn't reliably follow
        // AddStream() call order for a live (real-time-encoded) recording --
        // see writer.rs/remux.rs -- but process-loopback (app-audio) tracks
        // still consistently land after device tracks in practice, since
        // activating one needs an async WASAPI round-trip a plain device
        // capture doesn't, so it starts flushing samples later regardless of
        // call order. That leaves device-vs-device order as the one lever
        // actually worth pulling: putting loopback (system/game audio, what
        // users overwhelmingly want as "the" track) ahead of the mic
        // improves the odds it's what a player defaults to without explicit
        // track selection, even though nothing here can fully guarantee it.
        selected_devices.sort_by_key(|dev| !dev.is_loopback);
        let selected_gains: Vec<f32> = selected_devices
            .iter()
            .map(|dev| self.config.audio_gain(&dev.id))
            .collect();
        let selected_app_sources: Vec<_> = self
            .app_audio_sources
            .iter()
            .zip(self.selected_app_audio.iter())
            .filter(|&(_, &sel)| sel)
            .map(|(src, _)| src.clone())
            .collect();
        let selected_app_gains: Vec<f32> = selected_app_sources
            .iter()
            .map(|src| self.config.app_audio_gain(&src.exe_name))
            .collect();
        let track_count = selected_devices.len() + selected_app_sources.len();
        let audio_labels = build_audio_labels(&selected_devices, &selected_app_sources);
        let encode = EncodeSettings {
            codec: self.config.encode.codec.clone(),
            fps: self.config.encode.fps(),
            resolution_mode: self.config.encode.resolution_mode(),
            bitrate_mode: self.config.encode.bitrate_mode(),
            encoder_mode: self.config.encode.encoder_mode(),
        };
        // Only transition to the Recording state once start_capture actually
        // succeeds -- otherwise a disk-full refusal would leave the UI showing
        // "Recording" for a capture that never started.
        match self.session.start_capture(
            source,
            selected_devices,
            selected_gains,
            selected_app_sources,
            selected_app_gains,
            self.app_audio_only,
            self.show_recording_border,
            Arc::clone(&self.frame_count),
            &self.config.output_dir,
            encode,
        ) {
            Ok(path) => {
                tracing::info!(
                    "recording started: source={source_title:?} audio_tracks={track_count} output={}",
                    path.display()
                );
                self.session.apply(SessionAction::Start);
                self.recording_start = Some(Instant::now());
                self.last_recording_audio_labels = audio_labels;
                self.last_recording_app_name = app_name;
            }
            Err(e) => {
                tracing::error!("start_capture failed for source={source_title:?}: {e}");
                let prefix = self.config.lang().strings().couldnt_start_recording_prefix;
                self.error_message = Some(format!("{prefix}{e}"));
            }
        }
    }

    /// Keeps the Highlight background buffer's running/not-running state and
    /// target window in sync with `config.highlight.enabled`, whether a
    /// manual recording is active, and the live foreground window. Runs every
    /// frame; every check here is a cheap comparison against already-known
    /// state, not an unconditional capture-thread spawn.
    pub(super) fn refresh_highlight_buffering(&mut self) {
        // The highlight actor stopped itself early (e.g. disk ran low) --
        // mirrors how poll_background_work detects manual recording's
        // recorder stopping itself early via `recorder_handle.is_finished()`.
        if self.session.highlight_actor_finished() {
            let disk_full = self
                .session
                .highlight
                .as_ref()
                .is_some_and(|h| h.disk_full_flag.load(Ordering::Relaxed));
            self.session.stop_highlight_buffering(true);
            if disk_full {
                let strings = self.config.lang().strings();
                self.error_message = Some(strings.highlight_disk_full_message.to_string());
            }
        }

        if !self.config.highlight.enabled {
            if self.session.is_highlighting() {
                self.session.stop_highlight_buffering(true);
            }
            return;
        }

        // Highlight buffering and manual recording never run at once --
        // doubling GPU/encode load for no benefit while the thing the user
        // actually pressed record for is what matters. `discard: false` here
        // (not `true`) is just about not wastefully deleting files that are
        // about to get cleaned up anyway the moment buffering resumes for
        // whatever's foreground once the recording stops (see
        // `start_highlight_buffering`'s stale-file cleanup) -- buffering
        // effectively restarts fresh after a manual recording, it does not
        // carry the pre-recording buffer forward.
        if self.session.is_recording() || self.session.is_paused() {
            if self.session.is_highlighting() {
                self.session.stop_highlight_buffering(false);
            }
            return;
        }

        let Some(source) = foreground_capture_source() else {
            // Not a real switch away -- e.g. focus is briefly on PolyRec's
            // own window while the user changes a setting. Leave whatever's
            // already running (if anything) alone; WGC keeps capturing a
            // window that isn't foreground just fine.
            return;
        };

        let following_different_window = self
            .session
            .highlight_hwnd()
            .is_some_and(|hwnd| hwnd != source.hwnd);
        if following_different_window {
            // Different app/resolution -- can't concatenate across the
            // switch (see encode::highlight_export), so start over.
            self.session.stop_highlight_buffering(true);
        }

        // Re-using the already-throttled free-space reading (refreshed a few
        // lines earlier in poll_background_work) instead of a fresh syscall
        // every frame -- also naturally rate-limits retrying after a
        // disk-full stop to that same ~3s cadence instead of every frame.
        let has_room = self
            .free_space_bytes
            .is_none_or(|f| f >= crate::disk_space::MIN_FREE_BYTES);
        if !self.session.is_highlighting() && has_room {
            self.start_highlight_buffering_for(source);
        }
    }

    pub(super) fn start_highlight_buffering_for(&mut self, source: CaptureSource) {
        let mut selected_devices: Vec<_> = self
            .audio_devices
            .iter()
            .zip(self.selected_audio.iter())
            .filter(|&(_, &sel)| sel)
            .map(|(dev, _)| dev.clone())
            .collect();
        // See start_recording_with_source's identical sort for why.
        selected_devices.sort_by_key(|dev| !dev.is_loopback);
        let selected_gains: Vec<f32> = selected_devices
            .iter()
            .map(|dev| self.config.audio_gain(&dev.id))
            .collect();
        let selected_app_sources: Vec<_> = self
            .app_audio_sources
            .iter()
            .zip(self.selected_app_audio.iter())
            .filter(|&(_, &sel)| sel)
            .map(|(src, _)| src.clone())
            .collect();
        let selected_app_gains: Vec<f32> = selected_app_sources
            .iter()
            .map(|src| self.config.app_audio_gain(&src.exe_name))
            .collect();
        let encode = EncodeSettings {
            codec: self.config.encode.codec.clone(),
            fps: self.config.encode.fps(),
            resolution_mode: self.config.encode.resolution_mode(),
            bitrate_mode: self.config.encode.bitrate_mode(),
            encoder_mode: self.config.encode.encoder_mode(),
        };
        let buffer_seconds = self.config.highlight.buffer_seconds.clamp(
            crate::config::HIGHLIGHT_BUFFER_SECONDS_MIN,
            crate::config::HIGHLIGHT_BUFFER_SECONDS_MAX,
        );
        if let Err(e) = self.session.start_highlight_buffering(
            &source,
            selected_devices,
            selected_gains,
            selected_app_sources,
            selected_app_gains,
            self.app_audio_only,
            &self.config.output_dir,
            encode,
            buffer_seconds,
        ) {
            tracing::warn!("failed to start Highlight buffering: {e}");
        }
    }

    /// Forces the current segment to finalize and saves the buffer to a file
    /// -- a no-op (with a status message, not a popup, per the established
    /// export-UI convention) if Highlight buffering isn't currently active.
    pub(super) fn handle_save_highlight_hotkey(&mut self, s: &'static Strings) {
        let Some(hwnd) = self.session.highlight_hwnd() else {
            self.error_message = Some(s.highlight_save_not_active.to_string());
            return;
        };
        if matches!(self.highlight_save_state, HighlightSaveState::Saving) {
            return;
        }
        let real_hwnd = windows::Win32::Foundation::HWND(hwnd as *mut core::ffi::c_void);
        let source = crate::sources::capture_source_for_hwnd(real_hwnd);
        let app_name = crate::session::app_name_from_exe(&source.exe_name);
        match self
            .session
            .save_highlight(&self.config.output_dir, &app_name)
        {
            Ok(handle) => {
                self.highlight_save_state = HighlightSaveState::Saving;
                self.highlight_save_handle = Some(handle);
            }
            Err(e) => {
                self.highlight_save_state = HighlightSaveState::Failed(e.to_string());
            }
        }
    }
}

/// The window currently in the foreground, as a `CaptureSource` — or `None` if
/// there isn't one, or it belongs to this process (PolyRec's own dashboard/overlay
/// windows), which would otherwise let a hotkey press while focused on our own
/// window "record" it instead of whatever the user actually meant to capture.
fn foreground_capture_source() -> Option<CaptureSource> {
    unsafe {
        let hwnd = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let mut pid = 0u32;
        windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == std::process::id() {
            return None;
        }
        Some(crate::sources::capture_source_for_hwnd(hwnd))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mic() -> AudioDevice {
        AudioDevice {
            id: "mic-id".into(),
            name: "Mic".into(),
            is_loopback: false,
        }
    }

    fn app_source(display_name: &str) -> AppAudioSource {
        app_source_with_pids(display_name, vec![1234])
    }

    fn app_source_with_pids(display_name: &str, process_ids: Vec<u32>) -> AppAudioSource {
        AppAudioSource {
            process_ids,
            exe_name: "game.exe".into(),
            display_name: display_name.into(),
            icon_rgba: None,
        }
    }

    #[test]
    fn build_audio_labels_orders_devices_before_app_sources() {
        let labels = build_audio_labels(&[mic()], &[app_source("Game")]);
        assert_eq!(labels, vec!["🎙 Mic".to_string(), "Game".to_string()]);
    }

    #[test]
    fn build_audio_labels_empty_when_nothing_selected() {
        assert!(build_audio_labels(&[], &[]).is_empty());
    }

    #[test]
    fn build_audio_labels_repeats_name_once_per_process_id() {
        // session::start_capture spawns one track per pid, not one per
        // AppAudioSource -- e.g. Discord's main + helper processes, or two
        // independent windows of the same exe.
        let labels = build_audio_labels(
            &[mic()],
            &[app_source_with_pids("Discord", vec![111, 222, 333])],
        );
        assert_eq!(
            labels,
            vec![
                "🎙 Mic".to_string(),
                "Discord".to_string(),
                "Discord".to_string(),
                "Discord".to_string(),
            ]
        );
    }

    #[test]
    fn build_audio_labels_contributes_nothing_for_a_registered_but_inactive_source() {
        // Empty process_ids (registered but not currently running) spawns no
        // capture task in session::start_capture, so it must contribute no
        // label either -- otherwise a later source's label would land on the
        // wrong track.
        let labels = build_audio_labels(
            &[mic()],
            &[
                app_source_with_pids("Idle App", vec![]),
                app_source_with_pids("Live App", vec![42]),
            ],
        );
        assert_eq!(labels, vec!["🎙 Mic".to_string(), "Live App".to_string()]);
    }
}
