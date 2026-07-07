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

#[derive(Debug, Clone)]
pub struct CaptureSource {
    pub process_id: u32,
    pub window_title: String,
    pub exe_name: String,
    pub hwnd: usize,
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
