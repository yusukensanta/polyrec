use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Config error: {0}")]
    Config(String),

    #[error("Capture error: {0}")]
    Capture(String),

    #[error("Encode error: {0}")]
    Encode(String),

    #[error("Windows API error: {0}")]
    Windows(String),

    #[error("Update error: {0}")]
    Update(String),

    // The MB figure is a plain string literal, not computed from
    // disk_space::MIN_FREE_BYTES -- thiserror's #[error(...)] can't mix a
    // field reference ({0}) with an extra formatted expression unambiguously.
    // Keep this in sync with MIN_FREE_BYTES if that constant ever changes.
    #[error("Not enough disk space on {0} (less than 500 MB free)")]
    DiskFull(std::path::PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display() {
        let e = AppError::Config("missing output_dir".into());
        assert_eq!(e.to_string(), "Config error: missing output_dir");
    }

    #[test]
    fn io_error_converts() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let app_err: AppError = io_err.into();
        assert!(app_err.to_string().contains("IO error"));
    }
}
