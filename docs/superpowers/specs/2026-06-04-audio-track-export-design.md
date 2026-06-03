# Audio Track Export — Design Spec

**Date:** 2026-06-04
**Branch:** feat/audio-track-export (to be created)
**Status:** Approved

## Summary

After recording, the user can select a subset of captured audio tracks and export a new `.mp4` containing only those tracks (plus video). Uses Media Foundation SourceReader + SinkWriter in passthrough mode — no re-encode. Output path chosen via native save-file dialog (`rfd`).

## Scope

- New file: `src/encode/remux.rs`
- Modified file: `src/ui/dashboard.rs`
- No changes to capture, session, or config subsystems

## Dependencies

No new crates. `rfd` (already in `Cargo.toml`) provides the save-file dialog.

## Stream Layout

Recorded `.mp4` stream indices (as written by `RecordingWriter`):
- Stream 0: H264 video
- Stream 1: first audio track (AAC)
- Stream 2: second audio track (AAC)
- Stream N: Nth audio track (AAC)

`audio_track_indices[i]` (0-based UI index) maps to MF stream index `i + 1`.

## New File: src/encode/remux.rs

```rust
pub fn remux(
    input: &Path,
    output: &Path,
    audio_track_indices: &[usize],
) -> Result<PathBuf, AppError>
```

### Algorithm

1. `CoInitializeEx(COINIT_MULTITHREADED)` — same pattern as audio capture
2. `MFStartup`
3. `MFCreateSourceReaderFromURL(input)` → `IMFSourceReader`
4. Disable all streams: `SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS, false)`
5. Enable video: `SetStreamSelection(0, true)`
6. Enable selected audio streams: for each `i` in `audio_track_indices`, `SetStreamSelection(i + 1, true)`
7. For each enabled stream, `GetCurrentMediaType` → native compressed type (H264 / AAC)
8. `MFCreateSinkWriterFromURL(output)` → `IMFSinkWriter`
9. Per enabled stream: `AddStream(native_type)` → sink stream index, then `SetInputMediaType(sink_idx, native_type, None)` — matching in/out types triggers passthrough (no re-encode)
10. `BeginWriting`
11. Read/write loop:
    - `ReadSample(MF_SOURCE_READER_ANY_STREAM)` → `(actual_stream_idx, flags, timestamp, sample)`
    - If `MF_SOURCE_READERF_ENDOFSTREAM` flag set for a stream, mark it done
    - Otherwise map `actual_stream_idx` → sink stream index, `WriteSample`
    - Exit loop when all enabled streams are marked done
12. `Finalize`
13. `MFShutdown`

### Empty audio_track_indices

`audio_track_indices` empty → only video stream enabled → output has video, no audio. No special case needed; the loop naturally handles it.

### Error handling

All MF HRESULT failures return `AppError::Encode(format!("...: {e}"))` — consistent with `encode/writer.rs`.

## Dashboard Changes

### New types (top of dashboard.rs)

```rust
enum ExportState {
    Idle,
    Running,
    Done(PathBuf),
    Failed(String),
}
```

### New fields on App

```rust
export_state: ExportState,
export_result_rx: Option<std::sync::mpsc::Receiver<Result<PathBuf, String>>>,
```

Both initialised to `ExportState::Idle` and `None` in `App::new()`.

### update() polling

At the top of `update()`, poll the receiver:
```rust
if let Some(rx) = &self.export_result_rx {
    if let Ok(result) = rx.try_recv() {
        self.export_state = match result {
            Ok(path) => ExportState::Done(path),
            Err(msg) => ExportState::Failed(msg),
        };
        self.export_result_rx = None;
    }
}
```

### Export dialog layout

```
Recording saved:
  /path/to/polyrec_1234.mp4

── AUDIO TRACKS ──
  ☑ 🔊 Speakers …
  ☑ 🎙 Microphone …

[Export]  [Open Folder]  [Close]      ← ExportState::Idle

── Exporting… ──                      ← ExportState::Running
  (buttons disabled)

── Export complete ──                  ← ExportState::Done(path)
  /path/to/chosen_output.mp4
  [Open Folder]  [Close]

── Export failed: <msg> ──            ← ExportState::Failed
  [Close]
```

"Export" button:
- Disabled while `ExportState::Running`
- On click: `rfd::FileDialog::new().add_filter("MP4", &["mp4"]).save_file()` → if `Some(path)`:
  - Collect `audio_track_indices`: indices where `export_track_selection[i] == true`
  - Clone `last_output_path` (source)
  - Create `std::sync::mpsc::channel()`
  - `std::thread::spawn` closure: call `remux(&source, &dest, &indices)`, send `Ok(path)` or `Err(msg)` into tx
  - Store rx in `export_result_rx`, set `export_state = ExportState::Running`

Dialog close resets `export_state = ExportState::Idle` and `export_result_rx = None`.

`ctx.request_repaint_after(Duration::from_millis(100))` while `ExportState::Running` so polling is timely.

## Non-Goals

- Progress percentage (MF passthrough gives no byte-level progress)
- Cancel in-progress export
- Format conversion (always MP4 in, MP4 out)
- Multiple simultaneous exports
