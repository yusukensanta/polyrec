//! Interleaves the tracks' samples into fragments and writes them: the
//! init segment first, then a fragment each time the video reaches a
//! keyframe -- each written whole and flushed, so the file up to the last
//! one reads back whatever happens to the process after.

use super::boxes::{RunSample, TrackInfo, TrackKind, TrackRun, fragment, init_segment};
use std::io::{self, Write};

/// A fragment no longer than this, keyframe or not: an encoder that keeps
/// its own, longer GOP still loses at most this much in a crash.
const MAX_FRAGMENT_HNS: i64 = 20_000_000;

/// One encoded sample, as the encoders hand it over.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    /// Presentation time, in 100 ns units.
    pub time: i64,
    pub duration: i64,
    pub keyframe: bool,
    /// As the file holds it (video length-prefixed, see `nal`).
    pub data: Vec<u8>,
}

pub struct Muxer<W: Write> {
    out: W,
    tracks: Vec<TrackInfo>,
    /// The video track's index in `tracks`.
    video: Option<usize>,
    pending: Vec<Vec<Sample>>,
    sequence: u32,
    /// The last video sample's time: the samples' order is taken for their
    /// order in time, which B-frames would break (the writer makes sure the
    /// encoder uses none, as far as it can tell).
    last_video_time: Option<i64>,
    /// Whether a video sample has come before the one ahead of it in time.
    reordered: bool,
}

impl<W: Write> Muxer<W> {
    /// Writes the init segment for `tracks` to `out`.
    pub fn new(mut out: W, tracks: Vec<TrackInfo>) -> io::Result<Self> {
        out.write_all(&init_segment(&tracks))?;
        out.flush()?;
        let video = tracks
            .iter()
            .position(|t| matches!(t.kind, TrackKind::Video { .. }));
        Ok(Self {
            out,
            pending: vec![Vec::new(); tracks.len()],
            tracks,
            video,
            sequence: 0,
            last_video_time: None,
            reordered: false,
        })
    }

    /// Takes the next sample of track `track` (an index into the tracks
    /// given to `new`), writing the fragment before it first if it starts
    /// the next.
    pub fn push(&mut self, track: usize, sample: Sample) -> io::Result<()> {
        if Some(track) == self.video {
            if !self.reordered && self.last_video_time.is_some_and(|last| sample.time < last) {
                self.reordered = true;
                tracing::warn!(
                    "video frames out of time order (B-frames?): the recording's timing is off"
                );
            }
            self.last_video_time = Some(sample.time);
        }
        if Some(track) == self.video
            && let Some(first) = self.pending[track].first()
            && (sample.keyframe || sample.time - first.time >= MAX_FRAGMENT_HNS)
        {
            self.write_fragment(Some(sample.time))?;
        }
        self.pending[track].push(sample);
        Ok(())
    }

    /// Writes what's left as a last fragment.
    pub fn finish(mut self) -> io::Result<W> {
        self.write_fragment(None)?;
        Ok(self.out)
    }

    /// Writes the samples before `until` (a time; all of them without one)
    /// as the next fragment, if there are any.
    fn write_fragment(&mut self, until: Option<i64>) -> io::Result<()> {
        let mut runs = Vec::new();
        for (index, track) in self.tracks.iter().enumerate() {
            let pending = &mut self.pending[index];
            let count = until.map_or(pending.len(), |until| {
                pending.iter().take_while(|s| s.time < until).count()
            });
            if count == 0 {
                continue;
            }
            let samples: Vec<Sample> = pending.drain(..count).collect();
            // Where the last one ends: where the next one starts, when
            // that's known -- the cut, for the video -- else its own
            // duration.
            let next = pending
                .first()
                .map(|s| s.time)
                .or(until.filter(|_| Some(index) == self.video));
            runs.push(run(track, &samples, next));
        }
        if runs.is_empty() {
            return Ok(());
        }
        self.sequence += 1;
        self.out.write_all(&fragment(self.sequence, &runs))?;
        self.out.flush()
    }
}

/// `samples` of `track` as a run, the last lasting until `next` if given.
fn run(track: &TrackInfo, samples: &[Sample], next: Option<i64>) -> TrackRun {
    let ticks = |hns: i64| -> u64 {
        (i128::from(hns.max(0)) * i128::from(track.timescale) / 10_000_000) as u64
    };
    let last = &samples[samples.len() - 1];
    let ends = samples
        .iter()
        .skip(1)
        .map(|s| s.time)
        .chain([next.unwrap_or(last.time + last.duration)]);
    TrackRun {
        track_id: track.id,
        base_decode_time: ticks(samples[0].time),
        samples: samples
            .iter()
            .zip(ends)
            .map(|(s, end)| RunSample {
                duration: ticks(end).saturating_sub(ticks(s.time)) as u32,
                size: s.data.len() as u32,
                keyframe: s.keyframe,
            })
            .collect(),
        data: samples
            .iter()
            .flat_map(|s| s.data.iter().copied())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::boxes::tests::{boxes, children, kinds};
    use super::super::nal::VideoCodec;
    use super::*;

    const MS: i64 = 10_000;

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

    fn frame(ms: i64, keyframe: bool) -> Sample {
        Sample {
            time: ms * MS,
            duration: 100 * MS,
            keyframe,
            data: vec![ms as u8; 3],
        }
    }

    fn audio(ms: i64) -> Sample {
        Sample {
            time: ms * MS,
            duration: 50 * MS,
            keyframe: true,
            data: vec![0xa0; 2],
        }
    }

    /// Per fragment: (track id, base decode time, durations) of each run.
    fn runs(file: &[u8]) -> Vec<Vec<(u32, u64, Vec<u32>)>> {
        let top = boxes(file);
        top.iter()
            .filter(|b| b.kind == "moof")
            .map(|moof| {
                children(moof, 0)
                    .iter()
                    .filter(|b| b.kind == "traf")
                    .map(|traf| {
                        let parts = children(traf, 0);
                        let id = u32::from_be_bytes(parts[0].body[4..8].try_into().unwrap());
                        let base = u64::from_be_bytes(parts[1].body[4..12].try_into().unwrap());
                        let trun = parts[2].body;
                        let count = u32::from_be_bytes(trun[4..8].try_into().unwrap()) as usize;
                        let durations = (0..count)
                            .map(|i| {
                                let at = 12 + i * 12;
                                u32::from_be_bytes(trun[at..at + 4].try_into().unwrap())
                            })
                            .collect();
                        (id, base, durations)
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_fragment_ends_where_the_next_keyframe_starts() {
        let mut muxer = Muxer::new(Vec::new(), tracks()).unwrap();
        for (ms, key) in [
            (0, true),
            (100, false),
            (200, false),
            (300, true),
            (400, false),
        ] {
            muxer.push(0, frame(ms, key)).unwrap();
            muxer.push(1, audio(ms)).unwrap();
            muxer.push(1, audio(ms + 50)).unwrap();
        }
        let file = muxer.finish().unwrap();
        assert_eq!(
            kinds(&boxes(&file)),
            ["ftyp", "moov", "moof", "mdat", "moof", "mdat"]
        );
        let runs = runs(&file);
        // Video at 90 kHz: three frames up to the keyframe at 300 ms.
        assert_eq!(runs[0][0], (1, 0, vec![9000, 9000, 9000]));
        // Audio at 48 kHz: what came before 300 ms, at 50 ms each.
        assert_eq!(runs[0][1], (2, 0, vec![2400; 6]));
        assert_eq!(runs[1][0], (1, 27_000, vec![9000, 9000]));
        assert_eq!(runs[1][1], (2, 14_400, vec![2400; 4]));
    }

    /// Each sample lasts until the next one starts, across fragments too,
    /// so the times read back are the ones written.
    #[test]
    fn times_carry_on_across_fragments() {
        let mut muxer = Muxer::new(Vec::new(), tracks()).unwrap();
        muxer.push(0, frame(0, true)).unwrap();
        muxer.push(0, frame(40, false)).unwrap();
        muxer.push(0, frame(70, true)).unwrap();
        let file = muxer.finish().unwrap();
        let runs = runs(&file);
        assert_eq!(runs[0][0], (1, 0, vec![3600, 2700]));
        // The last of all lasts its own duration.
        assert_eq!(runs[1][0], (1, 6300, vec![9000]));
    }

    #[test]
    fn a_long_gop_is_cut_anyway() {
        let mut muxer = Muxer::new(Vec::new(), tracks()).unwrap();
        for ms in (0..3000).step_by(100) {
            muxer.push(0, frame(ms, ms == 0)).unwrap();
        }
        let file = muxer.finish().unwrap();
        let runs = runs(&file);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0][0].2.len(), 20, "2 s of frames");
    }

    /// The crash case: nothing finishes. Every fragment written before is
    /// whole in the output, and the file reads to its end.
    #[test]
    fn whats_written_reads_back_without_finishing() {
        let mut muxer = Muxer::new(Vec::new(), tracks()).unwrap();
        for (ms, key) in [(0, true), (100, false), (200, true), (300, false)] {
            muxer.push(0, frame(ms, key)).unwrap();
        }
        let cut_off = std::mem::take(&mut muxer.out);
        drop(muxer);
        assert_eq!(kinds(&boxes(&cut_off)), ["ftyp", "moov", "moof", "mdat"]);
        assert_eq!(runs(&cut_off)[0][0].2.len(), 2);
    }

    #[test]
    fn a_track_with_nothing_in_a_fragment_has_no_run_there() {
        let mut muxer = Muxer::new(Vec::new(), tracks()).unwrap();
        muxer.push(0, frame(0, true)).unwrap();
        muxer.push(0, frame(100, true)).unwrap();
        muxer.push(1, audio(100)).unwrap();
        let file = muxer.finish().unwrap();
        let ids: Vec<Vec<u32>> = runs(&file)
            .iter()
            .map(|f| f.iter().map(|r| r.0).collect())
            .collect();
        assert_eq!(ids, [vec![1], vec![1, 2]]);
    }
}
