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
/// fragmented, cut off with nothing readable) stays as it is.
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
        if std::fs::metadata(&reserved).is_ok_and(|m| m.is_file() && m.len() == 0) {
            tracing::info!("removing a finish cut off: {}", reserved.display());
            let _ = std::fs::remove_file(entry.path());
            let _ = std::fs::remove_file(&reserved);
        }
    }
}

/// The app and start (ms since the epoch) of a recording's temporary file,
/// `<app>_recording_<started>.tmp.mp4`.
fn cut_off_recording(name: &OsStr) -> Option<(String, u128)> {
    let stem = name.to_str()?.strip_suffix(".tmp.mp4")?;
    let (app, started) = stem.rsplit_once("_recording_")?;
    Some((app.to_owned(), started.parse().ok()?))
}

fn recover(path: &Path, app: &str, started: u128, dir: &Path) -> Result<PathBuf, AppError> {
    // Whether it reads at all, and how many audio tracks it has.
    let audio_tracks = crate::encode::remux::count_audio_tracks(path)?;
    let started = UNIX_EPOCH + Duration::from_millis(started as u64);
    let saved = reserve_recovered_recording_path(app, started.into(), dir)?;
    crate::encode::fmp4::finish(path, &saved, audio_tracks).inspect_err(|_| {
        let _ = std::fs::remove_file(&saved);
    })?;
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::fmp4::tests::{record_fragmented, top_level};
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

    #[test]
    fn a_folder_that_isnt_there_has_nothing_to_recover() {
        let dir = tempfile::tempdir().unwrap();
        assert!(recover_cut_off_recordings(&dir.path().join("gone"), SystemTime::now()).is_empty());
    }
}
