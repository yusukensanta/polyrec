use crate::config::Config;
use crate::session::{state::SessionAction, SessionManager};
use crate::sources::enumerate_sources;
use crate::types::CaptureSource;
use eframe::egui;

pub struct App {
    config: Config,
    session: SessionManager,
    sources: Vec<CaptureSource>,
    selected_source: Option<usize>,
    overlay_enabled: bool,
}

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>, config: Config) -> Self {
        let overlay_enabled = config.overlay.enabled;
        Self {
            config,
            session: SessionManager::new(),
            sources: enumerate_sources(),
            selected_source: None,
            overlay_enabled,
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
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (i, source) in self.sources.iter().enumerate() {
                        let selected = self.selected_source == Some(i);
                        let label = format!("🎮 {}", source.window_title);
                        if ui.selectable_label(selected, &label).clicked() {
                            self.selected_source = Some(i);
                        }
                    }
                });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label(egui::RichText::new("AUDIO TRACKS").small().weak());
            ui.separator();
            ui.label("Audio tracks will appear here in Plan 2.");
            ui.add_space(16.0);

            let is_recording = self.session.is_recording();
            let rec_label = if is_recording { "⏹ STOP" } else { "⏺ REC" };
            let rec_color = if is_recording {
                egui::Color32::from_rgb(248, 113, 113)
            } else {
                egui::Color32::from_rgb(74, 222, 128)
            };

            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                let btn = egui::Button::new(
                    egui::RichText::new(rec_label).color(rec_color).size(18.0),
                );
                if ui.add_sized([120.0, 48.0], btn).clicked() {
                    if is_recording {
                        self.session.apply(SessionAction::Stop);
                    } else {
                        self.session.apply(SessionAction::Start);
                    }
                }
                let state_text = format!("State: {:?}", self.session.state());
                ui.label(egui::RichText::new(state_text).small().weak());
            });
        });
    }
}
