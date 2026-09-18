//! Free-disk-space checks for the recording output location — used both before
//! starting a recording (refuse to start if there's clearly not enough room) and
//! periodically during one (stop gracefully instead of letting Media Foundation
//! fail mid-write and produce a corrupt/truncated file with no explanation).

use crate::error::AppError;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

/// Below this, refuse to start a new recording and stop an in-progress one.
/// 500 MB is a few seconds of buffer at typical recording bitrates (see
/// `encode::writer::video_bitrate_bps`) — enough to finalize cleanly rather
/// than fail mid-write.
pub const MIN_FREE_BYTES: u64 = 500 * 1024 * 1024;

/// Rate-limited low-disk-space guard shared by every write loop that needs
/// one (the manual-recording actor and the highlight-buffer actor) --
/// checking every frame would mean a syscall 30-60+ times a second for no
/// benefit; a few seconds of lag between "disk went low" and "stopped" is
/// fine given the goal is stopping before Media Foundation fails mid-write,
/// not stopping at the exact byte it would have failed.
pub struct DiskSpaceGuard {
    path: PathBuf,
    interval: Duration,
    last_check: Instant,
}

impl DiskSpaceGuard {
    pub fn new(path: PathBuf, interval: Duration) -> Self {
        Self { path, interval, last_check: Instant::now() }
    }

    /// A no-op unless `interval` has elapsed since the last real check.
    /// Returns `true` (and sets `disk_full_flag`) once free space on `path`
    /// drops below `MIN_FREE_BYTES` -- the caller should stop its write loop
    /// in that case. `context` (e.g. "recording", "highlight buffering") is
    /// folded into the log message so it's clear which loop stopped.
    pub fn should_stop(&mut self, disk_full_flag: &AtomicBool, context: &str) -> bool {
        if self.last_check.elapsed() < self.interval {
            return false;
        }
        self.last_check = Instant::now();
        match free_bytes(&self.path) {
            Ok(free) if free < MIN_FREE_BYTES => {
                tracing::warn!(
                    "disk space low ({} MB free on {}) -- stopping {context} early",
                    free / (1024 * 1024),
                    self.path.display()
                );
                disk_full_flag.store(true, Ordering::Relaxed);
                true
            }
            Ok(_) => false,
            Err(e) => {
                tracing::warn!("{context} disk space check failed, continuing: {e}");
                false
            }
        }
    }
}

/// Bytes free to the current user on the volume containing `path`. `path` must
/// already exist (a directory, typically) — pass the actual output directory,
/// not a not-yet-created file path within it.
pub fn free_bytes(path: &Path) -> Result<u64, AppError> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut free_bytes_available = 0u64;
    unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(wide.as_ptr()),
            Some(&mut free_bytes_available),
            None,
            None,
        )
        .map_err(|e| AppError::Windows(format!("GetDiskFreeSpaceExW({}): {e}", path.display())))?;
    }
    Ok(free_bytes_available)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_bytes_reports_a_positive_value_for_a_real_directory() {
        let dir = tempfile::tempdir().unwrap();
        let free = free_bytes(dir.path()).expect("free_bytes failed for a real, existing directory");
        assert!(free > 0, "expected some free space on the temp dir's volume");
    }

    #[test]
    fn free_bytes_errors_for_a_nonexistent_path() {
        let bogus = Path::new(r"Z:\this\path\does\not\exist\at\all");
        assert!(free_bytes(bogus).is_err());
    }
}
