use crate::capture::audio::enumerate_audio_devices;
use crate::config::Config;
use crate::hotkeys::{HotkeyEvent, HotkeyListener};
use crate::session::{state::SessionAction, SessionManager};
use crate::sources::enumerate_sources;
use crate::types::{AudioDevice, CaptureSource};
use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

// WCAG 2.2 AA palette — all contrast ratios verified against BG_BASE (rgb 18,18,28)
const BG_DEEP:      egui::Color32 = egui::Color32::from_rgb(10, 10, 16);
const BG_BASE:      egui::Color32 = egui::Color32::from_rgb(18, 18, 28);
const BG_CARD:      egui::Color32 = egui::Color32::from_rgb(26, 26, 40);
const BG_SELECTED:  egui::Color32 = egui::Color32::from_rgb(38, 38, 66);
const BORDER:       egui::Color32 = egui::Color32::from_rgb(40, 40, 60);
const BORDER_SEL:   egui::Color32 = egui::Color32::from_rgb(90, 90, 190);
const TEXT_PRIMARY: egui::Color32 = egui::Color32::from_rgb(220, 220, 235);
const TEXT_MUTED:   egui::Color32 = egui::Color32::from_rgb(130, 130, 155);
const ACCENT_REC:   egui::Color32 = egui::Color32::from_rgb(248, 80, 80);
const ACCENT_IDLE:  egui::Color32 = egui::Color32::from_rgb(74, 222, 128);

pub struct App {
    config: Config,
    session: SessionManager,
    sources: Vec<CaptureSource>,
    selected_source: Option<usize>,
    audio_devices: Vec<AudioDevice>,
    selected_audio: Vec<bool>,
    overlay_enabled: bool,
    frame_count: Arc<AtomicU64>,
    recording_start: Option<Instant>,
    last_output_path: Option<PathBuf>,
    show_export_dialog: bool,
    export_track_selection: Vec<bool>,
    hotkey_listener: HotkeyListener,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, config: Config) -> Self {
        setup_theme(&cc.egui_ctx);
        let overlay_enabled = config.overlay.enabled;
        let audio_devices = enumerate_audio_devices().unwrap_or_default();
        let n = audio_devices.len();
        let selected_audio = vec![true; n];
        let export_track_selection = vec![true; n];
        let hotkey_listener = HotkeyListener::spawn(
            &config.hotkeys.start_stop,
            &config.hotkeys.pause,
            &config.hotkeys.toggle_overlay,
        );
        Self {
            config,
            session: SessionManager::new(),
            sources: enumerate_sources(),
            selected_source: None,
            audio_devices,
            selected_audio,
            overlay_enabled,
            frame_count: Arc::new(AtomicU64::new(0)),
            recording_start: None,
            last_output_path: None,
            show_export_dialog: false,
            export_track_selection,
            hotkey_listener,
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let is_recording = self.session.is_recording();
        let frames = self.frame_count.load(Ordering::Relaxed);

        // Poll hotkey events (non-blocking)
        while let Some(event) = self.hotkey_listener.try_recv() {
            match event {
                HotkeyEvent::StartStop => self.handle_rec_button(is_recording),
                HotkeyEvent::Pause => {}
                HotkeyEvent::ToggleOverlay => {
                    self.overlay_enabled = !self.overlay_enabled;
                    self.config.overlay.enabled = self.overlay_enabled;
                }
            }
        }

        // ── Menu bar ──────────────────────────────────────────────────────────
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("PolyRec");
                ui.separator();
                if ui.button("⟳ Refresh").clicked() {
                    self.sources = enumerate_sources();
                    self.selected_source = None;
                    self.audio_devices = enumerate_audio_devices().unwrap_or_default();
                    let n = self.audio_devices.len();
                    self.selected_audio = vec![true; n];
                    self.export_track_selection = vec![true; n];
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if self.overlay_enabled { "Overlay: ON" } else { "Overlay: OFF" };
                    if ui.button(label).clicked() {
                        self.overlay_enabled = !self.overlay_enabled;
                        self.config.overlay.enabled = self.overlay_enabled;
                    }
                });
            });
        });

        // ── Left panel ────────────────────────────────────────────────────────
        egui::SidePanel::left("source_panel")
            .min_width(200.0)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new("CAPTURE SOURCE").small().weak());
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .show(ui, |ui| {
                        for (i, source) in self.sources.iter().enumerate() {
                            let selected = self.selected_source == Some(i);
                            let label = format!("🎮 {}", source.window_title);
                            if ui.selectable_label(selected, &label).clicked() {
                                self.selected_source = Some(i);
                            }
                        }
                    });

                ui.add_space(8.0);
                ui.label(egui::RichText::new("AUDIO DEVICES").small().weak());
                ui.separator();
                for (i, dev) in self.audio_devices.iter().enumerate() {
                    let icon = if dev.is_loopback { "🔊" } else { "🎙" };
                    ui.checkbox(
                        &mut self.selected_audio[i],
                        format!("{icon} {}", dev.name),
                    );
                }
            });

        // ── Center panel ──────────────────────────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label(egui::RichText::new("RECORDING STATUS").small().weak());
            ui.separator();

            if is_recording {
                let elapsed = self
                    .recording_start
                    .map(|t| t.elapsed())
                    .unwrap_or_default();
                let secs = elapsed.as_secs();
                ui.label(format!(
                    "Recording  {:02}:{:02}:{:02}",
                    secs / 3600,
                    (secs % 3600) / 60,
                    secs % 60
                ));
                ui.label(format!(
                    "{} audio track(s)  |  {} video frames",
                    self.selected_audio.iter().filter(|&&b| b).count(),
                    frames
                ));
                if let Some(active) = self.session.active.as_ref() {
                    ui.label(
                        egui::RichText::new(
                            active
                                .output_path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("recording.mp4"),
                        )
                        .small()
                        .weak(),
                    );
                }
            } else if let Some(path) = &self.last_output_path {
                ui.label("Last recording:");
                ui.label(
                    egui::RichText::new(path.to_string_lossy().as_ref())
                        .small()
                        .weak(),
                );
            } else {
                ui.label("Select a source and press REC to start recording.");
            }

            ui.add_space(16.0);

            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                ui.add_space(8.0);

                let rec_label = if is_recording { "⏹ STOP" } else { "⏺ REC" };
                let rec_color = if is_recording {
                    egui::Color32::from_rgb(248, 113, 113)
                } else {
                    egui::Color32::from_rgb(74, 222, 128)
                };

                let btn = egui::Button::new(
                    egui::RichText::new(rec_label).color(rec_color).size(18.0),
                );

                if ui.add_sized([120.0, 48.0], btn).clicked() {
                    self.handle_rec_button(is_recording);
                }

                ui.label(
                    egui::RichText::new(format!("State: {:?}", self.session.state()))
                        .small()
                        .weak(),
                );
            });
        });

        // ── Overlay viewport (second OS window, click-through) ────────────────
        if is_recording && self.overlay_enabled {
            let track_count = self.selected_audio.iter().filter(|&&b| b).count();
            let elapsed_secs = self
                .recording_start
                .map(|t| t.elapsed().as_secs())
                .unwrap_or(0);

            let screen_w = unsafe {
                windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                    windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN,
                ) as f32
            };

            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("polyrec_overlay"),
                egui::ViewportBuilder::default()
                    .with_title("PolyRec Overlay")
                    .with_always_on_top()
                    .with_decorations(false)
                    .with_transparent(true)
                    .with_mouse_passthrough(true)
                    .with_inner_size([310.0, 32.0])
                    .with_position(egui::pos2(screen_w - 320.0, 10.0)),
                move |ctx, _class| {
                    egui::CentralPanel::default()
                        .frame(
                            egui::Frame::none()
                                .fill(egui::Color32::from_rgba_premultiplied(20, 20, 20, 200))
                                .inner_margin(egui::Margin::same(6.0)),
                        )
                        .show(ctx, |ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "● {:02}:{:02}:{:02}  |  {} tracks  |  F9 stop",
                                    elapsed_secs / 3600,
                                    (elapsed_secs % 3600) / 60,
                                    elapsed_secs % 60,
                                    track_count,
                                ))
                                .color(egui::Color32::WHITE)
                                .size(13.0),
                            );
                        });
                },
            );
        }

        // ── Export dialog ─────────────────────────────────────────────────────
        if self.show_export_dialog {
            if let Some(path) = self.last_output_path.clone() {
                let mut close = false;
                egui::Window::new("Recording Complete")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                    .show(ctx, |ui| {
                        ui.label("Recording saved:");
                        ui.label(
                            egui::RichText::new(path.to_string_lossy().as_ref())
                                .small()
                                .weak(),
                        );

                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("AUDIO TRACKS").small().weak());
                        for (i, dev) in self.audio_devices.iter().enumerate() {
                            if i < self.export_track_selection.len() {
                                let icon = if dev.is_loopback { "🔊" } else { "🎙" };
                                ui.checkbox(
                                    &mut self.export_track_selection[i],
                                    format!("{icon} {}", dev.name),
                                );
                            }
                        }

                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Open Folder").clicked() {
                                open_folder(path.as_ref());
                            }
                            if ui.button("Close").clicked() {
                                close = true;
                            }
                        });
                    });
                if close {
                    self.show_export_dialog = false;
                }
            } else {
                self.show_export_dialog = false;
            }
        }

        if is_recording {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
    }
}

fn setup_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();

    // Background layers
    v.panel_fill         = BG_BASE;
    v.window_fill        = egui::Color32::from_rgb(22, 22, 34);
    v.extreme_bg_color   = BG_DEEP;
    v.faint_bg_color     = egui::Color32::from_rgb(14, 14, 22);
    v.override_text_color = Some(TEXT_PRIMARY);

    // Window chrome
    v.window_rounding = egui::Rounding::same(10.0);

    // Widget rounding — consistent across all interaction states
    let r = egui::Rounding::same(5.0);
    v.widgets.noninteractive.rounding = r;
    v.widgets.inactive.rounding       = r;
    v.widgets.hovered.rounding        = r;
    v.widgets.active.rounding         = r;
    v.widgets.open.rounding           = r;

    // Subtle hover/active bg fills for checkboxes, buttons, etc.
    v.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(28, 28, 44);
    v.widgets.hovered.weak_bg_fill  = egui::Color32::from_rgb(38, 38, 58);
    v.widgets.active.weak_bg_fill   = egui::Color32::from_rgb(48, 48, 72);

    ctx.set_visuals(v);

    let mut s = (*ctx.style()).clone();
    s.spacing.item_spacing   = egui::Vec2::new(8.0, 5.0);
    s.spacing.button_padding = egui::Vec2::new(14.0, 7.0);
    s.spacing.window_margin  = egui::Margin::same(12.0);
    ctx.set_style(s);
}

fn open_folder(path: &std::path::Path) {
    let folder = path.parent().unwrap_or(path);
    let _ = std::process::Command::new("explorer").arg(folder).spawn();
}

impl App {
    fn handle_rec_button(&mut self, is_recording: bool) {
        if is_recording {
            let path = self
                .session
                .active
                .as_ref()
                .map(|a| a.output_path.clone());
            if self.session.apply(SessionAction::Stop) {
                self.session.stop_capture();
            }
            self.last_output_path = path.clone();
            self.recording_start = None;
            self.frame_count.store(0, Ordering::Relaxed);
            self.export_track_selection = self.selected_audio.clone();
            self.show_export_dialog = path.is_some();
        } else if let Some(idx) = self.selected_source {
            let Some(source) = self.sources.get(idx).cloned() else {
                self.selected_source = None;
                return;
            };
            let selected_devices: Vec<_> = self
                .audio_devices
                .iter()
                .zip(self.selected_audio.iter())
                .filter(|(_, &sel)| sel)
                .map(|(dev, _)| dev.clone())
                .collect();
            self.session.apply(SessionAction::Start);
            self.session.start_capture(
                source,
                selected_devices,
                Arc::clone(&self.frame_count),
            );
            self.recording_start = Some(Instant::now());
        }
    }
}
