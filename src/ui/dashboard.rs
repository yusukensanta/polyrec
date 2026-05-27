use crate::capture::audio::enumerate_audio_devices;
use crate::config::Config;
use crate::session::{state::SessionAction, SessionManager};
use crate::sources::enumerate_sources;
use crate::types::{AudioDevice, CaptureSource};
use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

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
}

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>, config: Config) -> Self {
        let overlay_enabled = config.overlay.enabled;
        let audio_devices = enumerate_audio_devices().unwrap_or_default();
        let selected_audio = vec![true; audio_devices.len()];
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
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("PolyRec");
                ui.separator();
                if ui.button("⟳ Refresh").clicked() {
                    self.sources = enumerate_sources();
                    self.selected_source = None;
                    self.audio_devices = enumerate_audio_devices().unwrap_or_default();
                    self.selected_audio = vec![true; self.audio_devices.len()];
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let overlay_label = if self.overlay_enabled {
                        "Overlay: ON"
                    } else {
                        "Overlay: OFF"
                    };
                    if ui.button(overlay_label).clicked() {
                        self.overlay_enabled = !self.overlay_enabled;
                        self.config.overlay.enabled = self.overlay_enabled;
                    }
                });
            });
        });

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

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label(egui::RichText::new("RECORDING STATUS").small().weak());
            ui.separator();

            let is_recording = self.session.is_recording();
            let frames = self.frame_count.load(Ordering::Relaxed);

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
                    if is_recording {
                        let path = self
                            .session
                            .active
                            .as_ref()
                            .map(|a| a.output_path.clone());
                        if self.session.apply(SessionAction::Stop) {
                            self.session.stop_capture();
                        }
                        self.last_output_path = path;
                        self.recording_start = None;
                        self.frame_count.store(0, Ordering::Relaxed);
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

                ui.label(
                    egui::RichText::new(format!("State: {:?}", self.session.state()))
                        .small()
                        .weak(),
                );
            });

            if is_recording {
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
            }
        });
    }
}
