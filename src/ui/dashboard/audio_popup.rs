use super::App;
use super::theme::{ACCENT_SECONDARY, POPUP_WIDTH, TEXT_CAPTION, TEXT_MUTED};
use super::widgets::{
    accent_button, audio_device_icon, checkbox_with_volume_slider, section_header,
};
use crate::config::Config;
use crate::i18n::Strings;
use eframe::egui;

impl App {
    pub(super) fn render_audio_popup(&mut self, ctx: &egui::Context, s: &'static Strings) {
        if !self.show_audio_popup {
            return;
        }
        let mut close = false;
        egui::Window::new(s.audio_title)
            .collapsible(false)
            .resizable(false)
            // Same fixed-width convention as Quality/Hotkeys -- see
            // POPUP_WIDTH's doc comment for why this is pinned rather than
            // left to auto-size.
            .default_width(POPUP_WIDTH)
            .min_width(POPUP_WIDTH)
            .max_width(POPUP_WIDTH)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                // Caps the popup's height below a typical app window's, so a
                // machine with many audio devices/apps scrolls inside the
                // popup instead of forcing it taller than its parent.
                egui::ScrollArea::vertical()
                    .max_height(400.0)
                    .show(ui, |ui| {
                        section_header(ui, s.system_audio_header);
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
                        section_header(ui, s.applications_header);
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
                        // A `Monitor` source has no owning process to scope
                        // loopback to (see `session::start_capture`'s
                        // `use_process_loopback` gate), so this only ever
                        // does anything for a `Window` source.
                        let selected_is_window = self
                            .selected_source
                            .and_then(|i| self.sources.get(i))
                            .is_some_and(|src| src.kind == crate::types::CaptureKind::Window);
                        ui.add_enabled_ui(loopback_selected && selected_is_window, |ui| {
                            let response = ui
                                .checkbox(
                                    &mut self.app_audio_only,
                                    egui::RichText::new(s.app_audio_only_label)
                                        .color(ACCENT_SECONDARY),
                                )
                                .on_hover_text(if !has_loopback_device {
                                    s.tooltip_no_loopback_device
                                } else if !loopback_selected {
                                    s.tooltip_check_loopback_first
                                } else if !selected_is_window {
                                    s.tooltip_app_audio_only_needs_window
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
                    }); // end settings ScrollArea -- Close button stays outside so it's never scrolled out of view

                ui.add_space(8.0);
                if ui.add(accent_button(s.close_button, TEXT_MUTED)).clicked() {
                    close = true;
                }
            });
        // Escape as an emergency exit -- same effect as clicking Close.
        if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.show_audio_popup = false;
        }
    }
}
