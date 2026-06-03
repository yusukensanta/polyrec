use crate::error::AppError;
use std::path::{Path, PathBuf};

/// Remux `input` into `output`, keeping the video stream and only the
/// audio tracks at the given 0-based indices (matching the order devices
/// were passed to `SessionManager::start_capture`).
/// Empty `audio_track_indices` produces a video-only file.
/// Uses MF SourceReader + SinkWriter passthrough — no re-encode.
pub fn remux(
    _input: &Path,
    _output: &Path,
    _audio_track_indices: &[usize],
) -> Result<PathBuf, AppError> {
    Err(AppError::Encode("remux: not yet implemented".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::RecordingWriter;
    use crate::types::{AudioSamples, TrackId, VideoFrame};
    use std::time::Duration;

    fn make_test_mp4(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("source.mp4");
        let writer =
            RecordingWriter::new(&path, 64, 64, 30, &[(48000u32, 2u16)]).expect("RecordingWriter::new");
        writer.begin_writing().expect("begin_writing");
        writer
            .write_video(VideoFrame {
                pts: Duration::ZERO,
                width: 64,
                height: 64,
                data: vec![0u8; 64 * 64 * 4],
            })
            .expect("write_video");
        writer
            .write_audio(
                0,
                AudioSamples {
                    track_id: TrackId::new(0),
                    pts: Duration::ZERO,
                    samples: vec![0.0f32; 480 * 2],
                    sample_rate: 48000,
                    channels: 2,
                },
            )
            .expect("write_audio");
        writer.finalize().expect("finalize")
    }

    #[test]
    fn remux_video_only_creates_output_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = make_test_mp4(dir.path());
        let dest = dir.path().join("video_only.mp4");
        let result = remux(&source, &dest, &[]);
        assert!(result.is_ok(), "remux failed: {:?}", result.err());
        assert!(dest.exists(), "output file not created");
        assert!(dest.metadata().unwrap().len() > 0, "output file is empty");
    }

    #[test]
    fn remux_with_audio_track_creates_output_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = make_test_mp4(dir.path());
        let dest = dir.path().join("with_audio.mp4");
        let result = remux(&source, &dest, &[0]);
        assert!(result.is_ok(), "remux failed: {:?}", result.err());
        assert!(dest.exists(), "output file not created");
        assert!(dest.metadata().unwrap().len() > 0, "output file is empty");
    }
}
