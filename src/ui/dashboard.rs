use crate::capture::audio::enumerate_audio_devices;
use crate::config::Config;
use crate::encode::remux::remux;
use crate::hotkeys::{HotkeyEvent, HotkeyListener};
use crate::session::{state::SessionAction, EncodeSettings, SessionManager};
use crate::sources::enumerate_sources;
use crate::types::{AudioDevice, CaptureSource};
use eframe::egui;
use rfd::FileDialog;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Instant;

// WCAG 2.2 AA palette — all contrast ratios verified against BG_BASE (rgb 18,18,28)
                    const BG_DEEP:      egui::Color32 = egui::Color32::from_rgb(10, 10, 16);
                    const BG_WINDOW:    egui::Color32 = egui::Color32::from_rgb(22, 22, 34);
                    const BG_FAINT:     egui::Color32 = egui::Color32::from_rgb(14, 14, 22);
                    const BG_BASE:      egui::Color32 = egui::Color32::from_rgb(18, 18, 28);
                    const BG_CARD:      egui::Color32 = egui::Color32::from_rgb(26, 26, 40);
                    const BG_SELECTED:  egui::Color32 = egui::Color32::from_rgb(38, 38, 66);
                    const BORDER:       egui::Color32 = egui::Color32::from_rgb(40, 40, 60);
                    const BORDER_SEL:   egui::Color32 = egui::Color32::from_rgb(90, 90, 190);
                    const ACCENT_SECONDARY: egui::Color32 = egui::Color32::from_rgb(0x86, 0x86, 0xCF);
                    const TEXT_PRIMARY: egui::Color32 = egui::Color32::from_rgb(220, 220, 235);
                    const TEXT_MUTED:   egui::Color32 = egui::Color32::from_rgb(130, 130, 155);
                    const ACCENT_REC:   egui::Color32 = egui::Color32::from_rgb(248, 80, 80);
                    const ACCENT_IDLE:  egui::Color32 = egui::Color32::from_rgb(74, 222, 128);
                    const BG_BTN_STOP:  egui::Color32 = egui::Color32::from_rgb(52, 18, 18);
                    const BG_BTN_IDLE:  egui::Color32 = egui::Color32::from_rgb(18, 46, 28);

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
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, config: Config) -> Self {
        setup_theme(&cc.egui_ctx);
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
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let is_recording = self.session.is_recording();
        let frames = self.frame_count.load(Ordering::Relaxed);

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
                HotkeyEvent::StartStop => self.handle_rec_button(is_recording),
                HotkeyEvent::Pause => self.handle_pause_button(),
                HotkeyEvent::ToggleOverlay => {
                    self.overlay_enabled = !self.overlay_enabled;
                    self.config.overlay.enabled = self.overlay_enabled;
                }
            }
        }

        // Show export dialog once recorder has finished writing the file
        if self.finalizing_handle.as_ref().map_or(false, |h| h.is_finished()) {
            let handle = self.finalizing_handle.take().unwrap();
            self.finalizing_path = None;
            match tokio::runtime::Handle::current().block_on(handle) {
                Ok(Ok(path)) => {
                    self.last_output_path = Some(path);
                    self.show_export_dialog = true;
                }
                Ok(Err(e)) => {
                    tracing::error!("recording finalize failed: {e}");
                    self.show_export_dialog = false;
                }
                Err(e) => {
                    tracing::error!("recorder task did not complete cleanly: {e}");
                    self.show_export_dialog = false;
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
                    self.source_icon_textures.clear();
                    self.selected_source = None;
                    self.audio_devices = enumerate_audio_devices().unwrap_or_default();
                    let n = self.audio_devices.len();
                    self.selected_audio = self.audio_devices.iter().map(|d| d.is_loopback).collect();
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
                section_header(ui, "CAPTURE SOURCE");
                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .show(ui, |ui| {
                        for (i, source) in self.sources.iter().enumerate() {
                            let selected = self.selected_source == Some(i);
                            let fill   = if selected { BG_SELECTED } else { BG_CARD };
                            let border = if selected { BORDER_SEL } else { BORDER };

                            if !self.source_icon_textures.contains_key(&i) {
                                if let Some((rgba, w, h)) = &source.icon_rgba {
                                    let image = egui::ColorImage::from_rgba_unmultiplied([*w as usize, *h as usize], rgba);
                                    let tex = ui.ctx().load_texture(
                                        format!("source_icon_{i}"),
                                        image,
                                        egui::TextureOptions::LINEAR,
                                    );
                                    self.source_icon_textures.insert(i, tex);
                                }
                            }

                            let inner = egui::Frame::none()
                                .fill(fill)
                                .stroke(egui::Stroke::new(1.0, border))
                                .rounding(egui::Rounding::same(6.0))
                                .inner_margin(egui::Margin::same(8.0))
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

                            if inner.response.interact(egui::Sense::click()).clicked() {
                                self.selected_source = Some(i);
                            }
                            ui.add_space(3.0);
                        }
                    });

                ui.add_space(8.0);
                section_header(ui, "AUDIO");
                for (i, dev) in self.audio_devices.iter().enumerate() {
                    let icon = if dev.is_loopback { "🔊" } else { "🎙" };
                    ui.checkbox(
                        &mut self.selected_audio[i],
                        format!("{icon} {}", dev.name),
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
                        egui::RichText::new("🎯 App audio only (exclude other system sounds)")
                            .color(ACCENT_SECONDARY),
                    )
                    .on_hover_text(if !has_loopback_device {
                        "No system playback device found — this needs one to exist (being muted doesn't matter, but a device must be present)."
                    } else if !loopback_selected {
                        "Check the system audio (🔊) box above first."
                    } else {
                        "Records only the selected window's own audio via Windows' Process Loopback API, instead of the full desktop mix. Needs an active system playback device — muting it doesn't stop this from working."
                    });
                });
            });

        // ── Center panel ──────────────────────────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            section_header(ui, "STATUS");

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
                let t = ctx.input(|i| i.time) as f32;
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
                    let state_label = if is_paused { "PAUSED" } else { "RECORDING" };
                    let state_color = if is_paused {
                        egui::Color32::from_rgb(200, 160, 50)
                    } else {
                        egui::Color32::from_rgb(200, 80, 80)
                    };
                    ui.label(
                        egui::RichText::new(state_label)
                            .size(10.0)
                            .color(state_color)
                            .strong(),
                    );
                });

                ui.add_space(6.0);

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
                        egui::RichText::new(format!("{} tracks", track_count))
                            .size(12.0)
                            .color(TEXT_MUTED),
                    );
                    ui.label(egui::RichText::new("  ·  ").size(12.0).color(TEXT_MUTED));
                    ui.label(
                        egui::RichText::new(format!("{} frames", frames))
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
                    egui::RichText::new("Saving recording…")
                        .size(13.0)
                        .color(TEXT_MUTED),
                );
            } else if let Some(path) = &self.last_output_path {
                ui.label(egui::RichText::new("Last recording:").size(11.0).color(TEXT_MUTED));
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(path.to_string_lossy().as_ref())
                        .size(12.0)
                        .color(TEXT_PRIMARY),
                );
            } else {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("Select a source and press REC to start.")
                        .size(13.0)
                        .color(TEXT_MUTED),
                );
            }

            ui.add_space(16.0);
            section_header(ui, "OUTPUT");

            if ui
                .add(egui::Button::new(egui::RichText::new("⚙ Quality").color(ACCENT_SECONDARY)))
                .clicked()
            {
                self.show_quality_popup = true;
            }
            ui.add_space(4.0);

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
                if ui.button("Browse…").clicked() {
                    if let Some(path) = FileDialog::new()
                        .set_directory(&self.config.output_dir)
                        .pick_folder()
                    {
                        self.output_dir_input = path.to_string_lossy().into_owned();
                        self.config.output_dir = path;
                        if let Err(e) = self.config.save() {
                            tracing::error!("failed to save config: {e}");
                        }
                    }
                }
            });

            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                ui.add_space(8.0);

                let is_paused = self.session.is_paused();

                ui.label(
                    egui::RichText::new(format!("State: {:?}", self.session.state()))
                        .size(10.0)
                        .color(TEXT_MUTED),
                );

                if is_paused {
                    let btn = egui::Button::new(
                        egui::RichText::new("▶ RESUME").color(ACCENT_IDLE).size(18.0),
                    )
                    .fill(BG_BTN_IDLE)
                    .min_size(egui::Vec2::new(130.0, 52.0));
                    if ui.add(btn).clicked() {
                        self.handle_pause_button();
                    }
                } else if is_recording {
                    ui.horizontal(|ui| {
                        let stop_btn = egui::Button::new(
                            egui::RichText::new("⏹ STOP")
                                .color(egui::Color32::from_rgb(248, 113, 113))
                                .size(18.0),
                        )
                        .fill(BG_BTN_STOP)
                        .min_size(egui::Vec2::new(90.0, 52.0));
                        if ui.add(stop_btn).clicked() {
                            self.handle_rec_button(is_recording);
                        }

                        let pause_btn = egui::Button::new(
                            egui::RichText::new("⏸").color(TEXT_MUTED).size(18.0),
                        )
                        .fill(egui::Color32::from_rgb(30, 30, 46))
                        .min_size(egui::Vec2::new(36.0, 52.0));
                        if ui.add(pause_btn).clicked() {
                            self.handle_pause_button();
                        }
                    });
                } else {
                    let btn = egui::Button::new(
                        egui::RichText::new("⏺ REC").color(ACCENT_IDLE).size(18.0),
                    )
                    .fill(BG_BTN_IDLE)
                    .min_size(egui::Vec2::new(130.0, 52.0));
                    if ui.add(btn).clicked() {
                        self.handle_rec_button(is_recording);
                    }
                }
            });
        });

        // ── Overlay viewport (second OS window, click-through) ────────────────
        if is_recording && self.overlay_enabled {
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

        // ── Quality settings popup ────────────────────────────────────────────
        if self.show_quality_popup {
            let mut close = false;
            egui::Window::new("Quality Settings")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    section_header(ui, "FPS");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.config.encode.fps, 30, "30");
                        ui.selectable_value(&mut self.config.encode.fps, 60, "60");
                    });

                    section_header(ui, "CODEC");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.config.encode.codec, "h264".into(), "H264");
                        ui.selectable_value(&mut self.config.encode.codec, "h265".into(), "H265");
                    });

                    section_header(ui, "RESOLUTION");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.config.encode.resolution_mode, "native".into(), "Native (window)");
                        ui.selectable_value(&mut self.config.encode.resolution_mode, "display".into(), "Match display");
                        ui.selectable_value(&mut self.config.encode.resolution_mode, "custom".into(), "Custom");
                    });
                    if self.config.encode.resolution_mode == "custom" {
                        ui.horizontal(|ui| {
                            ui.label("W:");
                            ui.add(egui::DragValue::new(&mut self.config.encode.custom_width).range(2..=7680));
                            ui.label("H:");
                            ui.add(egui::DragValue::new(&mut self.config.encode.custom_height).range(2..=4320));
                        });
                    }

                    section_header(ui, "BITRATE");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.config.encode.bitrate_mode, "auto".into(), "Auto");
                        ui.selectable_value(&mut self.config.encode.bitrate_mode, "manual".into(), "Manual");
                    });
                    if self.config.encode.bitrate_mode == "manual" {
                        ui.horizontal(|ui| {
                            ui.label("Mbps:");
                            ui.add(egui::DragValue::new(&mut self.config.encode.manual_bitrate_mbps).range(1..=100));
                        });
                    }

                    ui.add_space(8.0);
                    if ui.button("Close").clicked() {
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

        // ── Export dialog ─────────────────────────────────────────────────────
        if self.show_export_dialog {
            if let Some(path) = self.last_output_path.clone() {
                let mut close = false;
                egui::Window::new("Recording Complete")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                    .show(ctx, |ui| {
                        ui.label(egui::RichText::new("Recording saved:").size(11.0).color(TEXT_MUTED));
                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new(path.to_string_lossy().as_ref())
                                .size(12.0)
                                .color(TEXT_PRIMARY),
                        );

                        ui.add_space(8.0);
                        section_header(ui, "AUDIO TRACKS");
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
                                if ui.button("Export").clicked() {
                                    if let Some(dest) = rfd::FileDialog::new()
                                        .add_filter("MP4 video", &["mp4"])
                                        .set_file_name("export.mp4")
                                        .save_file()
                                    {
                                        let src = path.clone();
                                        let indices: Vec<usize> = self
                                            .export_track_selection
                                            .iter()
                                            .enumerate()
                                            .filter(|(_, &sel)| sel)
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
                                }
                                if ui.button("Open Folder").clicked() {
                                    open_folder(path.as_ref());
                                }
                                if ui.button("Close").clicked() {
                                    close = true;
                                }
                            });
                        } else if is_running {
                            section_header(ui, "EXPORTING…");
                            ui.label(
                                egui::RichText::new("Please wait…")
                                    .size(11.0)
                                    .color(TEXT_MUTED),
                            );
                        } else if let Some(export_path) = done_path {
                            section_header(ui, "EXPORT COMPLETE");
                            ui.label(
                                egui::RichText::new(export_path.to_string_lossy().as_ref())
                                    .size(11.0)
                                    .color(TEXT_PRIMARY),
                            );
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                if ui.button("Open Folder").clicked() {
                                    open_folder(&export_path);
                                }
                                if ui.button("Close").clicked() {
                                    close = true;
                                }
                            });
                        } else if let Some(msg) = failed_msg {
                            section_header(ui, "EXPORT FAILED");
                            ui.label(
                                egui::RichText::new(&msg)
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(248, 80, 80)),
                            );
                            ui.add_space(4.0);
                            if ui.button("Close").clicked() {
                                close = true;
                            }
                        }
                    });
                if close {
                    self.show_export_dialog = false;
                    self.export_state = ExportState::Idle;
                    self.export_result_rx = None;
                }
            } else {
                self.show_export_dialog = false;
                self.export_state = ExportState::Idle;
                self.export_result_rx = None;
            }
        }

        if is_recording {
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

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(h) = self.hotkey_listener.take() {
            h.stop();
        }
    }
}

fn section_header(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new(title).size(10.0).color(TEXT_MUTED).strong());
    ui.add_space(2.0);
    ui.separator();
    ui.add_space(4.0);
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
            let path = self
                .session
                .active
                .as_ref()
                .map(|a| a.output_path.clone());
            self.session.apply(SessionAction::Stop);
            self.finalizing_handle = self.session.stop_capture();
            self.finalizing_path = path;
            self.recording_start = None;
            self.frame_count.store(0, Ordering::Relaxed);
            self.export_track_selection = self.selected_audio.clone();
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
            let encode = EncodeSettings {
                codec: self.config.encode.codec.clone(),
                fps: self.config.encode.fps,
                resolution_mode: self.config.encode.resolution_mode(),
                bitrate_mode: self.config.encode.bitrate_mode(),
            };
            self.session.start_capture(
                source,
                selected_devices,
                self.app_audio_only,
                Arc::clone(&self.frame_count),
                &self.config.output_dir,
                encode,
            );
            self.recording_start = Some(Instant::now());
        }
    }
}
