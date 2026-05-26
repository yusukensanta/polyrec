use std::sync::Arc;
use std::time::Duration;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

pub struct RecordingClock {
    start_ticks: i64,
    frequency: i64,
}

impl RecordingClock {
    pub fn new() -> Arc<Self> {
        let mut frequency = 0i64;
        let mut ticks = 0i64;
        unsafe {
            QueryPerformanceFrequency(&mut frequency).unwrap();
            QueryPerformanceCounter(&mut ticks).unwrap();
        }
        Arc::new(Self {
            start_ticks: ticks,
            frequency,
        })
    }

    pub fn elapsed(&self) -> Duration {
        let mut now = 0i64;
        unsafe {
            QueryPerformanceCounter(&mut now).unwrap();
        }
        let ticks = now - self.start_ticks;
        let nanos = (ticks as u128 * 1_000_000_000) / self.frequency as u128;
        Duration::from_nanos(nanos as u64)
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
        assert!(elapsed.as_millis() < 10, "elapsed should be < 10ms right after creation");
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
        let diff = if t1 > t2 { t1 - t2 } else { t2 - t1 };
        assert!(diff.as_micros() < 100, "two reads of same clock should be within 100µs");
    }
}
