//! Fragmented MP4, written by PolyRec itself, so a recording survives a
//! crash. Media Foundation's MP4 sink writes the file's index only when it
//! finishes, so a recording that never finishes (PolyRec killed, a crash,
//! a power cut, a driver reset) can't be read at all; its fragmented MP4
//! sink, which can, takes H.264 only, and HEVC is PolyRec's default. So a
//! media sink of PolyRec's own (`sink`) takes the sink writer's place
//! behind the encoders, which stay Media Foundation's, and writes the file
//! a fragment (one GOP, about a second) at a time: a recording cut off
//! loses its last fragment, no more. At Stop it's remuxed into a plain MP4
//! (`encode::remux`), so the finished file is what it always was.

pub mod boxes;
pub mod muxer;
pub mod nal;
pub mod sink;

use crate::error::AppError;
use std::path::Path;

/// Makes the fragmented recording at `fragmented` (with `audio_tracks`
/// tracks of audio) the plain MP4 at `dest`, removing it. If remuxing
/// fails -- or there's no room on the drive for the second copy it writes
/// -- it's moved to `dest` as it is: a fragmented MP4 plays all the same.
pub fn finish(fragmented: &Path, dest: &Path, audio_tracks: usize) -> Result<(), AppError> {
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
        return keep_fragmented(fragmented, dest);
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
            keep_fragmented(fragmented, dest)
        }
    }
}

fn keep_fragmented(fragmented: &Path, dest: &Path) -> Result<(), AppError> {
    use crate::error::ResultExt;
    std::fs::rename(fragmented, dest).encode_ctx(format_args!(
        "moving {} to {}",
        fragmented.display(),
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

    fn params() -> EncoderParams {
        EncoderParams {
            container: Container::FragmentedMp4,
            ..EncoderParams::tiny_test(Codec::H264, &TRACKS)
        }
    }

    /// `seconds` of 30 fps video and both tracks' audio (10 ms packets, as
    /// WASAPI's), in real time if `realtime`.
    fn record(writer: &RecordingWriter, seconds: u64, realtime: bool) {
        let started = Instant::now();
        for frame in 0..seconds * 30 {
            let pts = Duration::from_millis(frame * 1000 / 30);
            writer
                .write_video(VideoFrame {
                    pts,
                    data: vec![(frame % 255) as u8; 64 * 64 * 4].into(),
                })
                .expect("write_video");
            for packet in 0..3u64 {
                for (track, (rate, channels)) in TRACKS.iter().enumerate() {
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
    }

    /// The configuration records and length-prefixed samples are right: a
    /// decoder takes them.
    #[test]
    fn a_fragmented_recording_decodes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.tmp.mp4");
        let writer = RecordingWriter::new(&path, &params()).expect("writer");
        writer.begin_writing().unwrap();
        record(&writer, 3, false);
        writer.finalize().expect("finalize");
        assert_eq!(decoded_frames(&path), 90);
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
        let writer = RecordingWriter::new(&path, &params()).expect("writer");
        writer.begin_writing().unwrap();
        record(&writer, 3, false);
        writer.finalize().expect("finalize");
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

    /// 3 s of fragmented recording at `path`, video and two audio tracks.
    pub(crate) fn record_fragmented(path: &Path) {
        let writer = RecordingWriter::new(path, &params()).expect("writer");
        writer.begin_writing().unwrap();
        record(&writer, 3, false);
        writer.finalize().expect("finalize");
    }

    #[test]
    fn a_finished_recording_is_a_plain_mp4() {
        let dir = tempfile::tempdir().unwrap();
        let fragmented = fragmented_recording(dir.path());
        assert!(top_level(&fragmented).contains(&"moof".to_owned()));
        // Its name reserved by an empty file, as recordings' are.
        let dest = dir.path().join("app_2026.mp4");
        std::fs::write(&dest, b"").unwrap();

        super::finish(&fragmented, &dest, TRACKS.len()).expect("finish");
        assert!(!fragmented.exists(), "the fragmented one is gone");
        assert!(
            !top_level(&dest).contains(&"moof".to_owned()),
            "{:?}",
            top_level(&dest)
        );
        assert_holds_three_seconds_of_everything(&read_back(&dest));
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
        let fragmented = fragmented_recording(dir.path());
        let dest = dir.path().join("app_2026.mp4");
        std::fs::write(&dest, b"").unwrap();

        super::finish(&fragmented, &dest, TRACKS.len() + 3).expect("finish");
        assert!(!fragmented.exists());
        assert!(top_level(&dest).contains(&"moof".to_owned()));
        assert_holds_three_seconds_of_everything(&read_back(&dest));
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
    }
}
