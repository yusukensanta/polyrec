//! Where each frame goes in a recording's constant-frame-rate video.

use crate::capture::frame_rate::FrameGrid;
use std::ops::Range;
use std::time::Duration;

/// Places a file's frames on its `FrameGrid`, so its video runs at exactly
/// its fps from its first instant to its last.
///
/// Each frame takes the slot whose start is nearest the time it was
/// captured, and every slot before it that no frame took repeats the picture
/// before it -- or, before the first frame, the first frame itself. Windows
/// hands out a frame only when the picture changes, so a game that drops a
/// frame or a still window leaves slots no frame takes; their times used to
/// be left to the frames around them, and players took the result for a
/// 59.94 fps video.
#[derive(Debug)]
pub(crate) struct CfrTimeline {
    grid: FrameGrid,
    /// The first slot no frame has taken yet.
    next: u64,
    stats: CfrStats,
}

/// What a `CfrTimeline` has placed so far: how steady the video's source
/// was. Repeats are slots the source gave no new picture for -- a still
/// window, or a frame the game (or the capture) missed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CfrStats {
    /// Slots in the video so far.
    pub slots: u64,
    /// Slots before the first frame, filled with it.
    pub lead_in: u64,
    /// Slots after the first frame no frame took, repeating the one before.
    pub repeats: u64,
    /// Frames dropped as too late for any slot (see `CfrTimeline::place`).
    pub dropped: u64,
}

impl std::fmt::Display for CfrStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pct = |n: u64| 100.0 * n as f64 / self.slots.max(1) as f64;
        write!(
            f,
            "{} frames, {} ({:.2}%) repeating the one before, {} lead-in, {} dropped",
            self.slots,
            self.repeats,
            pct(self.repeats),
            self.lead_in,
            self.dropped
        )
    }
}

/// Where `CfrTimeline::place` puts a frame.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Placement {
    /// The slots the picture before this frame (or, at the start, this
    /// frame's own) still fills.
    pub fill: Range<u64>,
    /// This frame's own slot.
    pub slot: u64,
}

impl CfrTimeline {
    pub(crate) fn new(fps: u32) -> Self {
        Self {
            grid: FrameGrid::new(fps),
            next: 0,
            stats: CfrStats::default(),
        }
    }

    pub(crate) fn stats(&self) -> CfrStats {
        self.stats
    }

    pub(crate) fn grid(&self) -> FrameGrid {
        self.grid
    }

    /// Places the frame captured at `pts` (which doesn't go backwards from
    /// one frame to the next). A frame whose nearest slot is already taken
    /// goes in the next one, a slot late -- the capture loop records at most
    /// one frame per slot (see `FramePacer`), so only a frame whose time was
    /// moved can land there. One that would go later still is dropped
    /// (None), rather than putting the video behind the sound for good.
    pub(crate) fn place(&mut self, pts: Duration) -> Option<Placement> {
        let nearest = self.grid.slot(pts);
        if nearest + 1 < self.next {
            self.stats.dropped += 1;
            return None;
        }
        let slot = nearest.max(self.next);
        let fill = self.next..slot;
        if self.next == 0 {
            self.stats.lead_in = slot;
        } else {
            self.stats.repeats += slot - self.next;
        }
        self.next = slot + 1;
        self.stats.slots = self.next;
        Some(Placement { fill, slot })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    fn at(fill: Range<u64>, slot: u64) -> Option<Placement> {
        Some(Placement { fill, slot })
    }

    #[test]
    fn frames_on_time_take_their_own_slots() {
        let mut timeline = CfrTimeline::new(60);
        assert_eq!(timeline.place(ms(0)), at(0..0, 0));
        assert_eq!(timeline.place(ms(17)), at(1..1, 1));
        assert_eq!(timeline.place(ms(33)), at(2..2, 2));
    }

    #[test]
    fn slots_no_frame_took_repeat_the_picture_before() {
        let mut timeline = CfrTimeline::new(60);
        assert_eq!(timeline.place(ms(0)), at(0..0, 0));
        // A still window's picture, sent again after 250 ms.
        assert_eq!(timeline.place(ms(250)), at(1..15, 15));
    }

    #[test]
    fn the_first_frame_fills_the_slots_from_the_start() {
        // Opening the capture took 400 ms of the recording.
        let mut timeline = CfrTimeline::new(60);
        assert_eq!(timeline.place(ms(400)), at(0..24, 24));
    }

    #[test]
    fn a_frame_a_slot_late_takes_the_next_free_one() {
        // Two frames nearest one slot: a highlight segment's times, moved to
        // start from its first sound, can do that.
        let mut timeline = CfrTimeline::new(60);
        assert_eq!(timeline.place(ms(0)), at(0..0, 0));
        assert_eq!(timeline.place(ms(5)), at(1..1, 1));
        assert_eq!(timeline.place(ms(20)), at(2..2, 2));
        assert_eq!(timeline.place(ms(50)), at(3..3, 3));
    }

    #[test]
    fn a_frame_more_than_a_slot_late_is_dropped() {
        // Video can't run behind the sound for good: a frame is never put
        // more than a slot after its own.
        let mut timeline = CfrTimeline::new(60);
        assert_eq!(timeline.place(ms(0)), at(0..0, 0));
        assert_eq!(timeline.place(ms(1)), at(1..1, 1));
        assert_eq!(timeline.place(ms(2)), None);
        assert_eq!(timeline.place(ms(33)), at(2..2, 2));
    }

    #[test]
    fn stats_count_the_slots_no_frame_took() {
        let mut timeline = CfrTimeline::new(60);
        timeline.place(ms(400)); // slots 0-23 lead in, the frame in 24
        timeline.place(ms(450)); // 25 and 26 repeat, the frame in 27
        timeline.place(ms(451)); // a slot late, in 28
        timeline.place(ms(452)); // dropped
        assert_eq!(
            timeline.stats(),
            CfrStats {
                slots: 29,
                lead_in: 24,
                repeats: 2,
                dropped: 1,
            }
        );
    }

    #[test]
    fn every_slot_is_written_once_over_an_hour() {
        // An hour of a jittery source that sometimes stalls: the slots
        // written are 0, 1, 2, ... with none skipped or written twice, and
        // the video ends where the last frame was captured.
        let mut timeline = CfrTimeline::new(60);
        let mut written = 0;
        let mut t = Duration::ZERO;
        let mut i = 0u64;
        while t < Duration::from_secs(3600) {
            let place = timeline.place(t).expect("no frame is late here");
            assert_eq!(place.fill.start, written, "at {t:?}");
            assert_eq!(place.slot, place.fill.end, "at {t:?}");
            written = place.slot + 1;
            i += 1;
            // ~60 Hz with up to 3 ms of jitter, and a 300 ms stall now and then.
            t += Duration::from_micros(16_667 + [0, 3_000, 0, 1_500][i as usize % 4])
                - Duration::from_micros([1_500, 0, 3_000, 0][i as usize % 4]);
            if i.is_multiple_of(997) {
                t += ms(300);
            }
        }
        let last = timeline.grid().slot(t) as i64;
        assert!(
            (written as i64 - last).abs() <= 1,
            "wrote {written} slots for {last}"
        );
    }
}
