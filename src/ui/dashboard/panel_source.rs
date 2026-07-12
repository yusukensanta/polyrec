use super::App;
use super::theme::{
    ACCENT_SECONDARY, BG_CARD, BG_HOVER, BG_SELECTED, BORDER, BORDER_HOVER, BORDER_SEL, TEXT_BODY,
    TEXT_CAPTION, TEXT_MUTED, TEXT_PRIMARY,
};
use super::widgets::{
    audio_device_icon, checkbox_with_volume_slider, section_header, subsection_header,
};
use crate::config::Config;
use crate::i18n::Strings;
use eframe::egui;

impl App {
    pub(super) fn render_source_panel(&mut self, ui: &mut egui::Ui, s: &'static Strings) {
        egui::Panel::left("source_panel")
            .default_size(260.0)
            .size_range(200.0..=380.0)
            .show(ui, |ui| {
                // AUDIO is pinned to the bottom, fully visible, never
                // scrolled -- rendered first so it claims its natural
                // height from the bottom of the panel (Panel::bottom isn't
                // resizable by default: it should size to its own content,
                // not be draggable), leaving whatever remains above for the
                // source list. Audio needs to be readable at a glance on
                // launch, not something you scroll to find, and the set of
                // system devices + currently audio-active apps is normally
                // small enough that "always fully shown" is the common
                // case, not just the best case.
                //
                // That leaves exactly one scrollable region in this panel
                // (the source list below) rather than two -- multiple
                // simultaneously-visible/independently-scrollable regions
                // on one page is a documented accessibility problem
                // (keyboard/switch users can't reliably tell which region
                // has scroll focus; screen magnifier users can miss content
                // cropped by an inner region's own boundary).
                egui::Panel::bottom("audio_footer").show(ui, |ui| {
                    // AUDIO is the parent heading for the two subsections
                    // below it -- SYSTEM (physical devices) and
                    // APPLICATIONS (per-app sources) are both audio
                    // *inputs* in the same sense, just scoped differently,
                    // so they read as siblings under one "AUDIO" umbrella
                    // rather than as two unrelated top-level sections.
                    section_header(ui, s.audio_header);

                    subsection_header(ui, s.system_audio_header);
                    if self.audio_devices.is_empty() {
                        ui.label(
                            egui::RichText::new(s.no_audio_devices)
                                .size(TEXT_CAPTION)
                                .color(TEXT_MUTED),
                        );
                    }
                    for (i, dev) in self.audio_devices.iter().enumerate() {
                        checkbox_with_volume_slider(
                            ui,
                            None,
                            &mut self.config,
                            &mut self.error_message,
                            s.config_save_failed_prefix,
                            &mut self.selected_audio[i],
                            format!("{} {}", audio_device_icon(dev), dev.name),
                            dev.id.clone(),
                        );
                    }

                    ui.add_space(8.0);
                    subsection_header(ui, s.applications_header);
                    if self.app_audio_sources.is_empty() {
                        ui.label(
                            egui::RichText::new(s.no_app_audio_sources)
                                .size(TEXT_CAPTION)
                                .color(TEXT_MUTED),
                        );
                    }
                    for (i, src) in self.app_audio_sources.iter().enumerate() {
                        if let std::collections::hash_map::Entry::Vacant(entry) =
                            self.app_audio_icon_textures.entry(i)
                            && let Some((rgba, w, h)) = &src.icon_rgba
                        {
                            let image = egui::ColorImage::from_rgba_unmultiplied(
                                [*w as usize, *h as usize],
                                rgba,
                            );
                            let tex = ui.ctx().load_texture(
                                format!("app_audio_icon_{i}"),
                                image,
                                egui::TextureOptions::LINEAR,
                            );
                            entry.insert(tex);
                        }
                        checkbox_with_volume_slider(
                            ui,
                            self.app_audio_icon_textures.get(&i).map(|tex| tex.id()),
                            &mut self.config,
                            &mut self.error_message,
                            s.config_save_failed_prefix,
                            &mut self.selected_app_audio[i],
                            src.display_name.clone(),
                            Config::app_audio_gain_key(&src.exe_name),
                        );
                    }

                    ui.add_space(8.0);
                    let loopback_selected = self
                        .audio_devices
                        .iter()
                        .zip(self.selected_audio.iter())
                        .any(|(dev, &sel)| dev.is_loopback && sel);
                    let has_loopback_device = self.audio_devices.iter().any(|d| d.is_loopback);
                    let has_source = self.selected_source.is_some();
                    ui.add_enabled_ui(loopback_selected && has_source, |ui| {
                        let response = ui
                            .checkbox(
                                &mut self.app_audio_only,
                                egui::RichText::new(s.app_audio_only_label).color(ACCENT_SECONDARY),
                            )
                            .on_hover_text(if !has_loopback_device {
                                s.tooltip_no_loopback_device
                            } else if !loopback_selected {
                                s.tooltip_check_loopback_first
                            } else {
                                s.tooltip_app_audio_only
                            });
                        // Persisted as the default for next launch (and thus what a
                        // hotkey-started recording uses) -- same pattern as overlay_enabled.
                        if response.changed() {
                            self.config.default_app_audio_only = self.app_audio_only;
                            if let Err(e) = self.config.save() {
                                tracing::error!("failed to save config: {e}");
                                self.error_message =
                                    Some(format!("{}{e}", s.config_save_failed_prefix));
                            }
                        }
                    });
                });

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
