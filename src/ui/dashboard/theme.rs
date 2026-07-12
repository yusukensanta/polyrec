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

pub(super) fn setup_fonts(ctx: &egui::Context) {
    const CJK_FONT_PATH: &str = r"C:\Windows\Fonts\msgothic.ttc";
    const CJK_FONT_KEY: &str = "cjk_fallback";

    let font_bytes = match std::fs::read(CJK_FONT_PATH) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!("CJK fallback font not loaded from {CJK_FONT_PATH}: {e}");
            return;
        }
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        CJK_FONT_KEY.to_owned(),
        egui::FontData::from_owned(font_bytes).into(),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push(CJK_FONT_KEY.to_owned());
    }
    ctx.set_fonts(fonts);
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
