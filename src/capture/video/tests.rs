use super::*;
use crate::capture::pacer::STILL_FRAME_REPEAT;

#[test]
fn ping_pong_indices_alternate_and_never_collide() {
    for frame_counter in 0..8u64 {
        let (write_idx, read_idx) = ping_pong_indices(frame_counter);
        assert_ne!(
            write_idx, read_idx,
            "must never write and read the same slot"
        );
        assert!(write_idx < 2 && read_idx < 2);
        // write_idx alternates 0,1,0,1,...
        assert_eq!(write_idx, (frame_counter % 2) as usize);
    }
}

#[test]
fn ping_pong_indices_reads_the_slot_written_last_iteration() {
    // Whatever slot iteration N writes to, iteration N+1 must read back --
    // that's the entire point of the ping-pong (read what was copied ~1
    // iteration ago, never the slot just written this iteration).
    for frame_counter in 0..8u64 {
        let (write_idx, _) = ping_pong_indices(frame_counter);
        let (_, next_read_idx) = ping_pong_indices(frame_counter + 1);
        assert_eq!(write_idx, next_read_idx);
    }
}

/// WGC is asked for frames half a slot apart: a hair early against the
/// display's vsync still gets through, and fewer frames come as the fps
/// steps down.
#[test]
fn wgc_is_asked_for_frames_half_a_slot_apart() {
    use std::time::Duration;
    assert_eq!(update_interval_for(60), Duration::from_secs(1) / 120);
    assert_eq!(update_interval_for(240), Duration::from_secs(1) / 480);
    assert!(update_interval_for(60) > update_interval_for(240));
}

#[test]
fn scale_bgra_identity_when_sizes_match() {
    let src = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
    let expected = src.clone();
    let out = scale_bgra(src.into(), 2, 1, 2, 1);
    assert_eq!(&*out, expected.as_slice());
}

#[test]
fn scale_bgra_identity_returns_the_same_pooled_buffer_without_copying() {
    let pool = FramePool::new();
    let src = pool.take(8);
    let src_ptr = src.as_ptr();
    let out = scale_bgra(src, 2, 1, 2, 1);
    assert_eq!(out.as_ptr(), src_ptr);
}

#[test]
fn scale_bgra_upscales_2x() {
    // 1x1 BGRA pixel -> 2x2, every output pixel should equal the source pixel
    let src = vec![10u8, 20, 30, 40];
    let out = scale_bgra(src.into(), 1, 1, 2, 2);
    assert_eq!(out.len(), 2 * 2 * 4);
    for chunk in out.chunks(4) {
        assert_eq!(chunk, &[10, 20, 30, 40]);
    }
}

#[test]
fn scale_bgra_downscales() {
    // 2x2 -> 1x1: nearest-neighbor picks one of the four source pixels
    let src: Vec<u8> = (0..16).collect();
    let expected = src.clone();
    let out = scale_bgra(src.into(), 2, 2, 1, 1);
    assert_eq!(out.len(), 4);
    assert!(expected.chunks(4).any(|px| px == &*out));
}

#[test]
fn copy_rows_drops_driver_row_padding() {
    // 2 rows of 3 pixels (12 bytes) each, mapped with a 16-byte pitch:
    // the 4 padding bytes per row must not reach the packed output.
    let (packed_row, row_pitch, height) = (12usize, 16usize, 2usize);
    let mut mapped = vec![0xEEu8; row_pitch * height];
    for row in 0..height {
        for i in 0..packed_row {
            mapped[row * row_pitch + i] = (row * 100 + i) as u8;
        }
    }
    let mut dst = vec![0u8; packed_row * height];
    unsafe { copy_rows(mapped.as_ptr(), row_pitch, packed_row, height, &mut dst) };
    let expected: Vec<u8> = (0..height)
        .flat_map(|row| (0..packed_row).map(move |i| (row * 100 + i) as u8))
        .collect();
    assert_eq!(dst, expected);
    assert!(!dst.contains(&0xEE));
}

#[test]
fn copy_rows_with_an_unpadded_pitch_is_a_straight_copy() {
    let mapped: Vec<u8> = (0..32).collect();
    let mut dst = vec![0u8; 32];
    unsafe { copy_rows(mapped.as_ptr(), 8, 8, 4, &mut dst) };
    assert_eq!(dst, mapped);
}

#[test]
fn query_display_size_returns_positive_dimensions() {
    use windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow;
    let hwnd = unsafe { GetDesktopWindow() };
    let (w, h) = query_display_size(hwnd).expect("query_display_size failed");
    assert!(
        w > 0 && h > 0,
        "expected positive display dimensions, got {w}x{h}"
    );
}

/// A small topmost window with a white bar sliding across it about once
/// a millisecond, so DWM -- and WGC -- produce a new frame on every display
/// refresh (or, from `spawn_still`, a bar that never moves). Closed on
/// drop.
struct SlidingBarWindow {
    hwnd: usize,
    stop: Arc<std::sync::atomic::AtomicBool>,
    /// The bar moves only once this is set.
    go: Arc<std::sync::atomic::AtomicBool>,
    /// Set once the bar has made all its moves.
    settled: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SlidingBarWindow {
    const W: i32 = 320;
    const H: i32 = 240;
    const BAR: i32 = 8;

    fn spawn() -> Self {
        Self::spawn_with(u32::MAX, true)
    }

    fn spawn_still() -> Self {
        Self::spawn_with(1, true)
    }

    /// The bar moves one pixel `steps` times, once `go` is called unless
    /// `going` already, and then stays put.
    fn spawn_with(steps: u32, going: bool) -> Self {
        use windows::Win32::Graphics::Gdi::{BLACKNESS, GetDC, PatBlt, ReleaseDC, WHITENESS};
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, DispatchMessageW, MSG, PM_REMOVE, PeekMessageW,
            WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
        };
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let go = Arc::new(std::sync::atomic::AtomicBool::new(going));
        let settled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (thread_stop, thread_go, thread_settled) =
            (Arc::clone(&stop), Arc::clone(&go), Arc::clone(&settled));
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || unsafe {
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                windows::core::w!("STATIC"),
                windows::core::w!("polyrec fps test"),
                WS_POPUP | WS_VISIBLE,
                0,
                0,
                Self::W,
                Self::H,
                None,
                None,
                None,
                None,
            )
            .expect("CreateWindowExW");
            tx.send(hwnd.0 as usize).unwrap();
            let mut msg = MSG::default();
            let (mut x, mut moved) = (0, 0);
            while !thread_stop.load(std::sync::atomic::Ordering::Relaxed) {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&msg);
                }
                if moved < steps && thread_go.load(std::sync::atomic::Ordering::Relaxed) {
                    let dc = GetDC(Some(hwnd));
                    let _ = PatBlt(dc, x, 0, Self::BAR, Self::H, BLACKNESS);
                    x = (x + 1) % (Self::W - Self::BAR);
                    let _ = PatBlt(dc, x, 0, Self::BAR, Self::H, WHITENESS);
                    ReleaseDC(Some(hwnd), dc);
                    moved += 1;
                    if moved == steps {
                        thread_settled.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let _ = DestroyWindow(hwnd);
        });
        let hwnd = rx.recv().expect("window thread died");
        Self {
            hwnd,
            stop,
            go,
            settled,
            thread: Some(thread),
        }
    }

    fn go(&self) {
        self.go.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    fn settled(&self) -> bool {
        self.settled.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Where the bar's left edge ends up after `steps` moves.
    fn bar_x_after(steps: u32) -> i32 {
        (steps % (Self::W - Self::BAR) as u32) as i32
    }
}

impl Drop for SlidingBarWindow {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Process CPU time (user + kernel) so far.
fn process_cpu_time() -> std::time::Duration {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
        .expect("GetProcessTimes");
    }
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    std::time::Duration::from_nanos((ticks(kernel) + ticks(user)) * 100)
}

/// Captures `target` (as `CaptureTarget::from_raw` takes it) for `secs` at
/// `fps` and returns (frames delivered, process CPU time spent meanwhile).
fn capture_for(
    (kind, handle): (CaptureKind, usize),
    fps: u32,
    secs: u64,
    gpu: Option<GpuFrames>,
) -> (u64, std::time::Duration) {
    let target = CaptureTarget::from_raw(kind, handle);
    let (w, h) = query_capture_size(target).expect("query_capture_size");
    let size = (w & !1, h & !1);
    let ctrl = CaptureControl::new();
    let (tx, mut rx) = mpsc::channel(256);
    let capture_ctrl = ctrl.clone();
    let expect_gpu = gpu.is_some();
    let cpu_before = process_cpu_time();
    let capture = std::thread::spawn(move || {
        let target = CaptureTarget::from_raw(kind, handle);
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_video_capture(
                target,
                size,
                size,
                LiveFps::new(fps),
                capture_ctrl,
                tx,
                false,
                false,
                gpu,
            ))
    });
    // Only frames in the expected form count: textures with `gpu`,
    // memory buffers without.
    let counter = std::thread::spawn(move || {
        let mut frames = 0u64;
        while let Some(frame) = rx.blocking_recv() {
            if matches!(frame.data, FrameData::Gpu(_)) == expect_gpu {
                frames += 1;
            }
        }
        frames
    });
    std::thread::sleep(std::time::Duration::from_secs(secs));
    ctrl.request_stop();
    capture.join().unwrap().expect("run_video_capture failed");
    let frames = counter.join().unwrap();
    (frames, process_cpu_time() - cpu_before)
}

/// A still window keeps the video flowing. Windows hands out a frame only
/// when the picture changes, so a still window gives one and then nothing
/// -- which used to leave the recording with no video at all (the one
/// frame waited for a next one to be read back), stalling the MP4 sink
/// and, on stop, the dashboard. Now the last frame is sent again every
/// `STILL_FRAME_REPEAT`. Shows a small window in the top-left corner for
/// ~3 s, so it's ignored by default.
#[test]
#[ignore]
fn still_window_keeps_sending_frames() {
    const SECS: u64 = 3;
    let window = SlidingBarWindow::spawn_still();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let (frames, _) = capture_for((CaptureKind::Window, window.hwnd), 30, SECS, None);
    let expected =
        (std::time::Duration::from_secs(SECS).as_millis() / STILL_FRAME_REPEAT.as_millis()) as u64;
    println!("still window: {frames} frames in {SECS} s (repeats due: {expected})");
    assert!(
        frames >= expected - 2,
        "{frames} frames in {SECS} s from a still window"
    );
}

/// A capture loop that lands on a thread something left in a
/// single-threaded COM apartment -- one of tokio's reused pool threads
/// -- still captures. It used to fail before its first frame: a
/// recording with no video. Shows a small window in the top-left corner
/// for ~1 s, so it's ignored by default.
#[test]
#[ignore]
fn capture_works_on_a_thread_left_in_a_single_threaded_apartment() {
    let window = SlidingBarWindow::spawn();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let hwnd = window.hwnd;
    let target = CaptureTarget::from_raw(CaptureKind::Window, hwnd);
    let (w, h) = query_capture_size(target).expect("query_capture_size");
    let size = (w & !1, h & !1);
    let ctrl = CaptureControl::new();
    let (tx, mut rx) = mpsc::channel(256);
    let capture_ctrl = ctrl.clone();
    let capture = std::thread::spawn(move || {
        let _sta = crate::sources::SingleThreadedApartment::join();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_video_capture(
                CaptureTarget::from_raw(CaptureKind::Window, hwnd),
                size,
                size,
                LiveFps::new(30),
                capture_ctrl,
                tx,
                false,
                false,
                None,
            ))
    });
    std::thread::sleep(std::time::Duration::from_secs(1));
    ctrl.request_stop();
    capture
        .join()
        .unwrap()
        .expect("run_video_capture failed on a single-threaded apartment's thread");
    let mut frames = 0;
    while rx.try_recv().is_ok() {
        frames += 1;
    }
    assert!(frames > 0, "no frames");
}

/// A minimized window gives no frame at all, so there's nothing to send
/// again: black frames keep the video going instead. Minimizes a small
/// window for ~3 s, so it's ignored by default.
#[test]
#[ignore]
fn minimized_window_gets_black_frames() {
    use windows::Win32::UI::WindowsAndMessaging::{SW_MINIMIZE, ShowWindow};
    const SECS: u64 = 3;
    let window = SlidingBarWindow::spawn_still();
    let hwnd = HWND(window.hwnd as *mut core::ffi::c_void);
    let _ = unsafe { ShowWindow(hwnd, SW_MINIMIZE) };
    std::thread::sleep(std::time::Duration::from_millis(300));
    let (frames, _) = capture_for((CaptureKind::Window, window.hwnd), 30, SECS, None);
    let expected =
        (std::time::Duration::from_secs(SECS).as_millis() / STILL_FRAME_REPEAT.as_millis()) as u64;
    println!("minimized window: {frames} frames in {SECS} s (due: {expected})");
    assert!(
        frames >= expected - 2,
        "{frames} frames in {SECS} s from a minimized window"
    );
}

/// The left edge of the white bar a `SlidingBarWindow` frame of `width`
/// pixels shows, if any, judged along its middle row.
fn white_bar_x(frame: &[u8], (width, height): (u32, u32)) -> Option<usize> {
    let row = (height / 2) as usize * width as usize * 4;
    frame[row..row + width as usize * 4]
        .as_chunks::<4>()
        .0
        .iter()
        .position(|px| px[..3].iter().all(|&c| c >= 250))
}

/// The last change before a window goes still makes it into the
/// recording, even when it came too soon after the frame before it for
/// the fps cap: it used to be dropped, and the frame before it repeated
/// in its place for as long as the window stayed still. Slides the bar
/// of a small window, stops it, and checks where the recording's last
/// frame shows it -- four times, at 20 fps so most display refreshes are
/// over the cap. Shows a small window for ~6 s, so it's ignored by
/// default.
#[test]
#[ignore]
fn the_last_change_before_a_still_picture_is_recorded() {
    for steps in [97, 131, 173, 211] {
        let window = SlidingBarWindow::spawn_with(steps, false);
        let handle = window.hwnd;
        let target = CaptureTarget::from_raw(CaptureKind::Window, handle);
        let (w, h) = query_capture_size(target).expect("query_capture_size");
        let size = (w & !1, h & !1);
        let ctrl = CaptureControl::new();
        let (tx, mut rx) = mpsc::channel(256);
        let capture_ctrl = ctrl.clone();
        let capture = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(run_video_capture(
                    CaptureTarget::from_raw(CaptureKind::Window, handle),
                    size,
                    size,
                    LiveFps::new(20),
                    capture_ctrl,
                    tx,
                    false,
                    false,
                    None,
                ))
        });
        let last = std::thread::spawn(move || {
            let mut last = None;
            while let Some(frame) = rx.blocking_recv() {
                if let FrameData::Cpu(pixels) = frame.data {
                    last = Some(pixels);
                }
            }
            last
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        window.go();
        while !window.settled() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // A slot to record a held frame in, and a repeat to send it.
        std::thread::sleep(std::time::Duration::from_millis(600));
        ctrl.request_stop();
        capture.join().unwrap().expect("run_video_capture failed");
        let last = last.join().unwrap().expect("no frame recorded");

        // The window may be stretched for the display's scaling.
        let scale = f64::from(size.0) / f64::from(SlidingBarWindow::W);
        let expected = f64::from(SlidingBarWindow::bar_x_after(steps)) * scale;
        let found = white_bar_x(&last, size).expect("no bar in the last frame");
        println!("{steps} steps: bar at {found}, expected {expected:.0}");
        assert!(
            (found as f64 - expected).abs() <= scale.ceil() + 1.0,
            "after {steps} steps the recording ends with the bar at {found}, not {expected:.0}"
        );
    }
}

/// End-to-end fps cap: a window that changes every millisecond makes WGC
/// deliver a frame of its monitor on every display refresh. Uncapped (what
/// every recording did before the cap) that's the refresh rate; capped at
/// 30 fps it must be about 30 (30, not 60, so a 60 Hz monitor can still
/// show the difference). Also prints a 60 fps run for comparison. Shows a
/// small window in the top-left corner for ~10 s, so it's ignored by
/// default.
#[test]
#[ignore]
fn capture_is_capped_to_the_configured_fps() {
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
    const SECS: u64 = 3;
    const FPS: u32 = 30;
    let window = SlidingBarWindow::spawn();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let hwnd = HWND(window.hwnd as *mut core::ffi::c_void);
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let target = (CaptureKind::Monitor, monitor.0 as usize);

    let per_sec = |n: u64| n as f64 / SECS as f64;
    let cpu_ms_per_sec = |d: std::time::Duration| d.as_secs_f64() * 1000.0 / SECS as f64;
    let mut runs = Vec::new();
    for fps in [1000, 60, FPS] {
        let (frames, cpu) = capture_for(target, fps, SECS, None);
        println!(
            "cap {fps:>4}: {:>5.1} fps delivered, {:>4.0} ms CPU/s",
            per_sec(frames),
            cpu_ms_per_sec(cpu)
        );
        runs.push(frames);
    }
    let (uncapped, capped) = (runs[0], runs[2]);

    let budget = u64::from(FPS) * SECS;
    assert!(
        per_sec(uncapped) > f64::from(FPS) * 1.5,
        "the display didn't refresh fast enough to test a {FPS} fps cap"
    );
    assert!(
        capped <= budget + 2,
        "{capped} frames in {SECS} s at {FPS} fps"
    );
    assert!(
        capped >= budget * 8 / 10,
        "{capped} frames in {SECS} s at {FPS} fps"
    );
}

/// Frames that stay on the GPU come through as textures (`capture_for`
/// only counts those): at the capped rate from a window that changes
/// every millisecond, and as repeats from a still one. Shows small
/// windows in the top-left corner for ~6 s, so it's ignored by default.
#[test]
#[ignore]
fn gpu_frames_arrive_as_textures() {
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
    const SECS: u64 = 3;
    const FPS: u32 = 30;
    let gpu = GpuFrames::new().expect("GpuFrames");

    let sliding = SlidingBarWindow::spawn();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let hwnd = HWND(sliding.hwnd as *mut core::ffi::c_void);
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let (frames, cpu) = capture_for(
        (CaptureKind::Monitor, monitor.0 as usize),
        FPS,
        SECS,
        Some(gpu.clone()),
    );
    println!("gpu, changing display: {frames} frames in {SECS} s, {cpu:?} CPU");
    let budget = u64::from(FPS) * SECS;
    assert!(
        (budget * 8 / 10..=budget + 2).contains(&frames),
        "{frames} frames in {SECS} s at {FPS} fps"
    );
    drop(sliding);

    let still = SlidingBarWindow::spawn_still();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let (frames, _) = capture_for((CaptureKind::Window, still.hwnd), FPS, SECS, Some(gpu));
    let expected =
        (std::time::Duration::from_secs(SECS).as_millis() / STILL_FRAME_REPEAT.as_millis()) as u64;
    println!("gpu, still window: {frames} frames in {SECS} s (repeats due: {expected})");
    assert!(
        frames >= expected - 2,
        "{frames} frames from a still window"
    );
}

/// What keeping frames on the GPU saves: records the display the
/// top-left corner is on (where a window changing every millisecond
/// keeps frames coming at the full rate) through the real session, at
/// the display's own resolution and 60 fps, once each way, and compares
/// the process's CPU time. Both recordings must finish and hold video.
/// Takes ~15 s and shows a small window, so it's ignored by default.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn gpu_frames_record_with_less_cpu() {
    use crate::session::{CaptureInputs, EncodeSettings, SessionManager};
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
    const SECS: u64 = 5;

    let sliding = SlidingBarWindow::spawn();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let hwnd = HWND(sliding.hwnd as *mut core::ffi::c_void);
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) }.0 as usize;
    let source = crate::sources::enumerate_sources()
        .into_iter()
        .find(|s| s.kind == CaptureKind::Monitor && s.hwnd == monitor)
        .expect("the display source");
    let dir = tempfile::tempdir().unwrap();
    let mut cpu_ms_per_sec = Vec::new();
    for gpu_frames in [false, true] {
        let frame_count = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut sm = SessionManager::new();
        let inputs = CaptureInputs {
            encode: EncodeSettings {
                fps: 60,
                gpu_frames,
                ..EncodeSettings::default()
            },
            ..CaptureInputs::default()
        };
        sm.start_capture(
            source.clone(),
            inputs,
            false,
            Arc::clone(&frame_count),
            dir.path(),
            crate::recording_naming::RecordingNaming::CustomPrefix(format!("gpu_{gpu_frames}")),
        )
        .expect("start_capture");
        // Settle past the writer's start-up before measuring.
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let (frames_before, cpu_before) = (
            frame_count.load(std::sync::atomic::Ordering::Relaxed),
            process_cpu_time(),
        );
        tokio::time::sleep(std::time::Duration::from_secs(SECS)).await;
        let cpu = process_cpu_time() - cpu_before;
        let frames = frame_count.load(std::sync::atomic::Ordering::Relaxed) - frames_before;
        let handle = tokio::task::block_in_place(|| sm.stop_capture()).expect("stop_capture");
        let path = tokio::time::timeout(std::time::Duration::from_secs(10), handle)
            .await
            .expect("finalizing took over 10 s")
            .expect("recorder task panicked")
            .expect("finalize failed");
        let size = std::fs::metadata(&path).unwrap().len();
        let per_sec = cpu.as_secs_f64() * 1000.0 / SECS as f64;
        println!(
            "gpu_frames={gpu_frames}: {} fps recorded, {per_sec:.0} ms CPU/s, {size} bytes",
            frames / SECS
        );
        assert!(frames / SECS >= 40, "only {frames} frames in {SECS} s");
        assert!(size > 100_000, "{size}-byte recording");
        cpu_ms_per_sec.push(per_sec);
    }
    assert!(
        cpu_ms_per_sec[1] < cpu_ms_per_sec[0],
        "GPU frames took more CPU: {cpu_ms_per_sec:?} ms/s"
    );
}

#[test]
fn a_copy_for_readback_is_done_by_the_time_its_frame_is_read() {
    use windows::Win32::Graphics::Direct3D11::{D3D11_BIND_SHADER_RESOURCE, D3D11_USAGE_DEFAULT};

    let device = D3dDevice::new().unwrap();
    let texture = |usage, bind: u32, cpu: u32| -> ID3D11Resource {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: 1920,
            Height: 1080,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: usage,
            BindFlags: bind,
            CPUAccessFlags: cpu,
            MiscFlags: 0,
        };
        let mut made: Option<ID3D11Texture2D> = None;
        unsafe {
            device
                .d3d_device
                .CreateTexture2D(&desc, None, Some(&mut made))
                .unwrap();
        }
        made.unwrap().cast().unwrap()
    };
    let frame = texture(D3D11_USAGE_DEFAULT, D3D11_BIND_SHADER_RESOURCE.0 as u32, 0);
    let staging = texture(D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ.0 as u32);

    unsafe { copy_for_readback(&device, &staging, &frame) };
    // A readback comes one frame after its copy.
    std::thread::sleep(std::time::Duration::from_millis(16));

    // Asks without waiting until the frame is there. A copy that was
    // started is there at once or after one ask (a driver may want to be
    // asked before it hands over what it has); one that only starts with
    // the first ask takes hundreds of them.
    let mut misses = 0;
    loop {
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        let asked = unsafe {
            device.d3d_context.Map(
                &staging,
                0,
                D3D11_MAP_READ,
                D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32,
                Some(&mut mapped),
            )
        };
        match asked {
            Ok(()) => break,
            Err(e) if e.code() == DXGI_ERROR_WAS_STILL_DRAWING => misses += 1,
            Err(e) => panic!("Map: {e}"),
        }
        assert!(misses < 10_000, "the copy never finished");
    }
    unsafe { device.d3d_context.Unmap(&staging, 0) };
    assert!(
        misses <= 20,
        "the copy only started when asked for: {misses} misses"
    );
}
