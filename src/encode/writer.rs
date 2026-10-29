use crate::config::Codec;
use crate::encode::mf::MfSession;
use crate::error::{AppError, ResultExt};
use crate::types::{AudioSamples, VideoFrame};
use std::path::{Path, PathBuf};
use windows::Win32::Media::MediaFoundation::{
    IMFMediaType, IMFSinkWriter, MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE,
    MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND,
    MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MFAudioFormat_AAC, MFAudioFormat_PCM,
    MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFCreateSinkWriterFromURL,
    MFMediaType_Audio, MFMediaType_Video, MFVideoFormat_ARGB32, MFVideoFormat_H264,
    MFVideoFormat_HEVC, MFVideoInterlace_Progressive,
};
use windows::core::GUID;
use windows::core::HSTRING;

const AUDIO_BITRATE_BPS: u32 = 192_000;

/// Bits per pixel per frame at the target quality — mid-range for H264 screen/gaming
/// content with real motion. A flat bitrate regardless of resolution/fps previously
/// starved anything above ~720p60, and H264's in-loop deblocking filter over-smooths
/// detail when it can't hit the target bitrate, which reads as blur rather than the
/// blockiness you'd expect from under-provisioning.
const VIDEO_BITS_PER_PIXEL_PER_FRAME: f64 = 0.1;

pub(crate) fn video_bitrate_bps(width: u32, height: u32, fps: u32) -> u32 {
    let raw = width as f64 * height as f64 * fps as f64 * VIDEO_BITS_PER_PIXEL_PER_FRAME;
    raw.round() as u32
}

/// Everything `RecordingWriter::new` needs to configure its sink writer --
/// built once per recording (or Highlight buffering session) and handed to
/// the actor that owns the writer; the Highlight actor reuses the same
/// params for every segment it rotates through.
#[derive(Debug, Clone)]
pub struct EncoderParams {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// H265 falls back to H264 if no HEVC encoder is available.
    pub codec: Codec,
    pub bitrate_bps: u32,
    /// `(sample_rate, channels)` per audio track, in track-id order.
    pub audio_tracks: Vec<(u32, u16)>,
    pub allow_hardware_encode: bool,
}

#[cfg(test)]
impl EncoderParams {
    /// The tiny software-encoded 64x64@30fps configuration the writer,
    /// remux, export and highlight tests all use.
    pub(crate) fn tiny_test(codec: Codec, audio_tracks: &[(u32, u16)]) -> Self {
        Self {
            width: 64,
            height: 64,
            fps: 30,
            codec,
            bitrate_bps: 500_000,
            audio_tracks: audio_tracks.to_vec(),
            allow_hardware_encode: false,
        }
    }
}

pub struct RecordingWriter {
    writer: IMFSinkWriter,
    video_stream: u32,
    audio_streams: Vec<u32>,
    output_path: PathBuf,
    fps: u32,
    /// Declared last so it drops last -- `writer` must be released before
    /// Media Foundation shuts down (see `MfSession`).
    _mf: MfSession,
}

impl RecordingWriter {
    pub fn new(output_path: &Path, params: &EncoderParams) -> Result<Self, AppError> {
        let EncoderParams {
            width,
            height,
            fps,
            codec,
            bitrate_bps,
            ref audio_tracks,
            allow_hardware_encode,
        } = *params;
        let mf = MfSession::start()?;
        unsafe {
            let url = HSTRING::from(output_path);
            // `allow_hardware_encode` should be true for every real recording -- false
            // here previously (unconditionally, for every caller including the actual
            // shipped app) forced Media Foundation onto a pure-software encoder even
            // when a GPU hardware MFT (NVENC/QSV/AMF) was available, which is enough
            // CPU load at 1080p60+ to visibly steal frame time from whatever's being
            // recorded (e.g. a game). Only the test suite passes false here, since
            // some CI/headless runners lack a real GPU and this keeps them exercising
            // the same known-working path they always have.
            use windows::Win32::Media::MediaFoundation::IMFAttributes;
            use windows::Win32::Media::MediaFoundation::{
                MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MFCreateAttributes,
            };
            let mut attrs_opt: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs_opt, 1).encode_ctx("MFCreateAttributes")?;
            let attrs = attrs_opt
                .ok_or_else(|| AppError::Encode("MFCreateAttributes returned None".into()))?;
            attrs
                .SetUINT32(
                    &MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS,
                    allow_hardware_encode as u32,
                )
                .encode_ctx("SetUINT32 hw_transforms")?;
            let writer: IMFSinkWriter = MFCreateSinkWriterFromURL(&url, None, Some(&attrs))
                .encode_ctx("MFCreateSinkWriterFromURL")?;

            let requested_subtype = match codec {
                Codec::H265 => MFVideoFormat_HEVC,
                Codec::H264 => MFVideoFormat_H264,
            };
            let video_out =
                make_video_output_type(width, height, fps, requested_subtype, bitrate_bps)?;
            let video_stream = match writer.AddStream(&video_out) {
                Ok(idx) => idx,
                Err(e) if requested_subtype == MFVideoFormat_HEVC => {
                    tracing::warn!("HEVC AddStream failed ({e}), falling back to H264");
                    let fallback_out = make_video_output_type(
                        width,
                        height,
                        fps,
                        MFVideoFormat_H264,
                        bitrate_bps,
                    )?;
                    writer
                        .AddStream(&fallback_out)
                        .encode_ctx("AddStream video (H264 fallback)")?
                }
                Err(e) => return Err(AppError::Encode(format!("AddStream video: {e}"))),
            };
            let video_in = make_video_input_type(width, height, fps)?;
            writer
                .SetInputMediaType(video_stream, &video_in, None)
                .encode_ctx("SetInputMediaType video")?;

            let mut audio_streams = Vec::new();
            for (sample_rate, channels) in audio_tracks {
                let audio_out = make_audio_output_type(*sample_rate, *channels)?;
                let audio_in = make_audio_input_type(*sample_rate, *channels)?;
                let idx = writer.AddStream(&audio_out).encode_ctx("AddStream audio")?;
                writer
                    .SetInputMediaType(idx, &audio_in, None)
                    .encode_ctx("SetInputMediaType audio")?;
                audio_streams.push(idx);
            }

            Ok(Self {
                writer,
                video_stream,
                audio_streams,
                output_path: output_path.to_path_buf(),
                fps,
                _mf: mf,
            })
        }
    }

    pub fn begin_writing(&self) -> Result<(), AppError> {
        unsafe { self.writer.BeginWriting().encode_ctx("BeginWriting") }
    }

    pub fn write_video(&self, frame: VideoFrame) -> Result<(), AppError> {
        let pts_hns = frame.pts.as_nanos() as i64 / 100;
        let duration_hns = 10_000_000i64 / self.fps as i64;
        let data = frame.data;
        unsafe {
            let sample = make_sample(&data, pts_hns, duration_hns)?;
            self.writer
                .WriteSample(self.video_stream, &sample)
                .encode_ctx("WriteSample video")
        }
    }

    pub fn write_audio(&self, track_idx: usize, samples: AudioSamples) -> Result<(), AppError> {
        let stream_idx = *self
            .audio_streams
            .get(track_idx)
            .ok_or_else(|| AppError::Encode(format!("no audio stream {track_idx}")))?;

        let pts_hns = samples.pts.as_nanos() as i64 / 100;
        let sample_count = samples.samples.len();
        let channels = samples.channels as usize;
        let frame_count = sample_count / channels.max(1);
        let sample_rate = samples.sample_rate as i64;
        let duration_hns = if sample_rate > 0 {
            (frame_count as i64 * 10_000_000) / sample_rate
        } else {
            0
        };

        let pcm: Vec<u8> = samples
            .samples
            .iter()
            .flat_map(|&s| {
                let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                v.to_le_bytes()
            })
            .collect();

        unsafe {
            let sample = make_sample(&pcm, pts_hns, duration_hns)?;
            self.writer
                .WriteSample(stream_idx, &sample)
                .encode_ctx(format_args!("WriteSample audio[{track_idx}]"))
        }
    }

    pub fn finalize(self) -> Result<PathBuf, AppError> {
        unsafe {
            self.writer.Finalize().encode_ctx("Finalize")?;
        }
        Ok(self.output_path)
    }
}

unsafe fn make_video_output_type(
    width: u32,
    height: u32,
    fps: u32,
    subtype: GUID,
    bitrate_bps: u32,
) -> Result<IMFMediaType, AppError> {
    unsafe {
        let t = MFCreateMediaType().encode_ctx("MFCreateMediaType")?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .encode_ctx("SetGUID MajorType")?;
        t.SetGUID(&MF_MT_SUBTYPE, &subtype)
            .encode_ctx("SetGUID subtype")?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))
            .encode_ctx("SetUINT64 frame_size")?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))
            .encode_ctx("SetUINT64 frame_rate")?;
        t.SetUINT32(&MF_MT_AVG_BITRATE, bitrate_bps)
            .encode_ctx("SetUINT32 bitrate")?;
        // MFVideoInterlace_Progressive = MFVideoInterlaceMode(2i32)
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .encode_ctx("SetUINT32 interlace")?;
        Ok(t)
    }
}

unsafe fn make_video_input_type(
    width: u32,
    height: u32,
    fps: u32,
) -> Result<IMFMediaType, AppError> {
    unsafe {
        let t = MFCreateMediaType().encode_ctx("MFCreateMediaType")?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .encode_ctx("SetGUID MajorType")?;
        // ARGB32 matches WGC BGRA output; MF topology inserts colour-space converter automatically
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_ARGB32)
            .encode_ctx("SetGUID ARGB32")?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))
            .encode_ctx("SetUINT64 frame_size")?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))
            .encode_ctx("SetUINT64 frame_rate")?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .encode_ctx("SetUINT32 interlace")?;
        // Our pixel buffer is top-down (D3D11's natural row order, preserved by the
        // straight row-by-row copy in capture/video.rs). Without this, the color
        // converter defaults to assuming bottom-up (classic DIB convention), which
        // flips the encoded video vertically. Positive stride = top-down.
        t.SetUINT32(&MF_MT_DEFAULT_STRIDE, width * 4)
            .encode_ctx("SetUINT32 default_stride")?;
        Ok(t)
    }
}

pub(crate) unsafe fn make_audio_output_type(
    sample_rate: u32,
    channels: u16,
) -> Result<IMFMediaType, AppError> {
    unsafe {
        let t = MFCreateMediaType().encode_ctx("MFCreateMediaType")?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
            .encode_ctx("SetGUID audio major")?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)
            .encode_ctx("SetGUID AAC")?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate)
            .encode_ctx("sample_rate out")?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels as u32)
            .encode_ctx("channels out")?;
        t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AUDIO_BITRATE_BPS / 8)
            .encode_ctx("audio bitrate out")?;
        Ok(t)
    }
}

pub(crate) unsafe fn make_audio_input_type(
    sample_rate: u32,
    channels: u16,
) -> Result<IMFMediaType, AppError> {
    unsafe {
        let block_align = channels as u32 * 2;
        let bytes_per_sec = sample_rate * block_align;
        let t = MFCreateMediaType().encode_ctx("MFCreateMediaType")?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
            .encode_ctx("SetGUID audio major")?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)
            .encode_ctx("SetGUID PCM")?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate)
            .encode_ctx("sample_rate in")?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels as u32)
            .encode_ctx("channels in")?;
        t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)
            .encode_ctx("bits_per_sample")?;
        t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, block_align)
            .encode_ctx("block_align")?;
        t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, bytes_per_sec)
            .encode_ctx("bytes_per_sec")?;
        Ok(t)
    }
}

pub(crate) unsafe fn make_sample(
    data: &[u8],
    pts_hns: i64,
    duration_hns: i64,
) -> Result<windows::Win32::Media::MediaFoundation::IMFSample, AppError> {
    unsafe {
        let buffer = MFCreateMemoryBuffer(data.len() as u32).encode_ctx("MFCreateMemoryBuffer")?;

        let mut buf_ptr: *mut u8 = std::ptr::null_mut();
        buffer.Lock(&mut buf_ptr, None, None).encode_ctx("Lock")?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), buf_ptr, data.len());
        buffer.Unlock().encode_ctx("Unlock")?;
        buffer
            .SetCurrentLength(data.len() as u32)
            .encode_ctx("SetCurrentLength")?;

        let sample = MFCreateSample().encode_ctx("MFCreateSample")?;
        sample.AddBuffer(&buffer).encode_ctx("AddBuffer")?;
        sample.SetSampleTime(pts_hns).encode_ctx("SetSampleTime")?;
        sample
            .SetSampleDuration(duration_hns)
            .encode_ctx("SetSampleDuration")?;
        Ok(sample)
    }
}

fn pack_u64(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_bitrate_scales_with_resolution_and_fps() {
        let p1080_60 = video_bitrate_bps(1920, 1080, 60);
        let p1440_60 = video_bitrate_bps(2560, 1440, 60);
        let p1080_30 = video_bitrate_bps(1920, 1080, 30);
        assert!(
            p1440_60 > p1080_60,
            "1440p should need a higher bitrate than 1080p at the same fps"
        );
        assert!(
            p1080_60 > p1080_30,
            "60fps should need a higher bitrate than 30fps at the same resolution"
        );
        // Sanity check against common streaming-quality guidance for 1080p60 (~8-12 Mbps).
        assert!(
            p1080_60 > 8_000_000 && p1080_60 < 16_000_000,
            "1080p60 bitrate {p1080_60} out of expected range"
        );
    }

    #[test]
    fn writer_accepts_explicit_bitrate_and_h264_codec() {
        use crate::types::VideoFrame;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("h264_test.mp4");
        let writer = RecordingWriter::new(&output, &EncoderParams::tiny_test(Codec::H264, &[]))
            .expect("RecordingWriter::new with explicit bitrate failed");
        writer.begin_writing().expect("begin_writing failed");
        writer
            .write_video(VideoFrame {
                pts: Duration::ZERO,
                data: vec![0u8; 64 * 64 * 4],
            })
            .expect("write_video failed");
        let path = writer.finalize().expect("finalize failed");
        assert!(path.metadata().unwrap().len() > 0, "output file is empty");
    }

    /// HEVC MFT availability varies by Windows version/install — this exercises the
    /// real encoder (or its automatic H264 fallback) rather than asserting a specific
    /// codec landed in the file, since either outcome is a pass for this codebase.
    #[tokio::test]
    #[ignore]
    async fn writer_accepts_h265_codec_or_falls_back_cleanly() {
        use crate::types::VideoFrame;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("h265_test.mp4");
        let writer = RecordingWriter::new(&output, &EncoderParams::tiny_test(Codec::H265, &[]))
            .expect("RecordingWriter::new with h265 (or its fallback) failed");
        writer.begin_writing().expect("begin_writing failed");
        writer
            .write_video(VideoFrame {
                pts: Duration::ZERO,
                data: vec![0u8; 64 * 64 * 4],
            })
            .expect("write_video failed");
        let path = writer.finalize().expect("finalize failed");
        assert!(path.metadata().unwrap().len() > 0, "output file is empty");
    }

    #[test]
    fn pack_u64_encodes_correctly() {
        assert_eq!(pack_u64(1920, 1080), 0x0000_0780_0000_0438);
        assert_eq!(pack_u64(60, 1), 0x0000_003C_0000_0001);
    }

    #[test]
    fn f32_to_pcm16_conversion_clamps() {
        let samples = [0.0f32, 1.0, -1.0, 2.0, -2.0];
        let pcm: Vec<i16> = samples
            .iter()
            .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
            .collect();
        assert_eq!(pcm[0], 0);
        assert_eq!(pcm[1], 32767);
        assert_eq!(pcm[2], -32767);
        assert_eq!(pcm[3], 32767);
        assert_eq!(pcm[4], -32767);
    }

    #[test]
    fn writer_creates_mp4_for_tiny_resolution() {
        use crate::types::{AudioSamples, TrackId, VideoFrame};
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("test.mp4");

        // 64×64 at 30fps, one stereo 48kHz audio track
        let audio_tracks = vec![(48000u32, 2u16)];
        let writer = RecordingWriter::new(
            &output,
            &EncoderParams::tiny_test(Codec::H264, &audio_tracks),
        );
        assert!(
            writer.is_ok(),
            "RecordingWriter::new failed: {:?}",
            writer.err()
        );
        let writer = writer.unwrap();

        writer.begin_writing().expect("begin_writing failed");

        // One blank BGRA frame (64×64×4 bytes)
        let frame = VideoFrame {
            pts: Duration::ZERO,
            data: vec![0u8; 64 * 64 * 4],
        };
        writer.write_video(frame).expect("write_video failed");

        // One 10ms audio chunk (480 samples × 2 channels at 48kHz)
        let samples = AudioSamples {
            track_id: TrackId::new(0),
            pts: Duration::ZERO,
            samples: vec![0.0f32; 480 * 2],
            sample_rate: 48000,
            channels: 2,
        };
        writer.write_audio(0, samples).expect("write_audio failed");

        let path = writer.finalize().expect("finalize failed");
        assert!(path.exists(), "output file not created");
        assert!(path.metadata().unwrap().len() > 0, "output file is empty");
    }

    /// Reproduces a real-world "Saving recording..." hang: 3 selected audio
    /// sources (e.g. 2 devices + 1 app-audio process) but the user stops
    /// almost immediately, before the app-audio track's async
    /// process-loopback activation has delivered its first buffer -- so
    /// that track's stream never receives a single `WriteSample` call.
    /// Bounded by a timeout thread rather than calling `finalize()` inline,
    /// so if the hang is still present this fails with a clear panic after
    /// 15s instead of hanging the test run indefinitely.
    #[test]
    fn finalize_does_not_hang_when_one_of_several_audio_tracks_receives_no_samples() {
        use crate::types::{AudioSamples, TrackId, VideoFrame};
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("multi_track_test.mp4");

        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let audio_tracks = vec![(48000u32, 2u16), (48000u32, 2u16), (48000u32, 2u16)];
            let writer = RecordingWriter::new(
                &output,
                &EncoderParams::tiny_test(Codec::H264, &audio_tracks),
            )
            .expect("RecordingWriter::new failed");
            writer.begin_writing().expect("begin_writing failed");

            writer
                .write_video(VideoFrame {
                    pts: Duration::ZERO,
                    data: vec![0u8; 64 * 64 * 4],
                })
                .expect("write_video failed");

            let sample_for = |track_idx: usize| AudioSamples {
                track_id: TrackId::new(track_idx as u32),
                pts: Duration::ZERO,
                samples: vec![0.0f32; 480 * 2],
                sample_rate: 48000,
                channels: 2,
            };
            // Tracks 0 and 1 (the two "devices") get a sample. Track 2 (the
            // "app-audio" slot) never does.
            writer
                .write_audio(0, sample_for(0))
                .expect("write_audio 0 failed");
            writer
                .write_audio(1, sample_for(1))
                .expect("write_audio 1 failed");

            let _ = done_tx.send(writer.finalize());
        });

        match done_rx.recv_timeout(Duration::from_secs(15)) {
            Ok(result) => {
                let path = result.expect("finalize failed");
                assert!(path.exists(), "output file not created");
            }
            Err(_) => panic!(
                "finalize() did not return within 15s -- an audio track with zero \
                 samples appears to hang IMFSinkWriter::Finalize"
            ),
        }
    }
}
