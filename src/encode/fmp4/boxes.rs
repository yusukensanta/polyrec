//! ISO base media file format boxes for a fragmented MP4 (ISO/IEC
//! 14496-12): the init segment (`ftyp` + `moov`, with empty sample tables
//! and `mvex`), then a `moof` + `mdat` per fragment.

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

/// `ftyp` + `moov` for `tracks`.
pub fn init_segment(tracks: &[TrackInfo]) -> Vec<u8> {
    let mut out = boxed(b"ftyp", |o| {
        o.bytes(b"isom").u32(0x200);
        for brand in [b"isom", b"iso6", b"iso2", b"mp41"] {
            o.bytes(brand);
        }
    });
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

/// `moof` + `mdat` for fragment number `sequence` (from 1).
pub fn fragment(sequence: u32, runs: &[TrackRun]) -> Vec<u8> {
    // The data offsets don't change the moof's size: build it once to
    // learn that, then with the offsets.
    let moof_size = moof(sequence, runs, 0).len() as u32;
    let mut out = moof(sequence, runs, moof_size + 8);
    out.extend(boxed(b"mdat", |o| {
        for run in runs {
            o.bytes(&run.data);
        }
    }));
    out
}

/// `moof` whose runs' data starts `data_start` bytes from its first byte.
fn moof(sequence: u32, runs: &[TrackRun], data_start: u32) -> Vec<u8> {
    boxed(b"moof", |o| {
        o.bytes(&full_box(b"mfhd", 0, 0, |o| {
            o.u32(sequence);
        }));
        let mut data_offset = data_start;
        for run in runs {
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
            data_offset += run.data.len() as u32;
        }
    })
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
struct Out(Vec<u8>);

impl Out {
    fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.0.extend_from_slice(b);
        self
    }
    fn zeros(&mut self, n: usize) -> &mut Self {
        self.0.resize(self.0.len() + n, 0);
        self
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }
    fn u64(&mut self, v: u64) -> &mut Self {
        self.bytes(&v.to_be_bytes())
    }
}

fn boxed(kind: &[u8; 4], body: impl FnOnce(&mut Out)) -> Vec<u8> {
    let mut out = Out(vec![0; 4]);
    out.bytes(kind);
    body(&mut out);
    let size = out.0.len() as u32;
    out.0[..4].copy_from_slice(&size.to_be_bytes());
    out.0
}

fn full_box(kind: &[u8; 4], version: u8, flags: u32, body: impl FnOnce(&mut Out)) -> Vec<u8> {
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
            let size = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
            assert!(
                size >= 8 && at + size <= data.len(),
                "bad box size {size} at {at}"
            );
            out.push(Mp4Box {
                kind: String::from_utf8_lossy(&data[at + 4..at + 8]).into_owned(),
                body: &data[at + 8..at + size],
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

    fn be32(b: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
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
        assert_eq!(kinds(&top), ["ftyp", "moov"]);
        let moov = children(&top[1], 0);
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
        let bytes = fragment(7, &runs);
        let top = boxes(&bytes);
        assert_eq!(kinds(&top), ["moof", "mdat"]);
        let moof = children(&top[0], 0);
        assert_eq!(kinds(&moof), ["mfhd", "traf", "traf"]);
        assert_eq!(be32(moof[0].body, 4), 7, "sequence_number");
        for (traf, run) in moof[1..].iter().zip(&runs) {
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
}
