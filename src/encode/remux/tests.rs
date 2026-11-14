use super::*;

use crate::config::Codec;
use crate::encode::{EncoderParams, RecordingWriter};
use crate::types::{AudioSamples, TrackId, VideoFrame};
use std::time::Duration;

fn make_test_mp4(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("source.mp4");
    let writer = RecordingWriter::new(
        &path,
        &EncoderParams::tiny_test(Codec::H264, &[(48000u32, 2u16)]),
    )
    .expect("RecordingWriter::new");
    writer.begin_writing().expect("begin_writing");
    writer
        .write_video(VideoFrame {
            pts: Duration::ZERO,
            data: vec![0u8; 64 * 64 * 4].into(),
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
        &EncoderParams::tiny_test(Codec::H264, &[(48000u32, 2u16), (48000u32, 2u16)]),
    )
    .expect("RecordingWriter::new");
    writer.begin_writing().expect("begin_writing");
    writer
        .write_video(VideoFrame {
            pts: Duration::ZERO,
            data: vec![0u8; 64 * 64 * 4].into(),
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

fn make_three_track_test_mp4(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("three_track_source.mp4");
    let writer = RecordingWriter::new(
        &path,
        &EncoderParams::tiny_test(
            Codec::H264,
            &[(48000u32, 2u16), (48000u32, 2u16), (48000u32, 2u16)],
        ),
    )
    .expect("RecordingWriter::new");
    writer.begin_writing().expect("begin_writing");
    writer
        .write_video(VideoFrame {
            pts: Duration::ZERO,
            data: vec![0u8; 64 * 64 * 4].into(),
        })
        .expect("write_video");
    for track_idx in 0..3 {
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

fn failing_export(output: &Path) -> Result<(), AppError> {
    std::fs::write(output, b"half an mp4").unwrap();
    Err(AppError::Encode(
        "ReadSample: stream 1 reported error".into(),
    ))
}

/// An export that failed partway used to leave its half-written file
/// under the name the user chose.
#[test]
fn a_failed_export_leaves_nothing_under_the_chosen_name() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("recording.mp4");
    let dest = dir.path().join("export.mp4");
    assert!(export_to(&src, &dest, failing_export).is_err());
    let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().collect();
    assert!(left.is_empty(), "{left:?}");
}

/// A file the user chose to replace stays as it was when the export
/// that was to replace it fails.
#[test]
fn a_failed_export_keeps_the_file_it_was_to_replace() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("recording.mp4");
    let dest = dir.path().join("export.mp4");
    std::fs::write(&dest, b"last week's export").unwrap();
    assert!(export_to(&src, &dest, failing_export).is_err());
    assert_eq!(std::fs::read(&dest).unwrap(), b"last week's export");
}

#[test]
fn a_finished_export_lands_under_the_chosen_name() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("recording.mp4");
    let dest = dir.path().join("export.mp4");
    std::fs::write(&dest, b"last week's export").unwrap();
    let written = export_to(&src, &dest, |output| {
        std::fs::write(output, b"the export").unwrap();
        Ok(())
    });
    assert_eq!(written.unwrap(), dest);
    assert_eq!(std::fs::read(&dest).unwrap(), b"the export");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

/// Picking the recording itself in the Save dialog -- Windows' own
/// "replace it?" agreed to -- had Media Foundation truncate it while
/// still reading it.
#[test]
fn exporting_a_recording_over_itself_is_refused_and_leaves_it_whole() {
    let dir = tempfile::tempdir().unwrap();
    let src = make_test_mp4(dir.path());
    let before = std::fs::read(&src).unwrap();
    let same = dir.path().join(".").join("SOURCE.MP4");
    assert!(is_same_file(&src, &same));
    assert!(export_grouped(&src, &same, &[vec![0]]).is_err());
    assert_eq!(std::fs::read(&src).unwrap(), before);
}

#[test]
fn different_files_and_a_file_not_there_are_not_the_same_file() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.mp4");
    let b = dir.path().join("b.mp4");
    std::fs::write(&a, b"a").unwrap();
    std::fs::write(&b, b"b").unwrap();
    assert!(!is_same_file(&a, &b));
    assert!(!is_same_file(&a, &dir.path().join("c.mp4")));
}

#[test]
fn export_grouped_merges_within_a_group_but_keeps_groups_separate() {
    let dir = tempfile::tempdir().unwrap();
    let source = make_three_track_test_mp4(dir.path());
    let dest = dir.path().join("grouped.mp4");
    // Tracks 0 and 1 (e.g. a multi-process app) merge into one output
    // track; track 2 (e.g. the mic) stays its own separate track.
    let result = export_grouped(&source, &dest, &[vec![0, 1], vec![2]]);
    assert!(result.is_ok(), "export_grouped failed: {:?}", result.err());
    assert_eq!(
        count_audio_tracks(&dest).expect("count_audio_tracks failed"),
        2,
        "expected one merged track + one separate track"
    );
}

#[test]
fn export_grouped_with_only_single_track_groups_behaves_like_remux() {
    let dir = tempfile::tempdir().unwrap();
    let source = make_three_track_test_mp4(dir.path());
    let dest = dir.path().join("all_separate.mp4");
    let result = export_grouped(&source, &dest, &[vec![0], vec![1], vec![2]]);
    assert!(result.is_ok(), "export_grouped failed: {:?}", result.err());
    assert_eq!(
        count_audio_tracks(&dest).expect("count_audio_tracks failed"),
        3,
        "no group has 2+ indices, so nothing should get merged"
    );
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

/// Regression test for a real bug: `mix_tracks`'s sink writer left
/// `MF_SINK_WRITER_DISABLE_THROTTLING` unset, so `IMFSinkWriter`
/// defaulted to pacing `WriteSample` to real-time speed (its assumption
/// for any pipeline with an active encoder MFT, which the mixed audio
/// track always has). A real ~5.5min recording took 15+ minutes and
/// never finished before this was found and fixed -- decode+mix alone
/// took 1.3s, confirming the write loop itself was the bottleneck.
/// Bounded by a timeout thread (not an inline call) so a real
/// regression fails fast instead of hanging the test run.
#[test]
fn mix_tracks_does_not_throttle_to_real_time_on_a_longer_recording() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("longer_two_track_source.mp4");
    let writer = RecordingWriter::new(
        &path,
        &EncoderParams::tiny_test(Codec::H264, &[(48000u32, 2u16), (48000u32, 2u16)]),
    )
    .expect("RecordingWriter::new");
    writer.begin_writing().expect("begin_writing");
    writer
        .write_video(VideoFrame {
            pts: Duration::ZERO,
            data: vec![0u8; 64 * 64 * 4].into(),
        })
        .expect("write_video");
    // ~3s of audio per track -- long enough that real-time throttling
    // (if it regresses) makes this an actually slow test, not just a
    // theoretical concern.
    const CHUNKS: u64 = 30;
    for chunk in 0..CHUNKS {
        let pts = Duration::from_millis(chunk * 100);
        for track_idx in 0..2 {
            writer
                .write_audio(
                    track_idx,
                    AudioSamples {
                        track_id: TrackId::new(track_idx as u32),
                        pts,
                        samples: vec![0.1f32; 4800 * 2],
                        sample_rate: 48000,
                        channels: 2,
                    },
                )
                .expect("write_audio");
        }
    }
    let source = writer.finalize().expect("finalize");

    let dest = dir.path().join("mixed_longer.mp4");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let start = std::time::Instant::now();
        let result = mix_tracks(&source, &dest, &[0, 1]);
        let _ = tx.send((result, start.elapsed()));
    });
    let (result, elapsed) = rx
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("mix_tracks did not complete within 20s -- real-time throttling regression?");
    result.expect("mix_tracks failed");
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "mix_tracks took {elapsed:.2?} for a ~3s recording -- should be near-instant, not throttled to real time"
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

/// A source reader whose stream reports a decode error (and no sample)
/// on every read, for up to `errors` reads; end of stream after that,
/// so a loop that reads on past the error ends instead of spinning.
#[windows::core::implement(IMFSourceReader)]
struct FailingReader {
    errors: std::cell::Cell<u32>,
}

#[allow(non_snake_case)]
impl windows::Win32::Media::MediaFoundation::IMFSourceReader_Impl for FailingReader_Impl {
    fn ReadSample(
        &self,
        stream: u32,
        _control: u32,
        actual: *mut u32,
        flags: *mut u32,
        timestamp: *mut i64,
        _sample: windows::core::OutRef<IMFSample>,
    ) -> windows::core::Result<()> {
        let left = self.errors.get();
        let flag = if left > 0 {
            self.errors.set(left - 1);
            MF_SOURCE_READERF_ERROR
        } else {
            MF_SOURCE_READERF_ENDOFSTREAM
        };
        unsafe {
            *actual = stream;
            *flags = flag.0 as u32;
            *timestamp = 0;
        }
        Ok(())
    }
    fn GetStreamSelection(&self, _: u32) -> windows::core::Result<windows::core::BOOL> {
        Err(windows::Win32::Foundation::E_NOTIMPL.into())
    }
    fn SetStreamSelection(&self, _: u32, _: windows::core::BOOL) -> windows::core::Result<()> {
        Err(windows::Win32::Foundation::E_NOTIMPL.into())
    }
    fn GetNativeMediaType(&self, _: u32, _: u32) -> windows::core::Result<IMFMediaType> {
        Err(windows::Win32::Foundation::E_NOTIMPL.into())
    }
    fn GetCurrentMediaType(&self, _: u32) -> windows::core::Result<IMFMediaType> {
        Err(windows::Win32::Foundation::E_NOTIMPL.into())
    }
    fn SetCurrentMediaType(
        &self,
        _: u32,
        _: *const u32,
        _: windows::core::Ref<IMFMediaType>,
    ) -> windows::core::Result<()> {
        Err(windows::Win32::Foundation::E_NOTIMPL.into())
    }
    fn SetCurrentPosition(
        &self,
        _: *const windows::core::GUID,
        _: *const windows::Win32::System::Com::StructuredStorage::PROPVARIANT,
    ) -> windows::core::Result<()> {
        Err(windows::Win32::Foundation::E_NOTIMPL.into())
    }
    fn Flush(&self, _: u32) -> windows::core::Result<()> {
        Err(windows::Win32::Foundation::E_NOTIMPL.into())
    }
    fn GetServiceForStream(
        &self,
        _: u32,
        _: *const windows::core::GUID,
        _: *const windows::core::GUID,
        _: *mut *mut std::ffi::c_void,
    ) -> windows::core::Result<()> {
        Err(windows::Win32::Foundation::E_NOTIMPL.into())
    }
    fn GetPresentationAttribute(
        &self,
        _: u32,
        _: *const windows::core::GUID,
    ) -> windows::core::Result<windows::Win32::System::Com::StructuredStorage::PROPVARIANT> {
        Err(windows::Win32::Foundation::E_NOTIMPL.into())
    }
}

/// Media Foundation: after `MF_SOURCE_READERF_ERROR` the reader
/// delivers nothing more and `ReadSample` mustn't be called again. A
/// track that fails to decode fails the mix instead of being read
/// forever.
#[test]
fn a_track_that_fails_to_decode_fails_the_mix() {
    let mut track = TrackDecoder {
        reader: FailingReader {
            errors: std::cell::Cell::new(1000),
        }
        .into(),
        src_idx: 1,
        idx: 0,
        rate: 48_000,
    };
    let error = track.next_piece().expect_err("a decode error");
    assert!(
        error.to_string().contains("audio 0"),
        "names the track: {error}"
    );
}

/// A track's pieces, from memory.
struct FakePieces(VecDeque<(usize, Vec<i16>)>);

impl PcmPieces for FakePieces {
    fn next_piece(&mut self) -> Result<Option<PcmPiece>, AppError> {
        Ok(self.0.pop_front())
    }
}

/// Mixes `tracks` (each a list of pieces) the way an export does: at
/// unity gain, and again scaled if that clipped.
fn mix_whole(tracks: &[Vec<(usize, Vec<i16>)>], channels: usize) -> Vec<i16> {
    let run = |scale: f64| {
        let inputs: Vec<MixInput<FakePieces>> = tracks
            .iter()
            .map(|pieces| {
                let mut input = MixInput::new(FakePieces(pieces.clone().into()));
                input.prime(channels).unwrap();
                input
            })
            .collect();
        let mut mix = Mix::new(inputs, channels, scale);
        let mut out = Vec::new();
        while let Some((frame, chunk)) = mix.next_chunk().unwrap() {
            assert_eq!(frame * channels, out.len(), "chunks aren't contiguous");
            out.extend_from_slice(&chunk);
        }
        (mix.peak, out)
    };
    let (peak, out) = run(1.0);
    if peak > i16::MAX as u32 {
        run(scale_for(peak)).1
    } else {
        out
    }
}

/// How a merged track was made before `Mix`: every track
/// decoded whole, summed (each aligned by its start), then scaled down
/// to full scale only if the sum would clip. The streaming mix has to
/// come out the same, sample for sample.
fn reference_mix(tracks: &[(usize, Vec<i16>)], channels: usize) -> Vec<i16> {
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
    acc.iter().map(|&v| scaled(v, scale_for(peak))).collect()
}

fn whole(offset: usize, samples: Vec<i16>) -> Vec<(usize, Vec<i16>)> {
    vec![(offset, samples)]
}

#[test]
fn mix_sums_overlapping_tracks_at_unity_gain_when_no_clipping() {
    let tracks = vec![
        whole(0, vec![1000i16, -1000, 500, -500]),
        whole(0, vec![2000i16, -2000, 300, -300]),
    ];
    assert_eq!(mix_whole(&tracks, 2), vec![3000, -3000, 800, -800]);
}

#[test]
fn mix_aligns_tracks_by_frame_offset() {
    // Track 1 starts at frame 0 (2 stereo frames = 4 samples), track 2
    // starts 1 frame later (offset 1 -> sample index 2) -- they should
    // only overlap on the second frame of track 1 / first frame of track 2.
    let tracks = vec![
        whole(0, vec![100i16, 100, 200, 200]),
        whole(1, vec![50i16, 50]),
    ];
    assert_eq!(mix_whole(&tracks, 2), vec![100, 100, 250, 250]);
}

#[test]
fn mix_scales_down_only_when_sum_would_clip() {
    let tracks = vec![whole(0, vec![30000i16]), whole(0, vec![30000i16])];
    // 30000 + 30000 = 60000, clips i16 (max 32767) -- must be scaled
    // down to exactly the max, not left clipped/wrapped.
    assert_eq!(mix_whole(&tracks, 1), vec![i16::MAX]);
}

#[test]
fn mix_of_nothing_is_nothing() {
    assert!(mix_whole(&[], 2).is_empty());
    assert!(mix_whole(&[vec![], whole(0, vec![])], 2).is_empty());
}

/// The streaming mix against the whole-track one it replaced, over
/// tracks of every shape: several, starting late, ending early, longer
/// than many chunks, split into pieces of every size (including empty
/// ones), loud enough to clip or not.
#[test]
fn mix_matches_summing_whole_tracks_sample_for_sample() {
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = |below: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % below.max(1)
    };
    let channels = 2;
    for case in 0..200 {
        let track_count = next(4) as usize + 1;
        let loud = case % 3 == 0;
        let mut whole_tracks = Vec::new();
        let mut piece_tracks = Vec::new();
        for _ in 0..track_count {
            let offset = if next(3) == 0 {
                0
            } else {
                next(3 * MIX_CHUNK_FRAMES as u64) as usize
            };
            let frames = next(4 * MIX_CHUNK_FRAMES as u64) as usize;
            let amplitude = if loud { 30000 } else { 9000 };
            let samples: Vec<i16> = (0..frames * channels)
                .map(|_| (next(2 * amplitude) as i64 - amplitude as i64) as i16)
                .collect();
            let mut pieces = Vec::new();
            let mut at = 0;
            while at < samples.len() || pieces.is_empty() {
                let len = (next(3000) as usize * channels).min(samples.len() - at);
                // Only the first piece's frame counts.
                let frame = if pieces.is_empty() {
                    offset
                } else {
                    next(1_000_000) as usize
                };
                pieces.push((frame, samples[at..at + len].to_vec()));
                at += len;
            }
            whole_tracks.push((offset, samples));
            piece_tracks.push(pieces);
        }
        assert_eq!(
            mix_whole(&piece_tracks, channels),
            reference_mix(&whole_tracks, channels),
            "case {case}"
        );
    }
}

/// A mix that would clip is made again scaled to fit (see
/// `do_export_grouped`), not written hard-clipped: two copies of a loud
/// tone sum to nearly twice full scale. Clipped, most of each cycle
/// would sit at full scale; scaled, only its crests come near it.
#[test]
fn mixing_tracks_that_would_clip_scales_them_to_fit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("loud.mp4");
    let writer = RecordingWriter::new(
        &path,
        &EncoderParams::tiny_test(Codec::H264, &[(48000u32, 2u16), (48000u32, 2u16)]),
    )
    .expect("RecordingWriter::new");
    writer.begin_writing().expect("begin_writing");
    writer
        .write_video(VideoFrame {
            pts: Duration::ZERO,
            data: vec![0u8; 64 * 64 * 4].into(),
        })
        .expect("write_video");
    for chunk in 0..10u32 {
        for track in 0..2 {
            let samples = (0..4800 * 2)
                .map(|i| ((chunk * 4800 + i / 2) as f32 * 0.05).sin() * 0.9)
                .collect();
            writer
                .write_audio(
                    track,
                    AudioSamples {
                        track_id: TrackId::new(track as u32),
                        pts: Duration::from_millis(u64::from(chunk) * 100),
                        samples,
                        sample_rate: 48000,
                        channels: 2,
                    },
                )
                .expect("write_audio");
        }
    }
    let source = writer.finalize().expect("finalize");
    let dest = dir.path().join("mixed.mp4");
    mix_tracks(&source, &dest, &[0, 1]).expect("mix_tracks");

    let _mf = MfSession::start_mta().unwrap();
    let url = HSTRING::from(dest.as_path());
    let reader: IMFSourceReader = unsafe { MFCreateSourceReaderFromURL(&url, None).unwrap() };
    let layout = unsafe { discover_stream_layout(&reader).unwrap() };
    let mut decoder = unsafe { TrackDecoder::open(&url, &layout, 0, 48000, 2).unwrap() };
    let mut samples = Vec::new();
    while let Some((_, piece)) = decoder.next_piece().unwrap() {
        samples.extend(piece);
    }
    let near_full_scale = samples.iter().filter(|s| s.unsigned_abs() > 31000).count();
    let share = near_full_scale as f64 / samples.len() as f64;
    assert!(
        samples.len() > 40_000,
        "too little audio: {}",
        samples.len()
    );
    assert!(
        share < 0.25,
        "{:.0}% of samples near full scale: clipped",
        share * 100.0
    );
    let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
    assert!(peak > 26_000, "scaled down too far: peak {peak}");
}

/// What's recorded is what's heard: a tone on one track of a recording
/// -- written crash-safe, then finished into a plain MP4 at Stop --
/// decodes back at its loudness, and the quiet track beside it stays
/// quiet. Checks everything after capture without playing a sound (the
/// diagnostics this replaces played one out loud).
#[test]
fn a_recorded_tone_comes_back_at_its_loudness_on_its_own_track() {
    let dir = tempfile::tempdir().unwrap();
    let fragmented = dir.path().join("tone_recording_1.tmp.mp4");
    let writer = RecordingWriter::new(
        &fragmented,
        &EncoderParams {
            container: crate::encode::Container::FragmentedMp4,
            ..EncoderParams::tiny_test(Codec::H264, &[(48000u32, 2u16), (48000u32, 2u16)])
        },
    )
    .expect("RecordingWriter::new");
    writer.begin_writing().expect("begin_writing");
    // 1 s in 10 ms packets, as WASAPI hands them over: a 440 Hz tone at
    // half scale on track 0, silence on track 1.
    for packet in 0..100u32 {
        let pts = Duration::from_millis(u64::from(packet) * 10);
        if packet % 3 == 0 {
            writer
                .write_video(VideoFrame {
                    pts,
                    data: vec![0u8; 64 * 64 * 4].into(),
                })
                .expect("write_video");
        }
        let tone = (0..480u32)
            .flat_map(|i| {
                let t = (packet * 480 + i) as f32 / 48000.0;
                let v = (t * 440.0 * std::f32::consts::TAU).sin() * 0.5;
                [v, v]
            })
            .collect();
        for (track, samples) in [(0, tone), (1, vec![0.0f32; 480 * 2])] {
            writer
                .write_audio(
                    track,
                    AudioSamples {
                        track_id: TrackId::new(track as u32),
                        pts,
                        samples,
                        sample_rate: 48000,
                        channels: 2,
                    },
                )
                .expect("write_audio");
        }
    }
    writer.finalize().expect("finalize");

    let _mf = MfSession::start_mta().unwrap();
    let url = HSTRING::from(fragmented.as_path());
    let reader: IMFSourceReader = unsafe { MFCreateSourceReaderFromURL(&url, None).unwrap() };
    let layout = unsafe { discover_stream_layout(&reader).unwrap() };
    let peaks: Vec<u16> = (0..2)
        .map(|track| {
            let mut decoder =
                unsafe { TrackDecoder::open(&url, &layout, track, 48000, 2).unwrap() };
            let mut peak = 0;
            while let Some((_, piece)) = decoder.next_piece().unwrap() {
                peak = piece.iter().map(|s| s.unsigned_abs()).fold(peak, u16::max);
            }
            peak
        })
        .collect();
    // Half scale is 16384, give or take what AAC does to a tone.
    assert!(
        (13_000..=20_000).contains(&peaks[0]),
        "the tone came back at {}",
        peaks[0]
    );
    assert!(peaks[1] < 300, "the quiet track came back at {}", peaks[1]);
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
    let writer = RecordingWriter::new(&path, &EncoderParams::tiny_test(Codec::H264, &[]))
        .expect("RecordingWriter::new");
    writer.begin_writing().expect("begin_writing");
    // Several frames, not one -- a single WriteSample followed immediately
    // by Finalize occasionally raced Media Foundation's async software
    // encoder on CI's (virtualized, no real GPU) runner: Finalize decided
    // the stream had processed zero samples (MF_E_SINK_NO_SAMPLES_PROCESSED,
    // 0xC00D4A44) because the lone sample hadn't reached the encoder MFT
    // yet. A real recording never hits this -- it always spans real
    // wall-clock time across many frames -- so this only ever showed up in
    // this exact "write once, finalize immediately" test shape.
    for i in 0..3u32 {
        writer
            .write_video(VideoFrame {
                pts: Duration::from_millis(33 * i as u64),
                data: vec![0u8; 64 * 64 * 4].into(),
            })
            .expect("write_video");
    }
    let path = writer.finalize().expect("finalize");

    assert_eq!(
        count_audio_tracks(&path).expect("count_audio_tracks failed"),
        0
    );
}

#[test]
fn read_error_context_matches_the_messages_each_caller_used_to_build() {
    // copy_passthrough_samples' former literals:
    let (ctx, suffix) = read_error_context(None);
    assert_eq!(ctx, "ReadSample");
    assert_eq!(
        format!("ReadSample: stream 3 reported error{suffix}"),
        "ReadSample: stream 3 reported error"
    );
    // concat_and_trim's former format strings:
    let (ctx, suffix) = read_error_context(Some(r"segment C:\seg_0.mp4"));
    assert_eq!(ctx, r"ReadSample(segment C:\seg_0.mp4)");
    assert_eq!(
        format!("ReadSample: stream 3 reported error{suffix}"),
        r"ReadSample: stream 3 reported error in segment C:\seg_0.mp4"
    );
}

/// Opens `path` with every stream selected, the way `do_remux` does.
unsafe fn open_all_streams(path: &std::path::Path) -> (IMFSourceReader, StreamLayout) {
    unsafe {
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path), None).unwrap();
        reader.SetStreamSelection(ALL_STREAMS, true).unwrap();
        let layout = discover_stream_layout(&reader).unwrap();
        (reader, layout)
    }
}

#[test]
fn for_each_sample_delivers_every_stream_until_all_reach_end_of_stream() {
    let _mf = MfSession::start_mta().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = make_test_mp4(dir.path());
    unsafe {
        let (reader, layout) = open_all_streams(&path);
        let mut per_stream: HashMap<u32, usize> = HashMap::new();
        for_each_sample(&reader, 1 + layout.audio.len(), None, |stream, _s, _ts| {
            *per_stream.entry(stream).or_default() += 1;
            Ok(())
        })
        .unwrap();
        assert!(per_stream.get(&layout.video).copied().unwrap_or(0) >= 1);
        assert!(per_stream.get(&layout.audio[0]).copied().unwrap_or(0) >= 1);
        assert_eq!(per_stream.len(), 2, "only the two streams in the file");
    }
}

#[test]
fn for_each_sample_stops_at_the_first_callback_error() {
    let _mf = MfSession::start_mta().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = make_test_mp4(dir.path());
    unsafe {
        let (reader, layout) = open_all_streams(&path);
        let mut calls = 0;
        let result = for_each_sample(&reader, 1 + layout.audio.len(), None, |_, _, _| {
            calls += 1;
            Err(AppError::Encode("stop".into()))
        });
        assert!(matches!(result, Err(AppError::Encode(msg)) if msg == "stop"));
        assert_eq!(calls, 1);
    }
}

#[test]
fn split_export_groups_passes_singletons_through_and_merges_the_rest() {
    let groups = vec![vec![0], vec![1, 2], vec![], vec![3], vec![4, 5, 6]];
    let (passthrough, merge) = split_export_groups(&groups);
    assert_eq!(passthrough, vec![0, 3]);
    assert_eq!(merge, vec![&vec![1, 2], &vec![4, 5, 6]]);
}
