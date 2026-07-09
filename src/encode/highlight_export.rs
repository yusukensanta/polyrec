//! Concatenates the highlight buffer's rotating segment files into one output
//! file, trimming the oldest contributing segment so the result is close to
//! the requested window. Sibling to `remux.rs` rather than an extension of
//! it -- `remux::remux` is a single-input stream copy (used for the
//! post-recording track-selection export); this reads from *multiple* input
//! files into one sink, which needs its own PTS-rebasing logic across the
//! seams that a single-input remux never has to do.

use crate::encode::remux::{discover_stream_layout, StreamLayout};
use crate::error::AppError;
use crate::highlight::SegmentInfo;
use std::path::{Path, PathBuf};
use std::time::Duration;
use windows::core::{GUID, HSTRING};
use windows::Win32::Media::MediaFoundation::{
    IMFMediaType, IMFSample, IMFSinkWriter, IMFSourceReader, MFCreateSinkWriterFromURL,
    MFCreateSourceReaderFromURL, MFShutdown, MFStartup, MF_SOURCE_READERF_ENDOFSTREAM, MF_VERSION,
    MFSTARTUP_FULL,
};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::Variant::VT_I8;

const MF_SOURCE_READER_ALL_STREAMS: u32 = 0xFFFF_FFFE;
const MF_SOURCE_READER_ANY_STREAM: u32 = 0xFFFF_FFFE;

pub fn concat_and_trim(
    segments: &[SegmentInfo],
    target_seconds: u32,
    output: &Path,
) -> Result<PathBuf, AppError> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL)
            .map_err(|e| AppError::Encode(format!("MFStartup: {e}")))?;
        let result = do_concat_and_trim(segments, target_seconds, output);
        let _ = MFShutdown();
        result
    }
}

/// Opens `path` for compressed passthrough (no decode/re-encode) on its video
/// stream and every audio stream it contains, mirroring `remux.rs`'s
/// `do_remux` setup -- returns the reader plus the native media types
/// actually negotiated, since the sink writer needs those exact types.
unsafe fn open_passthrough_reader(
    path: &Path,
) -> Result<(IMFSourceReader, StreamLayout, IMFMediaType, Vec<IMFMediaType>), AppError> { unsafe {
    let url = HSTRING::from(
        path.to_str()
            .ok_or_else(|| AppError::Encode("segment path not valid UTF-8".into()))?,
    );
    let reader: IMFSourceReader = MFCreateSourceReaderFromURL(&url, None)
        .map_err(|e| AppError::Encode(format!("MFCreateSourceReaderFromURL({}): {e}", path.display())))?;

    let layout = discover_stream_layout(&reader)?;

    reader
        .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS, false)
        .map_err(|e| AppError::Encode(format!("SetStreamSelection(all,false): {e}")))?;
    reader
        .SetStreamSelection(layout.video, true)
        .map_err(|e| AppError::Encode(format!("SetStreamSelection(video): {e}")))?;
    for &a in &layout.audio {
        reader
            .SetStreamSelection(a, true)
            .map_err(|e| AppError::Encode(format!("SetStreamSelection(audio {a}): {e}")))?;
    }

    let video_type: IMFMediaType = reader
        .GetNativeMediaType(layout.video, 0)
        .map_err(|e| AppError::Encode(format!("GetNativeMediaType(video): {e}")))?;
    reader
        .SetCurrentMediaType(layout.video, None, &video_type)
        .map_err(|e| AppError::Encode(format!("SetCurrentMediaType(video): {e}")))?;

    let mut audio_types = Vec::new();
    for &a in &layout.audio {
        let t: IMFMediaType = reader
            .GetNativeMediaType(a, 0)
            .map_err(|e| AppError::Encode(format!("GetNativeMediaType(audio {a}): {e}")))?;
        reader
            .SetCurrentMediaType(a, None, &t)
            .map_err(|e| AppError::Encode(format!("SetCurrentMediaType(audio {a}): {e}")))?;
        audio_types.push(t);
    }

    Ok((reader, layout, video_type, audio_types))
}}

/// `VT_I8` `PROPVARIANT` carrying a 100ns-unit time position, for
/// `IMFSourceReader::SetCurrentPosition` -- built the same manual way
/// `capture/audio.rs` builds its `VT_BLOB` activation-params variant, since
/// windows-rs 0.62 makes `PROPVARIANT` a plain public struct rather than
/// offering a safe constructor for every variant type.
fn time_propvariant(hns: i64) -> PROPVARIANT {
    PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: core::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_I8,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 { hVal: hns },
            }),
        },
    }
}

unsafe fn do_concat_and_trim(
    segments: &[SegmentInfo],
    target_seconds: u32,
    output: &Path,
) -> Result<PathBuf, AppError> { unsafe {
    if segments.is_empty() {
        return Err(AppError::Encode("no highlight segments available to save".into()));
    }

    // Walk newest-to-oldest accumulating duration until the target window is
    // covered (or everything available is used, whichever is less), then
    // restore oldest-to-newest order for the actual concatenation pass.
    let target = Duration::from_secs(target_seconds as u64);
    let mut total = Duration::ZERO;
    let mut contributing: Vec<&SegmentInfo> = Vec::new();
    for seg in segments.iter().rev() {
        contributing.push(seg);
        total += seg.duration;
        if total >= target {
            break;
        }
    }
    contributing.reverse();

    // How much to trim off the front of the oldest contributing segment --
    // zero if the buffer doesn't even contain a full `target_seconds` yet.
    let skip = total.saturating_sub(target);
    let skip_hns: i64 = (skip.as_nanos() / 100).min(i64::MAX as u128) as i64;

    let output_url = HSTRING::from(
        output
            .to_str()
            .ok_or_else(|| AppError::Encode("output path not valid UTF-8".into()))?,
    );

    // ── Sink writer, built from the oldest contributing segment's types --
    // every segment shares identical encode settings (same buffering
    // session), so its native compressed types stand in for all of them. ──
    let (first_reader, first_layout, video_type, audio_types) =
        open_passthrough_reader(contributing[0].path.as_path())?;
    if skip_hns > 0 {
        first_reader
            .SetCurrentPosition(&GUID::zeroed(), &time_propvariant(skip_hns))
            .map_err(|e| AppError::Encode(format!("SetCurrentPosition({skip_hns}): {e}")))?;
    }

    let writer: IMFSinkWriter = MFCreateSinkWriterFromURL(&output_url, None, None)
        .map_err(|e| AppError::Encode(format!("MFCreateSinkWriterFromURL: {e}")))?;
    let video_sink = writer
        .AddStream(&video_type)
        .map_err(|e| AppError::Encode(format!("AddStream(video): {e}")))?;
    writer
        .SetInputMediaType(video_sink, &video_type, None)
        .map_err(|e| AppError::Encode(format!("SetInputMediaType(video): {e}")))?;
    let mut audio_sinks = Vec::new();
    for t in &audio_types {
        let asink = writer
            .AddStream(t)
            .map_err(|e| AppError::Encode(format!("AddStream(audio): {e}")))?;
        writer
            .SetInputMediaType(asink, t, None)
            .map_err(|e| AppError::Encode(format!("SetInputMediaType(audio): {e}")))?;
        audio_sinks.push(asink);
    }
    writer
        .BeginWriting()
        .map_err(|e| AppError::Encode(format!("BeginWriting: {e}")))?;

    // ── Copy each contributing segment in order, re-basing timestamps so ──
    // the output is one continuously increasing timeline across the seams.
    let mut running_offset_hns: i64 = 0;
    for (i, seg) in contributing.iter().enumerate() {
        let baseline_hns = if i == 0 { skip_hns } else { 0 };
        let (reader, layout) = if i == 0 {
            (first_reader.clone(), first_layout.clone_layout())
        } else {
            let (r, l, _video_type, _audio_types) = open_passthrough_reader(seg.path.as_path())?;
            (r, l)
        };

        // This segment's own stream index -> canonical sink stream index,
        // by position (video always first, audio streams in discovery
        // order) -- safe since every segment shares the same track layout.
        let mut source_to_sink: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        source_to_sink.insert(layout.video, video_sink);
        for (idx, &src_audio) in layout.audio.iter().enumerate() {
            if let Some(&sink) = audio_sinks.get(idx) {
                source_to_sink.insert(src_audio, sink);
            }
        }

        let total_streams = 1 + layout.audio.len();
        let mut done_streams: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut max_ts_seen: i64 = baseline_hns;

        loop {
            let mut actual_idx: u32 = 0;
            let mut stream_flags: u32 = 0;
            let mut timestamp: i64 = 0;
            let mut sample: Option<windows::Win32::Media::MediaFoundation::IMFSample> = None;

            reader
                .ReadSample(
                    MF_SOURCE_READER_ANY_STREAM,
                    0,
                    Some(&mut actual_idx),
                    Some(&mut stream_flags),
                    Some(&mut timestamp),
                    Some(&mut sample),
                )
                .map_err(|e| AppError::Encode(format!("ReadSample(segment {}): {e}", seg.path.display())))?;

            if stream_flags & 1 != 0 {
                return Err(AppError::Encode(format!(
                    "ReadSample: stream {actual_idx} reported error in segment {}",
                    seg.path.display()
                )));
            }
            if stream_flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
                done_streams.insert(actual_idx);
                if done_streams.len() >= total_streams {
                    break;
                }
                continue;
            }

            if let (Some(s), Some(&sink_idx)) = (sample, source_to_sink.get(&actual_idx)) {
                max_ts_seen = max_ts_seen.max(timestamp);
                let out_ts = timestamp - baseline_hns + running_offset_hns;
                s.SetSampleTime(out_ts)
                    .map_err(|e| AppError::Encode(format!("SetSampleTime: {e}")))?;
                write_sample(&writer, sink_idx, &s)?;
            }
        }

        running_offset_hns += (max_ts_seen - baseline_hns).max(0);
    }

    writer
        .Finalize()
        .map_err(|e| AppError::Encode(format!("Finalize: {e}")))?;

    Ok(output.to_path_buf())
}}

fn write_sample(
    writer: &IMFSinkWriter,
    stream_idx: u32,
    sample: &IMFSample,
) -> Result<(), AppError> {
    unsafe {
        writer
            .WriteSample(stream_idx, sample)
            .map_err(|e| AppError::Encode(format!("WriteSample(stream {stream_idx}): {e}")))
    }
}

impl StreamLayout {
    /// `remux.rs`'s `StreamLayout` has no `Clone` derive since its only other
    /// caller never needs to reuse one across two calls -- this reuses the
    /// already-opened first segment's reader for its own passthrough pass
    /// (`open_passthrough_reader` is only called once for it), so its layout
    /// needs to be duplicated rather than moved.
    fn clone_layout(&self) -> StreamLayout {
        StreamLayout { video: self.video, audio: self.audio.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::RecordingWriter;
    use crate::types::{AudioSamples, TrackId, VideoFrame};

    fn make_segment(dir: &std::path::Path, name: &str, frames: u32) -> SegmentInfo {
        let path = dir.join(name);
        let writer = RecordingWriter::new(&path, 64, 64, 30, "h264", 500_000, &[(48000u32, 2u16)], false)
            .expect("RecordingWriter::new");
        writer.begin_writing().expect("begin_writing");
        let mut last_pts = Duration::ZERO;
        for i in 0..frames {
            let pts = Duration::from_millis((i as u64) * 100);
            last_pts = pts;
            writer
                .write_video(VideoFrame { pts, data: vec![0u8; 64 * 64 * 4] })
                .expect("write_video");
            writer
                .write_audio(
                    0,
                    AudioSamples {
                        track_id: TrackId::new(0),
                        pts,
                        samples: vec![0.0f32; 480 * 2],
                        sample_rate: 48000,
                        channels: 2,
                    },
                )
                .expect("write_audio");
        }
        writer.finalize().expect("finalize");
        SegmentInfo { path, duration: last_pts }
    }

    #[test]
    fn concat_all_segments_when_target_exceeds_total_duration() {
        let dir = tempfile::tempdir().unwrap();
        let segments = vec![
            make_segment(dir.path(), "seg0.mp4", 5),
            make_segment(dir.path(), "seg1.mp4", 5),
        ];
        let output = dir.path().join("saved.mp4");
        let result = concat_and_trim(&segments, 3600, &output);
        assert!(result.is_ok(), "concat_and_trim failed: {:?}", result.err());
        assert!(output.exists());
        assert!(output.metadata().unwrap().len() > 0, "output file is empty");
    }

    #[test]
    fn concat_trims_the_oldest_segment_when_target_is_shorter_than_total() {
        let dir = tempfile::tempdir().unwrap();
        let segments = vec![
            make_segment(dir.path(), "seg0.mp4", 10),
            make_segment(dir.path(), "seg1.mp4", 10),
        ];
        let output = dir.path().join("saved_trimmed.mp4");
        // Each segment holds ~1s of content (10 frames * 100ms); asking for a
        // 1s window should only need part of the newest segment.
        let result = concat_and_trim(&segments, 1, &output);
        assert!(result.is_ok(), "concat_and_trim failed: {:?}", result.err());
        assert!(output.exists());
        assert!(output.metadata().unwrap().len() > 0, "output file is empty");
    }

    #[test]
    fn concat_errors_on_empty_segment_list() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("saved_empty.mp4");
        assert!(concat_and_trim(&[], 60, &output).is_err());
    }
}
