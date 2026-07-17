use super::theme::{
    ACCENT_IDLE, ACCENT_PAUSE, ACCENT_REC, ACCENT_SECONDARY, BG_BTN_IDLE, BG_BTN_STOP, BG_FAINT,
    BORDER, CORNER_CONTROL, ROUNDING_PRIMARY_BTN, SPACE_NORMAL, SPACE_TIGHT, TEXT_BODY,
    TEXT_BUTTON, TEXT_CAPTION, TEXT_DISPLAY, TEXT_MUTED, TEXT_PRIMARY,
};
use super::util::open_folder;
use super::widgets::{accent_button, centered_action_row, format_bytes_free, section_header};
use super::{App, ExportState, HighlightSaveState};
use crate::encode::remux::{mix_tracks, remux};
use crate::i18n::Strings;
use crate::types::SessionState;
use eframe::egui;
use rfd::FileDialog;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc;

impl App {
    pub(super) fn render_center_panel(&mut self, ui: &mut egui::Ui, s: &'static Strings) {
        let is_recording = self.session.is_recording();
        let frames = self.frame_count.load(Ordering::Relaxed);

        egui::CentralPanel::default().show(ui, |ui| {
            // Cap content width instead of letting it stretch to fill however
            // wide the window happens to be resized -- actual content here
            // (status text, the two settings buttons, the output-dir row) has
            // a natural width well under this; without the cap, widening the
            // window just reopens empty right-side space rather than the
            // window becoming more useful.
            ui.set_max_width(440.0);
            section_header(ui, s.status_header);

            let is_paused = self.session.is_paused();
            if is_recording || is_paused {
                let elapsed = self
                    .session
                    .active
                    .as_ref()
                    .map(|a| a.clock.elapsed())
                    .unwrap_or_default();
                let secs = elapsed.as_secs();

                // Pulsing dot — alpha oscillates 56%–100% (never fully off)
                let t = ui.ctx().input(|i| i.time) as f32;
                let alpha = ((t * 1.8_f32).sin() * 0.22 + 0.78).clamp(0.0, 1.0);
                let dot_col = egui::Color32::from_rgba_unmultiplied(
                    ACCENT_REC.r(),
                    ACCENT_REC.g(),
                    ACCENT_REC.b(),
                    (alpha * 255.0) as u8,
                );
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                    ui.painter().circle_filled(rect.center(), 5.0, dot_col);
                    let state_label = if is_paused {
                        s.state_paused
                    } else {
                        s.state_recording
                    };
                    // Named tokens, not ad-hoc hex — also fixes RECORDING's label color,
                    // which previously computed to 4.18:1 contrast on BG_BASE (fails
                    // WCAG AA's 4.5:1 minimum for normal text). ACCENT_REC is 5.54:1.
                    let state_color = if is_paused { ACCENT_PAUSE } else { ACCENT_REC };
                    ui.label(
                        egui::RichText::new(state_label)
                            .size(TEXT_CAPTION)
                            .color(state_color)
                            .strong(),
                    );
                });

                ui.add_space(8.0);

                // Large monospace timer
                ui.label(
                    egui::RichText::new(format!(
                        "{:02}:{:02}:{:02}",
                        secs / 3600,
                        (secs % 3600) / 60,
                        secs % 60,
                    ))
                    .font(egui::FontId::monospace(TEXT_DISPLAY))
                    .color(TEXT_PRIMARY),
                );

                ui.add_space(4.0);

                // Stats row
                let track_count = self.selected_audio.iter().filter(|&&b| b).count()
                    + self.selected_app_audio.iter().filter(|&&b| b).count();
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("{track_count} {}", s.tracks_word))
                            .size(TEXT_CAPTION)
                            .color(TEXT_MUTED),
                    );
                    ui.label(
                        egui::RichText::new("  ·  ")
                            .size(TEXT_CAPTION)
                            .color(TEXT_MUTED),
                    );
                    ui.label(
                        egui::RichText::new(format!("{frames} {}", s.frames_word))
                            .size(TEXT_CAPTION)
                            .color(TEXT_MUTED),
                    );
                });

                if let Some(active) = self.session.active.as_ref() {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(
                            active
                                .output_path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("recording.mp4"),
                        )
                        .size(TEXT_CAPTION)
                        .color(TEXT_MUTED),
                    );
                }
            } else if self.finalizing_handle.is_some() {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(s.saving_recording)
                        .size(TEXT_BODY)
                        .color(TEXT_MUTED),
                );
            } else if let Some(path) = self.last_output_path.clone() {
                self.render_export_controls(ui, s, &path);
            } else {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(s.select_source_prompt)
                        .size(TEXT_BODY)
                        .color(TEXT_MUTED),
                );
            }

            ui.add_space(16.0);
            section_header(ui, s.output_header);

            ui.horizontal(|ui| {
                if ui
                    .add(accent_button(s.quality_button, ACCENT_SECONDARY))
                    .on_hover_text(s.quality_tooltip)
                    .clicked()
                {
                    self.show_quality_popup = true;
                }
                if ui
                    .add(accent_button(s.hotkeys_button, ACCENT_SECONDARY))
                    .on_hover_text(s.hotkeys_tooltip)
                    .clicked()
                {
                    self.show_hotkeys_popup = true;
                }
                if ui
                    .add(accent_button(s.audio_button, ACCENT_SECONDARY))
                    .on_hover_text(s.audio_tooltip)
                    .clicked()
                {
                    self.show_audio_popup = true;
                }
            });
            // Current audio selection at a glance, since it now lives behind
            // the Audio button rather than always-visible in the source
            // panel -- without this, checking what's selected would mean
            // opening the popup every time.
            let audio_selected_count = self.selected_audio.iter().filter(|&&b| b).count()
                + self.selected_app_audio.iter().filter(|&&b| b).count();
            ui.label(
                egui::RichText::new(if audio_selected_count == 0 {
                    s.no_audio_selected.to_string()
                } else {
                    format!("{audio_selected_count} {}", s.audio_sources_word)
                })
                .size(TEXT_CAPTION)
                .color(TEXT_MUTED),
            );
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                let btn_width = 74.0;
                let tf_width = (ui.available_width() - btn_width - 8.0).max(60.0);
                let tf = ui.add_sized(
                    [tf_width, 22.0],
                    egui::TextEdit::singleline(&mut self.output_dir_input),
                );
                if tf.lost_focus() && !self.output_dir_input.trim().is_empty() {
                    self.config.output_dir = PathBuf::from(&self.output_dir_input);
                    if let Err(e) = self.config.save() {
                        tracing::error!("failed to save config: {e}");
                        self.error_message = Some(format!("{}{e}", s.config_save_failed_prefix));
                    }
                }
                if ui
                    .add(accent_button(s.browse_button, ACCENT_SECONDARY))
                    .clicked()
                    && let Some(path) = FileDialog::new()
                        .set_directory(&self.config.output_dir)
                        .pick_folder()
                {
                    self.output_dir_input = path.to_string_lossy().into_owned();
                    self.config.output_dir = path;
                    if let Err(e) = self.config.save() {
                        tracing::error!("failed to save config: {e}");
                        self.error_message = Some(format!("{}{e}", s.config_save_failed_prefix));
                    }
                }
            });

            let show_free_space = self.free_space_bytes.is_some();
            let show_highlight_active = self.session.is_highlighting();
            let show_highlight_save =
                !matches!(self.highlight_save_state, HighlightSaveState::Idle);
            // Grouped into one visually distinct region (rather than a flat
            // pile of same-size lines distinguished only by color) so it
            // reads as "the status area" separate from the output-dir
            // controls above it -- same treatment as source cards.
            if show_free_space || show_highlight_active || show_highlight_save {
                ui.add_space(SPACE_NORMAL);
                egui::Frame::NONE
                    .fill(BG_FAINT)
                    .stroke(egui::Stroke::new(1.0, BORDER))
                    .corner_radius(CORNER_CONTROL)
                    .inner_margin(8i8)
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        let mut first = true;

                        if let Some(free) = self.free_space_bytes {
                            // Same threshold the recording loop itself refuses
                            // to start/continue below (disk_space::MIN_FREE_BYTES)
                            // -- flagging it here too means the user sees it
                            // coming before pressing REC, not just as a
                            // refusal/mid-recording stop after the fact.
                            let low = free < crate::disk_space::MIN_FREE_BYTES;
                            let color = if low { ACCENT_PAUSE } else { TEXT_MUTED };
                            ui.label(
                                egui::RichText::new(format!(
                                    "{}{}",
                                    s.free_space_prefix,
                                    format_bytes_free(free)
                                ))
                                .size(TEXT_CAPTION)
                                .color(color),
                            );
                            first = false;
                        }

                        if show_highlight_active {
                            if !first {
                                ui.add_space(SPACE_TIGHT);
                            }
                            ui.label(
                                egui::RichText::new(s.highlight_status_active)
                                    .size(TEXT_CAPTION)
                                    .color(TEXT_PRIMARY),
                            );
                            first = false;
                        }

                        if !first && show_highlight_save {
                            ui.add_space(SPACE_TIGHT);
                        }
                        match &self.highlight_save_state {
                            HighlightSaveState::Idle => {}
                            HighlightSaveState::Saving => {
                                ui.label(
                                    egui::RichText::new(s.highlight_saving_label)
                                        .size(TEXT_CAPTION)
                                        .color(TEXT_MUTED),
                                );
                            }
                            HighlightSaveState::Done(path) => {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{}{}",
                                        s.highlight_saved_prefix,
                                        path.file_name()
                                            .and_then(|n| n.to_str())
                                            .unwrap_or("highlight.mp4")
                                    ))
                                    .size(TEXT_CAPTION)
                                    .color(ACCENT_IDLE),
                                );
                            }
                            HighlightSaveState::Failed(msg) => {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{}{msg}",
                                        s.highlight_save_failed_prefix
                                    ))
                                    .size(TEXT_CAPTION)
                                    .color(ACCENT_REC),
                                );
                            }
                        }
                    });
            }

            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                ui.add_space(8.0);

                let is_paused = self.session.is_paused();

                // Recording/Paused already show their state via the pulsing
                // dot + colored label at the top of this panel -- showing it
                // again here too would just be the same fact twice. Idle has
                // no other indicator, so it's the only state this line adds.
                //
                // Always render this label -- even with empty text -- rather
                // than skipping it outside Idle. Skipping it changed the
                // action row's position in this Ui's widget order between
                // states, which shifted REC/STOP+pause/Resume up or down
                // relative to each other (and separately tripped egui's
                // "widget rect changed id between passes" warning, since the
                // row landing at a given screen rect had a different id from
                // one frame to the next).
                let state_line = if matches!(self.session.state(), SessionState::Idle) {
                    format!("{}{}", s.state_prefix, s.session_state_idle)
                } else {
                    String::new()
                };
                ui.label(
                    egui::RichText::new(state_line)
                        .size(TEXT_CAPTION)
                        .color(TEXT_MUTED),
                );

                if is_paused {
                    centered_action_row(ui, 130.0, 52.0, |ui| {
                        let btn = egui::Button::new(
                            egui::RichText::new(s.resume_button)
                                .color(ACCENT_IDLE)
                                .size(TEXT_BUTTON),
                        )
                        .fill(BG_BTN_IDLE)
                        .corner_radius(ROUNDING_PRIMARY_BTN)
                        .min_size(egui::Vec2::new(130.0, 52.0));
                        if ui.add(btn).clicked() {
                            self.handle_pause_button();
                        }
                    });
                } else if is_recording {
                    let content_width = 90.0 + ui.spacing().item_spacing.x + 44.0;
                    centered_action_row(ui, content_width, 52.0, |ui| {
                        let stop_btn = egui::Button::new(
                            egui::RichText::new(s.stop_button)
                                .color(ACCENT_REC)
                                .size(TEXT_BUTTON),
                        )
                        .fill(BG_BTN_STOP)
                        .corner_radius(ROUNDING_PRIMARY_BTN)
                        .min_size(egui::Vec2::new(90.0, 52.0));
                        if ui.add(stop_btn).clicked() {
                            self.handle_rec_button(is_recording);
                        }

                        let pause_btn = egui::Button::new(
                            egui::RichText::new("⏸").color(TEXT_MUTED).size(TEXT_BUTTON),
                        )
                        .fill(egui::Color32::from_rgb(30, 30, 46))
                        .min_size(egui::Vec2::new(44.0, 52.0));
                        if ui.add(pause_btn).on_hover_text(s.pause_tooltip).clicked() {
                            self.handle_pause_button();
                        }
                    });
                } else {
                    centered_action_row(ui, 130.0, 52.0, |ui| {
                        let btn = egui::Button::new(
                            egui::RichText::new(s.rec_button)
                                .color(ACCENT_IDLE)
                                .size(TEXT_BUTTON),
                        )
                        .fill(BG_BTN_IDLE)
                        .corner_radius(ROUNDING_PRIMARY_BTN)
                        .min_size(egui::Vec2::new(130.0, 52.0));
                        if ui.add(btn).clicked() {
                            self.handle_rec_button(is_recording);
                        }
                    });
                }
            });
        });
    }

    pub(super) fn render_export_controls(
        &mut self,
        ui: &mut egui::Ui,
        s: &'static Strings,
        path: &std::path::Path,
    ) {
        ui.label(
            egui::RichText::new(s.recording_saved_label)
                .size(TEXT_CAPTION)
                .color(TEXT_MUTED),
        );
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(path.to_string_lossy().as_ref())
                .size(TEXT_CAPTION)
                .color(TEXT_PRIMARY),
        );
        ui.add_space(8.0);

        // Fewer than 2 tracks means there's nothing a track-selection export
        // could meaningfully remove -- the raw recording already IS the most
        // that could be exported, so the export button/track list would just
        // be a confusing no-op. Still offer Open Folder either way.
        if self.export_available_tracks < 2 {
            if ui
                .add(accent_button(s.open_folder_button, ACCENT_SECONDARY))
                .on_hover_text(s.open_folder_tooltip)
                .clicked()
            {
                open_folder(path);
            }
            return;
        }

        section_header(ui, s.audio_tracks_header);
        for i in 0..self.export_track_selection.len() {
            let label = match self.last_recording_audio_labels.get(i) {
                Some(name) => name.clone(),
                None => format!("{} {}", s.track_label_fallback, i + 1),
            };
            ui.checkbox(&mut self.export_track_selection[i], label);
        }
        ui.checkbox(&mut self.export_mix_tracks, s.export_mix_tracks_label)
            .on_hover_text(s.export_mix_tracks_tooltip);

        ui.add_space(8.0);

        let done_path = if let ExportState::Done(p) = &self.export_state {
            Some(p.clone())
        } else {
            None
        };
        let failed_msg = if let ExportState::Failed(m) = &self.export_state {
            Some(m.clone())
        } else {
            None
        };
        let is_idle = matches!(self.export_state, ExportState::Idle);
        let is_running = matches!(self.export_state, ExportState::Running);

        if is_idle {
            ui.horizontal(|ui| {
                if ui
                    .add(accent_button(s.export_button, ACCENT_IDLE))
                    .on_hover_text(s.export_tooltip)
                    .clicked()
                {
                    let default_dir = default_export_dir(&self.config.output_dir);
                    let _ = std::fs::create_dir_all(&default_dir);
                    let default_name = default_export_file_name(
                        &self.last_recording_app_name,
                        chrono::Local::now(),
                    );
                    if let Some(dest) = rfd::FileDialog::new()
                        .add_filter("MP4 video", &["mp4"])
                        .set_directory(&default_dir)
                        .set_file_name(&default_name)
                        .save_file()
                    {
                        let src = path.to_path_buf();
                        let indices: Vec<usize> = self
                            .export_track_selection
                            .iter()
                            .enumerate()
                            .filter(|&(_, &sel)| sel)
                            .map(|(i, _)| i)
                            .collect();
                        let mix = self.export_mix_tracks;
                        let (tx, rx) = mpsc::channel();
                        std::thread::spawn(move || {
                            let result = if mix {
                                mix_tracks(&src, &dest, &indices)
                            } else {
                                remux(&src, &dest, &indices)
                            }
                            .map_err(|e| e.to_string());
                            let _ = tx.send(result);
                        });
                        self.export_result_rx = Some(rx);
                        self.export_state = ExportState::Running;
                    }
                }
                if ui
                    .add(accent_button(s.open_folder_button, ACCENT_SECONDARY))
                    .on_hover_text(s.open_folder_tooltip)
                    .clicked()
                {
                    open_folder(path);
                }
            });
        } else if is_running {
            section_header(ui, s.exporting_header);
            ui.label(
                egui::RichText::new(s.please_wait)
                    .size(TEXT_CAPTION)
                    .color(TEXT_MUTED),
            );
        } else if let Some(export_path) = done_path {
            section_header(ui, s.export_complete_header);
            ui.label(
                egui::RichText::new(export_path.to_string_lossy().as_ref())
                    .size(TEXT_CAPTION)
                    .color(TEXT_PRIMARY),
            );
            ui.add_space(4.0);
            if ui
                .add(accent_button(s.open_folder_button, ACCENT_SECONDARY))
                .on_hover_text(s.open_folder_tooltip)
                .clicked()
            {
                open_folder(&export_path);
            }
        } else if let Some(msg) = failed_msg {
            section_header(ui, s.export_failed_header);
            ui.label(
                egui::RichText::new(&msg)
                    .size(TEXT_CAPTION)
                    .color(ACCENT_REC),
            );
        }
    }
}

/// `<output_dir>/polyrec/exported/` -- recordings themselves live in
/// `<output_dir>/polyrec/` (see `session::prepare_recording_paths`);
/// exports get their own subdirectory there instead of mixing in with the
/// raw recordings.
fn default_export_dir(output_dir: &std::path::Path) -> PathBuf {
    output_dir.join("polyrec").join("exported")
}

/// `<app_name>_<YYYY-MM-DD-HH-MM-SS>.mp4` -- same convention the original
/// recording's own filename uses (see `encode::actor::spawn_recording_actor`),
/// just stamped with the export moment rather than the recording's finish
/// time, so an exported file reads as "the same thing, exported" instead of
/// a differently-shaped name.
fn default_export_file_name(app_name: &str, now: chrono::DateTime<chrono::Local>) -> String {
    let name = if app_name.is_empty() {
        "recording"
    } else {
        app_name
    };
    format!("{name}_{}.mp4", now.format("%Y-%m-%d-%H-%M-%S"))
}

#[cfg(test)]
mod default_export_tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn default_export_dir_is_polyrec_exported_under_output_dir() {
        let output_dir = std::path::Path::new(r"E:\recordings");
        assert_eq!(
            default_export_dir(output_dir),
            std::path::Path::new(r"E:\recordings\polyrec\exported")
        );
    }

    #[test]
    fn default_export_file_name_matches_recording_naming_convention() {
        let now = chrono::Local
            .with_ymd_and_hms(2026, 7, 17, 14, 18, 11)
            .unwrap();
        assert_eq!(
            default_export_file_name("DeadByDaylight-Win64-Shipping", now),
            "DeadByDaylight-Win64-Shipping_2026-07-17-14-18-11.mp4"
        );
    }

    #[test]
    fn default_export_file_name_falls_back_to_recording_when_app_name_empty() {
        let now = chrono::Local
            .with_ymd_and_hms(2026, 7, 17, 14, 18, 11)
            .unwrap();
        assert_eq!(
            default_export_file_name("", now),
            "recording_2026-07-17-14-18-11.mp4"
        );
    }
}
