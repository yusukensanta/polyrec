# Recorder Finalize Race Fix — Design Spec

**Date:** 2026-06-08
**Status:** Approved

## Summary

Fix the race condition where the export dialog appears before the MP4 file is fully written. `stop_capture()` now returns the `JoinHandle` for the recorder task. The dashboard stores it and polls `is_finished()` each frame; only when true does it show the export dialog. A "Saving recording…" label is shown in the meantime.

## Scope

- `src/session/mod.rs` — change `stop_capture()` return type to `Option<JoinHandle<Result<PathBuf, AppError>>>`
- `src/ui/dashboard.rs` — add `finalizing_handle`/`finalizing_path` fields, update `handle_rec_button`, add polling in `update()`, add "Saving" status display

## 1. session/mod.rs

### stop_capture return type

```rust
pub fn stop_capture(&mut self) -> Option<JoinHandle<Result<PathBuf, AppError>>> {
    if let Some(active) = self.active.take() {
        for h in active.capture_handles { h.abort(); }
        for h in active.pump_handles { h.abort(); }
        let _ = active.recording_tx.blocking_send(RecordingCommand::Stop);
        Some(active.recorder_handle)
    } else {
        None
    }
}
```

`recorder_handle` is returned instead of dropped. The caller is responsible for polling it.

## 2. dashboard.rs

### New fields on App

```rust
finalizing_handle: Option<tokio::task::JoinHandle<Result<PathBuf, crate::error::AppError>>>,
finalizing_path: Option<std::path::PathBuf>,
```

Both initialized to `None` in `App::new()`.

### handle_rec_button — stop path

Replace the stop branch. Previously:
```rust
// old — shows export dialog immediately
self.last_output_path = path.clone();
self.export_track_selection = ...;
self.show_export_dialog = path.is_some();
```

New stop branch:
```rust
let path = self.session.active.as_ref().map(|a| a.output_path.clone());
let handle = self.session.stop_capture();
self.finalizing_handle = handle;
self.finalizing_path = path;
self.recording_start = None;
self.frame_count.store(0, Ordering::Relaxed);
self.export_track_selection = self.selected_audio.clone();
// export dialog NOT shown here — polling in update() will trigger it
```

### update() polling

At the top of `update()`, after the export result polling, add:

```rust
// Show export dialog once recorder has finished writing the file
if self.finalizing_handle.as_ref().map_or(false, |h| h.is_finished()) {
    self.finalizing_handle = None;   // drop — task already done, no abort occurs
    self.last_output_path = self.finalizing_path.take();
    self.show_export_dialog = self.last_output_path.is_some();
}
```

### STATUS section — "Saving" state

In the center panel STATUS section, the current condition is `if is_recording || is_paused`. Add an `else if` branch for finalizing before the `else if last_output_path` branch:

```rust
if is_recording || is_paused {
    // ... existing recording/paused display ...
} else if self.finalizing_handle.is_some() {
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new("Saving recording…")
            .size(13.0)
            .color(TEXT_MUTED),
    );
} else if let Some(path) = &self.last_output_path {
    // ... existing last recording display ...
} else {
    // ... idle prompt ...
}
```

### Repaint while finalizing

```rust
if self.finalizing_handle.is_some() {
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
}
```

## Non-Goals

- Displaying finalization errors to the user (logged via tracing; export dialog appears regardless since we have the path from ActiveCapture)
- Progress percentage during finalization
- Cancelling an in-progress finalization
