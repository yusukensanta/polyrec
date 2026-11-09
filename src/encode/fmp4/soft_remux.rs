//! Finishing a fragmented recording in place -- a "soft remux", the
//! technique OBS ships as Hybrid MP4: a complete `moov` is written at the
//! file's end from the index of its samples, then the placeholder after
//! `ftyp` (`boxes::PLACEHOLDER`) becomes the header of one `mdat` holding
//! everything before that `moov`. No media is copied or moved.

use super::boxes::{
    PLACEHOLDER, RunSample, boxed, co64, ctts, mdat_header, stsc, stss, stsz, stts, with_duration,
};
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Instant;

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
///
/// The tracks go in the order Media Foundation's remux, which finished
/// recordings before, put them in: video, then the audio last first. (Media
/// Foundation lists a file's streams last `trak` first, and the remux
/// wrote them in the order it read them.) They're numbered in that order,
/// as it numbered them. So every player shows a recording's tracks in the
/// order it always did, and Export -- which reads through Media Foundation
/// -- in the order they were recorded.
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
    let mut video = Vec::new();
    let mut audio = Vec::new();
    for (_, trak) in parts.iter().filter(|(kind, _)| kind == b"trak") {
        if handler(trak)? == *b"vide" {
            video.push(*trak);
        } else {
            audio.push(*trak);
        }
    }
    let mut traks = Vec::new();
    let mut movie_duration = 0;
    for (position, trak) in video.into_iter().chain(audio.into_iter().rev()).enumerate() {
        let new_id = position as u32 + 1;
        let (built, duration) = complete_trak(trak, index, movie_timescale, new_id)?;
        traks.push(built);
        movie_duration = movie_duration.max(duration);
    }
    let mvhd = with_duration(mvhd, movie_duration)?;
    Ok(boxed(b"moov", |o| {
        o.bytes(&mvhd);
        for trak in &traks {
            o.bytes(trak);
        }
    }))
}

/// The handler type of the `trak` (whole): `vide` or `soun`.
fn handler(trak: &[u8]) -> io::Result<[u8; 4]> {
    let mdia = parse(body(one(&parse(body(trak))?, b"mdia")?))?;
    let hdlr = one(&mdia, b"hdlr")?;
    hdlr.get(16..20)
        .map(|h| h.try_into().unwrap())
        .ok_or_else(|| invalid("an hdlr too short"))
}

/// The complete `trak` for the stub's `trak`, as track `new_id`, and its
/// duration in the movie's timescale.
fn complete_trak(
    trak: &[u8],
    index: &Index,
    movie_timescale: u64,
    new_id: u32,
) -> io::Result<(Vec<u8>, u64)> {
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
    let mdhd = with_duration(mdhd, media_duration)?;
    let mut tkhd = with_duration(tkhd, duration)?;
    // track_ID: after the 32- or 64-bit creation and modification times.
    let id_at = if tkhd[8] == 0 { 20 } else { 28 };
    tkhd[id_at..id_at + 4].copy_from_slice(&new_id.to_be_bytes());
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

/// What finishing a recording's file in place came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InPlace {
    /// A plain MP4 now -- or already, its finish cut off just before the
    /// rename -- under the same name.
    Finished,
    /// Written with the placeholder but not finished: it holds no whole
    /// fragment, it's damaged between two, or it couldn't be written to.
    /// Still a fragmented MP4, which reads as it is.
    KeptFragmented,
    /// No placeholder: written before recordings were finished in place
    /// (or not a recording at all), for the remux that finished them then.
    Legacy,
}

/// Finishes the recording at `path` in place, whatever state an earlier
/// finish was cut off in (see the spec's state table): a fragmented file
/// is indexed from its fragments and finished; one already finished is
/// left as it is.
pub fn finish_in_place(path: &Path) -> InPlace {
    let started = Instant::now();
    let layout = match read_start(path) {
        Ok(start) => layout(&start),
        Err(e) => {
            tracing::warn!("couldn't read {} ({e})", path.display());
            return InPlace::Legacy;
        }
    };
    let outcome = match layout {
        Layout::Finished => InPlace::Finished,
        Layout::Other => InPlace::Legacy,
        Layout::Placeholder { .. } => finish_fragmented(path).unwrap_or_else(|e| {
            tracing::warn!(
                "couldn't finish {} in place ({e}); keeping it fragmented",
                path.display()
            );
            InPlace::KeptFragmented
        }),
    };
    tracing::info!(
        "{}: {outcome:?}, in {:?}",
        path.display(),
        started.elapsed()
    );
    outcome
}

fn finish_fragmented(path: &Path) -> io::Result<InPlace> {
    use std::os::windows::fs::OpenOptionsExt;
    // Held for this process alone while it's changed.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(path)?;
    let scan = scan(&mut file)?;
    if scan.damaged {
        tracing::warn!(
            "{} is damaged {} bytes in, between two fragments; keeping it fragmented as it is",
            path.display(),
            scan.index.end()
        );
        return Ok(InPlace::KeptFragmented);
    }
    if scan.index.sample_count() == 0 {
        tracing::warn!("{} holds no whole fragment", path.display());
        return Ok(InPlace::KeptFragmented);
    }
    complete(&mut file, &scan.head, &scan.index)?;
    Ok(InPlace::Finished)
}

/// Finishes the fragmented file `file` in place, given its init segment
/// `head` (as written at its start) and the index of its samples: drops
/// whatever follows the last whole fragment, appends the complete `moov`,
/// then turns the placeholder into the header of an `mdat` holding
/// everything before that `moov`, each step on disk before the next.
///
/// If writing fails, the file is put back as the fragmented file it was,
/// which reads as it is.
pub fn complete(file: &mut impl Disk, head: &[u8], index: &Index) -> io::Result<()> {
    let started = Instant::now();
    let Layout::Placeholder { at } = layout(head) else {
        return Err(invalid("no placeholder after ftyp"));
    };
    let stub = stub_moov(head, at)?;
    let moov = complete_moov(stub, index)?;
    let end = index.end();
    if end < (at + 16) + stub.len() as u64 {
        return Err(invalid("the index ends before the stub moov does"));
    }
    file.set_len(end)?;
    if let Err(e) = write_finish(file, at, end, &moov) {
        undo_finish(file, at, end);
        return Err(e);
    }
    tracing::info!(
        "finished in place: {} samples, a {} KB index, in {:?}",
        index.sample_count(),
        moov.len() >> 10,
        started.elapsed()
    );
    Ok(())
}

/// Puts a file whose finish failed back as the fragmented file it was,
/// which reads as it is: a `moov` half-written at its end could stop it
/// reading at all. The placeholder goes back, onto the disk, before the
/// `moov` goes: the other way round, a power cut between could leave an
/// `mdat` header with no `moov` after the data, which nothing reads. If
/// the placeholder can't go back, the `moov` stays: the header may be in,
/// and with its `moov` the file is whole.
fn undo_finish(file: &mut impl Disk, at: u64, end: u64) {
    let restored = file
        .seek(SeekFrom::Start(at))
        .and_then(|_| file.write_all(&PLACEHOLDER))
        .and_then(|()| file.sync_all());
    if restored.is_ok() {
        let _ = file.set_len(end).and_then(|()| file.sync_all());
    }
}

fn write_finish(file: &mut impl Disk, at: u64, end: u64, moov: &[u8]) -> io::Result<()> {
    file.seek(SeekFrom::Start(end))?;
    file.write_all(moov)?;
    file.sync_all()?;
    file.seek(SeekFrom::Start(at))?;
    file.write_all(&mdat_header(end - at))?;
    file.sync_all()
}

/// What finishing a file in place does to it, besides writing: a
/// `File`'s own (a test's stand-in fails where it's told to).
pub trait Disk: Write + Seek {
    fn set_len(&mut self, len: u64) -> io::Result<()>;
    /// Gets everything written so far onto the disk.
    fn sync_all(&mut self) -> io::Result<()>;
}

impl Disk for File {
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        File::set_len(self, len)
    }

    fn sync_all(&mut self) -> io::Result<()> {
        File::sync_all(self)
    }
}

/// What follows `ftyp` at the start of a file.
enum Layout {
    /// The placeholder, `at` bytes in: a fragmented recording, to finish.
    Placeholder { at: u64 },
    /// A 64-bit `mdat` header: finished in place.
    Finished,
    /// Anything else.
    Other,
}

fn layout(start: &[u8]) -> Layout {
    if start.get(4..8) != Some(b"ftyp") {
        return Layout::Other;
    }
    let Ok(at) = be32(start, 0) else {
        return Layout::Other;
    };
    match start.get(at as usize..at as usize + 16) {
        Some(next) if next == PLACEHOLDER => Layout::Placeholder { at: u64::from(at) },
        Some(next) if next[..8] == [0, 0, 0, 1, b'm', b'd', b'a', b't'] => Layout::Finished,
        _ => Layout::Other,
    }
}

/// Enough of the start of the file at `path` to tell its `layout`.
fn read_start(path: &Path) -> io::Result<Vec<u8>> {
    let mut start = Vec::with_capacity(64);
    File::open(path)?.take(64).read_to_end(&mut start)?;
    Ok(start)
}

/// The stub `moov` in `head`, after the placeholder at `at`.
fn stub_moov(head: &[u8], at: u64) -> io::Result<&[u8]> {
    let moov_at = at as usize + 16;
    let size = be32(head, moov_at)? as usize;
    match head.get(moov_at..moov_at + size) {
        Some(moov) if size >= 8 && &moov[4..8] == b"moov" => Ok(moov),
        _ => Err(invalid("no stub moov after the placeholder")),
    }
}

/// The longest `moof` or stub `moov` read whole: far past any PolyRec
/// writes (a 2-second fragment at 240 fps is about 10 KB).
const MAX_HEADER_BOX: u64 = 16 << 20;

/// A fragmented file read for finishing: its init segment, and the index
/// of every whole fragment's samples up to where it ends or is cut off.
struct Scan {
    head: Vec<u8>,
    index: Index,
    /// Whether it stopped at damage between two fragments rather than at
    /// the end of what was written.
    damaged: bool,
}

/// Walks the fragments of `file` (which has the placeholder), reading
/// only their `moof` boxes and `mdat` headers, never the media.
fn scan(file: &mut File) -> io::Result<Scan> {
    let len = file.metadata()?.len();
    let mut reader = BufReader::with_capacity(16 << 10, file);
    let start = read_at(&mut reader, 0, len.min(64))?;
    let Layout::Placeholder { at } = layout(&start) else {
        return Err(invalid("no placeholder after ftyp"));
    };
    let moov_at = at + 16;
    let header = read_at(&mut reader, moov_at, 8)?;
    let moov_size = u64::from(be32(&header, 0)?);
    if &header[4..8] != b"moov" || !(8..=MAX_HEADER_BOX).contains(&moov_size) {
        return Err(invalid("no stub moov after the placeholder"));
    }
    let head = read_at(&mut reader, 0, moov_at + moov_size)?;
    let mut index = Index::new(&track_ids(stub_moov(&head, at)?)?);
    let mut at = head.len() as u64;
    index.set_end(at);
    let damaged = loop {
        match next_fragment(&mut reader, at, len)? {
            Next::Fragment { runs, end } => {
                let known = runs
                    .iter()
                    .all(|run| index.add_run(run.track_id, run.data_at, run.base, &run.samples));
                if !known {
                    break true;
                }
                at = end;
                index.set_end(end);
            }
            Next::End => break false,
            Next::Damage => break true,
        }
    };
    Ok(Scan {
        head,
        index,
        damaged,
    })
}

/// What's at a point between fragments.
enum Next {
    /// A whole fragment, ending at `end`.
    Fragment { runs: Vec<Run>, end: u64 },
    /// The end of what was written: the file's end, a fragment cut off,
    /// a zeroed tail a power cut left, or the `moov` of a finish cut off.
    End,
    /// Anything else.
    Damage,
}

/// A track's run of samples in a fragment, as its `traf` gives it.
struct Run {
    track_id: u32,
    /// Where its data starts in the file.
    data_at: u64,
    base: u64,
    samples: Vec<RunSample>,
}

fn next_fragment<R: Read + Seek>(reader: &mut R, at: u64, len: u64) -> io::Result<Next> {
    if len - at < 8 {
        return Ok(Next::End);
    }
    let header = read_at(reader, at, 8)?;
    if header == [0; 8] || &header[4..8] == b"moov" {
        return Ok(Next::End);
    }
    let size = u64::from(be32(&header, 0)?);
    if &header[4..8] != b"moof" || !(8..=MAX_HEADER_BOX).contains(&size) {
        return Ok(Next::Damage);
    }
    let mdat_at = at + size;
    if mdat_at > len || len - mdat_at < 8 {
        return Ok(Next::End);
    }
    let moof = read_at(reader, at, size)?;
    let mdat = read_at(reader, mdat_at, (len - mdat_at).min(16))?;
    if mdat[..8] == [0; 8] {
        return Ok(Next::End);
    }
    if &mdat[4..8] != b"mdat" {
        return Ok(Next::Damage);
    }
    let (mdat_size, mdat_header) = match be32(&mdat, 0)? {
        1 if mdat.len() < 16 => return Ok(Next::End),
        1 => (be64(&mdat, 8)?, 16),
        size if size < 8 => return Ok(Next::Damage),
        size => (u64::from(size), 8),
    };
    if mdat_size < mdat_header || mdat_size > len - mdat_at {
        return Ok(Next::End);
    }
    let Some(runs) = parse_moof(&moof, at) else {
        return Ok(Next::Damage);
    };
    let data_start = mdat_at + mdat_header;
    let data_end = mdat_at + mdat_size;
    let inside = runs.iter().all(|run| {
        let size: u64 = run.samples.iter().map(|s| u64::from(s.size)).sum();
        run.data_at >= data_start && run.data_at + size <= data_end
    });
    if !inside {
        return Ok(Next::Damage);
    }
    Ok(Next::Fragment {
        runs,
        end: data_end,
    })
}

/// The runs of the `moof` (whole) starting `moof_at` bytes into the file,
/// as `boxes::fragment` writes them; `None` for any other layout.
fn parse_moof(moof: &[u8], moof_at: u64) -> Option<Vec<Run>> {
    const DEFAULT_BASE_IS_MOOF: u32 = 0x02_0000;
    const TRUN_FLAGS: u32 = 0x0001 | 0x0100 | 0x0200 | 0x0400;
    let mut runs = Vec::new();
    for (kind, traf) in parse(body(moof)).ok()? {
        if &kind != b"traf" {
            continue;
        }
        let parts = parse(body(traf)).ok()?;
        let tfhd = one(&parts, b"tfhd").ok()?;
        if be32(tfhd, 8).ok()? & 0x00ff_ffff != DEFAULT_BASE_IS_MOOF {
            return None;
        }
        let tfdt = one(&parts, b"tfdt").ok()?;
        let base = match tfdt.get(8)? {
            1 => be64(tfdt, 12).ok()?,
            _ => u64::from(be32(tfdt, 12).ok()?),
        };
        let trun = one(&parts, b"trun").ok()?;
        if be32(trun, 8).ok()? & 0x00ff_ffff != TRUN_FLAGS {
            return None;
        }
        let count = be32(trun, 12).ok()? as usize;
        if trun.len() != 20 + count * 12 {
            return None;
        }
        let samples = (0..count)
            .map(|i| {
                let at = 20 + i * 12;
                Some(RunSample {
                    duration: be32(trun, at).ok()?,
                    size: be32(trun, at + 4).ok()?,
                    keyframe: be32(trun, at + 8).ok()? & 0x0001_0000 == 0,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        runs.push(Run {
            track_id: be32(tfhd, 12).ok()?,
            data_at: moof_at + u64::from(be32(trun, 16).ok()?),
            base,
            samples,
        });
    }
    Some(runs)
}

/// The `track_ID`s of the stub `moov`'s tracks, in order.
fn track_ids(stub: &[u8]) -> io::Result<Vec<u32>> {
    parse(body(stub))?
        .iter()
        .filter(|(kind, _)| kind == b"trak")
        .map(|(_, trak)| be32(one(&parse(body(trak))?, b"tkhd")?, 20))
        .collect()
}

fn read_at<R: Read + Seek>(reader: &mut R, at: u64, len: u64) -> io::Result<Vec<u8>> {
    reader.seek(SeekFrom::Start(at))?;
    let mut out = vec![0; len as usize];
    reader.read_exact(&mut out)?;
    Ok(out)
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

fn be64(b: &[u8], at: usize) -> io::Result<u64> {
    b.get(at..at + 8)
        .map(|v| u64::from_be_bytes(v.try_into().unwrap()))
        .ok_or_else(|| invalid("a box too short"))
}

fn invalid(what: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.into())
}

#[cfg(test)]
pub(crate) mod tests {
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

    /// Written before soft remux: no placeholder. Today's remux takes it.
    #[test]
    fn a_file_without_the_placeholder_is_legacy() {
        let case = Case::new();
        let legacy = [&case.muxed.out[..32], &case.muxed.out[48..]].concat();
        let path = case.file(&legacy);
        assert_eq!(finish_in_place(&path), InPlace::Legacy);
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
    #[test]
    fn three_hours_finish_in_seconds() {
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
        assert!(at_stop < Duration::from_secs(2), "at Stop: {at_stop:?}");
        assert!(
            at_start < Duration::from_secs(2),
            "at the next start: {at_start:?}"
        );
        assert!(
            finished == std::fs::read(&copy).unwrap(),
            "the same file either way"
        );
    }
}
