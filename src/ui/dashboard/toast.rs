//! A brief on-screen acknowledgement after a global hotkey: "● Recording",
//! "⏸ Paused" and so on. Global hotkeys are pressed from inside another app,
//! typically a fullscreen game, where nothing else confirms the press landed.

use super::App;
use super::overlay::{keep_on_top_and_out_of_captures, show_viewport_immediate};
use super::theme::{BG_OVERLAY, TEXT_BODY};
use crate::hotkeys::HotkeyEvent;
use crate::i18n::Strings;
use crate::types::SessionState;
use eframe::egui;
use std::time::{Duration, Instant};

/// How long a toast stays up.
const TOAST_DURATION: Duration = Duration::from_millis(1500);

pub(super) fn toast_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("polyrec_toast")
}

pub(super) struct Toast {
    text: String,
    until: Instant,
    /// Where to show it, in screen coordinates: the horizontal centre and
    /// the top of the monitor the user was working on when they pressed the
    /// hotkey.
    anchor: (f32, f32),
}

impl Toast {
    /// A toast for `text`, on the monitor of whatever window is in front.
    pub(super) fn new(text: String) -> Self {
        Self {
            text,
            until: Instant::now() + TOAST_DURATION,
            anchor: foreground_monitor_anchor(),
        }
    }
}

/// The toast for `event`, judged by what it actually changed (`before` →
/// `after`). A start that was refused says so -- the dashboard says why,
/// but it's likely behind whatever is being recorded. `None` for anything
/// else that changed nothing, like Pause while idle.
pub(super) fn hotkey_toast(
    event: HotkeyEvent,
    before: &SessionState,
    after: &SessionState,
    overlay_on: bool,
    highlight_saving: bool,
    s: &Strings,
) -> Option<String> {
    use SessionState::{Idle, Paused, Recording};
    let text = match event {
        HotkeyEvent::StartStop => match (before, after) {
            (Idle, Recording) => s.toast_recording,
            (Recording | Paused, Idle) => s.toast_stopped,
            (Idle, Idle) => s.toast_not_started,
            _ => return None,
        },
        HotkeyEvent::Pause => match (before, after) {
            (Recording, Paused) => s.toast_paused,
            (Paused, Recording) => s.toast_resumed,
            _ => return None,
        },
        HotkeyEvent::ToggleOverlay if overlay_on => s.overlay_on,
        HotkeyEvent::ToggleOverlay => s.overlay_off,
        HotkeyEvent::SaveHighlight if highlight_saving => s.highlight_saving_label,
        HotkeyEvent::SaveHighlight => s.toast_highlight_off,
    };
    Some(text.to_owned())
}

/// Top-centre of the monitor the foreground window is on, falling back to
/// the primary display's.
fn foreground_monitor_anchor() -> (f32, f32) {
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromWindow};
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    let rect = unsafe {
        let monitor = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY);
        crate::capture::video::monitor_rect(monitor).ok()
    };
    rect.map_or((960.0, 0.0), |r| {
        ((r.left + r.right) as f32 / 2.0, r.top as f32)
    })
}

impl App {
    /// Shows the current toast, if any, as its own click-through,
    /// always-on-top window -- but only while the dashboard isn't focused:
    /// with it in front, its own controls already show the change.
    pub(super) fn render_toast_viewport(&mut self, ctx: &egui::Context) {
        let Some(toast) = &self.toast else {
            return;
        };
        let now = Instant::now();
        if now >= toast.until {
            self.toast = None;
            return;
        }
        ctx.request_repaint_after(toast.until - now);
        if ctx.input(|i| i.viewport().focused) == Some(true) {
            return;
        }

        let font = egui::FontId::proportional(TEXT_BODY);
        let text_size = ctx.fonts_mut(|f| {
            f.layout_no_wrap(toast.text.clone(), font.clone(), egui::Color32::WHITE)
                .size()
        });
        let size = text_size + egui::vec2(24.0, 14.0);
        let (center_x, top) = toast.anchor;
        let text = toast.text.clone();
        show_viewport_immediate(
            ctx,
            toast_viewport_id(),
            egui::ViewportBuilder::default()
                .with_title("PolyRec Toast")
                .with_always_on_top()
                .with_decorations(false)
                .with_transparent(true)
                .with_mouse_passthrough(true)
                .with_inner_size(size)
                .with_position(egui::pos2(center_x - size.x / 2.0, top + 40.0)),
            move |ctx, _class| {
                // Same ctx-only form as the overlay HUD -- see the comment
                // in `render_overlay_viewport`.
                #[allow(deprecated)]
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::NONE
                            .fill(BG_OVERLAY)
                            .corner_radius(6u8)
                            .inner_margin(egui::Margin::symmetric(12, 7)),
                    )
                    .show(ctx, |ui| {
                        ui.label(
                            egui::RichText::new(&text)
                                .font(font.clone())
                                .color(egui::Color32::WHITE),
                        );
                    });
            },
        );
        keep_on_top_and_out_of_captures(windows::core::w!("PolyRec Toast"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;
    use SessionState::{Idle, Paused, Recording};

    fn toast(event: HotkeyEvent, before: SessionState, after: SessionState) -> Option<String> {
        hotkey_toast(event, &before, &after, false, false, Lang::En.strings())
    }

    #[test]
    fn start_stop_and_pause_toasts_follow_what_actually_changed() {
        let s = Lang::En.strings();
        assert_eq!(
            toast(HotkeyEvent::StartStop, Idle, Recording).as_deref(),
            Some(s.toast_recording)
        );
        assert_eq!(
            toast(HotkeyEvent::StartStop, Recording, Idle).as_deref(),
            Some(s.toast_stopped)
        );
        assert_eq!(
            toast(HotkeyEvent::StartStop, Paused, Idle).as_deref(),
            Some(s.toast_stopped)
        );
        assert_eq!(
            toast(HotkeyEvent::Pause, Recording, Paused).as_deref(),
            Some(s.toast_paused)
        );
        assert_eq!(
            toast(HotkeyEvent::Pause, Paused, Recording).as_deref(),
            Some(s.toast_resumed)
        );
    }

    #[test]
    fn a_refused_start_says_so() {
        let s = Lang::En.strings();
        assert_eq!(
            toast(HotkeyEvent::StartStop, Idle, Idle).as_deref(),
            Some(s.toast_not_started)
        );
    }

    #[test]
    fn pause_while_idle_gets_no_toast() {
        assert_eq!(toast(HotkeyEvent::Pause, Idle, Idle), None);
    }

    #[test]
    fn overlay_and_highlight_toasts_report_the_new_state() {
        let s = Lang::En.strings();
        let with =
            |event, overlay_on, saving| hotkey_toast(event, &Idle, &Idle, overlay_on, saving, s);
        assert_eq!(
            with(HotkeyEvent::ToggleOverlay, true, false).as_deref(),
            Some(s.overlay_on)
        );
        assert_eq!(
            with(HotkeyEvent::ToggleOverlay, false, false).as_deref(),
            Some(s.overlay_off)
        );
        assert_eq!(
            with(HotkeyEvent::SaveHighlight, false, true).as_deref(),
            Some(s.highlight_saving_label)
        );
        assert_eq!(
            with(HotkeyEvent::SaveHighlight, false, false).as_deref(),
            Some(s.toast_highlight_off)
        );
    }
}
