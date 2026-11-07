//! PolyRec's log: what it reports as it runs -- a recording recovered, a
//! device gone, an encoder falling back -- written to `polyrec.log` next to
//! `config.toml`. The release build has no console, so before this all of
//! it was lost. `RUST_LOG` still picks what's logged.

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
}

impl LogFile {
    pub fn new(path: PathBuf, max_bytes: u64) -> Self {
        Self {
            path,
            max_bytes,
            open: None,
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
            // Should it fail (another PolyRec has the file open, say), the
            // log goes on as it is, and tries again a whole log later.
            if len > self.max_bytes {
                let _ = std::fs::rename(&self.path, self.path.with_extension("log.old"));
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

impl Write for LogFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let (file, len) = self.file()?;
        let written = file.write(buf)?;
        *len += written as u64;
        Ok(written)
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
}
