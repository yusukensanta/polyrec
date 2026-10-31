//! Starting PolyRec again after it had to close on its own: the graphics
//! device was lost (a driver update or reset, or a game that crashed the
//! GPU), or something panicked on the dashboard's thread. A recording that
//! was running is saved on the way out, as on any exit (see the end of
//! `main`); the new copy brings the tray icon and the hotkeys back, so
//! someone in a game doesn't find PolyRec quietly gone.

use std::time::Duration;

/// What the new copy is told about why it was started -- see `Reason::arg`.
const ARG_PREFIX: &str = "--restarted=";

/// A copy that was itself restarted and has to close again this soon
/// doesn't start another, so something that fails on every start can't
/// loop.
const QUICK_FAILURE: Duration = Duration::from_secs(60);

/// Why PolyRec closed on its own, for the notice the new copy shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The graphics device was lost.
    Graphics,
    /// A panic on the dashboard's thread (see `crash_log`).
    Error,
}

impl Reason {
    fn arg(self) -> String {
        let name = match self {
            Self::Graphics => "graphics",
            Self::Error => "error",
        };
        format!("{ARG_PREFIX}{name}")
    }

    /// Why this copy was started in place of one that closed on its own;
    /// `None` for any other start.
    pub fn of_this_start(mut args: impl Iterator<Item = String>) -> Option<Self> {
        args.find_map(|arg| match arg.strip_prefix(ARG_PREFIX)? {
            "graphics" => Some(Self::Graphics),
            "error" => Some(Self::Error),
            _ => None,
        })
    }
}

/// Whether a copy that has to close on its own, after running for
/// `ran_for`, should start a new one -- see `QUICK_FAILURE`.
pub fn should_restart(this_was_a_restart: bool, ran_for: Duration) -> bool {
    !(this_was_a_restart && ran_for < QUICK_FAILURE)
}

/// Starts a new copy of PolyRec, told `reason`. Call it once this copy has
/// let go of its claim (`single_instance::Instance::release`).
pub fn start_new_copy(reason: Reason) {
    let started = std::env::current_exe()
        .and_then(|exe| std::process::Command::new(exe).arg(reason.arg()).spawn());
    match started {
        Ok(_) => tracing::info!("started a new PolyRec ({reason:?})"),
        Err(e) => tracing::error!("couldn't start a new PolyRec: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(args: &[&str]) -> impl Iterator<Item = String> {
        args.iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn the_reason_comes_back_from_the_new_copys_arguments() {
        for reason in [Reason::Graphics, Reason::Error] {
            assert_eq!(
                Reason::of_this_start(args(&["polyrec.exe", &reason.arg()])),
                Some(reason)
            );
        }
        assert_eq!(Reason::of_this_start(args(&["polyrec.exe"])), None);
        assert_eq!(
            Reason::of_this_start(args(&["polyrec.exe", "--restarted=other"])),
            None
        );
    }

    #[test]
    fn a_restart_that_fails_again_quickly_isnt_restarted() {
        let soon = QUICK_FAILURE - Duration::from_secs(1);
        assert!(should_restart(false, Duration::ZERO));
        assert!(should_restart(true, QUICK_FAILURE));
        assert!(!should_restart(true, soon));
    }
}
