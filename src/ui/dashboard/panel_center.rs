use super::App;
use super::state::{ExportState, HighlightSaveState, RecordingStatusCache};
use super::theme::{
    ACCENT_IDLE, ACCENT_PAUSE, ACCENT_REC, ACCENT_SECONDARY, BG_BTN_IDLE, BG_BTN_PAUSE,
    BG_BTN_STOP, BG_FAINT, BORDER, CORNER_CONTROL, ROUNDING_PRIMARY_BTN, SPACE_NORMAL, SPACE_TIGHT,
    TEXT_BODY, TEXT_BUTTON, TEXT_DISPLAY, TEXT_MUTED, TEXT_PRIMARY,
};
use super::util::{OutputDirProblem, open_directory, open_folder, output_dir_problem};
use super::widgets::{accent_button, caption, centered_action_row, format_bytes, section_header};
use crate::encode::remux::{export_grouped, mix_tracks};
use crate::i18n::Strings;
use crate::types::{AudioLevel, SessionState};
use eframe::egui;
use rfd::FileDialog;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc;

impl App {
    /// How many audio tracks the running recording has, for the status row
    /// and the overlay HUD. This is the recording's own count, fixed at
    /// start. The live checkboxes can change mid-recording, and an app source
    /// records one track per running process (none while it's idle), not one
    /// per checkbox.
    pub(super) fn recording_track_count(&self) -> usize {
        self.last_recording_audio_labels.len()
    }

    /// The dismissible notice strip at the top of the Status panel -- see
    /// `notice`. The text wraps beside a ✕ that clears it.
    fn render_notice(&mut self, ui: &mut egui::Ui, s: &'static Strings) {
        let Some(text) = &self.notice else {
            return;
        };
        let mut dismissed = false;
        egui::Frame::NONE
            .fill(BG_FAINT)
            .stroke(egui::Stroke::new(1.0, ACCENT_PAUSE))
            .corner_radius(CORNER_CONTROL)
            .inner_margin(8i8)
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    let dismiss_width = 24.0;
                    ui.allocate_ui(
                        egui::vec2(ui.available_width() - dismiss_width, 0.0),
                        |ui| {
                            ui.add(egui::Label::new(caption(text.as_str(), ACCENT_PAUSE)).wrap());
                        },
                    );
                    dismissed = ui.small_button("✕").on_hover_text(s.close_button).clicked();
                });
            });
        if dismissed {
            self.notice = None;
        }
        ui.add_space(SPACE_NORMAL);
    }

    /// One live level meter per audio track of the running recording, each
    /// labelled with its source, so a silent mic or a muted app shows up
    /// while there's still time to fix it rather than in the finished file.
    /// While paused nothing is captured, so the meters fall to empty.
    fn render_audio_meters(&mut self, ui: &mut egui::Ui, is_paused: bool) {
        let peaks: Vec<f32> = self
            .session
            .active()
            .map(|a| a.audio_levels().iter().map(AudioLevel::peak).collect())
            .unwrap_or_default();
        if peaks.is_empty() {
            return;
        }
        self.meter_levels.resize(peaks.len(), 0.0);
        ui.add_space(SPACE_TIGHT);
        for (i, peak) in peaks.into_iter().enumerate() {
            self.meter_levels[i] =
                meter_decay(self.meter_levels[i], if is_paused { 0.0 } else { peak });
        }
        let labels = &self.last_recording_audio_labels;
        let meter_levels = &self.meter_levels;
        ui.scope(|ui| {
            // Compact rows: the theme's 32 px minimum control height would
            // make each meter line as tall as a button.
            ui.spacing_mut().interact_size.y = 16.0;
            ui.spacing_mut().item_spacing.y = 2.0;
            for (i, &shown) in meter_levels.iter().enumerate() {
                let label = labels
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("#{}", i + 1));
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(90.0, 6.0), egui::Sense::hover());
                    let painter = ui.painter();
                    painter.rect_filled(rect, 2.0, BG_FAINT);
                    let fill = meter_fill(shown);
                    let mut lit = rect;
                    lit.set_width(rect.width() * fill);
                    painter.rect_filled(lit, 2.0, meter_color(fill));
                    ui.label(caption(label, TEXT_MUTED));
                });
            }
        });
    }

    /// Makes `dir` the output folder: re-checks whether recordings can be
    /// saved there (`output_dir_problem`) and saves the config.
    fn set_output_dir(&mut self, dir: PathBuf, s: &'static Strings) {
        self.output_dir_problem = output_dir_problem(&dir);
        self.config.output_dir = dir;
        self.refresh_recent();
        self.save_config_or_report(s);
    }

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
            // The REC row gets its own bottom panel, so it keeps its space
            // however tall the content above gets (e.g. a finished
            // recording's export controls); that content scrolls instead.
            // Laid out in the same ui, the two used to overlap.
            egui::Panel::bottom("action_row")
                .resizable(false)
                .show_separator_line(false)
                .frame(egui::Frame::NONE)
                .show(ui, |ui| self.render_action_row(ui, s, is_recording));
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.render_notice(ui, s);
                section_header(ui, s.status_header);

                self.render_status(ui, s, is_recording, frames);

                ui.add_space(16.0);
                section_header(ui, s.output_header);

                self.render_output_settings(ui, s);
                self.render_status_box(ui, s);
                self.render_recent(ui, s);
            });
        });
    }

    /// The Status section's body: live timer/stats while recording or
    /// paused, "saving" while finalizing, export controls once a
    /// recording finished, otherwise the select-a-source prompt.
    fn render_status(
        &mut self,
        ui: &mut egui::Ui,
        s: &'static Strings,
        is_recording: bool,
        frames: u64,
    ) {
        let is_paused = self.session.is_paused();
        if is_recording || is_paused {
            let elapsed = self
                .session
                .active()
                .map(|a| a.elapsed())
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
                ui.label(caption(state_label, state_color).strong());
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

            // Recomputed only on a new elapsed second, not every frame --
            // see `RecordingStatusCache`.
            if let Some(active) = self.session.active()
                && self.recording_status.as_ref().map(|c| c.secs) != Some(secs)
            {
                let stem = crate::recording_naming::resolve_finished_recording_stem(
                    &active.naming,
                    &active.app_name,
                    &active.output_dir,
                );
                self.recording_status = Some(RecordingStatusCache {
                    secs,
                    name: format!("{stem}.mp4"),
                    file_bytes: std::fs::metadata(&active.output_path).ok().map(|m| m.len()),
                });
            }

            // Stats row: tracks · achieved fps · size so far. Fps and size
            // say how the recording is going; the raw frame count it used to
            // show said neither.
            let mut stats = vec![format!(
                "{} {}",
                self.recording_track_count(),
                s.tracks_word
            )];
            if let Some(fps) = achieved_fps(frames, elapsed) {
                stats.push(format!("{fps:.0} fps"));
            }
            if let Some(bytes) = self.recording_status.as_ref().and_then(|c| c.file_bytes) {
                stats.push(format_bytes(bytes));
            }
            ui.label(caption(stats.join("  ·  "), TEXT_MUTED))
                .on_hover_text(s.recording_stats_tooltip);

            ui.add_space(4.0);
            ui.label(caption(
                self.recording_status
                    .as_ref()
                    .map_or("recording.mp4", |c| c.name.as_str()),
                TEXT_MUTED,
            ));

            self.render_audio_meters(ui, is_paused);
        } else if self.finalizing.handle.is_some() {
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
    }

    /// The Output section: Quality/Hotkeys/Audio buttons, audio selection
    /// summary, output folder, and recording-name options.
    fn render_output_settings(&mut self, ui: &mut egui::Ui, s: &'static Strings) {
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
        ui.label(caption(
            if audio_selected_count == 0 {
                s.no_audio_selected.to_string()
            } else {
                format!("{audio_selected_count} {}", s.audio_sources_word)
            },
            TEXT_MUTED,
        ));
        ui.add_space(8.0);

        // Laid out right to left: the buttons take whatever width their
        // labels need (which depends on the font and language), and the
        // folder field gets the rest. A fixed width guess clipped Open. The
        // row is one control tall; a bare `with_layout` would claim all the
        // panel's remaining height.
        let row = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
        ui.allocate_ui_with_layout(
            row,
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                // Opens the `polyrec` subfolder recordings actually land in once
                // there is one, otherwise the chosen folder itself.
                if ui
                    .add_enabled(
                        self.output_dir_problem.is_none(),
                        accent_button(s.open_output_dir_button, ACCENT_SECONDARY),
                    )
                    .on_hover_text(s.open_output_dir_tooltip)
                    .clicked()
                {
                    let recordings = self.config.output_dir.join("polyrec");
                    open_directory(if recordings.is_dir() {
                        &recordings
                    } else {
                        &self.config.output_dir
                    });
                }
                if ui
                    .add(accent_button(s.browse_button, ACCENT_SECONDARY))
                    .clicked()
                    && let Some(path) = FileDialog::new()
                        .set_directory(&self.config.output_dir)
                        .pick_folder()
                {
                    self.output_dir_input = path.to_string_lossy().into_owned();
                    self.set_output_dir(path, s);
                }
                let tf = ui.add_sized(
                    [ui.available_width().max(60.0), 22.0],
                    egui::TextEdit::singleline(&mut self.output_dir_input),
                );
                if tf.lost_focus() && !self.output_dir_input.trim().is_empty() {
                    self.set_output_dir(PathBuf::from(&self.output_dir_input), s);
                }
            },
        );
        if let Some(problem) = self.output_dir_problem {
            ui.label(caption(output_dir_problem_text(problem, s), ACCENT_PAUSE));
        }

        ui.add_space(6.0);
        let checkbox_response = ui
            .checkbox(
                &mut self.config.use_process_name_for_recording,
                s.use_process_name_label,
            )
            .on_hover_text(s.use_process_name_tooltip);
        if checkbox_response.changed() {
            self.save_config_or_report(s);
        }
        if !self.config.use_process_name_for_recording {
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                let tf = ui.add(
                    egui::TextEdit::singleline(&mut self.config.custom_recording_prefix)
                        .hint_text(s.custom_recording_prefix_placeholder),
                );
                if tf.lost_focus() {
                    self.save_config_or_report(s);
                }
            });
        }
    }

    /// The framed status box (free space, Highlight buffering, Highlight
    /// save result), shown only when it has something to say.
    fn render_status_box(&self, ui: &mut egui::Ui, s: &'static Strings) {
        let show_free_space = self.free_space.bytes.is_some();
        let show_highlight_active = self.session.is_highlighting();
        let show_highlight_save = !matches!(self.highlight_save.state, HighlightSaveState::Idle);
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

                    if let Some(free) = self.free_space.bytes {
                        // Same threshold the recording loop itself refuses
                        // to start/continue below (disk_space::MIN_FREE_BYTES)
                        // -- flagging it here too means the user sees it
                        // coming before pressing REC, not just as a
                        // refusal/mid-recording stop after the fact.
                        let low = free < crate::disk_space::MIN_FREE_BYTES;
                        let color = if low { ACCENT_PAUSE } else { TEXT_MUTED };
                        ui.label(caption(
                            format!("{}{}", s.free_space_prefix, format_bytes(free)),
                            color,
                        ));
                        first = false;
                    }

                    if show_highlight_active {
                        if !first {
                            ui.add_space(SPACE_TIGHT);
                        }
                        ui.label(caption(s.highlight_status_active, TEXT_PRIMARY));
                        first = false;
                    }

                    if !first && show_highlight_save {
                        ui.add_space(SPACE_TIGHT);
                    }
                    match &self.highlight_save.state {
                        HighlightSaveState::Idle => {}
                        HighlightSaveState::Saving => {
                            ui.label(caption(s.highlight_saving_label, TEXT_MUTED));
                        }
                        HighlightSaveState::Done(path) => {
                            ui.label(caption(
                                format!(
                                    "{}{}",
                                    s.highlight_saved_prefix,
                                    path.file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("highlight.mp4")
                                ),
                                ACCENT_IDLE,
                            ));
                        }
                        HighlightSaveState::Failed(msg) => {
                            ui.label(caption(
                                format!("{}{msg}", s.highlight_save_failed_prefix),
                                ACCENT_REC,
                            ));
                        }
                    }
                });
        }
    }

    /// The bottom-anchored REC / STOP+pause / Resume row.
    fn render_action_row(&mut self, ui: &mut egui::Ui, s: &'static Strings, is_recording: bool) {
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
            ui.label(caption(state_line, TEXT_MUTED));

            // The global hotkeys for what the buttons above do right now.
            // Always rendered (empty when nothing's bound), for the same
            // stable-layout reason as the state line.
            let hotkeys = &self.config.hotkeys;
            ui.label(caption(
                hotkey_hint(self.session.state(), &hotkeys.start_stop, &hotkeys.pause, s),
                TEXT_MUTED,
            ));

            let stop_btn = || {
                egui::Button::new(
                    egui::RichText::new(s.stop_button)
                        .color(ACCENT_REC)
                        .size(TEXT_BUTTON),
                )
                .fill(BG_BTN_STOP)
                .corner_radius(ROUNDING_PRIMARY_BTN)
                .min_size(egui::Vec2::new(90.0, 52.0))
            };
            if is_paused {
                // Stop stays reachable while paused, next to Resume -- it used
                // to take a Resume first, recording a moment more.
                let content_width = 110.0 + ui.spacing().item_spacing.x + 90.0;
                centered_action_row(ui, content_width, 52.0, |ui| {
                    let btn = egui::Button::new(
                        egui::RichText::new(s.resume_button)
                            .color(ACCENT_IDLE)
                            .size(TEXT_BUTTON),
                    )
                    .fill(BG_BTN_IDLE)
                    .corner_radius(ROUNDING_PRIMARY_BTN)
                    .min_size(egui::Vec2::new(110.0, 52.0));
                    if ui.add(btn).clicked() {
                        self.handle_pause_button();
                    }
                    if ui.add(stop_btn()).clicked() {
                        self.handle_rec_button(is_recording);
                    }
                });
            } else if is_recording {
                let content_width = 90.0 + ui.spacing().item_spacing.x + 44.0;
                centered_action_row(ui, content_width, 52.0, |ui| {
                    if ui.add(stop_btn()).clicked() {
                        self.handle_rec_button(is_recording);
                    }

                    let pause_btn = egui::Button::new(
                        egui::RichText::new("⏸").color(TEXT_MUTED).size(TEXT_BUTTON),
                    )
                    .fill(BG_BTN_PAUSE)
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
                    // REC used to do nothing at all when clicked without a
                    // source, and only failed on start with an unusable
                    // output folder; now it says what's missing instead.
                    let blocked = if self.selected_source.is_none() {
                        Some(s.rec_needs_source_tooltip)
                    } else {
                        self.output_dir_problem
                            .map(|problem| output_dir_problem_text(problem, s))
                    };
                    let rec = ui.add_enabled(blocked.is_none(), btn);
                    if let Some(reason) = blocked {
                        rec.on_disabled_hover_text(reason);
                    } else if rec.clicked() {
                        self.handle_rec_button(is_recording);
                    }
                });
            }
        });
    }

    /// The export's track checkboxes (one per app/device label, see
    /// `group_tracks_by_label`) and the mix toggle. Returns the groups.
    fn render_export_track_checkboxes(
        &mut self,
        ui: &mut egui::Ui,
        s: &'static Strings,
    ) -> Vec<(String, Vec<usize>)> {
        section_header(ui, s.audio_tracks_header);
        // Grouped by resolved label, not one checkbox per raw track -- a
        // multi-process app (e.g. Discord with more than one live audio
        // session at once) gets one checkbox covering all of its tracks
        // instead of one per underlying process, matching how nobody
        // thinks of "Discord" as multiple audio sources. Toggling a group
        // checkbox sets every track in that group together, so they can
        // never end up selected inconsistently.
        let resolved_labels: Vec<String> = (0..self.export.track_selection.len())
            .map(|i| match self.last_recording_audio_labels.get(i) {
                Some(name) => name.clone(),
                None => format!("{} {}", s.track_label_fallback, i + 1),
            })
            .collect();
        let track_groups = group_tracks_by_label(&resolved_labels);
        for (label, indices) in &track_groups {
            let mut checked = indices.iter().all(|&i| self.export.track_selection[i]);
            if ui.checkbox(&mut checked, label.clone()).changed() {
                for &i in indices {
                    self.export.track_selection[i] = checked;
                }
            }
        }
        ui.checkbox(&mut self.export.mix_tracks, s.export_mix_tracks_label)
            .on_hover_text(s.export_mix_tracks_tooltip);
        track_groups
    }

    /// Export clicked: asks where to save, then runs the selected
    /// export (mix, or per-group passthrough/merge) on a worker thread.
    fn start_export(&mut self, path: &std::path::Path, track_groups: &[(String, Vec<usize>)]) {
        let default_dir = default_export_dir(&self.config.output_dir);
        let _ = std::fs::create_dir_all(&default_dir);
        let default_name =
            default_export_file_name(&self.last_recording_app_name, chrono::Local::now());
        if let Some(dest) = rfd::FileDialog::new()
            .add_filter("MP4 video", &["mp4"])
            .set_directory(&default_dir)
            .set_file_name(&default_name)
            .save_file()
        {
            let src = path.to_path_buf();
            // Global "Mix into one track" (if checked) still
            // mixes everything selected regardless of app
            // grouping -- selected_groups (same-app tracks
            // merged, everything else kept separate) is only
            // used when that's off.
            let indices: Vec<usize> = self
                .export
                .track_selection
                .iter()
                .enumerate()
                .filter(|&(_, &sel)| sel)
                .map(|(i, _)| i)
                .collect();
            let selected_groups: Vec<Vec<usize>> = track_groups
                .iter()
                .map(|(_, idxs)| {
                    idxs.iter()
                        .copied()
                        .filter(|&i| self.export.track_selection[i])
                        .collect::<Vec<usize>>()
                })
                .filter(|g: &Vec<usize>| !g.is_empty())
                .collect();
            let mix = self.export.mix_tracks;
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let result = if mix {
                    mix_tracks(&src, &dest, &indices)
                } else {
                    export_grouped(&src, &dest, &selected_groups)
                }
                .map_err(|e| e.to_string());
                let _ = tx.send(result);
            });
            self.export.result_rx = Some(rx);
            self.export.state = ExportState::Running;
        }
    }

    pub(super) fn render_export_controls(
        &mut self,
        ui: &mut egui::Ui,
        s: &'static Strings,
        path: &std::path::Path,
    ) {
        ui.label(caption(s.recording_saved_label, TEXT_MUTED));
        ui.add_space(2.0);
        ui.label(caption(path.to_string_lossy().as_ref(), TEXT_PRIMARY));
        ui.add_space(8.0);

        // Fewer than 2 tracks means there's nothing a track-selection export
        // could meaningfully remove -- the raw recording already IS the most
        // that could be exported, so the export button/track list would just
        // be a confusing no-op. Still offer Open Folder either way.
        if self.export.available_tracks < 2 {
            if ui
                .add(accent_button(s.open_folder_button, ACCENT_SECONDARY))
                .on_hover_text(s.open_folder_tooltip)
                .clicked()
            {
                open_folder(path);
            }
            return;
        }

        let track_groups = self.render_export_track_checkboxes(ui, s);

        ui.add_space(8.0);

        let done_path = if let ExportState::Done(p) = &self.export.state {
            Some(p.clone())
        } else {
            None
        };
        let failed_msg = if let ExportState::Failed(m) = &self.export.state {
            Some(m.clone())
        } else {
            None
        };
        let is_idle = matches!(self.export.state, ExportState::Idle);
        let is_running = matches!(self.export.state, ExportState::Running);

        if is_idle {
            ui.horizontal(|ui| {
                if ui
                    .add(accent_button(s.export_button, ACCENT_IDLE))
                    .on_hover_text(s.export_tooltip)
                    .clicked()
                {
                    self.start_export(path, &track_groups);
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
            ui.label(caption(s.please_wait, TEXT_MUTED));
        } else if let Some(export_path) = done_path {
            section_header(ui, s.export_complete_header);
            ui.label(caption(
                export_path.to_string_lossy().as_ref(),
                TEXT_PRIMARY,
            ));
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
            ui.label(caption(&msg, ACCENT_REC));
        }
    }
}

/// The message for an unusable output folder, shown under the folder and on
/// a disabled REC.
fn output_dir_problem_text(problem: OutputDirProblem, s: &Strings) -> &'static str {
    match problem {
        OutputDirProblem::Unavailable => s.output_dir_unavailable,
        OutputDirProblem::NotAFolder => s.output_dir_not_a_folder,
    }
}

/// How much of a level meter to fill for a linear peak: a dBFS scale from
/// -60 dB (empty) to 0 dB (full), so quiet but present audio still shows.
fn meter_fill(peak: f32) -> f32 {
    if peak <= 0.0 {
        return 0.0;
    }
    ((20.0 * peak.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// The level a meter shows: up to a new peak at once, back down slowly, so a
/// brief peak stays readable at the dashboard's frame rate.
fn meter_decay(shown: f32, peak: f32) -> f32 {
    peak.max(shown * 0.85)
}

/// Green normally, amber above -12 dBFS, red above -3 dBFS (close to
/// clipping), by meter fill (see `meter_fill`).
fn meter_color(fill: f32) -> egui::Color32 {
    if fill >= 0.95 {
        ACCENT_REC
    } else if fill >= 0.8 {
        ACCENT_PAUSE
    } else {
        ACCENT_IDLE
    }
}

/// Frames per second the recording has achieved so far, averaged over its
/// recorded (pause-excluded) time. `None` for the first second, when a
/// handful of frames over a fraction of a second says little.
fn achieved_fps(frames: u64, elapsed: std::time::Duration) -> Option<f64> {
    let secs = elapsed.as_secs_f64();
    (secs >= 1.0).then(|| frames as f64 / secs)
}

/// The hotkey line under the action buttons, for `state`. Idle explains what
/// the start key really does (it records the window in front, not the
/// selected source -- see `handle_hotkey_start_stop`). Recording and paused
/// list stop plus pause or resume. An unbound key (empty) is left out.
pub(super) fn hotkey_hint(
    state: &SessionState,
    start_stop: &str,
    pause: &str,
    s: &Strings,
) -> String {
    let parts: &[(&str, &str)] = match state {
        SessionState::Idle => &[(s.hotkey_hint_record_front, start_stop)],
        SessionState::Recording => &[
            (s.hotkey_hint_stop, start_stop),
            (s.hotkey_hint_pause, pause),
        ],
        SessionState::Paused => &[
            (s.hotkey_hint_stop, start_stop),
            (s.hotkey_hint_resume, pause),
        ],
    };
    parts
        .iter()
        .filter(|(_, key)| !key.is_empty())
        .map(|(template, key)| template.replace("{key}", key))
        .collect::<Vec<_>>()
        .join("  ·  ")
}

/// Groups raw track indices by their resolved label, preserving each
/// group's first-appearance order -- e.g. "Discord" appearing at indices
/// `[1, 2]` (a multi-process app; see `AppAudioSource::process_ids`'s doc
/// comment for why that happens) groups into one entry covering both,
/// instead of exposing the process-level split as two separate checkboxes.
/// Matches by label text, not adjacency, so it stays correct even if track
/// order in the file doesn't match selection order.
fn group_tracks_by_label(labels: &[String]) -> Vec<(String, Vec<usize>)> {
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, label) in labels.iter().enumerate() {
        match groups.iter_mut().find(|(l, _)| l == label) {
            Some((_, indices)) => indices.push(i),
            None => groups.push((label.clone(), vec![i])),
        }
    }
    groups
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

#[cfg(test)]
mod meter_tests {
    use super::*;

    #[test]
    fn meter_fill_is_a_minus_60_to_0_dbfs_scale() {
        assert_eq!(meter_fill(0.0), 0.0);
        assert_eq!(meter_fill(1.0), 1.0);
        assert!((meter_fill(0.1) - 2.0 / 3.0).abs() < 1e-4, "-20 dBFS");
        assert_eq!(meter_fill(0.0001), 0.0, "-80 dBFS is below the scale");
    }

    #[test]
    fn meter_rises_at_once_and_falls_slowly() {
        assert_eq!(meter_decay(0.1, 0.8), 0.8);
        let after_one_frame = meter_decay(0.8, 0.0);
        assert!(after_one_frame > 0.6 && after_one_frame < 0.8);
    }

    #[test]
    fn meter_turns_amber_then_red_near_full_scale() {
        assert_eq!(meter_color(meter_fill(0.1)), ACCENT_IDLE); // -20 dBFS
        assert_eq!(meter_color(meter_fill(0.5)), ACCENT_PAUSE); // -6 dBFS
        assert_eq!(meter_color(meter_fill(0.9)), ACCENT_REC); // -0.9 dBFS
    }
}

#[cfg(test)]
mod achieved_fps_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn achieved_fps_averages_over_the_recorded_time() {
        assert_eq!(achieved_fps(600, Duration::from_secs(10)), Some(60.0));
        assert_eq!(achieved_fps(45, Duration::from_millis(1500)), Some(30.0));
    }

    #[test]
    fn achieved_fps_waits_for_the_first_second() {
        assert_eq!(achieved_fps(20, Duration::from_millis(400)), None);
        assert_eq!(achieved_fps(0, Duration::ZERO), None);
    }
}

#[cfg(test)]
mod hotkey_hint_tests {
    use super::*;
    use crate::i18n::Lang;

    #[test]
    fn hotkey_hint_lists_the_keys_for_each_state() {
        let s = Lang::En.strings();
        assert_eq!(
            hotkey_hint(&SessionState::Idle, "F9", "F10", s),
            "F9: record the window in front"
        );
        assert_eq!(
            hotkey_hint(&SessionState::Recording, "F9", "F10", s),
            "F9 stop  ·  F10 pause"
        );
        assert_eq!(
            hotkey_hint(&SessionState::Paused, "F9", "F10", s),
            "F9 stop  ·  F10 resume"
        );
    }

    #[test]
    fn hotkey_hint_leaves_out_unbound_keys() {
        let s = Lang::En.strings();
        assert_eq!(
            hotkey_hint(&SessionState::Recording, "F9", "", s),
            "F9 stop"
        );
        assert_eq!(hotkey_hint(&SessionState::Idle, "", "F10", s), "");
    }
}

#[cfg(test)]
mod group_tracks_by_label_tests {
    use super::*;

    fn labels(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn each_unique_label_becomes_its_own_group() {
        let groups = group_tracks_by_label(&labels(&["Mic", "Speakers", "Spotify"]));
        assert_eq!(
            groups,
            vec![
                ("Mic".to_string(), vec![0]),
                ("Speakers".to_string(), vec![1]),
                ("Spotify".to_string(), vec![2]),
            ]
        );
    }

    #[test]
    fn repeated_label_groups_all_its_indices_together() {
        // Mirrors a multi-process app: "Discord" showing up at tracks 1 and 2.
        let groups = group_tracks_by_label(&labels(&["Mic", "Discord", "Discord"]));
        assert_eq!(
            groups,
            vec![
                ("Mic".to_string(), vec![0]),
                ("Discord".to_string(), vec![1, 2])
            ]
        );
    }

    #[test]
    fn groups_by_label_text_not_just_adjacency() {
        // Same label at non-adjacent indices still ends up in one group.
        let groups = group_tracks_by_label(&labels(&["Discord", "Mic", "Discord"]));
        assert_eq!(
            groups,
            vec![
                ("Discord".to_string(), vec![0, 2]),
                ("Mic".to_string(), vec![1])
            ]
        );
    }

    #[test]
    fn empty_input_produces_no_groups() {
        assert!(group_tracks_by_label(&[]).is_empty());
    }
}
