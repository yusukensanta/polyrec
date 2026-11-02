//! COM, as the threads PolyRec captures, encodes and enumerates devices on
//! need it.

use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

/// Joins this thread to COM's multithreaded apartment, best-effort.
///
/// Whatever comes back is fine to carry on from: `S_FALSE` means it had
/// joined already, and `RPC_E_CHANGED_MODE` means it's already in a
/// single-threaded apartment -- one of tokio's reused pool threads, left in
/// one by whatever ran on it before, or the dashboard's own thread -- from
/// which Media Foundation, WASAPI and Windows Graphics Capture's
/// free-threaded frame pool all work too. Video capture used to treat the
/// latter as fatal, which on such a thread made a recording with no video.
///
/// Never left again: these threads stay in COM for as long as they run.
pub(crate) fn join_mta() {
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
}
