use super::App;
use super::theme::{ACCENT_SECONDARY, POPUP_WIDTH, TEXT_CAPTION, TEXT_MUTED};
use super::widgets::{
    accent_button, audio_device_icon, checkbox_with_volume_slider, section_header,
};
use crate::capture::audio::enumerate_app_audio_sessions;
use crate::config::Config;
use crate::i18n::Strings;
use eframe::egui;
use rfd::FileDialog;

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
                                None,
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
                        // Iterated from a clone, not `self.app_audio_sources`
                        // directly -- an uncheck below rebuilds that Vec
                        // in-place via `rebuild_app_audio_sources_now`,
                        // which a live borrow of it here would conflict
                        // with.
                        let app_audio_sources = self.app_audio_sources.clone();
                        for (i, src) in app_audio_sources.iter().enumerate() {
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
                            let was_checked = self.selected_app_audio[i];
                            checkbox_with_volume_slider(
                                ui,
                                self.app_audio_icon_textures.get(&i).map(|tex| tex.id()),
                                &mut self.config,
                                &mut self.error_message,
                                s.config_save_failed_prefix,
                                &mut self.selected_app_audio[i],
                                src.display_name.clone(),
                                Config::app_audio_gain_key(&src.exe_name),
                                Some(s.remove_registered_app_tooltip),
                            );
                            // Every entry here is already registered (see
                            // `actions::build_app_audio_sources`) -- the
                            // checkbox is the only pin/unpin control, no
                            // separate remove button. Unchecking
                            // immediately un-registers and drops the row,
                            // whether or not the app is currently running
                            // (there's no "checked but idle" state to fall
                            // back to -- being in this list at all already
                            // means "record this").
                            if was_checked && !self.selected_app_audio[i] {
                                self.config.unregister_app_audio(&src.exe_name);
                                if let Err(e) = self.config.save() {
                                    tracing::error!("failed to save config: {e}");
                                    self.error_message =
                                        Some(format!("{}{e}", s.config_save_failed_prefix));
                                }
                                self.rebuild_app_audio_sources_now();
                                break;
                            }
                        }
                        // The Applications list is entirely opt-in via this
                        // button -- unlike SYSTEM devices, it never shows an
                        // app just because it's currently making sound (see
                        // `actions::build_app_audio_sources`'s doc comment).
                        // A fresh install shows nothing here but this button.
                        if !self.show_add_app_picker {
                            if ui
                                .add(accent_button(s.add_app_button, ACCENT_SECONDARY))
                                .clicked()
                            {
                                self.show_add_app_picker = true;
                                self.add_app_search.clear();
                            }
                        } else {
                            self.render_add_app_picker(ui, s);
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

    /// Inline search-and-pick panel shown in place of the "+ Add app"
    /// button once clicked -- lists currently running, not-yet-registered
    /// apps (same search-box convention as the source panel's
    /// `source_filter`) so the common case needs no file browsing at all;
    /// registers on click, deriving the path from the live process the same
    /// way the old register-via-checkbox flow did. "Browse for .exe
    /// instead…" remains as the only path for an app that isn't running.
    fn render_add_app_picker(&mut self, ui: &mut egui::Ui, s: &'static Strings) {
        ui.add(
            egui::TextEdit::singleline(&mut self.add_app_search)
                .hint_text(s.add_app_search_placeholder)
                .desired_width(f32::INFINITY),
        );
        ui.add_space(4.0);

        let registered: std::collections::HashSet<String> = self
            .config
            .registered_app_audio
            .iter()
            .map(|r| r.exe_name.clone())
            .collect();
        let filter = self.add_app_search.to_lowercase();
        let candidates: Vec<_> = enumerate_app_audio_sessions()
            .unwrap_or_default()
            .into_iter()
            .filter(|src| !registered.contains(&src.exe_name))
            .filter(|src| {
                filter.is_empty()
                    || src.display_name.to_lowercase().contains(&filter)
                    || src.exe_name.to_lowercase().contains(&filter)
            })
            .collect();

        if candidates.is_empty() {
            let msg = if filter.is_empty() {
                s.add_app_none_running
            } else {
                s.add_app_no_matches
            };
            ui.label(
                egui::RichText::new(msg)
                    .size(TEXT_CAPTION)
                    .color(TEXT_MUTED),
            );
        }
        let mut picked: Option<(String, u32)> = None;
        egui::ScrollArea::vertical()
            .max_height(120.0)
            .id_salt("add_app_picker_scroll")
            .show(ui, |ui| {
                for c in &candidates {
                    if ui.button(&c.display_name).clicked() {
                        picked = Some((c.exe_name.clone(), c.process_ids[0]));
                    }
                }
            });
        if let Some((exe_name, pid)) = picked {
            let exe_path = crate::sources::get_exe_path(pid).unwrap_or_default();
            self.config.register_app_audio(exe_name, exe_path);
            if let Err(e) = self.config.save() {
                tracing::error!("failed to save config: {e}");
                self.error_message = Some(format!("{}{e}", s.config_save_failed_prefix));
            }
            self.rebuild_app_audio_sources_now();
            self.show_add_app_picker = false;
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button(s.add_app_browse_button).clicked()
                && let Some(path) = FileDialog::new()
                    .add_filter("Executable", &["exe"])
                    .pick_file()
                && let Some(exe_name) = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
            {
                self.config
                    .register_app_audio(exe_name, path.to_string_lossy().into_owned());
                if let Err(e) = self.config.save() {
                    tracing::error!("failed to save config: {e}");
                    self.error_message = Some(format!("{}{e}", s.config_save_failed_prefix));
                }
                self.rebuild_app_audio_sources_now();
                self.show_add_app_picker = false;
            }
            if ui.button(s.close_button).clicked() {
                self.show_add_app_picker = false;
            }
        });
    }
}
