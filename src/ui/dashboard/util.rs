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

pub(super) fn open_folder(path: &std::path::Path) {
    let folder = path.parent().unwrap_or(path);
    let _ = std::process::Command::new(EXPLORER_EXE).arg(folder).spawn();
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
