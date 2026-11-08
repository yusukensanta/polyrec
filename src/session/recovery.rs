//! Recordings cut off -- PolyRec killed, a crash, a power cut -- are left
//! under their temporary names (`<app>_recording_<started, ms>.tmp.mp4`,
//! see `prepare_recording_paths`). Written fragmented (see
//! `encode::fmp4`), they read back up to their last fragment, so at the
//! next start each is finished like any recording, as
//! `<app>_<started>_recovered.mp4`.

use crate::error::AppError;
use crate::recording_naming::reserve_recovered_recording_path;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Recovers the recordings in `polyrec_dir` cut off before `before` -- this
/// start: one begun since is being written. The files they're saved as.
/// A leftover that can't be read (one from before recordings were written
/// fragmented, cut off with nothing readable) stays as it is, as does one
/// still open: another copy of PolyRec recording into it.
pub fn recover_cut_off_recordings(polyrec_dir: &Path, before: SystemTime) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(polyrec_dir) else {
        return Vec::new();
    };
    let before = before
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let mut recovered = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some((app, started)) = cut_off_recording(&entry.file_name()) else {
            continue;
        };
        if started >= before {
            continue;
        }
        if in_use(&path) {
            tracing::info!("left {}: it's being written", path.display());
            continue;
        }
        match recover(&path, &app, started, polyrec_dir) {
            Ok(saved) => {
                tracing::info!("recovered {} as {}", path.display(), saved.display());
                recovered.push(saved);
            }
            Err(e) => tracing::warn!("left {} as it is: {e}", path.display()),
        }
    }
    if !recovered.is_empty() {
        remove_cut_off_finishes(polyrec_dir);
    }
    recovered.sort();
    recovered
}

/// A recording's finish cut off -- PolyRec quit while remuxing a long one,
/// the save on exit timing out -- leaves the empty file that reserved its
/// name (`X.mp4`) and the remux's partial file (`X.tmp.mp4`); the
/// recording itself is still in its temporary file, and recovered.
fn remove_cut_off_finishes(polyrec_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(polyrec_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".tmp.mp4")) else {
            continue;
        };
        if cut_off_recording(&name).is_some() {
            continue;
        }
        let reserved = polyrec_dir.join(format!("{stem}.mp4"));
        if remove_cut_off_finish(&entry.path(), &reserved) {
            tracing::info!("removed a finish cut off: {}", reserved.display());
        }
    }
}

/// Removes a finish's `partial` file and the file `reserved` that holds its
/// name, if `reserved` is still empty: the finish was cut off. Both are held
/// for this process alone from that check to their removal, so a finish
/// still going on -- writing its partial, or renaming it over the reserved
/// name -- is never removed: its files can't be opened, or it can't rename
/// while they're held, and keeps its recording's fragmented file instead.
fn remove_cut_off_finish(partial: &Path, reserved: &Path) -> bool {
    let Some(partial) = open_alone(partial) else {
        return false;
    };
    let Some(reserved) = open_alone(reserved) else {
        return false;
    };
    let empty = reserved
        .metadata()
        .is_ok_and(|meta| meta.is_file() && meta.len() == 0);
    empty && remove_when_closed(&partial) && remove_when_closed(&reserved)
}

/// `path` opened for this process alone, to be removed: not if anything
/// else has it open. A link is opened itself, not what it points to.
fn open_alone(path: &Path) -> Option<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
    };
    std::fs::OpenOptions::new()
        .access_mode(FILE_GENERIC_READ.0 | DELETE.0)
        .share_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
        .ok()
}

/// Marks `file` (opened by `open_alone`) to be removed once it's closed.
fn remove_when_closed(file: &std::fs::File) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FILE_DISPOSITION_INFO, FileDispositionInfo, SetFileInformationByHandle,
    };
    let info = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: `file`'s own handle, opened with DELETE access, and `info`
    // the FILE_DISPOSITION_INFO the call is told it is, alive through it.
    unsafe {
        SetFileInformationByHandle(
            HANDLE(file.as_raw_handle()),
            FileDispositionInfo,
            (&raw const info).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    }
    .is_ok()
}

/// Whether something has `path` open, so it can't be opened for this
/// process alone: another copy of PolyRec (an installed and a portable one
/// can run side by side) recording into it, or finishing a recording.
fn in_use(path: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(path)
        .is_err()
}

/// The app and start (ms since the epoch) of a recording's temporary file,
/// `<app>_recording_<started>.tmp.mp4`.
fn cut_off_recording(name: &OsStr) -> Option<(String, u128)> {
    let stem = name.to_str()?.strip_suffix(".tmp.mp4")?;
    let (app, started) = stem.rsplit_once("_recording_")?;
    Some((app.to_owned(), started.parse().ok()?))
}

fn recover(path: &Path, app: &str, started: u128, dir: &Path) -> Result<PathBuf, AppError> {
    let started = UNIX_EPOCH + Duration::from_millis(started as u64);
    crate::encode::fmp4::finish(
        path,
        || Ok(reserve_recovered_recording_path(app, started.into(), dir)?),
        // Asked only of a file written before recordings were finished in
        // place: whether it reads at all, and how many audio tracks it has.
        || crate::encode::remux::count_audio_tracks(path),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::fmp4::tests::{
        cut_off_after_index_copy, record_finished, record_fragmented, top_level,
    };
    use std::time::{Duration, UNIX_EPOCH};

    fn millis(at: SystemTime) -> u128 {
        at.duration_since(UNIX_EPOCH).unwrap().as_millis()
    }

    #[test]
    fn a_recording_cut_off_is_saved_as_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let started = SystemTime::now() - Duration::from_secs(60);
        let leftover = dir
            .path()
            .join(format!("game_recording_{}.tmp.mp4", millis(started)));
        record_fragmented(&leftover);

        let recovered = recover_cut_off_recordings(dir.path(), SystemTime::now());
        assert_eq!(recovered.len(), 1, "{recovered:?}");
        let name = recovered[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let stamp = chrono::DateTime::<chrono::Local>::from(started).format("%Y-%m-%d-%H-%M-%S");
        assert_eq!(name, format!("game_{stamp}_recovered.mp4"));
        assert!(!leftover.exists());
        assert!(
            !top_level(&recovered[0]).contains(&"moof".to_owned()),
            "a plain MP4"
        );
    }

    /// PolyRec quit (or the power went) while finishing a recording, after
    /// its index was written but before its header went in: it's finished
    /// at the next start all the same.
    #[test]
    fn a_recording_whose_finish_was_cut_off_after_its_index_is_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let started = SystemTime::now() - Duration::from_secs(60);
        let leftover = dir
            .path()
            .join(format!("game_recording_{}.tmp.mp4", millis(started)));
        let elsewhere = tempfile::tempdir().unwrap();
        let finished = elsewhere.path().join("rec.mp4");
        record_finished(&finished, 90);
        cut_off_after_index_copy(&finished, &leftover);

        let recovered = recover_cut_off_recordings(dir.path(), SystemTime::now());
        assert_eq!(recovered.len(), 1, "{recovered:?}");
        assert!(!leftover.exists());
        assert_eq!(
            std::fs::read(&recovered[0]).unwrap(),
            std::fs::read(&finished).unwrap(),
            "finished as at Stop"
        );
    }

    /// Cut off after the recording was finished, before it was moved to
    /// its name: it's moved as it is.
    #[test]
    fn a_recording_finished_but_not_renamed_is_recovered_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let started = SystemTime::now() - Duration::from_secs(60);
        let leftover = dir
            .path()
            .join(format!("game_recording_{}.tmp.mp4", millis(started)));
        record_finished(&leftover, 90);
        let bytes = std::fs::read(&leftover).unwrap();

        let recovered = recover_cut_off_recordings(dir.path(), SystemTime::now());
        assert_eq!(recovered.len(), 1, "{recovered:?}");
        assert!(!leftover.exists());
        assert_eq!(std::fs::read(&recovered[0]).unwrap(), bytes);
    }

    /// A recording started after this start is one being written; a file
    /// that isn't a recording's isn't touched; one that can't be read stays.
    #[test]
    fn only_readable_recordings_cut_off_before_this_start_are_touched() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let being_written = dir
            .path()
            .join(format!("game_recording_{}.tmp.mp4", millis(now) + 5000));
        record_fragmented(&being_written);
        let export_partial = dir.path().join("game_2026-11-05.tmp.mp4");
        record_fragmented(&export_partial);
        let unreadable = dir.path().join("old_recording_1000.tmp.mp4");
        std::fs::write(&unreadable, b"mdat with no index").unwrap();

        assert!(recover_cut_off_recordings(dir.path(), now).is_empty());
        for path in [&being_written, &export_partial, &unreadable] {
            assert!(path.exists(), "{} was touched", path.display());
        }
        let files = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(files, 3, "nothing else left behind");
    }

    /// Another copy of PolyRec (an installed one and a portable one run
    /// side by side) is recording into the same folder: its file, open, is
    /// left alone, not finished from under it.
    #[test]
    fn a_recording_still_being_written_is_left_alone() {
        use crate::encode::RecordingWriter;
        use crate::encode::fmp4::tests::{params, record};

        let dir = tempfile::tempdir().unwrap();
        let started = SystemTime::now() - Duration::from_secs(60);
        let name = format!("game_recording_{}.tmp.mp4", millis(started));
        let being_written = dir.path().join(&name);
        let writer = RecordingWriter::new(&being_written, &params()).expect("writer");
        writer.begin_writing().unwrap();
        record(&writer, 3, false);

        let recovered = recover_cut_off_recordings(dir.path(), SystemTime::now());
        writer.finalize().expect("finalize");
        assert!(recovered.is_empty(), "{recovered:?}");
        let files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(files, [name.as_str()]);
    }

    /// Another copy of PolyRec is finishing a recording: the remux it's
    /// writing and the name it reserved stay.
    #[test]
    fn a_finish_still_being_written_is_left_alone() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;

        let dir = tempfile::tempdir().unwrap();
        let started = SystemTime::now() - Duration::from_secs(60);
        record_fragmented(
            &dir.path()
                .join(format!("game_recording_{}.tmp.mp4", millis(started))),
        );
        let reserved = dir.path().join("game_2026-11-05-01-00-00.mp4");
        std::fs::write(&reserved, b"").unwrap();
        let partial = dir.path().join("game_2026-11-05-01-00-00.tmp.mp4");
        let _writing = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(FILE_SHARE_READ.0)
            .open(&partial)
            .unwrap();

        assert_eq!(
            recover_cut_off_recordings(dir.path(), SystemTime::now()).len(),
            1
        );
        assert!(reserved.exists() && partial.exists());
    }

    /// PolyRec quit while finishing a long recording, the save on exit
    /// timing out: left behind are the empty file that reserved its name,
    /// the remux's partial file, and the recording itself, recovered.
    #[test]
    fn a_finish_cut_off_leaves_only_the_recovered_recording() {
        let dir = tempfile::tempdir().unwrap();
        let started = SystemTime::now() - Duration::from_secs(60);
        let leftover = dir
            .path()
            .join(format!("game_recording_{}.tmp.mp4", millis(started)));
        record_fragmented(&leftover);
        let reserved = dir.path().join("game_2026-11-05-01-00-00.mp4");
        std::fs::write(&reserved, b"").unwrap();
        let partial = dir.path().join("game_2026-11-05-01-00-00.tmp.mp4");
        std::fs::write(&partial, b"half a remux").unwrap();
        // A recording someone kept, and a partial with no empty name beside it.
        let kept = dir.path().join("other_2026.mp4");
        std::fs::write(&kept, b"a recording").unwrap();
        let unrelated = dir.path().join("export.tmp.mp4");
        std::fs::write(&unrelated, b"an export").unwrap();

        let recovered = recover_cut_off_recordings(dir.path(), SystemTime::now());
        assert_eq!(recovered.len(), 1);
        assert!(!reserved.exists() && !partial.exists(), "debris removed");
        assert!(kept.exists() && unrelated.exists(), "nothing else touched");
    }

    /// A finish going on beside recovery -- another copy of PolyRec about
    /// to rename its finished file over the reserved name -- holds that
    /// name: neither file of it is removed.
    #[test]
    fn a_finish_whose_name_is_held_is_left_alone() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;

        let dir = tempfile::tempdir().unwrap();
        let started = SystemTime::now() - Duration::from_secs(60);
        record_fragmented(
            &dir.path()
                .join(format!("game_recording_{}.tmp.mp4", millis(started))),
        );
        let reserved = dir.path().join("game_2026-11-05-01-00-00.mp4");
        std::fs::write(&reserved, b"").unwrap();
        let partial = dir.path().join("game_2026-11-05-01-00-00.tmp.mp4");
        std::fs::write(&partial, b"a finished remux").unwrap();
        let _held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0)
            .open(&reserved)
            .unwrap();

        assert_eq!(
            recover_cut_off_recordings(dir.path(), SystemTime::now()).len(),
            1
        );
        assert!(reserved.exists() && partial.exists());
    }

    /// A partial that's a link is removed itself; what it points to stays.
    /// Making a link takes Developer Mode or an elevated test run (CI's).
    #[test]
    fn a_finish_cut_off_through_a_link_removes_only_the_link() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let started = SystemTime::now() - Duration::from_secs(60);
        record_fragmented(
            &dir.path()
                .join(format!("game_recording_{}.tmp.mp4", millis(started))),
        );
        let reserved = dir.path().join("game_2026-11-05-01-00-00.mp4");
        std::fs::write(&reserved, b"").unwrap();
        let target = elsewhere.path().join("kept.mp4");
        std::fs::write(&target, b"someone's recording").unwrap();
        let partial = dir.path().join("game_2026-11-05-01-00-00.tmp.mp4");
        if let Err(e) = std::os::windows::fs::symlink_file(&target, &partial) {
            eprintln!("skipped: can't make a link here ({e})");
            return;
        }

        recover_cut_off_recordings(dir.path(), SystemTime::now());
        assert!(!partial.exists() && !reserved.exists(), "debris removed");
        assert_eq!(std::fs::read(&target).unwrap(), b"someone's recording");
    }

    #[test]
    fn a_folder_that_isnt_there_has_nothing_to_recover() {
        let dir = tempfile::tempdir().unwrap();
        assert!(recover_cut_off_recordings(&dir.path().join("gone"), SystemTime::now()).is_empty());
    }
}
