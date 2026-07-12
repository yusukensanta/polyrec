use super::widgets::accent_button;
use super::{App, SelfUpdateState};
use crate::i18n::Strings;
use eframe::egui;

use super::theme::{ACCENT_PAUSE, ACCENT_SECONDARY, TEXT_CAPTION, TEXT_MUTED};

impl App {
    pub(super) fn render_menu_bar(&mut self, ui: &mut egui::Ui, s: &'static Strings) {
        egui::Panel::top("menu_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("PolyRec");
                // Single source of truth for the app's version: Cargo.toml's `version`
                // field, baked in at compile time. Never hardcode a version string
                // elsewhere — the release CI also checks the git tag against this
                // same field before publishing, so this label always matches what
                // update_check compares against.
                ui.label(
                    egui::RichText::new(concat!("v", env!("CARGO_PKG_VERSION")))
                        .size(TEXT_CAPTION)
                        .color(TEXT_MUTED),
                );
                ui.separator();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if self.overlay_enabled {
                        s.overlay_on
                    } else {
                        s.overlay_off
                    };
                    if ui
                        .add(accent_button(label, ACCENT_SECONDARY))
                        .on_hover_text(s.overlay_toggle_tooltip)
                        .clicked()
                    {
                        self.overlay_enabled = !self.overlay_enabled;
                        self.config.overlay.enabled = self.overlay_enabled;
                    }
                    let lang = self.config.lang();
                    if ui
                        .add(accent_button(lang.toggle_button_label(), ACCENT_SECONDARY))
                        .on_hover_text(s.language_toggle_tooltip)
                        .clicked()
                    {
                        self.config.language = lang.toggle().config_value().to_string();
                        if let Err(e) = self.config.save() {
                            tracing::error!("failed to save config: {e}");
                            self.error_message =
                                Some(format!("{}{e}", s.config_save_failed_prefix));
                        }
                    }
                    if let Some(update) = self.update_available.clone() {
                        let clicked = ui
                            .add(accent_button(
                                &format!("⬆ {} {}", update.version, s.update_available_suffix),
                                ACCENT_PAUSE,
                            ))
                            .on_hover_text(s.update_tooltip)
                            .clicked();
                        if clicked {
                            // Closing/restarting mid-recording (or mid-Highlight-
                            // buffering) would corrupt or lose it -- block the
                            // confirm dialog from even opening in that case,
                            // same "explain why, don't just silently ignore the
                            // click" approach as the other blocked-action paths.
                            if self.session.is_recording()
                                || self.session.is_paused()
                                || self.session.is_highlighting()
                            {
                                self.error_message =
                                    Some(s.update_blocked_while_recording.to_string());
                            } else {
                                self.self_update_state = SelfUpdateState::Confirming(update);
                            }
                        }
                    }
                });
            });
        });
    }
}
