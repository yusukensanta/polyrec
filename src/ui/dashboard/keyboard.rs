//! Using the dashboard from the keyboard: Tab moves between controls and
//! Space/Enter press them (egui does both), and a ring shows where focus is.

use super::App;
use super::theme::{CORNER_CONTROL, pal};
use eframe::egui;

/// Whether the keyboard is being used to move around: from a Tab or arrow
/// key until the next click -- when Windows, too, shows where focus is.
fn keyboard_navigating(was: bool, moved_by_keyboard: bool, clicked: bool) -> bool {
    !clicked && (was || moved_by_keyboard)
}

/// A 2 px ring, in the theme's text colour, around the control focus is on,
/// while `navigating`. egui's own sign of focus is a control's pressed look,
/// easy to miss among the rest.
fn draw_focus_ring(ctx: &egui::Context, navigating: bool) {
    if !navigating {
        return;
    }
    let Some(response) = ctx
        .memory(|m| m.focused())
        .and_then(|id| ctx.read_response(id))
    else {
        return;
    };
    ctx.layer_painter(response.layer_id).rect_stroke(
        response.rect.expand(2.0),
        CORNER_CONTROL + 2,
        egui::Stroke::new(2.0, pal().text_primary),
        egui::StrokeKind::Outside,
    );
}

impl App {
    /// Shows where keyboard focus is, once the keyboard is used to move
    /// around. Call after everything is drawn.
    pub(super) fn show_keyboard_focus(&mut self, ctx: &egui::Context) {
        let (moved, clicked) = ctx.input(|i| {
            let moved = [
                egui::Key::Tab,
                egui::Key::ArrowUp,
                egui::Key::ArrowDown,
                egui::Key::ArrowLeft,
                egui::Key::ArrowRight,
            ]
            .into_iter()
            .any(|key| i.key_pressed(key));
            (moved, i.pointer.any_pressed())
        });
        self.keyboard_navigating = keyboard_navigating(self.keyboard_navigating, moved, clicked);
        draw_focus_ring(ctx, self.keyboard_navigating);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_shows_after_moving_by_keyboard_until_the_next_click() {
        // Windows draws its focus rectangles the same way: only once the
        // keyboard is used to move around.
        assert!(!keyboard_navigating(false, false, false));
        assert!(keyboard_navigating(false, true, false));
        assert!(keyboard_navigating(true, false, false), "stays on");
        assert!(!keyboard_navigating(true, false, true), "a click ends it");
        assert!(!keyboard_navigating(false, true, true), "a click wins");
    }

    /// The 2 px rectangle strokes painted in one frame with a focused
    /// button, while `navigating` or not.
    fn ring_strokes(navigating: bool) -> usize {
        let ctx = egui::Context::default();
        let frame = |ctx: &egui::Context| {
            let mut button = None;
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                button = Some(ui.button("REC").id);
                draw_focus_ring(ui.ctx(), navigating);
            });
            output.textures_delta.clear();
            (output, button.unwrap())
        };
        let (_, button) = frame(&ctx);
        ctx.memory_mut(|m| m.request_focus(button));
        let (output, _) = frame(&ctx);
        output
            .shapes
            .iter()
            .filter(|clipped| {
                matches!(&clipped.shape, egui::Shape::Rect(rect) if rect.stroke.width == 2.0)
            })
            .count()
    }

    #[test]
    fn the_focused_control_gets_a_ring_only_while_navigating_by_keyboard() {
        assert_eq!(ring_strokes(true), 1);
        assert_eq!(ring_strokes(false), 0);
    }
}
