//! Concatenates the highlight buffer's rotating segment files into one output
//! file, trimming the oldest contributing segment so the result is close to
//! the requested window. Sibling to `remux.rs` rather than an extension of
//! it -- `remux::remux` is a single-input stream copy (used for the
//! post-recording track-selection export); this reads from *multiple* input
//! files into one sink, which needs its own PTS-rebasing logic across the
//! seams that a single-input remux never has to do.

use crate::encode::mf::MfSession;
use crate::encode::remux::{ALL_STREAMS, StreamLayout, discover_stream_layout, for_each_sample};
use crate::error::{AppError, ResultExt};
use crate::highlight::SegmentInfo;
use std::path::{Path, PathBuf};
use std::time::Duration;
use windows::Win32::Media::MediaFoundation::{
    IMFMediaType, IMFSample, IMFSinkWriter, IMFSourceReader, MFCreateSourceReaderFromURL,
};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Variant::VT_I8;
use windows::core::{GUID, HSTRING};

pub fn concat_and_trim(
    segments: &[SegmentInfo],
    target_seconds: u32,
    output: &Path,
) -> Result<PathBuf, AppError> {
    let _mf = MfSession::start_mta()?;
    unsafe { do_concat_and_trim(segments, target_seconds, output) }
}

/// Opens `path` for compressed passthrough (no decode/re-encode) on its video
/// stream and every audio stream it contains, mirroring `remux.rs`'s
/// `do_remux` setup -- returns the reader plus the native media types
/// actually negotiated, since the sink writer needs those exact types.
unsafe fn open_passthrough_reader(
    path: &Path,
) -> Result<
    (
        IMFSourceReader,
        StreamLayout,
        IMFMediaType,
        Vec<IMFMediaType>,
    ),
    AppError,
> {
    unsafe {
        let url = HSTRING::from(path);
        let reader: IMFSourceReader = MFCreateSourceReaderFromURL(&url, None).encode_ctx(
            format_args!("MFCreateSourceReaderFromURL({})", path.display()),
        )?;

        let layout = discover_stream_layout(&reader)?;

        reader
            .SetStreamSelection(ALL_STREAMS, false)
            .encode_ctx("SetStreamSelection(all,false)")?;
        reader
            .SetStreamSelection(layout.video, true)
            .encode_ctx("SetStreamSelection(video)")?;
        for &a in &layout.audio {
            reader
                .SetStreamSelection(a, true)
                .encode_ctx(format_args!("SetStreamSelection(audio {a})"))?;
        }

        let video_type: IMFMediaType = reader
            .GetNativeMediaType(layout.video, 0)
            .encode_ctx("GetNativeMediaType(video)")?;
        reader
            .SetCurrentMediaType(layout.video, None, &video_type)
            .encode_ctx("SetCurrentMediaType(video)")?;

        let mut audio_types = Vec::new();
        for &a in &layout.audio {
            let t: IMFMediaType = reader
                .GetNativeMediaType(a, 0)
                .encode_ctx(format_args!("GetNativeMediaType(audio {a})"))?;
            reader
                .SetCurrentMediaType(a, None, &t)
                .encode_ctx(format_args!("SetCurrentMediaType(audio {a})"))?;
            audio_types.push(t);
        }

        Ok((reader, layout, video_type, audio_types))
    }
}

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
) -> Result<PathBuf, AppError> {
    unsafe {
        if segments.is_empty() {
            return Err(AppError::Encode(
                "no highlight segments available to save".into(),
            ));
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
        let skip_hns = hns(total.saturating_sub(target));

        let output_url = HSTRING::from(output);

        // ── Sink writer, built from the oldest contributing segment's types --
        // every segment shares identical encode settings (same buffering
        // session), so its native compressed types stand in for all of them. ──
        let (first_reader, first_layout, video_type, audio_types) =
            open_passthrough_reader(contributing[0].path.as_path())?;
        if skip_hns > 0 {
            first_reader
                .SetCurrentPosition(&GUID::zeroed(), &time_propvariant(skip_hns))
                .encode_ctx(format_args!("SetCurrentPosition({skip_hns})"))?;
        }

        let (writer, video_sink, audio_sinks) =
            crate::encode::remux::build_passthrough_sink_writer(
                &output_url,
                &video_type,
                &audio_types,
            )?;

        // ── Copy each contributing segment in order, each where it was ──
        // captured: its start on the buffering session's clock, from the
        // oldest's. Every stream keeps its time against the others across
        // the seams, and any gap a rotation left stays a gap, rather than
        // each segment going on where the last sample before it began.
        for (i, seg) in contributing.iter().enumerate() {
            let offset_hns = hns(seg.start.saturating_sub(contributing[0].start)) - skip_hns;
            let (reader, layout) = if i == 0 {
                (first_reader.clone(), first_layout.clone())
            } else {
                let (r, l, _video_type, _audio_types) =
                    open_passthrough_reader(seg.path.as_path())?;
                (r, l)
            };

            // This segment's own stream index -> canonical sink stream index,
            // by position (video always first, audio streams in discovery
            // order) -- safe since every segment shares the same track layout.
            let mut source_to_sink: std::collections::HashMap<u32, u32> =
                std::collections::HashMap::new();
            source_to_sink.insert(layout.video, video_sink);
            for (idx, &src_audio) in layout.audio.iter().enumerate() {
                if let Some(&sink) = audio_sinks.get(idx) {
                    source_to_sink.insert(src_audio, sink);
                }
            }

            let total_streams = 1 + layout.audio.len();
            let label = format!("segment {}", seg.path.display());
            for_each_sample(
                &reader,
                total_streams,
                Some(&label),
                |stream, s, timestamp| {
                    if let Some(&sink_idx) = source_to_sink.get(&stream) {
                        s.SetSampleTime(timestamp + offset_hns)
                            .encode_ctx("SetSampleTime")?;
                        write_sample(&writer, sink_idx, &s)?;
                    }
                    Ok(())
                },
            )?;
        }

        writer.Finalize().encode_ctx("Finalize")?;

        Ok(output.to_path_buf())
    }
}

/// `duration` in 100 ns units, Media Foundation's.
fn hns(duration: Duration) -> i64 {
    (duration.as_nanos() / 100).min(i64::MAX as u128) as i64
}

fn write_sample(
    writer: &IMFSinkWriter,
    stream_idx: u32,
    sample: &IMFSample,
) -> Result<(), AppError> {
    unsafe {
        writer
            .WriteSample(stream_idx, sample)
            .encode_ctx(format_args!("WriteSample(stream {stream_idx})"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Codec;
    use crate::encode::{EncoderParams, RecordingWriter};
    use crate::types::{AudioSamples, TrackId, VideoFrame};

    fn make_segment(dir: &std::path::Path, name: &str, frames: u32) -> SegmentInfo {
        let path = dir.join(name);
        let writer = RecordingWriter::new(
            &path,
            &EncoderParams::tiny_test(Codec::H264, &[(48000u32, 2u16)]),
        )
        .expect("RecordingWriter::new");
        writer.begin_writing().expect("begin_writing");
        let mut last_pts = Duration::ZERO;
        for i in 0..frames {
            let pts = Duration::from_millis((i as u64) * 100);
            last_pts = pts;
            writer
                .write_video(VideoFrame {
                    pts,
                    data: vec![0u8; 64 * 64 * 4].into(),
                })
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
        SegmentInfo {
            path,
            start: Duration::ZERO,
            // Up to where the last frame ends, as the actor counts it.
            duration: last_pts + Duration::from_secs(1) / 30,
        }
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
    fn concat_writes_over_the_empty_file_that_reserved_its_name() {
        // See `recording_naming::reserve_finished_recording_path`.
        let dir = tempfile::tempdir().unwrap();
        let segments = vec![make_segment(dir.path(), "seg0.mp4", 5)];
        let output = dir.path().join("reserved.mp4");
        std::fs::write(&output, b"").unwrap();
        let result = concat_and_trim(&segments, 3600, &output);
        assert!(result.is_ok(), "concat_and_trim failed: {:?}", result.err());
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

    /// Each segment goes where it was captured. It used to go on at the
    /// last sample's *start* in the segment before -- whichever stream's
    /// that was: here the audio's, 2.7 ms after the last frame began, so the
    /// next segment's first frame overlapped it, and the streams shifted
    /// against each other at every seam.
    #[test]
    fn segments_go_where_they_were_captured() {
        let dir = tempfile::tempdir().unwrap();
        let segments: Vec<_> = [0, 500, 1000]
            .into_iter()
            .enumerate()
            .map(|(i, start)| {
                let mut segment = make_segment(dir.path(), &format!("seg{i}.mp4"), 5);
                segment.start = Duration::from_millis(start);
                segment
            })
            .collect();
        let output = dir.path().join("saved.mp4");
        concat_and_trim(&segments, 3600, &output).expect("concat_and_trim");

        let _mf = MfSession::start_mta().unwrap();
        let mut video = Vec::new();
        unsafe {
            let (reader, layout, _, _) = open_passthrough_reader(&output).unwrap();
            for_each_sample(&reader, 1 + layout.audio.len(), None, |stream, _s, ts| {
                if stream == layout.video {
                    video.push(ts);
                }
                Ok(())
            })
            .unwrap();
        }
        for start_hns in [0, 5_000_000, 10_000_000] {
            assert!(
                video.contains(&start_hns),
                "no frame at {start_hns}: {video:?}"
            );
        }
        let count = video.len();
        video.sort_unstable();
        video.dedup();
        assert_eq!(video.len(), count, "two frames at one time");
    }

    #[test]
    fn concat_errors_on_empty_segment_list() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("saved_empty.mp4");
        assert!(concat_and_trim(&[], 60, &output).is_err());
    }
}
