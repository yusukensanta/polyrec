//! What went wrong, when PolyRec fails: every panic, on any thread, is
//! appended to `crash.log` next to `config.toml`. The release build has no
//! console, so before this a panic on the dashboard's thread made PolyRec
//! vanish without leaving anything behind to say why.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// Past this the log starts over, keeping the one before as
/// `crash.log.old`, so a panic that keeps repeating can't fill the disk.
const MAX_LOG_BYTES: u64 = 256 * 1024;

/// Where the last panic logged on the main thread was raised -- see
/// `last_main_thread_panic_location`.
static LAST_PANIC_LOCATION: Mutex<Option<String>> = Mutex::new(None);

/// Where the log goes: next to `config.toml`.
pub fn path() -> PathBuf {
    crate::config::Config::config_path().with_file_name("crash.log")
}

/// Installs the panic hook that writes the log, in front of the default one
/// (which still prints to the console in a debug build). `caught_on_purpose`
/// names panics PolyRec catches and recovers from as a matter of course;
/// those aren't logged or printed at all.
pub fn install(caught_on_purpose: fn(&str) -> bool) {
    let path = path();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info.payload_as_str().unwrap_or("(no message)");
        if caught_on_purpose(message) {
            return;
        }
        let thread = std::thread::current();
        // UTC: a panic in here would abort, and local time can panic on
        // unusual time-zone data.
        let time = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
        let location = info.location().map(ToString::to_string);
        if thread.name() == Some("main") {
            *LAST_PANIC_LOCATION
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = location.clone();
        }
        let entry = entry(
            &time.to_string(),
            thread.name().unwrap_or("unnamed"),
            message,
            location.as_deref(),
        );
        // Nowhere to report a failure to: the console is what this is for.
        let _ = append(&path, &entry);
        previous(info);
    }));
}

/// Where the last panic logged on the main thread -- the dashboard's -- was
/// raised (`file:line:column`), if any.
pub fn last_main_thread_panic_location() -> Option<String> {
    LAST_PANIC_LOCATION
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

fn entry(time: &str, thread: &str, message: &str, location: Option<&str>) -> String {
    format!(
        "{time}  PolyRec {}  thread '{thread}' panicked at {}:\n{message}\n\n",
        env!("CARGO_PKG_VERSION"),
        location.unwrap_or("an unknown place"),
    )
}

fn append(path: &Path, entry: &str) -> std::io::Result<()> {
    if std::fs::metadata(path).is_ok_and(|meta| meta.len() > MAX_LOG_BYTES) {
        std::fs::rename(path, path.with_extension("log.old"))?;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(entry.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_says_when_where_and_what() {
        let entry = entry(
            "2026-11-01 01:02:03 UTC",
            "main",
            "Failed to create staging buffer",
            Some("src/renderer.rs:1033:17"),
        );
        assert_eq!(
            entry,
            format!(
                "2026-11-01 01:02:03 UTC  PolyRec {}  thread 'main' panicked at src/renderer.rs:1033:17:\nFailed to create staging buffer\n\n",
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    #[test]
    fn entries_are_added_to_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("PolyRec").join("crash.log");
        append(&path, "first\n").unwrap();
        append(&path, "second\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\nsecond\n");
    }

    #[test]
    fn the_log_starts_over_past_its_limit_keeping_the_last_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crash.log");
        let full = "x".repeat(MAX_LOG_BYTES as usize + 1);
        std::fs::write(&path, &full).unwrap();
        append(&path, "new\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("crash.log.old")).unwrap(),
            full
        );
    }
}
