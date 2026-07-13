use super::{App, HighlightSaveState};
use crate::capture::audio::{enumerate_app_audio_sessions, enumerate_audio_devices};
use crate::config::Config;
use crate::i18n::Strings;
use crate::session::{EncodeSettings, state::SessionAction};
use crate::sources::enumerate_sources;
use crate::types::{AppAudioSource, CaptureSource};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

/// `enumerate_app_audio_sessions()` plus a synthetic entry (`process_ids:
/// vec![]`) for every `Config::registered_app_audio` app that isn't
/// currently live -- so a pinned app still shows (greyed, see
/// `render_audio_popup`) even while not producing sound, and its icon can
/// still be shown via the path it was registered with.
pub(super) fn merge_registered_app_audio(
    mut sources: Vec<AppAudioSource>,
    config: &Config,
) -> Vec<AppAudioSource> {
    for reg in &config.registered_app_audio {
        if sources.iter().any(|s| s.exe_name == reg.exe_name) {
            continue;
        }
        let display_name = reg
            .exe_name
            .strip_suffix(".exe")
            .or_else(|| reg.exe_name.strip_suffix(".EXE"))
            .unwrap_or(&reg.exe_name)
            .to_string();
        sources.push(AppAudioSource {
            process_ids: Vec::new(),
            exe_name: reg.exe_name.clone(),
            display_name,
            icon_rgba: crate::sources::extract_exe_icon_rgba(&reg.exe_path),
        });
    }
    sources
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
            self.selected_audio = new_audio_devices.iter().map(|d| d.is_loopback).collect();
            self.audio_devices = new_audio_devices;
            // export_track_selection is NOT reset here -- same reasoning as the
            // old Refresh button: it's tied to the last finished recording's
            // actually-probed track count, not the live device list.
        }

        let new_app_audio_sources = merge_registered_app_audio(
            enumerate_app_audio_sessions().unwrap_or_default(),
            &self.config,
        );
        if app_audio_fingerprint(&new_app_audio_sources)
            != app_audio_fingerprint(&self.app_audio_sources)
        {
            // Preserved by exe name, not list index or process id -- a
            // refresh can legitimately re-enumerate the same still-running
            // app at the same PID (no change needed), but also survives the
            // app having been closed and reopened (new PID) between
            // refreshes without silently unchecking it. An exe name that
            // already existed keeps whatever checked state it had
            // (including a manual uncheck, even across it going idle<->live)
            // -- only a genuinely brand-new exe name gets a fresh default,
            // which is true for a registered app (registering one implies
            // "yes, record it") and false otherwise, matching the
            // already-existing default for any newly-appearing live app.
            let previous_checked_by_exe: std::collections::HashMap<&str, bool> = self
                .app_audio_sources
                .iter()
                .zip(self.selected_app_audio.iter())
                .map(|(s, &sel)| (s.exe_name.as_str(), sel))
                .collect();
            let registered_exe_names: std::collections::HashSet<&str> = self
                .config
                .registered_app_audio
                .iter()
                .map(|r| r.exe_name.as_str())
                .collect();
            self.selected_app_audio = new_app_audio_sources
                .iter()
                .map(|s| {
                    previous_checked_by_exe
                        .get(s.exe_name.as_str())
                        .copied()
                        .unwrap_or_else(|| registered_exe_names.contains(s.exe_name.as_str()))
                })
                .collect();
            self.app_audio_sources = new_app_audio_sources;
            self.app_audio_icon_textures.clear();
        }
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
        let selected_devices: Vec<_> = self
            .audio_devices
            .iter()
            .zip(self.selected_audio.iter())
            .filter(|&(_, &sel)| sel)
            .map(|(dev, _)| dev.clone())
            .collect();
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
        let encode = EncodeSettings {
            codec: self.config.encode.codec.clone(),
            fps: self.config.encode.fps,
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
        let selected_devices: Vec<_> = self
            .audio_devices
            .iter()
            .zip(self.selected_audio.iter())
            .filter(|&(_, &sel)| sel)
            .map(|(dev, _)| dev.clone())
            .collect();
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
            fps: self.config.encode.fps,
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
