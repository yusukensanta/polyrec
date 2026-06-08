# Audio Format Probe — Design Spec

**Date:** 2026-06-08
**Status:** Approved

## Summary

Replace hardcoded `(48000, 2)` audio specs in `SessionManager::start_capture` with actual WASAPI mix format values per device, obtained via a new synchronous `probe_audio_format` function. Fixes potential A/V sync drift when devices report non-48kHz or mono formats.

## Scope

- `src/capture/audio.rs` — add `pub fn probe_audio_format(device_id: &str, is_loopback: bool) -> (u32, u16)`
- `src/session/mod.rs` — replace hardcoded audio_specs, add import

## probe_audio_format

```rust
pub fn probe_audio_format(device_id: &str, is_loopback: bool) -> (u32, u16)
```

### Algorithm

1. `CoInitializeEx(None, COINIT_MULTITHREADED)` — ignore result (may already be init'd on calling thread)
2. `CoCreateInstance::<IMMDeviceEnumerator>()` — on error, return `(48000, 2)`
3. Open device:
   - If `device_id` is empty: `GetDefaultAudioEndpoint(eRender if is_loopback else eCapture, eMultimedia)`
   - Otherwise: `GetDevice(&HSTRING::from(device_id))`
   - On error: return `(48000, 2)`
4. `device.Activate::<IAudioClient>()` — on error: return `(48000, 2)`
5. `audio_client.GetMixFormat()` — on error: return `(48000, 2)`
6. Read `(*mix_format).nSamplesPerSec` and `(*mix_format).nChannels`
7. Guard: if either is 0, return `(48000, 2)`
8. Return `(sample_rate as u32, channels as u16)`

No COM cleanup needed — `IMMDeviceEnumerator` and `IAudioClient` are dropped via RAII when the function returns.

### Fallback

Any error at any step returns `(48000, 2)`. This keeps recordings working even if probing fails, and matches the previous hardcoded behavior.

## session/mod.rs change

Add import:
```rust
use crate::capture::audio::probe_audio_format;
```

Replace in `start_capture`:
```rust
// Before
let audio_specs: Vec<(u32, u16)> = audio_devices
    .iter()
    .map(|_| (48000u32, 2u16))
    .collect();

// After
let audio_specs: Vec<(u32, u16)> = audio_devices
    .iter()
    .map(|dev| probe_audio_format(&dev.id, dev.is_loopback))
    .collect();
```

## Testing

One integration test in `src/capture/audio.rs`:

```rust
#[test]
fn probe_audio_format_returns_valid_format() {
    let devices = enumerate_audio_devices().expect("enumerate");
    for dev in &devices {
        let (sample_rate, channels) = probe_audio_format(&dev.id, dev.is_loopback);
        assert!(sample_rate > 0, "sample_rate must be > 0");
        assert!(channels > 0, "channels must be > 0");
    }
}
```

## Non-Goals

- Format conversion / resampling (if device is 44.1kHz, the writer encodes at 44.1kHz — MF handles it)
- Exposing format in the UI
- Caching the probed format
