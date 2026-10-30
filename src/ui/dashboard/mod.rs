mod hotkeys_popup;

mod actions;
mod audio_popup;
mod background;
mod fps_support;
mod overlay;
mod panel_center;
mod panel_menu;
mod panel_source;
mod popups;
mod state;
mod theme;
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
use state::{AddAppPicker, ExportUi, Finalizing, FreeSpaceCache, HighlightSave, SelfUpdate};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::mpsc;
use std::time::Instant;
use theme::{setup_fonts, setup_theme};

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
    /// Cached preview of the filename the active recording will be saved as
    /// (elapsed-seconds it was computed at, and the name itself) -- recomputed
    /// only when `elapsed` ticks over to a new second (see
    /// `render_center_panel`), not every frame, since previewing
    /// `RecordingNaming::CustomPrefix` does a directory listing
    /// (`recording_naming::next_sequence_number`).
    displayed_recording_name: Option<(u64, String)>,
    last_output_path: Option<PathBuf>,
    output_dir_input: String,
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
    last_recording_audio_labels: Vec<String>,
    /// App name for the last finished manual recording (same derivation as
    /// the recording's own filename, via `session::app_name_from_exe`) --
    /// used to pre-fill the export Save dialog's default filename. Stored at
    /// record-start rather than re-parsed from the recording's filename, so
    /// it can't drift if that filename format ever changes.
    last_recording_app_name: String,
    hotkey_listener: Option<HotkeyListener>,
    update_available: Option<crate::update_check::AvailableUpdate>,
    update_check_rx: Option<mpsc::Receiver<Option<crate::update_check::AvailableUpdate>>>,
    /// User-facing error banner (disk-full refusal to start, disk-full
    /// mid-recording stop, or a finalize failure) — cleared by the "Close"
    /// button in its popup.
    error_message: Option<String>,
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
    pub fn new(cc: &eframe::CreationContext<'_>, mut config: Config) -> Self {
        setup_theme(&cc.egui_ctx);
        setup_fonts(&cc.egui_ctx);
        let overlay_enabled = config.overlay.enabled;
        let show_recording_border = config.show_recording_border;
        let app_audio_only = config.default_app_audio_only;
        let output_dir_input = config.output_dir.to_string_lossy().into_owned();
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
            displayed_recording_name: None,
            last_output_path: None,
            output_dir_input,
            sources_checked_at: None,
            refresh_rx: None,
            last_recording_audio_labels: Vec::new(),
            last_recording_app_name: String::new(),
            hotkey_listener: Some(hotkey_listener),
            update_available: None,
            update_check_rx: Some(update_check_rx),
            error_message: None,
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

impl eframe::App for App {
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
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        let s: &'static Strings = self.config.lang().strings();

        // In-memory only -- see the `last_window_pos` field doc for why this
        // isn't saved to config.toml here.
        if let Some(outer_rect) = ctx.input(|i| i.viewport().outer_rect) {
            self.last_window_pos = Some(outer_rect.min);
        }

        self.poll_background_work(ctx, s);

        self.render_menu_bar(ui, s);
        self.render_source_panel(ui, s);
        self.render_center_panel(ui, s);
        self.render_overlay_viewport(ctx, s);
        self.render_audio_popup(ctx, s);
        self.render_quality_popup(ctx, s);
        self.render_hotkeys_popup(ctx, s);
        self.render_error_banner(ctx, s);
        self.render_self_update_popup(ctx, s);

        self.request_repaints(ctx);
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
