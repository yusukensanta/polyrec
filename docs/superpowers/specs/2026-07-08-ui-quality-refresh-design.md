# UI + Quality Settings Refresh — Design Spec

**Date:** 2026-07-08
**Status:** Approved

## Summary

Two things, bundled because they touch the same UI surface:

1. **Layout refresh** — reorganize the dashboard (source list, audio panel) for clarity, add exe icons to the source list, add a precondition tooltip to "App audio only", and align accent colors with the app icon's palette (already close; add the unused periwinkle accent).
2. **Quality settings** — expose FPS, codec, resolution mode, and bitrate mode/value in a new `⚙ Quality` popup, wired all the way through to the encoder. Codec selection was previously dead config (writer hardcoded H264); resolution defaults to the window's native size (recent fix) with `display`/`custom` as explicit opt-ins, not defaults.

## Scope

- `src/config.rs` — extend `EncodeConfig`, add resolution/bitrate mode types + parsing
- `src/capture/video.rs` — restore `query_display_size` (opt-in only, not default)
- `src/encode/writer.rs` — parameterize codec (H264/HEVC) with fallback; accept explicit bitrate
- `src/session/mod.rs` — bundle new settings into `EncodeSettings`, resolve output size per mode
- `src/sources.rs` — extract exe icon per window
- `src/ui/dashboard.rs` — source list icons, audio tooltip, `⚙ Quality` popup, palette accent

## 1. config.rs — EncodeConfig

Flat, TOML-friendly fields (not tagged enums) so an old/hand-edited config never fails to parse — unknown/invalid values fall back to safe defaults rather than erroring:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EncodeConfig {
    pub codec: String,           // "h265" | "h264"
    pub fps: u32,                // 30 | 60
    pub resolution_mode: String, // "native" | "display" | "custom"
    pub custom_width: u32,
    pub custom_height: u32,
    pub bitrate_mode: String,    // "auto" | "manual"
    pub manual_bitrate_mbps: u32,
}
```

Defaults: `codec="h265"`, `fps=60`, `resolution_mode="native"`, `custom_width=1920`, `custom_height=1080`, `bitrate_mode="auto"`, `manual_bitrate_mbps=12`.

Add parsing helpers (pure functions, unit-testable without touching Windows APIs):

```rust
pub enum ResolutionMode { Native, Display, Custom(u32, u32) }
pub enum BitrateMode { Auto, Manual(u32) } // Manual carries Mbps

impl EncodeConfig {
    pub fn resolution_mode(&self) -> ResolutionMode {
        match self.resolution_mode.as_str() {
            "display" => ResolutionMode::Display,
            "custom" => ResolutionMode::Custom(
                self.custom_width.clamp(2, 7680) & !1,
                self.custom_height.clamp(2, 4320) & !1,
            ),
            _ => ResolutionMode::Native, // unknown string -> safe default
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

## 2. capture/video.rs — query_display_size restored

Bring back the function removed in the resolution-regression fix, unchanged, but now only called when the user explicitly picks `resolution_mode="display"`:

```rust
pub fn query_display_size(hwnd: HWND) -> Result<(u32, u32), AppError> { /* MonitorFromWindow + GetMonitorInfoW, as before */ }
```

Needs `Win32_Graphics_Gdi` re-added to `Cargo.toml` features (removed as dead in the earlier fix).

## 3. encode/writer.rs — codec parameter + fallback, explicit bitrate

`RecordingWriter::new` takes `codec: &str` and `bitrate_bps: u32` (caller resolves Auto vs Manual before calling — writer doesn't know about modes, just a final number):

```rust
pub fn new(
    output_path: &Path, width: u32, height: u32, fps: u32,
    codec: &str, bitrate_bps: u32,
    audio_tracks: &[(u32, u16)],
) -> Result<Self, AppError>
```

`make_video_output_type` takes a `subtype: GUID` param instead of hardcoding `MFVideoFormat_H264`. In `RecordingWriter::new`:

```rust
let subtype = if codec == "h265" { MFVideoFormat_HEVC } else { MFVideoFormat_H264 };
let video_out = make_video_output_type(width, height, fps, subtype, bitrate_bps);
let video_stream = match writer.AddStream(&video_out) {
    Ok(idx) => idx,
    Err(e) if subtype == MFVideoFormat_HEVC => {
        tracing::warn!("HEVC AddStream failed ({e}), falling back to H264");
        let fallback = make_video_output_type(width, height, fps, MFVideoFormat_H264, bitrate_bps);
        writer.AddStream(&fallback).map_err(|e| AppError::Encode(format!("AddStream video (H264 fallback): {e}")))?
    }
    Err(e) => return Err(AppError::Encode(format!("AddStream video: {e}"))),
};
```

Same fallback applies to `SetInputMediaType` if needed (input type subtype for H264 vs HEVC compressed input is actually the same ARGB32 pre-encode side — only the *output* subtype changes, so the input type function is untouched).

`video_bitrate_bps()` (the resolution-aware formula from the last fix) stays as-is and becomes the thing callers use to compute the Auto value before calling `new`.

## 4. session/mod.rs — EncodeSettings + resolution resolution

New struct bundles what used to be scattered params:

```rust
pub struct EncodeSettings {
    pub codec: String,
    pub fps: u32,
    pub resolution_mode: crate::config::ResolutionMode,
    pub bitrate_mode: crate::config::BitrateMode,
}
```

`start_capture` takes `encode: EncodeSettings` instead of a bare fps. Output size resolution (pure logic, unit-testable by passing in the already-queried sizes):

```rust
fn resolve_output_size(
    mode: &ResolutionMode,
    capture_size: (u32, u32),
    display_size: Option<(u32, u32)>, // None if query failed
) -> (u32, u32) {
    match mode {
        ResolutionMode::Native => capture_size,
        ResolutionMode::Display => display_size.unwrap_or(capture_size),
        ResolutionMode::Custom(w, h) => (*w, *h),
    }
}
```

`start_capture` only calls `query_display_size` when `mode == Display` — no wasted syscall, and no change to the Native-default behavior fixed earlier. Bitrate resolved via `match encode.bitrate_mode { Auto => video_bitrate_bps(w,h,fps), Manual(bps) => bps }` and passed to `RecordingWriter::new`.

## 5. sources.rs — exe icon extraction

Keep `sources.rs` free of egui (it's UI-agnostic); add `icon_rgba: Option<(Vec<u8>, u32, u32)>` (raw RGBA + dimensions) to `CaptureSource`. Extraction: `SHGetFileInfoW(path, 0, &mut info, SHGFI_ICON | SHGFI_SMALLICON)` on the resolved exe path gives an `HICON`; convert to RGBA via `GetIconInfo` (for the mask/color bitmaps) + `GetDIBits`. Dashboard converts the raw RGBA to an `egui::TextureHandle` once per source list refresh (cached in a `HashMap<usize, TextureHandle>`, not rebuilt per-frame).

## 6. ui/dashboard.rs

- Source list row: 16x16 icon (from `CaptureSource.icon_rgba`, see §5 for the texture cache) to the left of title/exe text.
- Audio panel: `ui.label("🎯 ...").on_hover_text("Requires an active system playback device — being muted doesn't stop this from working.")` on the App-audio-only row (and disabled-reason text shown when no loopback device exists at all: `audio_devices.iter().any(|d| d.is_loopback)` is false).
- New `⚙ Quality` button in center panel (near OUTPUT section) opens an `egui::Window` popup:
  - FPS: `ComboBox` 30/60
  - Codec: `ComboBox` H264/H265
  - Resolution: `ComboBox` Native (window) / Match display / Custom → reveals two `DragValue<u32>` (width/height) when Custom
  - Bitrate: `ComboBox` Auto / Manual → reveals one `DragValue<u32>` (Mbps, 1-100) when Manual
  - Changes write directly into `self.config.encode` and call `self.config.save()` on close, matching the existing output-dir-picker pattern.
- New periwinkle accent `const ACCENT_SECONDARY: Color32 = Color32::from_rgb(0x86, 0x86, 0xCF);` used for the `⚙ Quality` button fill and the `ⓘ` tooltip glyph color.

## Testing

- `config.rs`: extend round-trip test for new fields; new tests for `resolution_mode()`/`bitrate_mode()` parsing (valid + invalid-string-falls-back-to-default cases).
- `session/mod.rs`: unit tests for `resolve_output_size()` covering all three modes, including `Display` with `display_size: None` (query failure) falling back to capture size.
- `encode/writer.rs`: existing `video_bitrate_bps` tests stay; add a codec→GUID mapping unit test; keep the real HEVC-accepted-by-hardware check as a new `#[ignore]`d integration test (writes a tiny HEVC clip) alongside the existing hardware-dependent tests — CI/dev machines vary in HEVC MFT availability so this isn't part of the default suite.
- `sources.rs`: icon extraction — assert `enumerate_sources()` entries have `icon_rgba.is_some()` for at least one window with a resolvable exe (best-effort; some system windows won't resolve, that's fine).

## Non-goals

- No live window thumbnails (icon-only, per discussion — avoids a second capture path and per-frame cost).
- No per-track bitrate/codec (single video bitrate/codec setting, audio unaffected).
- Not reintroducing display-mode as default — it's explicit opt-in only, to avoid regressing the blur/artifact fix.
