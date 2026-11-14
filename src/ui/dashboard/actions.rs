use super::App;
use super::icons::Icon;
use super::state::{HighlightRetry, HighlightSaveState, SelfUpdateState};
use crate::capture::audio::{enumerate_app_audio_sessions, enumerate_audio_devices};
use crate::config::Config;
use crate::i18n::Strings;
use crate::recording_naming::RecordingNaming;
use crate::session::{CaptureInputs, EncodeSettings, HighlightStopped};
use crate::sources::enumerate_sources;
use crate::types::{AppAudioSource, AudioDevice, CaptureSource};
use eframe::egui;
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
///
/// Takes `&mut Config` (not just `&Config`) because the idle path can
/// self-heal `RegisteredApp::exe_path`: a Squirrel-updated app (Discord,
/// Slack, ...) moves itself into a new `app-<version>` folder and removes
/// the old one, which otherwise leaves the path pinned at registration time
/// permanently stale -- breaking icon extraction for as long as the app
/// stays idle (see `sources::resolve_possibly_stale_exe_path`). When a
/// fresher path is found, it's written back into `config` and saved
/// immediately (best-effort -- logged, not surfaced to the UI, same as
/// other non-interactive background saves in this codebase) so the walk
/// that found it doesn't need repeating on every future idle rebuild.
pub(super) fn build_app_audio_sources(config: &mut Config) -> Vec<AppAudioSource> {
    build_app_audio_sources_from(&enumerate_app_audio_sessions().unwrap_or_default(), config)
}

/// `build_app_audio_sources` over an already-taken `live` session list --
/// lets the periodic refresh enumerate sessions on its worker thread (see
/// `take_refresh_snapshot`) while the `config`-mutating merge below stays on
/// the UI thread that owns `config`.
fn build_app_audio_sources_from(
    live: &[AppAudioSource],
    config: &mut Config,
) -> Vec<AppAudioSource> {
    let mut healed_any = false;
    let sources = config
        .registered_app_audio
        .iter_mut()
        .map(|reg| {
            if let Some(matched) = live.iter().find(|s| s.exe_name == reg.exe_name) {
                matched.clone()
            } else {
                let resolved_path = crate::sources::resolve_possibly_stale_exe_path(&reg.exe_path);
                if resolved_path != reg.exe_path {
                    reg.exe_path = resolved_path.clone();
                    healed_any = true;
                }
                AppAudioSource {
                    process_ids: Vec::new(),
                    exe_name: reg.exe_name.clone(),
                    display_name: crate::sources::display_name_from_exe_name(&reg.exe_name),
                    icon_rgba: crate::sources::extract_exe_icon_rgba(&resolved_path),
                }
            }
        })
        .collect();
    if healed_any && let Err(e) = config.save() {
        tracing::error!("failed to save config after healing a stale registered app path: {e}");
    }
    sources
}

/// Which of `devices`' checkboxes should start checked: the default
/// output's (the loopback device) as `Config::records_default_output` says,
/// the default microphone's as `Config::records_default_input` says --
/// whichever devices Windows' defaults are now, so a switched-to device
/// keeps the tick its role had. Shared by `App::new` (initial load) and
/// `refresh_sources_and_audio_if_due` (device set changed) so both apply
/// the exact same rule.
pub(super) fn resolve_selected_audio(devices: &[AudioDevice], config: &Config) -> Vec<bool> {
    devices
        .iter()
        .map(|d| {
            if d.is_loopback {
                config.records_default_output()
            } else {
                config.records_default_input()
            }
        })
        .collect()
}

/// Saves which SYSTEM checkboxes are ticked (`selected`, one per `devices`)
/// to `config`: as Windows' default output or default microphone being
/// recorded, not as the devices they happen to be -- see
/// `Config::record_default_output`. A role with no device listed (no
/// microphone plugged in) keeps what it had.
pub(super) fn remember_selected_audio(
    config: &mut Config,
    devices: &[AudioDevice],
    selected: &[bool],
) {
    for (device, &on) in devices.iter().zip(selected) {
        config.record_default(device.is_loopback, &device.id, on);
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

/// Everything one periodic refresh enumerates -- taken off the UI thread
/// (see `refresh_sources_and_audio_if_due`) and applied back on it by
/// `App::apply_refresh_snapshot`.
pub(super) struct RefreshSnapshot {
    sources: Vec<CaptureSource>,
    audio_devices: Vec<AudioDevice>,
    live_app_audio: Vec<AppAudioSource>,
}

/// The same three enumerations, in the same order, that the refresh used to
/// run inline on the UI thread.
fn take_refresh_snapshot() -> RefreshSnapshot {
    RefreshSnapshot {
        sources: enumerate_sources(),
        audio_devices: enumerate_audio_devices().unwrap_or_default(),
        live_app_audio: enumerate_app_audio_sessions().unwrap_or_default(),
    }
}

/// Runs `take_refresh_snapshot` on a short-lived worker thread and hands
/// the result back through the returned channel, waking the UI when it
/// lands. The worker joins a single-threaded apartment -- the same one the
/// UI thread these calls used to run on is in (winit's `OleInitialize`) --
/// so `SHGetFileInfoW` icon extraction and the MMDevice calls behave
/// exactly as they did there. A dedicated thread rather than
/// `spawn_blocking` keeps that apartment off the tokio pool threads the
/// recording actors run on. `None` if the thread couldn't be spawned.
fn spawn_refresh_worker(ctx: &egui::Context) -> Option<std::sync::mpsc::Receiver<RefreshSnapshot>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let wake_ctx = ctx.clone();
    std::thread::Builder::new()
        .name("polyrec-source-refresh".into())
        .spawn(move || {
            let snapshot = {
                let _com = crate::sources::SingleThreadedApartment::join();
                take_refresh_snapshot()
            };
            let _ = tx.send(snapshot);
            wake_ctx.request_repaint();
        })
        .inspect_err(|e| {
            tracing::warn!("failed to spawn source-refresh thread, refreshing inline: {e}")
        })
        .ok()
        .map(|_| rx)
}

/// `None` when `new` covers exactly the same window/monitor handles as `old`
/// (order and titles aren't compared), so nothing needs replacing.
/// Otherwise `new` plus the selection re-pointed at the same handle it
/// pointed at in `old` (`None` if that source is gone).
fn reconcile_sources(
    old: &[CaptureSource],
    selected: Option<usize>,
    new: Vec<CaptureSource>,
) -> Option<(Vec<CaptureSource>, Option<usize>)> {
    let old_hwnds: std::collections::HashSet<usize> = old.iter().map(|s| s.hwnd).collect();
    let new_hwnds: std::collections::HashSet<usize> = new.iter().map(|s| s.hwnd).collect();
    if new_hwnds == old_hwnds {
        return None;
    }
    let previously_selected_hwnd = selected.and_then(|i| old.get(i)).map(|src| src.hwnd);
    let selected =
        previously_selected_hwnd.and_then(|hwnd| new.iter().position(|src| src.hwnd == hwnd));
    Some((new, selected))
}

/// Copies each window's current title from `new` onto the entry in `sources`
/// with the same handle, leaving order, selection and icons alone. Window
/// titles change all the time without the window itself changing (a browser
/// switching tabs, a launcher showing progress), and the list used to keep
/// showing whatever title a window had when it first appeared.
fn retitle_sources(sources: &mut [CaptureSource], new: &[CaptureSource]) {
    let titles: std::collections::HashMap<usize, &str> = new
        .iter()
        .map(|src| (src.hwnd, src.window_title.as_str()))
        .collect();
    for source in sources {
        if let Some(&title) = titles.get(&source.hwnd)
            && source.window_title != title
        {
            source.window_title = title.to_owned();
        }
    }
}

/// Whether the set of endpoint ids differs (order isn't compared).
fn audio_device_ids_changed(old: &[AudioDevice], new: &[AudioDevice]) -> bool {
    let old_ids: std::collections::HashSet<&str> = old.iter().map(|d| d.id.as_str()).collect();
    let new_ids: std::collections::HashSet<&str> = new.iter().map(|d| d.id.as_str()).collect();
    new_ids != old_ids
}

/// What an audio track of a recording is called on screen: its source's
/// name and, for a device, its 🔊/🎙 glyph -- kept apart so lists can draw
/// the glyph in their icon column (`widgets::icon_slot`) and line the names
/// up; in a sentence (`Display`) the glyph goes in front.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TrackLabel {
    pub(super) kind: TrackKind,
    pub(super) name: String,
}

/// Where an audio track's sound comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TrackKind {
    /// A playback device's loopback: what the PC plays.
    Speakers,
    Microphone,
    /// One app's own sound.
    App,
}

impl TrackLabel {
    /// The icon lists draw in their icon column; apps have none (their
    /// tracks are named after them).
    pub(super) fn icon(&self) -> Option<Icon> {
        match self.kind {
            TrackKind::Speakers => Some(Icon::Speaker),
            TrackKind::Microphone => Some(Icon::Microphone),
            TrackKind::App => None,
        }
    }
}

impl std::fmt::Display for TrackLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.icon() {
            Some(icon) => write!(f, "{} {}", icon.emoji(), self.name),
            None => f.write_str(&self.name),
        }
    }
}

/// Display label for each audio track a recording with these sources will
/// produce, in the same order `session::start_capture` assigns track ids
/// (devices first, then app-audio sources) -- see
/// `LastRecording::audio_labels`'s field doc for why the export dialog
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
fn build_audio_labels<'a>(
    devices: impl IntoIterator<Item = &'a AudioDevice>,
    app_sources: impl IntoIterator<Item = &'a AppAudioSource>,
) -> Vec<TrackLabel> {
    devices
        .into_iter()
        .map(|dev| TrackLabel {
            kind: if dev.is_loopback {
                TrackKind::Speakers
            } else {
                TrackKind::Microphone
            },
            name: dev.name.clone(),
        })
        .chain(app_sources.into_iter().flat_map(|src| {
            std::iter::repeat_n(
                TrackLabel {
                    kind: TrackKind::App,
                    name: src.display_name.clone(),
                },
                src.process_ids.len(),
            )
        }))
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

    /// Whether a manual recording is running or paused -- read from the
    /// session each time, never from a copy taken earlier in the frame:
    /// several start/stop presses can be handled in one frame, each after
    /// the last one's start or stop.
    pub(super) fn has_recording(&self) -> bool {
        self.session.is_recording() || self.session.is_paused()
    }

    /// Whether closing PolyRec now would lose footage: a manual recording
    /// runs, or a recording or a highlight is still being saved.
    pub(super) fn has_footage_to_lose(&self) -> bool {
        self.has_recording()
            || self.finalizing.handle.is_some()
            || matches!(self.highlight_save.state, HighlightSaveState::Saving)
    }

    /// Whether PolyRec is downloading and installing an update, which ends
    /// with it closing: nothing that would be cut off starts meanwhile.
    pub(super) fn updating(&self) -> bool {
        matches!(self.self_update.state, SelfUpdateState::Working)
    }

    pub(super) fn handle_rec_button(&mut self) {
        if self.has_recording() {
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
    /// With the dashboard itself in front, it does what REC would.
    pub(super) fn handle_hotkey_start_stop(&mut self, s: &'static Strings) {
        if self.has_recording() {
            self.stop_recording();
            return;
        }
        match hotkey_start(foreground(), self.rec_blocked_reason(s)) {
            HotkeyStart::Record(source) => {
                tracing::info!("start/stop hotkey: recording the window in front");
                self.start_recording_with_source(source);
            }
            HotkeyStart::RecordSelected => {
                tracing::info!(
                    "start/stop hotkey: recording what the dashboard in front has selected"
                );
                self.handle_rec_button();
            }
            HotkeyStart::Explain(why) => self.notices.push(why),
            HotkeyStart::Nothing => {
                tracing::warn!("start/stop hotkey pressed with no window in front");
            }
        }
    }

    /// Refreshes `free_space.bytes` for `config.output_dir`'s volume, at most
    /// every `FREE_SPACE_CHECK_INTERVAL` -- or immediately if the output dir
    /// itself changed since the last check (typing a new path, or Browse...),
    /// so switching drives doesn't show a stale number from the old one.
    pub(super) fn refresh_free_space(&mut self) {
        const FREE_SPACE_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

        let dir_changed =
            self.free_space.checked_dir.as_deref() != Some(self.config.output_dir.as_path());
        let due = self
            .free_space
            .checked_at
            .is_none_or(|t| t.elapsed() >= FREE_SPACE_CHECK_INTERVAL);
        if !dir_changed && !due {
            return;
        }

        self.free_space.bytes = crate::disk_space::free_bytes(&self.config.output_dir).ok();
        self.free_space.checked_at = Some(Instant::now());
        self.free_space.checked_dir = Some(self.config.output_dir.clone());
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
    /// Every `SOURCES_CHECK_INTERVAL`, re-enumerates capture sources, audio
    /// devices and live app-audio sessions. The enumeration itself (~26 ms
    /// measured, mostly per-window icon extraction and the process
    /// snapshot) runs on a worker thread (`spawn_refresh_worker`) so it
    /// doesn't stall a UI frame every interval; its result is applied here
    /// on a later frame by `apply_refresh_snapshot`. At most one refresh is
    /// in flight at a time.
    pub(super) fn refresh_sources_and_audio_if_due(&mut self, ctx: &egui::Context) {
        const SOURCES_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

        if let Some(rx) = &self.refresh_rx {
            match rx.try_recv() {
                Ok(snapshot) => {
                    self.refresh_rx = None;
                    self.apply_refresh_snapshot(snapshot);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                // The worker died without sending (it panicked) -- drop it
                // and let the interval check below schedule the next one.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.refresh_rx = None,
            }
        }

        if self
            .sources_checked_at
            .is_some_and(|t| t.elapsed() < SOURCES_CHECK_INTERVAL)
        {
            return;
        }
        self.sources_checked_at = Some(Instant::now());

        match spawn_refresh_worker(ctx) {
            Some(rx) => self.refresh_rx = Some(rx),
            None => self.apply_refresh_snapshot(take_refresh_snapshot()),
        }
    }

    fn apply_refresh_snapshot(&mut self, snapshot: RefreshSnapshot) {
        // Titles first: when the handle set is unchanged (the usual case, a
        // browser switching tabs) the list isn't replaced below, and this is
        // the only thing that keeps its titles current.
        retitle_sources(&mut self.sources, &snapshot.sources);
        if let Some((sources, selected)) =
            reconcile_sources(&self.sources, self.selected_source, snapshot.sources)
        {
            self.sources = sources;
            self.source_icon_textures.clear();
            self.selected_source = selected;
        }

        let new_audio_devices = snapshot.audio_devices;
        if audio_device_ids_changed(&self.audio_devices, &new_audio_devices) {
            self.selected_audio = resolve_selected_audio(&new_audio_devices, &self.config);
            self.audio_devices = new_audio_devices;
            // export.track_selection is NOT reset here -- same reasoning as the
            // old Refresh button: it's tied to the last finished recording's
            // actually-probed track count, not the live device list.
        }

        // Every entry the list can ever contain is a registered app (see
        // `build_app_audio_sources`'s doc comment), so there's no separate
        // "checked" state to preserve across a rebuild the way SYSTEM
        // devices have -- being in the list at all already means "record
        // this", always. Only the live process_ids (idle vs running) can
        // change between refreshes.
        let new_app_audio_sources =
            build_app_audio_sources_from(&snapshot.live_app_audio, &mut self.config);
        if app_audio_fingerprint(&new_app_audio_sources)
            != app_audio_fingerprint(&self.app_audio_sources)
        {
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
        remember_selected_audio(&mut self.config, &self.audio_devices, &self.selected_audio);
        self.save_config_or_report(s);
    }

    /// Saves `self.config`, logging and surfacing `s.config_save_failed_prefix`
    /// plus the error via `self.error_message` on failure. Forwards `self`'s
    /// fields into `util::save_config_or_report`, which is the one place
    /// this logic actually lives.
    pub(super) fn save_config_or_report(&mut self, s: &Strings) {
        super::util::save_config_or_report(
            &mut self.config,
            &mut self.error_message,
            s.config_save_failed_prefix,
        );
    }

    /// Rebuilds the Applications audio list from `self.config` immediately
    /// -- called right after a register/unregister action so the popup
    /// reflects it the same frame, instead of waiting for
    /// `refresh_sources_and_audio_if_due`'s next timer tick (which also
    /// wouldn't notice a registration change on its own: unregistering an
    /// app that's still running doesn't change its live `process_ids`, the
    /// thing that poll's fingerprint actually watches for).
    pub(super) fn rebuild_app_audio_sources_now(&mut self) {
        self.app_audio_sources = build_app_audio_sources(&mut self.config);
        self.app_audio_icon_textures.clear();
    }

    pub(super) fn stop_recording(&mut self) {
        if !self.finalizing.stop(&mut self.session) {
            return;
        }
        self.frame_count.store(0, Ordering::Relaxed);
        // export.track_selection/export.available_tracks are set once finalize
        // actually succeeds and the file can be probed (poll_background_work) --
        // not here, since what's selected pre-recording isn't a guarantee of
        // what ends up in the file.
    }

    /// Resolves the currently-checked audio devices/app-audio sources into
    /// the exact shape both `start_recording_with_source` and
    /// `start_highlight_buffering_for` pass into
    /// `session::start_capture`/`start_highlight_buffering` -- identical
    /// resolution for either caller, including sorting loopback devices
    /// first. That sort matters: the MP4 container's physical track order
    /// doesn't reliably follow AddStream() call order for a live
    /// (real-time-encoded) recording -- see writer.rs/remux.rs -- but
    /// process-loopback (app-audio) tracks still consistently land after
    /// device tracks in practice, since activating one needs an async
    /// WASAPI round-trip a plain device capture doesn't, so it starts
    /// flushing samples later regardless of call order. That leaves
    /// device-vs-device order as the one lever actually worth pulling:
    /// putting loopback (system/game audio, what users overwhelmingly want
    /// as "the" track) ahead of the mic improves the odds it's what a
    /// player defaults to without explicit track selection, even though
    /// nothing here can fully guarantee it.
    fn resolve_capture_inputs(&self) -> CaptureInputs {
        let mut devices: Vec<(AudioDevice, f32)> = self
            .audio_devices
            .iter()
            .zip(self.selected_audio.iter())
            .filter(|&(_, &sel)| sel)
            .map(|(dev, _)| (dev.clone(), self.config.audio_gain(&dev.id)))
            .collect();
        devices.sort_by_key(|(dev, _)| !dev.is_loopback);
        let app_sources = self
            .app_audio_sources
            .iter()
            .map(|src| (src.clone(), self.config.app_audio_gain(&src.exe_name)))
            .collect();
        CaptureInputs {
            devices,
            app_sources,
            app_audio_only: self.config.default_app_audio_only,
            hide_cursor: !self.config.record_cursor,
            encode: self.encode_settings(),
        }
    }

    /// `Config::encode` resolved into what a recording (and the Quality
    /// popup's fps probe) is configured with.
    pub(super) fn encode_settings(&self) -> EncodeSettings {
        (&self.config.encode).into()
    }

    /// Resolves `Config::use_process_name_for_recording` /
    /// `custom_recording_prefix` into the `RecordingNaming` `start_capture`
    /// and `save_highlight` need -- shared by both since a user's naming
    /// choice applies the same way to a manual recording and a Highlight
    /// save.
    pub(super) fn resolve_recording_naming(&self) -> RecordingNaming {
        if self.config.use_process_name_for_recording {
            RecordingNaming::ProcessName
        } else {
            RecordingNaming::CustomPrefix(self.config.custom_recording_prefix.clone())
        }
    }

    pub(super) fn start_recording_with_source(&mut self, source: CaptureSource) {
        // REC and the tray's Start are disabled meanwhile (see
        // `why_rec_is_blocked`), but the start/stop hotkey comes here
        // directly. Stopping a recording started now would replace the
        // running save's handle, losing its result.
        if self.finalizing.handle.is_some() {
            tracing::warn!("not starting a recording while the last one is still being saved");
            return;
        }
        // PolyRec closes once the update is in: that would cut it off.
        if self.updating() {
            tracing::warn!("not starting a recording while PolyRec updates itself");
            return;
        }
        let app_name = crate::session::app_name_from_exe(&source.exe_name);
        let inputs = self.resolve_capture_inputs();
        let track_count = inputs.devices.len() + inputs.app_sources.len();
        let audio_labels = build_audio_labels(
            inputs.devices.iter().map(|(dev, _)| dev),
            inputs.app_sources.iter().map(|(src, _)| src),
        );
        let naming = self.resolve_recording_naming();
        let kind = source.kind;
        let starting = std::time::Instant::now();
        match self.session.start_capture(
            source,
            inputs,
            self.config.show_recording_border,
            Arc::clone(&self.frame_count),
            &self.config.output_dir,
            naming,
        ) {
            Ok(path) => {
                tracing::info!(
                    "recording started in {:?}: app={app_name:?} source={kind:?} border={} audio_tracks={track_count} output={}",
                    starting.elapsed(),
                    self.config.show_recording_border,
                    path.display()
                );
                self.fps_shortfall_warned = false;
                self.recording_checks = Default::default();
                // Otherwise the previous recording's cached preview -- keyed
                // on elapsed seconds, which this new recording also starts
                // at 0 from -- would flash as this one's name until the
                // first elapsed-second tick recomputes it.
                self.recording_status = None;
                self.meter_levels.clear();
                // A notice about the previous recording would read as being
                // about this one.
                self.notices.clear();
                self.last_recording.started(audio_labels, app_name);
            }
            Err(e) => {
                tracing::error!("start_capture failed for app={app_name:?}: {e}");
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
        let saving = matches!(self.highlight_save.state, HighlightSaveState::Saving);
        // The highlight actor stopped itself early (disk ran low, or its
        // writer couldn't start or gave out) -- mirrors how
        // poll_background_work detects manual recording's recorder stopping
        // itself early via `recorder_handle.is_finished()`. Not while a save
        // reads the buffer: this discards it.
        if !saving {
            self.note_highlight_stopped_itself();
        }
        if !self.config.highlight.enabled {
            // Turned back on, it tries at once.
            self.highlight_retry = HighlightRetry::default();
        } else if self.highlight_retry.is_failing() && self.session.highlight_buffered() {
            self.highlight_retry.working();
        }

        // Just the handle, and only with Highlight on: the window's details
        // (its icon read from its exe among them) used to be fetched on every
        // pass, to compare handles.
        // Set aside while PolyRec updates itself (it closes when that's
        // done), back on if the update fails -- the setting itself stays.
        let enabled = self.config.highlight.enabled && !self.updating();
        let front = match enabled.then(front_window) {
            Some(Foreground::Window(hwnd)) => Some(hwnd.0 as usize),
            _ => None,
        };
        // Re-using the already-throttled free-space reading (refreshed a few
        // lines earlier in poll_background_work) instead of a fresh syscall
        // every frame -- also naturally rate-limits retrying after a
        // disk-full stop to that same ~3s cadence instead of every frame.
        let has_room = self
            .free_space
            .bytes
            .is_none_or(|f| f >= crate::disk_space::MIN_FREE_BYTES);
        let now = Instant::now();
        let view = HighlightView {
            enabled,
            recording: self.has_recording(),
            saving,
            following: self.session.highlight_hwnd(),
            front,
            may_start: front
                .is_some_and(|hwnd| has_room && self.highlight_retry.may_start(hwnd, now)),
        };
        match highlight_step(&view) {
            HighlightStep::Leave => {}
            HighlightStep::Pause => {
                self.session.stop_highlight_buffering(false);
            }
            HighlightStep::Discard => {
                self.session.stop_highlight_buffering(true);
            }
            HighlightStep::Start(hwnd) => {
                if view.following.is_some() {
                    self.session.stop_highlight_buffering(true);
                }
                let hwnd = windows::Win32::Foundation::HWND(hwnd as *mut core::ffi::c_void);
                self.start_highlight_buffering_for(&crate::sources::capture_source_for_hwnd(hwnd));
            }
        }
    }

    /// Once Highlight buffering has stopped by itself: says why, and holds
    /// off starting it again for that window if it failed (see
    /// `HighlightRetry`).
    fn note_highlight_stopped_itself(&mut self) {
        let hwnd = self.session.highlight_hwnd();
        let Some(stopped) = self.session.highlight_stopped_itself() else {
            return;
        };
        match (stopped, hwnd) {
            (HighlightStopped::DiskFull, _) => {
                let strings = self.config.lang().strings();
                self.notices.push(strings.highlight_disk_full_message);
            }
            (HighlightStopped::Failed(why), Some(hwnd)) => self.highlight_failed(hwnd, &why),
            (HighlightStopped::Failed(_), None) => {}
        }
    }

    /// Buffering `hwnd` failed: said once in a row, and not tried again for
    /// that window for a while.
    fn highlight_failed(&mut self, hwnd: usize, why: &str) {
        tracing::warn!("Highlight buffering failed: {why}");
        if self.highlight_retry.failed(hwnd, Instant::now()) {
            let strings = self.config.lang().strings();
            self.notices
                .push(strings.highlight_failed_template.replace("{why}", why));
        }
    }

    pub(super) fn start_highlight_buffering_for(&mut self, source: &CaptureSource) {
        let inputs = self.resolve_capture_inputs();
        let buffer_seconds = self.config.highlight.buffer_seconds.clamp(
            crate::config::HIGHLIGHT_BUFFER_SECONDS_MIN,
            crate::config::HIGHLIGHT_BUFFER_SECONDS_MAX,
        );
        if let Err(e) = self.session.start_highlight_buffering(
            source,
            inputs,
            &self.config.output_dir,
            buffer_seconds,
        ) {
            self.highlight_failed(source.hwnd, &e.to_string());
        }
    }

    /// Forces the current segment to finalize and saves the buffer to a file
    /// -- a no-op (with a status message, not a popup, per the established
    /// export-UI convention) if Highlight buffering isn't currently active.
    pub(super) fn handle_save_highlight_hotkey(&mut self, s: &'static Strings) {
        let Some(hwnd) = self.session.highlight_hwnd() else {
            self.notices.push(s.highlight_save_not_active);
            return;
        };
        if matches!(self.highlight_save.state, HighlightSaveState::Saving) {
            return;
        }
        let real_hwnd = windows::Win32::Foundation::HWND(hwnd as *mut core::ffi::c_void);
        let source = crate::sources::capture_source_for_hwnd(real_hwnd);
        let app_name = crate::session::app_name_from_exe(&source.exe_name);
        let naming = self.resolve_recording_naming();
        match self
            .session
            .save_highlight(&self.config.output_dir, &app_name, naming)
        {
            Ok(handle) => {
                self.highlight_save.state = HighlightSaveState::Saving;
                self.highlight_save.handle = Some(handle);
            }
            Err(e) => {
                self.highlight_save.state = HighlightSaveState::Failed(e.to_string());
            }
        }
    }
}

/// What's in front: another app's window -- `W`, as a `CaptureSource` to
/// record, or just its handle -- or one of PolyRec's own windows (its
/// dashboard or overlay), which would otherwise let a hotkey press while
/// focused on our own window "record" it instead of whatever the user
/// actually meant to capture.
enum Foreground<W = CaptureSource> {
    /// Another app's window, to record.
    Window(W),
    /// One of PolyRec's own windows.
    Dashboard,
    Nothing,
}

/// What's in front, another app's window by its handle alone.
fn front_window() -> Foreground<windows::Win32::Foundation::HWND> {
    unsafe {
        let hwnd = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
        if hwnd.0.is_null() {
            return Foreground::Nothing;
        }
        let mut pid = 0u32;
        windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == std::process::id() {
            return Foreground::Dashboard;
        }
        Foreground::Window(hwnd)
    }
}

fn foreground() -> Foreground {
    match front_window() {
        Foreground::Window(hwnd) => {
            Foreground::Window(crate::sources::capture_source_for_hwnd(hwnd))
        }
        Foreground::Dashboard => Foreground::Dashboard,
        Foreground::Nothing => Foreground::Nothing,
    }
}

/// Where Highlight buffering stands, for `highlight_step`.
struct HighlightView {
    enabled: bool,
    /// A manual recording is running or paused.
    recording: bool,
    /// A save is reading the buffer's files.
    saving: bool,
    /// The window buffering follows, while it runs.
    following: Option<usize>,
    /// Another app's window in front, if one is.
    front: Option<usize>,
    /// Whether buffering may start for `front`: there's room on the disk,
    /// and no failure for it to wait out (`HighlightRetry`).
    may_start: bool,
}

/// What `refresh_highlight_buffering` does this pass.
#[derive(Debug, PartialEq)]
enum HighlightStep {
    Leave,
    /// Stop, leaving the files: a manual recording runs.
    Pause,
    /// Stop and delete the buffer.
    Discard,
    /// Start for this window -- discarding first a buffer that follows
    /// another.
    Start(usize),
}

fn highlight_step(view: &HighlightView) -> HighlightStep {
    let running = view.following.is_some();
    // Highlight buffering and manual recording never run at once --
    // doubling GPU/encode load for no benefit while the thing the user
    // actually pressed record for is what matters. Pausing rather than
    // discarding is just about not wastefully deleting files that are about
    // to get cleaned up anyway the moment buffering resumes for whatever's
    // foreground once the recording stops (see `start_highlight_buffering`'s
    // stale-file cleanup) -- buffering effectively restarts fresh after a
    // manual recording, it does not carry the pre-recording buffer forward.
    if view.recording {
        return if running {
            HighlightStep::Pause
        } else {
            HighlightStep::Leave
        };
    }
    // A save reads the buffer's segment files until it's done: discarding
    // them, or a start clearing the folder, would delete them under it.
    if view.saving {
        return HighlightStep::Leave;
    }
    if !view.enabled {
        return if running {
            HighlightStep::Discard
        } else {
            HighlightStep::Leave
        };
    }
    // Nothing else's in front -- e.g. focus is briefly on PolyRec's own
    // window while the user changes a setting: not a real switch away.
    // Leave whatever's already running (if anything) alone; WGC keeps
    // capturing a window that isn't foreground just fine. A different
    // window is a different app/resolution, which can't be concatenated
    // across (see encode::highlight_export): start over for it.
    let Some(front) = view.front else {
        return HighlightStep::Leave;
    };
    match view.following {
        Some(following) if following == front => HighlightStep::Leave,
        _ if view.may_start => HighlightStep::Start(front),
        Some(_) => HighlightStep::Discard,
        None => HighlightStep::Leave,
    }
}

/// What the start/stop hotkey starts, with nothing recording.
enum HotkeyStart {
    Record(CaptureSource),
    /// What REC would: the source picked on the dashboard.
    RecordSelected,
    /// Why REC can't start, as its tooltip says.
    Explain(&'static str),
    Nothing,
}

/// The hotkey records the window in front. With the dashboard in front it
/// used to do nothing at all -- a warning in a log release builds don't
/// show, and the "not started" toast is kept off the focused dashboard -- so
/// now it does what REC would, or says why REC can't (`rec_blocked`).
fn hotkey_start(front: Foreground, rec_blocked: Option<&'static str>) -> HotkeyStart {
    match (front, rec_blocked) {
        (Foreground::Window(source), _) => HotkeyStart::Record(source),
        (Foreground::Dashboard, None) => HotkeyStart::RecordSelected,
        (Foreground::Dashboard, Some(why)) => HotkeyStart::Explain(why),
        (Foreground::Nothing, _) => HotkeyStart::Nothing,
    }
}

#[cfg(test)]
mod tests;
