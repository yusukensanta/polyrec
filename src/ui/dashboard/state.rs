//! Feature-scoped pieces of the dashboard `App`'s state -- each struct
//! groups the fields one feature reads and writes together, so `App` itself
//! reads as a list of features rather than ~50 loose fields.

use crate::error::AppError;
use crate::sources::InstalledApp;
use eframe::egui;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
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
    pub path: Option<PathBuf>,
    pub disk_full: Option<Arc<AtomicBool>>,
    /// What the recording being saved lost before it was stopped.
    pub lost_sources: crate::capture::LostSources,
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
