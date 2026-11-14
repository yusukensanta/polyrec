//! Reading the MP4 boxes PolyRec itself wrote (`boxes`): a fragment's runs,
//! the stub `moov`'s tracks. The one place that does; the flags it expects
//! are the writer's own constants, so the two can't drift apart -- a reader
//! that disagreed with the writer would report every recording damaged.
//!
//! (The tests' own parser in `boxes::tests` is separate on purpose: it
//! checks what was written without trusting this code.)

use super::boxes::{RunSample, SAMPLE_IS_NON_SYNC, TFHD_FLAGS, TRUN_FLAGS};
use std::io::{self, Read, Seek, SeekFrom};

/// A track's run of samples in a fragment, as its `traf` gives it.
pub(super) struct Run {
    pub(super) track_id: u32,
    /// Where its data starts in the file.
    pub(super) data_at: u64,
    pub(super) base: u64,
    pub(super) samples: Vec<RunSample>,
}

/// The runs of the `moof` (whole) starting `moof_at` bytes into the file,
/// as `boxes::fragment` writes them; `None` for any other layout.
pub(super) fn parse_moof(moof: &[u8], moof_at: u64) -> Option<Vec<Run>> {
    let mut runs = Vec::new();
    for (kind, traf) in parse(body(moof)).ok()? {
        if &kind != b"traf" {
            continue;
        }
        let parts = parse(body(traf)).ok()?;
        let tfhd = one(&parts, b"tfhd").ok()?;
        if be32(tfhd, 8).ok()? & 0x00ff_ffff != TFHD_FLAGS {
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
                    keyframe: be32(trun, at + 8).ok()? & SAMPLE_IS_NON_SYNC == 0,
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
pub(super) fn track_ids(stub: &[u8]) -> io::Result<Vec<u32>> {
    parse(body(stub))?
        .iter()
        .filter(|(kind, _)| kind == b"trak")
        .map(|(_, trak)| be32(one(&parse(body(trak))?, b"tkhd")?, 20))
        .collect()
}

pub(super) fn read_at<R: Read + Seek>(reader: &mut R, at: u64, len: u64) -> io::Result<Vec<u8>> {
    reader.seek(SeekFrom::Start(at))?;
    let mut out = vec![0; len as usize];
    reader.read_exact(&mut out)?;
    Ok(out)
}

/// The boxes one after another in `data`, each whole: (type, bytes).
/// 32-bit sizes only: this reads the stub `moov`, PolyRec's own.
pub(super) fn parse(data: &[u8]) -> io::Result<Vec<([u8; 4], &[u8])>> {
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
pub(super) fn body(b: &[u8]) -> &[u8] {
    &b[8..]
}

/// The box of type `kind` among `boxes`.
pub(super) fn one<'a>(boxes: &[([u8; 4], &'a [u8])], kind: &[u8; 4]) -> io::Result<&'a [u8]> {
    boxes
        .iter()
        .find(|(k, _)| k == kind)
        .map(|&(_, b)| b)
        .ok_or_else(|| invalid(format!("no {}", String::from_utf8_lossy(kind))))
}

pub(super) fn be32(b: &[u8], at: usize) -> io::Result<u32> {
    b.get(at..at + 4)
        .map(|v| u32::from_be_bytes(v.try_into().unwrap()))
        .ok_or_else(|| invalid("a box too short"))
}

pub(super) fn be64(b: &[u8], at: usize) -> io::Result<u64> {
    b.get(at..at + 8)
        .map(|v| u64::from_be_bytes(v.try_into().unwrap()))
        .ok_or_else(|| invalid("a box too short"))
}

pub(super) fn invalid(what: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.into())
}
