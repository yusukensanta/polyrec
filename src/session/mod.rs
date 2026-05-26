pub mod clock;
pub mod state;

use crate::types::SessionState;
use state::{transition, SessionAction};

pub struct SessionManager {
    state: SessionState,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            state: SessionState::Idle,
        }
    }

    pub fn state(&self) -> &SessionState {
        &self.state
    }

    pub fn apply(&mut self, action: SessionAction) -> bool {
        match transition(&self.state, &action) {
            Some(next) => {
                self.state = next;
                true
            }
            None => false,
        }
    }

    pub fn is_recording(&self) -> bool {
        matches!(self.state, SessionState::Recording)
    }

    pub fn is_idle(&self) -> bool {
        matches!(self.state, SessionState::Idle)
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_session_is_idle() {
        let sm = SessionManager::new();
        assert!(sm.is_idle());
    }

    #[test]
    fn start_transitions_to_recording() {
        let mut sm = SessionManager::new();
        assert!(sm.apply(SessionAction::Start));
        assert!(sm.is_recording());
    }

    #[test]
    fn illegal_action_returns_false() {
        let mut sm = SessionManager::new();
        assert!(!sm.apply(SessionAction::Stop));
        assert!(sm.is_idle());
    }
}
