use super::App;
use super::theme::{
    BG_CARD, BG_HOVER, BG_SELECTED, BORDER, BORDER_HOVER, BORDER_SEL, TEXT_BODY, TEXT_CAPTION,
    TEXT_MUTED, TEXT_PRIMARY,
};
use super::widgets::section_header;
use crate::i18n::Strings;
use eframe::egui;

impl App {
    pub(super) fn render_source_panel(&mut self, ui: &mut egui::Ui, s: &'static Strings) {
        egui::Panel::left("source_panel")
            .default_size(260.0)
            .size_range(200.0..=380.0)
            .show(ui, |ui| {
                section_header(ui, s.capture_source_header);

                ui.add(
                    egui::TextEdit::singleline(&mut self.source_filter)
                        .hint_text(s.source_filter_placeholder)
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(4.0);

                if self.sources.is_empty() {
                    ui.label(
                        egui::RichText::new(s.no_windows_found)
                            .size(TEXT_CAPTION)
                            .color(TEXT_MUTED),
                    );
                }

                // Filters by index rather than cloning matches -- source_icon_textures
                // and selected_source are both keyed by index into self.sources, so
                // the real (unfiltered) indices need to survive filtering.
                let filter = self.source_filter.to_lowercase();
                let filtered_indices: Vec<usize> = self
                    .sources
                    .iter()
                    .enumerate()
                    .filter(|(_, src)| {
                        filter.is_empty()
                            || src.window_title.to_lowercase().contains(&filter)
                            || src.exe_name.to_lowercase().contains(&filter)
                    })
                    .map(|(i, _)| i)
                    .collect();
                if !self.sources.is_empty() && filtered_indices.is_empty() {
                    ui.label(
                        egui::RichText::new(s.no_matching_windows)
                            .size(TEXT_CAPTION)
                            .color(TEXT_MUTED),
                    );
                }

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .id_salt("source_list_scroll")
                    .show(ui, |ui| {
                        for &i in &filtered_indices {
                            let source = &self.sources[i];
                            let selected = self.selected_source == Some(i);
                            // The card's size depends on its content (title/exe_name
                            // wrapping), so its Response -- and hover state -- isn't
                            // known until after it's drawn. Reading last frame's hover
                            // result from egui's per-widget temp storage (written back
                            // at the bottom of this loop body) is the standard egui
                            // pattern for hover-reactive fills on this kind of
                            // dynamically-sized composite widget; the one-frame lag is
                            // imperceptible at normal repaint rates.
                            let hover_id = ui.id().with(("source_card_hovered", i));
                            let was_hovered =
                                ui.data(|d| d.get_temp::<bool>(hover_id)).unwrap_or(false);
                            let fill = if selected {
                                BG_SELECTED
                            } else if was_hovered {
                                BG_HOVER
                            } else {
                                BG_CARD
                            };
                            let border = if selected {
                                BORDER_SEL
                            } else if was_hovered {
                                BORDER_HOVER
                            } else {
                                BORDER
                            };

                            if let std::collections::hash_map::Entry::Vacant(entry) =
                                self.source_icon_textures.entry(i)
                                && let Some((rgba, w, h)) = &source.icon_rgba
                            {
                                let image = egui::ColorImage::from_rgba_unmultiplied(
                                    [*w as usize, *h as usize],
                                    rgba,
                                );
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
                                                    .size(TEXT_BODY)
                                                    .strong()
                                                    .color(TEXT_PRIMARY),
                                            );
                                            if !source.exe_name.is_empty() {
                                                ui.label(
                                                    egui::RichText::new(&source.exe_name)
                                                        .size(TEXT_CAPTION)
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
                            ui.data_mut(|d| d.insert_temp(hover_id, response.hovered()));
                            if response.clicked() {
                                self.selected_source = Some(i);
                            }
                            ui.add_space(4.0);
                        }
                    });
            });
    }
}
