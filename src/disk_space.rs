//! Free-disk-space checks for the recording output location — used both before
//! starting a recording (refuse to start if there's clearly not enough room) and
//! periodically during one (stop gracefully instead of letting Media Foundation
//! fail mid-write and produce a corrupt/truncated file with no explanation).

use crate::error::AppError;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

/// Below this, refuse to start a new recording and stop an in-progress one.
/// 500 MB is a few seconds of buffer at typical recording bitrates (see
/// `encode::writer::video_bitrate_bps`) — enough to finalize cleanly rather
/// than fail mid-write.
pub const MIN_FREE_BYTES: u64 = 500 * 1024 * 1024;

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
