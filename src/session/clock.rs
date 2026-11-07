use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The recording's clock: time since it started, less the time spent
/// paused. Capture threads stamp video frames and audio with its readings
/// while the UI pauses and resumes it, so the pause state is read and
/// changed under one lock: a reading racing `resume()` can't see the
/// pause ended but not yet subtracted, and jump ahead by its length.
/// (`Instant` is the performance counter on Windows.)
pub struct RecordingClock {
    started: Instant,
    paused: Mutex<Paused>,
}

#[derive(Default)]
struct Paused {
    /// When `pause()` was called; `None` while running.
    since: Option<Instant>,
    /// Time spent paused before that.
    total: Duration,
}

impl RecordingClock {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            started: Instant::now(),
            paused: Mutex::default(),
        })
    }

    fn paused(&self) -> MutexGuard<'_, Paused> {
        self.paused.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Freeze the clock. Idempotent — calling twice without resume in between is harmless.
    pub fn pause(&self) {
        self.paused().since.get_or_insert_with(Instant::now);
    }

    /// Resume the clock, accumulating the paused duration. Idempotent.
    pub fn resume(&self) {
        let mut paused = self.paused();
        if let Some(since) = paused.since.take() {
            paused.total += since.elapsed();
        }
    }

    /// Elapsed recording time, excluding any paused periods.
    pub fn elapsed(&self) -> Duration {
        let paused = self.paused();
        // Frozen at the moment pause() was called; advancing otherwise.
        let now = paused.since.unwrap_or_else(Instant::now);
        now.duration_since(self.started)
            .saturating_sub(paused.total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn clock_starts_near_zero() {
        let clock = RecordingClock::new();
        let elapsed = clock.elapsed();
        assert!(
            elapsed.as_millis() < 100,
            "elapsed should be < 100ms right after creation"
        );
    }

    #[test]
    fn clock_advances_monotonically() {
        let clock = RecordingClock::new();
        let t1 = clock.elapsed();
        thread::sleep(Duration::from_millis(50));
        let t2 = clock.elapsed();
        assert!(t2 > t1, "clock must advance");
        assert!(t2.as_millis() >= 50, "at least 50ms should have elapsed");
    }

    #[test]
    fn shared_clock_reads_same_reference() {
        let clock = RecordingClock::new();
        let clone = Arc::clone(&clock);
        thread::sleep(Duration::from_millis(10));
        let t1 = clock.elapsed();
        let t2 = clone.elapsed();
        let diff = t1.abs_diff(t2);
        assert!(
            diff.as_micros() < 1000,
            "two reads of same clock should be within 1ms"
        );
    }

    #[test]
    fn elapsed_freezes_while_paused() {
        let clock = RecordingClock::new();
        thread::sleep(Duration::from_millis(20));
        clock.pause();
        let t1 = clock.elapsed();
        thread::sleep(Duration::from_millis(60));
        let t2 = clock.elapsed();
        let diff = if t2 > t1 { (t2 - t1).as_millis() } else { 0 };
        assert!(
            diff < 5,
            "elapsed should not advance while paused, advanced by {diff}ms"
        );
    }

    /// A reader racing `resume()` must never see the clock jump ahead by the pause length and
    /// then fall back: capture threads stamp frames and audio with these readings.
    #[test]
    fn elapsed_stays_monotonic_while_resuming() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let clock = RecordingClock::new();
        let done = Arc::new(AtomicBool::new(false));
        let reader = {
            let clock = Arc::clone(&clock);
            let done = Arc::clone(&done);
            thread::spawn(move || {
                let mut last = clock.elapsed();
                while !done.load(Ordering::Relaxed) {
                    let now = clock.elapsed();
                    if now < last {
                        return Some((last, now));
                    }
                    last = now;
                }
                None
            })
        };
        for _ in 0..300 {
            clock.pause();
            thread::sleep(Duration::from_millis(1));
            clock.resume();
        }
        done.store(true, Ordering::Relaxed);
        if let Some((before, after)) = reader.join().unwrap() {
            panic!("elapsed went back from {before:?} to {after:?}");
        }
    }

    #[test]
    fn elapsed_excludes_paused_time() {
        let clock = RecordingClock::new();
        thread::sleep(Duration::from_millis(20));
        clock.pause();
        thread::sleep(Duration::from_millis(80));
        clock.resume();
        thread::sleep(Duration::from_millis(20));
        let elapsed = clock.elapsed();
        assert!(
            elapsed.as_millis() < 70,
            "elapsed should exclude paused 80ms, got {}ms",
            elapsed.as_millis()
        );
        assert!(
            elapsed.as_millis() >= 30,
            "elapsed should include ~40ms active time, got {}ms",
            elapsed.as_millis()
        );
    }
}
