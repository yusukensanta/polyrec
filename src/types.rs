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
    Stopping,
    Exporting,
}

#[derive(Debug, Clone)]
pub struct CaptureSource {
    pub process_id: u32,
    pub window_title: String,
    pub exe_name: String,
}

/// Captured video frame — CPU-mapped BGRA bytes (Plan 2).
/// Plan 3 will add GPU texture path for NVENC.
#[derive(Debug)]
pub struct VideoFrame {
    pub pts: Duration,
    pub width: u32,
    pub height: u32,
    /// Raw BGRA pixel data, row-major, width*height*4 bytes.
    pub data: Vec<u8>,
}

impl VideoFrame {
    pub fn byte_len(&self) -> usize {
        (self.width * self.height * 4) as usize
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
    fn video_frame_byte_len() {
        let frame = VideoFrame {
            pts: Duration::ZERO,
            width: 1920,
            height: 1080,
            data: vec![0u8; 1920 * 1080 * 4],
        };
        assert_eq!(frame.byte_len(), 1920 * 1080 * 4);
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
