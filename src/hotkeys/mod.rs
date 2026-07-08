use std::sync::mpsc;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT, RegisterHotKey, UnregisterHotKey,
    VIRTUAL_KEY, VK_0, VK_1, VK_2, VK_3, VK_4, VK_5, VK_6, VK_7, VK_8, VK_9, VK_A, VK_B, VK_BACK,
    VK_C, VK_D, VK_DELETE, VK_DOWN, VK_E, VK_END, VK_F, VK_F1, VK_F10, VK_F11, VK_F12, VK_F13,
    VK_F14, VK_F15, VK_F16, VK_F17, VK_F18, VK_F19, VK_F2, VK_F20, VK_F21, VK_F22, VK_F23, VK_F24,
    VK_F3, VK_F4, VK_F5, VK_F6, VK_F7, VK_F8, VK_F9, VK_G, VK_H, VK_HOME, VK_I, VK_INSERT, VK_J,
    VK_K, VK_L, VK_LEFT, VK_M, VK_N, VK_NEXT, VK_O, VK_P, VK_PRIOR, VK_Q, VK_R, VK_RETURN,
    VK_RIGHT, VK_S, VK_SPACE, VK_T, VK_TAB, VK_U, VK_UP, VK_V, VK_W, VK_X, VK_Y, VK_Z,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, PeekMessageW, PM_NOREMOVE, PostThreadMessageW, TranslateMessage,
    MSG, WM_HOTKEY,
};

/// `MOD_NOREPEAT` (0x4000) — suppress auto-repeat key presses.
const MOD_NOREPEAT: HOT_KEY_MODIFIERS = HOT_KEY_MODIFIERS(0x4000);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    StartStop,
    Pause,
    ToggleOverlay,
}

pub struct HotkeyListener {
    rx: mpsc::Receiver<HotkeyEvent>,
    thread_id: u32,
    handle: std::thread::JoinHandle<()>,
}

impl HotkeyListener {
    pub fn spawn(start_stop: &str, pause: &str, toggle_overlay: &str) -> Self {
        let start_stop_hk = parse_hotkey(start_stop);
        let pause_hk = parse_hotkey(pause);
        let toggle_overlay_hk = parse_hotkey(toggle_overlay);

        let (event_tx, event_rx) = mpsc::channel::<HotkeyEvent>();
        let (id_tx, id_rx) = mpsc::channel::<u32>();

        let handle = std::thread::spawn(move || {
            let tid = unsafe { GetCurrentThreadId() };
            // Force Win32 message queue creation before signalling the ID.
            // PostThreadMessageW silently drops messages if the queue doesn't exist yet.
            unsafe {
                let mut dummy = MSG::default();
                let _ = PeekMessageW(&mut dummy, None, 0, 0, PM_NOREMOVE);
            }
            id_tx.send(tid).ok();
            run_hotkey_loop(event_tx, start_stop_hk, pause_hk, toggle_overlay_hk);
        });

        let thread_id = id_rx.recv().expect("hotkey thread panicked before sending thread ID");

        Self {
            rx: event_rx,
            thread_id,
            handle,
        }
    }

    pub fn try_recv(&self) -> Option<HotkeyEvent> {
        self.rx.try_recv().ok()
    }

    pub fn stop(self) {
        if self.thread_id != 0 {
            unsafe {
                PostThreadMessageW(self.thread_id, 0x0012u32, WPARAM(0), LPARAM(0)).ok();
            }
        }
        let _ = self.handle.join();
    }
}

/// Recognizes everything `dashboard.rs`'s hotkey-capture UI can produce as the
/// *base* key (see `egui_key_to_hotkey_string` there) plus the original
/// F1-F12 for config files written before free key capture replaced the
/// F-key-only button grid. Does not handle modifier prefixes -- see
/// [`parse_hotkey`] for the full `"CTRL+ALT+F9"`-style binding string.
fn parse_base_key(s: &str) -> Option<VIRTUAL_KEY> {
    match s.to_uppercase().as_str() {
        "F1" => Some(VK_F1),
        "F2" => Some(VK_F2),
        "F3" => Some(VK_F3),
        "F4" => Some(VK_F4),
        "F5" => Some(VK_F5),
        "F6" => Some(VK_F6),
        "F7" => Some(VK_F7),
        "F8" => Some(VK_F8),
        "F9" => Some(VK_F9),
        "F10" => Some(VK_F10),
        "F11" => Some(VK_F11),
        "F12" => Some(VK_F12),
        "F13" => Some(VK_F13),
        "F14" => Some(VK_F14),
        "F15" => Some(VK_F15),
        "F16" => Some(VK_F16),
        "F17" => Some(VK_F17),
        "F18" => Some(VK_F18),
        "F19" => Some(VK_F19),
        "F20" => Some(VK_F20),
        "F21" => Some(VK_F21),
        "F22" => Some(VK_F22),
        "F23" => Some(VK_F23),
        "F24" => Some(VK_F24),
        "A" => Some(VK_A),
        "B" => Some(VK_B),
        "C" => Some(VK_C),
        "D" => Some(VK_D),
        "E" => Some(VK_E),
        "F" => Some(VK_F),
        "G" => Some(VK_G),
        "H" => Some(VK_H),
        "I" => Some(VK_I),
        "J" => Some(VK_J),
        "K" => Some(VK_K),
        "L" => Some(VK_L),
        "M" => Some(VK_M),
        "N" => Some(VK_N),
        "O" => Some(VK_O),
        "P" => Some(VK_P),
        "Q" => Some(VK_Q),
        "R" => Some(VK_R),
        "S" => Some(VK_S),
        "T" => Some(VK_T),
        "U" => Some(VK_U),
        "V" => Some(VK_V),
        "W" => Some(VK_W),
        "X" => Some(VK_X),
        "Y" => Some(VK_Y),
        "Z" => Some(VK_Z),
        "0" => Some(VK_0),
        "1" => Some(VK_1),
        "2" => Some(VK_2),
        "3" => Some(VK_3),
        "4" => Some(VK_4),
        "5" => Some(VK_5),
        "6" => Some(VK_6),
        "7" => Some(VK_7),
        "8" => Some(VK_8),
        "9" => Some(VK_9),
        "SPACE" => Some(VK_SPACE),
        "TAB" => Some(VK_TAB),
        "ENTER" => Some(VK_RETURN),
        "BACKSPACE" => Some(VK_BACK),
        "DELETE" => Some(VK_DELETE),
        "INSERT" => Some(VK_INSERT),
        "HOME" => Some(VK_HOME),
        "END" => Some(VK_END),
        "PAGEUP" => Some(VK_PRIOR),
        "PAGEDOWN" => Some(VK_NEXT),
        "UP" => Some(VK_UP),
        "DOWN" => Some(VK_DOWN),
        "LEFT" => Some(VK_LEFT),
        "RIGHT" => Some(VK_RIGHT),
        _ => None,
    }
}

/// Parses a full binding string: zero or more `"CTRL"`/`"ALT"`/`"SHIFT"`
/// segments joined by `+`, followed by exactly one base key recognized by
/// [`parse_base_key`] (e.g. `"F9"`, `"CTRL+F9"`, `"CTRL+ALT+SHIFT+A"`).
/// Segment order doesn't matter and matching is case-insensitive. Returns
/// `None` if the base key is missing/unrecognized, or the same modifier is
/// repeated. Windows' own Super/Win key isn't supported as a modifier here --
/// egui doesn't expose it as a held-modifier flag the way Ctrl/Alt/Shift are
/// (see `egui::Modifiers`), only as a standalone key, which is a poor global
/// hotkey component anyway (Win+key combos are frequently OS-reserved).
pub fn parse_hotkey(s: &str) -> Option<(VIRTUAL_KEY, HOT_KEY_MODIFIERS)> {
    let mut parts: Vec<&str> = s.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
    let base = parts.pop()?;
    let vk = parse_base_key(base)?;

    let mut mods = HOT_KEY_MODIFIERS(0);
    for part in parts {
        let flag = match part.to_uppercase().as_str() {
            "CTRL" | "CONTROL" => MOD_CONTROL,
            "ALT" => MOD_ALT,
            "SHIFT" => MOD_SHIFT,
            _ => return None,
        };
        mods |= flag;
    }
    Some((vk, mods))
}

/// A throwaway ID for [`try_register`]'s test registration — distinct from
/// `ID_START_STOP`/`ID_PAUSE`/`ID_TOGGLE_OVERLAY` below so a probe from the UI
/// thread can never collide with a real binding's ID on the listener thread.
const ID_PROBE: i32 = 999;

/// Synchronously tests whether `vk`+`mods` can actually be registered as a
/// global hotkey right now, by registering and immediately unregistering it.
/// `RegisterHotKey` fails if the combination is already bound by another
/// running application, or if Windows itself reserves it -- both cases are
/// indistinguishable from here and both are equally "can't use this
/// combination", so this is the correct way to detect either, rather than
/// hand-maintaining a guessed list of reserved keys that may not match this
/// specific machine's actual state. Registration is per-thread, not global,
/// so this doesn't disturb the separate listener thread's own real
/// registrations in `run_hotkey_loop`.
pub fn try_register(vk: VIRTUAL_KEY, mods: HOT_KEY_MODIFIERS) -> bool {
    unsafe {
        let ok = RegisterHotKey(None, ID_PROBE, mods | MOD_NOREPEAT, vk.0 as u32).is_ok();
        if ok {
            let _ = UnregisterHotKey(None, ID_PROBE);
        }
        ok
    }
}

const ID_START_STOP: i32 = 1;
const ID_PAUSE: i32 = 2;
const ID_TOGGLE_OVERLAY: i32 = 3;

fn run_hotkey_loop(
    tx: mpsc::Sender<HotkeyEvent>,
    start_stop_hk: Option<(VIRTUAL_KEY, HOT_KEY_MODIFIERS)>,
    pause_hk: Option<(VIRTUAL_KEY, HOT_KEY_MODIFIERS)>,
    toggle_overlay_hk: Option<(VIRTUAL_KEY, HOT_KEY_MODIFIERS)>,
) {
    unsafe {
        if let Some((vk, mods)) = start_stop_hk {
            RegisterHotKey(None, ID_START_STOP, mods | MOD_NOREPEAT, vk.0 as u32).ok();
        }
        if let Some((vk, mods)) = pause_hk {
            RegisterHotKey(None, ID_PAUSE, mods | MOD_NOREPEAT, vk.0 as u32).ok();
        }
        if let Some((vk, mods)) = toggle_overlay_hk {
            RegisterHotKey(None, ID_TOGGLE_OVERLAY, mods | MOD_NOREPEAT, vk.0 as u32).ok();
        }

        let mut msg = MSG::default();
        loop {
            let ret = GetMessageW(&mut msg, None, 0, 0);
            if ret.0 <= 0 {
                break; // 0 = WM_QUIT, negative = error
            }
            if msg.message == WM_HOTKEY {
                let id = msg.wParam.0 as i32;
                let event = match id {
                    ID_START_STOP => Some(HotkeyEvent::StartStop),
                    ID_PAUSE => Some(HotkeyEvent::Pause),
                    ID_TOGGLE_OVERLAY => Some(HotkeyEvent::ToggleOverlay),
                    _ => None,
                };
                if let Some(e) = event {
                    tx.send(e).ok();
                }
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        if start_stop_hk.is_some() {
            UnregisterHotKey(None, ID_START_STOP).ok();
        }
        if pause_hk.is_some() {
            UnregisterHotKey(None, ID_PAUSE).ok();
        }
        if toggle_overlay_hk.is_some() {
            UnregisterHotKey(None, ID_TOGGLE_OVERLAY).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_base_key_known_keys() {
        // VK_F7=0x76, VK_F8=0x77, VK_F9=0x78
        assert_eq!(parse_base_key("F7").map(|v| v.0), Some(0x76u16));
        assert_eq!(parse_base_key("F8").map(|v| v.0), Some(0x77u16));
        assert_eq!(parse_base_key("F9").map(|v| v.0), Some(0x78u16));
    }

    #[test]
    fn parse_base_key_unknown_returns_none() {
        assert!(parse_base_key("F99").is_none());
        assert!(parse_base_key("").is_none());
        assert!(parse_base_key("CTRL").is_none());
    }

    #[test]
    fn parse_base_key_letters_digits_and_extended_keys() {
        // VK_A=0x41, VK_Z=0x5A, VK_0=0x30, VK_9=0x39 (ASCII-aligned by Win32 design)
        assert_eq!(parse_base_key("A").map(|v| v.0), Some(0x41));
        assert_eq!(parse_base_key("a").map(|v| v.0), Some(0x41), "case-insensitive");
        assert_eq!(parse_base_key("Z").map(|v| v.0), Some(0x5A));
        assert_eq!(parse_base_key("0").map(|v| v.0), Some(0x30));
        assert_eq!(parse_base_key("9").map(|v| v.0), Some(0x39));
        assert_eq!(parse_base_key("F24").map(|v| v.0), Some(0x87));
        assert!(parse_base_key("SPACE").is_some());
        assert!(parse_base_key("PAGEUP").is_some());
        assert!(parse_base_key("RIGHT").is_some());
    }

    #[test]
    fn parse_hotkey_bare_key_has_no_modifiers() {
        let (vk, mods) = parse_hotkey("F9").expect("F9 should parse");
        assert_eq!(vk.0, 0x78);
        assert_eq!(mods.0, 0);
    }

    #[test]
    fn parse_hotkey_single_modifier() {
        let (vk, mods) = parse_hotkey("CTRL+F9").expect("CTRL+F9 should parse");
        assert_eq!(vk.0, 0x78);
        assert_eq!(mods.0, MOD_CONTROL.0);
    }

    #[test]
    fn parse_hotkey_multiple_modifiers_any_order() {
        let (vk_a, mods_a) = parse_hotkey("CTRL+ALT+SHIFT+A").expect("should parse");
        let (vk_b, mods_b) = parse_hotkey("SHIFT+CTRL+ALT+A").expect("order shouldn't matter");
        assert_eq!(vk_a.0, vk_b.0);
        assert_eq!(mods_a.0, mods_b.0);
        assert_eq!(mods_a.0, MOD_CONTROL.0 | MOD_ALT.0 | MOD_SHIFT.0);
    }

    #[test]
    fn parse_hotkey_case_insensitive_modifiers() {
        assert!(parse_hotkey("ctrl+f9").is_some());
        assert!(parse_hotkey("Control+F9").is_some());
    }

    #[test]
    fn parse_hotkey_rejects_unknown_modifier_or_missing_base() {
        assert!(parse_hotkey("WIN+F9").is_none(), "Win/Super isn't a supported modifier here");
        assert!(parse_hotkey("CTRL+").is_none());
        assert!(parse_hotkey("").is_none());
        assert!(parse_hotkey("CTRL+NOTAKEY").is_none());
    }

    #[test]
    fn try_register_succeeds_for_an_unused_key() {
        // F24 is vanishingly unlikely to be bound by another running app in a
        // test environment; a real conflict would be a false test failure,
        // not a bug here, but that's an acceptable, very low-probability risk
        // for exercising the real RegisterHotKey round-trip.
        let vk = parse_base_key("F24").expect("F24 should be a known key");
        assert!(try_register(vk, HOT_KEY_MODIFIERS(0)), "expected an unused key to register successfully");
    }

    #[test]
    fn try_register_is_idempotent_when_called_repeatedly() {
        // Since try_register unregisters immediately after registering, calling
        // it again for the same key should also succeed (no stuck registration
        // left behind from the previous call).
        let vk = parse_base_key("F23").expect("F23 should be a known key");
        assert!(try_register(vk, HOT_KEY_MODIFIERS(0)));
        assert!(try_register(vk, HOT_KEY_MODIFIERS(0)));
    }

    #[test]
    fn try_register_with_modifier_succeeds_for_an_unused_combo() {
        let (vk, mods) = parse_hotkey("CTRL+ALT+F22").expect("should parse");
        assert!(try_register(vk, mods), "expected an unused modifier combo to register successfully");
    }
}
