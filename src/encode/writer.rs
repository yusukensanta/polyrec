use crate::error::AppError;
use crate::types::{AudioSamples, VideoFrame};
use std::path::{Path, PathBuf};
use windows::Win32::Media::MediaFoundation::{
    IMFMediaType, IMFSinkWriter, MFAudioFormat_AAC, MFAudioFormat_PCM, MFCreateMediaType,
    MFCreateMemoryBuffer, MFCreateSample, MFCreateSinkWriterFromURL, MFMediaType_Audio,
    MFMediaType_Video, MFShutdown, MFStartup, MFVideoFormat_ARGB32, MFVideoFormat_H264,
    MFVideoInterlace_Progressive, MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE,
    MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND,
    MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
    MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_VERSION, MFSTARTUP_FULL,
};
use windows::core::HSTRING;

const VIDEO_BITRATE_BPS: u32 = 8_000_000;
const AUDIO_BITRATE_BPS: u32 = 192_000;

pub struct RecordingWriter {
    writer: IMFSinkWriter,
    video_stream: u32,
    audio_streams: Vec<u32>,
    output_path: PathBuf,
    fps: u32,
}

impl RecordingWriter {
    pub fn new(
        output_path: &Path,
        width: u32,
        height: u32,
        fps: u32,
        audio_tracks: &[(u32, u16)],
    ) -> Result<Self, AppError> {
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_FULL)
                .map_err(|e| AppError::Encode(format!("MFStartup: {e}")))?;

            let url = HSTRING::from(output_path.to_str().unwrap_or("output.mp4"));
            let writer: IMFSinkWriter = MFCreateSinkWriterFromURL(&url, None, None)
                .map_err(|e| AppError::Encode(format!("MFCreateSinkWriterFromURL: {e}")))?;

            let video_out = make_video_output_type(width, height, fps)?;
            let video_in = make_video_input_type(width, height, fps)?;
            let video_stream = writer
                .AddStream(&video_out)
                .map_err(|e| AppError::Encode(format!("AddStream video: {e}")))?;
            writer
                .SetInputMediaType(video_stream, &video_in, None)
                .map_err(|e| AppError::Encode(format!("SetInputMediaType video: {e}")))?;

            let mut audio_streams = Vec::new();
            for (sample_rate, channels) in audio_tracks {
                let audio_out = make_audio_output_type(*sample_rate, *channels)?;
                let audio_in = make_audio_input_type(*sample_rate, *channels)?;
                let idx = writer
                    .AddStream(&audio_out)
                    .map_err(|e| AppError::Encode(format!("AddStream audio: {e}")))?;
                writer
                    .SetInputMediaType(idx, &audio_in, None)
                    .map_err(|e| AppError::Encode(format!("SetInputMediaType audio: {e}")))?;
                audio_streams.push(idx);
            }

            Ok(Self {
                writer,
                video_stream,
                audio_streams,
                output_path: output_path.to_path_buf(),
                fps,
            })
        }
    }

    pub fn begin_writing(&self) -> Result<(), AppError> {
        unsafe {
            self.writer
                .BeginWriting()
                .map_err(|e| AppError::Encode(format!("BeginWriting: {e}")))
        }
    }

    pub fn write_video(&self, frame: VideoFrame) -> Result<(), AppError> {
        let pts_hns = frame.pts.as_nanos() as i64 / 100;
        let duration_hns = 10_000_000i64 / self.fps as i64;
        let data = frame.data;
        unsafe {
            let sample = make_sample(&data, pts_hns, duration_hns)?;
            self.writer
                .WriteSample(self.video_stream, &sample)
                .map_err(|e| AppError::Encode(format!("WriteSample video: {e}")))
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
                .map_err(|e| AppError::Encode(format!("WriteSample audio[{track_idx}]: {e}")))
        }
    }

    pub fn finalize(self) -> Result<PathBuf, AppError> {
        unsafe {
            self.writer
                .Finalize()
                .map_err(|e| AppError::Encode(format!("Finalize: {e}")))?;
            MFShutdown().map_err(|e| AppError::Encode(format!("MFShutdown: {e}")))?;
        }
        Ok(self.output_path)
    }
}

unsafe fn make_video_output_type(
    width: u32,
    height: u32,
    fps: u32,
) -> Result<IMFMediaType, AppError> {
    let t =
        MFCreateMediaType().map_err(|e| AppError::Encode(format!("MFCreateMediaType: {e}")))?;
    t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
        .map_err(|e| AppError::Encode(format!("SetGUID MajorType: {e}")))?;
    t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
        .map_err(|e| AppError::Encode(format!("SetGUID H264: {e}")))?;
    t.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))
        .map_err(|e| AppError::Encode(format!("SetUINT64 frame_size: {e}")))?;
    t.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))
        .map_err(|e| AppError::Encode(format!("SetUINT64 frame_rate: {e}")))?;
    t.SetUINT32(&MF_MT_AVG_BITRATE, VIDEO_BITRATE_BPS)
        .map_err(|e| AppError::Encode(format!("SetUINT32 bitrate: {e}")))?;
    // MFVideoInterlace_Progressive = MFVideoInterlaceMode(2i32)
    t.SetUINT32(
        &MF_MT_INTERLACE_MODE,
        MFVideoInterlace_Progressive.0 as u32,
    )
    .map_err(|e| AppError::Encode(format!("SetUINT32 interlace: {e}")))?;
    Ok(t)
}

unsafe fn make_video_input_type(
    width: u32,
    height: u32,
    fps: u32,
) -> Result<IMFMediaType, AppError> {
    let t =
        MFCreateMediaType().map_err(|e| AppError::Encode(format!("MFCreateMediaType: {e}")))?;
    t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
        .map_err(|e| AppError::Encode(format!("SetGUID MajorType: {e}")))?;
    // ARGB32 matches WGC BGRA output; MF topology inserts colour-space converter automatically
    t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_ARGB32)
        .map_err(|e| AppError::Encode(format!("SetGUID ARGB32: {e}")))?;
    t.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))
        .map_err(|e| AppError::Encode(format!("SetUINT64 frame_size: {e}")))?;
    t.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))
        .map_err(|e| AppError::Encode(format!("SetUINT64 frame_rate: {e}")))?;
    t.SetUINT32(
        &MF_MT_INTERLACE_MODE,
        MFVideoInterlace_Progressive.0 as u32,
    )
    .map_err(|e| AppError::Encode(format!("SetUINT32 interlace: {e}")))?;
    Ok(t)
}

unsafe fn make_audio_output_type(
    sample_rate: u32,
    channels: u16,
) -> Result<IMFMediaType, AppError> {
    let t =
        MFCreateMediaType().map_err(|e| AppError::Encode(format!("MFCreateMediaType: {e}")))?;
    t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
        .map_err(|e| AppError::Encode(format!("SetGUID audio major: {e}")))?;
    t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)
        .map_err(|e| AppError::Encode(format!("SetGUID AAC: {e}")))?;
    t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate)
        .map_err(|e| AppError::Encode(format!("sample_rate out: {e}")))?;
    t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels as u32)
        .map_err(|e| AppError::Encode(format!("channels out: {e}")))?;
    t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AUDIO_BITRATE_BPS / 8)
        .map_err(|e| AppError::Encode(format!("audio bitrate out: {e}")))?;
    Ok(t)
}

unsafe fn make_audio_input_type(
    sample_rate: u32,
    channels: u16,
) -> Result<IMFMediaType, AppError> {
    let block_align = channels as u32 * 2;
    let bytes_per_sec = sample_rate * block_align;
    let t =
        MFCreateMediaType().map_err(|e| AppError::Encode(format!("MFCreateMediaType: {e}")))?;
    t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
        .map_err(|e| AppError::Encode(format!("SetGUID audio major: {e}")))?;
    t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)
        .map_err(|e| AppError::Encode(format!("SetGUID PCM: {e}")))?;
    t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate)
        .map_err(|e| AppError::Encode(format!("sample_rate in: {e}")))?;
    t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels as u32)
        .map_err(|e| AppError::Encode(format!("channels in: {e}")))?;
    t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)
        .map_err(|e| AppError::Encode(format!("bits_per_sample: {e}")))?;
    t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, block_align)
        .map_err(|e| AppError::Encode(format!("block_align: {e}")))?;
    t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, bytes_per_sec)
        .map_err(|e| AppError::Encode(format!("bytes_per_sec: {e}")))?;
    Ok(t)
}

unsafe fn make_sample(
    data: &[u8],
    pts_hns: i64,
    duration_hns: i64,
) -> Result<windows::Win32::Media::MediaFoundation::IMFSample, AppError> {
    let buffer = MFCreateMemoryBuffer(data.len() as u32)
        .map_err(|e| AppError::Encode(format!("MFCreateMemoryBuffer: {e}")))?;

    let mut buf_ptr: *mut u8 = std::ptr::null_mut();
    buffer
        .Lock(&mut buf_ptr, None, None)
        .map_err(|e| AppError::Encode(format!("Lock: {e}")))?;
    std::ptr::copy_nonoverlapping(data.as_ptr(), buf_ptr, data.len());
    buffer
        .Unlock()
        .map_err(|e| AppError::Encode(format!("Unlock: {e}")))?;
    buffer
        .SetCurrentLength(data.len() as u32)
        .map_err(|e| AppError::Encode(format!("SetCurrentLength: {e}")))?;

    let sample =
        MFCreateSample().map_err(|e| AppError::Encode(format!("MFCreateSample: {e}")))?;
    sample
        .AddBuffer(&buffer)
        .map_err(|e| AppError::Encode(format!("AddBuffer: {e}")))?;
    sample
        .SetSampleTime(pts_hns)
        .map_err(|e| AppError::Encode(format!("SetSampleTime: {e}")))?;
    sample
        .SetSampleDuration(duration_hns)
        .map_err(|e| AppError::Encode(format!("SetSampleDuration: {e}")))?;
    Ok(sample)
}

fn pack_u64(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_u64_encodes_correctly() {
        assert_eq!(pack_u64(1920, 1080), 0x0000_0780_0000_0438);
        assert_eq!(pack_u64(60, 1), 0x0000_003C_0000_0001);
    }

    #[test]
    fn f32_to_pcm16_conversion_clamps() {
        let samples = vec![0.0f32, 1.0, -1.0, 2.0, -2.0];
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
}
