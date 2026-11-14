use super::super::boxes::tests::{be32, be64, boxes, children, entries, find, kinds, whole};
use super::super::boxes::{RunSample, TrackInfo, TrackKind, init_segment};
use super::super::muxer::{Muxed, Muxer, Sample};
use super::super::nal::VideoCodec;
use super::*;

/// The whole `moov` box of an init segment.
pub(crate) fn moov_in(head: &[u8]) -> &[u8] {
    let top = boxes(head);
    whole(head, find(&top, "moov"))
}

/// The whole boxes of `stbl` of the `n`th `trak` of `moov`, by kind.
pub(crate) fn stbl(moov: &[u8], n: usize) -> Vec<(String, Vec<u8>)> {
    let top = boxes(moov);
    let traks: Vec<_> = children(&top[0], 0)
        .into_iter()
        .filter(|b| b.kind == "trak")
        .collect();
    let trak = children(&traks[n], 0);
    let mdia = children(find(&trak, "mdia"), 0);
    let minf = children(find(&mdia, "minf"), 0);
    children(find(&minf, "stbl"), 0)
        .iter()
        .map(|b| (b.kind.clone(), whole(moov, b).to_vec()))
        .collect()
}

pub(crate) fn table<'a>(stbl: &'a [(String, Vec<u8>)], kind: &str) -> &'a [u8] {
    &stbl
        .iter()
        .find(|(k, _)| k == kind)
        .unwrap_or_else(|| panic!("no {kind}"))
        .1
}

/// The whole header box `kind` (`mvhd`, or the `n`th track's `tkhd` or
/// `mdhd`) of `moov`.
pub(crate) fn header(moov: &[u8], kind: &str, n: usize) -> Vec<u8> {
    let top = boxes(moov);
    let parts = children(&top[0], 0);
    if kind == "mvhd" {
        return whole(moov, find(&parts, "mvhd")).to_vec();
    }
    let traks: Vec<_> = parts.iter().filter(|b| b.kind == "trak").collect();
    let trak = children(traks[n], 0);
    if kind == "tkhd" {
        return whole(moov, find(&trak, "tkhd")).to_vec();
    }
    let mdia = children(find(&trak, "mdia"), 0);
    whole(moov, find(&mdia, kind)).to_vec()
}

/// The duration a whole `mvhd`, `tkhd` or `mdhd` holds, either version.
fn duration(header: &[u8]) -> u64 {
    let tkhd = &header[4..8] == b"tkhd";
    match (header[8], tkhd) {
        (0, false) => u64::from(be32(header, 24)),
        (0, true) => u64::from(be32(header, 28)),
        (_, false) => be64(header, 32),
        (_, true) => be64(header, 36),
    }
}

fn durations(stts: &[u8]) -> Vec<u32> {
    entries(stts, 2)
        .into_iter()
        .flat_map(|e| std::iter::repeat_n(e[1], e[0] as usize))
        .collect()
}

fn sizes(stsz: &[u8]) -> Vec<u32> {
    assert_eq!(be32(stsz, 12), 0, "sizes in a table");
    let count = be32(stsz, 16) as usize;
    (0..count).map(|i| be32(stsz, 20 + i * 4)).collect()
}

fn offsets(co64: &[u8]) -> Vec<u64> {
    let count = be32(co64, 12) as usize;
    (0..count).map(|i| be64(co64, 16 + i * 8)).collect()
}

/// Per track of `moov`, each sample's bytes in `file`, found the way a
/// player finds them: chunks from `co64`, samples per chunk from
/// `stsc`, sizes from `stsz`.
pub(crate) fn sample_bytes(file: &[u8], moov: &[u8]) -> Vec<Vec<Vec<u8>>> {
    let traks = children(&boxes(moov)[0], 0)
        .iter()
        .filter(|b| b.kind == "trak")
        .count();
    (0..traks)
        .map(|n| {
            let stbl = stbl(moov, n);
            let sizes = sizes(table(&stbl, "stsz"));
            let chunks = offsets(table(&stbl, "co64"));
            let stsc = entries(table(&stbl, "stsc"), 3);
            let mut sizes = sizes.into_iter();
            let mut out = Vec::new();
            for (i, &offset) in chunks.iter().enumerate() {
                let chunk = i as u32 + 1;
                let per_chunk = stsc
                    .iter()
                    .rev()
                    .find(|e| e[0] <= chunk)
                    .expect("an stsc entry")[1];
                let mut at = offset as usize;
                for _ in 0..per_chunk {
                    let size = sizes.next().expect("a size per sample") as usize;
                    out.push(file[at..at + size].to_vec());
                    at += size;
                }
            }
            assert!(sizes.next().is_none(), "every sample in a chunk");
            out
        })
        .collect()
}

fn tracks() -> Vec<TrackInfo> {
    vec![
        TrackInfo {
            id: 1,
            timescale: 90_000,
            kind: TrackKind::Video {
                codec: VideoCodec::H264,
                width: 64,
                height: 64,
                config: vec![1],
            },
        },
        TrackInfo {
            id: 2,
            timescale: 48_000,
            kind: TrackKind::Audio {
                channels: 2,
                sample_rate: 48_000,
                audio_specific_config: vec![0x11, 0x90],
            },
        },
    ]
}

fn sample(duration: u32, size: u32, keyframe: bool) -> RunSample {
    RunSample {
        duration,
        size,
        keyframe,
    }
}

fn moov_for(index: &Index) -> Vec<u8> {
    let head = init_segment(&tracks());
    complete_moov(moov_in(&head), index).expect("complete_moov")
}

#[test]
fn tables_follow_the_runs() {
    let mut index = Index::new(&[1, 2]);
    let a = [
        sample(3000, 10, true),
        sample(3000, 20, false),
        sample(3000, 30, false),
    ];
    assert!(index.add_run(1, 100, 0, &a));
    assert!(index.add_run(
        1,
        500,
        9000,
        &[sample(3000, 5, true), sample(3000, 6, false)]
    ));
    let moov = moov_for(&index);

    let top = boxes(&moov);
    assert_eq!(kinds(&top), ["moov"]);
    assert_eq!(
        kinds(&children(&top[0], 0)),
        ["mvhd", "trak", "trak"],
        "no mvex: not fragmented any more"
    );
    let video = stbl(&moov, 0);
    let video_kinds: Vec<&str> = video.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        video_kinds,
        ["stsd", "stts", "stsc", "stsz", "co64", "stss"]
    );
    assert_eq!(entries(table(&video, "stts"), 2), [vec![5, 3000]]);
    assert_eq!(sizes(table(&video, "stsz")), [10, 20, 30, 5, 6]);
    assert_eq!(
        entries(table(&video, "stsc"), 3),
        [vec![1, 3, 1], vec![2, 2, 1]]
    );
    assert_eq!(offsets(table(&video, "co64")), [100, 500]);
    assert_eq!(entries(table(&video, "stss"), 1), [vec![1], vec![4]]);

    assert_eq!(duration(&header(&moov, "mdhd", 0)), 15_000);
    // 15000 / 90 kHz in the movie's 1 kHz, rounded up.
    assert_eq!(duration(&header(&moov, "tkhd", 0)), 167);
    assert_eq!(duration(&header(&moov, "mvhd", 0)), 167);

    let head = init_segment(&tracks());
    let stub = stbl(moov_in(&head), 0);
    assert_eq!(
        table(&video, "stsd"),
        table(&stub, "stsd"),
        "stsd as it was"
    );
}

/// As Media Foundation's MP4 sink writes it: no edit list, every sample
/// offset by when the track starts.
#[test]
fn a_track_that_starts_late_is_offset_by_ctts() {
    let mut index = Index::new(&[1, 2]);
    assert!(index.add_run(2, 200, 1182, &[sample(1024, 4, true); 3]));
    let moov = moov_for(&index);
    let audio = stbl(&moov, 1);
    let audio_kinds: Vec<&str> = audio.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        audio_kinds,
        ["stsd", "stts", "ctts", "stsc", "stsz", "co64"]
    );
    assert_eq!(entries(table(&audio, "ctts"), 2), [vec![3, 1182]]);
    assert_eq!(duration(&header(&moov, "mdhd", 1)), 1182 + 3072);
    // 4254 / 48 kHz in 1 kHz, rounded up.
    assert_eq!(duration(&header(&moov, "mvhd", 0)), 89);
}

/// The fragments' times stand: a fragment starting later than the
/// samples before it add up to lengthens the last of them.
#[test]
fn a_gap_between_fragments_goes_into_the_sample_before_it() {
    let mut index = Index::new(&[1, 2]);
    assert!(index.add_run(2, 0, 0, &[sample(1024, 1, true); 2]));
    assert!(index.add_run(2, 10, 2050, &[sample(1024, 1, true)]));
    let moov = moov_for(&index);
    let audio = stbl(&moov, 1);
    assert_eq!(
        entries(table(&audio, "stts"), 2),
        [vec![1, 1024], vec![1, 1026], vec![1, 1024]]
    );
    assert!(!audio.iter().any(|(k, _)| k == "ctts"));
    assert_eq!(duration(&header(&moov, "mdhd", 1)), 2050 + 1024);
}

#[test]
fn a_fragment_starting_before_the_last_one_ended_clamps_not_wraps() {
    let mut index = Index::new(&[1, 2]);
    assert!(index.add_run(2, 0, 0, &[sample(1024, 1, true), sample(10, 1, true)]));
    assert!(index.add_run(2, 10, 1000, &[sample(1024, 1, true)]));
    let moov = moov_for(&index);
    assert_eq!(durations(table(&stbl(&moov, 1), "stts")), [1024, 0, 1024]);
}

#[test]
fn offsets_past_4_gib_survive() {
    let mut index = Index::new(&[1, 2]);
    assert!(index.add_run(1, 5 << 30, 0, &[sample(3000, 1, true)]));
    let moov = moov_for(&index);
    assert_eq!(offsets(table(&stbl(&moov, 0), "co64")), [5 << 30]);
}

/// Past about 13.25 h at 90 kHz, a track's duration takes 64 bits.
#[test]
fn a_long_track_gets_version_1_headers() {
    let mut index = Index::new(&[1, 2]);
    let long = u32::MAX - 10;
    assert!(index.add_run(1, 0, 0, &[sample(long, 1, true)]));
    assert!(index.add_run(1, 1, u64::from(long), &[sample(3000, 1, false)]));
    let moov = moov_for(&index);
    let mdhd = header(&moov, "mdhd", 0);
    assert_eq!(mdhd[8], 1, "version 1");
    assert_eq!(duration(&mdhd), u64::from(long) + 3000);
    assert_eq!(be32(&mdhd, 28), 90_000, "timescale kept");
    let tkhd = header(&moov, "tkhd", 0);
    assert_eq!(duration(&tkhd), (u64::from(long) + 3000).div_ceil(90));
}

/// A track that never got a sample (a device that gave nothing) stays
/// in the file, empty, so the track count doesn't change.
#[test]
fn a_track_with_no_samples_keeps_an_empty_trak() {
    let mut index = Index::new(&[1, 2]);
    assert!(index.add_run(1, 100, 0, &[sample(3000, 1, true)]));
    let moov = moov_for(&index);
    let top = boxes(&moov);
    assert_eq!(kinds(&children(&top[0], 0)), ["mvhd", "trak", "trak"]);
    let audio = stbl(&moov, 1);
    assert!(entries(table(&audio, "stts"), 2).is_empty());
    assert!(entries(table(&audio, "stsc"), 3).is_empty());
    assert!(sizes(table(&audio, "stsz")).is_empty());
    assert!(offsets(table(&audio, "co64")).is_empty());
    assert_eq!(duration(&header(&moov, "mdhd", 1)), 0);
    assert_eq!(duration(&header(&moov, "tkhd", 1)), 0);
}

/// Media Foundation lists a file's streams last `trak` first, and its
/// remux -- what finished recordings before -- wrote the audio in the
/// order it read it: reversed. Finished in place, the audio goes in
/// that same order and numbering, so every player (and Export, which
/// reads through Media Foundation) shows the tracks as before.
#[test]
fn audio_tracks_go_in_the_order_the_remux_wrote_them() {
    let head = init_segment(&three_tracks());
    let mut index = Index::new(&[1, 2, 3]);
    assert!(index.add_run(2, 0, 0, &[sample(1024, 22, true)]));
    assert!(index.add_run(3, 0, 0, &[sample(1024, 33, true)]));
    let moov = complete_moov(moov_in(&head), &index).unwrap();
    let ids: Vec<u32> = (0..3)
        .map(|n| be32(&header(&moov, "tkhd", n), 20))
        .collect();
    assert_eq!(ids, [1, 2, 3], "numbered in their new order");
    assert!(
        sizes(table(&stbl(&moov, 0), "stsz")).is_empty(),
        "video first"
    );
    assert_eq!(
        sizes(table(&stbl(&moov, 1), "stsz")),
        [33],
        "the last audio track"
    );
    assert_eq!(sizes(table(&stbl(&moov, 2), "stsz")), [22]);
}

#[test]
fn add_run_ignores_an_unknown_track_or_an_empty_run() {
    let mut index = Index::new(&[1, 2]);
    assert!(!index.add_run(3, 0, 0, &[sample(1, 1, true)]));
    assert!(!index.add_run(1, 0, 0, &[]));
    assert_eq!(index, Index::new(&[1, 2]));
    assert_eq!(index.sample_count(), 0);
    index.set_end(1234);
    assert_eq!(index.end(), 1234);
}

#[test]
fn a_stub_that_isnt_a_moov_is_refused() {
    let index = Index::new(&[1, 2]);
    let head = init_segment(&tracks());
    assert!(complete_moov(&head[..32], &index).is_err(), "ftyp");
    assert!(complete_moov(&[0, 0, 0, 9, b'm', b'o', b'o', b'v'], &index).is_err());
    assert!(
        complete_moov(moov_in(&head), &Index::new(&[1])).is_err(),
        "a trak the index hasn't"
    );
}

/// Recovery reads the stub from the file: a damaged one is refused, not
/// a panic -- which would stop the other leftovers being recovered, at
/// every start.
#[test]
fn a_damaged_stub_is_refused_not_a_panic() {
    let index = Index::new(&[1, 2]);
    // An mvhd cut short just after its timescale.
    let mut mvhd = vec![0, 0, 0, 24, b'm', b'v', b'h', b'd'];
    mvhd.extend([0; 12]);
    mvhd.extend(1000u32.to_be_bytes());
    let mut moov = (8 + mvhd.len() as u32).to_be_bytes().to_vec();
    moov.extend(b"moov");
    moov.extend(&mvhd);
    assert!(complete_moov(&moov, &index).is_err(), "a short mvhd");

    // A tkhd that says version 1, laid out as version 0.
    let head = init_segment(&tracks());
    let mut stub = moov_in(&head).to_vec();
    let tkhd = stub.windows(4).position(|w| w == b"tkhd").unwrap();
    stub[tkhd + 4] = 1;
    assert!(
        complete_moov(&stub, &index).is_err(),
        "a tkhd of another version"
    );
}

// -- Finishing a file in place --

const MS: i64 = 10_000;

/// Video and two audio tracks.
fn three_tracks() -> Vec<TrackInfo> {
    let mut tracks = tracks();
    tracks.push(TrackInfo {
        id: 3,
        timescale: 48_000,
        kind: TrackKind::Audio {
            channels: 1,
            sample_rate: 48_000,
            audio_specific_config: vec![0x11, 0x88],
        },
    });
    tracks
}

/// What the muxer writes for 600 ms of `tracks` -- three fragments --
/// feeding the audio tracks in `fed` (by index) and not the others;
/// every sample with bytes of its own. With what was pushed, per track.
fn muxed_with(tracks: Vec<TrackInfo>, fed: &[usize]) -> (Muxed<Vec<u8>>, Vec<Vec<Vec<u8>>>) {
    let mut pushed = vec![Vec::new(); tracks.len()];
    let mut muxer = Muxer::new(Vec::new(), tracks).unwrap();
    let mut n = 0u8;
    for (i, ms) in (0..600).step_by(100).enumerate() {
        n = n.wrapping_add(1);
        let data = vec![n; 3 + usize::from(n % 7)];
        pushed[0].push(data.clone());
        let frame = Sample {
            time: ms * MS,
            duration: 100 * MS,
            keyframe: i % 2 == 0,
            data,
        };
        muxer.push(0, frame).unwrap();
        for &track in fed {
            for half in [0, 50] {
                n = n.wrapping_add(1);
                let data = vec![n; 2 + usize::from(n % 5)];
                pushed[track].push(data.clone());
                let sound = Sample {
                    time: (ms + half) * MS,
                    duration: 50 * MS,
                    keyframe: true,
                    data,
                };
                muxer.push(track, sound).unwrap();
            }
        }
    }
    (muxer.finish().unwrap(), pushed)
}

/// What was pushed per track, in the order a finished file holds the
/// tracks: video, then the audio last first.
fn in_file_order(mut pushed: Vec<Vec<Vec<u8>>>) -> Vec<Vec<Vec<u8>>> {
    pushed[1..].reverse();
    pushed
}

struct Case {
    dir: tempfile::TempDir,
    muxed: Muxed<Vec<u8>>,
    /// In file order (`in_file_order`).
    pushed: Vec<Vec<Vec<u8>>>,
    /// The recording finished from the muxer's own index.
    finished: Vec<u8>,
}

impl Case {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (muxed, pushed) = muxed_with(three_tracks(), &[1, 2]);
        let reference = dir.path().join("reference.mp4");
        std::fs::write(&reference, &muxed.out).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&reference)
            .unwrap();
        complete(&mut file, &muxed.head, &muxed.index).expect("complete");
        drop(file);
        let finished = std::fs::read(&reference).unwrap();
        Self {
            dir,
            muxed,
            pushed: in_file_order(pushed),
            finished,
        }
    }

    /// A recording's temporary file holding `bytes`.
    fn file(&self, bytes: &[u8]) -> std::path::PathBuf {
        let path = self.dir.path().join("app_recording_1.tmp.mp4");
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// The complete `moov` the finished file ends with.
    fn moov(&self) -> &[u8] {
        let top = boxes(&self.finished);
        let at = find(&top, "moov").at;
        &self.finished[at..]
    }
}

#[test]
fn the_walk_finds_the_index_the_muxer_kept() {
    let case = Case::new();
    let path = case.file(&case.muxed.out);
    let scan = scan(&mut std::fs::File::open(&path).unwrap()).expect("scan");
    assert!(!scan.damaged);
    assert_eq!(scan.head, case.muxed.head);
    assert_eq!(scan.index, case.muxed.index);
}

/// At Stop the sink finishes from the muxer's index; recovery, from the
/// walk. Both make the same file.
#[test]
fn finishing_from_the_walk_or_the_muxer_makes_the_same_file() {
    let case = Case::new();
    let path = case.file(&case.muxed.out);
    assert_eq!(finish_in_place(&path), InPlace::Finished);
    assert_eq!(std::fs::read(&path).unwrap(), case.finished);
}

#[test]
fn a_finished_file_is_ftyp_mdat_moov_and_its_tables_point_at_the_samples() {
    let case = Case::new();
    let file = &case.finished;
    let top = boxes(file);
    assert_eq!(kinds(&top), ["ftyp", "mdat", "moov"]);
    assert_eq!(
        file[top[1].at..top[1].at + 8],
        [0, 0, 0, 1, b'm', b'd', b'a', b't'],
        "a 64-bit mdat header"
    );
    assert_eq!(top[1].at, 32, "where the placeholder was");
    assert_eq!(top[1].at + 16 + top[1].body.len(), top[2].at);
    assert_eq!(sample_bytes(file, case.moov()), case.pushed);
    assert_eq!(
        file.len(),
        case.muxed.out.len() + case.moov().len(),
        "nothing copied: the file grows by its moov only"
    );
}

/// A crash or power cut partway through writing a fragment.
#[test]
fn a_cut_off_last_fragment_is_dropped() {
    let case = Case::new();
    let top = boxes(&case.muxed.out);
    let last_mdat = top.iter().rfind(|b| b.kind == "mdat").unwrap();
    let cut = last_mdat.at + 8 + last_mdat.body.len() / 2;
    let path = case.file(&case.muxed.out[..cut]);

    assert_eq!(finish_in_place(&path), InPlace::Finished);
    let file = std::fs::read(&path).unwrap();
    let top = boxes(&file);
    assert_eq!(kinds(&top), ["ftyp", "mdat", "moov"]);
    // The last fragment held 2 frames and 4 samples of each audio track.
    let expected: Vec<Vec<Vec<u8>>> = case
        .pushed
        .iter()
        .zip([2, 4, 4])
        .map(|(samples, lost)| samples[..samples.len() - lost].to_vec())
        .collect();
    assert_eq!(sample_bytes(&file, whole(&file, &top[2])), expected);
}

/// Cut off after the moov was appended, before the header went in.
#[test]
fn a_trailing_moov_from_a_cut_off_finish_is_replaced() {
    let case = Case::new();
    let path = case.file(&[case.muxed.out.as_slice(), case.moov()].concat());
    assert_eq!(finish_in_place(&path), InPlace::Finished);
    assert_eq!(std::fs::read(&path).unwrap(), case.finished);
}

#[test]
fn a_trailing_moov_cut_off_partway_is_replaced() {
    let case = Case::new();
    let half = &case.moov()[..case.moov().len() / 2];
    let path = case.file(&[case.muxed.out.as_slice(), half].concat());
    assert_eq!(finish_in_place(&path), InPlace::Finished);
    assert_eq!(std::fs::read(&path).unwrap(), case.finished);
}

/// A power cut can leave a file longer than what reached the disk,
/// the rest zeros.
#[test]
fn a_zero_filled_tail_counts_as_cut_off() {
    let case = Case::new();
    let path = case.file(&[case.muxed.out.as_slice(), &[0; 4096]].concat());
    assert_eq!(finish_in_place(&path), InPlace::Finished);
    assert_eq!(std::fs::read(&path).unwrap(), case.finished);
}

/// Cut off after the header went in, before the rename.
#[test]
fn a_file_finished_but_not_renamed_is_left_as_it_is() {
    let case = Case::new();
    let path = case.file(&case.finished);
    assert_eq!(finish_in_place(&path), InPlace::Finished);
    assert_eq!(std::fs::read(&path).unwrap(), case.finished);
}

/// Written before soft remux: no placeholder. It's left alone.
#[test]
fn a_file_without_the_placeholder_is_not_ours() {
    let case = Case::new();
    let legacy = [&case.muxed.out[..32], &case.muxed.out[48..]].concat();
    let path = case.file(&legacy);
    assert_eq!(finish_in_place(&path), InPlace::NotOurs);
    assert_eq!(std::fs::read(&path).unwrap(), legacy);
}

/// Something wrong between whole fragments isn't a cut-off end:
/// truncating there would throw away every fragment after it.
#[test]
fn damage_in_the_middle_is_never_truncated() {
    let case = Case::new();
    let mut damaged = case.muxed.out.clone();
    let second_moof = boxes(&damaged)
        .iter()
        .filter(|b| b.kind == "moof")
        .nth(1)
        .unwrap()
        .at;
    damaged[second_moof + 4..second_moof + 8].copy_from_slice(b"junk");
    let path = case.file(&damaged);
    assert_eq!(finish_in_place(&path), InPlace::KeptFragmented);
    assert_eq!(std::fs::read(&path).unwrap(), damaged);
}

#[test]
fn a_file_with_no_whole_fragment_is_kept_fragmented() {
    let case = Case::new();
    let bytes = &case.muxed.out[..case.muxed.head.len() + 20];
    let path = case.file(bytes);
    assert_eq!(finish_in_place(&path), InPlace::KeptFragmented);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn a_track_that_never_got_a_sample_still_finishes() {
    let case = Case::new();
    let (muxed, pushed) = muxed_with(three_tracks(), &[1]);
    let path = case.file(&muxed.out);
    assert_eq!(finish_in_place(&path), InPlace::Finished);
    let file = std::fs::read(&path).unwrap();
    let top = boxes(&file);
    let moov = whole(&file, &top[2]);
    let traks = children(&boxes(moov)[0], 0)
        .iter()
        .filter(|b| b.kind == "trak")
        .count();
    assert_eq!(traks, 3, "the empty track stays");
    assert_eq!(sample_bytes(&file, moov), in_file_order(pushed));
}

#[test]
fn complete_refuses_a_head_without_the_placeholder() {
    let case = Case::new();
    let legacy = [&case.muxed.out[..32], &case.muxed.out[48..]].concat();
    let path = case.file(&legacy);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let head = [&case.muxed.head[..32], &case.muxed.head[48..]].concat();
    assert!(complete(&mut file, &head, &case.muxed.index).is_err());
    drop(file);
    assert_eq!(std::fs::read(&path).unwrap(), legacy);
}

/// A file in memory that fails the writes and syncs it's told to, by
/// number (from 1), and does the rest.
struct FailingDisk {
    data: Vec<u8>,
    at: u64,
    writes: usize,
    syncs: usize,
    fail_writes: Vec<usize>,
    fail_syncs: Vec<usize>,
}

impl FailingDisk {
    fn new(data: &[u8], fail_writes: &[usize], fail_syncs: &[usize]) -> Self {
        Self {
            data: data.to_vec(),
            at: 0,
            writes: 0,
            syncs: 0,
            fail_writes: fail_writes.to_vec(),
            fail_syncs: fail_syncs.to_vec(),
        }
    }
}

impl Write for FailingDisk {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        if self.fail_writes.contains(&self.writes) {
            return Err(io::Error::other("write failed"));
        }
        let at = self.at as usize;
        if self.data.len() < at + buf.len() {
            self.data.resize(at + buf.len(), 0);
        }
        self.data[at..at + buf.len()].copy_from_slice(buf);
        self.at += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for FailingDisk {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.at = match to {
            SeekFrom::Start(at) => at,
            SeekFrom::End(by) => self.data.len() as u64 + by as u64,
            SeekFrom::Current(by) => self.at + by as u64,
        };
        Ok(self.at)
    }
}

impl Disk for FailingDisk {
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.data.resize(len as usize, 0);
        Ok(())
    }

    fn sync_all(&mut self) -> io::Result<()> {
        self.syncs += 1;
        if self.fail_syncs.contains(&self.syncs) {
            return Err(io::Error::other("sync failed"));
        }
        Ok(())
    }
}

/// Writing the index failed: the file is the fragmented one it was.
#[test]
fn a_finish_failing_to_write_its_index_puts_the_fragmented_file_back() {
    let case = Case::new();
    let mut disk = FailingDisk::new(&case.muxed.out, &[1], &[]);
    assert!(complete(&mut disk, &case.muxed.head, &case.muxed.index).is_err());
    assert!(disk.data == case.muxed.out);
}

/// The header went in, but getting it onto the disk failed: the file is
/// put back as the fragmented one it was.
#[test]
fn a_finish_that_fails_after_its_header_puts_the_fragmented_file_back() {
    let case = Case::new();
    let mut disk = FailingDisk::new(&case.muxed.out, &[], &[2]);
    assert!(complete(&mut disk, &case.muxed.head, &case.muxed.index).is_err());
    assert!(disk.data == case.muxed.out);
}

/// The header went in and putting the placeholder back failed too: the
/// index stays, so the file is a whole, finished one -- never a header
/// with nothing after the data it holds, which nothing could read.
#[test]
fn a_finish_whose_undoing_fails_is_left_whole() {
    let case = Case::new();
    // Writes: the moov, the header, then the placeholder back.
    let mut disk = FailingDisk::new(&case.muxed.out, &[3], &[2]);
    assert!(complete(&mut disk, &case.muxed.head, &case.muxed.index).is_err());
    assert!(disk.data == case.finished);
}

/// The longest recording here -- 3.3 h of 60 fps video and three AAC
/// tracks, 2.4 million samples -- in 1-byte samples, finished in place
/// in seconds whichever way the index comes: from the muxer at Stop, or
/// walked from the file at the next start. (The remux took 376 s.)
///
/// The limit is wide on purpose: most of the time is the flushes to
/// disk, and a CI runner's disk took 2.1 s where this machine takes a
/// fraction of a second. What it has to catch is work that grows with
/// the recording faster than its length, which takes minutes.
#[test]
fn three_hours_finish_in_seconds() {
    const LIMIT: Duration = Duration::from_secs(20);
    use std::io::BufWriter;
    use std::time::{Duration, Instant};
    const FRAMES: u64 = 712_800;
    const HNS: u64 = 10_000_000;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("long.tmp.mp4");
    let mut tracks = three_tracks();
    tracks.push(TrackInfo {
        id: 4,
        ..tracks[1].clone()
    });

    let generating = Instant::now();
    let file = File::create(&path).unwrap();
    let mut muxer = Muxer::new(BufWriter::with_capacity(1 << 20, file), tracks).unwrap();
    let mut audio = 0u64;
    for frame in 0..FRAMES {
        let frame_sample = Sample {
            time: (frame * HNS / 60) as i64,
            duration: (HNS / 60) as i64,
            keyframe: frame % 120 == 0,
            data: vec![1],
        };
        muxer.push(0, frame_sample).unwrap();
        let frame_end = ((frame + 1) * HNS / 60) as i64;
        loop {
            let at = (audio * 1024 * HNS / 48_000) as i64;
            if at >= frame_end {
                break;
            }
            for track in 1..=3 {
                let sound = Sample {
                    time: at,
                    duration: (1024 * HNS / 48_000) as i64,
                    keyframe: true,
                    data: vec![2],
                };
                muxer.push(track, sound).unwrap();
            }
            audio += 1;
        }
    }
    let muxed = muxer.finish().unwrap();
    let mut file = muxed.out.into_inner().unwrap();
    // The next start's case: the same file, never finished.
    let copy = dir.path().join("copy.tmp.mp4");
    std::fs::copy(&path, &copy).unwrap();
    let generating = generating.elapsed();

    let started = Instant::now();
    complete(&mut file, &muxed.head, &muxed.index).expect("complete");
    let at_stop = started.elapsed();
    drop(file);
    let started = Instant::now();
    assert_eq!(finish_in_place(&copy), InPlace::Finished);
    let at_start = started.elapsed();

    let finished = std::fs::read(&path).unwrap();
    let moov = finished.len() as u64 - muxed.index.end();
    println!(
        "3.3 h: {} samples ({FRAMES} frames, {audio} per audio track), a {} MB index in a {} MB file; finished at Stop in {at_stop:?}, at the next start in {at_start:?} (made in {generating:?})",
        muxed.index.sample_count(),
        moov >> 20,
        finished.len() >> 20
    );
    assert_eq!(audio, 556_875, "3.3 h of AAC frames");
    assert_eq!(muxed.index.sample_count() as u64, FRAMES + 3 * audio);
    assert!(at_stop < LIMIT, "at Stop: {at_stop:?}");
    assert!(at_start < LIMIT, "at the next start: {at_start:?}");
    assert!(
        finished == std::fs::read(&copy).unwrap(),
        "the same file either way"
    );
}
