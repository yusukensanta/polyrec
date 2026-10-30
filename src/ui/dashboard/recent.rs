//! The dashboard's "Recent" list: the newest finished recordings and
//! Highlight saves, with buttons to play one or show it in Explorer.

use super::App;
use super::theme::{ACCENT_SECONDARY, SPACE_TIGHT, TEXT_MUTED};
use super::util::{open_file, show_in_folder};
use super::widgets::{caption, format_bytes, section_header};
use crate::i18n::Strings;
use eframe::egui;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// How many recordings the list shows.
pub(super) const RECENT_LIMIT: usize = 3;
/// One list row's height.
const ROW_HEIGHT: f32 = 22.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RecentRecording {
    pub path: PathBuf,
    pub bytes: u64,
    modified: SystemTime,
}

/// The newest `limit` finished `.mp4`s under `output_dir`'s `polyrec`
/// folder, where recordings land, and its `highlights` subfolder, where
/// Highlight saves land. In-progress recordings (`*.tmp.mp4`) are left
/// out. A folder that can't be read just contributes nothing.
pub(super) fn recent_recordings(output_dir: &Path, limit: usize) -> Vec<RecentRecording> {
    let polyrec = output_dir.join("polyrec");
    let mut found: Vec<RecentRecording> = [polyrec.clone(), polyrec.join("highlights")]
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if !name.ends_with(".mp4") || name.ends_with(".tmp.mp4") {
                return None;
            }
            let meta = entry.metadata().ok().filter(std::fs::Metadata::is_file)?;
            Some(RecentRecording {
                path: entry.path(),
                bytes: meta.len(),
                modified: meta.modified().ok()?,
            })
        })
        .collect();
    found.sort_by_key(|r| std::cmp::Reverse(r.modified));
    found.truncate(limit);
    found
}

impl App {
    /// Re-reads the Recent list -- at startup, after a recording or
    /// Highlight save finishes, and when the output folder changes; not
    /// every frame, since it lists directories.
    pub(super) fn refresh_recent(&mut self) {
        self.recent = recent_recordings(&self.config.output_dir, RECENT_LIMIT);
    }

    /// The Recent list, while nothing is recording.
    pub(super) fn render_recent(&self, ui: &mut egui::Ui, s: &'static Strings) {
        if self.recent.is_empty() || self.session.is_recording() || self.session.is_paused() {
            return;
        }
        ui.add_space(SPACE_TIGHT);
        section_header(ui, s.recent_header);
        ui.scope(|ui| {
            // Compact rows; scoped so nothing after them inherits it.
            ui.spacing_mut().interact_size.y = ROW_HEIGHT;
            for recording in &self.recent {
                self.render_recent_row(ui, recording, s);
            }
        });
    }

    fn render_recent_row(&self, ui: &mut egui::Ui, recording: &RecentRecording, s: &Strings) {
        let name = recording
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        ui.horizontal(|ui| {
            if ui
                .small_button(egui::RichText::new("▶").color(ACCENT_SECONDARY))
                .on_hover_text(s.recent_play_tooltip)
                .clicked()
            {
                open_file(&recording.path);
            }
            if ui
                .small_button(egui::RichText::new("📁").color(ACCENT_SECONDARY))
                .on_hover_text(s.recent_show_tooltip)
                .clicked()
            {
                show_in_folder(&recording.path);
            }
            ui.add(
                egui::Label::new(caption(
                    format!("{name}  ·  {}", format_bytes(recording.bytes)),
                    TEXT_MUTED,
                ))
                .truncate(),
            )
            .on_hover_text(recording.path.to_string_lossy());
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn file(path: &Path, bytes: usize, age_secs: u64) {
        std::fs::write(path, vec![0u8; bytes]).unwrap();
        let modified = SystemTime::now() - Duration::from_secs(age_secs);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
    }

    fn names(list: &[RecentRecording]) -> Vec<String> {
        list.iter()
            .map(|r| r.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn newest_recordings_and_highlights_first_without_in_progress_ones() {
        let out = tempfile::tempdir().unwrap();
        let polyrec = out.path().join("polyrec");
        std::fs::create_dir_all(polyrec.join("highlights")).unwrap();
        std::fs::create_dir_all(polyrec.join("_highlight_buffer")).unwrap();
        file(&polyrec.join("old.mp4"), 10, 300);
        file(&polyrec.join("new.mp4"), 20, 10);
        file(&polyrec.join("live_recording_1.tmp.mp4"), 5, 1);
        file(&polyrec.join("notes.txt"), 5, 1);
        file(&polyrec.join("highlights").join("clip.mp4"), 30, 100);
        file(&polyrec.join("_highlight_buffer").join("seg.mp4"), 5, 1);

        let recent = recent_recordings(out.path(), 3);
        assert_eq!(names(&recent), ["new.mp4", "clip.mp4", "old.mp4"]);
        assert_eq!(recent[0].bytes, 20);
        assert_eq!(names(&recent_recordings(out.path(), 1)), ["new.mp4"]);
    }

    #[test]
    fn a_missing_folder_is_just_empty() {
        let out = tempfile::tempdir().unwrap();
        assert!(recent_recordings(&out.path().join("nowhere"), 3).is_empty());
    }
}
