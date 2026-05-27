use std::sync::mpsc;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, RegisterHotKey, UnregisterHotKey, VIRTUAL_KEY, VK_F1, VK_F10, VK_F11,
    VK_F12, VK_F2, VK_F3, VK_F4, VK_F5, VK_F6, VK_F7, VK_F8, VK_F9,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, PostThreadMessageW, TranslateMessage, MSG, WM_HOTKEY,
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
        let start_stop_vk = parse_vk(start_stop);
        let pause_vk = parse_vk(pause);
        let toggle_overlay_vk = parse_vk(toggle_overlay);

        let (event_tx, event_rx) = mpsc::channel::<HotkeyEvent>();
        let (id_tx, id_rx) = mpsc::channel::<u32>();

        let handle = std::thread::spawn(move || {
            let tid = unsafe { GetCurrentThreadId() };
            id_tx.send(tid).ok();
            run_hotkey_loop(event_tx, start_stop_vk, pause_vk, toggle_overlay_vk);
        });

        let thread_id = id_rx.recv().unwrap_or(0);

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

pub fn parse_vk(s: &str) -> Option<VIRTUAL_KEY> {
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
        _ => None,
    }
}

const ID_START_STOP: i32 = 1;
const ID_PAUSE: i32 = 2;
const ID_TOGGLE_OVERLAY: i32 = 3;

fn run_hotkey_loop(
    tx: mpsc::Sender<HotkeyEvent>,
    start_stop_vk: Option<VIRTUAL_KEY>,
    pause_vk: Option<VIRTUAL_KEY>,
    toggle_overlay_vk: Option<VIRTUAL_KEY>,
) {
    unsafe {
        if let Some(vk) = start_stop_vk {
            RegisterHotKey(None, ID_START_STOP, MOD_NOREPEAT, vk.0 as u32).ok();
        }
        if let Some(vk) = pause_vk {
            RegisterHotKey(None, ID_PAUSE, MOD_NOREPEAT, vk.0 as u32).ok();
        }
        if let Some(vk) = toggle_overlay_vk {
            RegisterHotKey(None, ID_TOGGLE_OVERLAY, MOD_NOREPEAT, vk.0 as u32).ok();
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
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

        UnregisterHotKey(None, ID_START_STOP).ok();
        UnregisterHotKey(None, ID_PAUSE).ok();
        UnregisterHotKey(None, ID_TOGGLE_OVERLAY).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_vk_known_keys() {
        // VK_F7=0x76, VK_F8=0x77, VK_F9=0x78
        assert_eq!(parse_vk("F7").map(|v| v.0), Some(0x76u16));
        assert_eq!(parse_vk("F8").map(|v| v.0), Some(0x77u16));
        assert_eq!(parse_vk("F9").map(|v| v.0), Some(0x78u16));
    }

    #[test]
    fn parse_vk_unknown_returns_none() {
        assert!(parse_vk("F99").is_none());
        assert!(parse_vk("").is_none());
        assert!(parse_vk("CTRL").is_none());
    }
}
