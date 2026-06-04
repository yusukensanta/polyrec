# Pause / Resume Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement pause/resume so F8 and a UI button stop timestamp progression and discard captured frames/samples, then resume seamlessly from where recording left off.

**Architecture:** `RecordingClock` gains atomic pause tracking so `elapsed()` excludes paused time; an `Arc<AtomicBool>` pause flag is threaded into capture loops to discard samples during pause; `SessionManager` exposes `pause_capture()`/`resume_capture()`; the dashboard replaces `recording_start` timer math with clock-based elapsed and adds a PAUSE/RESUME button alongside STOP.

**Tech Stack:** Rust, `std::sync::atomic`, QPC (`QueryPerformanceCounter`), egui 0.29

---

## File Map

| File | Change |
|------|--------|
| `src/session/clock.rs` | Add `AtomicU64`/`AtomicI64` fields; add `pause()`/`resume()`; update `elapsed()` |
| `src/session/mod.rs` | Add `pause_flag` to `ActiveCapture`; add `pause_capture()`/`resume_capture()`/`is_paused()` |
| `src/capture/audio.rs` | Add `pause_flag: Arc<AtomicBool>` param; release buffer then discard when paused |
| `src/capture/video.rs` | Add `pause_flag: Arc<AtomicBool>` param; skip frame processing when paused |
| `src/ui/dashboard.rs` | `handle_pause_button`, F8 routing, F9-while-paused, clock-based timer, PAUSED label, PAUSE+STOP/RESUME buttons |

---

## Task 1: RecordingClock pause tracking

**Files:**
- Modify: `src/session/clock.rs`

- [ ] **Step 1: Write the failing tests**

In `src/session/clock.rs`, add inside the `#[cfg(test)] mod tests` block after the existing tests:

```rust
    #[test]
    fn elapsed_freezes_while_paused() {
        let clock = RecordingClock::new();
        thread::sleep(Duration::from_millis(20));
        clock.pause();
        let t1 = clock.elapsed();
        thread::sleep(Duration::from_millis(60));
        let t2 = clock.elapsed();
        let diff = if t2 > t1 { (t2 - t1).as_millis() } else { 0 };
        assert!(diff < 5, "elapsed should not advance while paused, advanced by {diff}ms");
    }

    #[test]
    fn elapsed_excludes_paused_time() {
        let clock = RecordingClock::new();
        thread::sleep(Duration::from_millis(20));
        clock.pause();
        thread::sleep(Duration::from_millis(80)); // must not count
        clock.resume();
        thread::sleep(Duration::from_millis(20));
        let elapsed = clock.elapsed();
        assert!(
            elapsed.as_millis() < 70,
            "elapsed should exclude paused 80ms, got {}ms",
            elapsed.as_millis()
        );
        assert!(
            elapsed.as_millis() >= 30,
            "elapsed should include ~40ms active time, got {}ms",
            elapsed.as_millis()
        );
    }
```

- [ ] **Step 2: Run to verify they fail**

```powershell
cd "C:\Users\yusuk\work\polyrec\.worktrees\feat-pause-resume"
cargo test elapsed_freezes_while_paused elapsed_excludes_paused_time -- --nocapture 2>&1 | tail -10
```

Expected: compile error or `pause` method not found.

- [ ] **Step 3: Implement pause tracking in RecordingClock**

Replace the entire contents of `src/session/clock.rs` with:

```rust
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

pub struct RecordingClock {
    start_ticks: i64,
    frequency: i64,
    /// Total nanoseconds spent paused so far.
    accumulated_paused_nanos: AtomicU64,
    /// QPC ticks at the moment pause() was called; 0 means not paused.
    pause_tick: AtomicI64,
}

impl RecordingClock {
    pub fn new() -> Arc<Self> {
        let mut frequency = 0i64;
        let mut ticks = 0i64;
        unsafe {
            QueryPerformanceFrequency(&mut frequency)
                .expect("QueryPerformanceFrequency failed — requires Windows XP+");
            QueryPerformanceCounter(&mut ticks)
                .expect("QueryPerformanceCounter failed — requires Windows XP+");
        }
        Arc::new(Self {
            start_ticks: ticks,
            frequency,
            accumulated_paused_nanos: AtomicU64::new(0),
            pause_tick: AtomicI64::new(0),
        })
    }

    /// Freeze the clock. Idempotent — calling twice without resume in between is harmless.
    pub fn pause(&self) {
        let mut now = 0i64;
        unsafe {
            QueryPerformanceCounter(&mut now).expect("QueryPerformanceCounter failed");
        }
        // Only store if not already paused (pause_tick == 0 means running)
        let _ = self.pause_tick.compare_exchange(0, now, Ordering::SeqCst, Ordering::SeqCst);
    }

    /// Resume the clock, accumulating the paused duration. Idempotent.
    pub fn resume(&self) {
        let paused_at = self.pause_tick.swap(0, Ordering::SeqCst);
        if paused_at != 0 {
            let mut now = 0i64;
            unsafe {
                QueryPerformanceCounter(&mut now).expect("QueryPerformanceCounter failed");
            }
            let paused_ticks = now - paused_at;
            let paused_nanos =
                (paused_ticks as u128 * 1_000_000_000 / self.frequency as u128) as u64;
            self.accumulated_paused_nanos
                .fetch_add(paused_nanos, Ordering::SeqCst);
        }
    }

    /// Elapsed recording time, excluding any paused periods.
    pub fn elapsed(&self) -> Duration {
        let mut now = 0i64;
        unsafe {
            QueryPerformanceCounter(&mut now).expect("QueryPerformanceCounter failed");
        }
        let pt = self.pause_tick.load(Ordering::SeqCst);
        // Freeze at the tick when pause() was called; advance normally otherwise.
        let running_ticks = if pt > 0 {
            pt - self.start_ticks
        } else {
            now - self.start_ticks
        };
        let total_nanos =
            (running_ticks as u128 * 1_000_000_000) / self.frequency as u128;
        let paused_nanos = self.accumulated_paused_nanos.load(Ordering::SeqCst) as u128;
        Duration::from_nanos((total_nanos.saturating_sub(paused_nanos)) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn clock_starts_near_zero() {
        let clock = RecordingClock::new();
        let elapsed = clock.elapsed();
        assert!(elapsed.as_millis() < 100, "elapsed should be < 100ms right after creation");
    }

    #[test]
    fn clock_advances_monotonically() {
        let clock = RecordingClock::new();
        let t1 = clock.elapsed();
        thread::sleep(Duration::from_millis(50));
        let t2 = clock.elapsed();
        assert!(t2 > t1, "clock must advance");
        assert!(t2.as_millis() >= 50, "at least 50ms should have elapsed");
    }

    #[test]
    fn shared_clock_reads_same_reference() {
        let clock = RecordingClock::new();
        let clone = Arc::clone(&clock);
        thread::sleep(Duration::from_millis(10));
        let t1 = clock.elapsed();
        let t2 = clone.elapsed();
        let diff = if t1 > t2 { t1 - t2 } else { t2 - t1 };
        assert!(diff.as_micros() < 1000, "two reads of same clock should be within 1ms");
    }

    #[test]
    fn elapsed_freezes_while_paused() {
        let clock = RecordingClock::new();
        thread::sleep(Duration::from_millis(20));
        clock.pause();
        let t1 = clock.elapsed();
        thread::sleep(Duration::from_millis(60));
        let t2 = clock.elapsed();
        let diff = if t2 > t1 { (t2 - t1).as_millis() } else { 0 };
        assert!(diff < 5, "elapsed should not advance while paused, advanced by {diff}ms");
    }

    #[test]
    fn elapsed_excludes_paused_time() {
        let clock = RecordingClock::new();
        thread::sleep(Duration::from_millis(20));
        clock.pause();
        thread::sleep(Duration::from_millis(80));
        clock.resume();
        thread::sleep(Duration::from_millis(20));
        let elapsed = clock.elapsed();
        assert!(
            elapsed.as_millis() < 70,
            "elapsed should exclude paused 80ms, got {}ms",
            elapsed.as_millis()
        );
        assert!(
            elapsed.as_millis() >= 30,
            "elapsed should include ~40ms active time, got {}ms",
            elapsed.as_millis()
        );
    }
}
```

- [ ] **Step 4: Run all clock tests**

```powershell
cargo test session::clock -- --nocapture 2>&1 | tail -10
```

Expected: 5 tests, all pass.

- [ ] **Step 5: Commit**

```powershell
git add src/session/clock.rs
git commit -m "feat: add pause/resume to RecordingClock with atomic tracking"
```

---

## Task 2: Audio capture pause flag

**Files:**
- Modify: `src/capture/audio.rs`

- [ ] **Step 1: Add `pause_flag` parameter to `run_audio_capture`**

In `src/capture/audio.rs`, change the function signature from:

```rust
pub async fn run_audio_capture(
    device_id: String,
    track_id: TrackId,
    is_loopback: bool,
    clock: Arc<RecordingClock>,
    tx: mpsc::Sender<AudioSamples>,
) -> Result<(), AppError> {
```

To:

```rust
pub async fn run_audio_capture(
    device_id: String,
    track_id: TrackId,
    is_loopback: bool,
    clock: Arc<RecordingClock>,
    pause_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    tx: mpsc::Sender<AudioSamples>,
) -> Result<(), AppError> {
```

- [ ] **Step 2: Discard samples when paused (but always release buffer)**

Find the `Ok(()) if frames_available > 0 =>` branch inside the capture loop. It currently ends with `tx.send(audio).await...`. Replace the block from the `ReleaseBuffer` call onward with:

```rust
                    capture_client
                        .ReleaseBuffer(frames_available)
                        .map_err(|e| AppError::Windows(format!("ReleaseBuffer: {e}")))?;

                    // Always release buffer first (above), then discard if paused.
                    if pause_flag.load(std::sync::atomic::Ordering::Relaxed) {
                        continue;
                    }

                    let pts = clock.elapsed();
                    let audio = AudioSamples {
                        track_id,
                        pts,
                        samples,
                        sample_rate,
                        channels,
                    };

                    if tx.send(audio).await.is_err() {
                        break;
                    }
```

- [ ] **Step 3: Verify it compiles**

```powershell
cargo build 2>&1 | Select-String "^error" | Select-Object -First 20
```

Expected: compile errors only about call sites that haven't been updated yet (in `session/mod.rs`). The audio capture module itself should compile.

- [ ] **Step 4: Commit**

```powershell
git add src/capture/audio.rs
git commit -m "feat: add pause_flag to run_audio_capture"
```

---

## Task 3: Video capture pause flag

**Files:**
- Modify: `src/capture/video.rs`

- [ ] **Step 1: Add `pause_flag` parameter to `run_video_capture`**

Change the function signature from:

```rust
pub async fn run_video_capture(
    hwnd: HWND,
    clock: Arc<RecordingClock>,
    tx: mpsc::Sender<VideoFrame>,
) -> Result<(), AppError> {
```

To:

```rust
pub async fn run_video_capture(
    hwnd: HWND,
    clock: Arc<RecordingClock>,
    pause_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    tx: mpsc::Sender<VideoFrame>,
) -> Result<(), AppError> {
```

- [ ] **Step 2: Skip frame processing when paused**

In the frame loop, after successfully acquiring a frame with `TryGetNextFrame()`, add a pause check BEFORE the expensive GPU operations:

Find:
```rust
        let frame = match frame_pool.TryGetNextFrame() {
            Ok(f) => f,
            Err(_) => {
                tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                continue;
            }
        };

        let surface = frame
```

Replace with:

```rust
        let frame = match frame_pool.TryGetNextFrame() {
            Ok(f) => f,
            Err(_) => {
                tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                continue;
            }
        };

        // Discard frame when paused — skip GPU readback to save CPU/memory bandwidth.
        if pause_flag.load(std::sync::atomic::Ordering::Relaxed) {
            tokio::time::sleep(tokio::time::Duration::from_millis(16)).await;
            continue;
        }

        let surface = frame
```

- [ ] **Step 3: Verify it compiles (call-site errors expected)**

```powershell
cargo build 2>&1 | Select-String "^error" | Select-Object -First 20
```

Expected: errors only in `session/mod.rs` call sites. Video capture module itself compiles.

- [ ] **Step 4: Commit**

```powershell
git add src/capture/video.rs
git commit -m "feat: add pause_flag to run_video_capture"
```

---

## Task 4: SessionManager — pause_flag, pause_capture, resume_capture

**Files:**
- Modify: `src/session/mod.rs`

- [ ] **Step 1: Add pause_flag to ActiveCapture and imports**

At the top of `src/session/mod.rs`, add to the use block:

```rust
use std::sync::atomic::{AtomicBool, Ordering};
```

Add `pause_flag` field to `ActiveCapture`:

```rust
pub struct ActiveCapture {
    capture_handles: Vec<JoinHandle<()>>,
    pump_handles: Vec<JoinHandle<()>>,
    pub recorder_handle: JoinHandle<Result<PathBuf, AppError>>,
    pub recording_tx: mpsc::Sender<RecordingCommand>,
    pub clock: Arc<RecordingClock>,
    pub pause_flag: Arc<AtomicBool>,
    pub output_path: PathBuf,
}
```

- [ ] **Step 2: Create pause_flag in start_capture and pass to capture tasks**

In `start_capture`, after `let clock = RecordingClock::new();`, add:

```rust
        let pause_flag = Arc::new(AtomicBool::new(false));
```

Update the video capture spawn to pass `pause_flag`:

```rust
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
```

Update each audio capture spawn to pass `pause_flag`:

```rust
        for (i, dev) in audio_devices.into_iter().enumerate() {
            let track_id = TrackId::new(i as u32);
            let (audio_tx, audio_rx) = mpsc::channel(AUDIO_CHANNEL_CAPACITY);
            let audio_clock = Arc::clone(&clock);
            let audio_pause = Arc::clone(&pause_flag);
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
                        dev_id, track_id, is_loopback, audio_clock, audio_pause, audio_tx,
                    )
                    .await
                    {
                        tracing::error!("AudioCapture[{track_id:?}] error: {e}");
                    }
                });
            });
```

Store `pause_flag` in `ActiveCapture`:

```rust
        self.active = Some(ActiveCapture {
            capture_handles,
            pump_handles,
            recorder_handle,
            recording_tx,
            clock,
            pause_flag,
            output_path: output_path.clone(),
        });
```

- [ ] **Step 3: Add pause_capture, resume_capture, is_paused**

Add these methods to the `impl SessionManager` block:

```rust
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
```

- [ ] **Step 4: Run all tests**

```powershell
cargo test 2>&1 | tail -5
```

Expected: 43 passed, 0 failed.

- [ ] **Step 5: Commit**

```powershell
git add src/session/mod.rs
git commit -m "feat: add pause_flag and pause_capture/resume_capture to SessionManager"
```

---

## Task 5: Dashboard — UI wiring

**Files:**
- Modify: `src/ui/dashboard.rs`

### Step-by-step changes

- [ ] **Step 1: Add handle_pause_button method**

In the `impl App` block at the bottom of `dashboard.rs`, add after `handle_rec_button`:

```rust
    fn handle_pause_button(&mut self) {
        if self.session.is_recording() {
            self.session.pause_capture();
        } else if self.session.is_paused() {
            self.session.resume_capture();
        }
    }
```

- [ ] **Step 2: Wire F8 hotkey**

Find:
```rust
                HotkeyEvent::Pause => {}
```

Replace with:
```rust
                HotkeyEvent::Pause => self.handle_pause_button(),
```

- [ ] **Step 3: Make F9 (handle_rec_button) work while paused**

Find the start of `handle_rec_button`:

```rust
    fn handle_rec_button(&mut self, is_recording: bool) {
        if is_recording {
```

Replace with:

```rust
    fn handle_rec_button(&mut self, is_recording: bool) {
        let is_paused = self.session.is_paused();
        if is_recording || is_paused {
```

- [ ] **Step 4: Switch timer to use clock elapsed**

Find (around line 200):
```rust
            if is_recording {
                let elapsed = self
                    .recording_start
                    .map(|t| t.elapsed())
                    .unwrap_or_default();
```

Replace with:

```rust
            let is_paused = self.session.is_paused();
            if is_recording || is_paused {
                let elapsed = self
                    .session
                    .active
                    .as_ref()
                    .map(|a| a.clock.elapsed())
                    .unwrap_or_default();
```

- [ ] **Step 5: Add PAUSED label in status section**

Find inside the `if is_recording || is_paused {` block, after the pulsing dot `ui.horizontal(...)`:

The current code shows "RECORDING" label. Expand it to show the correct state label:

Replace:
```rust
                    ui.label(
                        egui::RichText::new("RECORDING")
                            .size(10.0)
                            .color(egui::Color32::from_rgb(200, 80, 80))
                            .strong(),
                    );
```

With:
```rust
                    let state_label = if is_paused { "PAUSED" } else { "RECORDING" };
                    let state_color = if is_paused {
                        egui::Color32::from_rgb(200, 160, 50)
                    } else {
                        egui::Color32::from_rgb(200, 80, 80)
                    };
                    ui.label(
                        egui::RichText::new(state_label)
                            .size(10.0)
                            .color(state_color)
                            .strong(),
                    );
```

- [ ] **Step 6: Update button row for PAUSE/RESUME**

Find the `ui.with_layout(egui::Layout::bottom_up(...))` block:

```rust
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                ui.add_space(8.0);

                let rec_label = if is_recording { "⏹ STOP" } else { "⏺ REC" };
                let rec_color = if is_recording {
                    egui::Color32::from_rgb(248, 113, 113)
                } else {
                    ACCENT_IDLE
                };
                let btn_bg = if is_recording { BG_BTN_STOP } else { BG_BTN_IDLE };

                let btn = egui::Button::new(
                    egui::RichText::new(rec_label).color(rec_color).size(18.0),
                )
                .fill(btn_bg)
                .min_size(egui::Vec2::new(130.0, 52.0));

                if ui.add(btn).clicked() {
                    self.handle_rec_button(is_recording);
                }

                ui.label(
                    egui::RichText::new(format!("State: {:?}", self.session.state()))
                        .size(10.0)
                        .color(TEXT_MUTED),
                );
            });
```

Replace with:

```rust
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                ui.add_space(8.0);

                let is_paused = self.session.is_paused();

                // State debug label at bottom
                ui.label(
                    egui::RichText::new(format!("State: {:?}", self.session.state()))
                        .size(10.0)
                        .color(TEXT_MUTED),
                );

                if is_paused {
                    // RESUME button replaces STOP while paused
                    let btn = egui::Button::new(
                        egui::RichText::new("▶ RESUME").color(ACCENT_IDLE).size(18.0),
                    )
                    .fill(BG_BTN_IDLE)
                    .min_size(egui::Vec2::new(130.0, 52.0));
                    if ui.add(btn).clicked() {
                        self.handle_pause_button();
                    }
                } else if is_recording {
                    // STOP + PAUSE buttons side by side
                    ui.horizontal(|ui| {
                        let stop_btn = egui::Button::new(
                            egui::RichText::new("⏹ STOP")
                                .color(egui::Color32::from_rgb(248, 113, 113))
                                .size(18.0),
                        )
                        .fill(BG_BTN_STOP)
                        .min_size(egui::Vec2::new(90.0, 52.0));
                        if ui.add(stop_btn).clicked() {
                            self.handle_rec_button(is_recording);
                        }

                        let pause_btn = egui::Button::new(
                            egui::RichText::new("⏸").color(TEXT_MUTED).size(18.0),
                        )
                        .fill(egui::Color32::from_rgb(30, 30, 46))
                        .min_size(egui::Vec2::new(36.0, 52.0));
                        if ui.add(pause_btn).clicked() {
                            self.handle_pause_button();
                        }
                    });
                } else {
                    // REC button when idle
                    let btn = egui::Button::new(
                        egui::RichText::new("⏺ REC").color(ACCENT_IDLE).size(18.0),
                    )
                    .fill(BG_BTN_IDLE)
                    .min_size(egui::Vec2::new(130.0, 52.0));
                    if ui.add(btn).clicked() {
                        self.handle_rec_button(is_recording);
                    }
                }
            });
```

- [ ] **Step 7: Run all tests**

```powershell
cargo test 2>&1 | tail -5
```

Expected: 43 passed, 0 failed.

- [ ] **Step 8: Commit**

```powershell
git add src/ui/dashboard.rs
git commit -m "feat: pause/resume UI — PAUSE button, RESUME button, paused timer, F8 hotkey"
```

---

## Task 6: Smoke test

- [ ] **Step 1: Run the app**

```powershell
cargo run
```

- [ ] **Step 2: Verify button states**

- Idle: green ⏺ REC button
- Click REC → Recording: ⏹ STOP + ⏸ PAUSE buttons appear side by side. Timer advances. "RECORDING" label in amber.
- Click ⏸ PAUSE → Paused: ▶ RESUME button. Timer freezes. Status label changes to "PAUSED" in yellow.
- Click ▶ RESUME → Recording: STOP + PAUSE appear again. Timer continues from where it froze.
- Press F8 to toggle pause/resume as well.

- [ ] **Step 3: Verify timer excludes paused time**

Record 5s, pause 10s, resume 5s, stop. The recorded file should be ~10s long (not ~20s).

---

## Self-Review

**Spec coverage:**
- ✅ `RecordingClock.pause()`/`resume()`/`elapsed()` with atomics
- ✅ Audio capture: release buffer then discard when paused
- ✅ Video capture: skip GPU readback when paused
- ✅ `ActiveCapture.pause_flag`
- ✅ `pause_capture()`/`resume_capture()`/`is_paused()` on SessionManager
- ✅ `handle_pause_button()` in dashboard
- ✅ F8 hotkey routes to `handle_pause_button`
- ✅ F9/STOP works while paused
- ✅ Timer uses clock elapsed (freezes during pause)
- ✅ STATUS shows "PAUSED" label when paused
- ✅ Button row: Recording→STOP+PAUSE, Paused→RESUME, Idle→REC

**Placeholder scan:** None found.

**Type consistency:**
- `pause_flag: Arc<AtomicBool>` defined in Task 4, used in Tasks 2 and 3 via `Arc::clone` ✓
- `session.is_paused()` defined in Task 4, used in Task 5 ✓
- `a.clock.elapsed()` — `ActiveCapture.clock` is `Arc<RecordingClock>` with `elapsed()` updated in Task 1 ✓
- `RecordingClock::pause()`/`resume()` called in Task 4, defined in Task 1 ✓
