use crate::capture::audio::{TARGET_CHANNELS, TARGET_SAMPLE_RATE};
use crate::encode::writer::{make_audio_input_type, make_audio_output_type, make_sample};
use crate::error::AppError;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use windows::Win32::Media::MediaFoundation::{
    IMFMediaType, IMFSample, IMFSinkWriter, IMFSourceReader, MF_MT_AUDIO_BITS_PER_SAMPLE,
    MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_VERSION, MFAudioFormat_PCM, MFCreateMediaType,
    MFCreateSinkWriterFromURL, MFCreateSourceReaderFromURL, MFMediaType_Audio, MFMediaType_Video,
    MFSTARTUP_FULL, MFShutdown, MFStartup,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::HSTRING;

/// Media Foundation's sink writer does not guarantee output track order matches
/// `AddStream` call order — it can depend on which encoder actually flushes first.
/// Never assume "stream 0 = video, 1.. = audio"; always find streams by major type.
pub(crate) struct StreamLayout {
    pub(crate) video: u32,
    pub(crate) audio: Vec<u32>,
}

pub(crate) unsafe fn discover_stream_layout(
    reader: &IMFSourceReader,
) -> Result<StreamLayout, AppError> {
    unsafe {
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
        let video =
            video.ok_or_else(|| AppError::Encode("no video stream found in input".into()))?;
        Ok(StreamLayout { video, audio })
    }
}

/// Builds an `IMFSinkWriter` for `output_url`, adds `video_type` as one stream
/// and each of `audio_types` as a subsequent stream (compressed passthrough --
/// same media type in and out, no transcoding), then calls `BeginWriting`.
/// Shared by `remux` and `highlight_export::concat_and_trim`, which otherwise
/// use the returned sink stream ids differently: `remux` maps them back to
/// source stream indices for its interleaved copy loop, `concat_and_trim`
/// just needs them in the same order as `audio_types` to write each segment's
/// samples in turn.
pub(crate) unsafe fn build_passthrough_sink_writer(
    output_url: &HSTRING,
    video_type: &IMFMediaType,
    audio_types: &[IMFMediaType],
) -> Result<(IMFSinkWriter, u32, Vec<u32>), AppError> {
    unsafe {
        let writer: IMFSinkWriter = MFCreateSinkWriterFromURL(output_url, None, None)
            .map_err(|e| AppError::Encode(format!("MFCreateSinkWriterFromURL: {e}")))?;

        let video_sink = writer
            .AddStream(video_type)
            .map_err(|e| AppError::Encode(format!("AddStream(video): {e}")))?;
        writer
            .SetInputMediaType(video_sink, video_type, None)
            .map_err(|e| AppError::Encode(format!("SetInputMediaType(video): {e}")))?;

        let mut audio_sinks = Vec::with_capacity(audio_types.len());
        for (i, audio_type) in audio_types.iter().enumerate() {
            let asink = writer
                .AddStream(audio_type)
                .map_err(|e| AppError::Encode(format!("AddStream(audio[{i}]): {e}")))?;
            writer
                .SetInputMediaType(asink, audio_type, None)
                .map_err(|e| AppError::Encode(format!("SetInputMediaType(audio[{i}]): {e}")))?;
            audio_sinks.push(asink);
        }

        writer
            .BeginWriting()
            .map_err(|e| AppError::Encode(format!("BeginWriting: {e}")))?;

        Ok((writer, video_sink, audio_sinks))
    }
}

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

/// How many audio streams a finished recording actually contains -- read
/// directly from the file itself rather than trusted from whatever devices
/// were selected before recording started, since a device selected at record
/// time isn't a guarantee it actually produced a stream in the output (e.g. a
/// device disconnecting mid-recording). The export UI uses this to build its
/// track-selection checkboxes and to decide whether exporting is even
/// meaningful (nothing to select between with fewer than two tracks).
pub fn count_audio_tracks(path: &Path) -> Result<usize, AppError> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL)
            .map_err(|e| AppError::Encode(format!("MFStartup: {e}")))?;
        let result = (|| {
            let url = HSTRING::from(
                path.to_str()
                    .ok_or_else(|| AppError::Encode("input path not valid UTF-8".into()))?,
            );
            let reader: IMFSourceReader = MFCreateSourceReaderFromURL(&url, None)
                .map_err(|e| AppError::Encode(format!("MFCreateSourceReaderFromURL: {e}")))?;
            Ok(discover_stream_layout(&reader)?.audio.len())
        })();
        let _ = MFShutdown();
        result
    }
}

/// Like `remux`, but decodes the selected audio tracks to PCM and sums them
/// into a single mixed track instead of keeping each as its own stream --
/// for platforms (e.g. YouTube) that only ever play one audio track from an
/// uploaded file and silently ignore the rest. Video is still a lossless
/// compressed passthrough copy, same as `remux`.
///
/// Fewer than 2 selected tracks has nothing to mix, so this just delegates
/// to `remux` (0 tracks -> video-only, 1 track -> that track unchanged).
pub fn mix_tracks(
    input: &Path,
    output: &Path,
    audio_track_indices: &[usize],
) -> Result<PathBuf, AppError> {
    if audio_track_indices.len() < 2 {
        return remux(input, output, audio_track_indices);
    }
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL)
            .map_err(|e| AppError::Encode(format!("MFStartup: {e}")))?;
        let result = do_mix(input, output, audio_track_indices);
        let _ = MFShutdown();
        result
    }
}

/// Sums `tracks` (each `(frame_offset, interleaved_samples)`, aligned by
/// each track's own start time so tracks that began capturing at different
/// moments -- e.g. process-loopback's async activation delay -- still line
/// up correctly) into one interleaved buffer sized to whichever track
/// reaches furthest, then peak-normalizes only if the sum would clip.
/// Unity gain otherwise, so two tracks that never overlap loudly (the
/// common case) don't lose volume for no reason.
fn sum_and_normalize(tracks: &[(usize, Vec<i16>)], channels: usize) -> Vec<i16> {
    let mut acc: Vec<i32> = Vec::new();
    for (frame_offset, samples) in tracks {
        let start = frame_offset * channels;
        let needed = start + samples.len();
        if acc.len() < needed {
            acc.resize(needed, 0);
        }
        for (i, &s) in samples.iter().enumerate() {
            acc[start + i] += s as i32;
        }
    }
    let peak = acc.iter().map(|&v| v.unsigned_abs()).max().unwrap_or(0);
    let scale = if peak > i16::MAX as u32 {
        i16::MAX as f64 / peak as f64
    } else {
        1.0
    };
    acc.iter()
        .map(|&v| {
            ((v as f64) * scale)
                .round()
                .clamp(i16::MIN as f64, i16::MAX as f64) as i16
        })
        .collect()
}

unsafe fn do_mix(
    input: &Path,
    output: &Path,
    audio_track_indices: &[usize],
) -> Result<PathBuf, AppError> {
    unsafe {
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

        let probe_reader: IMFSourceReader = MFCreateSourceReaderFromURL(&input_url, None)
            .map_err(|e| AppError::Encode(format!("MFCreateSourceReaderFromURL probe: {e}")))?;
        let layout = discover_stream_layout(&probe_reader)?;
        let video_type: IMFMediaType = probe_reader
            .GetNativeMediaType(layout.video, 0)
            .map_err(|e| AppError::Encode(format!("GetNativeMediaType(video): {e}")))?;

        let mix_rate = TARGET_SAMPLE_RATE;
        let mix_channels = TARGET_CHANNELS as usize;

        // ── Decode each selected track to PCM, forcing the same target
        // rate/channels this app always encodes at, so tracks line up
        // frame-for-frame without needing a separate resample step here. ──
        let mut decoded_tracks: Vec<(usize, Vec<i16>)> = Vec::new();
        for &idx in audio_track_indices {
            let src_idx = *layout.audio.get(idx).ok_or_else(|| {
                AppError::Encode(format!("no audio track at logical index {idx}"))
            })?;

            let reader: IMFSourceReader =
                MFCreateSourceReaderFromURL(&input_url, None).map_err(|e| {
                    AppError::Encode(format!("MFCreateSourceReaderFromURL audio[{idx}]: {e}"))
                })?;
            reader
                .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS, false)
                .map_err(|e| AppError::Encode(format!("SetStreamSelection(all,false): {e}")))?;
            reader
                .SetStreamSelection(src_idx, true)
                .map_err(|e| AppError::Encode(format!("SetStreamSelection(audio {idx}): {e}")))?;

            let pcm_type = MFCreateMediaType()
                .map_err(|e| AppError::Encode(format!("MFCreateMediaType: {e}")))?;
            pcm_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
                .map_err(|e| AppError::Encode(format!("SetGUID major_type: {e}")))?;
            pcm_type
                .SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)
                .map_err(|e| AppError::Encode(format!("SetGUID PCM: {e}")))?;
            pcm_type
                .SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, mix_rate)
                .map_err(|e| AppError::Encode(format!("SetUINT32 rate: {e}")))?;
            pcm_type
                .SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, mix_channels as u32)
                .map_err(|e| AppError::Encode(format!("SetUINT32 channels: {e}")))?;
            pcm_type
                .SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)
                .map_err(|e| AppError::Encode(format!("SetUINT32 bits: {e}")))?;
            reader
                .SetCurrentMediaType(src_idx, None, &pcm_type)
                .map_err(|e| AppError::Encode(format!("SetCurrentMediaType(audio {idx}): {e}")))?;

            let mut samples: Vec<i16> = Vec::new();
            let mut first_frame_offset: Option<usize> = None;

            loop {
                let mut actual_idx: u32 = 0;
                let mut stream_flags: u32 = 0;
                let mut timestamp: i64 = 0;
                let mut sample: Option<IMFSample> = None;
                reader
                    .ReadSample(
                        src_idx,
                        0,
                        Some(&mut actual_idx),
                        Some(&mut stream_flags),
                        Some(&mut timestamp),
                        Some(&mut sample),
                    )
                    .map_err(|e| AppError::Encode(format!("ReadSample(audio {idx}): {e}")))?;

                if stream_flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
                    break;
                }
                let Some(sample) = sample else { continue };
                let buffer = sample.ConvertToContiguousBuffer().map_err(|e| {
                    AppError::Encode(format!("ConvertToContiguousBuffer(audio {idx}): {e}"))
                })?;
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut cur_len = 0u32;
                buffer
                    .Lock(&mut data, None, Some(&mut cur_len))
                    .map_err(|e| AppError::Encode(format!("Lock(audio {idx}): {e}")))?;
                let bytes = std::slice::from_raw_parts(data, cur_len as usize);

                if first_frame_offset.is_none() {
                    let frame_offset = (timestamp * mix_rate as i64 / 10_000_000).max(0) as usize;
                    first_frame_offset = Some(frame_offset);
                }
                for chunk in bytes.chunks_exact(2) {
                    samples.push(i16::from_le_bytes([chunk[0], chunk[1]]));
                }

                buffer
                    .Unlock()
                    .map_err(|e| AppError::Encode(format!("Unlock(audio {idx}): {e}")))?;
            }

            decoded_tracks.push((first_frame_offset.unwrap_or(0), samples));
        }

        // Every selected track was empty (e.g. all muted/silent the whole
        // recording) -- fall back to a video-only file rather than writing
        // a zero-length audio stream.
        if decoded_tracks.iter().all(|(_, s)| s.is_empty()) {
            return do_remux(input, output, &[]);
        }

        let mixed = sum_and_normalize(&decoded_tracks, mix_channels);

        // ── Sink writer: video passthrough + one transcoded (PCM->AAC) audio stream ──
        let writer: IMFSinkWriter = MFCreateSinkWriterFromURL(&output_url, None, None)
            .map_err(|e| AppError::Encode(format!("MFCreateSinkWriterFromURL: {e}")))?;
        let video_sink = writer
            .AddStream(&video_type)
            .map_err(|e| AppError::Encode(format!("AddStream(video): {e}")))?;
        writer
            .SetInputMediaType(video_sink, &video_type, None)
            .map_err(|e| AppError::Encode(format!("SetInputMediaType(video): {e}")))?;
        let audio_out = make_audio_output_type(mix_rate, mix_channels as u16)?;
        let audio_in = make_audio_input_type(mix_rate, mix_channels as u16)?;
        let audio_sink = writer
            .AddStream(&audio_out)
            .map_err(|e| AppError::Encode(format!("AddStream(audio): {e}")))?;
        writer
            .SetInputMediaType(audio_sink, &audio_in, None)
            .map_err(|e| AppError::Encode(format!("SetInputMediaType(audio): {e}")))?;
        writer
            .BeginWriting()
            .map_err(|e| AppError::Encode(format!("BeginWriting: {e}")))?;

        // Video: compressed passthrough copy, same technique as `do_remux`.
        probe_reader
            .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS, false)
            .map_err(|e| AppError::Encode(format!("SetStreamSelection(all,false): {e}")))?;
        probe_reader
            .SetStreamSelection(layout.video, true)
            .map_err(|e| AppError::Encode(format!("SetStreamSelection(video): {e}")))?;
        loop {
            let mut actual_idx: u32 = 0;
            let mut stream_flags: u32 = 0;
            let mut timestamp: i64 = 0;
            let mut sample: Option<IMFSample> = None;
            probe_reader
                .ReadSample(
                    layout.video,
                    0,
                    Some(&mut actual_idx),
                    Some(&mut stream_flags),
                    Some(&mut timestamp),
                    Some(&mut sample),
                )
                .map_err(|e| AppError::Encode(format!("ReadSample(video): {e}")))?;
            if stream_flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
                break;
            }
            if let Some(s) = sample {
                writer
                    .WriteSample(video_sink, &s)
                    .map_err(|e| AppError::Encode(format!("WriteSample(video): {e}")))?;
            }
        }

        // Mixed audio, written in ~100ms chunks.
        const CHUNK_FRAMES: usize = 4800;
        let total_frames = mixed.len() / mix_channels;
        let mut frame = 0usize;
        while frame < total_frames {
            let end = (frame + CHUNK_FRAMES).min(total_frames);
            let chunk_bytes: Vec<u8> = mixed[frame * mix_channels..end * mix_channels]
                .iter()
                .flat_map(|s| s.to_le_bytes())
                .collect();
            let pts_hns = (frame as i64 * 10_000_000) / mix_rate as i64;
            let duration_hns = ((end - frame) as i64 * 10_000_000) / mix_rate as i64;
            let sample = make_sample(&chunk_bytes, pts_hns, duration_hns)?;
            writer
                .WriteSample(audio_sink, &sample)
                .map_err(|e| AppError::Encode(format!("WriteSample(audio): {e}")))?;
            frame = end;
        }

        writer
            .Finalize()
            .map_err(|e| AppError::Encode(format!("Finalize: {e}")))?;

        Ok(output.to_path_buf())
    }
}

unsafe fn do_remux(
    input: &Path,
    output: &Path,
    audio_track_indices: &[usize],
) -> Result<PathBuf, AppError> {
    unsafe {
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
            let src_idx = *layout.audio.get(idx).ok_or_else(|| {
                AppError::Encode(format!("no audio track at logical index {idx}"))
            })?;
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
        let audio_sink_types: Vec<IMFMediaType> =
            audio_types.iter().map(|(_, t)| t.clone()).collect();
        let (writer, vsink, audio_sinks) =
            build_passthrough_sink_writer(&output_url, &video_type, &audio_sink_types)?;

        // source stream index → sink stream index
        let mut source_to_sink: HashMap<u32, u32> = HashMap::new();
        source_to_sink.insert(layout.video, vsink);
        for ((src_idx, _), asink) in audio_types.iter().zip(audio_sinks.iter()) {
            source_to_sink.insert(*src_idx, *asink);
        }

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
                writer.WriteSample(sink_idx, &s).map_err(|e| {
                    AppError::Encode(format!("WriteSample(stream {actual_idx}): {e}"))
                })?;
            }
        }

        writer
            .Finalize()
            .map_err(|e| AppError::Encode(format!("Finalize: {e}")))?;

        Ok(output.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::encode::RecordingWriter;
    use crate::types::{AudioSamples, TrackId, VideoFrame};
    use std::time::Duration;

    fn make_test_mp4(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("source.mp4");
        let writer = RecordingWriter::new(
            &path,
            64,
            64,
            30,
            "h264",
            500_000,
            &[(48000u32, 2u16)],
            false,
        )
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

    fn make_two_track_test_mp4(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("two_track_source.mp4");
        let writer = RecordingWriter::new(
            &path,
            64,
            64,
            30,
            "h264",
            500_000,
            &[(48000u32, 2u16), (48000u32, 2u16)],
            false,
        )
        .expect("RecordingWriter::new");
        writer.begin_writing().expect("begin_writing");
        writer
            .write_video(VideoFrame {
                pts: Duration::ZERO,
                data: vec![0u8; 64 * 64 * 4],
            })
            .expect("write_video");
        for track_idx in 0..2 {
            writer
                .write_audio(
                    track_idx,
                    AudioSamples {
                        track_id: TrackId::new(track_idx as u32),
                        pts: Duration::ZERO,
                        samples: vec![0.1f32; 480 * 2],
                        sample_rate: 48000,
                        channels: 2,
                    },
                )
                .expect("write_audio");
        }
        writer.finalize().expect("finalize")
    }

    #[test]
    fn mix_tracks_with_two_selected_produces_exactly_one_audio_track() {
        let dir = tempfile::tempdir().unwrap();
        let source = make_two_track_test_mp4(dir.path());
        let dest = dir.path().join("mixed.mp4");
        let result = mix_tracks(&source, &dest, &[0, 1]);
        assert!(result.is_ok(), "mix_tracks failed: {:?}", result.err());
        assert!(dest.exists(), "output file not created");
        assert!(dest.metadata().unwrap().len() > 0, "output file is empty");
        assert_eq!(
            count_audio_tracks(&dest).expect("count_audio_tracks failed"),
            1,
            "mixed output should have exactly one audio track"
        );
    }

    #[test]
    fn mix_tracks_with_fewer_than_two_selected_delegates_to_remux() {
        let dir = tempfile::tempdir().unwrap();
        let source = make_two_track_test_mp4(dir.path());

        let one_track_dest = dir.path().join("one_track.mp4");
        mix_tracks(&source, &one_track_dest, &[0]).expect("mix_tracks with 1 index failed");
        assert_eq!(
            count_audio_tracks(&one_track_dest).expect("count_audio_tracks failed"),
            1
        );

        let no_track_dest = dir.path().join("no_track.mp4");
        mix_tracks(&source, &no_track_dest, &[]).expect("mix_tracks with 0 indices failed");
        assert_eq!(
            count_audio_tracks(&no_track_dest).expect("count_audio_tracks failed"),
            0
        );
    }

    #[test]
    fn sum_and_normalize_sums_overlapping_tracks_at_unity_gain_when_no_clipping() {
        let tracks = vec![
            (0usize, vec![1000i16, -1000, 500, -500]),
            (0usize, vec![2000i16, -2000, 300, -300]),
        ];
        let result = sum_and_normalize(&tracks, 2);
        assert_eq!(result, vec![3000, -3000, 800, -800]);
    }

    #[test]
    fn sum_and_normalize_aligns_tracks_by_frame_offset() {
        // Track 1 starts at frame 0 (2 stereo frames = 4 samples), track 2
        // starts 1 frame later (offset 1 -> sample index 2) -- they should
        // only overlap on the second frame of track 1 / first frame of track 2.
        let tracks = vec![
            (0usize, vec![100i16, 100, 200, 200]),
            (1usize, vec![50i16, 50]),
        ];
        let result = sum_and_normalize(&tracks, 2);
        assert_eq!(result, vec![100, 100, 250, 250]);
    }

    #[test]
    fn sum_and_normalize_scales_down_only_when_sum_would_clip() {
        let tracks = vec![(0usize, vec![30000i16]), (0usize, vec![30000i16])];
        let result = sum_and_normalize(&tracks, 1);
        // 30000 + 30000 = 60000, clips i16 (max 32767) -- must be scaled
        // down to exactly the max, not left clipped/wrapped.
        assert_eq!(result, vec![i16::MAX]);
    }

    #[test]
    fn sum_and_normalize_empty_input_produces_empty_output() {
        let result = sum_and_normalize(&[], 2);
        assert!(result.is_empty());
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

    #[test]
    fn count_audio_tracks_finds_the_single_audio_track() {
        let dir = tempfile::tempdir().unwrap();
        let source = make_test_mp4(dir.path());
        assert_eq!(
            count_audio_tracks(&source).expect("count_audio_tracks failed"),
            1
        );
    }

    #[test]
    fn count_audio_tracks_is_zero_for_a_video_only_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("video_only_source.mp4");
        let writer = RecordingWriter::new(&path, 64, 64, 30, "h264", 500_000, &[], false)
            .expect("RecordingWriter::new");
        writer.begin_writing().expect("begin_writing");
        writer
            .write_video(VideoFrame {
                pts: Duration::ZERO,
                data: vec![0u8; 64 * 64 * 4],
            })
            .expect("write_video");
        let path = writer.finalize().expect("finalize");

        assert_eq!(
            count_audio_tracks(&path).expect("count_audio_tracks failed"),
            0
        );
    }
}
