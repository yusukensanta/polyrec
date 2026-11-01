use super::App;
use super::icons::Icon;
use super::theme::{
    BG_CARD, BG_HOVER, BG_SELECTED, BORDER, BORDER_HOVER, BORDER_SEL, SPACE_GROUP, SPACE_NORMAL,
    TEXT_BODY, TEXT_MUTED, TEXT_PRIMARY, text_px,
};
use super::widgets::{RowIcon, caption, icon_slot, section_header};
use crate::i18n::Strings;
use crate::sources::MONITOR_GLYPH;
use eframe::egui;

impl App {
    /// Indices into `self.sources` whose title or exe name contains the
    /// search box's text (case-insensitive) -- real, unfiltered indices,
    /// since `source_icon_textures` and `selected_source` are keyed by them.
    fn filtered_source_indices(&self) -> Vec<usize> {
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
        filtered_indices
    }

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
                    ui.label(caption(s.no_windows_found, TEXT_MUTED));
                }

                // Filters by index rather than cloning matches -- source_icon_textures
                // and selected_source are both keyed by index into self.sources, so
                // the real (unfiltered) indices need to survive filtering.
                let filtered_indices = self.filtered_source_indices();
                if !self.sources.is_empty() && filtered_indices.is_empty() {
                    ui.label(caption(s.no_matching_windows, TEXT_MUTED));
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
                            let hover_id = ui.scope_id().with(("source_card_hovered", i));
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
                                .inner_margin(SPACE_GROUP)
                                .show(ui, |ui| {
                                    ui.set_min_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        // A display's title starts with its
                                        // glyph; it goes in the icon column
                                        // instead, lined up with windows'.
                                        let (icon, title) =
                                            match source.window_title.strip_prefix(MONITOR_GLYPH) {
                                                Some(title) => (
                                                    RowIcon::Icon(Icon::Monitor),
                                                    title.trim_start(),
                                                ),
                                                None => (
                                                    self.source_icon_textures
                                                        .get(&i)
                                                        .map_or(RowIcon::None, |tex| {
                                                            RowIcon::Image(tex.id())
                                                        }),
                                                    source.window_title.as_str(),
                                                ),
                                            };
                                        icon_slot(ui, icon);
                                        ui.vertical(|ui| {
                                            ui.label(
                                                egui::RichText::new(title)
                                                    .size(text_px(TEXT_BODY))
                                                    .strong()
                                                    .color(TEXT_PRIMARY),
                                            );
                                            if !source.exe_name.is_empty() {
                                                ui.label(caption(&source.exe_name, TEXT_MUTED));
                                            }
                                            if selected
                                                && !self.session.is_recording()
                                                && !self.session.is_paused()
                                            {
                                                self.show_preview(ui, s);
                                            }
                                        });
                                    });
                                });

                            let response = inner
                                .response
                                .interact(egui::Sense::click())
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            // A selectable button, the way egui describes its own
                            // selectable labels -- without it, screen readers (and
                            // UI Automation) saw only the card's text, with no way
                            // to pick it as the source.
                            response.widget_info(|| {
                                egui::WidgetInfo::selected(
                                    egui::Role::Button,
                                    true,
                                    selected,
                                    &source.window_title,
                                )
                            });
                            ui.data_mut(|d| d.insert_temp(hover_id, response.hovered()));
                            if response.clicked() {
                                self.selected_source = Some(i);
                            }
                            ui.add_space(SPACE_NORMAL);
                        }
                    });
            });
    }
}
