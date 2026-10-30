/// Takes `handle` if it's finished, leaving `None` behind either way -- so a
/// caller can `if let Some(h) = take_if_finished(&mut self.foo_handle) { ... }`
/// once per frame instead of repeating the `is_some_and(|h| h.is_finished())`
/// guard by hand. The guard matters, not just boilerplate: `.await`ing (or
/// `block_on`-ing) a `JoinHandle` that hasn't finished yet blocks until it
/// does, which on this render path means stalling the UI thread for as long
/// as the background task takes.
pub(super) fn take_if_finished<T>(
    handle: &mut Option<tokio::task::JoinHandle<T>>,
) -> Option<tokio::task::JoinHandle<T>> {
    if handle.as_ref().is_some_and(|h| h.is_finished()) {
        handle.take()
    } else {
        None
    }
}

/// Saves `config`, logging and setting `*error_message` to
/// `format!("{prefix}{e}")` on failure -- the common reaction to the
/// filesystem write itself failing (permissions, disk full, path gone)
/// shared by every config-mutating action across the dashboard that wants
/// immediate persistence. A free function taking individual field borrows
/// (not `&mut App`) so it also works from contexts like
/// `checkbox_with_volume_slider` that only have those borrows available;
/// `App::save_config_or_report` is a thin `&mut self` wrapper over this for
/// every other call site.
pub(super) fn save_config_or_report(
    config: &mut crate::config::Config,
    error_message: &mut Option<String>,
    prefix: &str,
) {
    if let Err(e) = config.save() {
        tracing::error!("failed to save config: {e}");
        *error_message = Some(format!("{prefix}{e}"));
    }
}

/// Full path rather than a bare "explorer" name — avoids relying on Windows'
/// executable search order (a directory ahead of System32 in PATH could
/// otherwise shadow the real explorer.exe).
const EXPLORER_EXE: &str = r"C:\Windows\explorer.exe";

/// Opens the folder containing the file at `path` in Explorer.
pub(super) fn open_folder(path: &std::path::Path) {
    open_directory(path.parent().unwrap_or(path));
}

/// Opens the folder `dir` itself in Explorer.
pub(super) fn open_directory(dir: &std::path::Path) {
    let _ = std::process::Command::new(EXPLORER_EXE).arg(dir).spawn();
}

/// Why recordings can't be saved under an output folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OutputDirProblem {
    /// Nothing along the path exists: its drive or network share isn't there.
    Unavailable,
    /// The path leads through a file.
    NotAFolder,
}

/// What would stop recordings being saved under `dir`, if anything. A folder
/// that doesn't exist yet is fine, since starting a recording creates it
/// (`prepare_recording_paths`). What isn't fine: nothing along the path
/// exists (its drive or network share isn't there), or the nearest part that
/// does exist is a file.
pub(super) fn output_dir_problem(dir: &std::path::Path) -> Option<OutputDirProblem> {
    let nearest_existing = dir
        .ancestors()
        .find(|p| !p.as_os_str().is_empty() && p.exists());
    match nearest_existing {
        None => Some(OutputDirProblem::Unavailable),
        Some(p) if !p.is_dir() => Some(OutputDirProblem::NotAFolder),
        Some(_) => None,
    }
}

/// Only ever called with a GitHub release page URL (see `update_check.rs`), but
/// validated anyway since it's the one place in the app that opens a string
/// pulled from a network response rather than a local path: an `explorer.exe`
/// argument that turned out to be a UNC path (`\\host\share`) rather than a URL
/// would make Explorer silently attempt an SMB connection using the current
/// Windows credentials -- a known NTLM-hash-leak technique. Requiring an
/// `https://github.com/` prefix rules that out.
pub(super) fn open_url(url: &str) {
    if !url.starts_with("https://github.com/") {
        tracing::warn!("refusing to open unexpected update URL: {url}");
        return;
    }
    let _ = std::process::Command::new(EXPLORER_EXE).arg(url).spawn();
}

#[cfg(test)]
mod output_dir_problem_tests {
    use super::*;

    #[test]
    fn an_existing_folder_or_one_still_to_be_created_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(output_dir_problem(dir.path()), None);
        assert_eq!(
            output_dir_problem(&dir.path().join("new").join("deeper")),
            None
        );
    }

    #[test]
    fn a_path_through_a_file_is_not_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file.txt");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(
            output_dir_problem(&file),
            Some(OutputDirProblem::NotAFolder)
        );
        assert_eq!(
            output_dir_problem(&file.join("sub")),
            Some(OutputDirProblem::NotAFolder)
        );
    }

    #[test]
    fn a_drive_that_isnt_there_is_unavailable() {
        let missing_drive = ('D'..='Z')
            .rev()
            .map(|letter| format!("{letter}:\\"))
            .find(|root| !std::path::Path::new(root).exists())
            .expect("every drive letter D-Z is in use");
        let dir = std::path::Path::new(&missing_drive).join("Recordings");
        assert_eq!(
            output_dir_problem(&dir),
            Some(OutputDirProblem::Unavailable)
        );
    }
}
