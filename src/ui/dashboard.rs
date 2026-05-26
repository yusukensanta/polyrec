use crate::capture::audio::enumerate_audio_devices;
use crate::config::Config;
use crate::session::{state::SessionAction, SessionManager};
use crate::sources::enumerate_sources;
use crate::types::{AudioDevice, CaptureSource};
use eframe::egui;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub struct App {
    config: Config,
    session: SessionManager,
    sources: Vec<CaptureSource>,
    selected_source: Option<usize>,
    audio_devices: Vec<AudioDevice>,
    selected_audio: Vec<bool>,
    overlay_enabled: bool,
    frame_count: Arc<AtomicU64>,
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
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drain video frames to update counter (non-blocking try_recv)
        if let Some(active) = self.session.active.as_mut() {
            while active.video_rx.try_recv().is_ok() {
                self.frame_count.fetch_add(1, Ordering::Relaxed);
            }
        }

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
                    let overlay_label = if self.overlay_enabled { "Overlay: ON" } else { "Overlay: OFF" };
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
                    ui.checkbox(&mut self.selected_audio[i], format!("{icon} {}", dev.name));
                }
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label(egui::RichText::new("AUDIO TRACKS").small().weak());
            ui.separator();
            if self.session.is_recording() {
                ui.label(format!(
                    "Capturing {} audio track(s)",
                    self.selected_audio.iter().filter(|&&b| b).count()
                ));
            } else {
                ui.label("Select audio devices and a capture source, then press REC.");
            }

            ui.add_space(16.0);

            let is_recording = self.session.is_recording();
            let frames = self.frame_count.load(Ordering::Relaxed);

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
                        self.session.apply(SessionAction::Stop);
                        self.session.stop_capture();
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
                        self.session.start_capture(source, selected_devices);
                    }
                }

                ui.label(
                    egui::RichText::new(format!(
                        "State: {:?}  |  Frames: {frames}",
                        self.session.state()
                    ))
                    .small()
                    .weak(),
                );
            });

            if is_recording {
                ctx.request_repaint();
            }
        });
    }
}
