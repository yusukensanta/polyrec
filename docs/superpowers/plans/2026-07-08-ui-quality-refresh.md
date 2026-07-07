# UI + Quality Settings Refresh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expose FPS/codec/resolution-mode/bitrate as real, wired-through settings in a new Quality popup, and refresh the dashboard (source-list icons, audio-panel tooltip, icon-matched accent color) per `docs/superpowers/specs/2026-07-08-ui-quality-refresh-design.md`.

**Architecture:** Config gains flat string fields with pure parsing methods that fall back to safe defaults on bad input. Those resolved values flow through a new `EncodeSettings` struct: `dashboard.rs` builds it from config → `SessionManager::start_capture` resolves the actual output resolution/bitrate (querying the display only when explicitly asked) → `spawn_recording_actor`/`RecordingWriter::new` apply codec + bitrate, falling back from HEVC to H264 if the encoder MFT isn't available.

**Tech Stack:** Rust, egui/eframe, Windows Media Foundation (`windows` crate 0.58), WASAPI.

## Global Constraints

- Windows-only codebase (`windows` crate 0.58, MSVC target) — every new API call needs its feature flag added to `Cargo.toml`, never assumed present.
- `resolution_mode="display"` and `bitrate_mode="manual"` are **opt-in only** — the default config (`native`/`auto`) must reproduce exactly today's behavior (window-native resolution, resolution-aware bitrate). Do not let any step silently flip these defaults.
- Follow existing project conventions: hardware/GUI-dependent tests are `#[ignore]`d `#[tokio::test]`s run manually with `--ignored --nocapture`; pure-logic tests are always-on `#[test]`s in the same file's `#[cfg(test)] mod tests`.
- `cargo build` and `cargo test --bin polyrec` (non-ignored) must stay green after every task.

---

### Task 1: `EncodeConfig` — resolution/bitrate modes

**Files:**
- Modify: `src/config.rs:1` (imports), `src/config.rs:26-31` (`EncodeConfig` struct), `src/config.rs:46-49` (`Default` construction)
- Test: `src/config.rs` `#[cfg(test)] mod tests` (existing, at end of file)

**Interfaces:**
- Produces: `pub enum ResolutionMode { Native, Display, Custom(u32, u32) }`, `pub enum BitrateMode { Auto, Manual(u32) }` (the `u32` in `Manual` is already-resolved **bits per second**, not Mbps), `impl EncodeConfig { pub fn resolution_mode(&self) -> ResolutionMode; pub fn bitrate_mode(&self) -> BitrateMode }`.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block at the bottom of `src/config.rs` (after the existing `save_and_load_roundtrip` test):

```rust
    #[test]
    fn resolution_mode_parses_known_strings() {
        let mut c = Config::default();
        c.encode.resolution_mode = "native".into();
        assert!(matches!(c.encode.resolution_mode(), ResolutionMode::Native));
        c.encode.resolution_mode = "display".into();
        assert!(matches!(c.encode.resolution_mode(), ResolutionMode::Display));
        c.encode.resolution_mode = "custom".into();
        c.encode.custom_width = 2560;
        c.encode.custom_height = 1440;
        assert!(matches!(c.encode.resolution_mode(), ResolutionMode::Custom(2560, 1440)));
    }

    #[test]
    fn resolution_mode_unknown_string_falls_back_to_native() {
        let mut c = Config::default();
        c.encode.resolution_mode = "not-a-real-mode".into();
        assert!(matches!(c.encode.resolution_mode(), ResolutionMode::Native));
    }

    #[test]
    fn resolution_mode_custom_clamps_to_even_and_bounds() {
        let mut c = Config::default();
        c.encode.resolution_mode = "custom".into();
        c.encode.custom_width = 1;
        c.encode.custom_height = 100_000;
        match c.encode.resolution_mode() {
            ResolutionMode::Custom(w, h) => {
                assert_eq!(w, 2, "width below minimum should clamp to 2");
                assert_eq!(h, 4320, "height above maximum should clamp to 4320");
                assert_eq!(w % 2, 0);
                assert_eq!(h % 2, 0);
            }
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    #[test]
    fn bitrate_mode_parses_known_strings() {
        let mut c = Config::default();
        c.encode.bitrate_mode = "auto".into();
        assert!(matches!(c.encode.bitrate_mode(), BitrateMode::Auto));
        c.encode.bitrate_mode = "manual".into();
        c.encode.manual_bitrate_mbps = 20;
        assert!(matches!(c.encode.bitrate_mode(), BitrateMode::Manual(20_000_000)));
    }

    #[test]
    fn bitrate_mode_unknown_string_falls_back_to_auto() {
        let mut c = Config::default();
        c.encode.bitrate_mode = "not-a-real-mode".into();
        assert!(matches!(c.encode.bitrate_mode(), BitrateMode::Auto));
    }

    #[test]
    fn bitrate_mode_manual_clamps_to_1_100_mbps() {
        let mut c = Config::default();
        c.encode.bitrate_mode = "manual".into();
        c.encode.manual_bitrate_mbps = 0;
        assert!(matches!(c.encode.bitrate_mode(), BitrateMode::Manual(1_000_000)));
        c.encode.manual_bitrate_mbps = 500;
        assert!(matches!(c.encode.bitrate_mode(), BitrateMode::Manual(100_000_000)));
    }

    #[test]
    fn encode_config_new_fields_round_trip_toml() {
        let mut original = Config::default();
        original.encode.resolution_mode = "custom".into();
        original.encode.custom_width = 1280;
        original.encode.custom_height = 720;
        original.encode.bitrate_mode = "manual".into();
        original.encode.manual_bitrate_mbps = 15;
        let text = toml::to_string_pretty(&original).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed.encode, original.encode);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin polyrec config:: 2>&1 | tail -30`
Expected: FAIL to compile — `ResolutionMode`, `BitrateMode`, `resolution_mode()`, `bitrate_mode()` not found, and `EncodeConfig` missing fields `resolution_mode`/`custom_width`/`custom_height`/`bitrate_mode`/`manual_bitrate_mbps`.

- [ ] **Step 3: Extend `EncodeConfig` and add the mode types + methods**

Replace the `EncodeConfig` struct (`src/config.rs:26-31`):

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EncodeConfig {
    /// "h265" or "h264"
    pub codec: String,
    pub fps: u32,
    /// "native" (window's own size, default) | "display" | "custom"
    pub resolution_mode: String,
    /// Only read when `resolution_mode == "custom"`.
    pub custom_width: u32,
    pub custom_height: u32,
    /// "auto" (resolution-aware formula, default) | "manual"
    pub bitrate_mode: String,
    /// Only read when `bitrate_mode == "manual"`.
    pub manual_bitrate_mbps: u32,
}

/// Resolved form of `EncodeConfig::resolution_mode` — see `EncodeConfig::resolution_mode()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionMode {
    /// The captured window's own native size (default — see the resolution-regression fix).
    Native,
    /// The display's native resolution — explicit opt-in only, not the default.
    Display,
    /// An explicit width/height, already clamped to even and within [2, 7680]x[2, 4320].
    Custom(u32, u32),
}

/// Resolved form of `EncodeConfig::bitrate_mode` — see `EncodeConfig::bitrate_mode()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitrateMode {
    /// Resolution-aware formula (see `encode::writer::video_bitrate_bps`).
    Auto,
    /// Explicit bits-per-second, already clamped from a 1-100 Mbps user input.
    Manual(u32),
}

impl EncodeConfig {
    pub fn resolution_mode(&self) -> ResolutionMode {
        match self.resolution_mode.as_str() {
            "display" => ResolutionMode::Display,
            "custom" => ResolutionMode::Custom(
                self.custom_width.clamp(2, 7680) & !1,
                self.custom_height.clamp(2, 4320) & !1,
            ),
            _ => ResolutionMode::Native,
        }
    }

    pub fn bitrate_mode(&self) -> BitrateMode {
        match self.bitrate_mode.as_str() {
            "manual" => BitrateMode::Manual(self.manual_bitrate_mbps.clamp(1, 100) * 1_000_000),
            _ => BitrateMode::Auto,
        }
    }
}
```

Update the `Default for Config` block's `encode` field (`src/config.rs:46-49`):

```rust
            encode: EncodeConfig {
                codec: "h265".into(),
                fps: 60,
                resolution_mode: "native".into(),
                custom_width: 1920,
                custom_height: 1080,
                bitrate_mode: "auto".into(),
                manual_bitrate_mbps: 12,
            },
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin polyrec config:: 2>&1 | tail -30`
Expected: PASS — all `config::tests::*` including the 7 new ones.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "$(cat <<'EOF'
feat: add resolution/bitrate mode settings to EncodeConfig

Flat string fields (not tagged enums) so an old or hand-edited
config.toml never fails to parse -- unknown values fall back to
native/auto rather than erroring.
EOF
)"
```

---

### Task 2: Cargo.toml features + restore `query_display_size`

**Files:**
- Modify: `Cargo.toml` (windows feature list)
- Modify: `src/capture/video.rs:1-13` (imports), insert restored function after `query_capture_size`
- Test: `src/capture/video.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `pub fn query_display_size(hwnd: HWND) -> Result<(u32, u32), AppError>` (same signature it had before removal).

- [ ] **Step 1: Add the Windows features this plan needs**

`Win32_Graphics_Gdi` was removed by the earlier resolution fix (now needed again for `query_display_size` and for icon extraction in Task 4); `Win32_UI_Shell` and `Win32_Storage_FileSystem` are new, needed by Task 4's icon extraction. Adding all three now avoids touching `Cargo.toml` twice.

In `Cargo.toml`, inside the `[dependencies.windows]` `features = [...]` list, after `"Win32_UI_Input_KeyboardAndMouse",`:

```toml
    "Win32_UI_Input_KeyboardAndMouse",
    "Win32_Graphics_Gdi",
    "Win32_UI_Shell",
    "Win32_Storage_FileSystem",
]
```

- [ ] **Step 2: Write the failing test**

Add to `src/capture/video.rs`'s `#[cfg(test)] mod tests` block (after `scale_bgra_downscales`):

```rust
    #[test]
    fn query_display_size_returns_positive_dimensions() {
        // Any visible top-level window works — GetDesktopWindow's monitor is always valid.
        let hwnd = unsafe { windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow() };
        let (w, h) = query_display_size(hwnd).expect("query_display_size failed");
        assert!(w > 0 && h > 0, "expected positive display dimensions, got {w}x{h}");
    }
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --bin polyrec capture::video:: 2>&1 | tail -20`
Expected: FAIL to compile — `query_display_size` not found, `GetDesktopWindow` not imported.

- [ ] **Step 4: Restore `query_display_size` and its imports**

In `src/capture/video.rs`, restore the Gdi import (it was deleted alongside the function) — after the existing `use windows::core::Interface;` line (`src/capture/video.rs:23`):

```rust
use windows::core::Interface;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
```

Insert this function right after `query_capture_size` (before the `scale_bgra` doc comment):

```rust
/// Query the resolution of the monitor a window is on. Only used when the user
/// explicitly picks resolution_mode = "display" — NOT the default (see the
/// resolution-regression fix: forcing this by default caused nearest-neighbor
/// upscale artifacts combined with an under-provisioned bitrate).
pub fn query_display_size(hwnd: HWND) -> Result<(u32, u32), AppError> {
    unsafe {
        let hmonitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(hmonitor, &mut info).as_bool() {
            return Err(AppError::Capture("GetMonitorInfoW failed".into()));
        }
        let w = (info.rcMonitor.right - info.rcMonitor.left) as u32;
        let h = (info.rcMonitor.bottom - info.rcMonitor.top) as u32;
        Ok((w, h))
    }
}
```

Add the `GetDesktopWindow` import inside the test module itself (keep it test-only — production code never needs the desktop window):

```rust
    #[test]
    fn query_display_size_returns_positive_dimensions() {
        use windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow;
        let hwnd = unsafe { GetDesktopWindow() };
        let (w, h) = query_display_size(hwnd).expect("query_display_size failed");
        assert!(w > 0 && h > 0, "expected positive display dimensions, got {w}x{h}");
    }
```

(This replaces the version written in Step 2 — same test, just with the import moved inline.)

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo build 2>&1 | tail -20 && cargo test --bin polyrec capture::video:: 2>&1 | tail -20`
Expected: clean build, PASS for `query_display_size_returns_positive_dimensions` and all existing `scale_bgra_*` tests.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml src/capture/video.rs
git commit -m "$(cat <<'EOF'
feat: restore query_display_size as an explicit opt-in

Brings back the display-resolution query removed by the earlier
regression fix, now only invoked when the user explicitly picks
resolution_mode="display" in the new Quality settings (Task 3+5).
Also pre-adds the Shell/FileSystem Windows features Task 4 needs.
EOF
)"
```

---

### Task 3: Encoder codec/bitrate + `EncodeSettings` wiring

This task spans three files (`writer.rs`, `actor.rs`, `session/mod.rs`) as one unit rather than three separate tasks: `RecordingWriter::new`'s signature, its one caller (`spawn_recording_actor`), and *that* function's one caller (`SessionManager::start_capture`) only compile together — splitting them into separately-committed tasks would mean an intermediate task leaves the build broken, which violates the global constraint that `cargo build` stays green after every task.

**Files:**
- Modify: `src/encode/writer.rs:1-27` (imports), `src/encode/writer.rs:24` (`video_bitrate_bps` visibility), `src/encode/writer.rs:37-98` (`RecordingWriter::new`), `src/encode/writer.rs:165-189` (`make_video_output_type`), `src/encode/writer.rs:338-374` (existing `writer_creates_mp4_for_tiny_resolution` test call site)
- Modify: `src/encode/actor.rs:12-25` (`spawn_recording_actor` signature + body)
- Modify: `src/session/mod.rs:1-22` (imports/consts), `src/session/mod.rs:24-32` (add `EncodeSettings` near `ActiveCapture`), `src/session/mod.rs:63-191` (`start_capture`), `src/session/mod.rs:257-539` (4 existing test call sites)
- Test: `src/encode/writer.rs` and `src/session/mod.rs`, both existing `#[cfg(test)] mod tests` blocks

**Interfaces:**
- Consumes: `crate::config::{ResolutionMode, BitrateMode}` and their `EncodeConfig::resolution_mode()`/`bitrate_mode()` methods (Task 1), `crate::capture::video::query_display_size` (Task 2).
- Produces: `RecordingWriter::new(output_path: &Path, width: u32, height: u32, fps: u32, codec: &str, bitrate_bps: u32, audio_tracks: &[(u32, u16)]) -> Result<Self, AppError>`. `pub(crate) fn video_bitrate_bps(width, height, fps) -> u32` (was private `fn`). `spawn_recording_actor(output_path: PathBuf, width: u32, height: u32, fps: u32, codec: String, bitrate_bps: u32, audio_device_specs: Vec<(u32, u16)>) -> (mpsc::Sender<RecordingCommand>, JoinHandle<Result<PathBuf, AppError>>)`. `pub struct EncodeSettings { pub codec: String, pub fps: u32, pub resolution_mode: ResolutionMode, pub bitrate_mode: BitrateMode }` with `impl Default` (mirrors `Config::default().encode`). `SessionManager::start_capture(&mut self, source, audio_devices, app_audio_only, frame_count, output_dir, encode: EncodeSettings) -> PathBuf` — Task 5 (the dashboard task) builds the `EncodeSettings` this expects.

- [ ] **Step 1: Write the failing tests**

Add to `src/encode/writer.rs`'s `#[cfg(test)] mod tests` (after `video_bitrate_scales_with_resolution_and_fps`):

```rust
    #[test]
    fn writer_accepts_explicit_bitrate_and_h264_codec() {
        use std::time::Duration;
        use crate::types::VideoFrame;

        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("h264_test.mp4");
        let writer = RecordingWriter::new(&output, 64, 64, 30, "h264", 500_000, &[])
            .expect("RecordingWriter::new with explicit bitrate failed");
        writer.begin_writing().expect("begin_writing failed");
        writer
            .write_video(VideoFrame { pts: Duration::ZERO, data: vec![0u8; 64 * 64 * 4] })
            .expect("write_video failed");
        let path = writer.finalize().expect("finalize failed");
        assert!(path.metadata().unwrap().len() > 0, "output file is empty");
    }

    /// HEVC MFT availability varies by Windows version/install — this exercises the
    /// real encoder (or its automatic H264 fallback) rather than asserting a specific
    /// codec landed in the file, since either outcome is a pass for this codebase.
    #[tokio::test]
    #[ignore]
    async fn writer_accepts_h265_codec_or_falls_back_cleanly() {
        use std::time::Duration;
        use crate::types::VideoFrame;

        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("h265_test.mp4");
        let writer = RecordingWriter::new(&output, 64, 64, 30, "h265", 500_000, &[])
            .expect("RecordingWriter::new with h265 (or its fallback) failed");
        writer.begin_writing().expect("begin_writing failed");
        writer
            .write_video(VideoFrame { pts: Duration::ZERO, data: vec![0u8; 64 * 64 * 4] })
            .expect("write_video failed");
        let path = writer.finalize().expect("finalize failed");
        assert!(path.metadata().unwrap().len() > 0, "output file is empty");
    }
```

Update the existing `writer_creates_mp4_for_tiny_resolution` test's constructor call (`src/encode/writer.rs:348`, inside that test) — change:

```rust
        let writer = RecordingWriter::new(&output, 64, 64, 30, &audio_tracks);
```

to:

```rust
        let writer = RecordingWriter::new(&output, 64, 64, 30, "h264", 500_000, &audio_tracks);
```

Also add to `src/session/mod.rs`'s `#[cfg(test)] mod tests` (after `make_output_path_has_mp4_extension`, before `recording_resolution_matches_window`):

```rust
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
```

Also update the 4 existing test call sites that invoke `sm.start_capture(...)` — in each of `recording_resolution_matches_window`, `full_capture_produces_nonempty_file`, `diag_recorded_audio_track_has_signal`, and `full_capture_app_audio_only_produces_nonempty_file`, add `EncodeSettings::default()` as a 6th argument, e.g. (`src/session/mod.rs:322`):

```rust
        sm.start_capture(source, audio_devices, false, Arc::clone(&frame_count), dir.path(), EncodeSettings::default());
```

(and the analogous 3 other call sites — same pattern, just append `, EncodeSettings::default()` before the closing `)`). These calls won't compile yet (`EncodeSettings` doesn't exist until Step 5) — that's expected for this step.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin polyrec encode::writer:: session:: 2>&1 | tail -40`
Expected: FAIL to compile — `RecordingWriter::new` called with wrong argument count, and `resolve_output_size`/`EncodeSettings` not found.

- [ ] **Step 3: Implement the codec parameter + fallback**

Add `MFVideoFormat_HEVC` to the imports at the top of `src/encode/writer.rs` (line 7, in the existing `use windows::Win32::Media::MediaFoundation::{...}` block) — change:

```rust
    MFMediaType_Video, MFShutdown, MFStartup, MFVideoFormat_ARGB32, MFVideoFormat_H264,
```

to:

```rust
    MFMediaType_Video, MFShutdown, MFStartup, MFVideoFormat_ARGB32, MFVideoFormat_H264,
    MFVideoFormat_HEVC,
```

Also import `windows_core::GUID` (used as the `subtype` param type) — add near the top:

```rust
use windows::core::GUID;
```

Replace `RecordingWriter::new`'s signature and video-stream setup (`src/encode/writer.rs:38-75`):

```rust
    pub fn new(
        output_path: &Path,
        width: u32,
        height: u32,
        fps: u32,
        codec: &str,
        bitrate_bps: u32,
        audio_tracks: &[(u32, u16)],
    ) -> Result<Self, AppError> {
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_FULL)
                .map_err(|e| AppError::Encode(format!("MFStartup: {e}")))?;

            let path_str = output_path
                .to_str()
                .ok_or_else(|| AppError::Encode("output path is not valid UTF-8".into()))?;
            let url = HSTRING::from(path_str);
            // Create attributes to force software encoding (needed for CI/headless test environments)
            use windows::Win32::Media::MediaFoundation::{MFCreateAttributes, MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS};
            use windows::Win32::Media::MediaFoundation::IMFAttributes;
            let mut attrs_opt: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs_opt, 1)
                .map_err(|e| AppError::Encode(format!("MFCreateAttributes: {e}")))?;
            let attrs = attrs_opt
                .ok_or_else(|| AppError::Encode("MFCreateAttributes returned None".into()))?;
            attrs
                .SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 0)
                .map_err(|e| AppError::Encode(format!("SetUINT32 hw_transforms: {e}")))?;
            let writer: IMFSinkWriter =
                MFCreateSinkWriterFromURL(&url, None, Some(&attrs))
                    .map_err(|e| AppError::Encode(format!("MFCreateSinkWriterFromURL: {e}")))?;

            let requested_subtype = if codec == "h265" { MFVideoFormat_HEVC } else { MFVideoFormat_H264 };
            let video_out = make_video_output_type(width, height, fps, requested_subtype, bitrate_bps)?;
            let video_stream = match writer.AddStream(&video_out) {
                Ok(idx) => idx,
                Err(e) if requested_subtype == MFVideoFormat_HEVC => {
                    tracing::warn!("HEVC AddStream failed ({e}), falling back to H264");
                    let fallback_out = make_video_output_type(width, height, fps, MFVideoFormat_H264, bitrate_bps)?;
                    writer
                        .AddStream(&fallback_out)
                        .map_err(|e| AppError::Encode(format!("AddStream video (H264 fallback): {e}")))?
                }
                Err(e) => return Err(AppError::Encode(format!("AddStream video: {e}"))),
            };
            let video_in = make_video_input_type(width, height, fps)?;
            writer
                .SetInputMediaType(video_stream, &video_in, None)
                .map_err(|e| AppError::Encode(format!("SetInputMediaType video: {e}")))?;

            let mut audio_streams = Vec::new();
            for (sample_rate, channels) in audio_tracks {
                let audio_out = make_audio_output_type(*sample_rate, *channels)?;
                let audio_in = make_audio_input_type(*sample_rate, *channels)?;
                let idx = writer
                    .AddStream(&audio_out)
                    .map_err(|e| AppError::Encode(format!("AddStream audio: {e}")))?;
                writer
                    .SetInputMediaType(idx, &audio_in, None)
                    .map_err(|e| AppError::Encode(format!("SetInputMediaType audio: {e}")))?;
                audio_streams.push(idx);
            }

            Ok(Self {
                writer,
                video_stream,
                audio_streams,
                output_path: output_path.to_path_buf(),
                fps,
            })
        }
    }
```

Replace `make_video_output_type` (`src/encode/writer.rs:165-189`) to take the subtype and bitrate as parameters instead of hardcoding them:

```rust
unsafe fn make_video_output_type(
    width: u32,
    height: u32,
    fps: u32,
    subtype: GUID,
    bitrate_bps: u32,
) -> Result<IMFMediaType, AppError> {
    let t =
        MFCreateMediaType().map_err(|e| AppError::Encode(format!("MFCreateMediaType: {e}")))?;
    t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
        .map_err(|e| AppError::Encode(format!("SetGUID MajorType: {e}")))?;
    t.SetGUID(&MF_MT_SUBTYPE, &subtype)
        .map_err(|e| AppError::Encode(format!("SetGUID subtype: {e}")))?;
    t.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))
        .map_err(|e| AppError::Encode(format!("SetUINT64 frame_size: {e}")))?;
    t.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))
        .map_err(|e| AppError::Encode(format!("SetUINT64 frame_rate: {e}")))?;
    t.SetUINT32(&MF_MT_AVG_BITRATE, bitrate_bps)
        .map_err(|e| AppError::Encode(format!("SetUINT32 bitrate: {e}")))?;
    // MFVideoInterlace_Progressive = MFVideoInterlaceMode(2i32)
    t.SetUINT32(
        &MF_MT_INTERLACE_MODE,
        MFVideoInterlace_Progressive.0 as u32,
    )
    .map_err(|e| AppError::Encode(format!("SetUINT32 interlace: {e}")))?;
    Ok(t)
}
```

Also change `video_bitrate_bps`'s visibility (`src/encode/writer.rs:24`) so `session/mod.rs` (Step 5 below) can call it — its existing test and formula are otherwise untouched:

```rust
pub(crate) fn video_bitrate_bps(width: u32, height: u32, fps: u32) -> u32 {
```

- [ ] **Step 4: Implement `spawn_recording_actor`'s signature change**

Replace `spawn_recording_actor` (`src/encode/actor.rs:12-25`):

```rust
pub fn spawn_recording_actor(
    output_path: PathBuf,
    width: u32,
    height: u32,
    fps: u32,
    codec: String,
    bitrate_bps: u32,
    audio_device_specs: Vec<(u32, u16)>,
) -> (
    mpsc::Sender<RecordingCommand>,
    JoinHandle<Result<PathBuf, AppError>>,
) {
    let (tx, mut rx) = mpsc::channel::<RecordingCommand>(256);

    let handle = tokio::task::spawn_blocking(move || {
        let writer = RecordingWriter::new(&output_path, width, height, fps, &codec, bitrate_bps, &audio_device_specs)?;
        writer.begin_writing()?;
```

(The rest of the function body — the `while let Some(cmd) = rx.blocking_recv()` loop — is unchanged.)

This file has no unit tests calling `spawn_recording_actor` directly (its tests exercise `spawn_video_pump`/`spawn_audio_pump` instead), so there's no test call site to update here.

- [ ] **Step 5: Add `EncodeSettings` and `resolve_output_size` to `session/mod.rs`, wire `start_capture`**

Update imports at the top of `src/session/mod.rs` (replace lines 4-9):

```rust
use crate::capture::audio::{
    run_audio_capture, run_process_loopback_capture, TARGET_CHANNELS, TARGET_SAMPLE_RATE,
};
use crate::capture::video::{query_capture_size, query_display_size, run_video_capture};
use crate::config::{BitrateMode, ResolutionMode};
use crate::encode::actor::{spawn_audio_pump, spawn_recording_actor, spawn_video_pump};
use crate::encode::writer::video_bitrate_bps;
use crate::encode::RecordingCommand;
```

Remove the now-unused `RECORDING_FPS` const (`src/session/mod.rs:22`) — `fps` now comes from `EncodeSettings` — and add `EncodeSettings` next to `ActiveCapture` (`src/session/mod.rs:24-32`):

```rust
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
        Self {
            codec: d.codec,
            fps: d.fps,
            resolution_mode: d.resolution_mode(),
            bitrate_mode: d.bitrate_mode(),
        }
    }
}

pub struct ActiveCapture {
```

Replace `start_capture`'s signature and the resolution/bitrate/spawn section (`src/session/mod.rs:63-114`):

```rust
    pub fn start_capture(
        &mut self,
        source: CaptureSource,
        audio_devices: Vec<AudioDevice>,
        app_audio_only: bool,
        frame_count: Arc<AtomicU64>,
        output_dir: &std::path::Path,
        encode: EncodeSettings,
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
        let (recording_tx, recorder_handle) = spawn_recording_actor(
            output_path.clone(),
            output_width,
            output_height,
            encode.fps,
            encode.codec.clone(),
            bitrate_bps,
            audio_specs,
        );
```

The rest of `start_capture` (video/audio capture + pump spawning, `self.active = Some(...)`) is unchanged — it doesn't reference `RECORDING_FPS` or the old resolution logic.

Add `resolve_output_size` as a free function near `make_output_path` (after it, `src/session/mod.rs:255` area):

```rust
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
```

(The 4 test call sites were already updated in Step 1, when `EncodeSettings` didn't exist yet — they'll compile now.)

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo build 2>&1 | tail -30 && cargo test --bin polyrec 2>&1 | tail -20`
Expected: clean build; all previously-passing non-ignored tests still pass (51+), plus the 2 new `writer::tests::*` and 5 new `session::tests::*` from Step 1. Then: `cargo test --bin polyrec writer_accepts_h265 -- --ignored --nocapture` — expect PASS (either real HEVC or the logged fallback, both produce a valid file).

- [ ] **Step 7: Commit**

```bash
git add src/encode/writer.rs src/encode/actor.rs src/session/mod.rs
git commit -m "$(cat <<'EOF'
feat: wire codec/bitrate/resolution settings through the record path

RecordingWriter now takes an explicit codec (with automatic HEVC ->
H264 fallback) and bitrate instead of hardcoding both. SessionManager
::start_capture takes an EncodeSettings resolved by the caller;
resolution-mode and bitrate-mode resolution are pure, unit-tested
functions. query_display_size is only called when resolution_mode
== Display (explicit opt-in), so the Native default's behavior (and
the earlier blur/artifact fix) is unchanged.
EOF
)"
```

---

### Task 4: Source-list exe icons

**Files:**
- Modify: `src/types.rs:19-25` (`CaptureSource`)
- Modify: `src/sources.rs` (icon extraction + wiring)
- Test: `src/sources.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `CaptureSource.icon_rgba: Option<(Vec<u8>, u32, u32)>` (RGBA bytes, width, height — top-down, straight/unpremultiplied alpha as returned by `GetDIBits`).

- [ ] **Step 1: Add the field**

In `src/types.rs:19-25`, add the field to `CaptureSource`:

```rust
#[derive(Debug, Clone)]
pub struct CaptureSource {
    pub process_id: u32,
    pub window_title: String,
    pub exe_name: String,
    pub hwnd: usize,
    /// (RGBA bytes, width, height) of the source exe's small icon, if extractable.
    pub icon_rgba: Option<(Vec<u8>, u32, u32)>,
}
```

- [ ] **Step 2: Write the failing test**

Add to `src/sources.rs`'s `#[cfg(test)] mod tests` (after `capture_sources_have_nonzero_hwnd`):

```rust
    #[test]
    fn at_least_one_source_has_an_extractable_icon() {
        // Best-effort: some system/shell windows won't resolve to a real exe path,
        // but on any normal desktop at least one visible window's exe icon extracts.
        let sources = enumerate_sources();
        assert!(
            sources.iter().any(|s| s.icon_rgba.is_some()),
            "expected at least one source with an extractable icon"
        );
    }

    #[test]
    fn extracted_icon_has_matching_buffer_length() {
        let sources = enumerate_sources();
        for s in &sources {
            if let Some((rgba, w, h)) = &s.icon_rgba {
                assert_eq!(rgba.len(), (*w * *h * 4) as usize, "RGBA buffer length must be width*height*4");
            }
        }
    }
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --bin polyrec sources:: 2>&1 | tail -30`
Expected: FAIL to compile — `CaptureSource` literal in `enum_window_callback` missing the new `icon_rgba` field (compile error), so these new tests can't even build yet.

- [ ] **Step 4: Implement icon extraction**

Replace `src/sources.rs` in full:

```rust
use crate::types::CaptureSource;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, DIB_RGB_COLORS,
};
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::System::ProcessStatus::GetModuleFileNameExW;
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ};
use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_SMALLICON};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, EnumWindows, GetIconInfo, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsWindowVisible, ICONINFO,
};
use windows::core::PCWSTR;

pub fn enumerate_sources() -> Vec<CaptureSource> {
    let mut sources: Vec<CaptureSource> = Vec::new();
    let sources_ptr = &mut sources as *mut Vec<CaptureSource> as isize;

    unsafe {
        let _ = EnumWindows(Some(enum_window_callback), LPARAM(sources_ptr));
    }

    sources
}

unsafe extern "system" fn enum_window_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let sources = &mut *(lparam.0 as *mut Vec<CaptureSource>);

    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1);
    }

    let title_len = GetWindowTextLengthW(hwnd);
    if title_len == 0 {
        return BOOL(1);
    }

    let mut title_buf = vec![0u16; (title_len + 1) as usize];
    GetWindowTextW(hwnd, &mut title_buf);
    let window_title = String::from_utf16_lossy(&title_buf[..title_len as usize]);

    let mut process_id = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut process_id));

    let exe_path = get_exe_path(process_id);
    let exe_name = exe_path
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_name())
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "Unknown".into());
    let icon_rgba = exe_path.as_deref().and_then(extract_exe_icon_rgba);

    sources.push(CaptureSource {
        process_id,
        window_title,
        exe_name,
        hwnd: hwnd.0 as usize,
        icon_rgba,
    });

    BOOL(1)
}

fn get_exe_path(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid).ok()?;
        let mut buf = vec![0u16; 260];
        let len = GetModuleFileNameExW(handle, None, &mut buf);
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        if len == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// Extracts the exe's small shell icon as straight-alpha, top-down RGBA bytes.
/// Best-effort: returns `None` on any failure rather than propagating an error —
/// a missing icon just means the source list row shows no icon, not a broken list.
fn extract_exe_icon_rgba(exe_path: &str) -> Option<(Vec<u8>, u32, u32)> {
    unsafe {
        let wide: Vec<u16> = exe_path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut info = SHFILEINFOW::default();
        let result = SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_SMALLICON,
        );
        if result == 0 || info.hIcon.is_invalid() {
            return None;
        }
        let hicon = info.hIcon;

        let mut icon_info = ICONINFO::default();
        if GetIconInfo(hicon, &mut icon_info).is_err() {
            let _ = DestroyIcon(hicon);
            return None;
        }
        let _ = DeleteObject(icon_info.hbmMask);

        let mut bmp = BITMAP::default();
        let bmp_size = std::mem::size_of::<BITMAP>() as i32;
        if GetObjectW(icon_info.hbmColor, bmp_size, Some(&mut bmp as *mut _ as *mut _)) == 0 {
            let _ = DeleteObject(icon_info.hbmColor);
            let _ = DestroyIcon(hicon);
            return None;
        }
        let width = bmp.bmWidth as u32;
        let height = bmp.bmHeight as u32;
        if width == 0 || height == 0 {
            let _ = DeleteObject(icon_info.hbmColor);
            let _ = DestroyIcon(hicon);
            return None;
        }

        let hdc = GetDC(None);
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32), // negative = top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let lines = GetDIBits(
            hdc,
            icon_info.hbmColor,
            0,
            height,
            Some(pixels.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        ReleaseDC(None, hdc);
        let _ = DeleteObject(icon_info.hbmColor);
        let _ = DestroyIcon(hicon);

        if lines == 0 {
            return None;
        }

        // GetDIBits fills BGRA (classic DIB order) — swap to RGBA.
        for px in pixels.chunks_exact_mut(4) {
            px.swap(0, 2);
        }

        Some((pixels, width, height))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerate_returns_at_least_one_window() {
        let sources = enumerate_sources();
        assert!(!sources.is_empty(), "expected at least one visible window");
    }

    #[test]
    fn capture_source_has_non_empty_title() {
        let sources = enumerate_sources();
        for s in &sources {
            assert!(!s.window_title.is_empty());
        }
    }

    #[test]
    fn capture_sources_have_nonzero_hwnd() {
        let sources = enumerate_sources();
        assert!(!sources.is_empty());
        for s in &sources {
            assert_ne!(s.hwnd, 0usize, "HWND should not be null for '{}'", s.window_title);
        }
    }

    #[test]
    fn at_least_one_source_has_an_extractable_icon() {
        let sources = enumerate_sources();
        assert!(
            sources.iter().any(|s| s.icon_rgba.is_some()),
            "expected at least one source with an extractable icon"
        );
    }

    #[test]
    fn extracted_icon_has_matching_buffer_length() {
        let sources = enumerate_sources();
        for s in &sources {
            if let Some((rgba, w, h)) = &s.icon_rgba {
                assert_eq!(rgba.len(), (*w * *h * 4) as usize, "RGBA buffer length must be width*height*4");
            }
        }
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo build 2>&1 | tail -30 && cargo test --bin polyrec sources:: 2>&1 | tail -20`
Expected: clean build; all 5 `sources::tests::*` pass.

- [ ] **Step 6: Commit**

```bash
git add src/types.rs src/sources.rs
git commit -m "$(cat <<'EOF'
feat: extract exe icons for the source list

Best-effort SHGetFileInfoW + GetDIBits extraction to RGBA; a
missing icon (unresolvable exe, shell window, etc.) degrades to
no icon for that row rather than failing the whole list.
EOF
)"
```

---

### Task 5: Dashboard — Quality popup, source icons, audio tooltip, palette accent

**Files:**
- Modify: `src/ui/dashboard.rs:16-30` (palette consts), `src/ui/dashboard.rs:39-96` (`App` struct + `new`), `src/ui/dashboard.rs:183-238` (source list + audio panel), `src/ui/dashboard.rs:351-378` (OUTPUT section — add Quality button), `src/ui/dashboard.rs:697-719` (`handle_rec_button`'s `start_capture` call)

**Interfaces:**
- Consumes: `crate::session::EncodeSettings` (Task 3), `crate::config::{ResolutionMode, BitrateMode}` (Task 1), `CaptureSource.icon_rgba` (Task 4).

- [ ] **Step 1: Add the secondary accent color**

In the palette const block (`src/ui/dashboard.rs:16-30`), add after `BORDER_SEL`:

```rust
                    const BORDER_SEL:   egui::Color32 = egui::Color32::from_rgb(90, 90, 190);
                    const ACCENT_SECONDARY: egui::Color32 = egui::Color32::from_rgb(0x86, 0x86, 0xCF);
```

- [ ] **Step 2: Add popup state + icon texture cache to `App`**

In the `App` struct (`src/ui/dashboard.rs:39-59`), add fields after `overlay_enabled`:

```rust
    overlay_enabled: bool,
    show_quality_popup: bool,
    source_icon_textures: std::collections::HashMap<usize, egui::TextureHandle>,
```

In `App::new` (`src/ui/dashboard.rs:75-96`), initialize them alongside `overlay_enabled`:

```rust
            overlay_enabled,
            show_quality_popup: false,
            source_icon_textures: std::collections::HashMap::new(),
```

- [ ] **Step 3: Invalidate the icon cache on refresh**

The cache is keyed by list index, not by window identity — if it isn't cleared when the source list is re-enumerated, a stale texture from the old list could render under the wrong window at that index. In the "⟳ Refresh" button handler (`src/ui/dashboard.rs:151-158`), add a clear right after re-enumerating:

```rust
                if ui.button("⟳ Refresh").clicked() {
                    self.sources = enumerate_sources();
                    self.source_icon_textures.clear();
                    self.selected_source = None;
                    self.audio_devices = enumerate_audio_devices().unwrap_or_default();
                    let n = self.audio_devices.len();
                    self.selected_audio = self.audio_devices.iter().map(|d| d.is_loopback).collect();
                    self.export_track_selection = vec![true; n];
                }
```

- [ ] **Step 4: Render source-list icons**

Replace the source list rendering loop (`src/ui/dashboard.rs:183-208`) to draw the icon before the title:

```rust
                        for (i, source) in self.sources.iter().enumerate() {
                            let selected = self.selected_source == Some(i);
                            let fill   = if selected { BG_SELECTED } else { BG_CARD };
                            let border = if selected { BORDER_SEL } else { BORDER };

                            if !self.source_icon_textures.contains_key(&i) {
                                if let Some((rgba, w, h)) = &source.icon_rgba {
                                    let image = egui::ColorImage::from_rgba_unmultiplied([*w as usize, *h as usize], rgba);
                                    let tex = ui.ctx().load_texture(
                                        format!("source_icon_{i}"),
                                        image,
                                        egui::TextureOptions::LINEAR,
                                    );
                                    self.source_icon_textures.insert(i, tex);
                                }
                            }

                            let inner = egui::Frame::none()
                                .fill(fill)
                                .stroke(egui::Stroke::new(1.0, border))
                                .rounding(egui::Rounding::same(6.0))
                                .inner_margin(egui::Margin::same(8.0))
                                .show(ui, |ui| {
                                    ui.set_min_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        if let Some(tex) = self.source_icon_textures.get(&i) {
                                            ui.image((tex.id(), egui::vec2(16.0, 16.0)));
                                        }
                                        ui.vertical(|ui| {
                                            ui.label(
                                                egui::RichText::new(&source.window_title)
                                                    .size(13.0)
                                                    .strong()
                                                    .color(TEXT_PRIMARY),
                                            );
                                            if !source.exe_name.is_empty() {
                                                ui.label(
                                                    egui::RichText::new(&source.exe_name)
                                                        .size(11.0)
                                                        .color(TEXT_MUTED),
                                                );
                                            }
                                        });
                                    });
                                });
```

(The `if inner.response.interact(...)` click handling and `ui.add_space(3.0)` right after stay exactly as they were.)

- [ ] **Step 5: Add the audio precondition tooltip**

Replace the `app_audio_only` checkbox block (`src/ui/dashboard.rs:227-238`):

```rust
                let loopback_selected = self
                    .audio_devices
                    .iter()
                    .zip(self.selected_audio.iter())
                    .any(|(dev, &sel)| dev.is_loopback && sel);
                let has_loopback_device = self.audio_devices.iter().any(|d| d.is_loopback);
                let has_source = self.selected_source.is_some();
                ui.add_enabled_ui(loopback_selected && has_source, |ui| {
                    ui.checkbox(
                        &mut self.app_audio_only,
                        egui::RichText::new("🎯 App audio only (exclude other system sounds)")
                            .color(ACCENT_SECONDARY),
                    )
                    .on_hover_text(if !has_loopback_device {
                        "No system playback device found — this needs one to exist (being muted doesn't matter, but a device must be present)."
                    } else if !loopback_selected {
                        "Check the system audio (🔊) box above first."
                    } else {
                        "Records only the selected window's own audio via Windows' Process Loopback API, instead of the full desktop mix. Needs an active system playback device — muting it doesn't stop this from working."
                    });
                });
```

- [ ] **Step 6: Add the Quality popup**

In the OUTPUT section (`src/ui/dashboard.rs:352-378`, right after `section_header(ui, "OUTPUT");` and before the output-dir `ui.horizontal`), add the button that opens the popup:

```rust
            section_header(ui, "OUTPUT");

            if ui
                .add(egui::Button::new(egui::RichText::new("⚙ Quality").color(ACCENT_SECONDARY)))
                .clicked()
            {
                self.show_quality_popup = true;
            }
            ui.add_space(4.0);
```

Add the popup window itself near the existing "Export dialog" block (`src/ui/dashboard.rs`, right before `// ── Export dialog ──` around line 485). No new imports needed here — the popup only reads/writes the plain `String`/`u32` fields on `self.config.encode` directly (`ResolutionMode`/`BitrateMode` are resolved later, only at the `start_capture` call site in Step 6, via `.resolution_mode()`/`.bitrate_mode()` method calls whose return type Rust infers without needing the enum names in scope):

```rust
        // ── Quality settings popup ────────────────────────────────────────────
        if self.show_quality_popup {
            let mut close = false;
            egui::Window::new("Quality Settings")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    section_header(ui, "FPS");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.config.encode.fps, 30, "30");
                        ui.selectable_value(&mut self.config.encode.fps, 60, "60");
                    });

                    section_header(ui, "CODEC");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.config.encode.codec, "h264".into(), "H264");
                        ui.selectable_value(&mut self.config.encode.codec, "h265".into(), "H265");
                    });

                    section_header(ui, "RESOLUTION");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.config.encode.resolution_mode, "native".into(), "Native (window)");
                        ui.selectable_value(&mut self.config.encode.resolution_mode, "display".into(), "Match display");
                        ui.selectable_value(&mut self.config.encode.resolution_mode, "custom".into(), "Custom");
                    });
                    if self.config.encode.resolution_mode == "custom" {
                        ui.horizontal(|ui| {
                            ui.label("W:");
                            ui.add(egui::DragValue::new(&mut self.config.encode.custom_width).range(2..=7680));
                            ui.label("H:");
                            ui.add(egui::DragValue::new(&mut self.config.encode.custom_height).range(2..=4320));
                        });
                    }

                    section_header(ui, "BITRATE");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.config.encode.bitrate_mode, "auto".into(), "Auto");
                        ui.selectable_value(&mut self.config.encode.bitrate_mode, "manual".into(), "Manual");
                    });
                    if self.config.encode.bitrate_mode == "manual" {
                        ui.horizontal(|ui| {
                            ui.label("Mbps:");
                            ui.add(egui::DragValue::new(&mut self.config.encode.manual_bitrate_mbps).range(1..=100));
                        });
                    }

                    ui.add_space(8.0);
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            if close {
                self.show_quality_popup = false;
                if let Err(e) = self.config.save() {
                    tracing::error!("failed to save config: {e}");
                }
            }
        }
```

- [ ] **Step 7: Build `EncodeSettings` at the `start_capture` call site**

Replace the `start_capture` call in `handle_rec_button` (`src/ui/dashboard.rs:710-716`):

```rust
            self.session.apply(SessionAction::Start);
            self.session.start_capture(
                source,
                selected_devices,
                self.app_audio_only,
                Arc::clone(&self.frame_count),
                &self.config.output_dir,
                crate::session::EncodeSettings {
                    codec: self.config.encode.codec.clone(),
                    fps: self.config.encode.fps,
                    resolution_mode: self.config.encode.resolution_mode(),
                    bitrate_mode: self.config.encode.bitrate_mode(),
                },
            );
```

(`ResolutionMode`/`BitrateMode` aren't referenced by name anywhere in `dashboard.rs` — `.resolution_mode()`/`.bitrate_mode()` return them, but Rust infers the type without needing the names in scope, so no import is added for them.)

- [ ] **Step 8: Build and manually verify**

Run: `cargo build 2>&1 | tail -40`
Expected: clean build.

Run: `cargo test --bin polyrec 2>&1 | tail -20`
Expected: all non-ignored tests still pass (unchanged by this UI-only task).

Then run the app and manually check (this task has no automated UI test — egui rendering isn't unit-testable here, consistent with the rest of `dashboard.rs` having no tests):

```bash
cargo run
```

Verify: source list rows show an icon; hovering "App audio only" shows the tooltip; the "⚙ Quality" button opens the popup with FPS/Codec/Resolution/Bitrate controls, and closing it persists to `config.toml` (reopen the popup or restart the app to confirm the selection stuck).

- [ ] **Step 9: Commit**

```bash
git add src/ui/dashboard.rs
git commit -m "$(cat <<'EOF'
feat: add Quality settings popup, source-list icons, audio tooltip

Exposes FPS/codec/resolution-mode/bitrate (previously unexposed or
dead config) via a new popup; adds exe icons to the source list;
adds a precondition tooltip to "App audio only"; introduces the
icon-matched periwinkle accent for both.
EOF
)"
```

---

## Final verification

- [ ] Run the full non-ignored suite once more: `cargo test --bin polyrec 2>&1 | tail -20` — expect all green.
- [ ] Run `cargo build --release 2>&1 | tail -20` once to confirm the release profile (used for the actual shipped exe) also compiles clean.
- [ ] Manually record a short clip via `cargo run` with Quality set to each of Native/Display/Custom resolution and Auto/Manual bitrate at least once, confirming no crash and a playable output file.
