//! Feature-scoped pieces of the dashboard `App`'s state -- each struct
//! groups the fields one feature reads and writes together, so `App` itself
//! reads as a list of features rather than ~50 loose fields.

use crate::encode::actor::StoppedItself;
use crate::error::AppError;
use crate::session::SessionManager;
use crate::sources::InstalledApp;
use eframe::egui;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Instant;
use tokio::task::JoinHandle;

pub(super) enum ExportState {
    Idle,
    Running,
    Done(PathBuf),
    Failed(String),
}

pub(super) enum HighlightSaveState {
    Idle,
    Saving,
    Done(PathBuf),
    Failed(String),
}

pub(super) enum SelfUpdateState {
    Idle,
    Confirming(crate::update_check::AvailableUpdate),
    Working,
    Failed(String),
}

/// Free space on `config.output_dir`'s volume, refreshed periodically (see
/// `refresh_free_space`) rather than on every frame -- `GetDiskFreeSpaceExW`
/// is fast, but there's no reason to make a syscall 30-60+ times a second for
/// a number that only meaningfully changes over seconds, same reasoning as
/// the recording loop's own `DISK_CHECK_INTERVAL`.
#[derive(Default)]
pub(super) struct FreeSpaceCache {
    pub bytes: Option<u64>,
    pub checked_at: Option<Instant>,
    pub checked_dir: Option<PathBuf>,
}

/// The recording status lines' slow parts, refreshed only when the elapsed
/// time ticks over to a new second, not every frame. Previewing a
/// `RecordingNaming::CustomPrefix` name lists a directory
/// (`recording_naming::next_sequence_number`), and the size is a
/// filesystem call.
pub(super) struct RecordingStatusCache {
    /// The elapsed second this was computed at.
    pub secs: u64,
    /// The name the recording will be saved as.
    pub name: String,
    /// The recording file's size so far, if it could be read.
    pub file_bytes: Option<u64>,
}

/// A stopped recording whose recorder is still finalizing the file in the
/// background -- see `stop_recording` / `poll_background_work`.
#[derive(Default)]
pub(super) struct Finalizing {
    pub handle: Option<JoinHandle<Result<PathBuf, AppError>>>,
    /// Why the recorder stopped itself, if it did -- see `StoppedItself`.
    pub stopped_itself: StoppedItself,
    /// What the recording being saved lost before it was stopped.
    pub lost_sources: crate::capture::LostSources,
}

impl Finalizing {
    /// Stops `session`'s manual recording and keeps what reporting on its
    /// save needs. Whether there was one to stop: with none -- a second
    /// Stop queued behind the first -- the save under way is left alone.
    pub(super) fn stop(&mut self, session: &mut SessionManager) -> bool {
        let Some(active) = session.active() else {
            return false;
        };
        tracing::info!(
            "recording stop requested, finalizing: {}",
            active.output_path.display()
        );
        self.stopped_itself = active.stopped_itself.clone();
        self.lost_sources = active.lost_sources();
        self.handle = session.stop_capture();
        true
    }
}

/// The manual recording the dashboard reports on and exports: the one under
/// way, or else the last one.
#[derive(Default)]
pub(super) struct LastRecording {
    /// Where it was saved: `None` until it is, and for one that failed.
    pub saved: Option<PathBuf>,
    /// Display label for each of its audio tracks (set when it starts;
    /// devices, then app-audio sources, in the same order
    /// `start_recording_with_source` assigns track ids) -- NOT the live
    /// system device list (`audio_devices`), which reflects whatever's
    /// currently plugged in and has no entries for app-audio sources at
    /// all. The export checkbox list zips this against
    /// `export.track_selection` by position; a length mismatch (e.g. a
    /// device disconnected mid-recording, producing fewer tracks than were
    /// selected) falls back to a generic "Track N" label for the unlabeled
    /// tail rather than panicking or misattributing a name.
    pub audio_labels: Vec<super::actions::TrackLabel>,
    /// Its app name (same derivation as the recording's own filename, via
    /// `session::app_name_from_exe`) -- used to pre-fill the export Save
    /// dialog's default filename. Stored at record-start rather than
    /// re-parsed from the recording's filename, so it can't drift if that
    /// filename format ever changes.
    pub app_name: String,
}

impl LastRecording {
    /// A new recording started, with these tracks; it's the one reported on
    /// from now on. The last one's file goes with the rest of it: kept, it
    /// showed as just saved after a new recording that failed, and Export
    /// used the new recording's track labels on it.
    pub(super) fn started(
        &mut self,
        audio_labels: Vec<super::actions::TrackLabel>,
        app_name: String,
    ) {
        *self = Self {
            saved: None,
            audio_labels,
            app_name,
        };
    }
}

/// The post-recording export controls -- see `render_export_controls`.
pub(super) struct ExportUi {
    /// How many audio streams the last finished recording actually contains,
    /// probed from the file itself (see `remux::count_audio_tracks`) rather
    /// than trusted from the pre-recording device selection. Export controls
    /// only make sense with 2+ tracks to choose between -- with 0 or 1,
    /// there's nothing a track-selection export could meaningfully remove.
    pub available_tracks: usize,
    pub track_selection: Vec<bool>,
    /// "Mix selected tracks into one" export checkbox -- unchecked by
    /// default.
    pub mix_tracks: bool,
    pub state: ExportState,
    pub result_rx: Option<mpsc::Receiver<Result<PathBuf, String>>>,
}

impl ExportUi {
    pub(super) fn new(track_selection: Vec<bool>) -> Self {
        Self {
            available_tracks: 0,
            track_selection,
            mix_tracks: false,
            state: ExportState::Idle,
            result_rx: None,
        }
    }
}

/// The Audio popup's "+ Add app" picker -- opens inline instead of going
/// straight to a native file browser, so an app can be found and pinned via
/// `Config::register_app_audio` by searching (currently open windows, or
/// installed-but-not-running apps found via Start Menu shortcuts) instead of
/// needing to know where it's installed. A native file browser remains as a
/// fallback ("Browse for .exe instead…") for the rare app neither search
/// source finds.
#[derive(Default)]
pub(super) struct AddAppPicker {
    pub open: bool,
    /// Live text from the picker's search box -- filters both currently open
    /// windows and installed-but-not-running apps by exe/display name, same
    /// convention as `source_filter`.
    pub search: String,
    /// Installed apps found by resolving Start Menu shortcuts, so the picker
    /// can also find one that isn't running yet -- scanned once when the
    /// picker opens (`enumerate_installed_apps` walks every `.lnk` under two
    /// Start Menu folders and resolves each via COM, too slow to redo every
    /// frame), not refreshed again until it's reopened. Populated
    /// asynchronously (see `installed_rx`) rather than on the UI thread --
    /// open-window candidates still show immediately in the meantime, since
    /// those come from the already-available `sources`.
    pub installed: Vec<InstalledApp>,
    /// One-shot channel for the background `enumerate_installed_apps` scan
    /// kicked off when the picker opens -- polled in `poll_background_work`,
    /// same pattern as `update_check_rx`. `None` once the result has arrived
    /// (or the picker was never opened).
    pub installed_rx: Option<mpsc::Receiver<Vec<InstalledApp>>>,
    /// Textures for picker candidates -- keyed by lowercased exe_name (not
    /// list index, unlike `app_audio_icon_textures`) since the candidate
    /// list's order and membership shift on every keystroke as the search
    /// filter changes; keying by identity means a texture already loaded for
    /// a still-matching candidate is never needlessly reloaded. Persists for
    /// the app's lifetime once loaded, same "clear on list change, rebuild
    /// lazily" spirit as the other icon caches, just never needing the clear
    /// since the key is stable.
    pub icon_textures: HashMap<String, egui::TextureHandle>,
}

/// A Highlight save in flight and its last result.
pub(super) struct HighlightSave {
    pub state: HighlightSaveState,
    pub handle: Option<JoinHandle<Result<PathBuf, AppError>>>,
}

impl Default for HighlightSave {
    fn default() -> Self {
        Self {
            state: HighlightSaveState::Idle,
            handle: None,
        }
    }
}

/// The in-app self-update flow (confirm -> download/verify/install).
pub(super) struct SelfUpdate {
    pub state: SelfUpdateState,
    pub handle: Option<JoinHandle<Result<(), AppError>>>,
}

impl Default for SelfUpdate {
    fn default() -> Self {
        Self {
            state: SelfUpdateState::Idle,
            handle: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The last recording's file stayed the one reported on through a new
    /// recording: if that one failed, the old file still showed as just
    /// saved -- and Export used the new recording's track labels on it.
    #[test]
    fn a_new_recording_takes_over_from_the_last_one_saved() {
        let mut last = LastRecording {
            saved: Some(PathBuf::from(r"E:\recordings\polyrec\game.mp4")),
            audio_labels: Vec::new(),
            app_name: "game".into(),
        };
        last.started(Vec::new(), "editor".into());
        assert_eq!(last.saved, None);
        assert_eq!(last.app_name, "editor");
    }

    /// The start/stop hotkey pressed twice before the dashboard caught up
    /// (a game holding the GPU, say) queues two Stops. The second finds
    /// nothing left to stop -- and must leave the save the first one
    /// started alone, or the saved recording's panel, and any disk-full or
    /// lost-source notice, never show.
    #[tokio::test]
    async fn a_second_stop_leaves_the_first_ones_save_alone() {
        let mut session = SessionManager::recording_stuck();
        let mut finalizing = Finalizing::default();
        assert!(finalizing.stop(&mut session));
        assert!(finalizing.handle.is_some());
        assert!(!finalizing.stop(&mut session));
        assert!(finalizing.handle.is_some());
    }
}
