mod hotkeys_popup;

mod actions;
mod audio_popup;
mod background;
mod overlay;
mod panel_center;
mod panel_menu;
mod panel_source;
mod popups;
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
use hotkeys_popup::HotkeySlot;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::mpsc;
use std::time::Instant;
use theme::{setup_fonts, setup_theme};

enum ExportState {
    Idle,
    Running,
    Done(PathBuf),
    Failed(String),
}

enum HighlightSaveState {
    Idle,
    Saving,
    Done(PathBuf),
    Failed(String),
}

enum SelfUpdateState {
    Idle,
    Confirming(crate::update_check::AvailableUpdate),
    Working,
    Failed(String),
}

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
    show_quality_popup: bool,
    show_hotkeys_popup: bool,
    show_audio_popup: bool,
    /// "+ Add app" opens this inline instead of going straight to a native
    /// file browser -- lets an app be found and pinned via
    /// `Config::register_app_audio` by searching (currently open windows,
    /// or installed-but-not-running apps found via Start Menu shortcuts)
    /// instead of needing to know where it's installed. A native file
    /// browser remains as a fallback ("Browse for .exe instead…") for the
    /// rare app neither search source finds.
    show_add_app_picker: bool,
    /// Live text from the add-app picker's search box -- filters both
    /// currently open windows and installed-but-not-running apps by
    /// exe/display name, same convention as `source_filter`.
    add_app_search: String,
    /// Installed apps found by resolving Start Menu shortcuts, so the add-app
    /// picker can also find one that isn't running yet -- scanned once when
    /// the picker opens (`enumerate_installed_apps` walks every `.lnk` under
    /// two Start Menu folders and resolves each via COM, too slow to redo
    /// every frame), not refreshed again until it's reopened.
    add_app_installed: Vec<crate::sources::InstalledApp>,
    source_icon_textures: std::collections::HashMap<usize, egui::TextureHandle>,
    frame_count: Arc<AtomicU64>,
    recording_start: Option<Instant>,
    last_output_path: Option<PathBuf>,
    output_dir_input: String,
    /// Free space on `config.output_dir`'s volume, refreshed periodically
    /// (see `refresh_free_space`) rather than on every frame -- `GetDiskFreeSpaceExW`
    /// is fast, but there's no reason to make a syscall 30-60+ times a second
    /// for a number that only meaningfully changes over seconds, same
    /// reasoning as the recording loop's own `DISK_CHECK_INTERVAL`.
    free_space_bytes: Option<u64>,
    free_space_checked_at: Option<Instant>,
    free_space_checked_dir: Option<PathBuf>,
    /// Last time `refresh_sources_and_audio_if_due` actually ran (see there
    /// for why this is throttled rather than run every frame).
    sources_checked_at: Option<Instant>,
    /// How many audio streams the last finished recording actually contains,
    /// probed from the file itself (see `remux::count_audio_tracks`) rather
    /// than trusted from the pre-recording device selection. Export controls
    /// only make sense with 2+ tracks to choose between -- with 0 or 1,
    /// there's nothing a track-selection export could meaningfully remove.
    export_available_tracks: usize,
    export_track_selection: Vec<bool>,
    export_state: ExportState,
    export_result_rx: Option<mpsc::Receiver<Result<PathBuf, String>>>,
    hotkey_listener: Option<HotkeyListener>,
    finalizing_handle: Option<tokio::task::JoinHandle<Result<PathBuf, crate::error::AppError>>>,
    finalizing_path: Option<PathBuf>,
    finalizing_disk_full: Option<Arc<std::sync::atomic::AtomicBool>>,
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
    highlight_save_state: HighlightSaveState,
    highlight_save_handle: Option<tokio::task::JoinHandle<Result<PathBuf, crate::error::AppError>>>,
    self_update_state: SelfUpdateState,
    self_update_handle: Option<tokio::task::JoinHandle<Result<(), crate::error::AppError>>>,
    /// Window position as of the most recent frame -- updated every frame in
    /// `ui()` (cheap, in-memory only) and written to `config.toml` exactly
    /// once, in `on_exit()`, rather than on every change. A window drag can
    /// generate far more position-changed frames than e.g. the volume
    /// slider's bounded ~200 steps, so save-on-every-change would be
    /// needlessly write-heavy here.
    last_window_pos: Option<egui::Pos2>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, config: Config) -> Self {
        setup_theme(&cc.egui_ctx);
        setup_fonts(&cc.egui_ctx);
        let overlay_enabled = config.overlay.enabled;
        let app_audio_only = config.default_app_audio_only;
        let output_dir_input = config.output_dir.to_string_lossy().into_owned();
        let audio_devices = enumerate_audio_devices().unwrap_or_default();
        let n = audio_devices.len();
        // Default to loopback (system/game audio) only. The MP4 container's physical
        // stream order doesn't follow AddStream() call order (see writer.rs/remux.rs),
        // so when multiple audio tracks are muxed, naive players picking "the first
        // audio stream" can land on a different track than intended. Recording just
        // the loopback device by default keeps the common case to a single,
        // deterministically audible track; mic capture remains an opt-in checkbox.
        let selected_audio: Vec<bool> = audio_devices.iter().map(|d| d.is_loopback).collect();
        // Curated entirely by config -- see `actions::build_app_audio_sources`'s
        // doc comment for why this doesn't auto-populate from whatever's
        // currently making sound. Every entry is registered by
        // construction, so it's always checked.
        let app_audio_sources = actions::build_app_audio_sources(&config);
        let selected_app_audio = vec![true; app_audio_sources.len()];
        let export_track_selection = vec![true; n];
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
            show_quality_popup: false,
            show_hotkeys_popup: false,
            show_audio_popup: false,
            show_add_app_picker: false,
            add_app_search: String::new(),
            add_app_installed: Vec::new(),
            source_icon_textures: std::collections::HashMap::new(),
            frame_count: Arc::new(AtomicU64::new(0)),
            recording_start: None,
            last_output_path: None,
            output_dir_input,
            free_space_bytes: None,
            free_space_checked_at: None,
            free_space_checked_dir: None,
            sources_checked_at: None,
            export_available_tracks: 0,
            export_track_selection,
            export_state: ExportState::Idle,
            export_result_rx: None,
            hotkey_listener: Some(hotkey_listener),
            finalizing_handle: None,
            finalizing_path: None,
            finalizing_disk_full: None,
            update_available: None,
            update_check_rx: Some(update_check_rx),
            error_message: None,
            recording_hotkey: None,
            hotkey_capture_warning: None,
            highlight_save_state: HighlightSaveState::Idle,
            highlight_save_handle: None,
            self_update_state: SelfUpdateState::Idle,
            self_update_handle: None,
            last_window_pos: None,
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
