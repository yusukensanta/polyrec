# Audio Track Export Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** After recording, let the user choose a subset of audio tracks and export a new MP4 with only those tracks (plus video), saving to a user-chosen path via native file dialog.

**Architecture:** New `src/encode/remux.rs` wraps MF SourceReader + SinkWriter in passthrough mode (no re-encode). `dashboard.rs` gains `ExportState` enum, two new fields, a result-polling step in `update()`, and an updated export dialog with an Export button wired to `rfd::FileDialog::save_file` + `std::thread::spawn`.

**Tech Stack:** Rust, Windows Media Foundation (`Win32_Media_MediaFoundation`), rfd 0.14 (already in Cargo.toml), egui 0.29

---

## File Map

| File | Change |
|------|--------|
| `src/encode/remux.rs` | **NEW** — `pub fn remux(input, output, audio_track_indices) -> Result<PathBuf, AppError>` |
| `src/encode/mod.rs` | Add `pub mod remux;` |
| `src/ui/dashboard.rs` | Add `ExportState` enum + 2 fields + polling + updated export dialog |

No new crates. No Cargo.toml changes.

---

## Task 1: Create remux.rs skeleton and wire into encode module

**Files:**
- Create: `src/encode/remux.rs`
- Modify: `src/encode/mod.rs`

- [ ] **Step 1: Add module declaration to encode/mod.rs**

In `src/encode/mod.rs`, add `pub mod remux;` so the final file reads:

```rust
pub mod actor;
pub mod remux;
pub mod writer;
pub use writer::RecordingWriter;

use crate::types::{AudioSamples, VideoFrame};

pub enum RecordingCommand {
    WriteVideo(VideoFrame),
    WriteAudio(AudioSamples),
    Stop,
}
```

- [ ] **Step 2: Create remux.rs with a stub**

Create `src/encode/remux.rs`:

```rust
use crate::error::AppError;
use std::path::{Path, PathBuf};

/// Remux `input` into `output`, keeping the video stream and only the
/// audio tracks at the given 0-based indices (matching the order devices
/// were passed to `SessionManager::start_capture`).
/// Empty `audio_track_indices` produces a video-only file.
/// Uses MF SourceReader + SinkWriter passthrough — no re-encode.
pub fn remux(
    _input: &Path,
    _output: &Path,
    _audio_track_indices: &[usize],
) -> Result<PathBuf, AppError> {
    Err(AppError::Encode("remux: not yet implemented".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::RecordingWriter;
    use crate::types::{AudioSamples, TrackId, VideoFrame};
    use std::time::Duration;

    fn make_test_mp4(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("source.mp4");
        let writer =
            RecordingWriter::new(&path, 64, 64, 30, &[(48000u32, 2u16)]).expect("RecordingWriter::new");
        writer.begin_writing().expect("begin_writing");
        writer
            .write_video(VideoFrame {
                pts: Duration::ZERO,
                width: 64,
                height: 64,
                data: vec![0u8; 64 * 64 * 4],
            })
            .expect("write_video");
        writer
            .write_audio(
                0,
                AudioSamples {
                    track_id: TrackId::new(0),
                    pts: Duration::ZERO,
                    samples: vec![0.0f32; 480 * 2],
                    sample_rate: 48000,
                    channels: 2,
                },
            )
            .expect("write_audio");
        writer.finalize().expect("finalize")
    }

    #[test]
    fn remux_video_only_creates_output_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = make_test_mp4(dir.path());
        let dest = dir.path().join("video_only.mp4");
        let result = remux(&source, &dest, &[]);
        assert!(result.is_ok(), "remux failed: {:?}", result.err());
        assert!(dest.exists(), "output file not created");
        assert!(dest.metadata().unwrap().len() > 0, "output file is empty");
    }

    #[test]
    fn remux_with_audio_track_creates_output_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = make_test_mp4(dir.path());
        let dest = dir.path().join("with_audio.mp4");
        let result = remux(&source, &dest, &[0]);
        assert!(result.is_ok(), "remux failed: {:?}", result.err());
        assert!(dest.exists(), "output file not created");
        assert!(dest.metadata().unwrap().len() > 0, "output file is empty");
    }
}
```

- [ ] **Step 3: Verify it compiles (tests will fail — that is expected)**

```powershell
cd "C:\Users\yusuk\work\polyrec\.worktrees\feat-audio-track-export"
cargo build 2>&1 | Select-String "^error" | Select-Object -First 20
```

Expected: no `error[E...]` lines. Stub compiles fine.

- [ ] **Step 4: Run the failing tests to confirm they fail for the right reason**

```powershell
cargo test remux_ -- --nocapture 2>&1 | Select-String -Pattern "FAILED|not yet implemented|panicked"
```

Expected: both tests fail with `"remux: not yet implemented"`.

- [ ] **Step 5: Commit**

```powershell
git add src/encode/mod.rs src/encode/remux.rs
git commit -m "feat: add remux module skeleton with failing tests"
```

---

## Task 2: Implement remux() — MF SourceReader + SinkWriter passthrough

**Files:**
- Modify: `src/encode/remux.rs`

### Background: MF passthrough remux

MF stream layout in the recorded `.mp4`: stream index 0 = H264 video, streams 1…N = AAC audio in capture order.

Passthrough strategy per stream:
1. `GetNativeMediaType(stream_idx, 0)` — get the compressed type (H264 or AAC, already encoded)
2. `SetCurrentMediaType(stream_idx, null, &native_type)` — tells the source reader to emit compressed bytes instead of decoding them
3. On the sink writer: `AddStream(&native_type)` + `SetInputMediaType(sink_idx, &native_type, None)` — matching compressed in/out signals passthrough to MF

- [ ] **Step 1: Replace remux.rs with the full implementation**

Replace the entire contents of `src/encode/remux.rs` with:

```rust
use crate::error::AppError;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::BOOL;
use windows::Win32::Media::MediaFoundation::{
    IMFSample, IMFSinkWriter, IMFSourceReader, MFCreateSinkWriterFromURL,
    MFCreateSourceReaderFromURL, MFShutdown, MFStartup, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_SOURCE_READER_FLAG, MF_VERSION, MFSTARTUP_FULL,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::core::HSTRING;

const MF_SOURCE_READER_ANY_STREAM: u32 = 0xFFFFFFFE;
const MF_SOURCE_READER_ALL_STREAMS: u32 = 0xFFFFFFFE;

pub fn remux(
    input: &Path,
    output: &Path,
    audio_track_indices: &[usize],
) -> Result<PathBuf, AppError> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL)
            .map_err(|e| AppError::Encode(format!("MFStartup: {e}")))?;
        let result = do_remux(input, output, audio_track_indices);
        let _ = MFShutdown();
        result
    }
}

unsafe fn do_remux(
    input: &Path,
    output: &Path,
    audio_track_indices: &[usize],
) -> Result<PathBuf, AppError> {
    let input_url = HSTRING::from(
        input
            .to_str()
            .ok_or_else(|| AppError::Encode("input path not valid UTF-8".into()))?,
    );
    let output_url = HSTRING::from(
        output
            .to_str()
            .ok_or_else(|| AppError::Encode("output path not valid UTF-8".into()))?,
    );

    // ── Source reader ────────────────────────────────────────────────────────
    let reader: IMFSourceReader = MFCreateSourceReaderFromURL(&input_url, None)
        .map_err(|e| AppError::Encode(format!("MFCreateSourceReaderFromURL: {e}")))?;

    // Disable all streams, then re-enable desired ones
    reader
        .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS, BOOL::from(false))
        .map_err(|e| AppError::Encode(format!("SetStreamSelection(all,false): {e}")))?;
    reader
        .SetStreamSelection(0, BOOL::from(true))
        .map_err(|e| AppError::Encode(format!("SetStreamSelection(video): {e}")))?;
    for &idx in audio_track_indices {
        reader
            .SetStreamSelection((idx + 1) as u32, BOOL::from(true))
            .map_err(|e| AppError::Encode(format!("SetStreamSelection(audio {idx}): {e}")))?;
    }

    // Configure each enabled stream for compressed passthrough output.
    // GetNativeMediaType returns the compressed type; SetCurrentMediaType with
    // that same type tells the reader to emit compressed bytes without decoding.
    let video_type = reader
        .GetNativeMediaType(0, 0)
        .map_err(|e| AppError::Encode(format!("GetNativeMediaType(video): {e}")))?;
    reader
        .SetCurrentMediaType(0, std::ptr::null_mut(), &video_type)
        .map_err(|e| AppError::Encode(format!("SetCurrentMediaType(video): {e}")))?;

    let mut audio_types: Vec<(u32, windows::Win32::Media::MediaFoundation::IMFMediaType)> =
        Vec::new();
    for &idx in audio_track_indices {
        let src_idx = (idx + 1) as u32;
        let t = reader
            .GetNativeMediaType(src_idx, 0)
            .map_err(|e| AppError::Encode(format!("GetNativeMediaType(audio {idx}): {e}")))?;
        reader
            .SetCurrentMediaType(src_idx, std::ptr::null_mut(), &t)
            .map_err(|e| AppError::Encode(format!("SetCurrentMediaType(audio {idx}): {e}")))?;
        audio_types.push((src_idx, t));
    }

    // ── Sink writer ──────────────────────────────────────────────────────────
    let writer: IMFSinkWriter = MFCreateSinkWriterFromURL(&output_url, None, None)
        .map_err(|e| AppError::Encode(format!("MFCreateSinkWriterFromURL: {e}")))?;

    // source stream index → sink stream index
    let mut source_to_sink: HashMap<u32, u32> = HashMap::new();

    let vsink = writer
        .AddStream(&video_type)
        .map_err(|e| AppError::Encode(format!("AddStream(video): {e}")))?;
    writer
        .SetInputMediaType(vsink, &video_type, None)
        .map_err(|e| AppError::Encode(format!("SetInputMediaType(video): {e}")))?;
    source_to_sink.insert(0, vsink);

    for (src_idx, audio_type) in &audio_types {
        let asink = writer
            .AddStream(audio_type)
            .map_err(|e| AppError::Encode(format!("AddStream(audio {src_idx}): {e}")))?;
        writer
            .SetInputMediaType(asink, audio_type, None)
            .map_err(|e| AppError::Encode(format!("SetInputMediaType(audio {src_idx}): {e}")))?;
        source_to_sink.insert(*src_idx, asink);
    }

    writer
        .BeginWriting()
        .map_err(|e| AppError::Encode(format!("BeginWriting: {e}")))?;

    // ── Read / write loop ────────────────────────────────────────────────────
    let total_enabled = 1 + audio_track_indices.len();
    let mut done_streams: std::collections::HashSet<u32> = std::collections::HashSet::new();

    loop {
        let mut actual_idx: u32 = 0;
        let mut stream_flags = MF_SOURCE_READER_FLAG(0);
        let mut timestamp: i64 = 0;
        let mut sample: Option<IMFSample> = None;

        reader
            .ReadSample(
                MF_SOURCE_READER_ANY_STREAM,
                0,
                Some(&mut actual_idx),
                Some(&mut stream_flags),
                Some(&mut timestamp),
                Some(&mut sample),
            )
            .map_err(|e| AppError::Encode(format!("ReadSample: {e}")))?;

        if (stream_flags & MF_SOURCE_READERF_ENDOFSTREAM) == MF_SOURCE_READERF_ENDOFSTREAM {
            done_streams.insert(actual_idx);
            if done_streams.len() >= total_enabled {
                break;
            }
            continue;
        }

        if let (Some(s), Some(&sink_idx)) = (sample, source_to_sink.get(&actual_idx)) {
            writer
                .WriteSample(sink_idx, &s)
                .map_err(|e| AppError::Encode(format!("WriteSample(stream {actual_idx}): {e}")))?;
        }
    }

    writer
        .Finalize()
        .map_err(|e| AppError::Encode(format!("Finalize: {e}")))?;

    Ok(output.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::RecordingWriter;
    use crate::types::{AudioSamples, TrackId, VideoFrame};
    use std::time::Duration;

    fn make_test_mp4(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("source.mp4");
        let writer =
            RecordingWriter::new(&path, 64, 64, 30, &[(48000u32, 2u16)]).expect("RecordingWriter::new");
        writer.begin_writing().expect("begin_writing");
        writer
            .write_video(VideoFrame {
                pts: Duration::ZERO,
                width: 64,
                height: 64,
                data: vec![0u8; 64 * 64 * 4],
            })
            .expect("write_video");
        writer
            .write_audio(
                0,
                AudioSamples {
                    track_id: TrackId::new(0),
                    pts: Duration::ZERO,
                    samples: vec![0.0f32; 480 * 2],
                    sample_rate: 48000,
                    channels: 2,
                },
            )
            .expect("write_audio");
        writer.finalize().expect("finalize")
    }

    #[test]
    fn remux_video_only_creates_output_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = make_test_mp4(dir.path());
        let dest = dir.path().join("video_only.mp4");
        let result = remux(&source, &dest, &[]);
        assert!(result.is_ok(), "remux failed: {:?}", result.err());
        assert!(dest.exists(), "output file not created");
        assert!(dest.metadata().unwrap().len() > 0, "output file is empty");
    }

    #[test]
    fn remux_with_audio_track_creates_output_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = make_test_mp4(dir.path());
        let dest = dir.path().join("with_audio.mp4");
        let result = remux(&source, &dest, &[0]);
        assert!(result.is_ok(), "remux failed: {:?}", result.err());
        assert!(dest.exists(), "output file not created");
        assert!(dest.metadata().unwrap().len() > 0, "output file is empty");
    }
}
```

> **Compilation note:** If `BOOL::from(bool)` doesn't compile, replace with `windows::Win32::Foundation::TRUE` / `FALSE`. If `MF_SOURCE_READER_FLAG` operators don't support `&`, use `stream_flags.0 & MF_SOURCE_READERF_ENDOFSTREAM.0 != 0`. Adjust as needed — the overall structure is correct.

- [ ] **Step 2: Verify it compiles**

```powershell
cargo build 2>&1 | Select-String "^error" | Select-Object -First 20
```

Expected: no error lines.

- [ ] **Step 3: Run the remux tests**

```powershell
cargo test remux_ -- --nocapture 2>&1 | tail -15
```

Expected: `remux_video_only_creates_output_file ... ok` and `remux_with_audio_track_creates_output_file ... ok`.

If tests fail with an MF HRESULT error, inspect the error string — it will indicate which API call failed and why. Common issues:
- `MFCreateSourceReaderFromURL` fails on an empty/short MP4 → test helper might need more frames
- Passthrough type mismatch → try removing `SetCurrentMediaType` calls and letting MF decode (accept re-encode for now, document as known limitation)

- [ ] **Step 4: Run full test suite**

```powershell
cargo test 2>&1 | tail -5
```

Expected: all tests pass.

- [ ] **Step 5: Commit**

```powershell
git add src/encode/remux.rs
git commit -m "feat: implement MF passthrough remux for audio track export"
```

---

## Task 3: Add ExportState + new App fields + update() polling

**Files:**
- Modify: `src/ui/dashboard.rs`

- [ ] **Step 1: Add ExportState enum and imports**

In `src/ui/dashboard.rs`, add after the existing `use` block (after line 11 `use std::time::Instant;`):

```rust
use std::sync::mpsc;
```

Add the `ExportState` enum before the `App` struct definition (before `pub struct App {`):

```rust
enum ExportState {
    Idle,
    Running,
    Done(PathBuf),
    Failed(String),
}
```

- [ ] **Step 2: Add fields to App struct**

In the `App` struct, add two fields after `export_track_selection`:

```rust
pub struct App {
    config: Config,
    session: SessionManager,
    sources: Vec<CaptureSource>,
    selected_source: Option<usize>,
    audio_devices: Vec<AudioDevice>,
    selected_audio: Vec<bool>,
    overlay_enabled: bool,
    frame_count: Arc<AtomicU64>,
    recording_start: Option<Instant>,
    last_output_path: Option<PathBuf>,
    output_dir_input: String,
    show_export_dialog: bool,
    export_track_selection: Vec<bool>,
    export_state: ExportState,
    export_result_rx: Option<mpsc::Receiver<Result<PathBuf, String>>>,
    hotkey_listener: HotkeyListener,
}
```

- [ ] **Step 3: Initialize new fields in App::new()**

In `App::new()`, add the two fields to the `Self { ... }` initializer after `export_track_selection`:

```rust
        Self {
            config,
            session: SessionManager::new(),
            sources: enumerate_sources(),
            selected_source: None,
            audio_devices,
            selected_audio,
            overlay_enabled,
            frame_count: Arc::new(AtomicU64::new(0)),
            recording_start: None,
            last_output_path: None,
            output_dir_input,
            show_export_dialog: false,
            export_track_selection,
            export_state: ExportState::Idle,
            export_result_rx: None,
            hotkey_listener,
        }
```

- [ ] **Step 4: Add result polling at the top of update()**

In `update()`, add this block immediately after `let frames = self.frame_count.load(Ordering::Relaxed);`:

```rust
        // Poll export result channel
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

- [ ] **Step 5: Add repaint request when export is running**

At the bottom of `update()`, alongside the existing `is_recording` repaint, add:

```rust
        if is_recording {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
        if matches!(self.export_state, ExportState::Running) {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
```

- [ ] **Step 6: Reset ExportState when dialog closes**

In the export dialog close logic (where `self.show_export_dialog = false` is set), also reset state. Find both places where `show_export_dialog` is set to false and add the reset:

```rust
                if close {
                    self.show_export_dialog = false;
                    self.export_state = ExportState::Idle;
                    self.export_result_rx = None;
                }
            } else {
                self.show_export_dialog = false;
                self.export_state = ExportState::Idle;
                self.export_result_rx = None;
            }
```

- [ ] **Step 7: Verify it compiles**

```powershell
cargo build 2>&1 | Select-String "^error" | Select-Object -First 20
```

Expected: no error lines.

- [ ] **Step 8: Commit**

```powershell
git add src/ui/dashboard.rs
git commit -m "feat: add ExportState and result channel to App"
```

---

## Task 4: Update export dialog UI — Export button + state rendering

**Files:**
- Modify: `src/ui/dashboard.rs`

This task replaces the button row in the export dialog and adds state-based rendering.

- [ ] **Step 1: Add remux import**

In `src/ui/dashboard.rs`, add to the `use` block at the top:

```rust
use crate::encode::remux::remux;
```

- [ ] **Step 2: Replace the export dialog content**

Find this block in the export dialog (currently around lines 402-410):

```rust
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Open Folder").clicked() {
                                open_folder(path.as_ref());
                            }
                            if ui.button("Close").clicked() {
                                close = true;
                            }
                        });
```

Replace it with:

```rust
                        ui.add_space(8.0);

                        match &self.export_state {
                            ExportState::Idle => {
                                ui.horizontal(|ui| {
                                    let export_btn = ui.button("Export");
                                    if export_btn.clicked() {
                                        if let Some(dest) = rfd::FileDialog::new()
                                            .add_filter("MP4 video", &["mp4"])
                                            .set_file_name("export.mp4")
                                            .save_file()
                                        {
                                            let src = path.clone();
                                            let indices: Vec<usize> = self
                                                .export_track_selection
                                                .iter()
                                                .enumerate()
                                                .filter(|(_, &sel)| sel)
                                                .map(|(i, _)| i)
                                                .collect();
                                            let (tx, rx) = mpsc::channel();
                                            std::thread::spawn(move || {
                                                let result = remux(&src, &dest, &indices)
                                                    .map_err(|e| e.to_string());
                                                let _ = tx.send(result);
                                            });
                                            self.export_result_rx = Some(rx);
                                            self.export_state = ExportState::Running;
                                        }
                                    }
                                    if ui.button("Open Folder").clicked() {
                                        open_folder(path.as_ref());
                                    }
                                    if ui.button("Close").clicked() {
                                        close = true;
                                    }
                                });
                            }
                            ExportState::Running => {
                                section_header(ui, "EXPORTING…");
                                ui.label(
                                    egui::RichText::new("Please wait…")
                                        .size(11.0)
                                        .color(TEXT_MUTED),
                                );
                            }
                            ExportState::Done(export_path) => {
                                let export_path = export_path.clone();
                                section_header(ui, "EXPORT COMPLETE");
                                ui.label(
                                    egui::RichText::new(export_path.to_string_lossy().as_ref())
                                        .size(11.0)
                                        .color(TEXT_PRIMARY),
                                );
                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    if ui.button("Open Folder").clicked() {
                                        open_folder(&export_path);
                                    }
                                    if ui.button("Close").clicked() {
                                        close = true;
                                    }
                                });
                            }
                            ExportState::Failed(msg) => {
                                let msg = msg.clone();
                                section_header(ui, "EXPORT FAILED");
                                ui.label(
                                    egui::RichText::new(&msg)
                                        .size(11.0)
                                        .color(egui::Color32::from_rgb(248, 80, 80)),
                                );
                                ui.add_space(4.0);
                                if ui.button("Close").clicked() {
                                    close = true;
                                }
                            }
                        }
```

- [ ] **Step 3: Verify it compiles**

```powershell
cargo build 2>&1 | Select-String "^error" | Select-Object -First 20
```

Expected: no error lines.

- [ ] **Step 4: Run all tests**

```powershell
cargo test 2>&1 | tail -5
```

Expected: all tests pass (43+ total), 0 failed.

- [ ] **Step 5: Commit**

```powershell
git add src/ui/dashboard.rs
git commit -m "feat: wire Export button to remux with state-driven dialog UI"
```

---

## Task 5: Smoke test

- [ ] **Step 1: Run the app**

```powershell
cargo run
```

- [ ] **Step 2: Record a short clip**

Select a source window, press REC, wait ~2 seconds, press STOP. The export dialog should appear.

- [ ] **Step 3: Verify export dialog shows Export button**

Confirm the dialog shows: path label, AUDIO TRACKS section with checkboxes, and an **Export** button alongside Open Folder and Close.

- [ ] **Step 4: Test Export with all tracks**

Click Export → OS save dialog opens → choose a filename → confirm. Dialog should show "EXPORTING…" briefly, then "EXPORT COMPLETE" with the path.

- [ ] **Step 5: Test Export with subset of tracks**

Close dialog, record again. In dialog, uncheck one track. Click Export → save to a different filename. Confirm it completes successfully.

- [ ] **Step 6: Test video-only export**

Uncheck all audio tracks. Click Export. Confirm it produces a file (video-only).

---

## Self-Review

**Spec coverage:**
- ✅ `src/encode/remux.rs` — new file, `remux()` with MF passthrough
- ✅ `src/encode/mod.rs` — `pub mod remux`
- ✅ Output path via `rfd::FileDialog::save_file()`
- ✅ Background thread with `std::thread::spawn`
- ✅ `ExportState` enum (Idle/Running/Done/Failed)
- ✅ `export_state` + `export_result_rx` on App
- ✅ `update()` polling via `try_recv()`
- ✅ `ctx.request_repaint_after(100ms)` while Running
- ✅ State-driven dialog: Idle → Running → Done/Failed
- ✅ Empty audio_track_indices → video-only (no special case needed)
- ✅ ExportState reset on dialog close

**Placeholder scan:** None found.

**Type consistency:**
- `ExportState::Done(PathBuf)` used in Task 3 definition and Task 4 match arm ✓
- `ExportState::Failed(String)` consistent throughout ✓
- `remux()` signature `(&Path, &Path, &[usize])` matches import in Task 4 ✓
- `mpsc::Receiver<Result<PathBuf, String>>` matches channel type in thread spawn ✓
