# Pause / Resume — Design Spec

**Date:** 2026-06-04
**Status:** Approved

## Summary

Implement pause/resume for an in-progress recording. F8 hotkey toggles pause; a ▶ RESUME button replaces the STOP button while paused. Timestamps in the output file exclude paused time. Capture tasks discard samples (without blocking their WASAPI buffers) while paused.

## Scope

- `src/session/clock.rs` — add pause tracking
- `src/session/mod.rs` — add `pause_flag`, `pause_capture()`, `resume_capture()`
- `src/capture/audio.rs` — check pause flag, discard + release buffer
- `src/capture/video.rs` — check pause flag, discard frame
- `src/ui/dashboard.rs` — pause button, PAUSED state display, F8 routing

## 1. RecordingClock (clock.rs)

Add two atomics for interior-mutability pause tracking:

```rust
pub struct RecordingClock {
    start_ticks: i64,
    frequency: i64,
    accumulated_paused_nanos: AtomicU64,  // total nanoseconds spent paused so far
    pause_tick: AtomicI64,                // QPC ticks at pause start; 0 = not paused
}
```

### Methods

**`pause()`** — called once when entering paused state:
```
current = qpc_now()
pause_tick.store(current, SeqCst)
```

**`resume()`** — called once when leaving paused state:
```
paused_at = pause_tick.swap(0, SeqCst)
if paused_at != 0:
    paused_ticks = qpc_now() - paused_at
    paused_nanos = ticks_to_nanos(paused_ticks)
    accumulated_paused_nanos.fetch_add(paused_nanos, SeqCst)
```

**`elapsed()`** — updated to exclude paused time:
```
now = qpc_now()
pt  = pause_tick.load(SeqCst)
running_ticks = if pt > 0 { pt - start_ticks } else { now - start_ticks }
total_nanos   = ticks_to_nanos(running_ticks)
paused_nanos  = accumulated_paused_nanos.load(SeqCst)
Duration::from_nanos(total_nanos - paused_nanos)
```

`ticks_to_nanos(t)` = `(t as u128 * 1_000_000_000) / frequency as u128` — same formula as existing.

`RecordingClock::new()` returns `Arc<Self>` as before; the two new fields init to 0.

## 2. SessionManager (session/mod.rs)

### ActiveCapture

Add field:
```rust
pub pause_flag: Arc<AtomicBool>,
```

### start_capture

Before spawning capture tasks:
```rust
let pause_flag = Arc::new(AtomicBool::new(false));
```
Pass `Arc::clone(&pause_flag)` to each capture task.
Store in `ActiveCapture`.

### New methods

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
```

### Helper

```rust
pub fn is_paused(&self) -> bool {
    matches!(self.state, SessionState::Paused)
}
```

## 3. Capture Tasks

### run_audio_capture (capture/audio.rs)

Add `pause_flag: Arc<AtomicBool>` parameter. In the `frames_available > 0` branch, after reading and converting samples:

```rust
capture_client.ReleaseBuffer(frames_available)?;  // always release

if pause_flag.load(Ordering::Relaxed) {
    continue;  // discard — don't forward to encoder
}

tx.send(AudioSamples { ... }).await...
```

Releasing the buffer while paused prevents WASAPI from blocking on a full capture buffer.

### run_video_capture (capture/video.rs)

Add `pause_flag: Arc<AtomicBool>` parameter. After acquiring a frame:

```rust
if pause_flag.load(Ordering::Relaxed) {
    // drop frame, don't send
    continue;
}
tx.send(frame).await...
```

## 4. Dashboard (dashboard.rs)

### New method: handle_pause_button

```rust
fn handle_pause_button(&mut self) {
    if self.session.is_recording() {
        self.session.pause_capture();
    } else if self.session.is_paused() {
        self.session.resume_capture();
    }
}
```

### F8 hotkey

```rust
HotkeyEvent::Pause => self.handle_pause_button(),
```

### F9 hotkey / STOP while paused

In `handle_rec_button`, the `is_recording` check already handles the stop path. Add paused case:

```rust
fn handle_rec_button(&mut self, is_recording: bool) {
    let is_paused = self.session.is_paused();
    if is_recording || is_paused {
        // stop
        ...
    } else {
        // start
        ...
    }
}
```

### Timer display

Replace `self.recording_start.map(|t| t.elapsed())` with `active.clock.elapsed()` so the timer reads directly from the clock and naturally freezes when paused.

Access via `self.session.active.as_ref().map(|a| a.clock.elapsed()).unwrap_or_default()`.

### Button row (bottom_up layout)

```
Recording  → [⏸ Pause]  [⏹ STOP]     (two buttons)
Paused     → [▶ RESUME]              (one button, replaces STOP)
Idle       → [⏺ REC]
```

"PAUSED" label shown above button row when `is_paused`.

## Non-Goals

- Pause indicator in overlay (future)
- Keyboard shortcut display in UI
- Per-track pause
