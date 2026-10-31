use super::App;
use super::theme::{BG_OVERLAY, TEXT_BODY};
use crate::i18n::Strings;
use crate::types::CaptureKind;
use eframe::egui;

pub(super) fn overlay_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("polyrec_overlay")
}

impl App {
    /// Whether the overlay HUD is up: recording or paused, with Overlay
    /// turned on. It used to vanish while paused, which looked the same as
    /// the recording having stopped.
    pub(super) fn overlay_visible(&self) -> bool {
        (self.session.is_recording() || self.session.is_paused()) && self.overlay_enabled
    }

    /// Second OS window (click-through, always-on-top) showing a compact
    /// timer/track-count HUD while recording -- opt-in via the Overlay toggle
    /// in the menu bar.
    pub(super) fn render_overlay_viewport(&mut self, ctx: &egui::Context, s: &'static Strings) {
        if !self.overlay_visible() {
            return;
        }

        let track_count = self.recording_track_count();
        let elapsed_secs = self
            .session
            .active()
            .map(|a| a.elapsed().as_secs())
            .unwrap_or(0);

        // Position on whichever monitor the recording is actually on -- falls
        // back to the primary display (matching query_capture_size's existing
        // graceful-degradation convention) if the handle is gone, the monitor
        // query fails, or (for a `Window` source) the window closed.
        let active_target = self.session.active().map(|a| (a.hwnd, a.kind));
        let monitor_rect = active_target.and_then(|(hwnd_val, kind)| match kind {
            CaptureKind::Window => {
                let hwnd = windows::Win32::Foundation::HWND(hwnd_val as *mut core::ffi::c_void);
                crate::capture::video::query_monitor_rect(hwnd).ok()
            }
            CaptureKind::Monitor => {
                let hmonitor =
                    windows::Win32::Graphics::Gdi::HMONITOR(hwnd_val as *mut core::ffi::c_void);
                crate::capture::video::monitor_rect(hmonitor).ok()
            }
        });
        let (screen_right, screen_top) = match monitor_rect {
            Some(rect) => (rect.right as f32, rect.top as f32),
            None => {
                let screen_w = unsafe {
                    windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                        windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN,
                    ) as f32
                };
                (screen_w, 0.0)
            }
        };
        let tracks_word = s.tracks_word;
        // Paused: a pause mark and how to resume. Recording: the red dot and
        // how to stop. The key is whatever is bound right now, not a hardcoded
        // default -- it's freely rebindable (see render_hotkeys_popup).
        let paused = self.session.is_paused();
        let (mark, hint_template, key) = if paused {
            ("⏸", s.hotkey_hint_resume, &self.config.hotkeys.pause)
        } else {
            ("●", s.hotkey_hint_stop, &self.config.hotkeys.start_stop)
        };
        let key_hint = if key.is_empty() {
            String::new()
        } else {
            format!("  |  {}", hint_template.replace("{key}", key))
        };

        show_viewport_immediate(
            ctx,
            overlay_viewport_id(),
            hud_window("PolyRec Overlay")
                // Widened from 310 to fit longer modifier combos (e.g.
                // "CTRL+ALT+SHIFT+F9") without clipping the stop-key hint.
                .with_inner_size([400.0, 32.0])
                .with_position(egui::pos2(screen_right - 410.0, screen_top + 10.0)),
            move |ctx, _class| {
                // CentralPanel::show(ctx, ...) is soft-deprecated in favor of
                // show_inside(ui, ...), but this closure only ever receives a
                // fresh viewport's &Context -- there's no pre-existing Ui to
                // nest inside here, so the ctx-based form is unavoidable.
                #[allow(deprecated)]
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::NONE
                            .fill(BG_OVERLAY)
                            .inner_margin(6i8),
                    )
                    .show(ctx, |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "{mark} {:02}:{:02}:{:02}  |  {track_count} {tracks_word}{key_hint}",
                                elapsed_secs / 3600,
                                (elapsed_secs % 3600) / 60,
                                elapsed_secs % 60,
                            ))
                            .color(egui::Color32::WHITE)
                            .size(TEXT_BODY),
                        );
                    });
            },
        );

        // `with_always_on_top()` only sets WS_EX_TOPMOST once, at window
        // creation. Fullscreen games commonly re-assert their own topmost
        // z-order on every frame or focus change (so their own overlays/UI
        // stay above everything), which silently demotes ours behind them --
        // matching the "takes several tries" symptom (it only shows when a
        // repaint happens to land right after the game's own re-assertion).
        // Re-asserting HWND_TOPMOST every frame the overlay is visible fights
        // that back instead of relying on a one-time flag.
        keep_on_top_and_out_of_captures(windows::core::w!("PolyRec Overlay"));
    }
}

/// The window for one of PolyRec's own on-screen displays (the overlay HUD,
/// the hotkey toast): borderless, see-through, always on top and
/// click-through -- and never activated when it appears. These come up on
/// Start and Stop, typically while a fullscreen game has focus; a window
/// that took focus as it was shown pulled the player out of the game, and
/// they had to switch back to it before they could play on.
pub(super) fn hud_window(title: &str) -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_title(title)
        .with_active(false)
        .with_always_on_top()
        .with_decorations(false)
        .with_transparent(true)
        .with_mouse_passthrough(true)
}

/// The panic message egui gives when eframe returns from showing an
/// immediate viewport without running its UI -- see `show_viewport_immediate`.
const NO_WINDOW_YET_PANIC: &str = "the user callback was never called";

/// `ctx.show_viewport_immediate` for one of PolyRec's own on-screen windows
/// (the overlay HUD, the hotkey toast), except that a pass that can't create
/// the window yet skips it instead of taking PolyRec down.
///
/// eframe 0.37 runs the passes of a hidden dashboard (hidden in the tray)
/// from `check_redraw_requests` in `new_events`, outside its event-loop
/// context, where an immediate viewport that has no window yet can't get
/// one: eframe returns without running the viewport's UI, and egui panics
/// with "egui backend is implemented incorrectly - the user callback was
/// never called". That crashed PolyRec when a recording started with the
/// overlay on, or a hotkey's toast came up, while it was hidden in the tray.
///
/// Nothing is left half-done when that panic is raised: eframe has already
/// registered the viewport, and creates its window at the start of its next
/// pass, through a path that does have the event loop. So the panic is
/// caught, that one pass goes without the window, and it comes up a pass
/// later. Any other panic is passed on.
pub(super) fn show_viewport_immediate(
    ctx: &egui::Context,
    id: egui::ViewportId,
    builder: egui::ViewportBuilder,
    ui: impl FnMut(&mut egui::Ui, egui::ViewportClass),
) {
    let shown = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ctx.show_viewport_immediate(id, builder, ui);
    }));
    if let Err(panic) = shown {
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied());
        if !message.is_some_and(|message| message.contains(NO_WINDOW_YET_PANIC)) {
            std::panic::resume_unwind(panic);
        }
        tracing::debug!("{id:?} has no window yet; it comes up on the next pass");
    }
}

/// For one of PolyRec's own on-screen windows (the overlay HUD, the hotkey
/// toast), found by its exact window title, since egui doesn't expose a
/// viewport's native HWND through its public API. A miss (the window not
/// created yet, e.g. its very first frame) is silently ignored; the next
/// frame's call picks it up. Two things, every frame it's shown:
///
/// - **Out of captures.** `WDA_EXCLUDEFROMCAPTURE` keeps the window out of
///   screen captures, PolyRec's own included: a display recording used to
///   contain the HUD. Windows 10 2004+; it fails harmlessly before that.
/// - **On top.** It re-asserts the window as topmost via a direct
///   `SetWindowPos`, on top of egui/winit's one-time `with_always_on_top()`
///   hint; see the call site in `render_overlay_viewport` for why that's
///   needed every frame rather than once.
///
/// `SWP_ASYNCWINDOWPOS` is required here, not optional: this runs on the
/// same thread that owns the window and is mid-repaint of it via egui/winit
/// when called. Without it, `SetWindowPos` sends
/// WM_WINDOWPOSCHANGING/CHANGED synchronously to that window's own message
/// pump on this same thread -- reentering winit's event handling from inside
/// its own repaint reliably crashed the process in testing. The async flag
/// posts the request instead of sending it, avoiding the reentrant call.
pub(super) fn keep_on_top_and_out_of_captures(title: windows::core::PCWSTR) {
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, HWND_TOPMOST, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        SetWindowDisplayAffinity, SetWindowPos, WDA_EXCLUDEFROMCAPTURE,
    };
    unsafe {
        let Ok(hwnd) = FindWindowW(None, title) else {
            return;
        };
        let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The overlay and the toast come up on Start and Stop, typically over
    /// a game that has focus: they're shown without taking it, and clicks go
    /// through them.
    #[test]
    fn hud_windows_never_take_focus() {
        let window = hud_window("PolyRec Overlay");
        assert_eq!(window.active, Some(false));
        assert_eq!(window.mouse_passthrough, Some(true));
    }
}
