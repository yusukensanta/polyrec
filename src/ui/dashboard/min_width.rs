//! The dashboard's minimum width, from what its parts need: the menu bar's
//! title and buttons side by side, and the centre's widest row that can't
//! shrink, beside the source panel at its narrowest. What they need depends
//! on the language, Windows' text size and whether an update is offered, so
//! it's measured as they're laid out rather than fixed. Narrower, the menu
//! bar's buttons drew over its title and the centre's rows ran off the
//! window's edge.

use eframe::egui;

/// The source panel's narrowest and widest, dragging its edge.
pub(super) const SOURCE_PANEL_MIN: f32 = 200.0;
pub(super) const SOURCE_PANEL_MAX: f32 = 380.0;

/// What the language and text size are, as far as how wide things are
/// drawn goes: what the centre needs is learned again when it changes.
pub(super) type LayoutKey = (crate::i18n::Lang, u32);

#[derive(Debug, Default)]
pub(super) struct MinWidth {
    /// The window width the menu bar needs, margins included.
    menu: f32,
    /// How wide the source panel is.
    panel: f32,
    /// The width the centre needs (its widest row that can't shrink, and
    /// the scroll bar beside it), once it has been too narrow for it; 0
    /// until then.
    centre: f32,
    /// The window's width beyond the source panel and the centre: the
    /// panels' margins and the separator between them.
    margins: f32,
    /// What `centre` was learned at.
    learned_for: Option<LayoutKey>,
    /// The minimum width last given to the window.
    applied: f32,
}

impl MinWidth {
    /// The menu bar's row is `row` wide in a `window` this wide, and its two
    /// ends, with the space between them, took `used`.
    pub(super) fn menu(&mut self, window: f32, row: f32, used: f32) {
        self.menu = window - row + used;
    }

    /// The source panel is `width` wide.
    pub(super) fn source_panel(&mut self, width: f32) {
        self.panel = width;
    }

    /// The centre was `space` wide beside the source panel in a `window`
    /// this wide, its language and text size `key`.
    pub(super) fn centre(&mut self, key: LayoutKey, window: f32, space: f32) {
        if self.learned_for != Some(key) {
            self.learned_for = Some(key);
            self.centre = 0.0;
        }
        self.margins = window - self.panel - space;
    }

    /// The centre's content, laid out `width` wide, took `content`: a row
    /// that didn't fit makes it wider, and the centre needs that much more.
    pub(super) fn centre_content(&mut self, space: f32, width: f32, content: f32) {
        if content > width + 0.5 {
            self.centre = self.centre.max(space - width + content);
        }
    }

    /// The widest the source panel may be dragged in a `window` this wide,
    /// leaving the centre what it needs.
    pub(super) fn panel_max(&self, window: f32) -> f32 {
        (window - self.margins - self.centre).clamp(SOURCE_PANEL_MIN, SOURCE_PANEL_MAX)
    }

    /// The window's minimum width: room for the menu bar, and for the
    /// centre beside the source panel at its narrowest -- never under
    /// `floor`.
    pub(super) fn needed(&self, floor: f32) -> f32 {
        floor
            .max(self.menu)
            .max(SOURCE_PANEL_MIN + self.margins + self.centre)
            .ceil()
    }

    /// Gives the window `needed` as its minimum size (`floor` its width and
    /// height otherwise) when that changed, widening it to that if it's
    /// narrower: Windows holds a window to its minimum only as it's next
    /// resized. A maximized window is left as it is.
    pub(super) fn apply(&mut self, ctx: &egui::Context, floor: (f32, f32)) {
        let width = self.needed(floor.0);
        if width == self.applied {
            return;
        }
        self.applied = width;
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(egui::vec2(
            width, floor.1,
        )));
        let window = ctx.content_rect().size();
        let maximized = ctx.input(|i| i.viewport().maximized) == Some(true);
        if window.x < width && !maximized {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                width, window.y,
            )));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

    const EN: LayoutKey = (Lang::En, 1400);

    /// Laid out at the window's floor: a 620 px window, the panel at 260,
    /// 16 px of margins, so the centre has 344.
    fn at_620() -> MinWidth {
        let mut w = MinWidth::default();
        w.menu(620.0, 604.0, 300.0);
        w.source_panel(260.0);
        w.centre(EN, 620.0, 344.0);
        w
    }

    #[test]
    fn with_everything_fitting_the_floor_holds() {
        let mut w = at_620();
        w.centre_content(344.0, 344.0, 344.0);
        assert_eq!(w.needed(620.0), 620.0);
        assert_eq!(w.panel_max(620.0), SOURCE_PANEL_MAX.min(620.0 - 16.0));
    }

    /// The menu bar's buttons, an update offered among them, are wider than
    /// the floor leaves: the window is made wide enough for them.
    #[test]
    fn a_wider_menu_bar_widens_the_window() {
        let mut w = at_620();
        w.menu(620.0, 604.0, 740.0);
        assert_eq!(w.needed(620.0), 756.0);
    }

    /// A row of the centre needs 360 where it had 344: the source panel
    /// gives way first (down to its narrowest), and the window's minimum
    /// leaves room for the row beside the narrowest panel.
    #[test]
    fn a_row_too_wide_for_the_centre_narrows_the_panel_and_sets_the_minimum() {
        let mut w = at_620();
        w.centre_content(344.0, 344.0, 360.0);
        assert_eq!(w.panel_max(620.0), 620.0 - 16.0 - 360.0);
        assert_eq!(w.needed(620.0), 620.0);
        assert_eq!(w.needed(0.0), SOURCE_PANEL_MIN + 16.0 + 360.0);
        // In a narrower window still, the panel stays at its narrowest.
        assert_eq!(w.panel_max(500.0), SOURCE_PANEL_MIN);
    }

    /// Laid out narrower than the space (the 440 cap, or a scroll bar), a
    /// row too wide needs the difference too.
    #[test]
    fn what_the_centre_needs_counts_its_scroll_bar() {
        let mut w = at_620();
        w.centre_content(344.0, 332.0, 352.0);
        assert_eq!(w.needed(0.0), SOURCE_PANEL_MIN + 16.0 + 364.0);
    }

    /// What the centre needs is kept while the layout stays the same -- a
    /// row that fits now still needs its width -- and learned again when
    /// the language or text size changes.
    #[test]
    fn what_the_centre_needs_is_relearned_for_another_language_or_text_size() {
        let mut w = at_620();
        w.centre_content(344.0, 344.0, 360.0);
        w.centre(EN, 700.0, 424.0);
        w.centre_content(424.0, 424.0, 424.0);
        assert_eq!(w.panel_max(700.0), 700.0 - 16.0 - 360.0);
        w.centre((Lang::Ja, 1400), 700.0, 424.0);
        assert_eq!(w.panel_max(700.0), SOURCE_PANEL_MAX);
    }
}
