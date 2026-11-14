//! Finishing a fragmented recording in place -- a "soft remux", the
//! technique OBS ships as Hybrid MP4: a complete `moov` is written at the
//! file's end from the index of its samples, then the placeholder after
//! `ftyp` (`boxes::PLACEHOLDER`) becomes the header of one `mdat` holding
//! everything before that `moov`. No media is copied or moved.

use super::boxes::{
    PLACEHOLDER, RunSample, boxed, co64, ctts, mdat_header, stsc, stss, stsz, stts, with_duration,
};
use super::read::*;
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
            // A start 2^32 ticks late (about 5 h at 240 fps, 20 h at 60)
            // doesn't fit; it can't happen in a recording.
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
    /// No placeholder, or unreadable: written before recordings were
    /// finished in place (PolyRec before 0.5.162), or not a recording at
    /// all. Not touched.
    NotOurs,
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
            return InPlace::NotOurs;
        }
    };
    let outcome = match layout {
        Layout::Finished => InPlace::Finished,
        Layout::Other => InPlace::NotOurs,
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

#[cfg(test)]
pub(crate) mod tests;
