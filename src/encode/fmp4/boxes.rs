//! ISO base media file format boxes for a fragmented MP4 (ISO/IEC
//! 14496-12): the init segment (`ftyp`, a placeholder, then `moov` with
//! empty sample tables and `mvex`), then a `moof` + `mdat` per fragment;
//! and the sample tables of the complete `moov` a finished file ends with
//! (see `soft_remux`).

use super::nal::VideoCodec;

/// One track of the file.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackInfo {
    /// From 1.
    pub id: u32,
    /// Ticks per second of the track's times.
    pub timescale: u32,
    pub kind: TrackKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TrackKind {
    Video {
        codec: VideoCodec,
        width: u16,
        height: u16,
        /// The `avcC` / `hvcC` body (see `nal::configuration_record`).
        config: Vec<u8>,
    },
    Audio {
        channels: u16,
        sample_rate: u32,
        /// AAC's AudioSpecificConfig.
        audio_specific_config: Vec<u8>,
    },
}

/// One sample of a fragment's run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RunSample {
    /// In the track's timescale.
    pub duration: u32,
    pub size: u32,
    /// A sync sample: decoding can start here.
    pub keyframe: bool,
}

/// A track's samples in one fragment, and their data.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackRun {
    pub track_id: u32,
    /// The first sample's decode time, in the track's timescale.
    pub base_decode_time: u64,
    pub samples: Vec<RunSample>,
    pub data: Vec<u8>,
}

/// Written straight after `ftyp`: an empty `free` box exactly the size of
/// a 64-bit `mdat` header (`mdat_header`), which finishing the file writes
/// over it. Readers skip a `free` box.
pub const PLACEHOLDER: [u8; 16] = [0, 0, 0, 16, b'f', b'r', b'e', b'e', 0, 0, 0, 0, 0, 0, 0, 0];

/// The header of an `mdat` box of `size` bytes, header included, in the
/// 64-bit form (`size` 1, then a `largesize`), whatever `size` is.
pub fn mdat_header(size: u64) -> [u8; 16] {
    let mut header = [0; 16];
    header[..4].copy_from_slice(&1u32.to_be_bytes());
    header[4..8].copy_from_slice(b"mdat");
    header[8..].copy_from_slice(&size.to_be_bytes());
    header
}

/// `ftyp`, the placeholder, then `moov`, for `tracks`.
pub fn init_segment(tracks: &[TrackInfo]) -> Vec<u8> {
    let mut out = boxed(b"ftyp", |o| {
        o.bytes(b"isom").u32(0x200);
        for brand in [b"isom", b"iso6", b"iso2", b"mp41"] {
            o.bytes(brand);
        }
    });
    out.extend(PLACEHOLDER);
    out.extend(boxed(b"moov", |o| {
        o.bytes(&full_box(b"mvhd", 0, 0, |o| {
            o.u32(0).u32(0).u32(1000).u32(0); // times, timescale, duration
            o.u32(0x0001_0000).u16(0x0100).zeros(10); // rate, volume
            matrix(o);
            o.zeros(24).u32(tracks.len() as u32 + 1); // next_track_ID
        }));
        for track in tracks {
            o.bytes(&trak(track));
        }
        o.bytes(&boxed(b"mvex", |o| {
            for track in tracks {
                o.bytes(&full_box(b"trex", 0, 0, |o| {
                    o.u32(track.id).u32(1).u32(0).u32(0).u32(0);
                }));
            }
        }));
    }));
    out
}

/// A fragment's bytes, and where each run's data starts in them.
#[derive(Debug, Clone, PartialEq)]
pub struct Fragment {
    /// `moof` + `mdat`.
    pub bytes: Vec<u8>,
    /// Per run, its data's offset from the `moof`'s first byte (the
    /// `trun`'s `data_offset`).
    pub data_offsets: Vec<u32>,
}

/// `moof` + `mdat` for fragment number `sequence` (from 1).
pub fn fragment(sequence: u32, runs: &[TrackRun]) -> Fragment {
    // The data offsets don't change the moof's size: build it once to
    // learn that, then with the offsets.
    let moof_size = moof(sequence, runs, &vec![0; runs.len()]).len() as u32;
    let data_offsets: Vec<u32> = runs
        .iter()
        .scan(moof_size + 8, |at, run| {
            let this = *at;
            *at += run.data.len() as u32;
            Some(this)
        })
        .collect();
    let mut bytes = moof(sequence, runs, &data_offsets);
    bytes.extend(boxed(b"mdat", |o| {
        for run in runs {
            o.bytes(&run.data);
        }
    }));
    Fragment {
        bytes,
        data_offsets,
    }
}

/// `moof` whose runs' data starts `data_offsets` bytes from its first byte.
fn moof(sequence: u32, runs: &[TrackRun], data_offsets: &[u32]) -> Vec<u8> {
    boxed(b"moof", |o| {
        o.bytes(&full_box(b"mfhd", 0, 0, |o| {
            o.u32(sequence);
        }));
        for (run, &data_offset) in runs.iter().zip(data_offsets) {
            o.bytes(&boxed(b"traf", |o| {
                const DEFAULT_BASE_IS_MOOF: u32 = 0x02_0000;
                o.bytes(&full_box(b"tfhd", 0, DEFAULT_BASE_IS_MOOF, |o| {
                    o.u32(run.track_id);
                }));
                o.bytes(&full_box(b"tfdt", 1, 0, |o| {
                    o.u64(run.base_decode_time);
                }));
                // data offset, and each sample's duration, size and flags
                const TRUN_FLAGS: u32 = 0x0001 | 0x0100 | 0x0200 | 0x0400;
                o.bytes(&full_box(b"trun", 0, TRUN_FLAGS, |o| {
                    o.u32(run.samples.len() as u32).u32(data_offset);
                    for sample in &run.samples {
                        // sample_depends_on 2 (nothing), or 1 plus
                        // sample_is_non_sync_sample.
                        let flags = if sample.keyframe {
                            0x0200_0000
                        } else {
                            0x0101_0000
                        };
                        o.u32(sample.duration).u32(sample.size).u32(flags);
                    }
                }));
            }));
        }
    })
}

/// `stts`: the samples' durations, equal ones in a row as one entry.
pub(super) fn stts(durations: &[u32]) -> Vec<u8> {
    let runs = run_lengths(durations);
    full_box(b"stts", 0, 0, |o| {
        o.u32(runs.len() as u32);
        for (count, duration) in runs {
            o.u32(count).u32(duration);
        }
    })
}

/// `stsz`: every sample's size (none shared).
pub(super) fn stsz(sizes: &[u32]) -> Vec<u8> {
    full_box(b"stsz", 0, 0, |o| {
        o.u32(0).u32(sizes.len() as u32);
        for &size in sizes {
            o.u32(size);
        }
    })
}

/// `stsc`, from each chunk's sample count: an entry where it changes.
pub(super) fn stsc(samples_per_chunk: &[u32]) -> Vec<u8> {
    let mut entries: Vec<(u32, u32)> = Vec::new();
    for (index, &count) in samples_per_chunk.iter().enumerate() {
        if entries.last().is_none_or(|&(_, last)| last != count) {
            entries.push((index as u32 + 1, count));
        }
    }
    full_box(b"stsc", 0, 0, |o| {
        o.u32(entries.len() as u32);
        for (first_chunk, count) in entries {
            o.u32(first_chunk).u32(count).u32(1); // sample_description_index
        }
    })
}

/// `co64`: each chunk's offset from the file's first byte.
pub(super) fn co64(offsets: &[u64]) -> Vec<u8> {
    full_box(b"co64", 0, 0, |o| {
        o.u32(offsets.len() as u32);
        for &offset in offsets {
            o.u64(offset);
        }
    })
}

/// `stss`: the sync samples' numbers, from 1.
pub(super) fn stss(sync_samples: &[u32]) -> Vec<u8> {
    full_box(b"stss", 0, 0, |o| {
        o.u32(sync_samples.len() as u32);
        for &sample in sync_samples {
            o.u32(sample);
        }
    })
}

/// `ctts` putting each of `sample_count` samples `offset` ticks after its
/// decode time: a track that starts after the others, as Media
/// Foundation's MP4 sink writes one.
pub(super) fn ctts(sample_count: u32, offset: u32) -> Vec<u8> {
    full_box(b"ctts", 0, 0, |o| {
        o.u32(1).u32(sample_count).u32(offset);
    })
}

/// The whole version-0 `mvhd`, `tkhd` or `mdhd` box `header` with
/// `duration`: still version 0 if that fits in 32 bits, else version 1,
/// whose times and duration take 64. Everything else stays. Read from a
/// file at recovery, `header` may be damaged: a box too short, or of
/// another version, is refused.
pub(super) fn with_duration(header: &[u8], duration: u64) -> std::io::Result<Vec<u8>> {
    // After size, type, version and flags: created and modified, then the
    // timescale (`mvhd`, `mdhd`) or the track_ID and 4 reserved bytes
    // (`tkhd`), then the duration.
    let duration_at = if header.get(4..8) == Some(b"tkhd") {
        28
    } else {
        24
    };
    if header.len() < duration_at + 4 || header[8] != 0 {
        let kind = String::from_utf8_lossy(header.get(4..8).unwrap_or_default());
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("a {kind} box PolyRec didn't write"),
        ));
    }
    if let Ok(short) = u32::try_from(duration) {
        let mut out = header.to_vec();
        out[duration_at..duration_at + 4].copy_from_slice(&short.to_be_bytes());
        return Ok(out);
    }
    let be32 = |at: usize| u32::from_be_bytes(header[at..at + 4].try_into().unwrap());
    let mut kind = [0; 4];
    kind.copy_from_slice(&header[4..8]);
    Ok(full_box(&kind, 1, be32(8) & 0x00ff_ffff, |o| {
        o.u64(u64::from(be32(12))).u64(u64::from(be32(16)));
        o.bytes(&header[20..duration_at]);
        o.u64(duration);
        o.bytes(&header[duration_at + 4..]);
    }))
}

/// `values` as (how many in a row, value) pairs.
fn run_lengths(values: &[u32]) -> Vec<(u32, u32)> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &value in values {
        match runs.last_mut() {
            Some((count, last)) if *last == value => *count += 1,
            _ => runs.push((1, value)),
        }
    }
    runs
}

fn trak(track: &TrackInfo) -> Vec<u8> {
    let video = matches!(track.kind, TrackKind::Video { .. });
    boxed(b"trak", |o| {
        // enabled, in the movie
        o.bytes(&full_box(b"tkhd", 0, 3, |o| {
            o.u32(0).u32(0).u32(track.id).u32(0).u32(0); // times, id, reserved, duration
            o.zeros(8).u16(0).u16(0); // reserved, layer, alternate_group
            o.u16(if video { 0 } else { 0x0100 }).u16(0); // volume
            matrix(o);
            let (width, height) = match track.kind {
                TrackKind::Video { width, height, .. } => (width, height),
                TrackKind::Audio { .. } => (0, 0),
            };
            o.u32(u32::from(width) << 16).u32(u32::from(height) << 16);
        }));
        o.bytes(&boxed(b"mdia", |o| {
            o.bytes(&full_box(b"mdhd", 0, 0, |o| {
                o.u32(0).u32(0).u32(track.timescale).u32(0);
                o.u16(0x55c4).u16(0); // language "und"
            }));
            o.bytes(&full_box(b"hdlr", 0, 0, |o| {
                o.u32(0)
                    .bytes(if video { b"vide" } else { b"soun" })
                    .zeros(12);
                o.bytes(if video {
                    b"VideoHandler\0"
                } else {
                    b"SoundHandler\0"
                });
            }));
            o.bytes(&boxed(b"minf", |o| {
                if video {
                    o.bytes(&full_box(b"vmhd", 0, 1, |o| {
                        o.zeros(8);
                    }));
                } else {
                    o.bytes(&full_box(b"smhd", 0, 0, |o| {
                        o.zeros(4);
                    }));
                }
                o.bytes(&boxed(b"dinf", |o| {
                    o.bytes(&full_box(b"dref", 0, 0, |o| {
                        // one entry: this file
                        o.u32(1).bytes(&full_box(b"url ", 0, 1, |_| {}));
                    }));
                }));
                o.bytes(&boxed(b"stbl", |o| {
                    o.bytes(&full_box(b"stsd", 0, 0, |o| {
                        o.u32(1).bytes(&sample_entry(&track.kind));
                    }));
                    // Empty: the samples are in the fragments.
                    let no_entries = |o: &mut Out| {
                        o.u32(0);
                    };
                    o.bytes(&full_box(b"stts", 0, 0, no_entries));
                    o.bytes(&full_box(b"stsc", 0, 0, no_entries));
                    o.bytes(&full_box(b"stsz", 0, 0, |o| {
                        o.u32(0).u32(0); // sample_size, sample_count
                    }));
                    o.bytes(&full_box(b"stco", 0, 0, no_entries));
                }));
            }));
        }));
    })
}

fn sample_entry(kind: &TrackKind) -> Vec<u8> {
    match kind {
        TrackKind::Video {
            codec,
            width,
            height,
            config,
        } => {
            let (entry, record) = match codec {
                VideoCodec::Hevc => (b"hvc1", b"hvcC"),
                VideoCodec::H264 => (b"avc1", b"avcC"),
            };
            boxed(entry, |o| {
                o.zeros(6).u16(1); // reserved, data_reference_index
                o.zeros(16).u16(*width).u16(*height);
                o.u32(0x0048_0000).u32(0x0048_0000).u32(0).u16(1); // 72 dpi, frame_count
                o.zeros(32).u16(0x0018).u16(0xffff); // compressorname, depth, pre_defined
                o.bytes(&boxed(record, |o| {
                    o.bytes(config);
                }));
            })
        }
        TrackKind::Audio {
            channels,
            sample_rate,
            audio_specific_config,
        } => boxed(b"mp4a", |o| {
            o.zeros(6).u16(1).zeros(8); // reserved, data_reference_index, reserved
            o.u16(*channels).u16(16).zeros(4); // samplesize, pre_defined, reserved
            o.u32((*sample_rate).min(0xffff) << 16);
            o.bytes(&esds(audio_specific_config));
        }),
    }
}

/// ISO/IEC 14496-1 ES_Descriptor for AAC (object type 0x40).
fn esds(audio_specific_config: &[u8]) -> Vec<u8> {
    let decoder_specific = descriptor(0x05, audio_specific_config);
    let mut config = vec![0x40, 0x15, 0, 0, 0]; // AAC, audio stream, buffer size
    config.extend([0; 8]); // max and average bitrate unknown
    config.extend(decoder_specific);
    let mut es = vec![0, 0, 0]; // ES_ID, flags
    es.extend(descriptor(0x04, &config));
    es.extend(descriptor(0x06, &[0x02])); // SLConfig: MP4
    full_box(b"esds", 0, 0, |o| {
        o.bytes(&descriptor(0x03, &es));
    })
}

fn descriptor(tag: u8, body: &[u8]) -> Vec<u8> {
    // Sizes here are always under 128: one byte.
    let mut out = vec![tag, body.len() as u8];
    out.extend_from_slice(body);
    out
}

fn matrix(o: &mut Out) {
    for v in [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
        o.u32(v);
    }
}

/// Big-endian box contents.
pub(super) struct Out(Vec<u8>);

impl Out {
    pub(super) fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.0.extend_from_slice(b);
        self
    }
    pub(super) fn zeros(&mut self, n: usize) -> &mut Self {
        self.0.resize(self.0.len() + n, 0);
        self
    }
    pub(super) fn u16(&mut self, v: u16) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }
    pub(super) fn u32(&mut self, v: u32) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }
    pub(super) fn u64(&mut self, v: u64) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }
}

/// A box of type `kind` whose body `body` writes; its size is filled in.
pub(super) fn boxed(kind: &[u8; 4], body: impl FnOnce(&mut Out)) -> Vec<u8> {
    let mut out = Out(vec![0; 4]);
    out.bytes(kind);
    body(&mut out);
    let size = out.0.len() as u32;
    out.0[..4].copy_from_slice(&size.to_be_bytes());
    out.0
}

/// A full box: `boxed`, its body after `version` and `flags`.
pub(super) fn full_box(
    kind: &[u8; 4],
    version: u8,
    flags: u32,
    body: impl FnOnce(&mut Out),
) -> Vec<u8> {
    boxed(kind, |o| {
        o.u32((u32::from(version) << 24) | flags);
        body(o);
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A box: its type and body.
    #[derive(Debug)]
    pub struct Mp4Box<'a> {
        pub kind: String,
        pub body: &'a [u8],
        /// Where its header starts in the buffer it was read from.
        pub at: usize,
    }

    /// The boxes one after another in `data`.
    pub fn boxes(data: &[u8]) -> Vec<Mp4Box<'_>> {
        boxes_at(data, 0)
    }

    fn boxes_at(data: &[u8], base: usize) -> Vec<Mp4Box<'_>> {
        let mut out = Vec::new();
        let mut at = 0;
        while at + 8 <= data.len() {
            let mut size = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
            let mut header = 8;
            if size == 1 {
                // A 64-bit `largesize` follows the type.
                size = u64::from_be_bytes(data[at + 8..at + 16].try_into().unwrap()) as usize;
                header = 16;
            }
            assert!(
                size >= header && at + size <= data.len(),
                "bad box size {size} at {at}"
            );
            out.push(Mp4Box {
                kind: String::from_utf8_lossy(&data[at + 4..at + 8]).into_owned(),
                body: &data[at + header..at + size],
                at: base + at,
            });
            at += size;
        }
        assert_eq!(at, data.len(), "trailing bytes");
        out
    }

    /// The child boxes of a container box (after `skip` bytes of its own).
    pub fn children<'a>(parent: &Mp4Box<'a>, skip: usize) -> Vec<Mp4Box<'a>> {
        boxes_at(&parent.body[skip..], parent.at + 8 + skip)
    }

    pub fn find<'a>(boxes: &'a [Mp4Box<'a>], kind: &str) -> &'a Mp4Box<'a> {
        boxes
            .iter()
            .find(|b| b.kind == kind)
            .unwrap_or_else(|| panic!("no {kind} in {:?}", kinds(boxes)))
    }

    pub fn kinds(boxes: &[Mp4Box]) -> Vec<String> {
        boxes.iter().map(|b| b.kind.clone()).collect()
    }

    pub fn be32(b: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
    }

    pub fn be64(b: &[u8], at: usize) -> u64 {
        u64::from_be_bytes(b[at..at + 8].try_into().unwrap())
    }

    /// The whole box `b` was read from `data` (header included).
    pub fn whole<'a>(data: &'a [u8], b: &Mp4Box) -> &'a [u8] {
        let size = be32(data, b.at) as usize;
        let size = if size == 1 {
            be64(data, b.at + 8) as usize
        } else {
            size
        };
        &data[b.at..b.at + size]
    }

    /// The entries of the whole table box `table` (a version-0 full box with
    /// an entry count): `width` u32s each.
    pub fn entries(table: &[u8], width: usize) -> Vec<Vec<u32>> {
        assert_eq!(table[8], 0, "version 0");
        let body = &table[12..];
        let count = be32(body, 0) as usize;
        assert_eq!(body.len(), 4 + count * width * 4, "entry count vs size");
        (0..count)
            .map(|i| {
                (0..width)
                    .map(|j| be32(body, 4 + (i * width + j) * 4))
                    .collect()
            })
            .collect()
    }

    fn tracks() -> Vec<TrackInfo> {
        vec![
            TrackInfo {
                id: 1,
                timescale: 90_000,
                kind: TrackKind::Video {
                    codec: VideoCodec::Hevc,
                    width: 1920,
                    height: 1080,
                    config: vec![1, 2, 3],
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

    #[test]
    fn the_init_segment_describes_every_track_and_says_fragments_follow() {
        let init = init_segment(&tracks());
        let top = boxes(&init);
        assert_eq!(kinds(&top), ["ftyp", "free", "moov"]);
        let moov = children(&top[2], 0);
        assert_eq!(kinds(&moov), ["mvhd", "trak", "trak", "mvex"]);
        let mvex = children(find(&moov, "mvex"), 0);
        assert_eq!(kinds(&mvex), ["trex", "trex"]);
        for (trex, id) in mvex.iter().zip([1, 2]) {
            assert_eq!(be32(trex.body, 4), id, "trex track_ID");
        }

        let sample_entry = |trak: &Mp4Box| -> (String, Vec<String>) {
            let trak_parts = children(trak, 0);
            let mdia = children(find(&trak_parts, "mdia"), 0);
            let minf = children(find(&mdia, "minf"), 0);
            let stbl = children(find(&minf, "stbl"), 0);
            assert_eq!(kinds(&stbl), ["stsd", "stts", "stsc", "stsz", "stco"]);
            let stsd = find(&stbl, "stsd");
            assert_eq!(be32(stsd.body, 4), 1, "one sample entry");
            let entry = &children(stsd, 8)[0];
            let skip = if entry.kind == "mp4a" { 28 } else { 78 };
            (entry.kind.clone(), kinds(&children(entry, skip)))
        };
        let traks: Vec<_> = moov.iter().filter(|b| b.kind == "trak").collect();
        assert_eq!(sample_entry(traks[0]), ("hvc1".into(), vec!["hvcC".into()]));
        assert_eq!(sample_entry(traks[1]), ("mp4a".into(), vec!["esds".into()]));
    }

    /// The codec configuration goes in as given; AAC's in an esds
    /// descriptor (ISO/IEC 14496-1) as object type 0x40.
    #[test]
    fn sample_entries_carry_the_codec_configurations() {
        let init = init_segment(&tracks());
        let at = |needle: &[u8]| init.windows(needle.len()).position(|w| w == needle);
        let hvcc = at(b"hvcC").expect("hvcC");
        assert_eq!(
            &init[hvcc - 4..hvcc + 7],
            [0, 0, 0, 11, b'h', b'v', b'c', b'C', 1, 2, 3]
        );
        let esds = at(b"esds").expect("esds");
        let body = &init[esds + 4..];
        assert!(
            body.windows(5).any(|w| w == [0x05, 2, 0x11, 0x90, 0x06]),
            "DecoderSpecificInfo"
        );
        assert!(
            body.windows(2).any(|w| w == [0x40, 0x15]),
            "AAC, audio stream"
        );
    }

    #[test]
    fn a_fragment_points_each_run_at_its_data() {
        let runs = vec![
            TrackRun {
                track_id: 1,
                base_decode_time: 90_000,
                samples: vec![
                    RunSample {
                        duration: 3000,
                        size: 3,
                        keyframe: true,
                    },
                    RunSample {
                        duration: 3000,
                        size: 2,
                        keyframe: false,
                    },
                ],
                data: vec![0xa1, 0xa2, 0xa3, 0xb1, 0xb2],
            },
            TrackRun {
                track_id: 2,
                base_decode_time: 48_000,
                samples: vec![RunSample {
                    duration: 1024,
                    size: 4,
                    keyframe: true,
                }],
                data: vec![0xc1, 0xc2, 0xc3, 0xc4],
            },
        ];
        let made = fragment(7, &runs);
        let bytes = made.bytes;
        let top = boxes(&bytes);
        assert_eq!(kinds(&top), ["moof", "mdat"]);
        let moof = children(&top[0], 0);
        assert_eq!(kinds(&moof), ["mfhd", "traf", "traf"]);
        assert_eq!(be32(moof[0].body, 4), 7, "sequence_number");
        assert_eq!(made.data_offsets.len(), runs.len());
        for ((traf, run), told) in moof[1..].iter().zip(&runs).zip(&made.data_offsets) {
            let parts = children(traf, 0);
            assert_eq!(kinds(&parts), ["tfhd", "tfdt", "trun"]);
            assert_eq!(be32(parts[0].body, 4), run.track_id);
            let tfdt = parts[1].body;
            assert_eq!(tfdt[0], 1, "64-bit tfdt");
            assert_eq!(
                u64::from_be_bytes(tfdt[4..12].try_into().unwrap()),
                run.base_decode_time
            );
            let trun = parts[2].body;
            assert_eq!(be32(trun, 4) as usize, run.samples.len());
            // Data offsets count from the moof's first byte.
            let offset = be32(trun, 8) as usize;
            assert_eq!(
                *told as usize, offset,
                "the offset it reports is the trun's"
            );
            let data_at = top[0].at + offset;
            assert_eq!(
                &bytes[data_at..data_at + run.data.len()],
                run.data.as_slice()
            );
            let first_flags = be32(trun, 12 + 8);
            assert_eq!(
                first_flags & 0x0001_0000,
                0,
                "the first sample is a sync sample"
            );
        }
        let trun = children(&moof[1], 0)[2].body;
        let second_flags = be32(trun, 12 + 12 + 8);
        assert_ne!(second_flags & 0x0001_0000, 0, "a non-sync sample says so");
    }

    /// Room for the 64-bit `mdat` header a finished file starts with, so
    /// finishing it never moves anything.
    #[test]
    fn the_init_segment_keeps_room_for_a_64_bit_mdat_header() {
        let init = init_segment(&tracks());
        let top = boxes(&init);
        assert_eq!(kinds(&top), ["ftyp", "free", "moov"]);
        let at = top[1].at;
        assert_eq!(init[at..at + 16], PLACEHOLDER);
    }

    #[test]
    fn an_mdat_header_fits_the_placeholder_exactly() {
        assert_eq!(
            mdat_header(0x1_0000_0010),
            [
                0, 0, 0, 1, b'm', b'd', b'a', b't', 0, 0, 0, 1, 0, 0, 0, 0x10
            ]
        );
        assert_eq!(mdat_header(0).len(), PLACEHOLDER.len());
        let mut file = mdat_header(24).to_vec();
        file.extend([7; 8]);
        let top = boxes(&file);
        assert_eq!(kinds(&top), ["mdat"]);
        assert_eq!(top[0].body, [7; 8]);
    }

    #[test]
    fn stts_runs_equal_durations_together() {
        let table = stts(&[1000, 1000, 1000, 1024, 1000]);
        assert_eq!(&table[4..8], b"stts");
        assert_eq!(
            entries(&table, 2),
            [vec![3, 1000], vec![1, 1024], vec![1, 1000]]
        );
        assert!(entries(&stts(&[]), 2).is_empty());
    }

    #[test]
    fn stsc_lists_only_chunks_whose_sample_count_changes() {
        let table = stsc(&[20, 20, 18, 18, 5]);
        assert_eq!(&table[4..8], b"stsc");
        assert_eq!(
            entries(&table, 3),
            [vec![1, 20, 1], vec![3, 18, 1], vec![5, 5, 1]]
        );
        assert!(entries(&stsc(&[]), 3).is_empty());
    }

    #[test]
    fn co64_holds_offsets_past_4_gib() {
        let table = co64(&[8, 5 << 30]);
        assert_eq!(&table[4..8], b"co64");
        assert_eq!(table[8], 0, "version 0");
        assert_eq!(be32(&table, 12), 2);
        assert_eq!(be64(&table, 16), 8);
        assert_eq!(be64(&table, 24), 5 << 30);
        assert_eq!(table.len(), 32);
    }

    #[test]
    fn stsz_stss_and_ctts_hold_what_theyre_given() {
        let sizes = stsz(&[10, 20, 30]);
        assert_eq!(&sizes[4..8], b"stsz");
        assert_eq!(be32(&sizes, 12), 0, "sample_size: they differ");
        assert_eq!(be32(&sizes, 16), 3, "sample_count");
        assert_eq!(
            [be32(&sizes, 20), be32(&sizes, 24), be32(&sizes, 28)],
            [10, 20, 30]
        );
        assert_eq!(sizes.len(), 32);

        let sync = stss(&[1, 4]);
        assert_eq!(&sync[4..8], b"stss");
        assert_eq!(entries(&sync, 1), [vec![1], vec![4]]);

        let offset = ctts(3, 1182);
        assert_eq!(&offset[4..8], b"ctts");
        assert_eq!(entries(&offset, 2), [vec![3, 1182]]);
    }

    /// `mvhd`, `tkhd` and `mdhd` hold their duration in 32 bits in version
    /// 0: about 13 h at the video's 90 kHz. Longer takes version 1.
    #[test]
    fn a_duration_past_32_bits_makes_version_1_headers() {
        let init = init_segment(&tracks());
        let top = boxes(&init);
        let moov = children(&top[2], 0);
        let trak = children(find(&moov, "trak"), 0);
        let mdia = children(find(&trak, "mdia"), 0);
        for header in [
            find(&moov, "mvhd"),
            find(&trak, "tkhd"),
            find(&mdia, "mdhd"),
        ] {
            let old = whole(&init, header);
            let what = &header.kind;
            // After version and flags: created, modified, then the
            // timescale (the track_ID and 4 reserved bytes in a tkhd),
            // then the duration.
            let duration_at = if what == "tkhd" { 28 } else { 24 };

            let short = with_duration(old, 100).unwrap();
            assert_eq!(short.len(), old.len(), "{what}");
            assert_eq!(short[8], 0, "{what}: version 0");
            assert_eq!(be32(&short, duration_at), 100, "{what}");
            assert_eq!(short[..duration_at], old[..duration_at], "{what}");
            assert_eq!(short[duration_at + 4..], old[duration_at + 4..], "{what}");

            let long = with_duration(old, 1 << 33).unwrap();
            assert_eq!(long.len(), old.len() + 12, "{what}");
            assert_eq!(be32(&long, 0) as usize, long.len(), "{what}: its size");
            assert_eq!(long[4..8], old[4..8], "{what}");
            assert_eq!(long[8], 1, "{what}: version 1");
            assert_eq!(long[9..12], old[9..12], "{what}: flags");
            assert_eq!(be64(&long, 12), u64::from(be32(old, 12)), "{what}");
            assert_eq!(be64(&long, 20), u64::from(be32(old, 16)), "{what}");
            assert_eq!(long[28..32], old[20..24], "{what}: timescale / track_ID");
            assert_eq!(long[32..duration_at + 8], old[24..duration_at], "{what}");
            assert_eq!(be64(&long, duration_at + 8), 1 << 33, "{what}");
            assert_eq!(long[duration_at + 16..], old[duration_at + 4..], "{what}");
        }
    }
}
