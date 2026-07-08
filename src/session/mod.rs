pub mod clock;
pub mod state;

use crate::capture::audio::{
    run_audio_capture, run_process_loopback_capture, TARGET_CHANNELS, TARGET_SAMPLE_RATE,
};
use crate::capture::video::{query_capture_size, query_display_size, run_video_capture};
use crate::config::{BitrateMode, ResolutionMode};
use crate::encode::actor::{spawn_audio_pump, spawn_recording_actor, spawn_video_pump};
use crate::encode::writer::video_bitrate_bps;
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

/// Resolved encoder settings for one recording — built by the caller (the dashboard,
/// from `Config::encode`) and passed into `start_capture`.
#[derive(Debug, Clone)]
pub struct EncodeSettings {
    pub codec: String,
    pub fps: u32,
    pub resolution_mode: ResolutionMode,
    pub bitrate_mode: BitrateMode,
}

impl Default for EncodeSettings {
    fn default() -> Self {
        let d = crate::config::Config::default().encode;
        let resolution_mode = d.resolution_mode();
        let bitrate_mode = d.bitrate_mode();
        Self {
            codec: d.codec,
            fps: d.fps,
            resolution_mode,
            bitrate_mode,
        }
    }
}

pub struct ActiveCapture {
    capture_handles: Vec<JoinHandle<()>>,
    pump_handles: Vec<JoinHandle<()>>,
    pub recorder_handle: JoinHandle<Result<PathBuf, AppError>>,
    pub recording_tx: mpsc::Sender<RecordingCommand>,
    pub clock: Arc<RecordingClock>,
    pub pause_flag: Arc<AtomicBool>,
    pub output_path: PathBuf,
    /// Set by the recorder actor if it stopped itself early because free disk
    /// space dropped below `disk_space::MIN_FREE_BYTES` — the file up to that
    /// point is still finalized normally, this just tells the caller *why* the
    /// recording ended without the user pressing stop, so it can be surfaced.
    pub disk_full_flag: Arc<AtomicBool>,
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
        encode: EncodeSettings,
    ) -> Result<PathBuf, AppError> {
        let clock = RecordingClock::new();
        let pause_flag = Arc::new(AtomicBool::new(false));

        let app_name = app_name_from_exe(&source.exe_name);
        let (output_path, polyrec_dir) = prepare_recording_paths(output_dir, &app_name);

        let free = crate::disk_space::free_bytes(&polyrec_dir)?;
        if free < crate::disk_space::MIN_FREE_BYTES {
            return Err(AppError::DiskFull(polyrec_dir));
        }

        // All captured audio is downmixed/resampled to this fixed target in
        // run_audio_capture, regardless of each device's native mix format.
        let audio_specs: Vec<(u32, u16)> = audio_devices
            .iter()
            .map(|_| (TARGET_SAMPLE_RATE, TARGET_CHANNELS))
            .collect();

        // Query the size Windows.Graphics.Capture will actually deliver frames at —
        // NOT GetClientRect, which excludes the title bar/borders and doesn't match
        // WGC's window capture size. Used to size the capture-side staging texture;
        // does NOT need to match the encoder (frames are scaled to output_width/height
        // below), only itself internally (frame pool vs. staging texture).
        let real_hwnd = windows::Win32::Foundation::HWND(
            source.hwnd as *mut core::ffi::c_void,
        );
        let (capture_width, capture_height) = match query_capture_size(real_hwnd) {
            Ok((w, h)) => (w.max(2) & !1, h.max(2) & !1),
            Err(e) => {
                tracing::warn!("query_capture_size failed for hwnd {:x}: {e}; using 1920x1080", source.hwnd);
                (1920u32, 1080u32)
            }
        };

        // Only query the display when the user explicitly asked for it — this is not
        // the default (see the resolution-regression fix). No wasted syscall otherwise.
        let display_size = if matches!(encode.resolution_mode, ResolutionMode::Display) {
            match query_display_size(real_hwnd) {
                Ok((w, h)) => Some((w.max(2) & !1, h.max(2) & !1)),
                Err(e) => {
                    tracing::warn!("query_display_size failed for hwnd {:x}: {e}; using capture size", source.hwnd);
                    None
                }
            }
        } else {
            None
        };
        let (output_width, output_height) =
            resolve_output_size(&encode.resolution_mode, (capture_width, capture_height), display_size);

        let bitrate_bps = match encode.bitrate_mode {
            BitrateMode::Auto => video_bitrate_bps(output_width, output_height, encode.fps),
            BitrateMode::Manual(bps) => bps,
        };

        // Spawn RecordingActor
        let disk_full_flag = Arc::new(AtomicBool::new(false));
        let (recording_tx, recorder_handle) = spawn_recording_actor(
            output_path.clone(),
            polyrec_dir,
            app_name,
            output_width,
            output_height,
            encode.fps,
            encode.codec.clone(),
            bitrate_bps,
            audio_specs,
            Arc::clone(&disk_full_flag),
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
                if let Err(e) = run_video_capture(hwnd, capture_width, capture_height, output_width, output_height, video_clock, video_pause, video_tx).await {
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
            disk_full_flag,
        });

        Ok(output_path)
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

/// Derives a filesystem-safe app name from a window's exe name (e.g. "vivaldi.exe"
/// -> "vivaldi"), used as the recording filename's prefix. Falls back to "recording"
/// if the exe name is empty or sanitizes away to nothing.
fn app_name_from_exe(exe_name: &str) -> String {
    let trimmed = if exe_name.len() >= 4 && exe_name[exe_name.len() - 4..].eq_ignore_ascii_case(".exe") {
        &exe_name[..exe_name.len() - 4]
    } else {
        exe_name
    };
    let sanitized: String = trimmed
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if sanitized.is_empty() {
        "recording".to_string()
    } else {
        sanitized
    }
}

/// Creates `<base_dir>/polyrec/` and returns (temp_recording_path, polyrec_dir).
/// The temp path is what the encoder actually writes to — its name doesn't matter
/// beyond being unique, since `spawn_recording_actor` renames it to
/// `<app_name>_<finish timestamp>.mp4` once the recording finishes.
fn prepare_recording_paths(base_dir: &std::path::Path, app_name: &str) -> (PathBuf, PathBuf) {
    let polyrec_dir = base_dir.join("polyrec");
    if let Err(e) = std::fs::create_dir_all(&polyrec_dir) {
        tracing::warn!("Failed to create output directory {}: {e}", polyrec_dir.display());
    }
    let start_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let temp_path = polyrec_dir.join(format!("{app_name}_recording_{start_ts}.tmp.mp4"));
    (temp_path, polyrec_dir)
}

/// Pure resolution-mode resolution — no I/O, so it's directly unit-testable without
/// touching real monitor/capture APIs. `display_size` is `None` when the caller didn't
/// query it (mode != Display) or the query failed.
fn resolve_output_size(
    mode: &ResolutionMode,
    capture_size: (u32, u32),
    display_size: Option<(u32, u32)>,
) -> (u32, u32) {
    match mode {
        ResolutionMode::Native => capture_size,
        ResolutionMode::Display => display_size.unwrap_or(capture_size),
        ResolutionMode::Custom(w, h) => (*w, *h),
    }
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
    fn app_name_from_exe_strips_exe_suffix() {
        assert_eq!(app_name_from_exe("vivaldi.exe"), "vivaldi");
        assert_eq!(app_name_from_exe("Notepad.EXE"), "Notepad");
    }

    #[test]
    fn app_name_from_exe_sanitizes_invalid_filename_chars() {
        assert_eq!(app_name_from_exe("weird name!.exe"), "weird_name_");
    }

    #[test]
    fn app_name_from_exe_empty_falls_back_to_recording() {
        assert_eq!(app_name_from_exe(""), "recording");
        assert_eq!(app_name_from_exe(".exe"), "recording");
    }

    #[test]
    fn prepare_recording_paths_creates_polyrec_subdir_with_temp_mp4() {
        let dir = tempfile::tempdir().unwrap();
        let (temp_path, polyrec_dir) = prepare_recording_paths(dir.path(), "vivaldi");
        assert_eq!(polyrec_dir, dir.path().join("polyrec"));
        assert!(polyrec_dir.is_dir(), "polyrec subdirectory should be created");
        assert_eq!(temp_path.parent(), Some(polyrec_dir.as_path()));
        assert_eq!(temp_path.extension().and_then(|e| e.to_str()), Some("mp4"));
        assert!(temp_path.file_name().unwrap().to_str().unwrap().starts_with("vivaldi_"));
    }

    #[test]
    fn resolve_output_size_native_uses_capture_size() {
        let size = resolve_output_size(&crate::config::ResolutionMode::Native, (1280, 720), Some((2560, 1440)));
        assert_eq!(size, (1280, 720));
    }

    #[test]
    fn resolve_output_size_display_uses_display_size_when_available() {
        let size = resolve_output_size(&crate::config::ResolutionMode::Display, (1280, 720), Some((2560, 1440)));
        assert_eq!(size, (2560, 1440));
    }

    #[test]
    fn resolve_output_size_display_falls_back_to_capture_size_when_query_failed() {
        let size = resolve_output_size(&crate::config::ResolutionMode::Display, (1280, 720), None);
        assert_eq!(size, (1280, 720));
    }

    #[test]
    fn resolve_output_size_custom_uses_explicit_values() {
        let size = resolve_output_size(&crate::config::ResolutionMode::Custom(1920, 1080), (1280, 720), Some((2560, 1440)));
        assert_eq!(size, (1920, 1080));
    }

    #[test]
    fn encode_settings_default_matches_config_default() {
        let settings = EncodeSettings::default();
        let config_default = crate::config::Config::default().encode;
        assert_eq!(settings.fps, config_default.fps);
        assert_eq!(settings.codec, config_default.codec);
        assert!(matches!(settings.resolution_mode, crate::config::ResolutionMode::Native));
        assert!(matches!(settings.bitrate_mode, crate::config::BitrateMode::Auto));
    }

    /// Verifies the recorded video's actual encoded resolution matches the captured
    /// window's own native size (not upscaled/downscaled to the display's resolution —
    /// that upscaling was reverted because nearest-neighbor scaling to a much larger,
    /// non-integer-ratio display size produced visible blocky/grainy artifacts).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn recording_resolution_matches_window() {
        use crate::capture::audio::enumerate_audio_devices;
        use crate::capture::video::query_capture_size;
        use crate::sources::enumerate_sources;
        use windows::Win32::Media::MediaFoundation::{MFCreateSourceReaderFromURL, MF_MT_FRAME_SIZE};

        let sources = enumerate_sources();
        let source = pick_source_with_real_client_rect(sources).expect("no usable capture source found");
        let hwnd = windows::Win32::Foundation::HWND(source.hwnd as *mut core::ffi::c_void);
        let (expected_w, expected_h) = query_capture_size(hwnd).expect("query_capture_size failed");

        let audio_devices = enumerate_audio_devices().unwrap_or_default();
        let dir = tempfile::tempdir().unwrap();
        let mut sm = SessionManager::new();
        let frame_count = Arc::new(AtomicU64::new(0));
        sm.start_capture(source, audio_devices, false, Arc::clone(&frame_count), dir.path(), EncodeSettings::default())
            .expect("start_capture failed");
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let handle = tokio::task::block_in_place(|| sm.stop_capture()).expect("stop_capture returned None");
        let finalized_path = handle.await.expect("recorder task panicked").expect("finalize failed");

        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
            let _ = windows::Win32::Media::MediaFoundation::MFStartup(
                windows::Win32::Media::MediaFoundation::MF_VERSION,
                windows::Win32::Media::MediaFoundation::MFSTARTUP_FULL,
            );
            use windows::Win32::Media::MediaFoundation::{MFMediaType_Video, MF_MT_MAJOR_TYPE};
            let url = windows::core::HSTRING::from(finalized_path.to_str().unwrap());
            let reader: windows::Win32::Media::MediaFoundation::IMFSourceReader =
                MFCreateSourceReaderFromURL(&url, None).expect("MFCreateSourceReaderFromURL");
            // Container stream order isn't guaranteed — find the video stream by type.
            let mut video_type = None;
            for i in 0..4u32 {
                if let Ok(t) = reader.GetNativeMediaType(i, 0)
                    && t.GetGUID(&MF_MT_MAJOR_TYPE).unwrap_or_default() == MFMediaType_Video
                {
                    video_type = Some(t);
                    break;
                }
            }
            let video_type = video_type.expect("no video stream found");
            let packed = video_type.GetUINT64(&MF_MT_FRAME_SIZE).expect("GetUINT64 frame_size");
            let actual_w = (packed >> 32) as u32;
            let actual_h = (packed & 0xFFFF_FFFF) as u32;
            println!("window: {expected_w}x{expected_h}, encoded: {actual_w}x{actual_h}");
            assert_eq!(actual_w, expected_w.max(2) & !1);
            assert_eq!(actual_h, expected_h.max(2) & !1);
        }
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
        let temp_path = sm.start_capture(source, audio_devices, false, Arc::clone(&frame_count), dir.path(), EncodeSettings::default())
            .expect("start_capture failed");

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let handle = tokio::task::block_in_place(|| sm.stop_capture())
            .expect("stop_capture returned None while active");
        let result = handle.await.expect("recorder task panicked/aborted");
        let finalized_path = result.expect("finalize() returned an error");

        // The temp path start_capture returned gets renamed to its finish-timestamped
        // final name inside polyrec/ once the recording completes — they're not equal.
        assert_ne!(finalized_path, temp_path);
        assert!(!temp_path.exists(), "temp recording file should have been renamed away");
        assert_eq!(finalized_path.parent(), Some(dir.path().join("polyrec")).as_deref());
        let metadata = std::fs::metadata(&finalized_path).expect("output file missing");
        assert!(metadata.len() > 0, "output file is empty: {}", finalized_path.display());
        println!("frames captured: {}", frame_count.load(Ordering::Relaxed));
        println!("output size: {} bytes", metadata.len());

        // Confirm the remux stream-type discovery fix: exporting with all audio
        // tracks selected must still succeed and produce a valid, non-empty file
        // regardless of physical stream order in the container.
        let export_out = dir.path().join("export_check.mp4");
        let export_result = crate::encode::remux::remux(&finalized_path, &export_out, &[0, 1])
            .expect("remux with both audio tracks failed");
        let export_meta = std::fs::metadata(&export_result).expect("remuxed export file missing");
        println!("export size: {} bytes", export_meta.len());
        assert!(export_meta.len() > 0, "exported file is empty");
    }

    /// DIAGNOSTIC (temporary): records real window + default audio devices while a
    /// system WAV plays, then decodes the finalized MP4's audio track back to PCM via
    /// Media Foundation and reports the peak sample magnitude actually stored in the
    /// file. Isolates whether silence is introduced downstream of audio.rs (which
    /// diag_default_loopback_captures_real_signal already proved captures real signal).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn diag_recorded_audio_track_has_signal() {
        use crate::capture::audio::enumerate_audio_devices;
        use crate::sources::enumerate_sources;
        use windows::Win32::Media::MediaFoundation::{
            MFAudioFormat_PCM, MFCreateMediaType, MFCreateSourceReaderFromURL, MFMediaType_Audio,
            MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SOURCE_READER_FIRST_AUDIO_STREAM,
        };

        let sources = enumerate_sources();
        let source = pick_source_with_real_client_rect(sources).expect("no usable capture source found");
        let audio_devices: Vec<_> = enumerate_audio_devices()
            .unwrap_or_default()
            .into_iter()
            .filter(|d| d.is_loopback)
            .collect();
        assert!(!audio_devices.is_empty(), "need a loopback audio device");

        let dir = tempfile::tempdir().unwrap();
        let mut sm = SessionManager::new();
        let frame_count = Arc::new(AtomicU64::new(0));
        sm.start_capture(source, audio_devices, false, Arc::clone(&frame_count), dir.path(), EncodeSettings::default())
            .expect("start_capture failed");

        // Deliberately fire-and-forget: PlaySync() blocks *inside* the spawned
        // process until the sound finishes, but we want it playing concurrently
        // with the recording below, not blocking this thread until done.
        #[allow(clippy::zombie_processes)]
        std::process::Command::new("powershell")
            .args(["-c", "(New-Object Media.SoundPlayer 'C:\\Windows\\Media\\Alarm01.wav').PlaySync()"])
            .spawn()
            .expect("failed to spawn powershell sound player");

        tokio::time::sleep(std::time::Duration::from_secs(4)).await;

        let handle = tokio::task::block_in_place(|| sm.stop_capture())
            .expect("stop_capture returned None while active");
        let result = handle.await.expect("recorder task panicked/aborted");
        let finalized_path = result.expect("finalize() returned an error");
        println!("output: {}", finalized_path.display());

        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
            let _ = windows::Win32::Media::MediaFoundation::MFStartup(
                windows::Win32::Media::MediaFoundation::MF_VERSION,
                windows::Win32::Media::MediaFoundation::MFSTARTUP_FULL,
            );
            let url = windows::core::HSTRING::from(finalized_path.to_str().unwrap());
            let reader = MFCreateSourceReaderFromURL(&url, None).expect("MFCreateSourceReaderFromURL");

            // Force decode to PCM so we can inspect raw samples.
            let pcm_type = MFCreateMediaType().unwrap();
            pcm_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).unwrap();
            pcm_type.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM).unwrap();
            reader
                .SetCurrentMediaType(MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32, None, &pcm_type)
                .expect("SetCurrentMediaType PCM failed — no audio stream in file?");

            let mut peak = 0i32;
            let mut total_bytes = 0usize;
            loop {
                let mut stream_index = 0u32;
                let mut flags = 0u32;
                let mut timestamp = 0i64;
                let mut sample = None;
                reader
                    .ReadSample(
                        MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32,
                        0,
                        Some(&mut stream_index),
                        Some(&mut flags),
                        Some(&mut timestamp),
                        Some(&mut sample),
                    )
                    .expect("ReadSample failed");

                const MF_SOURCE_READERF_ENDOFSTREAM: u32 = 0x2;
                if flags & MF_SOURCE_READERF_ENDOFSTREAM != 0 {
                    break;
                }
                let Some(sample) = sample else { continue };
                let buffer = sample.ConvertToContiguousBuffer().expect("ConvertToContiguousBuffer");
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut len = 0u32;
                buffer.Lock(&mut data, None, Some(&mut len)).expect("Lock");
                let bytes = std::slice::from_raw_parts(data, len as usize);
                for chunk in bytes.chunks_exact(2) {
                    let v = i16::from_le_bytes([chunk[0], chunk[1]]) as i32;
                    if v.abs() > peak {
                        peak = v.abs();
                    }
                }
                total_bytes += bytes.len();
                buffer.Unlock().expect("Unlock");
            }

            println!("DIAG: total_audio_bytes={total_bytes} peak_pcm16={peak}");
            assert!(total_bytes > 0, "audio track had zero bytes decoded");
            assert!(peak > 300, "peak PCM16 sample {peak} looks like silence in the recorded file");
        }
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
        let temp_path = sm.start_capture(source, audio_devices, true, Arc::clone(&frame_count), dir.path(), EncodeSettings::default())
            .expect("start_capture failed");

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let handle = tokio::task::block_in_place(|| sm.stop_capture())
            .expect("stop_capture returned None while active");
        let result = handle.await.expect("recorder task panicked/aborted");
        let finalized_path = result.expect("finalize() returned an error");

        assert_ne!(finalized_path, temp_path);
        assert_eq!(finalized_path.parent(), Some(dir.path().join("polyrec")).as_deref());
        let metadata = std::fs::metadata(&finalized_path).expect("output file missing");
        assert!(metadata.len() > 0, "output file is empty: {}", finalized_path.display());
        println!("output size: {} bytes", metadata.len());
    }
}
