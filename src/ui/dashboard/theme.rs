use eframe::egui;

// WCAG 2.2 AA palette — all contrast ratios verified against BG_BASE (rgb 18,18,28)
pub(super) const BG_DEEP: egui::Color32 = egui::Color32::from_rgb(10, 10, 16);
pub(super) const BG_WINDOW: egui::Color32 = egui::Color32::from_rgb(22, 22, 34);
pub(super) const BG_FAINT: egui::Color32 = egui::Color32::from_rgb(14, 14, 22);
pub(super) const BG_BASE: egui::Color32 = egui::Color32::from_rgb(18, 18, 28);
pub(super) const BG_CARD: egui::Color32 = egui::Color32::from_rgb(26, 26, 40);
// Midpoint between BG_CARD and BG_SELECTED -- a hovered, not-yet-selected
// source card should read as "about to be clickable", distinct from both
// resting and selected states.
pub(super) const BG_HOVER: egui::Color32 = egui::Color32::from_rgb(32, 32, 53);
pub(super) const BG_SELECTED: egui::Color32 = egui::Color32::from_rgb(38, 38, 66);
pub(super) const BORDER: egui::Color32 = egui::Color32::from_rgb(40, 40, 60);
pub(super) const BORDER_HOVER: egui::Color32 = egui::Color32::from_rgb(65, 65, 125);
pub(super) const BORDER_SEL: egui::Color32 = egui::Color32::from_rgb(90, 90, 190);
pub(super) const ACCENT_SECONDARY: egui::Color32 = egui::Color32::from_rgb(0x86, 0x86, 0xCF);
pub(super) const TEXT_PRIMARY: egui::Color32 = egui::Color32::from_rgb(220, 220, 235);
// 140/140/165, not 130/130/155: the darker value passed WCAG AA (4.5:1) on panel
// backgrounds but only hit 4.49:1 on button fill (28,28,44) -- brightening it here
// only ever increases contrast on every other background it's already used on.
pub(super) const TEXT_MUTED: egui::Color32 = egui::Color32::from_rgb(140, 140, 165);
pub(super) const ACCENT_REC: egui::Color32 = egui::Color32::from_rgb(248, 80, 80);
pub(super) const ACCENT_IDLE: egui::Color32 = egui::Color32::from_rgb(74, 222, 128);
pub(super) const ACCENT_PAUSE: egui::Color32 = egui::Color32::from_rgb(224, 178, 56);
pub(super) const BG_BTN_STOP: egui::Color32 = egui::Color32::from_rgb(52, 18, 18);
pub(super) const BG_BTN_IDLE: egui::Color32 = egui::Color32::from_rgb(18, 46, 28);
/// The small pause button next to STOP.
pub(super) const BG_BTN_PAUSE: egui::Color32 = egui::Color32::from_rgb(30, 30, 46);
/// The recording HUD overlay's translucent backdrop (premultiplied alpha).
pub(super) const BG_OVERLAY: egui::Color32 =
    egui::Color32::from_rgba_premultiplied(20, 20, 20, 200);
/// Larger, pill-like rounding used only for the primary REC/STOP/RESUME
/// action buttons, so they read as the one clearly primary action instead
/// of blending into every other rounded rectangle in the UI.
pub(super) const ROUNDING_PRIMARY_BTN: u8 = 14;
/// Every other rounded rectangle (source cards, the mini pause button) --
/// one deliberate secondary tier instead of incidentally falling through to
/// whatever the theme's own default happens to be.
pub(super) const CORNER_CONTROL: u8 = 6;

// Named type scale -- consolidates what had drifted into seven ad-hoc sizes
// (10/11/12/13/14/18/40) down to five. Roughly modeled on Fluent 2's type
// ramp (Microsoft's current Windows design system: Caption/Body/Subtitle/
// Title steps), not copied literally -- this app's own already-similar
// numbers just needed names, not a redesign.
pub(super) const TEXT_CAPTION: f32 = 11.0; // status lines, tooltips, secondary info
pub(super) const TEXT_BODY: f32 = 13.0; // popup body copy, prompts
pub(super) const TEXT_SUBTITLE: f32 = 14.0; // section headers
pub(super) const TEXT_BUTTON: f32 = 18.0; // primary action button labels
pub(super) const TEXT_DISPLAY: f32 = 40.0; // the recording timer

/// Related sub-lines within one status concept (e.g. the Highlight-save
/// outcome lines) -- Fluent 2's own spacing scale has a 2px/XXS step too,
/// this just names it instead of leaving it as an unexplained literal.
pub(super) const SPACE_TIGHT: f32 = 4.0;
/// Between distinct concepts/sections that aren't tightly related.
pub(super) const SPACE_NORMAL: f32 = 8.0;

/// Fixed width shared by every modal popup (Quality, Hotkeys). One constant
/// (not separate literals) is what guarantees they match; setting both
/// `min_width` and `max_width` to it pins the Window so it can't grow --
/// egui's Resize area auto-expands to whatever the widest content seen so
/// far was and never shrinks back, so without this the Hotkeys popup would
/// permanently widen the first time its longer "press any key" prompt
/// rendered, and never return to `default_width` afterward.
pub(super) const POPUP_WIDTH: f32 = 320.0;

/// The product typeface: IBM Plex Sans for Latin text, IBM Plex Sans JP for
/// Japanese. Both are compiled into the exe, so the UI looks the same on
/// every machine. They are the unmodified upstream files: the SIL OFL 1.1
/// they ship under reserves the name "Plex", so a subset (a modified version)
/// would have to be renamed. See `assets/fonts/README.md` for the upstream
/// release, checksums and license.
const PLEX_SANS_KEY: &str = "IBMPlexSans-Regular";
const PLEX_SANS_JP_KEY: &str = "IBMPlexSansJP-Regular";
const PLEX_SANS: &[u8] = include_bytes!("../../../assets/fonts/IBMPlexSans-Regular.ttf");
const PLEX_SANS_JP: &[u8] = include_bytes!("../../../assets/fonts/IBMPlexSansJP-Regular.ttf");
/// egui's own Latin font: first in Proportional, and the symbol fallback
/// (√ etc.) right after Hack in Monospace.
const EGUI_LATIN_FONT_KEY: &str = "Ubuntu-Light";

pub(super) fn setup_fonts(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());
}

/// egui's default fonts with IBM Plex in place of Ubuntu-Light:
/// - Plex Sans takes Ubuntu-Light's slot in both families, so it's the
///   Proportional face and stays Monospace's symbol fallback after Hack (the
///   recording timer keeps its Hack digits).
/// - Plex Sans JP goes last in both, after egui's icon/emoji fonts, so the
///   theme-tinted UI icons keep coming from those even where the much larger
///   CJK font also has a glyph (e.g. the REC dot ● or ⬆).
///
/// Anything still missing (e.g. Hangul or a color emoji in a window title) is
/// left to eframe's on-demand system-font fallback.
fn font_definitions() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.remove(EGUI_LATIN_FONT_KEY);
    for (key, bytes) in [(PLEX_SANS_KEY, PLEX_SANS), (PLEX_SANS_JP_KEY, PLEX_SANS_JP)] {
        fonts
            .font_data
            .insert(key.to_owned(), egui::FontData::from_static(bytes).into());
    }
    for names in fonts.families.values_mut() {
        for name in names.iter_mut().filter(|name| *name == EGUI_LATIN_FONT_KEY) {
            *name = PLEX_SANS_KEY.to_owned();
        }
        names.push(PLEX_SANS_JP_KEY.to_owned());
    }
    fonts
}

pub(super) fn setup_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();

    // Background layers
    v.panel_fill = BG_BASE;
    v.window_fill = BG_WINDOW;
    v.extreme_bg_color = BG_DEEP;
    v.faint_bg_color = BG_FAINT;
    v.override_text_color = Some(TEXT_PRIMARY);

    // Window chrome
    v.window_corner_radius = egui::CornerRadius::same(10);

    // Widget rounding — consistent across all interaction states
    let r = egui::CornerRadius::same(5);
    v.widgets.noninteractive.corner_radius = r;
    v.widgets.inactive.corner_radius = r;
    v.widgets.hovered.corner_radius = r;
    v.widgets.active.corner_radius = r;
    v.widgets.open.corner_radius = r;

    // Subtle hover/active bg fills for checkboxes, buttons, etc.
    v.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(28, 28, 44);
    v.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(38, 38, 58);
    v.widgets.active.weak_bg_fill = egui::Color32::from_rgb(48, 48, 72);

    ctx.set_visuals(v);

    let mut s = (*ctx.global_style()).clone();
    // egui's default interact_size.y (18.0) is well under Fluent's 32px / Material's
    // 36-40dp desktop minimum control height — every button in the app (Refresh,
    // Browse, Quality, Close, etc.) was inheriting that undersized floor. 32.0 is
    // the Fluent minimum and lands on the 8px spacing grid used everywhere else here.
    s.spacing.interact_size = egui::Vec2::new(40.0, 32.0);
    s.spacing.item_spacing = egui::Vec2::new(8.0, 8.0);
    s.spacing.button_padding = egui::Vec2::new(14.0, 8.0);
    s.spacing.window_margin = egui::Margin::same(16);
    s.spacing.indent = 16.0;
    ctx.set_global_style(s);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::self_update::hex_encode;
    use sha2::{Digest, Sha256};

    /// The OFL's Reserved Font Name only covers the unmodified files, so pin
    /// the exact upstream bytes listed in `assets/fonts/README.md`.
    #[test]
    fn bundled_plex_fonts_are_the_unmodified_upstream_files() {
        let sha256 = |bytes: &[u8]| hex_encode(Sha256::digest(bytes).as_slice());
        assert_eq!(
            sha256(PLEX_SANS),
            "975dcda37d80f038dcd143c22e33ca2d97a0cc5a929aace1c749153b0fe1afa5"
        );
        assert_eq!(
            sha256(PLEX_SANS_JP),
            "0168493ae143d16e4c0d374c090b1ee5eab9ee475536d2e429368dbcfc4a6abe"
        );
    }

    #[test]
    fn plex_takes_ubuntu_lights_slot_and_japanese_goes_last() {
        let fonts = font_definitions();
        let family = |family| fonts.families[&family].as_slice();
        let proportional = family(egui::FontFamily::Proportional);
        let monospace = family(egui::FontFamily::Monospace);

        assert_eq!(proportional.first().unwrap(), PLEX_SANS_KEY);
        assert_eq!(monospace[..2], ["Hack", PLEX_SANS_KEY]);
        for names in [proportional, monospace] {
            assert_eq!(names.last().unwrap(), PLEX_SANS_JP_KEY);
            assert!(!names.iter().any(|name| name == EGUI_LATIN_FONT_KEY));
        }
        assert!(!fonts.font_data.contains_key(EGUI_LATIN_FONT_KEY));
    }

    /// Every non-ASCII character in the UI strings (the Japanese UI, the
    /// "日本語" toggle, typographic punctuation) renders from the bundled fonts
    /// alone -- without relying on whatever system fonts the user has.
    #[test]
    fn bundled_fonts_cover_every_ui_string() {
        let ctx = egui::Context::default();
        ctx.set_fonts(font_definitions());
        // One frame loads the fonts. No renderer is attached, so discard its
        // texture uploads (egui asserts if they're silently dropped).
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
        output.textures_delta.clear();

        let ui_text: String = include_str!("../../i18n.rs")
            .chars()
            .filter(|c| !c.is_ascii_control() && !c.is_whitespace())
            .collect();
        let font = egui::FontId::proportional(TEXT_BODY);
        let missing: String = ctx.fonts_mut(|fonts| {
            ui_text
                .chars()
                .filter(|&c| !fonts.has_glyph(&font, c))
                .collect()
        });
        assert_eq!(missing, "", "characters with no glyph in the bundled fonts");
    }
}
