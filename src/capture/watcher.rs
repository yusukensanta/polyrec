// Stub — implemented in Task 4
pub struct SessionWatcher;

impl SessionWatcher {
    pub fn new() -> Self {
        Self
    }

    pub fn start(&self) {}
}

impl Default for SessionWatcher {
    fn default() -> Self {
        Self::new()
    }
}
