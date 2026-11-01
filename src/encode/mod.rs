pub mod actor;
pub(crate) mod cfr;
pub mod gpu;
pub mod highlight_export;
pub(crate) mod mf;
pub mod remux;
pub mod writer;
pub use writer::{EncoderParams, RecordingWriter};

use crate::types::{AudioSamples, VideoFrame};
use tokio::sync::OwnedSemaphorePermit;

pub enum RecordingCommand {
    /// A captured frame and the in-flight slot it occupies (see
    /// `actor::spawn_video_pump`) -- the slot frees up when the actor drops
    /// the command after writing the frame.
    WriteVideo(VideoFrame, VideoSlot),
    WriteAudio(AudioSamples),
    Stop,
}

impl RecordingCommand {
    /// `WriteVideo` for a frame that didn't come through a video pump, so
    /// it occupies no in-flight slot (tests feeding an actor directly).
    #[cfg(test)]
    pub fn video(frame: VideoFrame) -> Self {
        Self::WriteVideo(frame, VideoSlot { _permit: None })
    }
}

/// One of a video pump's in-flight slots -- see `actor::spawn_video_pump`.
/// Only ever held, never read: dropping it is what frees the slot.
pub struct VideoSlot {
    _permit: Option<OwnedSemaphorePermit>,
}

impl VideoSlot {
    pub(crate) fn held(permit: OwnedSemaphorePermit) -> Self {
        Self {
            _permit: Some(permit),
        }
    }
}
