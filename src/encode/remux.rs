use crate::error::AppError;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use windows::Win32::Media::MediaFoundation::{
    IMFMediaType, IMFSample, IMFSinkWriter, IMFSourceReader, MFCreateSinkWriterFromURL,
    MFCreateSourceReaderFromURL, MFMediaType_Audio, MFMediaType_Video, MFShutdown, MFStartup,
    MF_MT_MAJOR_TYPE, MF_SOURCE_READERF_ENDOFSTREAM, MF_VERSION, MFSTARTUP_FULL,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::core::HSTRING;

/// Media Foundation's sink writer does not guarantee output track order matches
/// `AddStream` call order — it can depend on which encoder actually flushes first.
/// Never assume "stream 0 = video, 1.. = audio"; always find streams by major type.
struct StreamLayout {
    video: u32,
    audio: Vec<u32>,
}

unsafe fn discover_stream_layout(reader: &IMFSourceReader) -> Result<StreamLayout, AppError> { unsafe {
    let mut video = None;
    let mut audio = Vec::new();
    for i in 0.. {
        let t: IMFMediaType = match reader.GetNativeMediaType(i, 0) {
            Ok(t) => t,
            Err(_) => break,
        };
        let major = t
            .GetGUID(&MF_MT_MAJOR_TYPE)
            .map_err(|e| AppError::Encode(format!("GetGUID major_type (stream {i}): {e}")))?;
        if major == MFMediaType_Video {
            video = video.or(Some(i));
        } else if major == MFMediaType_Audio {
            audio.push(i);
        }
    }
    let video = video.ok_or_else(|| AppError::Encode("no video stream found in input".into()))?;
    Ok(StreamLayout { video, audio })
}}

// 0xFFFFFFFE = MF_SOURCE_READER_ANY_STREAM
const MF_SOURCE_READER_ANY_STREAM: u32 = 0xFFFF_FFFE;
// 0xFFFFFFFE = MF_SOURCE_READER_ALL_STREAMS (same value)
const MF_SOURCE_READER_ALL_STREAMS: u32 = 0xFFFF_FFFE;

pub fn remux(
    input: &Path,
    output: &Path,
    audio_track_indices: &[usize],
) -> Result<PathBuf, AppError> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL)
            .map_err(|e| AppError::Encode(format!("MFStartup: {e}")))?;
        let result = do_remux(input, output, audio_track_indices);
        let _ = MFShutdown();
        result
    }
}

unsafe fn do_remux(
    input: &Path,
    output: &Path,
    audio_track_indices: &[usize],
) -> Result<PathBuf, AppError> { unsafe {
    let input_url = HSTRING::from(
        input
            .to_str()
            .ok_or_else(|| AppError::Encode("input path not valid UTF-8".into()))?,
    );
    let output_url = HSTRING::from(
        output
            .to_str()
            .ok_or_else(|| AppError::Encode("output path not valid UTF-8".into()))?,
    );

    // ── Source reader ────────────────────────────────────────────────────────
    let reader: IMFSourceReader = MFCreateSourceReaderFromURL(&input_url, None)
        .map_err(|e| AppError::Encode(format!("MFCreateSourceReaderFromURL: {e}")))?;

    let layout = discover_stream_layout(&reader)?;

    // Disable all streams, then re-enable desired ones
    reader
        .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS, false)
        .map_err(|e| AppError::Encode(format!("SetStreamSelection(all,false): {e}")))?;
    reader
        .SetStreamSelection(layout.video, true)
        .map_err(|e| AppError::Encode(format!("SetStreamSelection(video): {e}")))?;
    // audio_track_indices are 0-based logical indices into the discovered audio
    // stream list (in the order they appear in the source), NOT raw stream indices —
    // the sink writer does not guarantee AddStream order matches output track order.
    for &idx in audio_track_indices {
        let src_idx = *layout
            .audio
            .get(idx)
            .ok_or_else(|| AppError::Encode(format!("no audio track at logical index {idx}")))?;
        reader
            .SetStreamSelection(src_idx, true)
            .map_err(|e| AppError::Encode(format!("SetStreamSelection(audio {idx}): {e}")))?;
    }

    // Configure each enabled stream for compressed passthrough.
    // GetNativeMediaType(stream, 0) returns the compressed type.
    // SetCurrentMediaType with that same type tells the reader to emit
    // compressed bytes without decoding.
    let video_type: IMFMediaType = reader
        .GetNativeMediaType(layout.video, 0)
        .map_err(|e| AppError::Encode(format!("GetNativeMediaType(video): {e}")))?;
    reader
        .SetCurrentMediaType(layout.video, None, &video_type)
        .map_err(|e| AppError::Encode(format!("SetCurrentMediaType(video): {e}")))?;

    let mut audio_types: Vec<(u32, IMFMediaType)> = Vec::new();
    for &idx in audio_track_indices {
        let src_idx = layout.audio[idx];
        let t: IMFMediaType = reader
            .GetNativeMediaType(src_idx, 0)
            .map_err(|e| AppError::Encode(format!("GetNativeMediaType(audio {idx}): {e}")))?;
        reader
            .SetCurrentMediaType(src_idx, None, &t)
            .map_err(|e| AppError::Encode(format!("SetCurrentMediaType(audio {idx}): {e}")))?;
        audio_types.push((src_idx, t));
    }

    // ── Sink writer ──────────────────────────────────────────────────────────
    let writer: IMFSinkWriter = MFCreateSinkWriterFromURL(&output_url, None, None)
        .map_err(|e| AppError::Encode(format!("MFCreateSinkWriterFromURL: {e}")))?;

    // source stream index → sink stream index
    let mut source_to_sink: HashMap<u32, u32> = HashMap::new();

    let vsink = writer
        .AddStream(&video_type)
        .map_err(|e| AppError::Encode(format!("AddStream(video): {e}")))?;
    writer
        .SetInputMediaType(vsink, &video_type, None)
        .map_err(|e| AppError::Encode(format!("SetInputMediaType(video): {e}")))?;
    source_to_sink.insert(layout.video, vsink);

    for (src_idx, audio_type) in &audio_types {
        let asink = writer
            .AddStream(audio_type)
            .map_err(|e| AppError::Encode(format!("AddStream(audio {src_idx}): {e}")))?;
        writer
            .SetInputMediaType(asink, audio_type, None)
            .map_err(|e| AppError::Encode(format!("SetInputMediaType(audio {src_idx}): {e}")))?;
        source_to_sink.insert(*src_idx, asink);
    }

    writer
        .BeginWriting()
        .map_err(|e| AppError::Encode(format!("BeginWriting: {e}")))?;

    // ── Read / write loop ────────────────────────────────────────────────────
    let total_enabled = 1 + audio_track_indices.len();
    let mut done_streams: HashSet<u32> = HashSet::new();

    loop {
        let mut actual_idx: u32 = 0;
        let mut stream_flags: u32 = 0;
        let mut timestamp: i64 = 0;
        let mut sample: Option<IMFSample> = None;

        reader
            .ReadSample(
                MF_SOURCE_READER_ANY_STREAM,
                0,
                Some(&mut actual_idx),
                Some(&mut stream_flags),
                Some(&mut timestamp),
                Some(&mut sample),
            )
            .map_err(|e| AppError::Encode(format!("ReadSample: {e}")))?;

        if stream_flags & 1 != 0 {
            // MF_SOURCE_READERF_ERROR on this stream — abort
            return Err(AppError::Encode(format!(
                "ReadSample: stream {actual_idx} reported error"
            )));
        }

        // MF_SOURCE_READERF_ENDOFSTREAM = 2
        if stream_flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
            done_streams.insert(actual_idx);
            if done_streams.len() >= total_enabled {
                break;
            }
            continue;
        }

        if let (Some(s), Some(&sink_idx)) = (sample, source_to_sink.get(&actual_idx)) {
            writer
                .WriteSample(sink_idx, &s)
                .map_err(|e| AppError::Encode(format!("WriteSample(stream {actual_idx}): {e}")))?;
        }
    }

    writer
        .Finalize()
        .map_err(|e| AppError::Encode(format!("Finalize: {e}")))?;

    Ok(output.to_path_buf())
}}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::encode::RecordingWriter;
    use crate::types::{AudioSamples, TrackId, VideoFrame};
    use std::time::Duration;

    fn make_test_mp4(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("source.mp4");
        let writer = RecordingWriter::new(&path, 64, 64, 30, "h264", 500_000, &[(48000u32, 2u16)], false)
            .expect("RecordingWriter::new");
        writer.begin_writing().expect("begin_writing");
        writer
            .write_video(VideoFrame {
                pts: Duration::ZERO,
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
