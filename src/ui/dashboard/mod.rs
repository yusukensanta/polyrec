mod hotkeys_popup;

use crate::capture::audio::enumerate_audio_devices;
use crate::config::Config;
use crate::encode::remux::remux;
use crate::hotkeys::{HotkeyEvent, HotkeyListener};
use crate::i18n::Strings;
use crate::session::{state::SessionAction, EncodeSettings, SessionManager};
use crate::sources::enumerate_sources;
use crate::types::{AudioDevice, CaptureSource, SessionState};
use eframe::egui;
use hotkeys_popup::HotkeySlot;
use rfd::FileDialog;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Instant;

// WCAG 2.2 AA palette — all contrast ratios verified against BG_BASE (rgb 18,18,28)
const BG_DEEP: egui::Color32 = egui::Color32::from_rgb(10, 10, 16);
const BG_WINDOW: egui::Color32 = egui::Color32::from_rgb(22, 22, 34);
const BG_FAINT: egui::Color32 = egui::Color32::from_rgb(14, 14, 22);
const BG_BASE: egui::Color32 = egui::Color32::from_rgb(18, 18, 28);
const BG_CARD: egui::Color32 = egui::Color32::from_rgb(26, 26, 40);
const BG_SELECTED: egui::Color32 = egui::Color32::from_rgb(38, 38, 66);
const BORDER: egui::Color32 = egui::Color32::from_rgb(40, 40, 60);
const BORDER_SEL: egui::Color32 = egui::Color32::from_rgb(90, 90, 190);
const ACCENT_SECONDARY: egui::Color32 = egui::Color32::from_rgb(0x86, 0x86, 0xCF);
const TEXT_PRIMARY: egui::Color32 = egui::Color32::from_rgb(220, 220, 235);
// 140/140/165, not 130/130/155: the darker value passed WCAG AA (4.5:1) on panel
// backgrounds but only hit 4.49:1 on button fill (28,28,44) -- brightening it here
// only ever increases contrast on every other background it's already used on.
const TEXT_MUTED: egui::Color32 = egui::Color32::from_rgb(140, 140, 165);
const ACCENT_REC: egui::Color32 = egui::Color32::from_rgb(248, 80, 80);
const ACCENT_IDLE: egui::Color32 = egui::Color32::from_rgb(74, 222, 128);
const ACCENT_PAUSE: egui::Color32 = egui::Color32::from_rgb(224, 178, 56);
const BG_BTN_STOP: egui::Color32 = egui::Color32::from_rgb(52, 18, 18);
const BG_BTN_IDLE: egui::Color32 = egui::Color32::from_rgb(18, 46, 28);
/// Larger, pill-like rounding used only for the primary REC/STOP/RESUME
/// action buttons, so they read as the one clearly primary action instead
/// of blending into every other rounded rectangle in the UI.
const ROUNDING_PRIMARY_BTN: u8 = 14;

enum ExportState {
    Idle,
    Running,
    Done(PathBuf),
    Failed(String),
}

pub struct App {
    config: Config,
    session: SessionManager,
    sources: Vec<CaptureSource>,
    selected_source: Option<usize>,
    audio_devices: Vec<AudioDevice>,
    selected_audio: Vec<bool>,
    app_audio_only: bool,
    overlay_enabled: bool,
    show_quality_popup: bool,
    show_hotkeys_popup: bool,
    source_icon_textures: std::collections::HashMap<usize, egui::TextureHandle>,
    frame_count: Arc<AtomicU64>,
    recording_start: Option<Instant>,
    last_output_path: Option<PathBuf>,
    output_dir_input: String,
    show_export_dialog: bool,
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
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, config: Config) -> Self {
        setup_theme(&cc.egui_ctx);
        setup_fonts(&cc.egui_ctx);
        let overlay_enabled = config.overlay.enabled;
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
        let export_track_selection = vec![true; n];
        let hotkey_listener = HotkeyListener::spawn(
            &config.hotkeys.start_stop,
            &config.hotkeys.pause,
            &config.hotkeys.toggle_overlay,
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
            audio_devices,
            selected_audio,
            app_audio_only: false,
            overlay_enabled,
            show_quality_popup: false,
            show_hotkeys_popup: false,
            source_icon_textures: std::collections::HashMap::new(),
            frame_count: Arc::new(AtomicU64::new(0)),
            recording_start: None,
            last_output_path: None,
            output_dir_input,
            show_export_dialog: false,
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

        self.poll_background_work(s);

        self.render_menu_bar(ui, s);
        self.render_source_panel(ui, s);
        self.render_center_panel(ui, s);
        self.render_overlay_viewport(ctx, s);
        self.render_quality_popup(ctx, s);
        self.render_hotkeys_popup(ctx, s);
        self.render_error_banner(ctx, s);
        self.render_export_dialog(ctx, s);

        self.request_repaints(ctx);
    }

    fn on_exit(&mut self) {
        if let Some(h) = self.hotkey_listener.take() {
            h.stop();
        }
    }
}

impl App {
    /// Polls every non-UI background channel/state transition for this frame:
    /// the one-shot update-check result, the export-remux result, hotkey
    /// events, the recorder stopping itself early (disk full), and a
    /// just-finished recorder's finalize result. None of this renders
    /// anything -- it only updates `self` before the render_* methods below
    /// read it.
    fn poll_background_work(&mut self, s: &'static Strings) {
        let is_recording = self.session.is_recording();

        // Poll update-check result (one-shot; None result also clears the receiver
        // so we stop polling a channel whose sender has already sent its one message)
        if let Some(rx) = &self.update_check_rx
            && let Ok(result) = rx.try_recv()
        {
            self.update_available = result;
            self.update_check_rx = None;
        }

        // Poll export result channel
        let export_result = self.export_result_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(result) = export_result {
            self.export_state = match result {
                Ok(path) => ExportState::Done(path),
                Err(msg) => ExportState::Failed(msg),
            };
            self.export_result_rx = None;
        }

        // Poll hotkey events (non-blocking)
        while let Some(event) = self.hotkey_listener.as_ref().and_then(|h| h.try_recv()) {
            match event {
                HotkeyEvent::StartStop => self.handle_hotkey_start_stop(is_recording),
                HotkeyEvent::Pause => self.handle_pause_button(),
                HotkeyEvent::ToggleOverlay => {
                    self.overlay_enabled = !self.overlay_enabled;
                    self.config.overlay.enabled = self.overlay_enabled;
                }
            }
        }

        // The recorder can stop itself early (disk full — see disk_space.rs)
        // without the user pressing stop. Detect that and run the normal stop
        // sequence so the now-pointless capture/pump tasks get aborted and the
        // result flows through the same finalize handling below as any other
        // stop, instead of the UI being stuck showing "Recording" forever for
        // a capture that already ended on its own.
        if (is_recording || self.session.is_paused())
            && self.finalizing_handle.is_none()
            && self
                .session
                .active
                .as_ref()
                .is_some_and(|a| a.recorder_handle.is_finished())
        {
            self.stop_recording();
        }

        // Show export dialog once recorder has finished writing the file
        if self.finalizing_handle.as_ref().is_some_and(|h| h.is_finished()) {
            let handle = self.finalizing_handle.take().unwrap();
            self.finalizing_path = None;
            let disk_full = self
                .finalizing_disk_full
                .take()
                .is_some_and(|f| f.load(Ordering::Relaxed));
            match tokio::runtime::Handle::current().block_on(handle) {
                Ok(Ok(path)) => {
                    self.last_output_path = Some(path);
                    self.show_export_dialog = true;
                    if disk_full {
                        self.error_message = Some(s.disk_full_mid_recording.to_string());
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!("recording finalize failed: {e}");
                    self.show_export_dialog = false;
                    self.error_message = Some(format!("{}{e}", s.recording_failed_prefix));
                }
                Err(e) => {
                    tracing::error!("recorder task did not complete cleanly: {e}");
                    self.show_export_dialog = false;
                    self.error_message = Some(format!("{}{e}", s.recording_ended_unexpectedly_prefix));
                }
            }
        }
    }

    // Takes `&mut egui::Ui` (the root Ui from App::ui), not `&egui::Context` --
    // Panel's old ctx-based top-level `.show(ctx, ...)` (what
    // TopBottomPanel/SidePanel used to be deprecated aliases for) was removed
    // entirely in egui 0.35; the replacement needs an existing Ui to nest
    // inside. CentralPanel takes the same shape for consistency.
    fn render_menu_bar(&mut self, ui: &mut egui::Ui, s: &'static Strings) {
        egui::Panel::top("menu_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("PolyRec");
                // Single source of truth for the app's version: Cargo.toml's `version`
                // field, baked in at compile time. Never hardcode a version string
                // elsewhere — the release CI also checks the git tag against this
                // same field before publishing, so this label always matches what
                // update_check compares against.
                ui.label(
                    egui::RichText::new(concat!("v", env!("CARGO_PKG_VERSION")))
                        .size(11.0)
                        .color(TEXT_MUTED),
                );
                ui.separator();
                if ui.add(accent_button(s.refresh, ACCENT_SECONDARY)).clicked() {
                    self.sources = enumerate_sources();
                    self.source_icon_textures.clear();
                    self.selected_source = None;
                    self.audio_devices = enumerate_audio_devices().unwrap_or_default();
                    let n = self.audio_devices.len();
                    self.selected_audio = self.audio_devices.iter().map(|d| d.is_loopback).collect();
                    self.export_track_selection = vec![true; n];
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if self.overlay_enabled { s.overlay_on } else { s.overlay_off };
                    if ui.add(accent_button(label, ACCENT_SECONDARY)).clicked() {
                        self.overlay_enabled = !self.overlay_enabled;
                        self.config.overlay.enabled = self.overlay_enabled;
                    }
                    let lang = self.config.lang();
                    if ui.add(accent_button(lang.toggle_button_label(), ACCENT_SECONDARY)).clicked() {
                        self.config.language = lang.toggle().config_value().to_string();
                        if let Err(e) = self.config.save() {
                            tracing::error!("failed to save config: {e}");
                        }
                    }
                    if let Some(update) = &self.update_available {
                        let update_url = update.url.clone();
                        let clicked = ui
                            .add(accent_button(&format!("⬆ {} {}", update.version, s.update_available_suffix), ACCENT_SECONDARY))
                            .on_hover_text(s.update_tooltip)
                            .clicked();
                        if clicked {
                            open_url(&update_url);
                        }
                    }
                });
            });
        });
    }

    fn render_source_panel(&mut self, ui: &mut egui::Ui, s: &'static Strings) {
        egui::Panel::left("source_panel")
            .default_size(260.0)
            .size_range(200.0..=380.0)
            .show(ui, |ui| {
                section_header(ui, s.capture_source_header);
                if self.sources.is_empty() {
                    ui.label(
                        egui::RichText::new(s.no_windows_found)
                            .size(12.0)
                            .color(TEXT_MUTED),
                    );
                }
                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .show(ui, |ui| {
                        for (i, source) in self.sources.iter().enumerate() {
                            let selected = self.selected_source == Some(i);
                            let fill   = if selected { BG_SELECTED } else { BG_CARD };
                            let border = if selected { BORDER_SEL } else { BORDER };

                            if let std::collections::hash_map::Entry::Vacant(entry) =
                                self.source_icon_textures.entry(i)
                                && let Some((rgba, w, h)) = &source.icon_rgba
                            {
                                let image = egui::ColorImage::from_rgba_unmultiplied([*w as usize, *h as usize], rgba);
                                let tex = ui.ctx().load_texture(
                                    format!("source_icon_{i}"),
                                    image,
                                    egui::TextureOptions::LINEAR,
                                );
                                entry.insert(tex);
                            }

                            let inner = egui::Frame::NONE
                                .fill(fill)
                                .stroke(egui::Stroke::new(1.0, border))
                                .corner_radius(6u8)
                                .inner_margin(8i8)
                                .show(ui, |ui| {
                                    ui.set_min_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        if let Some(tex) = self.source_icon_textures.get(&i) {
                                            ui.image((tex.id(), egui::vec2(16.0, 16.0)));
                                        }
                                        ui.vertical(|ui| {
                                            ui.label(
                                                egui::RichText::new(&source.window_title)
                                                    .size(13.0)
                                                    .strong()
                                                    .color(TEXT_PRIMARY),
                                            );
                                            if !source.exe_name.is_empty() {
                                                ui.label(
                                                    egui::RichText::new(&source.exe_name)
                                                        .size(11.0)
                                                        .color(TEXT_MUTED),
                                                );
                                            }
                                        });
                                    });
                                });

                            let response = inner
                                .response
                                .interact(egui::Sense::click())
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            if response.clicked() {
                                self.selected_source = Some(i);
                            }
                            ui.add_space(4.0);
                        }
                    });

                ui.add_space(12.0);
                section_header(ui, s.audio_header);
                if self.audio_devices.is_empty() {
                    ui.label(
                        egui::RichText::new(s.no_audio_devices)
                            .size(12.0)
                            .color(TEXT_MUTED),
                    );
                }
                for (i, dev) in self.audio_devices.iter().enumerate() {
                    ui.checkbox(
                        &mut self.selected_audio[i],
                        format!("{} {}", audio_device_icon(dev), dev.name),
                    );
                }

                let loopback_selected = self
                    .audio_devices
                    .iter()
                    .zip(self.selected_audio.iter())
                    .any(|(dev, &sel)| dev.is_loopback && sel);
                let has_loopback_device = self.audio_devices.iter().any(|d| d.is_loopback);
                let has_source = self.selected_source.is_some();
                ui.add_enabled_ui(loopback_selected && has_source, |ui| {
                    ui.checkbox(
                        &mut self.app_audio_only,
                        egui::RichText::new(s.app_audio_only_label)
                            .color(ACCENT_SECONDARY),
                    )
                    .on_hover_text(if !has_loopback_device {
                        s.tooltip_no_loopback_device
                    } else if !loopback_selected {
                        s.tooltip_check_loopback_first
                    } else {
                        s.tooltip_app_audio_only
                    });
                });
            });
    }

    fn render_center_panel(&mut self, ui: &mut egui::Ui, s: &'static Strings) {
        let is_recording = self.session.is_recording();
        let frames = self.frame_count.load(Ordering::Relaxed);

        egui::CentralPanel::default().show(ui, |ui| {
            // Cap content width instead of letting it stretch to fill however
            // wide the window happens to be resized -- actual content here
            // (status text, the two settings buttons, the output-dir row) has
            // a natural width well under this; without the cap, widening the
            // window just reopens empty right-side space rather than the
            // window becoming more useful.
            ui.set_max_width(440.0);
            section_header(ui, s.status_header);

            let is_paused = self.session.is_paused();
            if is_recording || is_paused {
                let elapsed = self
                    .session
                    .active
                    .as_ref()
                    .map(|a| a.clock.elapsed())
                    .unwrap_or_default();
                let secs = elapsed.as_secs();

                // Pulsing dot — alpha oscillates 56%–100% (never fully off)
                let t = ui.ctx().input(|i| i.time) as f32;
                let alpha = ((t * 1.8_f32).sin() * 0.22 + 0.78).clamp(0.0, 1.0);
                let dot_col = egui::Color32::from_rgba_unmultiplied(
                    ACCENT_REC.r(), ACCENT_REC.g(), ACCENT_REC.b(), (alpha * 255.0) as u8,
                );
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(14.0, 14.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().circle_filled(rect.center(), 5.0, dot_col);
                    let state_label = if is_paused { s.state_paused } else { s.state_recording };
                    // Named tokens, not ad-hoc hex — also fixes RECORDING's label color,
                    // which previously computed to 4.18:1 contrast on BG_BASE (fails
                    // WCAG AA's 4.5:1 minimum for normal text). ACCENT_REC is 5.54:1.
                    let state_color = if is_paused { ACCENT_PAUSE } else { ACCENT_REC };
                    ui.label(
                        egui::RichText::new(state_label)
                            .size(10.0)
                            .color(state_color)
                            .strong(),
                    );
                });

                ui.add_space(8.0);

                // Large monospace timer
                ui.label(
                    egui::RichText::new(format!(
                        "{:02}:{:02}:{:02}",
                        secs / 3600,
                        (secs % 3600) / 60,
                        secs % 60,
                    ))
                    .font(egui::FontId::monospace(40.0))
                    .color(TEXT_PRIMARY),
                );

                ui.add_space(4.0);

                // Stats row
                let track_count = self.selected_audio.iter().filter(|&&b| b).count();
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("{track_count} {}", s.tracks_word))
                            .size(12.0)
                            .color(TEXT_MUTED),
                    );
                    ui.label(egui::RichText::new("  ·  ").size(12.0).color(TEXT_MUTED));
                    ui.label(
                        egui::RichText::new(format!("{frames} {}", s.frames_word))
                            .size(12.0)
                            .color(TEXT_MUTED),
                    );
                });

                if let Some(active) = self.session.active.as_ref() {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(
                            active
                                .output_path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("recording.mp4"),
                        )
                        .size(11.0)
                        .color(TEXT_MUTED),
                    );
                }
            } else if self.finalizing_handle.is_some() {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(s.saving_recording)
                        .size(13.0)
                        .color(TEXT_MUTED),
                );
            } else if let Some(path) = &self.last_output_path {
                ui.label(egui::RichText::new(s.last_recording_label).size(11.0).color(TEXT_MUTED));
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(path.to_string_lossy().as_ref())
                        .size(12.0)
                        .color(TEXT_PRIMARY),
                );
            } else {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(s.select_source_prompt)
                        .size(13.0)
                        .color(TEXT_MUTED),
                );
            }

            ui.add_space(16.0);
            section_header(ui, s.output_header);

            ui.horizontal(|ui| {
                if ui
                    .add(accent_button(s.quality_button, ACCENT_SECONDARY))
                    .on_hover_text(s.quality_tooltip)
                    .clicked()
                {
                    self.show_quality_popup = true;
                }
                if ui
                    .add(accent_button(s.hotkeys_button, ACCENT_SECONDARY))
                    .on_hover_text(s.hotkeys_tooltip)
                    .clicked()
                {
                    self.show_hotkeys_popup = true;
                }
            });
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                let btn_width = 74.0;
                let tf_width = (ui.available_width() - btn_width - 8.0).max(60.0);
                let tf = ui.add_sized(
                    [tf_width, 22.0],
                    egui::TextEdit::singleline(&mut self.output_dir_input),
                );
                if tf.lost_focus() && !self.output_dir_input.trim().is_empty() {
                    self.config.output_dir = PathBuf::from(&self.output_dir_input);
                    if let Err(e) = self.config.save() {
                        tracing::error!("failed to save config: {e}");
                    }
                }
                if ui.add(accent_button(s.browse_button, ACCENT_SECONDARY)).clicked()
                    && let Some(path) = FileDialog::new()
                        .set_directory(&self.config.output_dir)
                        .pick_folder()
                {
                    self.output_dir_input = path.to_string_lossy().into_owned();
                    self.config.output_dir = path;
                    if let Err(e) = self.config.save() {
                        tracing::error!("failed to save config: {e}");
                    }
                }
            });

            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                ui.add_space(8.0);

                let is_paused = self.session.is_paused();

                let state_name = match self.session.state() {
                    SessionState::Idle => s.session_state_idle,
                    SessionState::Recording => s.session_state_recording,
                    SessionState::Paused => s.session_state_paused,
                };
                ui.label(
                    egui::RichText::new(format!("{}{state_name}", s.state_prefix))
                        .size(10.0)
                        .color(TEXT_MUTED),
                );

                if is_paused {
                    let btn = egui::Button::new(
                        egui::RichText::new(s.resume_button).color(ACCENT_IDLE).size(18.0),
                    )
                    .fill(BG_BTN_IDLE)
                    .corner_radius(ROUNDING_PRIMARY_BTN)
                    .min_size(egui::Vec2::new(130.0, 52.0));
                    if ui.add(btn).clicked() {
                        self.handle_pause_button();
                    }
                } else if is_recording {
                    ui.horizontal(|ui| {
                        let stop_btn = egui::Button::new(
                            egui::RichText::new(s.stop_button)
                                .color(ACCENT_REC)
                                .size(18.0),
                        )
                        .fill(BG_BTN_STOP)
                        .corner_radius(ROUNDING_PRIMARY_BTN)
                        .min_size(egui::Vec2::new(90.0, 52.0));
                        if ui.add(stop_btn).clicked() {
                            self.handle_rec_button(is_recording);
                        }

                        let pause_btn = egui::Button::new(
                            egui::RichText::new("⏸").color(TEXT_MUTED).size(18.0),
                        )
                        .fill(egui::Color32::from_rgb(30, 30, 46))
                        .min_size(egui::Vec2::new(44.0, 52.0));
                        if ui.add(pause_btn).on_hover_text(s.pause_tooltip).clicked() {
                            self.handle_pause_button();
                        }
                    });
                } else {
                    let btn = egui::Button::new(
                        egui::RichText::new(s.rec_button).color(ACCENT_IDLE).size(18.0),
                    )
                    .fill(BG_BTN_IDLE)
                    .corner_radius(ROUNDING_PRIMARY_BTN)
                    .min_size(egui::Vec2::new(130.0, 52.0));
                    if ui.add(btn).clicked() {
                        self.handle_rec_button(is_recording);
                    }
                }
            });
        });
    }

    /// Second OS window (click-through, always-on-top) showing a compact
    /// timer/track-count HUD while recording -- opt-in via the Overlay toggle
    /// in the menu bar.
    fn render_overlay_viewport(&mut self, ctx: &egui::Context, s: &'static Strings) {
        let is_recording = self.session.is_recording();
        if !(is_recording && self.overlay_enabled) {
            return;
        }

        let track_count = self.selected_audio.iter().filter(|&&b| b).count();
        let elapsed_secs = self
            .session
            .active
            .as_ref()
            .map(|a| a.clock.elapsed().as_secs())
            .unwrap_or(0);

        let screen_w = unsafe {
            windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN,
            ) as f32
        };
        let tracks_word = s.tracks_word;
        let stop_word = s.overlay_hud_stop_word;
        // Reflects whatever start/stop is actually bound to right now, not a
        // hardcoded default -- it's freely rebindable (see render_hotkeys_popup).
        let stop_key = self.config.hotkeys.start_stop.clone();

        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("polyrec_overlay"),
            egui::ViewportBuilder::default()
                .with_title("PolyRec Overlay")
                .with_always_on_top()
                .with_decorations(false)
                .with_transparent(true)
                .with_mouse_passthrough(true)
                // Widened from 310 to fit longer modifier combos (e.g.
                // "CTRL+ALT+SHIFT+F9") without clipping the stop-key hint.
                .with_inner_size([400.0, 32.0])
                .with_position(egui::pos2(screen_w - 410.0, 10.0)),
            move |ctx, _class| {
                // CentralPanel::show(ctx, ...) is soft-deprecated in favor of
                // show_inside(ui, ...), but this closure only ever receives a
                // fresh viewport's &Context -- there's no pre-existing Ui to
                // nest inside here, so the ctx-based form is unavoidable.
                #[allow(deprecated)]
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::NONE
                            .fill(egui::Color32::from_rgba_premultiplied(20, 20, 20, 200))
                            .inner_margin(6i8),
                    )
                    .show(ctx, |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "● {:02}:{:02}:{:02}  |  {track_count} {tracks_word}  |  {stop_key} {stop_word}",
                                elapsed_secs / 3600,
                                (elapsed_secs % 3600) / 60,
                                elapsed_secs % 60,
                            ))
                            .color(egui::Color32::WHITE)
                            .size(13.0),
                        );
                    });
            },
        );
    }

    fn render_quality_popup(&mut self, ctx: &egui::Context, s: &'static Strings) {
        if !self.show_quality_popup {
            return;
        }
        let mut close = false;
        egui::Window::new(s.quality_title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                section_header(ui, s.fps_header);
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.config.encode.fps, 30, "30");
                    ui.selectable_value(&mut self.config.encode.fps, 60, "60");
                });

                section_header(ui, s.codec_header);
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.config.encode.codec, "h264".into(), "H264");
                    ui.selectable_value(&mut self.config.encode.codec, "h265".into(), "H265");
                });

                section_header(ui, s.resolution_header);
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.config.encode.resolution_mode, "native".into(), s.resolution_native);
                    ui.selectable_value(&mut self.config.encode.resolution_mode, "display".into(), s.resolution_display);
                    ui.selectable_value(&mut self.config.encode.resolution_mode, "custom".into(), s.resolution_custom);
                });
                if self.config.encode.resolution_mode == "custom" {
                    ui.horizontal(|ui| {
                        ui.label(s.width_label);
                        ui.add(egui::DragValue::new(&mut self.config.encode.custom_width).range(2..=7680));
                        ui.label(s.height_label);
                        ui.add(egui::DragValue::new(&mut self.config.encode.custom_height).range(2..=4320));
                    });
                }

                section_header(ui, s.bitrate_header);
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.config.encode.bitrate_mode, "auto".into(), s.bitrate_auto);
                    ui.selectable_value(&mut self.config.encode.bitrate_mode, "manual".into(), s.bitrate_manual);
                });
                if self.config.encode.bitrate_mode == "manual" {
                    ui.horizontal(|ui| {
                        ui.label(s.mbps_label);
                        ui.add(egui::DragValue::new(&mut self.config.encode.manual_bitrate_mbps).range(1..=100));
                    });
                }

                ui.add_space(8.0);
                if ui.add(accent_button(s.close_button, TEXT_MUTED)).clicked() {
                    close = true;
                }
            });
        if close {
            self.show_quality_popup = false;
            if let Err(e) = self.config.save() {
                tracing::error!("failed to save config: {e}");
            }
        }
    }

    fn render_error_banner(&mut self, ctx: &egui::Context, s: &'static Strings) {
        let Some(msg) = self.error_message.clone() else {
            return;
        };
        let mut close = false;
        egui::Window::new(s.error_title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(&msg).size(13.0).color(ACCENT_REC));
                ui.add_space(8.0);
                if ui.add(accent_button(s.close_button, TEXT_MUTED)).clicked() {
                    close = true;
                }
            });
        if close {
            self.error_message = None;
        }
    }

    fn render_export_dialog(&mut self, ctx: &egui::Context, s: &'static Strings) {
        if !self.show_export_dialog {
            return;
        }
        let Some(path) = self.last_output_path.clone() else {
            self.show_export_dialog = false;
            self.export_state = ExportState::Idle;
            self.export_result_rx = None;
            return;
        };

        let mut close = false;
        egui::Window::new(s.export_dialog_title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(s.recording_saved_label).size(11.0).color(TEXT_MUTED));
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(path.to_string_lossy().as_ref())
                        .size(12.0)
                        .color(TEXT_PRIMARY),
                );

                ui.add_space(8.0);
                section_header(ui, s.audio_tracks_header);
                for (i, dev) in self.audio_devices.iter().enumerate() {
                    if i < self.export_track_selection.len() {
                        ui.checkbox(
                            &mut self.export_track_selection[i],
                            format!("{} {}", audio_device_icon(dev), dev.name),
                        );
                    }
                }

                ui.add_space(8.0);

                let done_path = if let ExportState::Done(p) = &self.export_state {
                    Some(p.clone())
                } else {
                    None
                };
                let failed_msg = if let ExportState::Failed(m) = &self.export_state {
                    Some(m.clone())
                } else {
                    None
                };
                let is_idle = matches!(self.export_state, ExportState::Idle);
                let is_running = matches!(self.export_state, ExportState::Running);

                if is_idle {
                    ui.horizontal(|ui| {
                        if ui.add(accent_button(s.export_button, ACCENT_IDLE)).clicked()
                            && let Some(dest) = rfd::FileDialog::new()
                                .add_filter("MP4 video", &["mp4"])
                                .set_file_name("export.mp4")
                                .save_file()
                        {
                            let src = path.clone();
                            let indices: Vec<usize> = self
                                .export_track_selection
                                .iter()
                                .enumerate()
                                .filter(|&(_, &sel)| sel)
                                .map(|(i, _)| i)
                                .collect();
                            let (tx, rx) = mpsc::channel();
                            std::thread::spawn(move || {
                                let result = remux(&src, &dest, &indices)
                                    .map_err(|e| e.to_string());
                                let _ = tx.send(result);
                            });
                            self.export_result_rx = Some(rx);
                            self.export_state = ExportState::Running;
                        }
                        if ui.add(accent_button(s.open_folder_button, ACCENT_SECONDARY)).clicked() {
                            open_folder(path.as_ref());
                        }
                        if ui.add(accent_button(s.close_button, TEXT_MUTED)).clicked() {
                            close = true;
                        }
                    });
                } else if is_running {
                    section_header(ui, s.exporting_header);
                    ui.label(
                        egui::RichText::new(s.please_wait)
                            .size(11.0)
                            .color(TEXT_MUTED),
                    );
                } else if let Some(export_path) = done_path {
                    section_header(ui, s.export_complete_header);
                    ui.label(
                        egui::RichText::new(export_path.to_string_lossy().as_ref())
                            .size(11.0)
                            .color(TEXT_PRIMARY),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.add(accent_button(s.open_folder_button, ACCENT_SECONDARY)).clicked() {
                            open_folder(&export_path);
                        }
                        if ui.add(accent_button(s.close_button, TEXT_MUTED)).clicked() {
                            close = true;
                        }
                    });
                } else if let Some(msg) = failed_msg {
                    section_header(ui, s.export_failed_header);
                    ui.label(
                        egui::RichText::new(&msg)
                            .size(11.0)
                            .color(ACCENT_REC),
                    );
                    ui.add_space(4.0);
                    if ui.add(accent_button(s.close_button, TEXT_MUTED)).clicked() {
                        close = true;
                    }
                }
            });
        if close {
            self.show_export_dialog = false;
            self.export_state = ExportState::Idle;
            self.export_result_rx = None;
        }
    }

    fn request_repaints(&self, ctx: &egui::Context) {
        if self.session.is_recording() {
            // 33 ms ≈ 30 fps; needed for smooth pulsing dot animation
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
        if matches!(self.export_state, ExportState::Running) {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if self.session.is_paused() {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        if self.finalizing_handle.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

/// A button whose label is tinted to signal what kind of action it is, rather
/// than every button reading as identical plain text. Three tiers used
/// consistently across the app: ACCENT_IDLE/ACCENT_REC for the one primary
/// go/stop action per screen, ACCENT_SECONDARY for useful-but-not-primary
/// actions (opens a panel, refreshes, browses), TEXT_MUTED for low-emphasis
/// dismiss actions (Close). Explicit on every button rather than leaving some
/// unstyled — an unstyled button next to styled ones reads as an oversight,
/// not a deliberate "this one's less important."
fn accent_button(text: &str, color: egui::Color32) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.to_string()).color(color))
}

fn section_header(ui: &mut egui::Ui, title: &str) {
    // 14pt: up from 10pt (too small to read comfortably as a section label),
    // still clearly below the "PolyRec" heading's default 18pt so it reads as
    // a subordinate label, not competing with it.
    ui.add_space(4.0);
    ui.label(egui::RichText::new(title).size(14.0).color(TEXT_MUTED).strong());
    ui.add_space(2.0);
    ui.separator();
    ui.add_space(4.0);
}

/// Icon for an audio-device checkbox label — used both in the source panel's
/// device list and the export dialog's per-track selection.
fn audio_device_icon(dev: &AudioDevice) -> &'static str {
    if dev.is_loopback { "🔊" } else { "🎙" }
}

/// egui's bundled default font only covers Latin + a small symbol set — window
/// titles/exe names containing CJK or other multi-byte characters (and any
/// future localized UI text) render as tofu boxes without a fallback font.
/// Loads MS Gothic — a fixed-pitch (single-width per cell) CJK font bundled
/// with every Windows release since the 9x/NT era, so it's a correct fit for
/// the Monospace family (unlike a proportional font such as Yu Gothic) and
/// more universally present than newer CJK fonts — and appends it after the
/// default font in both families, so it's only used for glyphs the default
/// font can't cover; Latin text keeps its existing appearance. Best-effort:
/// if the font file isn't present on this Windows install, logs a warning
/// and leaves the default (Latin-only) fonts in place.
fn setup_fonts(ctx: &egui::Context) {
    const CJK_FONT_PATH: &str = r"C:\Windows\Fonts\msgothic.ttc";
    const CJK_FONT_KEY: &str = "cjk_fallback";

    let font_bytes = match std::fs::read(CJK_FONT_PATH) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!("CJK fallback font not loaded from {CJK_FONT_PATH}: {e}");
            return;
        }
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        CJK_FONT_KEY.to_owned(),
        egui::FontData::from_owned(font_bytes).into(),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push(CJK_FONT_KEY.to_owned());
    }
    ctx.set_fonts(fonts);
}

fn setup_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();

    // Background layers
    v.panel_fill         = BG_BASE;
    v.window_fill        = BG_WINDOW;
    v.extreme_bg_color   = BG_DEEP;
    v.faint_bg_color     = BG_FAINT;
    v.override_text_color = Some(TEXT_PRIMARY);

    // Window chrome
    v.window_corner_radius = egui::CornerRadius::same(10);

    // Widget rounding — consistent across all interaction states
    let r = egui::CornerRadius::same(5);
    v.widgets.noninteractive.corner_radius = r;
    v.widgets.inactive.corner_radius       = r;
    v.widgets.hovered.corner_radius        = r;
    v.widgets.active.corner_radius         = r;
    v.widgets.open.corner_radius           = r;

    // Subtle hover/active bg fills for checkboxes, buttons, etc.
    v.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(28, 28, 44);
    v.widgets.hovered.weak_bg_fill  = egui::Color32::from_rgb(38, 38, 58);
    v.widgets.active.weak_bg_fill   = egui::Color32::from_rgb(48, 48, 72);

    ctx.set_visuals(v);

    let mut s = (*ctx.global_style()).clone();
    // egui's default interact_size.y (18.0) is well under Fluent's 32px / Material's
    // 36-40dp desktop minimum control height — every button in the app (Refresh,
    // Browse, Quality, Close, etc.) was inheriting that undersized floor. 32.0 is
    // the Fluent minimum and lands on the 8px spacing grid used everywhere else here.
    s.spacing.interact_size  = egui::Vec2::new(40.0, 32.0);
    s.spacing.item_spacing   = egui::Vec2::new(8.0, 8.0);
    s.spacing.button_padding = egui::Vec2::new(14.0, 8.0);
    s.spacing.window_margin  = egui::Margin::same(16);
    s.spacing.indent         = 16.0;
    ctx.set_global_style(s);
}

/// Full path rather than a bare "explorer" name — avoids relying on Windows'
/// executable search order (a directory ahead of System32 in PATH could
/// otherwise shadow the real explorer.exe).
const EXPLORER_EXE: &str = r"C:\Windows\explorer.exe";

fn open_folder(path: &std::path::Path) {
    let folder = path.parent().unwrap_or(path);
    let _ = std::process::Command::new(EXPLORER_EXE).arg(folder).spawn();
}

/// Only ever called with a GitHub release page URL (see `update_check.rs`), but
/// validated anyway since it's the one place in the app that opens a string
/// pulled from a network response rather than a local path: an `explorer.exe`
/// argument that turned out to be a UNC path (`\\host\share`) rather than a URL
/// would make Explorer silently attempt an SMB connection using the current
/// Windows credentials -- a known NTLM-hash-leak technique. Requiring an
/// `https://github.com/` prefix rules that out.
fn open_url(url: &str) {
    if !url.starts_with("https://github.com/") {
        tracing::warn!("refusing to open unexpected update URL: {url}");
        return;
    }
    let _ = std::process::Command::new(EXPLORER_EXE).arg(url).spawn();
}

impl App {
    fn handle_pause_button(&mut self) {
        if self.session.is_recording() {
            self.session.pause_capture();
        } else if self.session.is_paused() {
            self.session.resume_capture();
        }
    }

    fn handle_rec_button(&mut self, is_recording: bool) {
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
    fn handle_hotkey_start_stop(&mut self, is_recording: bool) {
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

    fn stop_recording(&mut self) {
        let path = self
            .session
            .active
            .as_ref()
            .map(|a| a.output_path.clone());
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
        self.export_track_selection = self.selected_audio.clone();
    }

    fn start_recording_with_source(&mut self, source: CaptureSource) {
        let selected_devices: Vec<_> = self
            .audio_devices
            .iter()
            .zip(self.selected_audio.iter())
            .filter(|&(_, &sel)| sel)
            .map(|(dev, _)| dev.clone())
            .collect();
        let encode = EncodeSettings {
            codec: self.config.encode.codec.clone(),
            fps: self.config.encode.fps,
            resolution_mode: self.config.encode.resolution_mode(),
            bitrate_mode: self.config.encode.bitrate_mode(),
        };
        // Only transition to the Recording state once start_capture actually
        // succeeds -- otherwise a disk-full refusal would leave the UI showing
        // "Recording" for a capture that never started.
        match self.session.start_capture(
            source,
            selected_devices,
            self.app_audio_only,
            Arc::clone(&self.frame_count),
            &self.config.output_dir,
            encode,
        ) {
            Ok(_) => {
                self.session.apply(SessionAction::Start);
                self.recording_start = Some(Instant::now());
            }
            Err(e) => {
                let prefix = self.config.lang().strings().couldnt_start_recording_prefix;
                self.error_message = Some(format!("{prefix}{e}"));
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
