use std::fmt::Display;
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

    #[error(
        "Not enough disk space on {0} (less than {mb} MB free)",
        mb = crate::disk_space::MIN_FREE_BYTES / (1024 * 1024)
    )]
    DiskFull(std::path::PathBuf),
}

/// Converts any `Display` error into an `AppError` variant with a short
/// "what were we doing" prefix -- `.encode_ctx("MFCreateMediaType")?`
/// instead of `.map_err(|e| AppError::Encode(format!("MFCreateMediaType: {e}")))?`.
///
/// The context is only formatted on the error path, so passing
/// `format_args!("WriteSample audio[{idx}]")` keeps hot paths (per-frame and
/// per-buffer writes) allocation-free on success.
pub(crate) trait ResultExt<T> {
    fn encode_ctx(self, ctx: impl Display) -> Result<T, AppError>;
    fn capture_ctx(self, ctx: impl Display) -> Result<T, AppError>;
    fn windows_ctx(self, ctx: impl Display) -> Result<T, AppError>;
    fn update_ctx(self, ctx: impl Display) -> Result<T, AppError>;
}

impl<T, E: Display> ResultExt<T> for Result<T, E> {
    fn encode_ctx(self, ctx: impl Display) -> Result<T, AppError> {
        self.map_err(|e| AppError::Encode(format!("{ctx}: {e}")))
    }

    fn capture_ctx(self, ctx: impl Display) -> Result<T, AppError> {
        self.map_err(|e| AppError::Capture(format!("{ctx}: {e}")))
    }

    fn windows_ctx(self, ctx: impl Display) -> Result<T, AppError> {
        self.map_err(|e| AppError::Windows(format!("{ctx}: {e}")))
    }

    fn update_ctx(self, ctx: impl Display) -> Result<T, AppError> {
        self.map_err(|e| AppError::Update(format!("{ctx}: {e}")))
    }
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
    fn ctx_prefixes_the_message_and_picks_the_variant() {
        let failed: Result<(), &str> = Err("boom");
        let e = failed.encode_ctx("MFCreateMediaType").unwrap_err();
        assert!(matches!(&e, AppError::Encode(msg) if msg == "MFCreateMediaType: boom"));

        let idx = 3;
        let e = failed
            .capture_ctx(format_args!("Map staging[{idx}]"))
            .unwrap_err();
        assert!(matches!(&e, AppError::Capture(msg) if msg == "Map staging[3]: boom"));

        assert!(matches!(failed.windows_ctx("x"), Err(AppError::Windows(_))));
        assert!(matches!(failed.update_ctx("x"), Err(AppError::Update(_))));
    }

    #[test]
    fn ctx_passes_ok_values_through_untouched() {
        let ok: Result<u32, &str> = Ok(7);
        assert_eq!(ok.encode_ctx("unused").unwrap(), 7);
    }

    #[test]
    fn disk_full_message_reports_the_threshold_from_min_free_bytes() {
        let e = AppError::DiskFull(std::path::PathBuf::from(r"D:\recordings"));
        assert_eq!(
            e.to_string(),
            format!(
                r"Not enough disk space on D:\recordings (less than {} MB free)",
                crate::disk_space::MIN_FREE_BYTES / (1024 * 1024)
            )
        );
        assert!(e.to_string().contains("500 MB"));
    }

    #[test]
    fn io_error_converts() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let app_err: AppError = io_err.into();
        assert!(app_err.to_string().contains("IO error"));
    }
}
