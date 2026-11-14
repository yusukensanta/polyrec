use super::App;
use super::state::SelfUpdateState;
use super::widgets::{accent_button, caption};
use crate::i18n::Strings;
use eframe::egui;

use super::theme::pal;

impl App {
    pub(super) fn render_menu_bar(&mut self, ui: &mut egui::Ui, s: &'static Strings) {
        egui::Panel::top("menu_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                let row = ui.available_width();
                ui.heading("PolyRec");
                // Single source of truth for the app's version: Cargo.toml's `version`
                // field, baked in at compile time. Never hardcode a version string
                // elsewhere — the release CI also checks the git tag against this
                // same field before publishing, so this label always matches what
                // update_check compares against.
                ui.label(caption(
                    concat!("v", env!("CARGO_PKG_VERSION")),
                    pal().text_muted,
                ));
                ui.separator();
                // The title and the buttons, side by side: right-aligned,
                // the buttons draw over the title when the window is too
                // narrow for both, so the window is kept wide enough (see
                // `min_width`).
                let title = ui.min_rect().width() + ui.spacing().item_spacing.x;
                let right_to_left = egui::Layout::right_to_left(egui::Align::Center);
                let buttons = ui.with_layout(right_to_left, |ui| {
                    let label = if self.config.overlay.enabled {
                        s.overlay_on
                    } else {
                        s.overlay_off
                    };
                    if ui
                        .add(accent_button(label, pal().link))
                        .on_hover_text(s.overlay_toggle_tooltip)
                        .clicked()
                    {
                        self.config.overlay.enabled = !self.config.overlay.enabled;
                        // Saved at once: it used to be saved only on the
                        // way out, so a crash forgot it.
                        self.save_config_or_report(s);
                    }
                    let border_label = if self.config.show_recording_border {
                        s.border_on
                    } else {
                        s.border_off
                    };
                    if ui
                        .add(accent_button(border_label, pal().link))
                        .on_hover_text(s.border_toggle_tooltip)
                        .clicked()
                    {
                        self.config.show_recording_border = !self.config.show_recording_border;
                        self.save_config_or_report(s);
                    }
                    // Saved at once, like the language: it decides what the
                    // next close does, and a close that hides doesn't save.
                    let close_label = if self.config.close_to_tray {
                        s.close_hides
                    } else {
                        s.close_quits
                    };
                    if ui
                        .add(accent_button(close_label, pal().link))
                        .on_hover_text(s.close_toggle_tooltip)
                        .clicked()
                    {
                        self.config.close_to_tray = !self.config.close_to_tray;
                        self.save_config_or_report(s);
                    }
                    // Only with a tray icon: started with Windows, PolyRec
                    // comes up hidden in it.
                    if self.tray.is_some() {
                        let startup_label = if self.starts_with_windows {
                            s.startup_on
                        } else {
                            s.startup_off
                        };
                        if ui
                            .add(accent_button(startup_label, pal().link))
                            .on_hover_text(s.startup_toggle_tooltip)
                            .clicked()
                        {
                            if let Err(e) = crate::autostart::set(!self.starts_with_windows) {
                                tracing::error!("start with Windows not changed: {e}");
                                self.error_message =
                                    Some(format!("{}{e}", s.startup_failed_prefix));
                            }
                            self.starts_with_windows = crate::autostart::is_on();
                        }
                    }
                    let lang = self.config.lang();
                    if ui
                        .add(accent_button(lang.toggle_button_label(), pal().link))
                        .on_hover_text(s.language_toggle_tooltip)
                        .clicked()
                    {
                        self.config.language = lang.toggle();
                        self.save_config_or_report(s);
                    }
                    if let Some(update) = self.update_available.clone() {
                        let label = format!("{} {}", update.version, s.update_available_suffix);
                        let body = super::theme::text_px(super::theme::TEXT_BODY);
                        let clicked = super::icons::add_named(
                            ui,
                            true,
                            super::icons::icon_button(
                                super::icons::Icon::Update,
                                &label,
                                pal().on_accent,
                                body,
                            )
                            .fill(pal().accent)
                            .stroke(egui::Stroke::new(1.0, pal().accent)),
                            &label,
                        )
                        .on_hover_text(s.update_tooltip)
                        .clicked();
                        if clicked {
                            // Closing/restarting mid-recording would corrupt or
                            // lose it -- block the confirm dialog from even
                            // opening in that case, same "explain why, don't
                            // just silently ignore the click" approach as the
                            // other blocked-action paths. Highlight buffering
                            // is different: it's just a rotating background
                            // buffer the user never explicitly started, with
                            // nothing in-progress to lose, so it's set aside
                            // for the update itself (see `App::updating`).
                            if self.has_footage_to_lose() {
                                self.notices.push(s.update_blocked_while_recording);
                            } else {
                                self.self_update.state = SelfUpdateState::Confirming(update);
                            }
                        }
                    }
                });
                let buttons = buttons.response.rect.width();
                self.min_width
                    .menu(ui.ctx().content_rect().width(), row, title + buttons);
            });
        });
    }
}
