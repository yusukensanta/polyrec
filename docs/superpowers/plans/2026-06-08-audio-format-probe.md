# Audio Format Probe Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the hardcoded `(48000, 2)` audio specs in `SessionManager::start_capture` with actual WASAPI mix format values probed from each device before recording starts.

**Architecture:** New synchronous `probe_audio_format(device_id, is_loopback) -> (u32, u16)` function in `capture/audio.rs` uses the same WASAPI device-open logic as `run_audio_capture`; falls back to `(48000, 2)` on any error. `start_capture` calls it per device to build the real `audio_specs` for `RecordingWriter`.

**Tech Stack:** Rust, Windows WASAPI (`Win32_Media_Audio`), existing `windows-rs` 0.58 bindings

---

## File Map

| File | Change |
|------|--------|
| `src/capture/audio.rs` | Add `pub fn probe_audio_format(device_id: &str, is_loopback: bool) -> (u32, u16)` + test |
| `src/session/mod.rs` | Add import, replace hardcoded `audio_specs` |

---

## Task 1: probe_audio_format function + integration test

**Files:**
- Modify: `src/capture/audio.rs`

- [ ] **Step 1: Write the failing test**

In `src/capture/audio.rs`, inside the `#[cfg(test)] mod tests` block, add after the existing tests:

```rust
    #[test]
    fn probe_audio_format_returns_valid_format() {
        let devices = enumerate_audio_devices().expect("enumerate_audio_devices failed");
        assert!(!devices.is_empty(), "need at least one device to probe");
        for dev in &devices {
            let (sample_rate, channels) = probe_audio_format(&dev.id, dev.is_loopback);
            assert!(sample_rate > 0, "device '{}': sample_rate must be > 0", dev.name);
            assert!(channels > 0, "device '{}': channels must be > 0", dev.name);
        }
    }
```

- [ ] **Step 2: Run to verify it fails**

```powershell
cd "C:\Users\yusuk\work\polyrec\.worktrees\feat-audio-format-probe"
cargo test probe_audio_format_returns_valid_format -- --nocapture 2>&1 | tail -8
```

Expected: compile error — `probe_audio_format` not yet defined.

- [ ] **Step 3: Implement probe_audio_format**

In `src/capture/audio.rs`, add the following function BEFORE the `#[cfg(test)]` block (i.e., after `run_audio_capture` ends):

```rust
/// Query the WASAPI mix format for a device without starting a capture stream.
/// Returns `(sample_rate, channels)`. Falls back to `(48000, 2)` on any error.
pub fn probe_audio_format(device_id: &str, is_loopback: bool) -> (u32, u16) {
    unsafe { probe_audio_format_inner(device_id, is_loopback).unwrap_or((48000, 2)) }
}

unsafe fn probe_audio_format_inner(
    device_id: &str,
    is_loopback: bool,
) -> Result<(u32, u16), ()> {
    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

    let enumerator: IMMDeviceEnumerator =
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|_| ())?;

    let device = if device_id.is_empty() {
        let flow = if is_loopback { eRender } else { eCapture };
        enumerator
            .GetDefaultAudioEndpoint(flow, eMultimedia)
            .map_err(|_| ())?
    } else {
        let id: windows::core::HSTRING = device_id.into();
        enumerator.GetDevice(&id).map_err(|_| ())?
    };

    let audio_client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|_| ())?;
    let mix_format = audio_client.GetMixFormat().map_err(|_| ())?;

    let sample_rate = (*mix_format).nSamplesPerSec;
    let channels = (*mix_format).nChannels;

    if sample_rate == 0 || channels == 0 {
        return Err(());
    }

    Ok((sample_rate, channels as u16))
}
```

> **Note:** All imports (`CoInitializeEx`, `CoCreateInstance`, `IMMDeviceEnumerator`, `MMDeviceEnumerator`, `CLSCTX_ALL`, `COINIT_MULTITHREADED`, `eRender`, `eCapture`, `eMultimedia`, `IAudioClient`) are already present in the file's `use` block from `run_audio_capture`. No new imports needed.

- [ ] **Step 4: Run the test to verify it passes**

```powershell
cargo test probe_audio_format_returns_valid_format -- --nocapture 2>&1 | tail -8
```

Expected: `test capture::audio::tests::probe_audio_format_returns_valid_format ... ok`

- [ ] **Step 5: Run full test suite**

```powershell
cargo test 2>&1 | tail -5
```

Expected: all tests pass, 0 failed.

- [ ] **Step 6: Commit**

```powershell
git add src/capture/audio.rs
git commit -m "feat: add probe_audio_format to query WASAPI mix format per device"
```

---

## Task 2: Wire probe_audio_format into SessionManager

**Files:**
- Modify: `src/session/mod.rs`

- [ ] **Step 1: Add import**

In `src/session/mod.rs`, the imports currently include:
```rust
use crate::capture::audio::run_audio_capture;
```

Add `probe_audio_format` to that same line:
```rust
use crate::capture::audio::{probe_audio_format, run_audio_capture};
```

- [ ] **Step 2: Replace hardcoded audio_specs**

Find this exact block in `start_capture` (around line 71):
```rust
        // Audio device specs for RecordingWriter (sample_rate, channels per track).
        // We use defaults here; the capture actor will start with WASAPI native format.
        // Plan 4 can wire the actual format back.
        let audio_specs: Vec<(u32, u16)> = audio_devices
            .iter()
            .map(|_| (48000u32, 2u16))
            .collect();
```

Replace it with:
```rust
        // Probe actual WASAPI mix format per device; fall back to (48000, 2) on error.
        let audio_specs: Vec<(u32, u16)> = audio_devices
            .iter()
            .map(|dev| probe_audio_format(&dev.id, dev.is_loopback))
            .collect();
```

- [ ] **Step 3: Verify it compiles and all tests pass**

```powershell
cargo test 2>&1 | tail -5
```

Expected: all tests pass, 0 failed.

- [ ] **Step 4: Commit**

```powershell
git add src/session/mod.rs
git commit -m "fix: use actual WASAPI mix format per device instead of hardcoded 48kHz/stereo"
```

---

## Self-Review

**Spec coverage:**
- ✅ `probe_audio_format(device_id, is_loopback) -> (u32, u16)` added to `capture/audio.rs`
- ✅ Same WASAPI device-open logic as `run_audio_capture`
- ✅ Fallback `(48000, 2)` on any error via `unwrap_or`
- ✅ Guards against zero sample_rate or channels
- ✅ Integration test: all enumerated devices return valid (>0) format
- ✅ `audio_specs` in `start_capture` uses probed values

**Placeholder scan:** None found.

**Type consistency:** `probe_audio_format` returns `(u32, u16)` — matches `Vec<(u32, u16)>` type of `audio_specs` in `start_capture` and `audio_tracks: &[(u32, u16)]` in `RecordingWriter::new`. ✓
