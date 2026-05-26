pub mod clock;
pub mod state;

use crate::capture::audio::run_audio_capture;
use crate::capture::video::run_video_capture;
use crate::session::clock::RecordingClock;
use crate::types::{AudioDevice, AudioSamples, CaptureSource, TrackId, VideoFrame, SessionState};
use state::{transition, SessionAction};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const VIDEO_CHANNEL_CAPACITY: usize = 4;
const AUDIO_CHANNEL_CAPACITY: usize = 64;

pub struct ActiveCapture {
    pub video_rx: mpsc::Receiver<VideoFrame>,
    pub audio_tracks: Vec<(TrackId, mpsc::Receiver<AudioSamples>)>,
    video_handle: JoinHandle<()>,
    audio_handles: Vec<JoinHandle<()>>,
    pub clock: Arc<RecordingClock>,
}

pub struct SessionManager {
    state: SessionState,
    pub active: Option<ActiveCapture>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            state: SessionState::Idle,
            active: None,
        }
    }

    pub fn state(&self) -> &SessionState {
        &self.state
    }

    pub fn apply(&mut self, action: SessionAction) -> bool {
        match transition(&self.state, &action) {
            Some(next) => {
                self.state = next;
                true
            }
            None => false,
        }
    }

    pub fn start_capture(&mut self, source: CaptureSource, audio_devices: Vec<AudioDevice>) {
        let clock = RecordingClock::new();

        // Video capture — uses COM/WinRT types that are !Send.
        // Spawn on a dedicated OS thread with a single-threaded tokio runtime so
        // that the !Send future never crosses thread boundaries.
        let (video_tx, video_rx) = mpsc::channel::<VideoFrame>(VIDEO_CHANNEL_CAPACITY);
        let process_id = source.process_id;
        let video_clock = Arc::clone(&clock);
        let video_handle = tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("video capture runtime");
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                // Placeholder HWND: real lookup by PID is deferred to Plan 4.
                let hwnd =
                    windows::Win32::Foundation::HWND(process_id as *mut core::ffi::c_void);
                if let Err(e) = run_video_capture(hwnd, video_clock, video_tx).await {
                    tracing::error!("VideoCapture error: {e}");
                }
            });
        });

        // Audio capture — also holds !Send raw pointers across awaits.
        let mut audio_tracks = Vec::new();
        let mut audio_handles = Vec::new();
        for (i, dev) in audio_devices.into_iter().enumerate() {
            let track_id = TrackId::new(i as u32);
            let (audio_tx, audio_rx) = mpsc::channel::<AudioSamples>(AUDIO_CHANNEL_CAPACITY);
            let audio_clock = Arc::clone(&clock);
            let dev_id = dev.id.clone();
            let is_loopback = dev.is_loopback;
            let handle = tokio::task::spawn_blocking(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("audio capture runtime");
                let local = tokio::task::LocalSet::new();
                local.block_on(&rt, async move {
                    if let Err(e) =
                        run_audio_capture(dev_id, track_id, is_loopback, audio_clock, audio_tx)
                            .await
                    {
                        tracing::error!("AudioCapture[{track_id:?}] error: {e}");
                    }
                });
            });
            audio_tracks.push((track_id, audio_rx));
            audio_handles.push(handle);
        }

        self.active = Some(ActiveCapture {
            video_rx,
            audio_tracks,
            video_handle,
            audio_handles,
            clock,
        });
    }

    pub fn stop_capture(&mut self) {
        if let Some(active) = self.active.take() {
            active.video_handle.abort();
            for h in active.audio_handles {
                h.abort();
            }
        }
    }

    pub fn is_recording(&self) -> bool {
        matches!(self.state, SessionState::Recording)
    }

    pub fn is_idle(&self) -> bool {
        matches!(self.state, SessionState::Idle)
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_session_is_idle() {
        let sm = SessionManager::new();
        assert!(sm.is_idle());
    }

    #[test]
    fn start_transitions_to_recording() {
        let mut sm = SessionManager::new();
        assert!(sm.apply(SessionAction::Start));
        assert!(sm.is_recording());
    }

    #[test]
    fn illegal_action_returns_false() {
        let mut sm = SessionManager::new();
        assert!(!sm.apply(SessionAction::Stop));
        assert!(sm.is_idle());
    }
}
