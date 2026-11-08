//! Fragmented MP4, written by PolyRec itself, so a recording survives a
//! crash. Media Foundation's MP4 sink writes the file's index only when it
//! finishes, so a recording that never finishes (PolyRec killed, a crash,
//! a power cut, a driver reset) can't be read at all; its fragmented MP4
//! sink, which can, takes H.264 only, and HEVC is PolyRec's default. So a
//! media sink of PolyRec's own (`sink`) takes the sink writer's place
//! behind the encoders, which stay Media Foundation's, and writes the file
//! a fragment (one GOP, about a second) at a time: a recording cut off
//! loses its last fragment, no more.
//!
//! At Stop the file is finished in place (`soft_remux`): a complete index
//! goes at its end and a placeholder near its start becomes the header of
//! one `mdat` holding everything before it -- a plain MP4, in about a
//! second however long the recording, without copying it. A recording cut
//! off is finished the same way at the next start. One written before
//! recordings were finished in place (no placeholder) is remuxed into a
//! plain MP4 (`encode::remux`), as they were then.

pub mod boxes;
pub mod muxer;
pub mod nal;
pub mod sink;
pub mod soft_remux;

use crate::error::AppError;
use soft_remux::InPlace;
use std::path::{Path, PathBuf};

/// Finishes the recording at `temp` into a plain MP4 and moves it to the
/// name `reserve` gives -- asked only once the file is finished, so a
/// finish cut off never leaves an empty file holding a name. Returns where
/// it's saved. A recording that can't be finished in place is moved there
/// as it is: a fragmented MP4 plays all the same.
///
/// `audio_tracks` (how many it has) is asked only of a recording written
/// before recordings were finished in place, for the remux that finishes
/// it; Media Foundation, which counts them, may not open a file whose
/// finish was cut off between its two writes.
pub fn finish(
    temp: &Path,
    reserve: impl FnOnce() -> Result<PathBuf, AppError>,
    audio_tracks: impl FnOnce() -> Result<usize, AppError>,
) -> Result<PathBuf, AppError> {
    match soft_remux::finish_in_place(temp) {
        InPlace::Finished | InPlace::KeptFragmented => {
            into_reserved(reserve()?, |dest| move_over(temp, dest))
        }
        InPlace::Legacy => {
            let audio_tracks = audio_tracks()?;
            into_reserved(reserve()?, |dest| finish_legacy(temp, dest, audio_tracks))
        }
    }
}

/// `dest`, once `fill` has put the recording there; the empty file that
/// reserved it is removed if it couldn't.
fn into_reserved(
    dest: PathBuf,
    fill: impl FnOnce(&Path) -> Result<(), AppError>,
) -> Result<PathBuf, AppError> {
    fill(&dest).inspect_err(|_| {
        let _ = std::fs::remove_file(&dest);
    })?;
    Ok(dest)
}

/// Makes the fragmented recording at `fragmented` (with `audio_tracks`
/// tracks of audio, written before recordings were finished in place) the
/// plain MP4 at `dest`, removing it. If remuxing fails -- or there's no room
/// on the drive for the second copy it writes -- it's moved to `dest` as it
/// is: a fragmented MP4 plays all the same.
fn finish_legacy(fragmented: &Path, dest: &Path, audio_tracks: usize) -> Result<(), AppError> {
    let size = std::fs::metadata(fragmented).map_or(0, |m| m.len());
    let free = dest
        .parent()
        .and_then(|dir| crate::disk_space::free_bytes(dir).ok());
    if let Some(free) = free.filter(|&free| !room_to_remux(free, size)) {
        tracing::warn!(
            "{} MB free, {} MB to remux: keeping {} fragmented",
            free >> 20,
            size >> 20,
            fragmented.display()
        );
        return move_over(fragmented, dest);
    }
    let tracks: Vec<usize> = (0..audio_tracks).collect();
    match crate::encode::remux::remux(fragmented, dest, &tracks) {
        Ok(_) => {
            let _ = std::fs::remove_file(fragmented);
            Ok(())
        }
        Err(e) => {
            tracing::warn!(
                "couldn't remux {} ({e}); keeping it fragmented",
                fragmented.display()
            );
            move_over(fragmented, dest)
        }
    }
}

/// Moves the recording at `from` to `dest`, over the empty file that
/// reserved that name.
fn move_over(from: &Path, dest: &Path) -> Result<(), AppError> {
    use crate::error::ResultExt;
    std::fs::rename(from, dest).encode_ctx(format_args!(
        "moving {} to {}",
        from.display(),
        dest.display()
    ))
}

/// Whether a remux of a `size`-byte recording, with `free` bytes free on
/// its drive, would leave what recording itself does
/// (`disk_space::MIN_FREE_BYTES`): it writes a second copy before the
/// first goes.
fn room_to_remux(free: u64, size: u64) -> bool {
    free >= size.saturating_add(crate::disk_space::MIN_FREE_BYTES)
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::config::Codec;
    use crate::encode::mf::MfSession;
    use crate::encode::remux::{ALL_STREAMS, discover_stream_layout, for_each_sample};
    use crate::encode::{Container, EncoderParams, RecordingWriter};
    use crate::types::{AudioSamples, TrackId, VideoFrame};
    use std::path::Path;
    use std::time::{Duration, Instant};
    use windows::Win32::Media::MediaFoundation::{
        IMFMediaType, IMFSourceReader, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MFCreateMediaType,
        MFCreateSourceReaderFromURL, MFMediaType_Video, MFVideoFormat_NV12,
    };
    use windows::core::HSTRING;

    const TRACKS: [(u32, u16); 2] = [(48_000, 2), (48_000, 1)];

    pub(crate) fn params() -> EncoderParams {
        params_with(&TRACKS)
    }

    fn params_with(tracks: &[(u32, u16)]) -> EncoderParams {
        EncoderParams {
            container: Container::FragmentedMp4,
            ..EncoderParams::tiny_test(Codec::H264, tracks)
        }
    }

    /// `seconds` of 30 fps video and both tracks' audio (10 ms packets, as
    /// WASAPI's), in real time if `realtime`.
    pub(crate) fn record(writer: &RecordingWriter, seconds: u64, realtime: bool) {
        record_frames(writer, seconds * 30, realtime);
    }

    /// `frames` frames of 30 fps video and both tracks' audio alongside.
    pub(crate) fn record_frames(writer: &RecordingWriter, frames: u64, realtime: bool) {
        record_tracks(writer, &TRACKS, frames, realtime);
    }

    /// `frames` frames of 30 fps video and audio alongside for `tracks`
    /// (rate, channels), the writer's.
    fn record_tracks(writer: &RecordingWriter, tracks: &[(u32, u16)], frames: u64, realtime: bool) {
        let started = Instant::now();
        for frame in 0..frames {
            let pts = Duration::from_millis(frame * 1000 / 30);
            writer
                .write_video(VideoFrame {
                    pts,
                    data: vec![(frame % 255) as u8; 64 * 64 * 4].into(),
                })
                .expect("write_video");
            for packet in 0..3u64 {
                for (track, (rate, channels)) in tracks.iter().enumerate() {
                    let samples = AudioSamples {
                        track_id: TrackId::new(track as u32),
                        pts: pts + Duration::from_millis(packet * 10),
                        samples: vec![0.1; (*rate as usize / 100) * usize::from(*channels)],
                        sample_rate: *rate,
                        channels: *channels,
                    };
                    writer.write_audio(track, samples).expect("write_audio");
                }
            }
            if realtime {
                let due = started + pts + Duration::from_millis(1000 / 30);
                std::thread::sleep(due.saturating_duration_since(Instant::now()));
            }
        }
    }

    fn open(path: &Path) -> IMFSourceReader {
        unsafe {
            MFCreateSourceReaderFromURL(&HSTRING::from(path), None)
                .unwrap_or_else(|e| panic!("{} doesn't open: {e}", path.display()))
        }
    }

    /// Per stream, video first: how many samples, over how long.
    fn read_back(path: &Path) -> Vec<(usize, Duration)> {
        let _mf = MfSession::start_mta().unwrap();
        unsafe {
            let reader = open(path);
            let layout = discover_stream_layout(&reader).unwrap();
            reader.SetStreamSelection(ALL_STREAMS, true).unwrap();
            let order: Vec<u32> = std::iter::once(layout.video).chain(layout.audio).collect();
            let mut spans = vec![(0usize, i64::MAX, 0i64); order.len()];
            for_each_sample(&reader, order.len(), None, |stream, sample, time| {
                let at = order.iter().position(|&s| s == stream).unwrap();
                let span = &mut spans[at];
                span.0 += 1;
                span.1 = span.1.min(time);
                span.2 = span.2.max(time + sample.GetSampleDuration().unwrap_or(0));
                Ok(())
            })
            .unwrap();
            spans
                .into_iter()
                .map(|(n, first, end)| (n, Duration::from_nanos((end - first).max(0) as u64 * 100)))
                .collect()
        }
    }

    fn assert_about(span: Duration, expected: Duration, what: &str) {
        let off = span.abs_diff(expected);
        assert!(
            off < Duration::from_millis(100),
            "{what}: {span:?}, not {expected:?}"
        );
    }

    fn assert_holds_three_seconds_of_everything(spans: &[(usize, Duration)]) {
        assert_eq!(spans.len(), 3, "video and both audio tracks: {spans:?}");
        assert_eq!(spans[0].0, 90, "every frame");
        for (i, (_, span)) in spans.iter().enumerate() {
            assert_about(*span, Duration::from_secs(3), &format!("stream {i}"));
        }
    }

    #[test]
    fn a_fragmented_recording_holds_every_track() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.tmp.mp4");
        let writer = RecordingWriter::new(&path, &params()).expect("writer");
        writer.begin_writing().unwrap();
        record(&writer, 3, false);
        writer.finalize().expect("finalize");
        assert_holds_three_seconds_of_everything(&read_back(&path));
        let unfinished = dir.path().join("rec.frag.mp4");
        unfinished_copy(&path, &unfinished);
        assert_holds_three_seconds_of_everything(&read_back(&unfinished));
    }

    /// The configuration records and length-prefixed samples are right: a
    /// decoder takes them, before Stop finished the file and after.
    #[test]
    fn a_fragmented_recording_decodes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.tmp.mp4");
        let writer = RecordingWriter::new(&path, &params()).expect("writer");
        writer.begin_writing().unwrap();
        record(&writer, 3, false);
        writer.finalize().expect("finalize");
        assert_eq!(decoded_frames(&path), 90);
        let unfinished = dir.path().join("rec.frag.mp4");
        unfinished_copy(&path, &unfinished);
        assert_eq!(decoded_frames(&unfinished), 90);
    }

    /// How many video frames of `path` a decoder makes.
    fn decoded_frames(path: &Path) -> usize {
        let _mf = MfSession::start_mta().unwrap();
        unsafe {
            let reader = open(path);
            let layout = discover_stream_layout(&reader).unwrap();
            reader.SetStreamSelection(ALL_STREAMS, false).unwrap();
            reader.SetStreamSelection(layout.video, true).unwrap();
            let decoded: IMFMediaType = MFCreateMediaType().unwrap();
            decoded
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .unwrap();
            decoded
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
                .unwrap();
            reader
                .SetCurrentMediaType(layout.video, None, &decoded)
                .expect("a decoder for it");
            let mut frames = 0;
            for_each_sample(&reader, 1, None, |_, _, _| {
                frames += 1;
                Ok(())
            })
            .expect("decoding");
            frames
        }
    }

    #[test]
    fn a_fragmented_recording_remuxes_to_a_plain_mp4() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.frag.mp4");
        record_fragmented(&path);
        let plain = dir.path().join("rec.mp4");
        crate::encode::remux::remux(&path, &plain, &[0, 1]).expect("remux");
        assert_holds_three_seconds_of_everything(&read_back(&plain));
    }

    /// The top-level boxes of `path`: a plain MP4 has no `moof`.
    pub(crate) fn top_level(path: &Path) -> Vec<String> {
        let data = std::fs::read(path).unwrap();
        super::boxes::tests::kinds(&super::boxes::tests::boxes(&data))
            .into_iter()
            .filter(|k| k != "free")
            .collect()
    }

    fn fragmented_recording(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("app_recording_1.tmp.mp4");
        record_fragmented(&path);
        path
    }

    /// 3 s of recording at `path`, video and two audio tracks, as it is
    /// while being recorded: fragmented, never finished (a recording cut
    /// off).
    pub(crate) fn record_fragmented(path: &Path) {
        let elsewhere = tempfile::tempdir().unwrap();
        let finished = elsewhere.path().join("rec.mp4");
        record_finished(&finished, 90);
        unfinished_copy(&finished, path);
    }

    /// `frames` frames of recording at `path`, video and two audio tracks,
    /// finished at Stop.
    pub(crate) fn record_finished(path: &Path, frames: u64) {
        let writer = RecordingWriter::new(path, &params()).expect("writer");
        writer.begin_writing().unwrap();
        record_frames(&writer, frames, false);
        writer.finalize().expect("finalize");
    }

    /// The recording `finished` at `to` as it was before Stop finished it:
    /// everything before its `moov`, with the placeholder back where its
    /// `mdat` header is.
    pub(crate) fn unfinished_copy(finished: &Path, to: &Path) {
        std::fs::write(to, unfinished_bytes(finished)).unwrap();
    }

    /// The recording `finished` at `to` as a finish cut off after its
    /// `moov` was written, before its header went in, left it.
    pub(crate) fn cut_off_after_index_copy(finished: &Path, to: &Path) {
        let bytes = std::fs::read(finished).unwrap();
        let moov = super::boxes::tests::find(&super::boxes::tests::boxes(&bytes), "moov").at;
        let mut cut_off = unfinished_bytes(finished);
        cut_off.extend_from_slice(&bytes[moov..]);
        std::fs::write(to, cut_off).unwrap();
    }

    /// The recording `finished` at `to` as it would have been written
    /// before recordings were finished in place: no placeholder. (Its
    /// fragments' offsets count from their own `moof`, so they still hold.)
    pub(crate) fn legacy_copy(finished: &Path, to: &Path) {
        let mut bytes = unfinished_bytes(finished);
        let at = placeholder_at(&bytes);
        bytes.drain(at..at + 16);
        std::fs::write(to, bytes).unwrap();
    }

    fn unfinished_bytes(finished: &Path) -> Vec<u8> {
        let mut bytes = std::fs::read(finished).unwrap();
        let top = super::boxes::tests::boxes(&bytes);
        let kinds = super::boxes::tests::kinds(&top);
        assert_eq!(kinds, ["ftyp", "mdat", "moov"], "a finished recording");
        let (mdat, moov) = (top[1].at, top[2].at);
        bytes.truncate(moov);
        bytes[mdat..mdat + 16].copy_from_slice(&super::boxes::PLACEHOLDER);
        bytes
    }

    fn placeholder_at(bytes: &[u8]) -> usize {
        let top = super::boxes::tests::boxes(bytes);
        let free = super::boxes::tests::find(&top, "free").at;
        assert_eq!(bytes[free..free + 16], super::boxes::PLACEHOLDER);
        free
    }

    /// One sample as Media Foundation reads it back.
    #[derive(Clone, PartialEq)]
    struct MfSample {
        /// 100 ns units.
        time: i64,
        duration: i64,
        keyframe: bool,
        data: Vec<u8>,
    }

    impl std::fmt::Debug for MfSample {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            use std::hash::{DefaultHasher, Hasher};
            let mut hasher = DefaultHasher::new();
            hasher.write(&self.data);
            write!(
                f,
                "{{ time {}, duration {}, keyframe {}, {} bytes #{:016x} }}",
                self.time,
                self.duration,
                self.keyframe,
                self.data.len(),
                hasher.finish()
            )
        }
    }

    /// Every sample of `path` as Media Foundation reads it -- what Windows
    /// Media Player plays -- per stream, video first.
    fn mf_samples(path: &Path) -> Vec<Vec<MfSample>> {
        use windows::Win32::Media::MediaFoundation::MFSampleExtension_CleanPoint;
        let _mf = MfSession::start_mta().unwrap();
        unsafe {
            let reader = open(path);
            let layout = discover_stream_layout(&reader).unwrap();
            reader.SetStreamSelection(ALL_STREAMS, true).unwrap();
            let order: Vec<u32> = std::iter::once(layout.video).chain(layout.audio).collect();
            let mut streams = vec![Vec::new(); order.len()];
            for_each_sample(&reader, order.len(), None, |stream, sample, time| {
                let buffer = sample.ConvertToContiguousBuffer().unwrap();
                let mut data = std::ptr::null_mut();
                let mut len = 0u32;
                buffer.Lock(&mut data, None, Some(&mut len)).unwrap();
                let bytes = if len == 0 {
                    &[][..]
                } else {
                    std::slice::from_raw_parts(data, len as usize)
                };
                let read = MfSample {
                    time,
                    duration: sample.GetSampleDuration().unwrap_or(0),
                    keyframe: sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0,
                    data: bytes.to_vec(),
                };
                buffer.Unlock().unwrap();
                let at = order.iter().position(|&s| s == stream).unwrap();
                streams[at].push(read);
                Ok(())
            })
            .unwrap();
            streams
        }
    }

    /// How far apart (100 ns units) Media Foundation may read the same
    /// sample's time in a fragmented file and in the file finished from it:
    /// one tick of audio (23 us at 44.1 kHz). Its reader of fragmented files
    /// puts the samples of some fragments a tick early -- the file's own
    /// times (each fragment's `tfdt`, and the durations) say otherwise, and
    /// it doesn't add up (checked over 20 s) -- and it rounds a plain MP4's
    /// times to 100 ns its own way.
    const A_TICK: i64 = 250;

    /// `mf_samples` of a fragmented recording, its audio streams in the order
    /// Media Foundation reads them from the finished file: reversed (see
    /// `soft_remux::complete_moov`).
    fn as_finished(mut streams: Vec<Vec<MfSample>>) -> Vec<Vec<MfSample>> {
        streams[1..].reverse();
        streams
    }

    /// `ours` (a finished recording, read by `mf_samples`) against
    /// `theirs`: the same samples, at times and for durations within
    /// `tolerance` (100 ns units), with the same bytes and keyframes.
    ///
    /// A duration may differ where ours runs exactly to the next sample:
    /// where a fragment's audio didn't end where the next fragment's
    /// starts, the finished file lengthens (or shortens) its last sample to
    /// meet it, keeping every sample's time; Media Foundation reading the
    /// fragmented file gives that sample its own length, and leaves the gap.
    ///
    /// And bar one thing more. A fragment cut at 2 s where the encoder put no
    /// keyframe starts with an ordinary frame (an H.264 slice of NAL type
    /// 1), and Media Foundation reads such a frame of a fragmented file as
    /// a keyframe, with the parameter sets put in front of it; its remux
    /// wrote it so. It isn't one: a player seeking to it couldn't decode
    /// from there. A file finished in place keeps it as the encoder made
    /// it, so there `theirs` may be a keyframe that's `ours` with more in
    /// front. How many such frames there were.
    fn assert_reads_like(
        ours: &[Vec<MfSample>],
        theirs: &[Vec<MfSample>],
        tolerance: i64,
        what: &str,
    ) -> usize {
        assert_eq!(ours.len(), theirs.len(), "{what}: streams");
        let mut not_keyframes = 0;
        for (stream, (ours, theirs)) in ours.iter().zip(theirs).enumerate() {
            assert_eq!(ours.len(), theirs.len(), "{what}: stream {stream}: samples");
            for (i, (a, b)) in ours.iter().zip(theirs).enumerate() {
                let at = format!("{what}: stream {stream} sample {i}: {a:?} vs {b:?}");
                assert!((a.time - b.time).abs() <= tolerance, "{at}");
                let runs_to_next = ours
                    .get(i + 1)
                    .is_some_and(|next| (a.time + a.duration - next.time).abs() <= tolerance);
                assert!(
                    (a.duration - b.duration).abs() <= tolerance || runs_to_next,
                    "{at}"
                );
                if a.keyframe == b.keyframe {
                    assert!(a.data == b.data, "{at}: different bytes");
                } else {
                    let read_as_keyframe = stream == 0
                        && !a.keyframe
                        && b.data.len() > a.data.len()
                        && b.data.ends_with(&a.data);
                    assert!(read_as_keyframe, "{at}");
                    not_keyframes += 1;
                }
            }
        }
        not_keyframes
    }

    /// Per `trak` of `path`'s `moov`, in order: its track_ID, sample entry
    /// type, and channel count (audio).
    fn top_level_tracks(path: &Path) -> Vec<(u32, String, u16)> {
        use super::boxes::tests::{be32, boxes, children, find, whole};
        let bytes = std::fs::read(path).unwrap();
        let top = boxes(&bytes);
        let moov = find(&top, "moov");
        children(moov, 0)
            .iter()
            .filter(|b| b.kind == "trak")
            .map(|trak| {
                let parts = children(trak, 0);
                let tkhd = whole(&bytes, find(&parts, "tkhd"));
                let id = be32(tkhd, if tkhd[8] == 0 { 20 } else { 28 });
                let mdia = children(find(&parts, "mdia"), 0);
                let minf = children(find(&mdia, "minf"), 0);
                let stbl = children(find(&minf, "stbl"), 0);
                let entry = &children(find(&stbl, "stsd"), 8)[0];
                let channels = if entry.kind == "mp4a" {
                    u16::from_be_bytes(entry.body[16..18].try_into().unwrap())
                } else {
                    0
                };
                (id, entry.kind.clone(), channels)
            })
            .collect()
    }

    #[test]
    fn a_recording_is_finished_in_place_at_stop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.tmp.mp4");
        record_finished(&path, 90);
        let bytes = std::fs::read(&path).unwrap();
        let top = super::boxes::tests::boxes(&bytes);
        assert_eq!(super::boxes::tests::kinds(&top), ["ftyp", "mdat", "moov"]);
        assert_eq!(
            bytes[top[1].at..top[1].at + 8],
            [0, 0, 0, 1, b'm', b'd', b'a', b't'],
            "the 64-bit mdat header, over the placeholder"
        );
    }

    /// What Windows Media Player plays of a finished recording is exactly
    /// what it played of the recording before it was finished -- however
    /// short: one frame, half a second (less than a fragment), 3 seconds.
    #[test]
    fn a_finished_recording_reads_back_exactly_like_its_fragmented_original() {
        let dir = tempfile::tempdir().unwrap();
        for frames in [1, 15, 90] {
            let finished = dir.path().join(format!("rec{frames}.mp4"));
            record_finished(&finished, frames);
            let original = dir.path().join(format!("rec{frames}.frag.mp4"));
            unfinished_copy(&finished, &original);
            let read = mf_samples(&finished);
            assert_eq!(
                read.len(),
                3,
                "{frames} frames: video and two audio streams"
            );
            assert_eq!(read[0].len(), frames as usize, "every frame");
            let before = as_finished(mf_samples(&original));
            assert_reads_like(&read, &before, A_TICK, &format!("{frames} frames"));
        }
    }

    /// Against what Stop made before: Media Foundation's own remux. Its MP4
    /// sink rescales video to a timescale of its own (from the frame
    /// rate), so times may round differently -- by well under 1 ms. (And
    /// see `assert_reads_like` for the frames it marked keyframes.)
    #[test]
    fn a_finished_recording_matches_todays_remux() {
        // Three audio tracks, each its own kind, so their order shows.
        const THREE: [(u32, u16); 3] = [(48_000, 2), (48_000, 1), (44_100, 2)];
        let dir = tempfile::tempdir().unwrap();
        let finished = dir.path().join("rec.mp4");
        let writer = RecordingWriter::new(&finished, &params_with(&THREE)).expect("writer");
        writer.begin_writing().unwrap();
        record_tracks(&writer, &THREE, 90, false);
        writer.finalize().expect("finalize");
        let original = dir.path().join("rec.frag.mp4");
        unfinished_copy(&finished, &original);
        let remuxed = dir.path().join("rec.remuxed.mp4");
        crate::encode::remux::remux(&original, &remuxed, &[0, 1, 2]).expect("remux");
        assert_eq!(
            top_level_tracks(&finished),
            top_level_tracks(&remuxed),
            "the tracks in the same order, numbered the same"
        );

        let marked = assert_reads_like(
            &mf_samples(&finished),
            &mf_samples(&remuxed),
            10_000,
            "against the remux",
        );
        println!("frames the remux marked keyframes that aren't: {marked}");
    }

    /// An audio track starting after the video, and one missing a stretch:
    /// their times come back as recorded.
    #[test]
    fn a_late_start_and_a_gap_read_back_like_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let finished = dir.path().join("rec.mp4");
        let writer = RecordingWriter::new(&finished, &params()).expect("writer");
        writer.begin_writing().unwrap();
        for frame in 0..90u64 {
            let pts = Duration::from_millis(frame * 1000 / 30);
            writer
                .write_video(VideoFrame {
                    pts,
                    data: vec![(frame % 255) as u8; 64 * 64 * 4].into(),
                })
                .unwrap();
            for packet in 0..3u64 {
                let at = pts + Duration::from_millis(packet * 10);
                for (track, (rate, channels)) in TRACKS.iter().enumerate() {
                    let late = track == 0 && at < Duration::from_millis(30);
                    let gap = track == 1
                        && (Duration::from_millis(1000)..Duration::from_millis(1200)).contains(&at);
                    if late || gap {
                        continue;
                    }
                    let samples = AudioSamples {
                        track_id: TrackId::new(track as u32),
                        pts: at,
                        samples: vec![0.1; (*rate as usize / 100) * usize::from(*channels)],
                        sample_rate: *rate,
                        channels: *channels,
                    };
                    writer.write_audio(track, samples).unwrap();
                }
            }
        }
        writer.finalize().expect("finalize");
        let original = dir.path().join("rec.frag.mp4");
        unfinished_copy(&finished, &original);

        let before = mf_samples(&original);
        // (Media Foundation's stream order isn't the tracks'.)
        assert!(
            before[1..].iter().any(|stream| stream[0].time > 0),
            "a track starts late: {:?}",
            before.iter().map(|s| s[0].time).collect::<Vec<_>>()
        );
        let ours = mf_samples(&finished);
        assert_reads_like(&ours, &as_finished(before), A_TICK, "late start and gap");
    }

    /// Finishing a short recording in place takes no longer than the remux
    /// it replaces -- a few milliseconds either way at half a second, where
    /// it's mostly its two flushes to disk (so a power cut can't leave a
    /// broken file), which the remux never did -- and adds only the index
    /// to the file.
    #[test]
    fn a_short_recording_grows_only_by_its_index_and_finishes_faster_than_a_remux() {
        let dir = tempfile::tempdir().unwrap();
        let finished = dir.path().join("rec.mp4");
        record_finished(&finished, 15);
        let bytes = std::fs::read(&finished).unwrap();
        let top = super::boxes::tests::boxes(&bytes);
        let moov = bytes.len() - top[2].at;
        let unfinished = dir.path().join("a.tmp.mp4");
        unfinished_copy(&finished, &unfinished);
        let fragmented = std::fs::metadata(&unfinished).unwrap().len() as usize;
        assert_eq!(bytes.len(), fragmented + moov, "it grows by its index only");

        // The median of 5 of each: one alone is mostly scheduling noise at
        // this size (tests run side by side).
        let median = |mut times: Vec<Duration>| {
            times.sort();
            times[times.len() / 2]
        };
        let mut in_place = Vec::new();
        let mut remux = Vec::new();
        let mut remuxed_size = 0;
        for round in 0..5 {
            let copy = dir.path().join(format!("a{round}.tmp.mp4"));
            unfinished_copy(&finished, &copy);
            let started = Instant::now();
            let outcome = super::soft_remux::finish_in_place(&copy);
            in_place.push(started.elapsed());
            assert_eq!(outcome, super::soft_remux::InPlace::Finished);

            // Not `b.tmp.mp4`: the remux writes that while making `b.mp4`.
            let for_remux = dir.path().join(format!("b{round}.frag.mp4"));
            unfinished_copy(&finished, &for_remux);
            let remuxed = dir.path().join(format!("b{round}.mp4"));
            let started = Instant::now();
            crate::encode::remux::remux(&for_remux, &remuxed, &[0, 1]).expect("remux");
            remux.push(started.elapsed());
            remuxed_size = std::fs::metadata(&remuxed).unwrap().len();
        }
        let (in_place, remux) = (median(in_place), median(remux));
        println!(
            "half a second: in place {in_place:?}, remux {remux:?} (medians of 5); {} bytes in place ({fragmented} fragmented + {moov} index), {remuxed_size} remuxed",
            bytes.len()
        );
        assert!(
            in_place <= remux + Duration::from_millis(5),
            "in place {in_place:?}, remux {remux:?}"
        );
    }

    /// Recovery finds the index by walking the file; Stop has it from the
    /// muxer. The same file either way.
    #[test]
    fn the_sink_and_recovery_finish_a_recording_the_same_way() {
        let dir = tempfile::tempdir().unwrap();
        let finished = dir.path().join("rec.mp4");
        record_finished(&finished, 90);
        let unfinished = dir.path().join("rec.tmp.mp4");
        unfinished_copy(&finished, &unfinished);
        assert_eq!(
            super::soft_remux::finish_in_place(&unfinished),
            super::soft_remux::InPlace::Finished
        );
        assert_eq!(
            std::fs::read(&unfinished).unwrap(),
            std::fs::read(&finished).unwrap()
        );
    }

    #[test]
    fn the_placeholder_doesnt_stop_media_foundation_reading_a_fragmented_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app_recording_1.tmp.mp4");
        record_fragmented(&path);
        assert_eq!(placeholder_at(&std::fs::read(&path).unwrap()), 32);
        assert_eq!(
            crate::encode::remux::count_audio_tracks(&path).unwrap(),
            TRACKS.len()
        );
        assert_holds_three_seconds_of_everything(&read_back(&path));
    }

    /// Export (a remux with the tracks chosen) takes a finished recording.
    #[test]
    fn export_reads_a_finished_recording() {
        let dir = tempfile::tempdir().unwrap();
        let finished = dir.path().join("rec.mp4");
        record_finished(&finished, 90);
        let exported = dir.path().join("export.mp4");
        crate::encode::remux::remux(&finished, &exported, &[1]).expect("export");
        assert_eq!(
            crate::encode::remux::count_audio_tracks(&exported).unwrap(),
            1
        );
        assert_eq!(read_back(&exported)[0].0, 90, "every frame");
    }

    /// Reserves `path` as a recording's name is: with an empty file.
    fn reserve(path: &Path) -> Result<std::path::PathBuf, crate::error::AppError> {
        std::fs::write(path, b"").unwrap();
        Ok(path.to_path_buf())
    }

    /// A recording cut off (fragmented) is finished and moved to its name.
    #[test]
    fn a_finished_recording_is_a_plain_mp4() {
        let dir = tempfile::tempdir().unwrap();
        let fragmented = fragmented_recording(dir.path());
        assert!(top_level(&fragmented).contains(&"moof".to_owned()));
        let dest = dir.path().join("app_2026.mp4");

        let saved =
            super::finish(&fragmented, || reserve(&dest), || Ok(TRACKS.len())).expect("finish");
        assert_eq!(saved, dest);
        assert!(!fragmented.exists(), "the fragmented one is gone");
        assert!(
            !top_level(&dest).contains(&"moof".to_owned()),
            "{:?}",
            top_level(&dest)
        );
        assert_holds_three_seconds_of_everything(&read_back(&dest));
    }

    /// The name is reserved only once the file is finished: a finish cut
    /// off leaves no empty file holding it.
    #[test]
    fn the_name_is_reserved_only_once_the_file_is_finished() {
        let dir = tempfile::tempdir().unwrap();
        let fragmented = fragmented_recording(dir.path());
        let dest = dir.path().join("app_2026.mp4");
        super::finish(
            &fragmented,
            || {
                let bytes = std::fs::read(&fragmented).unwrap();
                let top = super::boxes::tests::boxes(&bytes);
                assert_eq!(super::boxes::tests::kinds(&top), ["ftyp", "mdat", "moov"]);
                reserve(&dest)
            },
            || Ok(TRACKS.len()),
        )
        .expect("finish");
        assert!(dest.exists());
    }

    /// Media Foundation's track count is for the old remux only: finishing
    /// in place keeps every track, and MF may not open a file whose finish
    /// was cut off between its two writes.
    #[test]
    fn a_soft_finish_never_asks_media_foundation_for_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let fragmented = fragmented_recording(dir.path());
        let dest = dir.path().join("app_2026.mp4");
        super::finish(
            &fragmented,
            || reserve(&dest),
            || panic!("asked for tracks"),
        )
        .expect("finish");
    }

    /// A recording written before they were finished in place -- no
    /// placeholder -- is remuxed, as then.
    #[test]
    fn a_legacy_recording_takes_todays_remux() {
        let dir = tempfile::tempdir().unwrap();
        let finished = dir.path().join("rec.mp4");
        record_finished(&finished, 90);
        let legacy = dir.path().join("app_recording_1.tmp.mp4");
        legacy_copy(&finished, &legacy);
        let dest = dir.path().join("app_2026.mp4");
        let asked = std::cell::Cell::new(false);

        super::finish(
            &legacy,
            || reserve(&dest),
            || {
                asked.set(true);
                Ok(TRACKS.len())
            },
        )
        .expect("finish");
        assert!(asked.get(), "the remux needs the track count");
        assert!(!legacy.exists());
        let kinds = top_level(&dest);
        assert!(!kinds.contains(&"moof".to_owned()), "{kinds:?}");
        assert_holds_three_seconds_of_everything(&read_back(&dest));
    }

    /// The file can't be written to (here: read-only), so it can't be
    /// finished in place: it's moved to its name as it is, fragmented,
    /// which reads all the same.
    #[test]
    fn a_recording_that_cant_be_finished_in_place_is_kept_fragmented() {
        let dir = tempfile::tempdir().unwrap();
        let fragmented = fragmented_recording(dir.path());
        set_read_only(&fragmented, true);
        let dest = dir.path().join("app_2026.mp4");

        let saved = super::finish(&fragmented, || reserve(&dest), || Ok(TRACKS.len()));
        set_read_only(&dest, false);
        set_read_only(&fragmented, false);
        saved.expect("finish");
        assert!(!fragmented.exists());
        assert!(top_level(&dest).contains(&"moof".to_owned()));
        assert_holds_three_seconds_of_everything(&read_back(&dest));
    }

    fn set_read_only(path: &Path, read_only: bool) {
        if let Ok(meta) = std::fs::metadata(path) {
            let mut permissions = meta.permissions();
            permissions.set_readonly(read_only);
            std::fs::set_permissions(path, permissions).unwrap();
        }
    }

    /// A remux writes a whole second copy before deleting the first: on a
    /// nearly full drive it would fill the disk to nothing, then fail.
    #[test]
    fn a_remux_needs_room_for_a_copy_and_what_recording_keeps_free() {
        use crate::disk_space::MIN_FREE_BYTES;
        const GB: u64 = 1 << 30;
        assert!(super::room_to_remux(20 * GB, 15 * GB));
        assert!(super::room_to_remux(15 * GB + MIN_FREE_BYTES, 15 * GB));
        assert!(!super::room_to_remux(15 * GB + MIN_FREE_BYTES - 1, 15 * GB));
        assert!(!super::room_to_remux(10 * GB, 15 * GB));
        assert!(!super::room_to_remux(0, u64::MAX), "no overflow");
    }

    /// Remuxing failed (here: a track it hasn't): the recording is kept
    /// all the same, fragmented.
    #[test]
    fn a_recording_that_wont_remux_is_kept_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let finished = dir.path().join("rec.mp4");
        record_finished(&finished, 90);
        let fragmented = dir.path().join("app_recording_1.tmp.mp4");
        legacy_copy(&finished, &fragmented);
        let dest = dir.path().join("app_2026.mp4");

        super::finish(&fragmented, || reserve(&dest), || Ok(TRACKS.len() + 3)).expect("finish");
        assert!(!fragmented.exists());
        assert!(top_level(&dest).contains(&"moof".to_owned()));
        assert_holds_three_seconds_of_everything(&read_back(&dest));
    }

    /// Nothing else -- another copy of PolyRec, say -- can delete or write
    /// to a recording while it's being written; reading it is fine.
    #[test]
    fn a_recording_being_written_is_held_for_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app_recording_1.tmp.mp4");
        let writer = RecordingWriter::new(&path, &params()).expect("writer");
        writer.begin_writing().unwrap();
        record(&writer, 2, false);

        let deleted = std::fs::remove_file(&path);
        let written = std::fs::OpenOptions::new().append(true).open(&path);
        let read = std::fs::File::open(&path);
        writer.finalize().expect("finalize");
        assert!(deleted.is_err(), "deleted while being written");
        assert!(written.is_err(), "opened for writing twice");
        assert!(read.is_ok(), "can't be read: {read:?}");
    }

    /// HEVC from a hardware encoder at 1080p -- what most recordings are,
    /// and what Media Foundation's own fragmented MP4 sink won't take --
    /// reads back, decodes and remuxes. Needs a GPU encoder: ignored.
    #[test]
    #[ignore = "needs a hardware HEVC encoder"]
    fn hardware_hevc_through_the_own_sink() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hevc.frag.mp4");
        let params = EncoderParams {
            width: 1920,
            height: 1080,
            codec: Codec::H265,
            bitrate_bps: 20_000_000,
            allow_hardware_encode: true,
            ..params()
        };
        let writer = RecordingWriter::new(&path, &params).expect("writer");
        writer.begin_writing().unwrap();
        let started = Instant::now();
        for frame in 0..90u64 {
            let pts = Duration::from_millis(frame * 1000 / 30);
            let data = vec![(frame * 7 % 255) as u8; 1920 * 1080 * 4];
            writer
                .write_video(VideoFrame {
                    pts,
                    data: data.into(),
                })
                .unwrap();
            for packet in 0..3u64 {
                for (track, (rate, channels)) in TRACKS.iter().enumerate() {
                    let samples = AudioSamples {
                        track_id: TrackId::new(track as u32),
                        pts: pts + Duration::from_millis(packet * 10),
                        samples: vec![0.1; (*rate as usize / 100) * usize::from(*channels)],
                        sample_rate: *rate,
                        channels: *channels,
                    };
                    writer.write_audio(track, samples).unwrap();
                }
            }
        }
        writer.finalize().expect("finalize");
        println!("encoded 3 s in {:?}", started.elapsed());
        assert_holds_three_seconds_of_everything(&read_back(&path));
        assert_eq!(decoded_frames(&path), 90);
        let plain = dir.path().join("hevc.mp4");
        crate::encode::remux::remux(&path, &plain, &[0, 1]).expect("remux");
        assert_holds_three_seconds_of_everything(&read_back(&plain));
    }

    /// The child half of `a_recording_cut_off_reads_back_to_its_last_fragment`:
    /// records into the file it's given until it's killed.
    #[test]
    #[ignore = "run by a_recording_cut_off_reads_back_to_its_last_fragment"]
    fn record_until_killed() {
        let Ok(path) = std::env::var("POLYREC_RECORD_UNTIL_KILLED") else {
            return;
        };
        let writer = RecordingWriter::new(Path::new(&path), &params()).expect("writer");
        writer.begin_writing().unwrap();
        record(&writer, 600, true);
    }

    /// PolyRec killed partway through a recording -- no Finalize, nothing
    /// flushed: what was written up to the last fragment reads back. A
    /// plain MP4 cut off like this can't be opened at all.
    #[test]
    fn a_recording_cut_off_reads_back_to_its_last_fragment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cut_off.tmp.mp4");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "encode::fmp4::tests::record_until_killed",
            ])
            .env("POLYREC_RECORD_UNTIL_KILLED", &path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        // Long enough for a few one-second fragments, from when the file is
        // made -- a little before the recording itself starts.
        let started = Instant::now();
        while !path.exists() && started.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(3500));
        child.kill().unwrap();
        child.wait().unwrap();

        // Up to the last whole fragment: the one being filled is lost.
        let spans = read_back(&path);
        assert_eq!(spans.len(), 3, "{spans:?}");
        for (i, (_, span)) in spans.iter().enumerate() {
            assert!(
                *span >= Duration::from_millis(1500),
                "stream {i}: only {span:?} of ~3.5 s"
            );
        }
        assert_eq!(
            crate::encode::remux::count_audio_tracks(&path).unwrap(),
            TRACKS.len()
        );

        // Finished at the next start, the same goes.
        assert_eq!(
            super::soft_remux::finish_in_place(&path),
            super::soft_remux::InPlace::Finished
        );
        let spans = read_back(&path);
        assert_eq!(spans.len(), 3, "{spans:?}");
        for (i, (_, span)) in spans.iter().enumerate() {
            assert!(
                *span >= Duration::from_millis(1500),
                "finished: stream {i}: only {span:?} of ~3.5 s"
            );
        }
    }
}
