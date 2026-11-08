//! Finishing a fragmented recording in place -- a "soft remux", the
//! technique OBS ships as Hybrid MP4: a complete `moov` is written at the
//! file's end from the index of its samples, then the placeholder after
//! `ftyp` (`boxes::PLACEHOLDER`) becomes the header of one `mdat` holding
//! everything before that `moov`. No media is copied or moved.

use super::boxes::{RunSample, boxed, co64, ctts, stsc, stss, stsz, stts, with_duration};
use std::io;

/// Where every sample of a fragmented file is, and how long it lasts: what
/// a complete `moov` is built from. Kept by the muxer as it writes, or
/// found by walking the file's fragments; either way the same.
#[derive(Debug, Clone, PartialEq)]
pub struct Index {
    tracks: Vec<TrackTable>,
    /// Where the last whole fragment ends.
    end: u64,
}

/// One track's samples.
#[derive(Debug, Clone, PartialEq)]
struct TrackTable {
    id: u32,
    /// The first sample's decode time, in the track's ticks.
    first_decode_time: Option<u64>,
    /// Where the samples so far end, in the track's ticks.
    decode_end: u64,
    durations: Vec<u32>,
    sizes: Vec<u32>,
    /// The sync samples' numbers, from 1 -- kept only once a sample that
    /// isn't one has come: until then, every sample is (audio always is).
    sync: Vec<u32>,
    all_sync: bool,
    /// Each run's data: its offset in the file, and how many samples.
    chunks: Vec<(u64, u32)>,
}

impl TrackTable {
    fn new(id: u32) -> Self {
        Self {
            id,
            first_decode_time: None,
            decode_end: 0,
            durations: Vec::new(),
            sizes: Vec::new(),
            sync: Vec::new(),
            all_sync: true,
            chunks: Vec::new(),
        }
    }

    /// The sync samples' numbers, from 1.
    fn sync_samples(&self) -> Vec<u32> {
        if self.all_sync {
            (1..=self.durations.len() as u32).collect()
        } else {
            self.sync.clone()
        }
    }
}

impl Index {
    /// An empty index for the tracks `track_ids`, in the file's order.
    pub fn new(track_ids: &[u32]) -> Self {
        Self {
            tracks: track_ids.iter().map(|&id| TrackTable::new(id)).collect(),
            end: 0,
        }
    }

    /// Adds a fragment's run of `samples` of track `track_id`, whose data
    /// starts `data_at` bytes into the file and whose first sample is
    /// decoded at `base_decode_time` (the `tfdt`). Whether it was added:
    /// not for a track the index hasn't, nor an empty run.
    ///
    /// The `tfdt` wins: if the run doesn't start where the samples before
    /// it end, the last of those is lengthened (or shortened, down to
    /// nothing) to meet it, so the samples keep the times the fragments
    /// gave them.
    pub fn add_run(
        &mut self,
        track_id: u32,
        data_at: u64,
        base_decode_time: u64,
        samples: &[RunSample],
    ) -> bool {
        let Some(track) = self.tracks.iter_mut().find(|t| t.id == track_id) else {
            return false;
        };
        if samples.is_empty() {
            return false;
        }
        match (track.first_decode_time, track.durations.last_mut()) {
            (None, _) => {
                track.first_decode_time = Some(base_decode_time);
                track.decode_end = base_decode_time;
            }
            (Some(_), Some(last)) if base_decode_time != track.decode_end => {
                let old = *last;
                let fixed =
                    i128::from(old) + i128::from(base_decode_time) - i128::from(track.decode_end);
                let new = fixed.clamp(0, i128::from(u32::MAX)) as u32;
                *last = new;
                track.decode_end = track.decode_end - u64::from(old) + u64::from(new);
            }
            _ => {}
        }
        for sample in samples {
            track.durations.push(sample.duration);
            track.sizes.push(sample.size);
            track.decode_end += u64::from(sample.duration);
            let number = track.durations.len() as u32;
            if !sample.keyframe && track.all_sync {
                track.all_sync = false;
                track.sync = (1..number).collect();
            }
            if sample.keyframe && !track.all_sync {
                track.sync.push(number);
            }
        }
        track.chunks.push((data_at, samples.len() as u32));
        true
    }

    /// Notes where the last whole fragment ends.
    pub fn set_end(&mut self, end: u64) {
        self.end = end;
    }

    /// Where the last whole fragment ends: where the complete `moov` goes.
    pub fn end(&self) -> u64 {
        self.end
    }

    /// How many samples, in every track.
    pub fn sample_count(&self) -> usize {
        self.tracks.iter().map(|t| t.durations.len()).sum()
    }
}

/// The complete `moov` for the file whose stub `moov` (the whole box,
/// as `boxes::init_segment` wrote it) is `stub_moov` and whose samples
/// `index` holds: the stub's headers and sample descriptions, real
/// durations, filled sample tables, and no `mvex` -- the file isn't
/// fragmented any more.
pub fn complete_moov(stub_moov: &[u8], index: &Index) -> io::Result<Vec<u8>> {
    let [(kind, moov)] = parse(stub_moov)?[..] else {
        return Err(invalid("the stub isn't one box"));
    };
    if &kind != b"moov" {
        return Err(invalid("the stub isn't a moov"));
    }
    let parts = parse(body(moov))?;
    let mvhd = one(&parts, b"mvhd")?;
    let movie_timescale = u64::from(be32(mvhd, 20)?);
    let mut traks = Vec::new();
    let mut movie_duration = 0;
    for (_, trak) in parts.iter().filter(|(kind, _)| kind == b"trak") {
        let (built, duration) = complete_trak(trak, index, movie_timescale)?;
        traks.push(built);
        movie_duration = movie_duration.max(duration);
    }
    let mvhd = with_duration(mvhd, movie_duration);
    Ok(boxed(b"moov", |o| {
        o.bytes(&mvhd);
        for trak in &traks {
            o.bytes(trak);
        }
    }))
}

/// The complete `trak` for the stub's `trak`, and its duration in the
/// movie's timescale.
fn complete_trak(trak: &[u8], index: &Index, movie_timescale: u64) -> io::Result<(Vec<u8>, u64)> {
    let parts = parse(body(trak))?;
    let tkhd = one(&parts, b"tkhd")?;
    let id = be32(tkhd, 20)?;
    let table = index
        .tracks
        .iter()
        .find(|t| t.id == id)
        .ok_or_else(|| invalid(format!("track {id} isn't in the index")))?;
    let mdia = parse(body(one(&parts, b"mdia")?))?;
    let mdhd = one(&mdia, b"mdhd")?;
    let hdlr = one(&mdia, b"hdlr")?;
    let video = hdlr.get(16..20) == Some(b"vide");
    let minf = parse(body(one(&mdia, b"minf")?))?;
    let media_header = one(&minf, if video { b"vmhd" } else { b"smhd" })?;
    let dinf = one(&minf, b"dinf")?;
    let stsd = one(&parse(body(one(&minf, b"stbl")?))?, b"stsd")?;

    let timescale = u64::from(be32(mdhd, 20)?).max(1);
    // Media Foundation's way: decode times from 0, each sample put
    // `first` later by a `ctts`, and the media lasting to its last sample.
    let first = table.first_decode_time.unwrap_or(0);
    let media_duration = if table.durations.is_empty() {
        0
    } else {
        table.decode_end
    };
    let duration = (u128::from(media_duration) * u128::from(movie_timescale))
        .div_ceil(u128::from(timescale)) as u64;

    let chunk_counts: Vec<u32> = table.chunks.iter().map(|&(_, n)| n).collect();
    let chunk_offsets: Vec<u64> = table.chunks.iter().map(|&(at, _)| at).collect();
    let stbl = boxed(b"stbl", |o| {
        o.bytes(stsd).bytes(&stts(&table.durations));
        if first > 0 && !table.durations.is_empty() {
            // A start 2^32 ticks late (over 13 h) doesn't fit; it can't
            // happen in a recording.
            let offset = u32::try_from(first).unwrap_or(u32::MAX);
            o.bytes(&ctts(table.durations.len() as u32, offset));
        }
        o.bytes(&stsc(&chunk_counts))
            .bytes(&stsz(&table.sizes))
            .bytes(&co64(&chunk_offsets));
        if video {
            o.bytes(&stss(&table.sync_samples()));
        }
    });
    let mdhd = with_duration(mdhd, media_duration);
    let tkhd = with_duration(tkhd, duration);
    let built = boxed(b"trak", |o| {
        o.bytes(&tkhd);
        o.bytes(&boxed(b"mdia", |o| {
            o.bytes(&mdhd).bytes(hdlr);
            o.bytes(&boxed(b"minf", |o| {
                o.bytes(media_header).bytes(dinf).bytes(&stbl);
            }));
        }));
    });
    Ok((built, duration))
}

/// The boxes one after another in `data`, each whole: (type, bytes).
/// 32-bit sizes only: this reads the stub `moov`, PolyRec's own.
fn parse(data: &[u8]) -> io::Result<Vec<([u8; 4], &[u8])>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let size = be32(data, at)? as usize;
        if size < 8 || data.len() - at < size {
            return Err(invalid(format!("a box of {size} bytes at {at}")));
        }
        let mut kind = [0; 4];
        kind.copy_from_slice(&data[at + 4..at + 8]);
        out.push((kind, &data[at..at + size]));
        at += size;
    }
    Ok(out)
}

/// The body of the whole box `b`.
fn body(b: &[u8]) -> &[u8] {
    &b[8..]
}

/// The box of type `kind` among `boxes`.
fn one<'a>(boxes: &[([u8; 4], &'a [u8])], kind: &[u8; 4]) -> io::Result<&'a [u8]> {
    boxes
        .iter()
        .find(|(k, _)| k == kind)
        .map(|&(_, b)| b)
        .ok_or_else(|| invalid(format!("no {}", String::from_utf8_lossy(kind))))
}

fn be32(b: &[u8], at: usize) -> io::Result<u32> {
    b.get(at..at + 4)
        .map(|v| u32::from_be_bytes(v.try_into().unwrap()))
        .ok_or_else(|| invalid("a box too short"))
}

fn invalid(what: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.into())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::boxes::tests::{be32, be64, boxes, children, entries, find, kinds, whole};
    use super::super::boxes::{RunSample, TrackInfo, TrackKind, init_segment};
    use super::super::nal::VideoCodec;
    use super::*;

    /// The whole `moov` box of an init segment.
    pub(crate) fn moov_in(head: &[u8]) -> &[u8] {
        let top = boxes(head);
        whole(head, find(&top, "moov"))
    }

    /// The whole boxes of `stbl` of the `n`th `trak` of `moov`, by kind.
    fn stbl(moov: &[u8], n: usize) -> Vec<(String, Vec<u8>)> {
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

    fn table<'a>(stbl: &'a [(String, Vec<u8>)], kind: &str) -> &'a [u8] {
        &stbl
            .iter()
            .find(|(k, _)| k == kind)
            .unwrap_or_else(|| panic!("no {kind}"))
            .1
    }

    /// The whole header box `kind` (`mvhd`, or the `n`th track's `tkhd` or
    /// `mdhd`) of `moov`.
    fn header(moov: &[u8], kind: &str, n: usize) -> Vec<u8> {
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

    fn complete(index: &Index) -> Vec<u8> {
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
        let moov = complete(&index);

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
        let moov = complete(&index);
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
        let moov = complete(&index);
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
        let moov = complete(&index);
        assert_eq!(durations(table(&stbl(&moov, 1), "stts")), [1024, 0, 1024]);
    }

    #[test]
    fn offsets_past_4_gib_survive() {
        let mut index = Index::new(&[1, 2]);
        assert!(index.add_run(1, 5 << 30, 0, &[sample(3000, 1, true)]));
        let moov = complete(&index);
        assert_eq!(offsets(table(&stbl(&moov, 0), "co64")), [5 << 30]);
    }

    /// Past about 13.25 h at 90 kHz, a track's duration takes 64 bits.
    #[test]
    fn a_long_track_gets_version_1_headers() {
        let mut index = Index::new(&[1, 2]);
        let long = u32::MAX - 10;
        assert!(index.add_run(1, 0, 0, &[sample(long, 1, true)]));
        assert!(index.add_run(1, 1, u64::from(long), &[sample(3000, 1, false)]));
        let moov = complete(&index);
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
        let moov = complete(&index);
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
}
