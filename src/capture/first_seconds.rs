//! How the first seconds of a capture went, for the log: whether the game
//! stuttered as the recording started shows there, next to the same
//! figures for the whole recording (see `run_video_capture`).

use crate::capture::pacer::PacerStats;
use crate::capture::video::GpuWaits;
use std::time::Duration;

/// How many seconds `FirstSeconds` watches a capture for.
const WATCHED_SECONDS: usize = 5;

/// Watches the first `WATCHED_SECONDS` of a capture, then sums them up once.
pub(crate) struct FirstSeconds {
    /// When the capture opened, on the recording's clock.
    opened: Duration,
    /// Frames from Windows by the end of each whole second so far.
    delivered_by: Vec<u64>,
    /// How long after opening the first frame from Windows came.
    first_frame: Option<Duration>,
    summed_up: bool,
}

impl FirstSeconds {
    /// Watches a capture that opened at `opened` on the recording's clock.
    pub(crate) fn new(opened: Duration) -> Self {
        Self {
            opened,
            delivered_by: Vec::with_capacity(WATCHED_SECONDS),
            first_frame: None,
            summed_up: false,
        }
    }

    /// Notes how the capture stands at `now` on the recording's clock:
    /// `stats` from its `FramePacer`, and `gpu_waits` readbacks that waited
    /// on the GPU so far. Gives the summary once the watched seconds are
    /// over, and only that once.
    pub(crate) fn note(
        &mut self,
        now: Duration,
        stats: &PacerStats,
        gpu_waits: GpuWaits,
    ) -> Option<String> {
        if self.summed_up {
            return None;
        }
        let since_opened = now.saturating_sub(self.opened);
        if self.first_frame.is_none() && stats.delivered > 0 {
            self.first_frame = Some(since_opened);
        }
        while self.delivered_by.len() < WATCHED_SECONDS
            && since_opened >= Duration::from_secs(self.delivered_by.len() as u64 + 1)
        {
            self.delivered_by.push(stats.delivered);
        }
        if self.delivered_by.len() < WATCHED_SECONDS {
            return None;
        }
        self.summed_up = true;
        Some(self.summary(stats, gpu_waits))
    }

    fn summary(&self, stats: &PacerStats, gpu_waits: GpuWaits) -> String {
        let mut before = 0;
        let per_second: Vec<String> = self
            .delivered_by
            .iter()
            .map(|&by| (by - std::mem::replace(&mut before, by)).to_string())
            .collect();
        let first = match self.first_frame {
            Some(after) => format!("the first {after:?} after it opened"),
            None => "none at all".to_owned(),
        };
        format!(
            "first {WATCHED_SECONDS} s of capture: {} frames from Windows each second, {first}; \
             the picture stood still for more than a slot {} times (longest gap {:?}); \
             {} turns of the loop more than a slot after the one before (longest {:?}); \
             {gpu_waits}",
            per_second.join(", "),
            stats.long_gaps,
            stats.longest_gap,
            stats.late_turns,
            stats.longest_turn,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn sums_up_the_first_seconds_once_they_are_over() {
        // Opening the capture took 400 ms of the recording's clock.
        let mut watch = FirstSeconds::new(ms(400));
        let mut stats = PacerStats::default();
        assert_eq!(watch.note(ms(400), &stats, GpuWaits::default()), None);
        stats.delivered = 1;
        assert_eq!(watch.note(ms(412), &stats, GpuWaits::default()), None);
        for (at, delivered) in [(1400, 118), (2400, 238), (3400, 300), (4400, 420)] {
            stats.delivered = delivered;
            assert_eq!(watch.note(ms(at), &stats, GpuWaits::default()), None);
        }
        stats.delivered = 540;
        stats.long_gaps = 2;
        stats.longest_gap = ms(45);
        stats.late_turns = 1;
        stats.longest_turn = ms(20);
        let waits = GpuWaits {
            count: 3,
            total: ms(21),
            longest: ms(9),
        };
        assert_eq!(
            watch.note(ms(5400), &stats, waits).as_deref(),
            Some(
                "first 5 s of capture: 118, 120, 62, 120, 120 frames from Windows each second, \
                 the first 12ms after it opened; \
                 the picture stood still for more than a slot 2 times (longest gap 45ms); \
                 1 turns of the loop more than a slot after the one before (longest 20ms); \
                 3 readbacks waited on the GPU (21ms in all, longest 9ms)"
            )
        );
        assert_eq!(watch.note(ms(6400), &stats, waits), None);
    }

    #[test]
    fn a_capture_with_no_frames_says_so() {
        // A minimized window gives none.
        let mut watch = FirstSeconds::new(ms(0));
        let summary = watch
            .note(ms(5000), &PacerStats::default(), GpuWaits::default())
            .unwrap();
        assert!(
            summary.starts_with(
                "first 5 s of capture: 0, 0, 0, 0, 0 frames from Windows each second, none at all;"
            ),
            "{summary}"
        );
    }

    #[test]
    fn a_loop_that_stalled_past_a_second_still_counts_every_second() {
        let mut watch = FirstSeconds::new(ms(0));
        let mut stats = PacerStats {
            delivered: 100,
            ..PacerStats::default()
        };
        assert_eq!(watch.note(ms(900), &stats, GpuWaits::default()), None);
        // Nothing turned for 1.6 s: the frames since go to the first second
        // it notices, and the one it slept through gets none.
        stats.delivered = 150;
        assert_eq!(watch.note(ms(2500), &stats, GpuWaits::default()), None);
        stats.delivered = 400;
        let summary = watch.note(ms(5000), &stats, GpuWaits::default()).unwrap();
        assert!(
            summary.starts_with("first 5 s of capture: 150, 0, 250, 0, 0 frames"),
            "{summary}"
        );
    }
}
