use super::App;
use super::theme::{Palette, TEXT_BODY};
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
        //
        // Not over a game in exclusive full-screen, though: there it goes on
        // another monitor, or isn't shown (see `hud_place`).
        let active_target = self.session.active().map(|a| (a.hwnd, a.kind));
        let target_monitor = active_target.map(|(hwnd_val, kind)| match kind {
            CaptureKind::Window => unsafe {
                windows::Win32::Graphics::Gdi::MonitorFromWindow(
                    windows::Win32::Foundation::HWND(hwnd_val as *mut core::ffi::c_void),
                    windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST,
                )
            },
            CaptureKind::Monitor => {
                windows::Win32::Graphics::Gdi::HMONITOR(hwnd_val as *mut core::ffi::c_void)
            }
        });
        let monitor_rect = match target_monitor {
            Some(monitor) => match super::hud_place::hud_monitor_rect(monitor) {
                Some(rect) => Some(rect),
                None => return,
            },
            None => None,
        };
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

        self.overlay_window.show(
            ctx,
            overlay_viewport_id(),
            hud_window("PolyRec Overlay")
                // Widened from 310 to fit longer modifier combos (e.g.
                // "CTRL+ALT+SHIFT+F9") without clipping the stop-key hint.
                .with_inner_size([400.0, 32.0])
                .with_position(egui::pos2(screen_right - 410.0, screen_top + 10.0)),
            move |ctx| {
                // CentralPanel::show(ctx, ...) is soft-deprecated in favor of
                // show_inside(ui, ...), but this closure only ever receives a
                // fresh viewport's &Context -- there's no pre-existing Ui to
                // nest inside here, so the ctx-based form is unavoidable.
                #[allow(deprecated)]
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::NONE
                            .fill(Palette::DARK.bg_overlay)
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

/// One of PolyRec's own on-screen windows (the overlay HUD, the hotkey
/// toast), kept hidden until it has been drawn. A window shows whatever it
/// has the moment it appears, and before its first frame that's blank
/// white: a white box flashed up before a hotkey's toast -- for a frame or
/// two normally, longer with a game keeping the graphics card busy, which
/// is when hotkeys get pressed.
#[derive(Default)]
pub(super) struct HudWindow {
    /// The pass it was last drawn on (`Context::cumulative_pass_nr`).
    drawn_on: Option<u64>,
}

impl HudWindow {
    /// Shows it for this pass, drawn by `ui`. Called on every pass it's
    /// wanted: egui closes the window on a pass that doesn't show it.
    pub(super) fn show(
        &mut self,
        ctx: &egui::Context,
        id: egui::ViewportId,
        builder: egui::ViewportBuilder,
        mut ui: impl FnMut(&mut egui::Ui),
    ) {
        let pass = ctx.cumulative_pass_nr();
        // Up and drawn only if it was drawn on the pass before this one.
        let drawn = self.drawn_on.is_some_and(|on| on + 1 >= pass);
        let mut drawn_now = false;
        show_viewport_immediate(ctx, id, builder.with_visible(drawn), |viewport_ui, _| {
            drawn_now = true;
            ui(viewport_ui);
        });
        if drawn_now {
            self.drawn_on = Some(pass);
            if !drawn {
                // Drawn while hidden: shown next pass, with that picture.
                ctx.request_repaint();
            }
        }
    }
}

/// The panic message egui gives when eframe returns from showing an
/// immediate viewport without running its UI -- see `show_viewport_immediate`.
const NO_WINDOW_YET_PANIC: &str = "the user callback was never called";

/// Whether a panic with `message` is the one `show_viewport_immediate`
/// catches and recovers from, which isn't worth a `crash_log` entry.
pub fn is_caught_on_purpose(message: &str) -> bool {
    message.contains(NO_WINDOW_YET_PANIC)
}

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

    /// Runs one dashboard pass per entry of `passes` against a stand-in for
    /// eframe's window backend, with a `HudWindow` shown on the passes that
    /// want it (`.0`). The backend draws the window only when it can
    /// (`.1`; it can't on a pass where the window can't be created yet --
    /// see `show_viewport_immediate`). Returns, for each pass the window was
    /// shown on, the visibility it asked the backend for.
    fn visibility_asked_for(passes: &[(bool, bool)]) -> Vec<Option<bool>> {
        use std::cell::{Cell, RefCell};
        use std::rc::Rc;
        let asked = Rc::new(RefCell::new(Vec::new()));
        let can_draw = Rc::new(Cell::new(true));
        {
            let (asked, can_draw) = (Rc::clone(&asked), Rc::clone(&can_draw));
            egui::Context::set_immediate_viewport_renderer(move |ctx, mut viewport| {
                asked.borrow_mut().push(viewport.builder.visible);
                if can_draw.get() {
                    let mut input = egui::RawInput {
                        viewport_id: viewport.ids.this,
                        ..Default::default()
                    };
                    input
                        .viewports
                        .insert(viewport.ids.this, Default::default());
                    ctx.run_ui(input, |ui| (viewport.viewport_ui_cb)(ui))
                        .textures_delta
                        .clear();
                }
            });
        }
        let ctx = egui::Context::default();
        ctx.set_embed_viewports(false);
        let mut window = HudWindow::default();
        for &(wanted, drawable) in passes {
            can_draw.set(drawable);
            ctx.run_ui(egui::RawInput::default(), |ui| {
                if wanted {
                    window.show(
                        ui.ctx(),
                        egui::ViewportId::from_hash_of("test hud"),
                        hud_window("Test HUD"),
                        |_| {},
                    );
                }
            })
            .textures_delta
            .clear();
        }
        asked.take()
    }

    /// A window shows whatever it has on screen the moment it appears --
    /// before its first frame, blank white: the white box that flashed up
    /// before a hotkey's toast.
    #[test]
    fn a_hud_window_stays_hidden_until_it_has_been_drawn() {
        assert_eq!(
            visibility_asked_for(&[(true, true), (true, true), (true, true)]),
            [Some(false), Some(true), Some(true)]
        );
    }

    #[test]
    fn a_hud_window_that_could_not_be_created_yet_is_still_hidden_next_time() {
        assert_eq!(
            visibility_asked_for(&[(true, false), (true, true), (true, true)]),
            [Some(false), Some(false), Some(true)]
        );
    }

    /// egui closes the window of a pass that doesn't show it; the next one
    /// is a new window, blank until drawn.
    #[test]
    fn a_hud_window_that_went_away_is_hidden_again_until_drawn() {
        assert_eq!(
            visibility_asked_for(&[(true, true), (true, true), (false, true), (true, true)]),
            [Some(false), Some(true), Some(false)]
        );
    }
}
