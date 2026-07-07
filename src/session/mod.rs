pub mod clock;
pub mod state;

use crate::capture::audio::{
    run_audio_capture, run_process_loopback_capture, TARGET_CHANNELS, TARGET_SAMPLE_RATE,
};
use crate::capture::video::run_video_capture;
use crate::encode::actor::{spawn_audio_pump, spawn_recording_actor, spawn_video_pump};
use crate::encode::RecordingCommand;
use crate::error::AppError;
use crate::session::clock::RecordingClock;
use crate::types::{AudioDevice, CaptureSource, SessionState, TrackId};
use state::{transition, SessionAction};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    pub pause_flag: Arc<AtomicBool>,
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
        app_audio_only: bool,
        frame_count: Arc<AtomicU64>,
        output_dir: &std::path::Path,
    ) -> PathBuf {
        let clock = RecordingClock::new();
        let pause_flag = Arc::new(AtomicBool::new(false));

        let output_path = make_output_path(output_dir);

        // All captured audio is downmixed/resampled to this fixed target in
        // run_audio_capture, regardless of each device's native mix format.
        let audio_specs: Vec<(u32, u16)> = audio_devices
            .iter()
            .map(|_| (TARGET_SAMPLE_RATE, TARGET_CHANNELS))
            .collect();

        // Resolve real window client dimensions via GetClientRect.
        let real_hwnd = windows::Win32::Foundation::HWND(
            source.hwnd as *mut core::ffi::c_void,
        );
        let (width, height) = unsafe {
            let mut rect = windows::Win32::Foundation::RECT::default();
            if windows::Win32::UI::WindowsAndMessaging::GetClientRect(real_hwnd, &mut rect).is_ok() {
                // H264 requires even width/height for 4:2:0 chroma subsampling;
                // round down so SetInputMediaType doesn't reject odd client rects.
                let w = ((rect.right - rect.left).max(2) as u32) & !1;
                let h = ((rect.bottom - rect.top).max(2) as u32) & !1;
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
        let video_pause = Arc::clone(&pause_flag);
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
                if let Err(e) = run_video_capture(hwnd, video_clock, video_pause, video_tx).await {
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
            let audio_pause = Arc::clone(&pause_flag);
            let dev_id = dev.id.clone();
            let is_loopback = dev.is_loopback;
            let use_process_loopback = is_loopback && app_audio_only;
            let target_pid = source.process_id;
            let capture_handle = tokio::task::spawn_blocking(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("audio capture runtime");
                let local = tokio::task::LocalSet::new();
                local.block_on(&rt, async move {
                    let result = if use_process_loopback {
                        run_process_loopback_capture(
                            target_pid, true, track_id, audio_clock, audio_pause, audio_tx,
                        )
                        .await
                    } else {
                        run_audio_capture(
                            dev_id, track_id, is_loopback, audio_clock, audio_pause, audio_tx,
                        )
                        .await
                    };
                    if let Err(e) = result {
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
            pause_flag,
            output_path: output_path.clone(),
        });

        output_path
    }

    /// Stops all capture and recording actors. Sends Stop to the recorder so it finalizes.
    /// Returns the recorder's `JoinHandle` so the caller can wait for finalization to complete
    /// before treating the output file as ready (see recorder-finalize-race design spec).
    pub fn stop_capture(&mut self) -> Option<JoinHandle<Result<PathBuf, AppError>>> {
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
            Some(active.recorder_handle)
        } else {
            None
        }
    }

    pub fn is_recording(&self) -> bool {
        matches!(self.state, SessionState::Recording)
    }

    pub fn pause_capture(&mut self) {
        if let Some(active) = &self.active {
            active.pause_flag.store(true, Ordering::SeqCst);
            active.clock.pause();
        }
        self.apply(SessionAction::Pause);
    }

    pub fn resume_capture(&mut self) {
        if let Some(active) = &self.active {
            active.pause_flag.store(false, Ordering::SeqCst);
            active.clock.resume();
        }
        self.apply(SessionAction::Resume);
    }

    pub fn is_paused(&self) -> bool {
        matches!(self.state, SessionState::Paused)
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

fn make_output_path(base_dir: &std::path::Path) -> PathBuf {
    let dir = base_dir.to_path_buf();
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

    /// `enumerate_sources()` can return transient shell popups (e.g. volume/brightness
    /// OSD "PopupHost" windows) with a degenerate client rect that breaks H264 setup —
    /// a separate, pre-existing issue unrelated to whatever this test is actually
    /// exercising. Skip past those so tests aren't flaky based on unrelated desktop state.
    fn pick_source_with_real_client_rect(sources: Vec<CaptureSource>) -> Option<CaptureSource> {
        sources.into_iter().find(|s| unsafe {
            let hwnd = windows::Win32::Foundation::HWND(s.hwnd as *mut core::ffi::c_void);
            let mut rect = windows::Win32::Foundation::RECT::default();
            windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rect).is_ok()
                && (rect.right - rect.left) >= 100
                && (rect.bottom - rect.top) >= 100
        })
    }

    #[test]
    fn new_session_is_idle() {
        let sm = SessionManager::new();
        assert!(matches!(sm.state(), SessionState::Idle));
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
        assert!(matches!(sm.state(), SessionState::Idle));
    }

    #[test]
    fn make_output_path_has_mp4_extension() {
        let p = make_output_path(std::path::Path::new("."));
        assert_eq!(p.extension().and_then(|e| e.to_str()), Some("mp4"));
    }

    /// End-to-end: real window capture + real audio devices, through the same
    /// start_capture/stop_capture path the GUI uses. Needs a display and audio
    /// endpoints, so it's ignored by default — run with `--ignored --nocapture`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn full_capture_produces_nonempty_file() {
        use crate::capture::audio::enumerate_audio_devices;
        use crate::sources::enumerate_sources;

        let sources = enumerate_sources();
        let source = pick_source_with_real_client_rect(sources).expect("no usable capture source found");
        let audio_devices = enumerate_audio_devices().unwrap_or_default();

        let dir = tempfile::tempdir().unwrap();
        let mut sm = SessionManager::new();
        let frame_count = Arc::new(AtomicU64::new(0));
        let output_path = sm.start_capture(source, audio_devices, false, Arc::clone(&frame_count), dir.path());

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let handle = tokio::task::block_in_place(|| sm.stop_capture())
            .expect("stop_capture returned None while active");
        let result = handle.await.expect("recorder task panicked/aborted");
        let finalized_path = result.expect("finalize() returned an error");

        assert_eq!(finalized_path, output_path);
        let metadata = std::fs::metadata(&finalized_path).expect("output file missing");
        assert!(metadata.len() > 0, "output file is empty: {}", finalized_path.display());
        println!("frames captured: {}", frame_count.load(Ordering::Relaxed));
        println!("output size: {} bytes", metadata.len());
    }

    /// Same as `full_capture_produces_nonempty_file` but with `app_audio_only = true`,
    /// exercising the process-loopback path through the real start_capture/stop_capture
    /// wiring (not just the raw capture function). Uses our own process id as the
    /// loopback target so a device is guaranteed to exist regardless of which visible
    /// window gets picked for video.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn full_capture_app_audio_only_produces_nonempty_file() {
        use crate::capture::audio::enumerate_audio_devices;
        use crate::sources::enumerate_sources;

        let sources = enumerate_sources();
        let mut source = pick_source_with_real_client_rect(sources).expect("no usable capture source found");
        source.process_id = std::process::id();
        let audio_devices = enumerate_audio_devices().unwrap_or_default();
        assert!(
            audio_devices.iter().any(|d| d.is_loopback),
            "need a loopback device to exercise app_audio_only"
        );

        let dir = tempfile::tempdir().unwrap();
        let mut sm = SessionManager::new();
        let frame_count = Arc::new(AtomicU64::new(0));
        let output_path = sm.start_capture(source, audio_devices, true, Arc::clone(&frame_count), dir.path());

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let handle = tokio::task::block_in_place(|| sm.stop_capture())
            .expect("stop_capture returned None while active");
        let result = handle.await.expect("recorder task panicked/aborted");
        let finalized_path = result.expect("finalize() returned an error");

        assert_eq!(finalized_path, output_path);
        let metadata = std::fs::metadata(&finalized_path).expect("output file missing");
        assert!(metadata.len() > 0, "output file is empty: {}", finalized_path.display());
        println!("output size: {} bytes", metadata.len());
    }
}
