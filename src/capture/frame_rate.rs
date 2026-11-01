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

/// A recording's frame slots: slot `n` starts exactly `n / fps` seconds into
/// the recording. Every frame of a file sits in a slot of its own (see
/// `encode::cfr`), so its video runs at exactly `fps` from start to end --
/// players took the frames' capture times, which wander around the slots
/// with the display's timing, for a 59.94 fps video.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameGrid {
    fps: u32,
}

impl FrameGrid {
    /// `fps` of 0 is treated as 1.
    pub(crate) fn new(fps: u32) -> Self {
        Self { fps: fps.max(1) }
    }

    /// The slot a frame captured `t` into the recording belongs to: the one
    /// whose start is nearest.
    pub(crate) fn slot(self, t: Duration) -> u64 {
        let ns = t.as_nanos() * u128::from(self.fps);
        ((ns + 500_000_000) / 1_000_000_000) as u64
    }

    /// When slot `slot` starts, in Media Foundation's 100 ns units, rounded
    /// up: the file keeps times in units of `1 / (1000 * fps)` s, and a time
    /// up to 100 ns past a slot's start lands on the slot's exact unit
    /// whether Media Foundation rounds or truncates it, where one just short
    /// of it would truncate to the unit before.
    pub(crate) fn start_hns(self, slot: u64) -> i64 {
        (u128::from(slot) * 10_000_000).div_ceil(u128::from(self.fps)) as i64
    }

    /// How long slot `slot` lasts, in 100 ns units.
    pub(crate) fn duration_hns(self, slot: u64) -> i64 {
        self.start_hns(slot + 1) - self.start_hns(slot)
    }

    /// When slot `slot` starts.
    pub(crate) fn time(self, slot: u64) -> Duration {
        Duration::from_nanos(self.start_hns(slot) as u64 * 100)
    }
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

/// Admits at most one frame per slot of the recording's `FrameGrid`, judged
/// by each frame's timestamp, so the frames admitted are the ones the file
/// needs: a 240 Hz source keeps every 4th frame at 60 fps, a 144 Hz one
/// alternates every 2nd and 3rd. A frame up to half a slot early counts for
/// its slot, so a source that already runs at or below `fps` (with the usual
/// timing jitter) keeps every frame. After a gap (static content, where WGC
/// delivers nothing), the next frame takes the slot it's nearest, with no
/// burst of catch-up frames.
#[derive(Debug)]
pub(crate) struct FrameRateLimiter {
    grid: FrameGrid,
    interval: Duration,
    /// The slot of the frame admitted last.
    last_slot: Option<u64>,
}

impl FrameRateLimiter {
    /// `fps` of 0 is treated as 1.
    pub(crate) fn new(fps: u32) -> Self {
        Self {
            grid: FrameGrid::new(fps),
            interval: Duration::from_secs(1) / fps.max(1),
            last_slot: None,
        }
    }

    /// The length of one frame slot, `1/fps`.
    pub(crate) fn interval(&self) -> Duration {
        self.interval
    }

    /// Whether the frame captured at `t` should be recorded: whether its slot
    /// comes after the last admitted frame's. `t` must not go backwards
    /// between calls.
    pub(crate) fn admit(&mut self, t: Duration) -> bool {
        let slot = self.grid.slot(t);
        if self.last_slot.is_some_and(|last| slot <= last) {
            return false;
        }
        self.last_slot = Some(slot);
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

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn the_grid_is_anchored_at_the_recording_start_not_the_first_frame() {
        // 30 fps slots start at 0, 33.3 and 66.7 ms: frames at 20 and 40 ms
        // are both nearest the 33.3 ms one.
        let mut limiter = FrameRateLimiter::new(30);
        assert!(limiter.admit(ms(20)));
        assert!(!limiter.admit(ms(40)));
        assert!(limiter.admit(ms(55)));
    }

    #[test]
    fn admitted_frames_each_take_a_slot_of_their_own() {
        // A 60 Hz source whose frames land on the slots' edges, jittering
        // either side of them.
        let grid = FrameGrid::new(60);
        let jitter = |i: u32| Duration::from_micros([0, 300, 600][i as usize % 3]);
        let kept = admitted(60, hz(60), 600, |i| {
            hz(120) - Duration::from_micros(300) + jitter(i)
        });
        let slots: Vec<u64> = kept.iter().map(|&t| grid.slot(t)).collect();
        // The rest aren't lost: the capture loop holds them for the next
        // slot (see `FramePacer`).
        assert!(slots.windows(2).all(|pair| pair[0] < pair[1]), "{slots:?}");
    }

    #[test]
    fn a_frame_belongs_to_the_slot_whose_start_is_nearest() {
        let grid = FrameGrid::new(60);
        assert_eq!(grid.slot(Duration::ZERO), 0);
        assert_eq!(grid.slot(Duration::from_micros(8_333)), 0);
        assert_eq!(grid.slot(Duration::from_micros(8_334)), 1);
        assert_eq!(grid.slot(Duration::from_secs(1)), 60);
        assert_eq!(grid.slot(Duration::from_secs(10 * 3600)), 2_160_000);
    }

    #[test]
    fn slot_starts_convert_exactly_to_the_files_timescale() {
        // Media Foundation keeps a 60 fps video's times in 1/60000 s units:
        // every slot has to start on a whole 1000 of them, whether it rounds
        // or truncates the 100 ns units it's given -- one unit off either
        // way, and the file's frames are no longer all the same length.
        for fps in FPS_PRESETS.into_iter().chain([25, 50, 75, 165]) {
            let grid = FrameGrid::new(fps);
            let units_per_s = u128::from(fps) * 1000;
            for slot in (0..20_000).chain([1_000_000, 10_000_000]) {
                let hns = grid.start_hns(slot) as u128;
                let units = u128::from(slot) * 1000;
                assert_eq!(
                    hns * units_per_s / 10_000_000,
                    units,
                    "{fps} fps, slot {slot}"
                );
                assert_eq!(
                    (hns * units_per_s + 5_000_000) / 10_000_000,
                    units,
                    "{fps} fps, slot {slot}, rounded"
                );
            }
        }
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
