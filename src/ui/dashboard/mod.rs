mod hotkeys_popup;

mod actions;
mod audio_popup;
mod background;
mod checks;
mod fps_support;
mod graphics_device;
mod overlay;
mod panel_center;
mod panel_menu;
mod panel_source;
mod popups;
mod preview;
mod recent;
mod state;
mod status_badge;
mod theme;
mod toast;
mod tray;
mod util;
mod widgets;

use crate::capture::audio::enumerate_audio_devices;
use crate::config::Config;
use crate::hotkeys::HotkeyListener;
use crate::i18n::Strings;
use crate::session::SessionManager;
use crate::sources::enumerate_sources;
use crate::types::{AppAudioSource, AudioDevice, CaptureSource};
use eframe::egui;
use fps_support::FpsSupportProbe;
use hotkeys_popup::HotkeySlot;
use state::{
    AddAppPicker, ExportUi, Finalizing, FreeSpaceCache, HighlightSave, RecordingStatusCache,
    SelfUpdate,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Instant;
use theme::{setup_fonts, setup_theme};

pub use overlay::is_caught_on_purpose;

pub struct App {
    config: Config,
    session: SessionManager,
    sources: Vec<CaptureSource>,
    selected_source: Option<usize>,
    /// Live text from the source-panel search box -- filters `sources` by
    /// window title or exe name at render time (see `render_source_panel`),
    /// never mutates `sources` itself.
    source_filter: String,
    audio_devices: Vec<AudioDevice>,
    selected_audio: Vec<bool>,
    /// Curated from `config.registered_app_audio`, not auto-populated from
    /// whatever's currently making sound -- see
    /// `actions::build_app_audio_sources`'s doc comment. Every entry here is
    /// registered by construction, so `selected_app_audio` is always `true`
    /// for each one; there's no separate "checked but not registered" state.
    app_audio_sources: Vec<AppAudioSource>,
    selected_app_audio: Vec<bool>,
    /// Keyed by index into `app_audio_sources` -- same convention and same
    /// "clear on any list change, rebuild lazily" lifecycle as
    /// `source_icon_textures`.
    app_audio_icon_textures: std::collections::HashMap<usize, egui::TextureHandle>,
    app_audio_only: bool,
    overlay_enabled: bool,
    /// Whether Windows' own colored capture-border indicator shows around
    /// the recorded window/monitor during a manual recording -- see
    /// `Config::show_recording_border`'s doc comment. Toggled via the menu
    /// bar button next to Overlay; persisted the same lazy, on-exit-only way.
    show_recording_border: bool,
    show_quality_popup: bool,
    /// What the Quality popup knows about the fps the selected source can
    /// really be recorded at -- see `FpsSupportProbe`.
    fps_support: FpsSupportProbe,
    show_hotkeys_popup: bool,
    show_audio_popup: bool,
    source_icon_textures: std::collections::HashMap<usize, egui::TextureHandle>,
    frame_count: Arc<AtomicU64>,
    recording_start: Option<Instant>,
    /// Set once per recording after the achieved fps (frame_count / elapsed)
    /// has been checked against the configured target and found persistently
    /// short -- the "frames" counter next to "tracks" is a real throughput
    /// signal (it only increments once a frame clears the whole pipeline
    /// through to the recording channel), but nothing previously surfaced a
    /// sustained shortfall to the user; it just silently degraded. One-shot
    /// per recording so it doesn't re-fire every frame once triggered.
    fps_shortfall_warned: bool,
    /// The next recording's estimated rate, for the free-space line.
    rate_estimate: checks::RateEstimate,
    /// What the running recording's checks found (see `checks`).
    recording_checks: checks::RecordingChecks,
    /// The active recording's saved-name preview and size so far -- see
    /// `RecordingStatusCache`.
    recording_status: Option<RecordingStatusCache>,
    /// What each audio level meter shows right now, by track -- see
    /// `render_audio_meters`.
    meter_levels: Vec<f32>,
    last_output_path: Option<PathBuf>,
    output_dir_input: String,
    /// Why recordings can't be saved under `config.output_dir`, if they
    /// can't -- checked at startup and whenever the folder is changed (see
    /// `set_output_dir`).
    output_dir_problem: Option<util::OutputDirProblem>,
    /// The newest finished recordings, for the Recent list -- see
    /// `refresh_recent`.
    recent: Vec<recent::RecentRecording>,
    /// Last time `refresh_sources_and_audio_if_due` actually ran (see there
    /// for why this is throttled rather than run every frame).
    sources_checked_at: Option<Instant>,
    /// The in-flight refresh's result channel, if one is running -- see
    /// `refresh_sources_and_audio_if_due`.
    refresh_rx: Option<mpsc::Receiver<actions::RefreshSnapshot>>,
    /// Display label for each audio track actually used in the current
    /// manual recording, or the last finished one (set when a recording
    /// starts; devices, then app-audio sources, in the same order
    /// `start_recording_with_source` assigns track ids) --
    /// NOT the live system device list (`audio_devices`), which reflects
    /// whatever's currently plugged in and has no entries for app-audio
    /// sources at all. The export checkbox list zips this against
    /// `export.track_selection` by position; a length mismatch (e.g. a
    /// device disconnected mid-recording, producing fewer tracks than were
    /// selected) falls back to a generic "Track N" label for the unlabeled
    /// tail rather than panicking or misattributing a name.
    last_recording_audio_labels: Vec<actions::TrackLabel>,
    /// App name for the last finished manual recording (same derivation as
    /// the recording's own filename, via `session::app_name_from_exe`) --
    /// used to pre-fill the export Save dialog's default filename. Stored at
    /// record-start rather than re-parsed from the recording's filename, so
    /// it can't drift if that filename format ever changes.
    last_recording_app_name: String,
    hotkey_listener: Option<HotkeyListener>,
    update_available: Option<crate::update_check::AvailableUpdate>,
    update_check_rx: Option<mpsc::Receiver<Option<crate::update_check::AvailableUpdate>>>,
    /// A real error, shown as a modal (a recording that couldn't start or
    /// finalize, a config that couldn't be saved) — cleared by the "Close"
    /// button in its popup. Things worth knowing that don't stop anything go
    /// to `notice` instead.
    error_message: Option<String>,
    /// Something worth knowing that doesn't stop anything: a recording that
    /// ran below its frame rate, came out with fewer audio tracks, or stopped
    /// early for disk space (the file is still saved), Highlight not being on
    /// when its hotkey is pressed, and so on. Shown as a dismissible strip at
    /// the top of the Status panel (`render_notice`) rather than a modal that
    /// has to be clicked away, which is what all of these used to be. Cleared
    /// when a new recording starts.
    notice: Option<String>,
    /// The acknowledgement for the last global hotkey, shown briefly while
    /// the dashboard isn't focused -- see `toast`.
    toast: Option<toast::Toast>,
    /// The notification-area icon; `None` if Windows wouldn't take one
    /// (logged), which leaves everything else working as before.
    tray: Option<tray::Tray>,
    /// The selected source's picture, in its card.
    preview: preview::SourcePreview,
    /// The dot on the dashboard's taskbar button while recording; `None`
    /// if Windows wouldn't give one.
    taskbar_badge: Option<status_badge::TaskbarBadge>,
    /// Whether the dashboard was hidden through the tray -- see
    /// `App::dashboard_shown`.
    dashboard_hidden: bool,
    /// Set when PolyRec was launched again while running -- see
    /// `single_instance`.
    show_requested: Arc<AtomicBool>,
    /// Watched for being lost -- see `graphics_device`.
    graphics: graphics_device::GraphicsDevice,
    /// Set (for `main`, which then starts a new copy) when the dashboard
    /// closes because `graphics` was lost.
    closed_for_lost_graphics: Arc<AtomicBool>,
    /// Set when PolyRec closes its own window to quit (`App::quit`), as
    /// opposed to the user's close button -- which `Config::close_to_tray`
    /// may turn into hiding instead.
    quitting: bool,
    /// Set while the Hotkeys popup is waiting for the user to press a key to
    /// bind, after clicking "Change" for that row.
    recording_hotkey: Option<HotkeySlot>,
    /// Shown inline in the Hotkeys popup when the last captured key turned
    /// out to be unregisterable (already used by another app, or reserved by
    /// Windows) — see `hotkeys::try_register`.
    hotkey_capture_warning: Option<String>,
    /// The Audio popup's "+ Add app" picker -- see `AddAppPicker`.
    add_app: AddAppPicker,
    free_space: FreeSpaceCache,
    finalizing: Finalizing,
    export: ExportUi,
    highlight_save: HighlightSave,
    self_update: SelfUpdate,
    /// Window position as of the most recent frame -- updated every frame in
    /// `ui()` (cheap, in-memory only) and written to `config.toml` exactly
    /// once, in `on_exit()`, rather than on every change. A window drag can
    /// generate far more position-changed frames than e.g. the volume
    /// slider's bounded ~200 steps, so save-on-every-change would be
    /// needlessly write-heavy here.
    last_window_pos: Option<egui::Pos2>,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        mut config: Config,
        show_requested: Arc<AtomicBool>,
        closed_for_lost_graphics: Arc<AtomicBool>,
        restarted: Option<crate::restart::Reason>,
    ) -> Self {
        setup_theme(&cc.egui_ctx);
        setup_fonts(&cc.egui_ctx);
        let graphics = graphics_device::GraphicsDevice::watch(cc);
        // In place of a copy that closed on its own -- quite possibly while
        // a game had focus, which a window coming up in front would take.
        let notice = restarted.map(|reason| {
            let s = config.lang().strings();
            match reason {
                crate::restart::Reason::Graphics => s.notice_restarted_graphics.to_owned(),
                crate::restart::Reason::Error => s
                    .notice_restarted_error
                    .replace("{path}", &crate::crash_log::path().to_string_lossy()),
            }
        });
        if restarted.is_some() {
            cc.egui_ctx
                .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        if config.encode.gpu_frames
            && config.encode.encoder_mode == crate::config::EncoderMode::Hardware
        {
            crate::encode::gpu::GpuFrames::warm_up();
        }
        let overlay_enabled = config.overlay.enabled;
        let show_recording_border = config.show_recording_border;
        let app_audio_only = config.default_app_audio_only;
        let output_dir_input = config.output_dir.to_string_lossy().into_owned();
        let output_dir_problem = util::output_dir_problem(&config.output_dir);
        let recent = recent::recent_recordings(&config.output_dir, recent::RECENT_LIMIT);
        let audio_devices = enumerate_audio_devices().unwrap_or_default();
        let n = audio_devices.len();
        // Restores the user's last saved SYSTEM audio selection if there is
        // one; otherwise defaults to loopback (system/game audio) only. The
        // MP4 container's physical stream order doesn't follow AddStream()
        // call order (see writer.rs/remux.rs), so when multiple audio tracks
        // are muxed, naive players picking "the first audio stream" can land
        // on a different track than intended. Recording just the loopback
        // device by default keeps the common case to a single,
        // deterministically audible track; mic capture remains an opt-in
        // checkbox -- see `actions::resolve_selected_audio`.
        let selected_audio = actions::resolve_selected_audio(&audio_devices, &config);
        // Curated entirely by config -- see `actions::build_app_audio_sources`'s
        // doc comment for why this doesn't auto-populate from whatever's
        // currently making sound. Every entry is registered by
        // construction, so it's always checked.
        let app_audio_sources = actions::build_app_audio_sources(&mut config);
        let selected_app_audio = vec![true; app_audio_sources.len()];
        let initial_track_selection = vec![true; n];
        let wake_ctx = cc.egui_ctx.clone();
        let hotkey_listener = HotkeyListener::spawn(
            &config.hotkeys.start_stop,
            &config.hotkeys.pause,
            &config.hotkeys.toggle_overlay,
            &config.hotkeys.save_highlight,
            move || wake_ctx.request_repaint(),
        );
        let wake_ctx = cc.egui_ctx.clone();
        let tray = tray::Tray::new(move || wake_ctx.request_repaint())
            .inspect_err(|e| tracing::warn!("no tray icon: {e}"))
            .ok();
        let taskbar_badge = status_badge::dashboard_hwnd(cc).and_then(|hwnd| {
            status_badge::TaskbarBadge::new(hwnd)
                .inspect_err(|e| tracing::warn!("no taskbar badge: {e}"))
                .ok()
        });

        // Fire-and-forget: checked once at startup, polled in `update()`. Any failure
        // (offline, GitHub unreachable, no releases yet) just means no banner shows —
        // see update_check::check_for_update's doc comment.
        let (update_tx, update_check_rx) = mpsc::channel();
        tokio::spawn(async move {
            let result = crate::update_check::check_for_update(env!("CARGO_PKG_VERSION")).await;
            let _ = update_tx.send(result);
        });

        Self {
            config,
            session: SessionManager::new(),
            sources: enumerate_sources(),
            selected_source: None,
            source_filter: String::new(),
            audio_devices,
            selected_audio,
            app_audio_sources,
            selected_app_audio,
            app_audio_icon_textures: std::collections::HashMap::new(),
            app_audio_only,
            overlay_enabled,
            show_recording_border,
            show_quality_popup: false,
            fps_support: FpsSupportProbe::default(),
            show_hotkeys_popup: false,
            show_audio_popup: false,
            source_icon_textures: std::collections::HashMap::new(),
            frame_count: Arc::new(AtomicU64::new(0)),
            recording_start: None,
            fps_shortfall_warned: false,
            rate_estimate: checks::RateEstimate::default(),
            recording_checks: checks::RecordingChecks::default(),
            recording_status: None,
            meter_levels: Vec::new(),
            last_output_path: None,
            output_dir_input,
            output_dir_problem,
            recent,
            sources_checked_at: None,
            refresh_rx: None,
            last_recording_audio_labels: Vec::new(),
            last_recording_app_name: String::new(),
            hotkey_listener: Some(hotkey_listener),
            update_available: None,
            update_check_rx: Some(update_check_rx),
            error_message: None,
            notice,
            toast: None,
            tray,
            preview: preview::SourcePreview::default(),
            taskbar_badge,
            dashboard_hidden: false,
            show_requested,
            graphics,
            closed_for_lost_graphics,
            quitting: false,
            recording_hotkey: None,
            hotkey_capture_warning: None,
            last_window_pos: None,
            add_app: AddAppPicker::default(),
            free_space: FreeSpaceCache::default(),
            finalizing: Finalizing::default(),
            export: ExportUi::new(initial_track_selection),
            highlight_save: HighlightSave::default(),
            self_update: SelfUpdate::default(),
        }
    }
}

impl App {
    /// Closes the window to quit PolyRec -- the tray's "Quit PolyRec", a
    /// finished self-update, a lost graphics device -- which
    /// `Config::close_to_tray` must not turn into hiding.
    pub(super) fn quit(&mut self, ctx: &egui::Context) {
        self.quitting = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

/// Whether a request to close the window hides PolyRec in the tray instead
/// of quitting: the user asked for that (`close_to_tray`), there's a tray
/// icon to bring it back from, and PolyRec isn't closing its own window to
/// quit (`App::quit`) -- egui reports that as a close request too.
fn close_button_hides(close_to_tray: bool, has_tray: bool, quitting: bool) -> bool {
    close_to_tray && has_tray && !quitting
}

impl eframe::App for App {
    // eframe runs `ui` only while the dashboard is on screen -- not while
    // it's minimized -- but calls `logic` either way: before every `ui`, and
    // on each requested repaint while minimized. So the work that has to go
    // on with the dashboard minimized (global hotkeys, finishing a
    // recording, Highlight saves) is polled here; in `ui` it all waited for
    // the window to be restored.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.graphics.check() && !self.closed_for_lost_graphics.swap(true, Ordering::Relaxed) {
            tracing::error!("closing so that a new PolyRec can start on a new graphics device");
            self.quit(ctx);
        }
        let s: &'static Strings = self.config.lang().strings();
        if ctx.input(|i| i.viewport().close_requested())
            && close_button_hides(
                self.config.close_to_tray,
                self.tray.is_some(),
                self.quitting,
            )
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.set_dashboard_shown(ctx, false);
            self.toast = Some(toast::Toast::new(s.toast_still_in_tray.to_owned()));
        }
        self.poll_background_work(ctx, s);
        if self.show_requested.swap(false, Ordering::Relaxed) {
            self.set_dashboard_shown(ctx, true);
        }
        self.poll_tray(ctx, s);
        self.run_recording_checks(s);
        self.update_preview(ctx);
        self.request_repaints(ctx);
        self.request_ui_for_new_windows_while_minimized(ctx);
    }

    // eframe 0.34 deprecated `update(&Context, &mut Frame)` in favor of this
    // `ui(&mut egui::Ui, &mut Frame)` entry point. The three docked panels
    // (menu bar, source list, center) now nest inside this root Ui via
    // Panel::show/CentralPanel::show (egui 0.35 renamed the ctx-based
    // top-level `show` these replaced to `show_inside`, then back to plain
    // `show` once the old one was removed entirely -- this is the final
    // name) -- everything else (popups via egui::Window, the overlay
    // viewport, background polling) is unaffected and still operates on
    // `&egui::Context` (Ui::ctx() hands one back, cheaply cloneable).
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Nothing is drawn on a lost device, overlay and toast included,
        // until the close `logic` asked for: egui's renderer panics uploading
        // a frame's shapes to one, and an empty frame uploads nothing.
        if self.graphics.was_lost() {
            return;
        }
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        let s: &'static Strings = self.config.lang().strings();

        // In-memory only -- see the `last_window_pos` field doc for why this
        // isn't saved to config.toml here.
        if let Some(outer_rect) = ctx.input(|i| i.viewport().outer_rect) {
            self.last_window_pos = Some(outer_rect.min);
        }

        // The overlay HUD and the toast are windows of their own, drawn from
        // this pass however the dashboard is. Hidden in the tray, the
        // dashboard itself has nothing to draw -- while recording with the
        // overlay on, this pass still runs at the overlay's full rate (see
        // `recording_needs_fast_repaint`).
        self.render_overlay_viewport(ctx, s);
        self.render_toast_viewport(ctx);
        if self.dashboard_hidden {
            return;
        }
        self.render_menu_bar(ui, s);
        self.render_source_panel(ui, s);
        self.render_center_panel(ui, s);
        self.render_audio_popup(ctx, s);
        self.render_quality_popup(ctx, s);
        self.render_hotkeys_popup(ctx, s);
        self.render_error_banner(ctx, s);
        self.render_self_update_popup(ctx, s);
    }

    fn on_exit(&mut self) {
        if let Some(h) = self.hotkey_listener.take() {
            h.stop();
        }
        // Covers both a normal close and the close a successful self-update
        // triggers (dashboard.rs's ViewportCommand::Close after
        // perform_self_update returns Ok) -- no special-casing needed in
        // self_update.rs itself.
        if let Some(pos) = self.last_window_pos {
            self.config.window_position = Some((pos.x, pos.y));
            if let Err(e) = self.config.save() {
                tracing::error!("failed to save window position: {e}");
            }
        }
    }
}

#[cfg(test)]
mod close_tests {
    use super::close_button_hides;

    #[test]
    fn the_close_button_hides_only_when_asked_to_and_theres_a_tray_to_come_back_from() {
        assert!(close_button_hides(true, true, false));
        // The default: closing quits.
        assert!(!close_button_hides(false, true, false));
        // With no tray icon, hiding would leave no way back but a relaunch.
        assert!(!close_button_hides(true, false, false));
        // PolyRec quitting (the tray's Quit, a self-update) isn't stopped.
        assert!(!close_button_hides(true, true, true));
    }
}
