use crate::types::SessionState;

/// Returns the valid next state, or None if the transition is illegal.
pub fn transition(current: &SessionState, action: &SessionAction) -> Option<SessionState> {
    match (current, action) {
        (SessionState::Idle, SessionAction::Start) => Some(SessionState::Recording),
        (SessionState::Recording, SessionAction::Pause) => Some(SessionState::Paused),
        (SessionState::Paused, SessionAction::Resume) => Some(SessionState::Recording),
        (SessionState::Recording, SessionAction::Stop) => Some(SessionState::Idle),
        (SessionState::Paused, SessionAction::Stop) => Some(SessionState::Idle),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionAction {
    Start,
    Pause,
    Resume,
    Stop,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SessionState;

    #[test]
    fn idle_to_recording_on_start() {
        assert_eq!(transition(&SessionState::Idle, &SessionAction::Start), Some(SessionState::Recording));
    }

    #[test]
    fn recording_to_paused_on_pause() {
        assert_eq!(transition(&SessionState::Recording, &SessionAction::Pause), Some(SessionState::Paused));
    }

    #[test]
    fn paused_to_recording_on_resume() {
        assert_eq!(transition(&SessionState::Paused, &SessionAction::Resume), Some(SessionState::Recording));
    }

    #[test]
    fn recording_to_idle_on_stop() {
        assert_eq!(transition(&SessionState::Recording, &SessionAction::Stop), Some(SessionState::Idle));
    }

    #[test]
    fn paused_to_idle_on_stop() {
        assert_eq!(transition(&SessionState::Paused, &SessionAction::Stop), Some(SessionState::Idle));
    }

    #[test]
    fn illegal_transition_returns_none() {
        assert_eq!(transition(&SessionState::Idle, &SessionAction::Stop), None);
    }

    #[test]
    fn cannot_start_from_recording() {
        assert_eq!(transition(&SessionState::Recording, &SessionAction::Start), None);
    }
}
