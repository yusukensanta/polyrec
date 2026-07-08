use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::mpsc;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT, RegisterHotKey,
    UnregisterHotKey, VIRTUAL_KEY, VK_0, VK_1, VK_2, VK_3, VK_4, VK_5, VK_6, VK_7, VK_8, VK_9,
    VK_A, VK_B, VK_BACK, VK_C, VK_CONTROL, VK_D, VK_DELETE, VK_DOWN, VK_E, VK_END, VK_F, VK_F1,
    VK_F10, VK_F11, VK_F12, VK_F13, VK_F14, VK_F15, VK_F16, VK_F17, VK_F18, VK_F19, VK_F2, VK_F20,
    VK_F21, VK_F22, VK_F23, VK_F24, VK_F3, VK_F4, VK_F5, VK_F6, VK_F7, VK_F8, VK_F9, VK_G, VK_H,
    VK_HOME, VK_I, VK_INSERT, VK_J, VK_K, VK_L, VK_LEFT, VK_M, VK_MENU, VK_N, VK_NEXT, VK_O, VK_P,
    VK_PRIOR, VK_Q, VK_R, VK_RETURN, VK_RIGHT, VK_S, VK_SHIFT, VK_SPACE, VK_T, VK_TAB, VK_U, VK_UP,
    VK_V, VK_W, VK_X, VK_Y, VK_Z,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PeekMessageW, PM_NOREMOVE, PostThreadMessageW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT,
    MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

/// `MOD_NOREPEAT` (0x4000) — only relevant to [`try_register`]'s `RegisterHotKey`
/// probe now (the real listener uses a keyboard hook, which has its own
/// repeat-suppression via `HookContext::held`, see `run_hook_loop`).
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
            run_hook_loop(event_tx, start_stop_hk, pause_hk, toggle_overlay_hk);
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

/// A throwaway ID for [`try_register`]'s test registration.
const ID_PROBE: i32 = 999;

/// Synchronously tests whether `vk`+`mods` can actually be registered as a
/// global hotkey right now, via `RegisterHotKey` (registering and immediately
/// unregistering). The real listener (`run_hook_loop`) uses a low-level
/// keyboard hook instead, which can observe any key combination -- hooks
/// don't have RegisterHotKey's "already claimed by another process" failure
/// mode, since multiple processes can each hook the same key independently.
/// So this probe is no longer testing "is this available for the listener"
/// (the listener can always bind); it's testing "does Windows treat this
/// combination as reserved" -- a small but genuine set of combinations (e.g.
/// Ctrl+Alt+Del) are intercepted by the OS before any hook or app ever sees
/// them, and `RegisterHotKey` failing for a combination is the best available
/// signal for that, short of hand-maintaining a guessed list that may not
/// match this specific machine's actual state.
pub fn try_register(vk: VIRTUAL_KEY, mods: HOT_KEY_MODIFIERS) -> bool {
    unsafe {
        let ok = RegisterHotKey(None, ID_PROBE, mods | MOD_NOREPEAT, vk.0 as u32).is_ok();
        if ok {
            let _ = UnregisterHotKey(None, ID_PROBE);
        }
        ok
    }
}

struct Binding {
    vk: VIRTUAL_KEY,
    mods: HOT_KEY_MODIFIERS,
    event: HotkeyEvent,
}

/// Per-thread state for the low-level keyboard hook -- `WH_KEYBOARD_LL`'s
/// callback is a bare function pointer with no user-data parameter, and is
/// always invoked on the thread that installed the hook, so `thread_local!`
/// is the natural (and safe) way to get the configured bindings and event
/// sender into it.
struct HookContext {
    bindings: Vec<Binding>,
    /// vk codes currently down and already matched/fired -- suppresses
    /// re-firing on Windows' OS-level key-repeat while the key stays held,
    /// the hook equivalent of `RegisterHotKey`'s `MOD_NOREPEAT` flag. Cleared
    /// on that key's key-up, regardless of what modifiers are held at that
    /// point (matching a key release always ends "repeat" for it).
    held: HashSet<u32>,
    tx: mpsc::Sender<HotkeyEvent>,
}

thread_local! {
    static HOOK_CTX: RefCell<Option<HookContext>> = const { RefCell::new(None) };
}

/// Reads which of Ctrl/Alt/Shift are *currently* held via `GetAsyncKeyState`,
/// rather than tracking modifier key-down/up events through the hook
/// ourselves -- `GetAsyncKeyState(VK_CONTROL/VK_MENU/VK_SHIFT)` already
/// reports either the left or right key of the pair being down, matching
/// `RegisterHotKey`'s own non-directional modifier semantics.
fn live_modifiers() -> HOT_KEY_MODIFIERS {
    const DOWN_BIT: i16 = u16::MAX.wrapping_shl(15) as i16; // 0x8000 as i16
    let mut m = HOT_KEY_MODIFIERS(0);
    unsafe {
        if GetAsyncKeyState(VK_CONTROL.0 as i32) & DOWN_BIT != 0 {
            m |= MOD_CONTROL;
        }
        if GetAsyncKeyState(VK_MENU.0 as i32) & DOWN_BIT != 0 {
            m |= MOD_ALT;
        }
        if GetAsyncKeyState(VK_SHIFT.0 as i32) & DOWN_BIT != 0 {
            m |= MOD_SHIFT;
        }
    }
    m
}

/// `WH_KEYBOARD_LL` callback -- sees every keystroke system-wide before it
/// reaches whatever window/app has focus (including exclusive-fullscreen
/// games), unlike `RegisterHotKey`'s `WM_HOTKEY`, which some games'
/// exclusive-fullscreen input handling can prevent from being delivered.
/// Matching bindings are consumed (return non-zero, skip `CallNextHookEx`) so
/// the keystroke doesn't also reach the foreground app -- same effect as
/// `RegisterHotKey` suppressing it. Everything else is passed through
/// unchanged; this must never swallow a keystroke that isn't one of our own
/// bindings.
unsafe extern "system" fn keyboard_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code as u32 == HC_ACTION {
        let msg = wparam.0 as u32;
        let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
        if is_down || is_up {
            let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            let vk_code = kb.vkCode;
            let mut consumed = false;
            HOOK_CTX.with(|ctx| {
                let Ok(mut ctx) = ctx.try_borrow_mut() else {
                    return;
                };
                let Some(hc) = ctx.as_mut() else {
                    return;
                };
                if is_up {
                    hc.held.remove(&vk_code);
                } else if !hc.held.contains(&vk_code) {
                    let current_mods = live_modifiers();
                    if let Some(binding) =
                        hc.bindings.iter().find(|b| b.vk.0 as u32 == vk_code && b.mods.0 == current_mods.0)
                    {
                        hc.held.insert(vk_code);
                        hc.tx.send(binding.event).ok();
                        consumed = true;
                    }
                }
            });
            if consumed {
                return LRESULT(1);
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn run_hook_loop(
    tx: mpsc::Sender<HotkeyEvent>,
    start_stop_hk: Option<(VIRTUAL_KEY, HOT_KEY_MODIFIERS)>,
    pause_hk: Option<(VIRTUAL_KEY, HOT_KEY_MODIFIERS)>,
    toggle_overlay_hk: Option<(VIRTUAL_KEY, HOT_KEY_MODIFIERS)>,
) {
    let mut bindings = Vec::new();
    if let Some((vk, mods)) = start_stop_hk {
        bindings.push(Binding { vk, mods, event: HotkeyEvent::StartStop });
    }
    if let Some((vk, mods)) = pause_hk {
        bindings.push(Binding { vk, mods, event: HotkeyEvent::Pause });
    }
    if let Some((vk, mods)) = toggle_overlay_hk {
        bindings.push(Binding { vk, mods, event: HotkeyEvent::ToggleOverlay });
    }

    HOOK_CTX.with(|ctx| {
        *ctx.borrow_mut() = Some(HookContext { bindings, held: HashSet::new(), tx });
    });

    let hook: HHOOK = match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook_proc), None, 0) } {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("SetWindowsHookExW(WH_KEYBOARD_LL) failed: {e}");
            HOOK_CTX.with(|ctx| *ctx.borrow_mut() = None);
            return;
        }
    };

    // GetMessageW blocks this thread (which owns no window) until either the
    // stop() signal (WM_QUIT via PostThreadMessageW) arrives, or Windows
    // delivers the installed hook's callback as part of its normal wait --
    // the hook doesn't need any message of its own to be dispatched, just a
    // thread that's actively pumping.
    let mut msg = MSG::default();
    loop {
        let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if ret.0 <= 0 {
            break; // 0 = WM_QUIT, negative = error
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    unsafe {
        let _ = UnhookWindowsHookEx(hook);
    }
    HOOK_CTX.with(|ctx| *ctx.borrow_mut() = None);
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

    #[test]
    fn live_modifiers_reflects_no_keys_held_in_test_context() {
        // A real test process isn't holding Ctrl/Alt/Shift, so this should
        // read as no modifiers -- a basic sanity check that the GetAsyncKeyState
        // bit-test logic isn't inverted or off-by-one.
        assert_eq!(live_modifiers().0, 0);
    }
}
