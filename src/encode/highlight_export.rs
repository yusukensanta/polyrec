//! Concatenates the highlight buffer's rotating segment files into one output
//! file, trimming the oldest contributing segment so the result is close to
//! the requested window. Sibling to `remux.rs` rather than an extension of
//! it -- `remux::remux` is a single-input stream copy (used for the
//! post-recording track-selection export); this reads from *multiple* input
//! files into one sink, which needs its own PTS-rebasing logic across the
//! seams that a single-input remux never has to do.

use crate::capture::frame_rate::FrameGrid;
use crate::encode::mf::MfSession;
use crate::encode::remux::{
    ALL_STREAMS, Read, StreamLayout, discover_stream_layout, for_each_sample, read_next,
};
use crate::error::{AppError, ResultExt};
use crate::highlight::SegmentInfo;
use std::path::{Path, PathBuf};
use std::time::Duration;
use windows::Win32::Media::MediaFoundation::{
    IMFMediaType, IMFSample, IMFSinkWriter, IMFSourceReader, MF_MT_FRAME_RATE,
    MFCreateSourceReaderFromURL, MFSampleExtension_DecodeTimestamp,
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

fn seek(reader: &IMFSourceReader, at_hns: i64) -> Result<(), AppError> {
    unsafe { reader.SetCurrentPosition(&GUID::zeroed(), &time_propvariant(at_hns)) }
        .encode_ctx(format_args!("SetCurrentPosition({at_hns})"))
}

/// The time of the keyframe at or before `at_hns` in `path`'s video: where
/// reading it from `at_hns` really starts, a seek in compressed streams
/// landing on one. `at_hns` itself if there's no frame from there.
fn keyframe_at_or_before(path: &Path, at_hns: i64) -> Result<i64, AppError> {
    let (reader, layout, _, _) = unsafe { open_passthrough_reader(path) }?;
    seek(&reader, at_hns)?;
    let label = format!("keyframe in {}", path.display());
    loop {
        match read_next(&reader, layout.video, Some(&label))? {
            Read::Sample(_, Some(_), timestamp) => return Ok(timestamp),
            Read::Sample(_, None, _) => {}
            Read::EndOfStream(_) => return Ok(at_hns),
        }
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
        let oldest = contributing[0].path.as_path();

        let output_url = HSTRING::from(output);

        // ── Sink writer, built from the oldest contributing segment's types --
        // every segment shares identical encode settings (same buffering
        // session), so its native compressed types stand in for all of them. ──
        let (first_reader, first_layout, video_type, audio_types) =
            open_passthrough_reader(oldest)?;
        // Where the save starts in the oldest segment: its first frame from
        // `skip_hns` in, or rather the keyframe before that, where a seek
        // lands -- every stream starts there. The frames between can't be
        // decoded without it, so the save comes out up to a GOP longer than
        // asked. The save's time 0 is that frame's, whatever sound comes
        // before it.
        if skip_hns > 0 {
            seek(&first_reader, skip_hns)?;
        }
        let start_hns = keyframe_at_or_before(oldest, skip_hns)?;

        // Every segment's frames sit on the one frame grid of the buffering
        // session (see `highlight::segment_base`), and a save starts on a
        // frame, so a save's frames are on a grid from its 0: each goes at
        // its slot's start exactly as `FrameGrid` gives it, which lands on
        // the file's own time unit whichever way that's rounded -- adding up
        // the 100 ns times of segment and offset can land a unit short.
        let grid = frame_grid(&video_type);
        // Media Foundation's MP4 sink puts whatever is written first at the
        // file's 0, so the first frame goes first: any sound read before it
        // waits until it's written.
        let mut first_frame_written = false;
        let mut held_sound: Vec<(u32, IMFSample)> = Vec::new();

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
            let offset_hns = hns(seg.start.saturating_sub(contributing[0].start)) - start_hns;
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
            // Per stream: how far ahead of its own time its first sample was
            // decoded -- see `decode_time`.
            let mut decoded_ahead = std::collections::HashMap::new();
            let label = format!("segment {}", seg.path.display());
            for_each_sample(
                &reader,
                total_streams,
                Some(&label),
                |stream, s, timestamp| {
                    let Some(&sink_idx) = source_to_sink.get(&stream) else {
                        return Ok(());
                    };
                    let mut time = timestamp + offset_hns;
                    if stream == layout.video {
                        if let Some(grid) = grid {
                            time = on_grid(grid, time);
                        }
                    } else if time < 0 {
                        // Sound from before the save's first frame.
                        return Ok(());
                    }
                    s.SetSampleTime(time).encode_ctx("SetSampleTime")?;
                    if let Ok(decoded) = s.GetUINT64(&MFSampleExtension_DecodeTimestamp) {
                        let ahead = timestamp - decoded as i64;
                        let first_ahead = *decoded_ahead.entry(stream).or_insert(ahead);
                        s.SetUINT64(
                            &MFSampleExtension_DecodeTimestamp,
                            decode_time(time, ahead, first_ahead),
                        )
                        .encode_ctx("SetUINT64 decode time")?;
                    }
                    if stream != layout.video && !first_frame_written {
                        held_sound.push((sink_idx, s));
                        return Ok(());
                    }
                    write_sample(&writer, sink_idx, &s)?;
                    if !first_frame_written {
                        first_frame_written = true;
                        for (sink_idx, held) in held_sound.drain(..) {
                            write_sample(&writer, sink_idx, &held)?;
                        }
                    }
                    Ok(())
                },
            )?;
        }
        // A save without a single frame still gets its sound.
        for (sink_idx, held) in held_sound.drain(..) {
            write_sample(&writer, sink_idx, &held)?;
        }

        writer.Finalize().encode_ctx("Finalize")?;

        Ok(output.to_path_buf())
    }
}

/// Every video sample in `path`: its time and how long it lasts, in 100 ns
/// units -- for tests that check a save's timeline.
#[cfg(test)]
pub(crate) fn video_sample_times(path: &Path) -> Vec<(i64, i64)> {
    let _mf = MfSession::start_mta().unwrap();
    let mut times = Vec::new();
    unsafe {
        let (reader, layout, _, _) = open_passthrough_reader(path).unwrap();
        for_each_sample(&reader, 1 + layout.audio.len(), None, |stream, s, ts| {
            if stream == layout.video {
                times.push((ts, s.GetSampleDuration().unwrap_or(0)));
            }
            Ok(())
        })
        .unwrap();
    }
    times
}

/// `duration` in 100 ns units, Media Foundation's.
/// The decode time of a sample going at `time` in a save, that was decoded
/// `ahead` of its own time in its segment, the stream's first sample there
/// `first_ahead`. A stream that starts after the segment's first sample has
/// its decode times counted from the segment's 0 all the same (a `ctts`),
/// each `first_ahead` behind: moved by the segment's place in the save like
/// its times, or Media Foundation's sink took them for times gone by and
/// left the segment out of the save. Only how much further ahead than its
/// first each is decoded -- B-frames' reordering -- is kept, so a stream
/// without any decodes at its times.
fn decode_time(time: i64, ahead: i64, first_ahead: i64) -> u64 {
    (time - (ahead - first_ahead)).max(0) as u64
}

/// The frame grid of a video stream's frame rate, if it's a whole one.
fn frame_grid(video_type: &IMFMediaType) -> Option<FrameGrid> {
    let packed = unsafe { video_type.GetUINT64(&MF_MT_FRAME_RATE) }.ok()?;
    let (fps, per) = ((packed >> 32) as u32, packed as u32);
    (per == 1 && fps > 0).then(|| FrameGrid::new(fps))
}

/// The start, in 100 ns units, of the slot of `grid` nearest `time`.
fn on_grid(grid: FrameGrid, time: i64) -> i64 {
    grid.start_hns(grid.slot(Duration::from_nanos(time.max(0) as u64 * 100)))
}

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

        let mut video: Vec<i64> = video_sample_times(&output)
            .into_iter()
            .map(|(ts, _)| ts)
            .collect();
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

    /// A save shorter than the buffer starts in its oldest segment, part
    /// way in. A compressed stream can only start at a keyframe -- the
    /// frames after it are told apart from it -- so the save starts at the
    /// keyframe before, a little longer than asked. It used to keep only
    /// what came after the start asked for: frames that can't be decoded
    /// without the keyframe left out.
    #[test]
    fn a_save_starting_part_way_into_a_segment_starts_at_a_keyframe() {
        let _mf = MfSession::start_mta().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // 3 s each; the oldest one's frames grey (from 40 up), the newest's
        // nearly white.
        let segment = |name: &str, start_s: u64, grey: fn(u64) -> u8| {
            let path = dir.path().join(name);
            let writer = RecordingWriter::new(
                &path,
                &EncoderParams::tiny_test(Codec::H264, &[(48000u32, 2u16)]),
            )
            .expect("RecordingWriter::new");
            writer.begin_writing().expect("begin_writing");
            for frame in 0..90u64 {
                let pts = Duration::from_millis(frame * 1000 / 30);
                writer
                    .write_video(VideoFrame {
                        pts,
                        data: vec![grey(frame); 64 * 64 * 4].into(),
                    })
                    .expect("write_video");
                for packet in 0..3u64 {
                    writer
                        .write_audio(
                            0,
                            AudioSamples {
                                track_id: TrackId::new(0),
                                pts: pts + Duration::from_millis(packet * 10),
                                samples: vec![0.1f32; 480 * 2],
                                sample_rate: 48000,
                                channels: 2,
                            },
                        )
                        .expect("write_audio");
                }
            }
            writer.finalize().expect("finalize");
            SegmentInfo {
                path,
                start: Duration::from_secs(start_s),
                duration: Duration::from_secs(3),
            }
        };
        let segments = [
            segment("seg0.mp4", 0, |frame| 40 + frame as u8),
            segment("seg1.mp4", 3, |_| 230),
        ];
        let output = dir.path().join("saved.mp4");
        concat_and_trim(&segments, 4, &output).expect("concat_and_trim");

        let video = video_sample_times(&output);
        assert_eq!(video[0].0, 0, "starts at 0: {:?}", &video[..3]);
        let last = video.last().unwrap();
        assert!(
            last.0 + last.1 >= 40_000_000 - 10,
            "at least the 4 s asked for (to the 100 ns frame durations round to): {last:?}"
        );
        // Decodes from its first frame on, to the oldest segment's picture.
        let (_, colours) = crate::encode::writer::tests::first_frame_colours(&output);
        for (r, g, b) in colours {
            for channel in [r, g, b] {
                assert!(
                    (30..=140).contains(&channel),
                    "first frame isn't the oldest segment's: {colours:?}"
                );
            }
        }
    }

    #[test]
    fn concat_errors_on_empty_segment_list() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("saved_empty.mp4");
        assert!(concat_and_trim(&[], 60, &output).is_err());
    }

    /// A stream that started late in its segment (a `ctts` of 23 ms): its
    /// samples decode at their times in the save. A B-frame's reordering
    /// -- decoded a frame further ahead than the first -- stays.
    #[test]
    fn a_save_decodes_each_sample_at_its_time_but_for_reordering() {
        let late = 233_333;
        assert_eq!(decode_time(3_333_334, late, late), 3_333_334);
        assert_eq!(decode_time(3_666_667, late + 333_333, late), 3_333_334);
        assert_eq!(decode_time(100, 0, 0), 100);
    }
}
