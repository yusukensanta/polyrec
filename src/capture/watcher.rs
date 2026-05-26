/// Monitors for new Windows audio sessions (stub — full implementation in Plan 3).
pub struct SessionWatcher;

impl SessionWatcher {
    pub fn new() -> Self {
        Self
    }

    pub fn start(&self) {
        tracing::info!("SessionWatcher: monitoring for new audio sessions (stub)");
    }
}

impl Default for SessionWatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watcher_creates_and_starts() {
        let w = SessionWatcher::new();
        w.start(); // must not panic
    }
}
