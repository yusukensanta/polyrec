//! Caps captured video at the configured recording fps.
//!
//! Windows.Graphics.Capture delivers a frame every time the captured content
//! changes, up to the display's refresh rate: 144 or 240 fps on a high-refresh
//! monitor, whatever the recording's fps setting says. Every delivered frame
//! used to be read back, copied and encoded, so a 60 fps recording on a 240 Hz
//! display did about 4x the work it needed and produced a file whose frame
//! rate didn't match its own setting. `FrameRateLimiter` decides, before any
//! GPU copy, which frames to keep.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

/// The fps a recording (or Highlight buffer) runs at, shared by its writer
/// actor, its capture loop and the dashboard. It starts at the requested rate
/// (clamped to the display), and the actor lowers it if the encoder turns that
/// rate down (see `RecordingWriter::new_at_highest_fps`). The capture loop's
/// cap follows within a frame. Clones share one value.
#[derive(Debug, Clone)]
pub struct LiveFps(Arc<AtomicU32>);

impl LiveFps {
    pub fn new(fps: u32) -> Self {
        Self(Arc::new(AtomicU32::new(fps)))
    }

    pub fn get(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }

    pub fn set(&self, fps: u32) {
        self.0.store(fps, Ordering::Relaxed);
    }
}

/// The fps presets the Quality popup offers.
pub(crate) const FPS_PRESETS: [u32; 5] = [30, 60, 120, 144, 240];

/// Whether a display refreshing at `display_hz` can deliver `fps` frames per
/// second. Windows reports fractional rates rounded down (59.94 Hz as 59,
/// 239.76 Hz as 239), hence the 1 Hz of slack. An unknown rate fits anything.
pub(crate) fn fps_fits_display(fps: u32, display_hz: Option<u32>) -> bool {
    display_hz.is_none_or(|hz| fps <= hz + 1)
}

/// The fps a recording actually runs at: `requested`, unless the display
/// can't refresh that fast, in which case its refresh rate. Recording 144 fps
/// from a 60 Hz display would only repeat frames (and trip the fps-shortfall
/// warning), so it records a real 60 instead.
pub(crate) fn effective_fps(requested: u32, display_hz: Option<u32>) -> u32 {
    match display_hz {
        Some(hz) if !fps_fits_display(requested, display_hz) => hz,
        _ => requested,
    }
}

/// The highest fps every encoder handled before the presets went above it,
/// so it's used without asking the encoder first.
const ALWAYS_ENCODABLE_FPS: u32 = 60;

/// The fps to record at, starting from `wanted` (already clamped to the
/// display, see `effective_fps`): `wanted` itself if the encoder accepts it,
/// otherwise the highest lower preset it does, otherwise 60. Encoders differ:
/// H.264 is held to level 5.2's macroblock rate (1440p tops out at 144 fps,
/// 4K at 60), and the software encoders reject 240. `encoder_accepts` is only
/// asked about rates above 60.
pub(crate) fn highest_encodable_fps(
    wanted: u32,
    mut encoder_accepts: impl FnMut(u32) -> bool,
) -> u32 {
    if wanted <= ALWAYS_ENCODABLE_FPS {
        return wanted;
    }
    std::iter::once(wanted)
        .chain(
            FPS_PRESETS
                .into_iter()
                .rev()
                .filter(|&fps| fps < wanted && fps > ALWAYS_ENCODABLE_FPS),
        )
        .find(|&fps| encoder_accepts(fps))
        .unwrap_or(ALWAYS_ENCODABLE_FPS)
}

/// Admits frames at (on average, exactly) `fps` per second, judged by each
/// frame's timestamp.
///
/// Admitted frames sit on a fixed grid of `1/fps` slots anchored at the first
/// frame, so the average rate is exact whatever the source rate is: a 240 Hz
/// source keeps every 4th frame at 60 fps, a 144 Hz one alternates every 2nd
/// and 3rd. A frame up to half a slot early still counts for its slot, so a
/// source that already runs at or below `fps` (with the usual timing jitter)
/// keeps every frame. After a gap longer than a slot (static content, where
/// WGC delivers nothing), the grid re-anchors at the next frame instead of
/// admitting a burst of catch-up frames.
#[derive(Debug)]
pub(crate) struct FrameRateLimiter {
    interval: Duration,
    next_due: Option<Duration>,
}

impl FrameRateLimiter {
    /// `fps` of 0 is treated as 1.
    pub(crate) fn new(fps: u32) -> Self {
        Self {
            interval: Duration::from_secs(1) / fps.max(1),
            next_due: None,
        }
    }

    /// The length of one frame slot, `1/fps`.
    pub(crate) fn interval(&self) -> Duration {
        self.interval
    }

    /// Whether the frame captured at `t` should be recorded. `t` must not go
    /// backwards between calls.
    pub(crate) fn admit(&mut self, t: Duration) -> bool {
        let Some(due) = self.next_due else {
            self.next_due = Some(t + self.interval);
            return true;
        };
        if t + self.interval / 2 < due {
            return false;
        }
        self.next_due = Some(if t >= due + self.interval {
            t + self.interval
        } else {
            due + self.interval
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds `frames` timestamps spaced `period` apart (offset by `jitter(i)`)
    /// through a limiter and returns the admitted timestamps.
    fn admitted(
        fps: u32,
        period: Duration,
        frames: u32,
        jitter: impl Fn(u32) -> Duration,
    ) -> Vec<Duration> {
        let mut limiter = FrameRateLimiter::new(fps);
        (0..frames)
            .map(|i| period * i + jitter(i))
            .filter(|&t| limiter.admit(t))
            .collect()
    }

    fn hz(rate: u32) -> Duration {
        Duration::from_secs(1) / rate
    }

    #[test]
    fn a_faster_source_is_cut_down_to_exactly_fps() {
        for source_hz in [240, 165, 144, 120, 75] {
            // 10 s of source frames at 60 fps -> 600 slots.
            let kept = admitted(60, hz(source_hz), source_hz * 10, |_| Duration::ZERO);
            assert!(
                (599..=601).contains(&kept.len()),
                "{source_hz} Hz source kept {} frames in 10 s, expected about 600",
                kept.len()
            );
        }
    }

    #[test]
    fn a_source_at_or_below_fps_keeps_every_frame_despite_jitter() {
        // +-2 ms jitter on a 60 Hz source: no frame may be dropped as "early".
        let jitter = |i: u32| Duration::from_millis(if i.is_multiple_of(2) { 2 } else { 0 });
        assert_eq!(admitted(60, hz(60), 600, jitter).len(), 600);
        assert_eq!(admitted(60, hz(30), 300, |_| Duration::ZERO).len(), 300);
    }

    #[test]
    fn kept_frames_are_never_closer_than_half_a_slot() {
        let kept = admitted(60, hz(240), 2400, |_| Duration::ZERO);
        for pair in kept.windows(2) {
            assert!(pair[1] - pair[0] >= hz(60) / 2, "{pair:?}");
        }
    }

    #[test]
    fn a_gap_re_anchors_the_grid_instead_of_bursting() {
        let mut limiter = FrameRateLimiter::new(60);
        let mut kept = Vec::new();
        // 1 s of 240 Hz, 5 s of nothing (static content), 1 s of 240 Hz.
        let times = (0..240)
            .map(|i| hz(240) * i)
            .chain((0..240).map(|i| Duration::from_secs(6) + hz(240) * i));
        for t in times {
            if limiter.admit(t) {
                kept.push(t);
            }
        }
        // 60-61 per 1 s burst: the last frame of a burst can be up to half a
        // slot early for the next one. A catch-up burst would add dozens.
        assert!((120..=122).contains(&kept.len()), "kept {}", kept.len());
        let resumed: Vec<_> = kept
            .iter()
            .filter(|&&t| t >= Duration::from_secs(6))
            .collect();
        assert_eq!(*resumed[0], Duration::from_secs(6));
        assert!(*resumed[1] - *resumed[0] >= hz(60) / 2);
    }

    #[test]
    fn fps_fits_a_display_up_to_its_rounded_down_refresh_rate() {
        assert!(fps_fits_display(240, Some(239)));
        assert!(fps_fits_display(60, Some(59)));
        assert!(fps_fits_display(144, Some(144)));
        assert!(!fps_fits_display(144, Some(60)));
        assert!(!fps_fits_display(120, Some(75)));
        assert!(fps_fits_display(240, None));
    }

    #[test]
    fn effective_fps_drops_to_the_display_rate_only_when_it_must() {
        assert_eq!(effective_fps(60, Some(239)), 60);
        assert_eq!(effective_fps(240, Some(239)), 240);
        assert_eq!(effective_fps(60, Some(59)), 60);
        assert_eq!(effective_fps(144, Some(60)), 60);
        assert_eq!(effective_fps(120, Some(75)), 75);
        assert_eq!(effective_fps(144, None), 144);
    }

    /// Runs `highest_encodable_fps` against a fake encoder that accepts up
    /// to `max`, returning the result and every rate it was asked about.
    fn step_down(wanted: u32, max: u32) -> (u32, Vec<u32>) {
        let mut asked = Vec::new();
        let fps = highest_encodable_fps(wanted, |fps| {
            asked.push(fps);
            fps <= max
        });
        (fps, asked)
    }

    #[test]
    fn encodable_fps_steps_down_through_the_presets() {
        assert_eq!(step_down(240, 144), (144, vec![240, 144]));
        assert_eq!(step_down(240, 240), (240, vec![240]));
        // A display-clamped rate that isn't a preset is tried first.
        assert_eq!(step_down(165, 150), (144, vec![165, 144]));
        // Nothing above 60 accepted (4K H.264): falls back to 60.
        assert_eq!(step_down(144, 64), (60, vec![144, 120]));
    }

    #[test]
    fn encodable_fps_never_asks_about_60_or_below() {
        assert_eq!(step_down(60, 0), (60, vec![]));
        assert_eq!(step_down(30, 0), (30, vec![]));
    }

    #[test]
    fn zero_fps_is_treated_as_one() {
        let limiter = FrameRateLimiter::new(0);
        assert_eq!(limiter.interval(), Duration::from_secs(1));
    }
}
