use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

pub struct RecordingClock {
    start_ticks: i64,
    frequency: i64,
    /// Total nanoseconds spent paused so far.
    accumulated_paused_nanos: AtomicU64,
    /// QPC ticks at the moment pause() was called; 0 means not paused.
    pause_tick: AtomicI64,
}

impl RecordingClock {
    pub fn new() -> Arc<Self> {
        let mut frequency = 0i64;
        let mut ticks = 0i64;
        unsafe {
            QueryPerformanceFrequency(&mut frequency)
                .expect("QueryPerformanceFrequency failed — requires Windows XP+");
            QueryPerformanceCounter(&mut ticks)
                .expect("QueryPerformanceCounter failed — requires Windows XP+");
        }
        Arc::new(Self {
            start_ticks: ticks,
            frequency,
            accumulated_paused_nanos: AtomicU64::new(0),
            pause_tick: AtomicI64::new(0),
        })
    }

    /// Freeze the clock. Idempotent — calling twice without resume in between is harmless.
    pub fn pause(&self) {
        let mut now = 0i64;
        unsafe {
            QueryPerformanceCounter(&mut now).expect("QueryPerformanceCounter failed");
        }
        // Only store if not already paused (pause_tick == 0 means running)
        let _ = self.pause_tick.compare_exchange(0, now, Ordering::SeqCst, Ordering::SeqCst);
    }

    /// Resume the clock, accumulating the paused duration. Idempotent.
    pub fn resume(&self) {
        let paused_at = self.pause_tick.swap(0, Ordering::SeqCst);
        if paused_at != 0 {
            let mut now = 0i64;
            unsafe {
                QueryPerformanceCounter(&mut now).expect("QueryPerformanceCounter failed");
            }
            let paused_ticks = now - paused_at;
            let paused_nanos =
                (paused_ticks as u128 * 1_000_000_000 / self.frequency as u128) as u64;
            self.accumulated_paused_nanos
                .fetch_add(paused_nanos, Ordering::SeqCst);
        }
    }

    /// Elapsed recording time, excluding any paused periods.
    pub fn elapsed(&self) -> Duration {
        let mut now = 0i64;
        unsafe {
            QueryPerformanceCounter(&mut now).expect("QueryPerformanceCounter failed");
        }
        let pt = self.pause_tick.load(Ordering::SeqCst);
        // Freeze at the tick when pause() was called; advance normally otherwise.
        let running_ticks = if pt > 0 {
            pt - self.start_ticks
        } else {
            now - self.start_ticks
        };
        let total_nanos =
            (running_ticks as u128 * 1_000_000_000) / self.frequency as u128;
        let paused_nanos = self.accumulated_paused_nanos.load(Ordering::SeqCst) as u128;
        Duration::from_nanos((total_nanos.saturating_sub(paused_nanos)) as u64)
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
        assert!(elapsed.as_millis() < 100, "elapsed should be < 100ms right after creation");
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
        assert!(diff.as_micros() < 1000, "two reads of same clock should be within 1ms");
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
        assert!(diff < 5, "elapsed should not advance while paused, advanced by {diff}ms");
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
