//! RAII guard for Media Foundation's process-wide startup reference count.

use crate::error::AppError;
use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFShutdown, MFStartup};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

/// One `MFStartup` reference, released by `MFShutdown` on drop.
///
/// `MFStartup`/`MFShutdown` are reference-counted, so every successful start
/// needs exactly one matching shutdown. Calling them by hand left that
/// pairing to every exit path -- e.g. `RecordingWriter::new` used to start MF
/// and only shut it down in `finalize()`, so any `?` error in between (or a
/// writer dropped without being finalized) leaked a reference. Holding one of
/// these instead makes the shutdown happen on every path, early returns
/// included.
///
/// Declare it *before* any MF objects in the same scope (locals drop in
/// reverse order), or *after* them as a struct field (fields drop in
/// declaration order) -- either way MF objects are released before MF itself
/// shuts down.
#[must_use = "Media Foundation shuts down again as soon as this is dropped"]
pub(crate) struct MfSession(());

impl MfSession {
    /// `MFStartup` only, leaving the calling thread's COM apartment alone --
    /// for the main thread (whose apartment belongs to winit/OLE) and the
    /// recording writer's thread (which has never needed one of its own).
    pub(crate) fn start() -> Result<Self, AppError> {
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_FULL)
                .map_err(|e| AppError::Encode(format!("MFStartup: {e}")))?;
        }
        Ok(Self(()))
    }

    /// Best-effort `CoInitializeEx(MTA)` first, then `start()` -- for the
    /// one-shot offline jobs (remux/export/highlight save/track probe). The
    /// COM result is deliberately ignored: `S_FALSE` (already initialized)
    /// and `RPC_E_CHANGED_MODE` (thread is already an STA, e.g. the UI
    /// thread probing a finished recording's track count) both leave the
    /// thread in a state Media Foundation works from.
    pub(crate) fn start_mta() -> Result<Self, AppError> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        Self::start()
    }
}

impl Drop for MfSession {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_sessions_keep_media_foundation_usable_until_the_last_drop() {
        use windows::Win32::Media::MediaFoundation::MFCreateMediaType;

        let outer = MfSession::start_mta().expect("outer MFStartup failed");
        {
            let _inner = MfSession::start().expect("inner MFStartup failed");
        }
        // The inner drop released only its own reference -- MF must still
        // be started for the outer session.
        unsafe { MFCreateMediaType() }.expect("MF should still be started after inner drop");
        drop(outer);
    }
}
