use super::*;
use crate::types::{AppAudioSource, AudioDevice};
use std::sync::atomic::Ordering;

/// A recording is set up with the frame rate `Config` clamps to (see
/// `EncodeConfig::fps`), not the raw one -- the dashboard's settings and
/// the default ones alike.
#[test]
fn encode_settings_take_the_clamped_frame_rate() {
    let config = EncodeConfig {
        fps: 0,
        ..crate::config::Config::default().encode
    };
    assert_eq!(EncodeSettings::from(&config).fps, 1);
    assert_eq!(
        EncodeSettings::default().fps,
        crate::config::Config::default().encode.fps()
    );
}

/// A manual recording whose capture loop and recorder never finish --
/// the shape of every recorder stall so far, like the still-window one
/// fixed in #204.
pub(super) fn stuck_recording() -> ActiveCapture {
    let (recording_tx, _recording_rx) = mpsc::channel(1);
    ActiveCapture {
        pipeline: CapturePipeline::stuck(),
        recorder_handle: tokio::spawn(std::future::pending()),
        recording_tx,
        output_path: PathBuf::from("stuck.tmp.mp4"),
        output_dir: PathBuf::new(),
        app_name: String::new(),
        naming: RecordingNaming::ProcessName,
        stopped_itself: StoppedItself::default(),
        hwnd: 0,
        kind: CaptureKind::Window,
        fps: LiveFps::new(60),
    }
}

/// Stop must hand back at once even when the recorder is stuck: it runs
/// on the dashboard's thread, which used to wait for the capture loops
/// and the recorder there -- so a stalled recorder froze the whole app
/// on Stop, and the save with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_returns_at_once_even_with_a_stuck_recorder() {
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    tokio::task::spawn_blocking(move || {
        let mut sm = SessionManager::new();
        sm.active = Some(stuck_recording());
        sm.state = SessionState::Recording;
        let handle = sm.stop_capture();
        let _ = done_tx.send((handle.is_some(), sm.state().clone()));
        // Kept alive so the test checks the stop itself, not cleanup.
        std::mem::forget(handle);
    });
    let (got_handle, state) = done_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("stop_capture blocked on the stuck recorder");
    assert!(got_handle);
    assert_eq!(state, SessionState::Idle);
}

/// Every device/app at unity gain with the default encode settings --
/// what the e2e tests below record with.
fn inputs(
    devices: Vec<AudioDevice>,
    app_sources: Vec<AppAudioSource>,
    app_audio_only: bool,
) -> CaptureInputs {
    CaptureInputs {
        devices: devices.into_iter().map(|d| (d, 1.0)).collect(),
        app_sources: app_sources.into_iter().map(|s| (s, 1.0)).collect(),
        app_audio_only,
        hide_cursor: false,
        encode: EncodeSettings::default(),
    }
}

/// `enumerate_sources()` can return transient shell popups (e.g. volume/brightness
/// OSD "PopupHost" windows) with a degenerate client rect that breaks H264 setup —
/// a separate, pre-existing issue unrelated to whatever this test is actually
/// exercising. Skip past those so tests aren't flaky based on unrelated desktop state.
fn pick_source_with_real_client_rect(sources: Vec<CaptureSource>) -> Option<CaptureSource> {
    sources.into_iter().find(|s| {
        // Monitor entries store an HMONITOR in `hwnd`, not a real window
        // handle -- GetClientRect would just fail on one anyway, but
        // filtering explicitly is clearer than relying on that.
        s.kind == CaptureKind::Window
            && unsafe {
                let hwnd = windows::Win32::Foundation::HWND(s.hwnd as *mut core::ffi::c_void);
                let mut rect = windows::Win32::Foundation::RECT::default();
                windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rect).is_ok()
                    && (rect.right - rect.left) >= 100
                    && (rect.bottom - rect.top) >= 100
            }
    })
}

/// What Highlight buffers in the app: the window in front -- a game
/// being played, say -- if it's big enough to encode; otherwise the
/// first window that is (see `pick_source_with_real_client_rect`).
fn highlight_source() -> CaptureSource {
    let front = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
    let in_front = (!front.0.is_null())
        .then(|| crate::sources::capture_source_for_hwnd(front))
        .and_then(|source| pick_source_with_real_client_rect(vec![source]));
    let source = in_front
        .or_else(|| pick_source_with_real_client_rect(crate::sources::enumerate_sources()))
        .expect("no usable capture source found");
    println!("buffering {} ({})", source.exe_name, source.window_title);
    source
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

/// A second Start while a recording runs -- a double-tapped hotkey, say
/// -- must leave it be: replaced, the first would go on capturing with
/// nothing left to stop it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_start_while_recording_leaves_the_recording_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut sm = SessionManager::recording_stuck();
    let second = CaptureSource {
        kind: CaptureKind::Window,
        process_id: 0,
        window_title: "second".into(),
        exe_name: "second.exe".into(),
        hwnd: 0,
        icon_rgba: None,
    };
    let started = sm.start_capture(
        second,
        inputs(vec![], vec![], false),
        false,
        Arc::new(AtomicU64::new(0)),
        dir.path(),
        RecordingNaming::ProcessName,
    );
    assert!(started.is_err(), "{started:?}");
    assert!(sm.is_recording());
    assert_eq!(
        sm.active().map(|a| a.output_path.clone()),
        Some(PathBuf::from("stuck.tmp.mp4"))
    );
}

#[test]
fn stop_capture_without_a_recording_stays_idle_and_returns_none() {
    let mut sm = SessionManager::new();
    assert!(sm.stop_capture().is_none());
    assert!(matches!(sm.state(), SessionState::Idle));
    assert!(sm.active().is_none());
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
    assert!(
        polyrec_dir.is_dir(),
        "polyrec subdirectory should be created"
    );
    assert_eq!(temp_path.parent(), Some(polyrec_dir.as_path()));
    assert_eq!(temp_path.extension().and_then(|e| e.to_str()), Some("mp4"));
    assert!(
        temp_path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("vivaldi_")
    );
}

#[test]
fn encode_settings_default_matches_config_default() {
    let settings = EncodeSettings::default();
    let config_default = crate::config::Config::default().encode;
    assert_eq!(settings.fps, config_default.fps);
    assert_eq!(settings.codec, config_default.codec);
    assert!(matches!(
        settings.resolution_mode,
        crate::config::ResolutionMode::Native
    ));
    assert!(matches!(
        settings.bitrate_mode,
        crate::config::BitrateMode::Auto
    ));
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
    use windows::Win32::Media::MediaFoundation::{MF_MT_FRAME_SIZE, MFCreateSourceReaderFromURL};

    let sources = enumerate_sources();
    let source =
        pick_source_with_real_client_rect(sources).expect("no usable capture source found");
    let hwnd = windows::Win32::Foundation::HWND(source.hwnd as *mut core::ffi::c_void);
    let (expected_w, expected_h) =
        query_capture_size(crate::capture::video::CaptureTarget::Window(hwnd))
            .expect("query_capture_size failed");

    let audio_devices = enumerate_audio_devices().unwrap_or_default();
    let dir = tempfile::tempdir().unwrap();
    let mut sm = SessionManager::new();
    let frame_count = Arc::new(AtomicU64::new(0));
    sm.start_capture(
        source,
        inputs(audio_devices, vec![], false),
        true,
        Arc::clone(&frame_count),
        dir.path(),
        RecordingNaming::ProcessName,
    )
    .expect("start_capture failed");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let handle =
        tokio::task::block_in_place(|| sm.stop_capture()).expect("stop_capture returned None");
    let finalized_path = handle
        .await
        .expect("recorder task panicked")
        .expect("finalize failed");

    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_MULTITHREADED,
        );
        let _ = windows::Win32::Media::MediaFoundation::MFStartup(
            windows::Win32::Media::MediaFoundation::MF_VERSION,
            windows::Win32::Media::MediaFoundation::MFSTARTUP_FULL,
        );
        use windows::Win32::Media::MediaFoundation::{MF_MT_MAJOR_TYPE, MFMediaType_Video};
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
        // What a player shows: the display aperture, when the encoded
        // size isn't it -- HEVC rounds a 1392-row picture up to 1408
        // coded rows and says so there.
        let mut aperture = [0u8; 16];
        let (actual_w, actual_h) = match video_type.GetBlob(
            &windows::Win32::Media::MediaFoundation::MF_MT_MINIMUM_DISPLAY_APERTURE,
            &mut aperture,
            None,
        ) {
            Ok(()) => (
                i32::from_le_bytes(aperture[8..12].try_into().unwrap()) as u32,
                i32::from_le_bytes(aperture[12..16].try_into().unwrap()) as u32,
            ),
            Err(_) => {
                let packed = video_type
                    .GetUINT64(&MF_MT_FRAME_SIZE)
                    .expect("GetUINT64 frame_size");
                ((packed >> 32) as u32, (packed & 0xFFFF_FFFF) as u32)
            }
        };
        println!("window: {expected_w}x{expected_h}, encoded: {actual_w}x{actual_h}");
        assert_eq!(actual_w, expected_w.max(2) & !1);
        assert_eq!(actual_h, expected_h.max(2) & !1);
    }
}

/// End-to-end regression for the stall a quiet loopback track caused:
/// with nothing playing, desktop loopback delivers no packets. The MP4
/// sink then held every other stream's data in memory (the file stayed
/// at its 79-byte header), throttled each write to about a second, and
/// stopping took minutes. Records a display with every audio device
/// (run it with nothing playing to reproduce the original case) and
/// checks the file grows while recording and stopping finishes promptly.
/// Records real devices but plays nothing; ignored by default.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn quiet_loopback_track_does_not_stall_the_recording() {
    use crate::capture::audio::enumerate_audio_devices;
    use crate::sources::enumerate_sources;

    let source = enumerate_sources()
        .into_iter()
        .find(|s| s.kind == CaptureKind::Monitor)
        .expect("no display source");
    let devices = enumerate_audio_devices().unwrap_or_default();
    assert!(
        devices.iter().any(|d| d.is_loopback),
        "needs a loopback device"
    );
    let dir = tempfile::tempdir().unwrap();
    let mut sm = SessionManager::new();
    let temp_path = sm
        .start_capture(
            source,
            inputs(devices, vec![], false),
            false,
            Arc::new(AtomicU64::new(0)),
            dir.path(),
            RecordingNaming::ProcessName,
        )
        .expect("start_capture failed");

    tokio::time::sleep(Duration::from_secs(6)).await;
    let size_while_recording = std::fs::metadata(&temp_path).map_or(0, |m| m.len());
    let handle = tokio::task::block_in_place(|| sm.stop_capture()).expect("stop_capture");
    let stop_started = std::time::Instant::now();
    let finalized = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("finalizing took over 5 s -- the recording stalled")
        .expect("recorder task panicked")
        .expect("finalize failed");
    println!(
        "while recording: {size_while_recording} bytes; stopped in {:?}; final {} bytes",
        stop_started.elapsed(),
        std::fs::metadata(&finalized).unwrap().len()
    );
    assert!(
        size_while_recording > 1024,
        "nothing reached the file while recording ({size_while_recording} bytes)"
    );
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
    let source =
        pick_source_with_real_client_rect(sources).expect("no usable capture source found");
    let audio_devices = enumerate_audio_devices().unwrap_or_default();

    let dir = tempfile::tempdir().unwrap();
    let mut sm = SessionManager::new();
    let frame_count = Arc::new(AtomicU64::new(0));
    let temp_path = sm
        .start_capture(
            source,
            inputs(audio_devices, vec![], false),
            true,
            Arc::clone(&frame_count),
            dir.path(),
            RecordingNaming::ProcessName,
        )
        .expect("start_capture failed");
    assert!(sm.is_recording(), "start_capture should move to Recording");

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let handle = tokio::task::block_in_place(|| sm.stop_capture())
        .expect("stop_capture returned None while active");
    assert!(matches!(sm.state(), SessionState::Idle));
    let result = handle.await.expect("recorder task panicked/aborted");
    let finalized_path = result.expect("finalize() returned an error");

    // The temp path start_capture returned gets renamed to its finish-timestamped
    // final name inside polyrec/ once the recording completes — they're not equal.
    assert_ne!(finalized_path, temp_path);
    assert!(
        !temp_path.exists(),
        "temp recording file should have been renamed away"
    );
    assert_eq!(
        finalized_path.parent(),
        Some(dir.path().join("polyrec")).as_deref()
    );
    let metadata = std::fs::metadata(&finalized_path).expect("output file missing");
    assert!(
        metadata.len() > 0,
        "output file is empty: {}",
        finalized_path.display()
    );
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

/// End-to-end: two real recordings in a row with `RecordingNaming::CustomPrefix`
/// through the same start_capture/stop_capture path the GUI uses, proving the
/// sequence number is resolved from what's actually on disk (not an in-memory
/// counter) across separate recordings -- needs a display, so it's ignored by
/// default -- run with `--ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn full_capture_with_custom_prefix_produces_sequential_files() {
    use crate::sources::enumerate_sources;

    let sources = enumerate_sources();
    let source =
        pick_source_with_real_client_rect(sources).expect("no usable capture source found");

    let dir = tempfile::tempdir().unwrap();
    let mut sm = SessionManager::new();
    let frame_count = Arc::new(AtomicU64::new(0));
    let naming = RecordingNaming::CustomPrefix("e2e_stream".into());

    let temp_path = sm
        .start_capture(
            source.clone(),
            inputs(vec![], vec![], false),
            true,
            Arc::clone(&frame_count),
            dir.path(),
            naming.clone(),
        )
        .expect("start_capture failed (first recording)");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let handle = tokio::task::block_in_place(|| sm.stop_capture())
        .expect("stop_capture returned None while active");
    let first_path = handle
        .await
        .expect("recorder task panicked/aborted")
        .expect("finalize() returned an error");
    assert_ne!(first_path, temp_path);
    assert_eq!(
        first_path.file_name().and_then(|n| n.to_str()),
        Some("e2e_stream_001.mp4"),
        "first custom-prefix recording should start the sequence at 001"
    );
    assert!(
        std::fs::metadata(&first_path)
            .expect("first output missing")
            .len()
            > 0
    );

    let frame_count2 = Arc::new(AtomicU64::new(0));
    let temp_path2 = sm
        .start_capture(
            source,
            inputs(vec![], vec![], false),
            true,
            Arc::clone(&frame_count2),
            dir.path(),
            naming,
        )
        .expect("start_capture failed (second recording)");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let handle2 = tokio::task::block_in_place(|| sm.stop_capture())
        .expect("stop_capture returned None while active");
    let second_path = handle2
        .await
        .expect("recorder task panicked/aborted")
        .expect("finalize() returned an error");
    assert_ne!(second_path, temp_path2);
    assert_eq!(
        second_path.file_name().and_then(|n| n.to_str()),
        Some("e2e_stream_002.mp4"),
        "second custom-prefix recording in the same dir should continue the sequence at 002, scanned from disk"
    );
    assert!(
        std::fs::metadata(&second_path)
            .expect("second output missing")
            .len()
            > 0
    );
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
    let mut source =
        pick_source_with_real_client_rect(sources).expect("no usable capture source found");
    source.process_id = std::process::id();
    let audio_devices = enumerate_audio_devices().unwrap_or_default();
    assert!(
        audio_devices.iter().any(|d| d.is_loopback),
        "need a loopback device to exercise app_audio_only"
    );

    let dir = tempfile::tempdir().unwrap();
    let mut sm = SessionManager::new();
    let frame_count = Arc::new(AtomicU64::new(0));
    let temp_path = sm
        .start_capture(
            source,
            inputs(audio_devices, vec![], true),
            true,
            Arc::clone(&frame_count),
            dir.path(),
            RecordingNaming::ProcessName,
        )
        .expect("start_capture failed");

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let handle = tokio::task::block_in_place(|| sm.stop_capture())
        .expect("stop_capture returned None while active");
    let result = handle.await.expect("recorder task panicked/aborted");
    let finalized_path = result.expect("finalize() returned an error");

    assert_ne!(finalized_path, temp_path);
    assert_eq!(
        finalized_path.parent(),
        Some(dir.path().join("polyrec")).as_deref()
    );
    let metadata = std::fs::metadata(&finalized_path).expect("output file missing");
    assert!(
        metadata.len() > 0,
        "output file is empty: {}",
        finalized_path.display()
    );
    println!("output size: {} bytes", metadata.len());
}

/// Stopping right after starting doesn't hang the save ("Saving
/// recording..." forever, #68/#69): the real devices plus an app source
/// of two processes, neither making a sound -- this test and a sleeping
/// child, like a multi-process app with one of its processes quiet --
/// stopped well inside process loopback's ~5 s activation, so the app
/// tracks likely never get a first buffer. Plays nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn stopping_right_after_start_with_quiet_app_tracks_does_not_hang() {
    use crate::capture::audio::enumerate_audio_devices;
    use crate::sources::enumerate_sources;
    use crate::types::AppAudioSource;

    let sources = enumerate_sources();
    let source =
        pick_source_with_real_client_rect(sources).expect("no usable capture source found");
    let audio_devices = enumerate_audio_devices().unwrap_or_default();
    println!("real audio devices: {}", audio_devices.len());

    let mut sleeper = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 60"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("failed to start a quiet child process");
    let app_audio_sources = vec![AppAudioSource {
        process_ids: vec![std::process::id(), sleeper.id()],
        exe_name: "quiet.exe".to_string(),
        display_name: "quiet".to_string(),
        icon_rgba: None,
    }];

    let dir = tempfile::tempdir().unwrap();
    let mut sm = SessionManager::new();
    let frame_count = Arc::new(AtomicU64::new(0));
    sm.start_capture(
        source,
        inputs(audio_devices, app_audio_sources, false),
        true,
        Arc::clone(&frame_count),
        dir.path(),
        RecordingNaming::ProcessName,
    )
    .expect("start_capture failed");

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let stop_started = std::time::Instant::now();
    let handle = tokio::task::block_in_place(|| sm.stop_capture())
        .expect("stop_capture returned None while active");
    let result = tokio::time::timeout(std::time::Duration::from_secs(30), handle).await;
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    let finalized_path = result
        .unwrap_or_else(|_| {
            panic!(
                "stop+finalize did not complete within 30s (elapsed before timeout: {:.1}s)",
                stop_started.elapsed().as_secs_f32()
            )
        })
        .expect("recorder task panicked/aborted")
        .expect("finalize() returned an error");

    println!(
        "stop+finalize took {:.2}s",
        stop_started.elapsed().as_secs_f32()
    );
    let metadata = std::fs::metadata(&finalized_path).expect("output file missing");
    assert!(metadata.len() > 0, "output file is empty");
}

/// End-to-end: real window + real audio devices through the actual
/// `start_highlight_buffering`/`save_highlight` wiring the GUI uses (not
/// just the isolated `highlight`/`encode::highlight_export` unit tests).
/// Runs long enough (past the minimum 30s buffer, well past several
/// internal 10s segment rotations) to exercise real segment rotation,
/// then verifies Save Highlight produces a real, playable, non-empty
/// file and that stopping buffering cleans up the segment directory.
/// Needs a display and audio endpoints, so it's ignored by default --
/// run with `--ignored --nocapture` (takes ~35s real time).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn highlight_buffering_saves_a_playable_file() {
    highlight_saves_a_playable_file(false).await;
}

/// `highlight_buffering_saves_a_playable_file` with the frames kept on
/// the GPU (`EncodeConfig::gpu_frames`): every rotated segment's writer
/// takes them as textures. Ignored by default for the same reasons.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn highlight_buffering_with_gpu_frames_saves_a_playable_file() {
    highlight_saves_a_playable_file(true).await;
}

async fn highlight_saves_a_playable_file(gpu_frames: bool) {
    use crate::capture::audio::enumerate_audio_devices;

    let source = highlight_source();
    let audio_devices = enumerate_audio_devices().unwrap_or_default();

    let dir = tempfile::tempdir().unwrap();
    let mut sm = SessionManager::new();
    let mut inputs = inputs(audio_devices, vec![], false);
    inputs.encode.gpu_frames = gpu_frames;
    sm.start_highlight_buffering(&source, inputs, dir.path(), 30)
        .expect("start_highlight_buffering failed");
    assert!(sm.is_highlighting());
    assert_eq!(sm.highlight_hwnd(), Some(source.hwnd));

    // Past the 30s minimum buffer and 3+ internal 10s segment rotations.
    tokio::time::sleep(std::time::Duration::from_secs(35)).await;

    let segment_dir = dir.path().join("polyrec").join("_highlight_buffer");
    let segments_before_save = std::fs::read_dir(&segment_dir)
        .expect("segment directory should exist by now")
        .count();
    assert!(
        segments_before_save > 0,
        "expected at least one rotated segment file on disk"
    );

    let handle = sm
        .save_highlight(
            dir.path(),
            "e2e_highlight_test",
            RecordingNaming::ProcessName,
        )
        .expect("save_highlight failed");
    let saved_path = handle
        .await
        .expect("save_highlight task panicked")
        .expect("concat_and_trim failed");

    let metadata = std::fs::metadata(&saved_path).expect("saved highlight file missing");
    assert!(
        metadata.len() > 0,
        "saved highlight file is empty: {}",
        saved_path.display()
    );
    println!(
        "highlight saved: {} ({} bytes)",
        saved_path.display(),
        metadata.len()
    );

    // Confirms the file is a real, readable container (not just non-empty
    // bytes) -- same verification remux.rs's own tests rely on.
    let audio_tracks = crate::encode::remux::count_audio_tracks(&saved_path)
        .expect("saved highlight file isn't a readable MP4");
    println!("highlight audio tracks: {audio_tracks}");

    tokio::task::block_in_place(|| sm.stop_highlight_buffering(true))
        .expect("was buffering")
        .await
        .expect("stopping panicked");
    assert!(!sm.is_highlighting());
    assert!(
        !segment_dir.exists() || std::fs::read_dir(&segment_dir).unwrap().count() == 0,
        "segment files should be gone after stop_highlight_buffering(discard: true)"
    );
}

/// End to end through the real wiring: two saves in a row each hold the
/// whole 30 s buffer. Kept by count, a save's early-finalized short
/// segment pushed a full one out -- 35 s in, a save held 25 s, and one
/// 3 s after it 18 s. And no two frames of a save share a time, as each
/// segment's first used to overlap the last before it. Records speaker
/// loopback only, no microphone, and plays nothing. Ignored like the
/// others (~45 s).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn back_to_back_highlight_saves_each_hold_the_whole_buffer() {
    use crate::capture::audio::enumerate_audio_devices;

    let source = highlight_source();
    let speakers: Vec<_> = enumerate_audio_devices()
        .unwrap_or_default()
        .into_iter()
        .filter(|d| d.is_loopback)
        .take(1)
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let mut sm = SessionManager::new();
    sm.start_highlight_buffering(&source, inputs(speakers, vec![], false), dir.path(), 30)
        .expect("start_highlight_buffering failed");
    tokio::time::sleep(Duration::from_secs(35)).await;

    for wait_before in [0, 3] {
        tokio::time::sleep(Duration::from_secs(wait_before)).await;
        let saved = sm
            .save_highlight(dir.path(), "e2e_back_to_back", RecordingNaming::ProcessName)
            .expect("save_highlight failed")
            .await
            .expect("save_highlight task panicked")
            .expect("concat_and_trim failed");
        let mut frames = highlight_export::video_sample_times(&saved);
        frames.sort_unstable();
        let (first, _) = frames.first().copied().expect("no frames");
        let (last, last_duration) = frames.last().copied().unwrap();
        let span = Duration::from_nanos((last + last_duration - first) as u64 * 100);
        println!(
            "save {}: {} frames over {span:?}",
            saved.display(),
            frames.len()
        );
        assert!(span >= Duration::from_secs(29), "only {span:?} saved");
        let count = frames.len();
        frames.dedup_by_key(|(ts, _)| *ts);
        assert_eq!(frames.len(), count, "two frames at one time");
    }

    tokio::task::block_in_place(|| sm.stop_highlight_buffering(true))
        .expect("was buffering")
        .await
        .expect("stopping panicked");
}

/// Regression test for a real bug caught via a live manual run: pausing
/// Highlight buffering with `stop_highlight_buffering(false)` (e.g. so a
/// manual recording can start) then resuming with a fresh
/// `start_highlight_buffering` call used to leave the *previous*
/// session's segment files sitting in the segment directory forever --
/// untracked by the new session's empty deque, so never eligible for
/// eviction or cleanup. `start_highlight_buffering` now clears any
/// leftover files from a prior session as soon as it (re)starts.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn pausing_then_resuming_highlight_buffering_does_not_orphan_segment_files() {
    use crate::capture::audio::enumerate_audio_devices;

    let source = highlight_source();
    let audio_devices = enumerate_audio_devices().unwrap_or_default();

    let dir = tempfile::tempdir().unwrap();
    let segment_dir = dir.path().join("polyrec").join("_highlight_buffer");
    let mut sm = SessionManager::new();

    sm.start_highlight_buffering(
        &source,
        inputs(audio_devices.clone(), vec![], false),
        dir.path(),
        30,
    )
    .expect("first start_highlight_buffering failed");
    tokio::time::sleep(std::time::Duration::from_secs(12)).await;
    let files_before_pause = std::fs::read_dir(&segment_dir).unwrap().count();
    assert!(
        files_before_pause > 0,
        "expected at least one segment file before pausing"
    );

    // Pause (discard: false) -- as `refresh_highlight_buffering` does
    // right before a manual recording starts.
    tokio::task::block_in_place(|| sm.stop_highlight_buffering(false))
        .expect("was buffering")
        .await
        .expect("stopping panicked");
    assert!(!sm.is_highlighting());
    let files_immediately_after_pause = std::fs::read_dir(&segment_dir).unwrap().count();
    assert_eq!(
        files_immediately_after_pause, files_before_pause,
        "pausing (discard: false) must not delete anything by itself"
    );

    // Resume -- as `refresh_highlight_buffering` does right after a
    // manual recording stops.
    sm.start_highlight_buffering(
        &source,
        inputs(audio_devices, vec![], false),
        dir.path(),
        30,
    )
    .expect("resumed start_highlight_buffering failed");

    // The pre-pause files must be gone immediately on resume, not just
    // eventually -- they're never tracked by the new session's deque, so
    // nothing else will ever clean them up.
    let files_right_after_resume: Vec<_> = std::fs::read_dir(&segment_dir).unwrap().collect();
    assert!(
        files_right_after_resume.len() <= 1,
        "expected the pre-pause segments to be cleared on resume (at most the brand-new in-progress one), found {}",
        files_right_after_resume.len()
    );

    tokio::task::block_in_place(|| sm.stop_highlight_buffering(true))
        .expect("was buffering")
        .await
        .expect("stopping panicked");
}
