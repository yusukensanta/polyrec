pub mod clock;
pub mod state;

use crate::capture::audio::run_audio_capture;
use crate::capture::video::run_video_capture;
use crate::encode::actor::{spawn_audio_pump, spawn_recording_actor, spawn_video_pump};
use crate::encode::RecordingCommand;
use crate::error::AppError;
use crate::session::clock::RecordingClock;
use crate::types::{AudioDevice, CaptureSource, SessionState, TrackId};
use state::{transition, SessionAction};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const VIDEO_CHANNEL_CAPACITY: usize = 4;
const AUDIO_CHANNEL_CAPACITY: usize = 64;
const RECORDING_FPS: u32 = 60;

pub struct ActiveCapture {
    capture_handles: Vec<JoinHandle<()>>,
    pump_handles: Vec<JoinHandle<()>>,
    pub recorder_handle: JoinHandle<Result<PathBuf, AppError>>,
    pub recording_tx: mpsc::Sender<RecordingCommand>,
    pub clock: Arc<RecordingClock>,
    pub output_path: PathBuf,
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

    /// Starts capture and recording actors.
    /// `frame_count` is incremented by the video pump for each frame forwarded to the encoder.
    pub fn start_capture(
        &mut self,
        source: CaptureSource,
        audio_devices: Vec<AudioDevice>,
        frame_count: Arc<AtomicU64>,
    ) -> PathBuf {
        let clock = RecordingClock::new();

        // Compute output path: ~/Videos/PolyRec/polyrec_<timestamp>.mp4
        let output_path = make_output_path();

        // Audio device specs for RecordingWriter (sample_rate, channels per track).
        // We use defaults here; the capture actor will start with WASAPI native format.
        // Plan 4 can wire the actual format back.
        let audio_specs: Vec<(u32, u16)> = audio_devices
            .iter()
            .map(|_| (48000u32, 2u16))
            .collect();

        // Resolve real window client dimensions via GetClientRect.
        let real_hwnd = windows::Win32::Foundation::HWND(
            source.hwnd as *mut core::ffi::c_void,
        );
        let (width, height) = unsafe {
            let mut rect = windows::Win32::Foundation::RECT::default();
            if windows::Win32::UI::WindowsAndMessaging::GetClientRect(real_hwnd, &mut rect).is_ok() {
                let w = (rect.right - rect.left).max(1) as u32;
                let h = (rect.bottom - rect.top).max(1) as u32;
                (w, h)
            } else {
                tracing::warn!("GetClientRect failed for hwnd {:x}; using 1920x1080", source.hwnd);
                (1920u32, 1080u32)
            }
        };

        // Spawn RecordingActor
        let (recording_tx, recorder_handle) = spawn_recording_actor(
            output_path.clone(),
            width,
            height,
            RECORDING_FPS,
            audio_specs,
        );

        // Spawn video capture + pump
        let (video_tx, video_rx) = mpsc::channel(VIDEO_CHANNEL_CAPACITY);
        let hwnd_val = source.hwnd;
        let video_clock = Arc::clone(&clock);
        let video_capture_handle = tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("video capture runtime");
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let hwnd = windows::Win32::Foundation::HWND(
                    hwnd_val as *mut core::ffi::c_void,
                );
                if let Err(e) = run_video_capture(hwnd, video_clock, video_tx).await {
                    tracing::error!("VideoCapture error: {e}");
                }
            });
        });
        let video_pump_handle =
            spawn_video_pump(video_rx, recording_tx.clone(), frame_count);

        // Spawn audio capture + pump (one per device)
        let mut capture_handles = vec![video_capture_handle];
        let mut pump_handles = vec![video_pump_handle];

        for (i, dev) in audio_devices.into_iter().enumerate() {
            let track_id = TrackId::new(i as u32);
            let (audio_tx, audio_rx) = mpsc::channel(AUDIO_CHANNEL_CAPACITY);
            let audio_clock = Arc::clone(&clock);
            let dev_id = dev.id.clone();
            let is_loopback = dev.is_loopback;
            let capture_handle = tokio::task::spawn_blocking(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("audio capture runtime");
                let local = tokio::task::LocalSet::new();
                local.block_on(&rt, async move {
                    if let Err(e) = run_audio_capture(
                        dev_id, track_id, is_loopback, audio_clock, audio_tx,
                    )
                    .await
                    {
                        tracing::error!("AudioCapture[{track_id:?}] error: {e}");
                    }
                });
            });
            let pump_handle = spawn_audio_pump(audio_rx, recording_tx.clone());
            capture_handles.push(capture_handle);
            pump_handles.push(pump_handle);
        }

        self.active = Some(ActiveCapture {
            capture_handles,
            pump_handles,
            recorder_handle,
            recording_tx,
            clock,
            output_path: output_path.clone(),
        });

        output_path
    }

    /// Stops all capture and recording actors. Sends Stop to the recorder so it finalizes.
    pub fn stop_capture(&mut self) {
        if let Some(active) = self.active.take() {
            // Abort capture sources first (stops new frame production)
            for h in active.capture_handles {
                h.abort();
            }
            // Abort pump tasks (stops forwarding)
            for h in active.pump_handles {
                h.abort();
            }
            // Deliver Stop to recorder; blocking_send is safe from non-async context
            let _ = active.recording_tx.blocking_send(RecordingCommand::Stop);
            // recorder_handle dropped here — spawn_blocking task continues running to completion
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

fn make_output_path() -> PathBuf {
    let base = dirs::video_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")));
    let dir = base.join("PolyRec");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("Failed to create output directory {}: {e}", dir.display());
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    dir.join(format!("polyrec_{ts}.mp4"))
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

    #[test]
    fn make_output_path_has_mp4_extension() {
        let p = make_output_path();
        assert_eq!(p.extension().and_then(|e| e.to_str()), Some("mp4"));
    }
}
