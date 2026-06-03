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
