pub mod audio;
pub mod device;
pub mod frame_pool;
pub mod frame_rate;
pub mod pacer;
pub mod video;

use crate::session::clock::RecordingClock;
use crate::types::TrackId;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// The signals every capture loop of one recording (or one Highlight
/// buffering session) shares: the pause-aware clock that stamps each frame's
/// and buffer's PTS, the pause flag (loops drop what they capture while it's
/// set), and the stop flag (loops exit on their next iteration once it's
/// set) -- and what the loops lost on the way (`LostSources`). Cloning
/// shares the same underlying signals rather than copying them.
#[derive(Clone)]
pub struct CaptureControl {
    pub clock: Arc<RecordingClock>,
    pause: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    lost: Arc<Mutex<LostSources>>,
}

/// What a recording lost before it was stopped: its video source (the window
/// closed, or its display was disconnected, or capturing it failed) and the
/// audio tracks whose device or app went away. Each stream goes on to the end
/// all the same -- the picture held, the audio silent -- since one that stops
/// getting samples stalls the whole file (see `pacer::STILL_FRAME_REPEAT`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LostSources {
    pub video: bool,
    pub audio: Vec<TrackId>,
}

impl CaptureControl {
    pub fn new() -> Self {
        Self {
            clock: RecordingClock::new(),
            pause: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(AtomicBool::new(false)),
            lost: Arc::new(Mutex::new(LostSources::default())),
        }
    }

    /// Notes that the video source went away -- see `LostSources`.
    pub fn video_source_lost(&self) {
        self.lost_sources_mut().video = true;
    }

    /// Notes that `track`'s device or app went away -- see `LostSources`.
    pub fn audio_source_lost(&self, track: TrackId) {
        let mut lost = self.lost_sources_mut();
        if !lost.audio.contains(&track) {
            lost.audio.push(track);
        }
    }

    pub fn lost_sources(&self) -> LostSources {
        self.lost_sources_mut().clone()
    }

    fn lost_sources_mut(&self) -> std::sync::MutexGuard<'_, LostSources> {
        self.lost.lock().unwrap_or_else(PoisonError::into_inner)
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

    #[test]
    fn what_a_loop_loses_is_seen_through_every_clone() {
        let ctrl = CaptureControl::new();
        assert_eq!(ctrl.lost_sources(), LostSources::default());
        let video_loop = ctrl.clone();
        let audio_pump = ctrl.clone();
        video_loop.video_source_lost();
        audio_pump.audio_source_lost(TrackId::new(1));
        audio_pump.audio_source_lost(TrackId::new(1));
        assert_eq!(
            ctrl.lost_sources(),
            LostSources {
                video: true,
                audio: vec![TrackId::new(1)],
            }
        );
    }
}
