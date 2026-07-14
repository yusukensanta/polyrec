use super::theme::{
    ACCENT_PAUSE, ACCENT_REC, POPUP_WIDTH, TEXT_BODY, TEXT_CAPTION, TEXT_MUTED, TEXT_PRIMARY,
};
use super::util::open_url;
use super::widgets::{accent_button, section_header};
use super::{App, SelfUpdateState};
use crate::i18n::Strings;
use eframe::egui;

impl App {
    pub(super) fn render_quality_popup(&mut self, ctx: &egui::Context, s: &'static Strings) {
        if !self.show_quality_popup {
            return;
        }
        let mut close = false;
        egui::Window::new(s.quality_title)
            .collapsible(false)
            .resizable(false)
            // Explicit width instead of relying on auto-sizing -- an
            // unconstrained floating Window's content area is effectively as
            // wide as the whole app window, and section_header's separator
            // (which fills "available width") would stretch the Grid/Window
            // out to match it otherwise. min/max pinned to the same value so
            // the Window can never grow or shrink away from it either.
            .default_width(POPUP_WIDTH)
            .min_width(POPUP_WIDTH)
            .max_width(POPUP_WIDTH)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                // Caps the popup's height below a typical app window's, so a
                // short window (or more settings added later) scrolls instead
                // of forcing the popup taller than its parent. 480 (not the
                // original 400) -- found via a live check that 400 already
                // clipped the default (no Custom resolution, no Manual
                // bitrate) content, hiding the Auto/Manual bitrate choice and
                // the entire Highlight section with no visible scrollbar cue
                // to suggest more was there.
                egui::ScrollArea::vertical()
                    .max_height(520.0)
                    .show(ui, |ui| {
                        section_header(ui, s.fps_header);
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut self.config.encode.fps, 30, "30");
                            ui.selectable_value(&mut self.config.encode.fps, 60, "60");
                        });

                        section_header(ui, s.codec_header);
                        ui.horizontal(|ui| {
                            ui.selectable_value(
                                &mut self.config.encode.codec,
                                "h264".into(),
                                "H264",
                            );
                            ui.selectable_value(
                                &mut self.config.encode.codec,
                                "h265".into(),
                                "H265",
                            );
                        });

                        section_header(ui, s.encoder_mode_header);
                        ui.horizontal(|ui| {
                            ui.selectable_value(
                                &mut self.config.encode.encoder_mode,
                                "hardware".into(),
                                s.encoder_mode_hardware,
                            )
                            .on_hover_text(s.encoder_mode_hardware_tooltip);
                            ui.selectable_value(
                                &mut self.config.encode.encoder_mode,
                                "software".into(),
                                s.encoder_mode_software,
                            )
                            .on_hover_text(s.encoder_mode_software_tooltip);
                        });

                        section_header(ui, s.resolution_header);
                        ui.horizontal(|ui| {
                            ui.selectable_value(
                                &mut self.config.encode.resolution_mode,
                                "native".into(),
                                s.resolution_native,
                            );
                            ui.selectable_value(
                                &mut self.config.encode.resolution_mode,
                                "display".into(),
                                s.resolution_display,
                            );
                            ui.selectable_value(
                                &mut self.config.encode.resolution_mode,
                                "custom".into(),
                                s.resolution_custom,
                            );
                        });
                        if self.config.encode.resolution_mode == "custom" {
                            ui.horizontal(|ui| {
                                ui.label(s.width_label);
                                ui.add(
                                    egui::DragValue::new(&mut self.config.encode.custom_width)
                                        .range(2..=7680),
                                );
                                ui.label(s.height_label);
                                ui.add(
                                    egui::DragValue::new(&mut self.config.encode.custom_height)
                                        .range(2..=4320),
                                );
                            });
                        }

                        section_header(ui, s.bitrate_header);
                        ui.horizontal(|ui| {
                            ui.selectable_value(
                                &mut self.config.encode.bitrate_mode,
                                "auto".into(),
                                s.bitrate_auto,
                            );
                            ui.selectable_value(
                                &mut self.config.encode.bitrate_mode,
                                "manual".into(),
                                s.bitrate_manual,
                            );
                        });
                        if self.config.encode.bitrate_mode == "manual" {
                            ui.horizontal(|ui| {
                                ui.label(s.mbps_label);
                                ui.add(
                                    egui::DragValue::new(
                                        &mut self.config.encode.manual_bitrate_mbps,
                                    )
                                    .range(1..=100),
                                );
                            });
                        }

                        ui.add_space(4.0);
                        section_header(ui, s.highlight_header);
                        ui.checkbox(
                            &mut self.config.highlight.enabled,
                            s.highlight_enabled_label,
                        )
                        .on_hover_text(s.tooltip_highlight_enabled);
                        if self.config.highlight.enabled {
                            ui.horizontal(|ui| {
                                ui.label(s.highlight_buffer_seconds_label);
                                ui.add(
                                    egui::DragValue::new(&mut self.config.highlight.buffer_seconds)
                                        .range(
                                            crate::config::HIGHLIGHT_BUFFER_SECONDS_MIN
                                                ..=crate::config::HIGHLIGHT_BUFFER_SECONDS_MAX,
                                        ),
                                );
                            });
                        }
                    }); // end settings ScrollArea -- Close button stays outside so it's never scrolled out of view

                ui.add_space(8.0);
                if ui.add(accent_button(s.close_button, TEXT_MUTED)).clicked() {
                    close = true;
                }
            });
        // Escape as an emergency exit -- same effect as clicking Close.
        if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.show_quality_popup = false;
            if let Err(e) = self.config.save() {
                tracing::error!("failed to save config: {e}");
                self.error_message = Some(format!("{}{e}", s.config_save_failed_prefix));
            }
        }
    }

    pub(super) fn render_error_banner(&mut self, ctx: &egui::Context, s: &'static Strings) {
        let Some(msg) = self.error_message.clone() else {
            return;
        };
        let mut close = false;
        egui::Window::new(s.error_title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(&msg).size(TEXT_BODY).color(ACCENT_REC));
                ui.add_space(8.0);
                if ui.add(accent_button(s.close_button, TEXT_MUTED)).clicked() {
                    close = true;
                }
            });
        if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.error_message = None;
        }
    }

    pub(super) fn render_self_update_popup(&mut self, ctx: &egui::Context, s: &'static Strings) {
        // Both this and the error banner are CENTER_CENTER-anchored windows --
        // showing both at once would draw them stacked on top of each other.
        // Deferring here means clicking Update while Highlight buffering was
        // active shows `update_highlight_disabled_notice` first; this popup
        // only appears once that's dismissed, even though `self_update_state`
        // was already set to `Confirming` in the same click (see
        // `render_menu_bar`).
        if self.error_message.is_some() {
            return;
        }
        match &self.self_update_state {
            SelfUpdateState::Idle => {}
            SelfUpdateState::Confirming(update) => {
                let update = update.clone();
                let mut action: Option<&str> = None;
                egui::Window::new(s.update_confirm_title)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                    .show(ctx, |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "{}{}{}",
                                s.update_confirm_prefix, update.version, s.update_confirm_suffix
                            ))
                            .size(TEXT_BODY)
                            .color(TEXT_PRIMARY),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(s.update_confirm_uac_note)
                                .size(TEXT_CAPTION)
                                .color(TEXT_MUTED),
                        );
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add(accent_button(s.update_now_button, ACCENT_PAUSE))
                                .clicked()
                            {
                                action = Some("now");
                            }
                            if ui
                                .add(accent_button(s.update_not_now_button, TEXT_MUTED))
                                .clicked()
                            {
                                action = Some("not_now");
                            }
                        });
                        if ui.link(s.update_view_release_notes).clicked() {
                            open_url(&update.url);
                        }
                    });
                // Escape == "Not Now" here, but deliberately not wired into the
                // Working/Failed branches below -- Working shouldn't let Escape
                // interrupt an in-flight update, and Failed already has its own
                // Escape handling.
                if action.is_none() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                    action = Some("not_now");
                }
                match action {
                    Some("now") => {
                        let version = update.version.clone();
                        self.self_update_handle = Some(tokio::spawn(
                            crate::self_update::perform_self_update(version),
                        ));
                        self.self_update_state = SelfUpdateState::Working;
                    }
                    Some("not_now") => self.self_update_state = SelfUpdateState::Idle,
                    _ => {}
                }
            }
            SelfUpdateState::Working => {
                egui::Window::new(s.update_confirm_title)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                    .show(ctx, |ui| {
                        ui.label(
                            egui::RichText::new(s.update_working_message)
                                .size(TEXT_BODY)
                                .color(TEXT_PRIMARY),
                        );
                    });
            }
            SelfUpdateState::Failed(msg) => {
                let msg = msg.clone();
                let mut close = false;
                egui::Window::new(s.update_confirm_title)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                    .show(ctx, |ui| {
                        ui.label(
                            egui::RichText::new(format!("{}{msg}", s.update_failed_prefix))
                                .size(TEXT_BODY)
                                .color(ACCENT_REC),
                        );
                        ui.add_space(8.0);
                        if ui.add(accent_button(s.close_button, TEXT_MUTED)).clicked() {
                            close = true;
                        }
                    });
                if close {
                    self.self_update_state = SelfUpdateState::Idle;
                }
            }
        }
    }

    /// Polls the self-update background task -- on success the exe has
    /// already been swapped-and-relaunched (portable) or the installer has
    /// already been launched (installed); our only remaining job is to close
    /// our own window so the old process actually exits (portable's file
    /// swap already happened, but the new process is running alongside us
    /// until we do; the installed path also wants this process's file lock
    /// on polyrec.exe released as soon as possible for the installer).
    pub(super) fn poll_self_update_result(&mut self, ctx: &egui::Context) {
        if !self
            .self_update_handle
            .as_ref()
            .is_some_and(|h| h.is_finished())
        {
            return;
        }
        let handle = self.self_update_handle.take().unwrap();
        match tokio::runtime::Handle::current().block_on(handle) {
            Ok(Ok(())) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Ok(Err(e)) => self.self_update_state = SelfUpdateState::Failed(e.to_string()),
            Err(e) => self.self_update_state = SelfUpdateState::Failed(e.to_string()),
        }
    }
}
