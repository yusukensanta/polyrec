//! PolyRec's log: what it reports as it runs -- a recording recovered, a
//! device gone, an encoder falling back -- written to `polyrec.log` next to
//! `config.toml`. The release build has no console, so before this all of
//! it was lost. `RUST_LOG` still picks what's logged.

use std::borrow::Cow;
use std::fs::File;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// Past this the log starts over, keeping the one before (see `LogFile`).
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;

/// What's logged without `RUST_LOG`: PolyRec's own reports from `info` up,
/// the libraries' (wgpu, eframe, ...) warnings and errors.
const DEFAULT_FILTER: &str = "warn,polyrec=info";

/// Where the log goes: next to `config.toml`.
pub fn path() -> PathBuf {
    crate::config::Config::config_path().with_file_name("polyrec.log")
}

/// Starts logging: to the log file, and to the console in a debug build
/// (the release build has none).
pub fn init() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let console = cfg!(debug_assertions).then(tracing_subscriber::fmt::layer);
    let started = tracing_subscriber::registry()
        .with(filter)
        .with(file_layer(LogFile::new(path(), MAX_LOG_BYTES)))
        .with(console)
        .try_init();
    if started.is_ok() {
        tracing::info!("PolyRec {} started", env!("CARGO_PKG_VERSION"));
    }
}

/// Writes each event as a line of plain text to `log`.
fn file_layer<S>(log: LogFile) -> impl Layer<S>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    tracing_subscriber::fmt::layer()
        .with_writer(Mutex::new(log))
        .with_ansi(false)
}

/// A log file that starts over once it's past `max_bytes`, keeping the one
/// before as `<name>.old`, so a log that keeps growing can't fill the disk.
/// Opened on the first write, and kept open.
pub struct LogFile {
    path: PathBuf,
    max_bytes: u64,
    /// The file, and how long it is.
    open: Option<(File, u64)>,
    /// The user's own folder, hidden in what's written (see `hide_home`).
    home: String,
}

impl LogFile {
    pub fn new(path: PathBuf, max_bytes: u64) -> Self {
        Self {
            path,
            max_bytes,
            open: None,
            home: std::env::var("USERPROFILE").unwrap_or_default(),
        }
    }

    fn file(&mut self) -> io::Result<&mut (File, u64)> {
        if self
            .open
            .as_ref()
            .is_some_and(|&(_, len)| len > self.max_bytes)
        {
            self.open = None;
        }
        if self.open.is_none() {
            let mut len = std::fs::metadata(&self.path).map_or(0, |meta| meta.len());
            if len > self.max_bytes {
                // Should keeping it fail (something holds the last one), the
                // log starts over without it rather than growing on.
                if std::fs::rename(&self.path, self.path.with_extension("log.old")).is_err() {
                    File::create(&self.path)?;
                }
                len = 0;
            }
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
            self.open = Some((file, len));
        }
        Ok(self.open.as_mut().expect("opened above"))
    }
}

/// How the user's own folder is written in the log.
const HOME_HIDDEN: &str = "%USERPROFILE%";

/// `text` with the user's own folder `home` (`C:\Users\<name>`) as
/// `%USERPROFILE%`, written plainly or with its backslashes doubled as
/// `{:?}` prints a path: users paste the log into public bug reports, and
/// it shouldn't carry their Windows user name. Matched regardless of ASCII
/// case, as Windows matches paths; a folder whose name only starts with the
/// user's (`C:\Users\yusuke` for `C:\Users\yu`) is left as it is.
fn hide_home<'a>(text: &'a [u8], home: &str) -> Cow<'a, [u8]> {
    if home.is_empty() {
        return Cow::Borrowed(text);
    }
    let escaped = home.replace('\\', r"\\");
    let mut text = Cow::Borrowed(text);
    for folder in [escaped.as_bytes(), home.as_bytes()] {
        if let Some(hidden) = replace_folder(&text, folder) {
            text = Cow::Owned(hidden);
        }
    }
    text
}

/// `text` with each `folder` in it as `HOME_HIDDEN`, if it has any.
fn replace_folder(text: &[u8], folder: &[u8]) -> Option<Vec<u8>> {
    // A folder's name goes on past `folder` if what follows could be part of it.
    let name_ends = |next: Option<&u8>| {
        next.is_none_or(|&b| !(b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b >= 0x80))
    };
    let mut hidden = Vec::new();
    let (mut copied, mut at) = (0, 0);
    while at + folder.len() <= text.len() {
        let end = at + folder.len();
        if text[at..end].eq_ignore_ascii_case(folder) && name_ends(text.get(end)) {
            hidden.extend_from_slice(&text[copied..at]);
            hidden.extend_from_slice(HOME_HIDDEN.as_bytes());
            (copied, at) = (end, end);
        } else {
            at += 1;
        }
    }
    (copied > 0).then(|| {
        hidden.extend_from_slice(&text[copied..]);
        hidden
    })
}

impl Write for LogFile {
    /// Writes all of `buf`, with the user's own folder hidden (`hide_home`):
    /// a line is never written in part.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let line = hide_home(buf, &self.home);
        let (file, len) = self.file()?;
        file.write_all(&line)?;
        *len += line.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.open {
            Some((file, _)) => file.flush(),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_holds_what_polyrec_reports_as_plain_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("polyrec.log");
        let subscriber = tracing_subscriber::registry()
            .with(file_layer(LogFile::new(path.clone(), MAX_LOG_BYTES)));
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(track = 2, "audio device went away");
        });
        let log = std::fs::read_to_string(&path).unwrap();
        assert!(
            log.contains("WARN") && log.contains("audio device went away track=2"),
            "{log}"
        );
        assert!(!log.contains('\x1b'), "colour codes: {log:?}");
    }

    #[test]
    fn writes_go_on_from_where_the_log_ends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("PolyRec").join("polyrec.log");
        LogFile::new(path.clone(), 100)
            .write_all(b"first\n")
            .unwrap();
        let mut log = LogFile::new(path.clone(), 100);
        log.write_all(b"second\n").unwrap();
        log.write_all(b"third\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "first\nsecond\nthird\n"
        );
    }

    /// Users paste the log into public bug reports: a path in their own
    /// folder, written plainly or as Rust prints it with `{:?}`, comes out
    /// as `%USERPROFILE%`, without their Windows user name.
    #[test]
    fn a_path_in_the_users_folder_is_logged_without_their_user_name() {
        let home = std::env::var("USERPROFILE").expect("USERPROFILE is set on Windows");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("polyrec.log");
        let subscriber = tracing_subscriber::registry()
            .with(file_layer(LogFile::new(path.clone(), MAX_LOG_BYTES)));
        let video = PathBuf::from(&home).join("Videos").join("a.mp4");
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("saved {} ({video:?})", video.display());
        });
        let log = std::fs::read_to_string(&path).unwrap();
        assert!(
            log.ends_with(
                "saved %USERPROFILE%\\Videos\\a.mp4 (\"%USERPROFILE%\\\\Videos\\\\a.mp4\")\n"
            ),
            "{log}"
        );
    }

    #[test]
    fn only_the_users_own_folder_is_hidden_not_one_its_name_starts() {
        let hidden = hide_home(
            br"C:\Users\yu\a C:\Users\yusuke\b c:\users\YU",
            r"C:\Users\yu",
        );
        assert_eq!(
            &*hidden,
            br"%USERPROFILE%\a C:\Users\yusuke\b %USERPROFILE%".as_slice()
        );
    }

    #[test]
    fn a_user_name_in_another_script_is_hidden_too() {
        let home = r"C:\Users\中山";
        let line = format!(r"{home}\Videos and {home}x");
        assert_eq!(
            &*hide_home(line.as_bytes(), home),
            r"%USERPROFILE%\Videos and C:\Users\中山x".as_bytes()
        );
    }

    /// Kept open, the log still starts over once it's past its limit.
    #[test]
    fn a_log_kept_open_starts_over_past_its_limit_keeping_the_last_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("polyrec.log");
        let mut log = LogFile::new(path.clone(), 10);
        log.write_all(b"0123456789").unwrap();
        log.write_all(b"a\n").unwrap();
        log.write_all(b"next\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "next\n");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("polyrec.log.old")).unwrap(),
            "0123456789a\n"
        );
    }

    /// Should the last log be held where it's kept (open in a viewer that
    /// doesn't share it), the log still can't grow past its limit: it starts
    /// over without keeping this one.
    #[test]
    fn a_log_that_cant_keep_the_last_one_still_starts_over() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("polyrec.log");
        let old = dir.path().join("polyrec.log.old");
        std::fs::write(&old, b"the one before").unwrap();
        let _held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&old)
            .unwrap();
        let mut log = LogFile::new(path.clone(), 10);
        log.write_all(b"0123456789").unwrap();
        log.write_all(b"a\n").unwrap();
        log.write_all(b"next\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "next\n");
    }
}
