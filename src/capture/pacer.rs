//! When the video capture loop records a frame, holds one back, or sends the
//! newest one again -- the timing rules of `run_video_capture`, kept apart
//! from the capture itself so they can be tested without one.

use crate::capture::frame_rate::FrameRateLimiter;
use std::time::Duration;

/// How long the video may go without a new frame before the newest one is
/// sent again. Windows hands out a frame only when the picture changes, so a
/// still window gives one and then nothing -- and the MP4 sink, waiting on
/// the video, then throttles every audio track too (the file stays at its
/// 79-byte header) until stopping deadlocks. The audio side of the same
/// stall is `SilenceFill`'s.
pub(crate) const STILL_FRAME_REPEAT: Duration = Duration::from_millis(250);

/// What the capture loop does next.
#[derive(Debug, PartialEq)]
pub(crate) enum Step<F> {
    /// Record this frame as the frame at the given time.
    Record(F, Duration),
    /// Nothing new to record for `STILL_FRAME_REPEAT`: send the newest frame
    /// again (or a black one, before any has come) as the frame at this time.
    Repeat(Duration),
    /// Nothing to do yet.
    Wait,
}

/// Decides, for each captured frame and each turn without one, what the
/// capture loop does -- see `Step`.
///
/// A frame that comes while paused, or too soon after the last one recorded
/// (see `FrameRateLimiter`), isn't thrown away but held until its slot comes:
/// Windows hands out nothing more for a picture that has stopped changing, so
/// a dropped frame used to leave its picture -- the last keystroke on a high
/// refresh display, or whatever changed during a pause -- out of the
/// recording for good, with the older picture repeated in its place. A newer
/// frame replaces a held one.
pub(crate) struct FramePacer<F> {
    limiter: FrameRateLimiter,
    fps: u32,
    held: Option<F>,
    /// When a frame was last recorded or repeated, or capture started.
    idle_since: Duration,
}

impl<F> FramePacer<F> {
    /// Paces frames at `fps` for a capture that started running at
    /// `started`. Starting the repeat timer there, not at the recording
    /// clock's zero, gives the first frame its full `STILL_FRAME_REPEAT`
    /// however long opening the capture took.
    pub(crate) fn new(fps: u32, started: Duration) -> Self {
        Self {
            limiter: FrameRateLimiter::new(fps),
            fps,
            held: None,
            idle_since: started,
        }
    }

    /// Follows a changed fps (see `LiveFps`).
    pub(crate) fn set_fps(&mut self, fps: u32) {
        if fps != self.fps {
            self.fps = fps;
            self.limiter = FrameRateLimiter::new(fps);
        }
    }

    /// A new frame was captured at `now`.
    pub(crate) fn on_frame(&mut self, frame: F, now: Duration, paused: bool) -> Step<F> {
        if paused || !self.limiter.admit(now) {
            self.held = Some(frame);
            return Step::Wait;
        }
        self.held = None;
        self.idle_since = now;
        Step::Record(frame, now)
    }

    /// No new frame has come by `now`.
    pub(crate) fn on_idle(&mut self, now: Duration, paused: bool) -> Step<F> {
        if paused {
            return Step::Wait;
        }
        if self.held.is_some() {
            if !self.limiter.admit(now) {
                return Step::Wait;
            }
            self.idle_since = now;
            return self
                .held
                .take()
                .map_or(Step::Wait, |frame| Step::Record(frame, now));
        }
        if now.saturating_sub(self.idle_since) >= STILL_FRAME_REPEAT && self.limiter.admit(now) {
            self.idle_since = now;
            return Step::Repeat(now);
        }
        Step::Wait
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::frame_rate::FrameGrid;

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn frames_within_the_cap_are_recorded_straight_away() {
        let mut pacer = FramePacer::new(60, ms(0));
        assert_eq!(pacer.on_frame("a", ms(0), false), Step::Record("a", ms(0)));
        assert_eq!(
            pacer.on_frame("b", ms(17), false),
            Step::Record("b", ms(17))
        );
        assert_eq!(pacer.on_idle(ms(30), false), Step::Wait);
    }

    #[test]
    fn a_frame_over_the_cap_is_recorded_at_its_slot() {
        let mut pacer = FramePacer::new(60, ms(0));
        assert_eq!(pacer.on_frame("a", ms(0), false), Step::Record("a", ms(0)));
        // A 240 Hz display's next frames -- the last of them is the picture
        // the window then keeps showing.
        assert_eq!(pacer.on_frame("b", ms(3), false), Step::Wait);
        assert_eq!(pacer.on_frame("c", ms(6), false), Step::Wait);
        assert_eq!(pacer.on_idle(ms(8), false), Step::Wait);
        // Up to half a slot early counts for the slot (see `FrameRateLimiter`).
        assert_eq!(pacer.on_idle(ms(9), false), Step::Record("c", ms(9)));
        // Recorded once, then repeated like any other still picture.
        assert_eq!(pacer.on_idle(ms(34), false), Step::Wait);
        assert_eq!(pacer.on_idle(ms(259), false), Step::Repeat(ms(259)));
    }

    #[test]
    fn a_frame_recorded_in_its_slot_replaces_a_held_one() {
        let mut pacer = FramePacer::new(60, ms(0));
        pacer.on_frame("a", ms(0), false);
        assert_eq!(pacer.on_frame("b", ms(4), false), Step::Wait);
        assert_eq!(
            pacer.on_frame("c", ms(17), false),
            Step::Record("c", ms(17))
        );
        assert_eq!(pacer.on_idle(ms(34), false), Step::Wait);
    }

    #[test]
    fn a_frame_from_a_pause_is_recorded_on_resume() {
        let mut pacer = FramePacer::new(60, ms(0));
        pacer.on_frame("a", ms(0), false);
        // The clock stands still while paused.
        assert_eq!(pacer.on_frame("b", ms(100), true), Step::Wait);
        assert_eq!(pacer.on_idle(ms(100), true), Step::Wait);
        assert_eq!(pacer.on_idle(ms(100), false), Step::Record("b", ms(100)));
    }

    #[test]
    fn nothing_is_repeated_while_paused() {
        let mut pacer = FramePacer::<&str>::new(60, ms(0));
        assert_eq!(pacer.on_idle(ms(1000), true), Step::Wait);
        assert_eq!(pacer.on_idle(ms(1000), false), Step::Repeat(ms(1000)));
    }

    #[test]
    fn a_still_picture_is_repeated_every_still_frame_repeat() {
        let mut pacer = FramePacer::new(30, ms(0));
        pacer.on_frame("a", ms(0), false);
        assert_eq!(pacer.on_idle(ms(249), false), Step::Wait);
        assert_eq!(pacer.on_idle(ms(250), false), Step::Repeat(ms(250)));
        assert_eq!(pacer.on_idle(ms(499), false), Step::Wait);
        assert_eq!(pacer.on_idle(ms(500), false), Step::Repeat(ms(500)));
    }

    #[test]
    fn the_first_repeat_waits_from_when_capture_started() {
        // Opening the capture took 400 ms of the recording's clock.
        let mut pacer = FramePacer::<&str>::new(60, ms(400));
        assert_eq!(pacer.on_idle(ms(400), false), Step::Wait);
        assert_eq!(pacer.on_idle(ms(649), false), Step::Wait);
        assert_eq!(pacer.on_idle(ms(650), false), Step::Repeat(ms(650)));
    }

    #[test]
    fn the_first_frame_survives_a_black_frame_just_before_it() {
        // A window's first frame came just after the black one sent while
        // waiting for it. It used to be dropped as too soon after the black
        // frame, and a still window then recorded black throughout.
        let mut pacer = FramePacer::new(60, ms(0));
        assert_eq!(pacer.on_idle(ms(250), false), Step::Repeat(ms(250)));
        assert_eq!(pacer.on_frame("first", ms(255), false), Step::Wait);
        assert_eq!(
            pacer.on_idle(ms(267), false),
            Step::Record("first", ms(267))
        );
    }

    #[test]
    fn a_new_fps_takes_effect_at_once() {
        let mut pacer = FramePacer::new(60, ms(0));
        pacer.on_frame("a", ms(0), false);
        pacer.set_fps(30);
        // A fresh limiter admits the next frame whenever it comes.
        assert_eq!(pacer.on_frame("b", ms(5), false), Step::Record("b", ms(5)));
        // 30 fps slots start at 0, 33.3 and 66.7 ms.
        assert_eq!(pacer.on_frame("c", ms(10), false), Step::Wait);
        assert_eq!(pacer.on_idle(ms(16), false), Step::Wait);
        assert_eq!(pacer.on_idle(ms(17), false), Step::Record("c", ms(17)));
    }

    #[test]
    fn a_source_on_the_slot_edges_loses_no_frames() {
        // A 60 Hz source whose frames land on the 60 fps slots' edges,
        // jittering either side of them, between the capture loop's turns
        // a millisecond apart. A frame that shares its nearest slot with
        // the one before goes in the next slot instead of being lost.
        let grid = FrameGrid::new(60);
        let interval = Duration::from_secs(1) / 60;
        let frame_time = |i: u32| {
            interval * i + interval / 2 - Duration::from_micros(300)
                + Duration::from_micros([0, 300, 600][i as usize % 3])
        };
        let mut pacer = FramePacer::new(60, ms(0));
        let mut recorded = Vec::new();
        let mut next = 0;
        for tick in 0..10_000 {
            let now = ms(tick);
            let step = if next < 600 && frame_time(next) <= now {
                next += 1;
                pacer.on_frame(next - 1, frame_time(next - 1), false)
            } else {
                pacer.on_idle(now, false)
            };
            if let Step::Record(frame, t) = step {
                recorded.push((frame, grid.slot(t)));
            }
        }
        let frames: Vec<u32> = recorded.iter().map(|&(frame, _)| frame).collect();
        assert_eq!(frames, (0..600).collect::<Vec<_>>());
        assert!(
            recorded.windows(2).all(|pair| pair[0].1 < pair[1].1),
            "{recorded:?}"
        );
    }
}
