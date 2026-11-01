use crate::capture::frame_pool::FrameBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrackId(pub u32);

impl TrackId {
    pub fn new(id: u32) -> Self {
        Self(id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    Idle,
    Recording,
    Paused,
}

/// What `CaptureSource::hwnd` actually identifies -- a real HWND for `Window`,
/// or an HMONITOR for `Monitor` (both are opaque pointer-sized Win32 handles,
/// so the same `usize` field stores either without needing a second field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureKind {
    Window,
    Monitor,
}

#[derive(Debug, Clone)]
pub struct CaptureSource {
    pub kind: CaptureKind,
    /// 0 for `Monitor` -- a display isn't owned by any one process, so there's
    /// no PID to scope Process Loopback Capture ("App audio only") to. See
    /// the `kind == CaptureKind::Window` gate on `use_process_loopback` in
    /// `session::start_capture`.
    pub process_id: u32,
    /// Window title for `Window`; a display's own generated label (e.g.
    /// "🖥 Display 1 (2560×1440)") for `Monitor`.
    pub window_title: String,
    /// Empty for `Monitor` -- a display has no owning exe.
    pub exe_name: String,
    /// HWND for `Window`, HMONITOR for `Monitor` -- see `CaptureKind`.
    pub hwnd: usize,
    /// (RGBA bytes, width, height) of the source exe's small icon, if extractable.
    /// Always `None` for `Monitor` -- the 🖥 in `window_title` stands in for an
    /// icon there instead, same convention `audio_device_icon` already uses
    /// for Speakers/Microphone.
    pub icon_rgba: Option<(Vec<u8>, u32, u32)>,
}

/// A captured video frame.
#[derive(Debug)]
pub struct VideoFrame {
    pub pts: Duration,
    pub data: FrameData,
}

/// A frame's pixels: in system memory, or -- with `EncodeConfig::gpu_frames`
/// -- still on the GPU (see `encode::gpu`).
pub enum FrameData {
    /// Raw BGRA pixel data at the encoder's output size, row-major,
    /// width*height*4 bytes -- usually a recycled buffer from the capture
    /// loop's `FramePool`.
    Cpu(FrameBuf),
    /// A BGRA texture at the capture size, on the device the recording's
    /// `GpuFrames` shares with the encoder, which scales it as needed.
    Gpu(windows::Win32::Graphics::Direct3D11::ID3D11Texture2D),
}

// SAFETY: a texture is only ever used through the device `GpuFrames` shares,
// whose multithread protection serializes access to it; nothing writes to a
// frame's texture once it has been sent.
unsafe impl Send for FrameData {}

impl std::fmt::Debug for FrameData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cpu(bytes) => write!(f, "Cpu({} bytes)", bytes.len()),
            Self::Gpu(_) => f.write_str("Gpu"),
        }
    }
}

impl From<FrameBuf> for FrameData {
    fn from(bytes: FrameBuf) -> Self {
        Self::Cpu(bytes)
    }
}

impl From<Vec<u8>> for FrameData {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Cpu(bytes.into())
    }
}

#[derive(Debug)]
pub struct AudioSamples {
    pub track_id: TrackId,
    pub pts: Duration,
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
}

/// The loudest an audio track has been since the dashboard last looked, as
/// linear amplitude (0.0 to 1.0): raised chunk by chunk by the track's pump,
/// read and reset by the dashboard's level meter once a frame. Clones share
/// one value.
///
/// Held rather than overwritten: a chunk is ~10 ms and a frame 33 ms or more,
/// so showing only the latest chunk skipped most of the sound and made the
/// meter flicker.
#[derive(Debug, Clone, Default)]
pub struct AudioLevel(Arc<AtomicU32>);

impl AudioLevel {
    pub fn record(&self, samples: &[f32]) {
        let peak = samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
        // Non-negative floats order the same way as their bits, so an
        // integer max keeps the louder of two peaks.
        self.0.fetch_max(peak.min(1.0).to_bits(), Ordering::Relaxed);
    }

    /// The loudest since the last call, starting the next interval from
    /// silence.
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.0.swap(0, Ordering::Relaxed))
    }
}

/// Identifies a Windows audio endpoint device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub is_loopback: bool,
}

/// One exe's WASAPI audio session(s) -- lets a specific app's audio
/// (Discord, Spotify, a game) be selected as its own independent recording
/// track via Process Loopback Capture, the same mechanism the "App audio
/// only" checkbox already uses for whichever window is selected as the
/// video capture source. This is deliberately separate from that: picking
/// an app here doesn't require it to be the video source, and vice versa
/// (see `capture::audio::enumerate_app_audio_sessions`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppAudioSource {
    /// One entry per live top-level process currently producing audio under
    /// this exe -- grouped by `exe_name` rather than one `AppAudioSource`
    /// per pid, so two independent instances of the same exe (not
    /// parent/child) share one checkbox/slider instead of needing both
    /// toggled separately. Each pid gets its own Process Loopback Capture
    /// task AND its own recording track at recording start (see
    /// `session::start_capture`'s per-app spawn loop, and
    /// `actions::build_audio_labels`'s doc comment for why anything deriving
    /// per-track state from this list must expand by `process_ids.len()`,
    /// not treat one `AppAudioSource` as one track) -- genuinely mixing
    /// separately captured PCM streams into one track would need a real-time
    /// mixer this doesn't have, so multiple processes stay as separate
    /// tracks instead of merging into a single one.
    ///
    /// Empty for a `Config::registered_app_audio` entry that isn't
    /// currently running: still shown (greyed, see `render_audio_popup`)
    /// so its pinned volume is visible, but nothing to actually spawn a
    /// capture task for.
    pub process_ids: Vec<u32>,
    /// e.g. "Discord.exe" -- used as the stable key for
    /// `Config::audio_device_gain` (prefixed `"app:"`) and for
    /// `Config::registered_app_audio`, since a pid isn't stable across the
    /// app's own restarts.
    pub exe_name: String,
    /// WASAPI's own session display name if the app set one (most don't);
    /// falls back to `exe_name` with the `.exe` suffix stripped.
    pub display_name: String,
    /// (RGBA bytes, width, height) of the exe's small icon, if extractable --
    /// same convention as `CaptureSource::icon_rgba`.
    pub icon_rgba: Option<(Vec<u8>, u32, u32)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_level_holds_the_loudest_chunk_until_it_is_read() {
        let level = AudioLevel::default();
        assert_eq!(level.take_peak(), 0.0);
        // Several chunks arrive between two dashboard frames: the meter
        // shows the loudest, not whichever came last.
        level.record(&[0.1, -0.5, 0.25]);
        level.clone().record(&[0.05]);
        assert_eq!(level.take_peak(), 0.5);
        // Reading starts the next interval from silence.
        assert_eq!(level.take_peak(), 0.0);
        level.record(&[-3.0]);
        assert_eq!(level.take_peak(), 1.0, "clamped to full scale");
    }

    #[test]
    fn track_id_equality() {
        assert_eq!(TrackId::new(1), TrackId::new(1));
        assert_ne!(TrackId::new(1), TrackId::new(2));
    }

    #[test]
    fn session_state_default_is_idle() {
        let s = SessionState::Idle;
        assert_eq!(s, SessionState::Idle);
    }

    #[test]
    fn audio_samples_preserves_track_id() {
        let samples = AudioSamples {
            track_id: TrackId::new(42),
            pts: Duration::from_millis(100),
            samples: vec![0.0, 1.0, -1.0],
            sample_rate: 48000,
            channels: 2,
        };
        assert_eq!(samples.track_id, TrackId::new(42));
        assert_eq!(samples.sample_rate, 48000);
    }

    #[test]
    fn audio_device_loopback_flag() {
        let dev = AudioDevice {
            id: "test".into(),
            name: "Speakers".into(),
            is_loopback: true,
        };
        assert!(dev.is_loopback);
    }
}
