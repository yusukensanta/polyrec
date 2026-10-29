pub mod audio;
pub mod device;
pub mod frame_pool;
pub mod video;

use crate::session::clock::RecordingClock;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The signals every capture loop of one recording (or one Highlight
/// buffering session) shares: the pause-aware clock that stamps each frame's
/// and buffer's PTS, the pause flag (loops drop what they capture while it's
/// set), and the stop flag (loops exit on their next iteration once it's
/// set). Cloning shares the same underlying signals rather than copying them.
#[derive(Clone)]
pub struct CaptureControl {
    pub clock: Arc<RecordingClock>,
    pause: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl CaptureControl {
    pub fn new() -> Self {
        Self {
            clock: RecordingClock::new(),
            pause: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_paused(&self) -> bool {
        self.pause.load(Ordering::Relaxed)
    }

    pub fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Sets the pause flag and freezes the clock together -- they're never
    /// meaningful apart (a paused loop must not stamp PTS that keep running).
    pub fn pause(&self) {
        self.pause.store(true, Ordering::SeqCst);
        self.clock.pause();
    }

    pub fn resume(&self) {
        self.pause.store(false, Ordering::SeqCst);
        self.clock.resume();
    }

    /// Tells every capture loop sharing this control to exit on its next
    /// iteration. Doesn't wait for them -- see `SessionManager::stop_capture`.
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Default for CaptureControl {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_control_is_running() {
        let ctrl = CaptureControl::new();
        assert!(!ctrl.is_paused());
        assert!(!ctrl.is_stopped());
    }

    #[test]
    fn clones_share_pause_and_stop_signals() {
        let ctrl = CaptureControl::new();
        let loop_side = ctrl.clone();
        ctrl.pause();
        assert!(loop_side.is_paused());
        ctrl.resume();
        assert!(!loop_side.is_paused());
        ctrl.request_stop();
        assert!(loop_side.is_stopped());
    }
}
