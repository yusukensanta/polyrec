//! Which monitor PolyRec's on-screen displays (the overlay HUD, the hotkey
//! toast) appear on. Normally the one being played or recorded on -- but not
//! over a game in exclusive full-screen: any window shown over one, even one
//! that never takes focus, knocks it out of full-screen, and the player has
//! to click back into the game. Windows holds its own notifications back in
//! that state for the same reason.

use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MonitorFromWindow,
};
use windows::Win32::UI::Shell::SHQueryUserNotificationState;
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
use windows::core::BOOL;

/// A monitor, as its raw handle (`HMONITOR`).
type MonitorId = isize;

/// Where a display that would go on `preferred` goes: there, unless a game
/// in exclusive full-screen is in front on it (`exclusive_game_on`); then
/// the dashboard's monitor if that's another one, else any other monitor;
/// `None` with nowhere else to go.
fn hud_monitor(
    preferred: MonitorId,
    exclusive_game_on: Option<MonitorId>,
    dashboard: Option<MonitorId>,
    monitors: &[MonitorId],
) -> Option<MonitorId> {
    if exclusive_game_on != Some(preferred) {
        return Some(preferred);
    }
    dashboard
        .filter(|&m| m != preferred)
        .or_else(|| monitors.iter().copied().find(|&m| m != preferred))
}

/// Whether `SHQueryUserNotificationState` says a Direct3D game is in
/// exclusive full-screen (`QUNS_RUNNING_D3D_FULL_SCREEN`). A borderless
/// full-screen window (`QUNS_BUSY`) isn't disturbed by a window over it.
fn is_exclusive_fullscreen(state: i32) -> bool {
    state == windows::Win32::UI::Shell::QUNS_RUNNING_D3D_FULL_SCREEN.0
}

/// The dashboard's window, for "the dashboard's monitor" (`remember_dashboard`).
static DASHBOARD: AtomicIsize = AtomicIsize::new(0);

/// Records the dashboard's window; called once at start-up.
pub(super) fn remember_dashboard(hwnd: HWND) {
    DASHBOARD.store(hwnd.0 as isize, Ordering::Relaxed);
}

/// The monitor the dashboard is (mostly) on, once it's been recorded.
pub(super) fn dashboard_monitor() -> Option<HMONITOR> {
    match DASHBOARD.load(Ordering::Relaxed) {
        0 => None,
        hwnd => Some(unsafe { MonitorFromWindow(HWND(hwnd as *mut _), MONITOR_DEFAULTTONEAREST) }),
    }
}

/// The monitor of the game in exclusive full-screen in front, if there is
/// one -- the foreground window's. Asked of Windows at most every
/// `CHECK_EVERY`: the overlay asks every frame.
fn exclusive_game_monitor() -> Option<MonitorId> {
    const CHECK_EVERY: Duration = Duration::from_millis(500);
    static LAST: Mutex<Option<(Instant, Option<MonitorId>)>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, monitor)) = *last
        && at.elapsed() < CHECK_EVERY
    {
        return monitor;
    }
    let exclusive = unsafe { SHQueryUserNotificationState() }
        .is_ok_and(|state| is_exclusive_fullscreen(state.0));
    let monitor = exclusive.then(|| unsafe {
        MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTONEAREST).0 as MonitorId
    });
    *last = Some((Instant::now(), monitor));
    monitor
}

/// Every monitor attached.
fn monitors() -> Vec<MonitorId> {
    unsafe extern "system" fn add(m: HMONITOR, _: HDC, _: *mut RECT, list: LPARAM) -> BOOL {
        let list = unsafe { &mut *(list.0 as *mut Vec<MonitorId>) };
        list.push(m.0 as MonitorId);
        BOOL(1)
    }
    let mut list: Vec<MonitorId> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(add), LPARAM(&raw mut list as isize));
    }
    list
}

/// The screen rectangle of the monitor a display that would go on
/// `preferred` goes on (see `hud_monitor`); `None` when it shouldn't be
/// shown at all.
pub(super) fn hud_monitor_rect(preferred: HMONITOR) -> Option<RECT> {
    let dashboard = dashboard_monitor().map(|m| m.0 as MonitorId);
    let monitor = hud_monitor(
        preferred.0 as MonitorId,
        exclusive_game_monitor(),
        dashboard,
        &monitors(),
    )?;
    crate::capture::video::monitor_rect(HMONITOR(monitor as *mut _)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAME: MonitorId = 1;
    const OTHER: MonitorId = 2;
    const THIRD: MonitorId = 3;

    #[test]
    fn a_hud_stays_where_it_was_going_unless_an_exclusive_full_screen_game_is_there() {
        assert_eq!(
            hud_monitor(GAME, None, Some(OTHER), &[GAME, OTHER]),
            Some(GAME)
        );
        // A borderless game, or an exclusive one on another monitor.
        assert_eq!(
            hud_monitor(GAME, Some(OTHER), Some(OTHER), &[GAME, OTHER]),
            Some(GAME)
        );
    }

    #[test]
    fn over_an_exclusive_full_screen_game_it_goes_to_the_dashboard_s_monitor_or_another() {
        // A window shown over one knocks it out of full-screen, and the
        // player has to click back into the game.
        assert_eq!(
            hud_monitor(GAME, Some(GAME), Some(OTHER), &[GAME, OTHER]),
            Some(OTHER)
        );
        // The dashboard is on the game's monitor too (or hidden): any other.
        assert_eq!(
            hud_monitor(GAME, Some(GAME), Some(GAME), &[GAME, THIRD]),
            Some(THIRD)
        );
        assert_eq!(
            hud_monitor(GAME, Some(GAME), None, &[GAME, THIRD]),
            Some(THIRD)
        );
        // Nowhere else: not shown at all.
        assert_eq!(hud_monitor(GAME, Some(GAME), Some(GAME), &[GAME]), None);
    }

    #[test]
    fn only_windows_exclusive_direct3d_state_counts() {
        // SHQueryUserNotificationState: 3 is QUNS_RUNNING_D3D_FULL_SCREEN,
        // what Fortnite in "Fullscreen" mode reported. 2 (QUNS_BUSY) is a
        // borderless full-screen window, which a HUD over it doesn't disturb.
        assert!(is_exclusive_fullscreen(3));
        assert!(!is_exclusive_fullscreen(2));
        assert!(!is_exclusive_fullscreen(5));
    }
}
