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

/// Captured video frame — CPU-mapped BGRA bytes (Plan 2).
/// Plan 3 will add GPU texture path for NVENC.
#[derive(Debug)]
pub struct VideoFrame {
    pub pts: Duration,
    /// Raw BGRA pixel data, row-major, width*height*4 bytes.
    pub data: Vec<u8>,
}

#[derive(Debug)]
pub struct AudioSamples {
    pub track_id: TrackId,
    pub pts: Duration,
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
}

/// Identifies a Windows audio endpoint device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub is_loopback: bool,
}

/// A running application's WASAPI audio session -- lets a specific app's
/// audio (Discord, Spotify, a game) be selected as its own independent
/// recording track via Process Loopback Capture, the same mechanism the
/// "App audio only" checkbox already uses for whichever window is selected
/// as the video capture source. This is deliberately separate from that:
/// picking an app here doesn't require it to be the video source, and vice
/// versa (see `capture::audio::enumerate_app_audio_sessions`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppAudioSource {
    pub process_id: u32,
    /// e.g. "Discord.exe" -- used as the stable key for
    /// `Config::audio_device_gain` (prefixed `"app:"`) since `process_id`
    /// isn't stable across the app's own restarts.
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
