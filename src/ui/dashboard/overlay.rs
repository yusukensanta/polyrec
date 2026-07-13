use super::App;
use super::theme::TEXT_BODY;
use crate::i18n::Strings;
use crate::types::CaptureKind;
use eframe::egui;

impl App {
    /// Second OS window (click-through, always-on-top) showing a compact
    /// timer/track-count HUD while recording -- opt-in via the Overlay toggle
    /// in the menu bar.
    pub(super) fn render_overlay_viewport(&mut self, ctx: &egui::Context, s: &'static Strings) {
        let is_recording = self.session.is_recording();
        if !(is_recording && self.overlay_enabled) {
            return;
        }

        let track_count = self.selected_audio.iter().filter(|&&b| b).count();
        let elapsed_secs = self
            .session
            .active
            .as_ref()
            .map(|a| a.clock.elapsed().as_secs())
            .unwrap_or(0);

        // Position on whichever monitor the recording is actually on -- falls
        // back to the primary display (matching query_capture_size's existing
        // graceful-degradation convention) if the handle is gone, the monitor
        // query fails, or (for a `Window` source) the window closed.
        let active_target = self.session.active.as_ref().map(|a| (a.hwnd, a.kind));
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
        let stop_word = s.overlay_hud_stop_word;
        // Reflects whatever start/stop is actually bound to right now, not a
        // hardcoded default -- it's freely rebindable (see render_hotkeys_popup).
        let stop_key = self.config.hotkeys.start_stop.clone();

        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("polyrec_overlay"),
            egui::ViewportBuilder::default()
                .with_title("PolyRec Overlay")
                .with_always_on_top()
                .with_decorations(false)
                .with_transparent(true)
                .with_mouse_passthrough(true)
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
                            .fill(egui::Color32::from_rgba_premultiplied(20, 20, 20, 200))
                            .inner_margin(6i8),
                    )
                    .show(ctx, |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "● {:02}:{:02}:{:02}  |  {track_count} {tracks_word}  |  {stop_key} {stop_word}",
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
        reassert_overlay_topmost();
    }
}

/// Re-asserts the overlay window as topmost via a direct `SetWindowPos`, on
/// top of egui/winit's one-time `with_always_on_top()` hint -- see the call
/// site in `render_overlay_viewport` for why this is needed every frame
/// rather than once. Looked up by its exact window title (set via
/// `.with_title("PolyRec Overlay")`) since egui doesn't expose a viewport's
/// native HWND through its public API. A miss (window not found yet, e.g.
/// the very first frame before winit has created it) is silently ignored --
/// the next frame's call will pick it up.
///
/// `SWP_ASYNCWINDOWPOS` is required here, not optional: this runs on the
/// same thread that owns the overlay window and is mid-repaint of it via
/// egui/winit when called. Without it, `SetWindowPos` sends
/// WM_WINDOWPOSCHANGING/CHANGED synchronously to that window's own message
/// pump on this same thread -- reentering winit's event handling from inside
/// its own repaint reliably crashed the process in testing. The async flag
/// posts the request instead of sending it, avoiding the reentrant call.
fn reassert_overlay_topmost() {
    use windows::Win32::UI::WindowsAndMessaging::{
        HWND_TOPMOST, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
    };
    unsafe {
        let Ok(hwnd) = windows::Win32::UI::WindowsAndMessaging::FindWindowW(
            None,
            windows::core::w!("PolyRec Overlay"),
        ) else {
            return;
        };
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
