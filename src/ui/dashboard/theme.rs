use eframe::egui;

/// The dashboard's colours, for the theme Windows is in -- see `pal` and
/// `ThemeWatch`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Palette {
    pub(super) bg_deep: egui::Color32,
    pub(super) bg_window: egui::Color32,
    pub(super) bg_faint: egui::Color32,
    pub(super) bg_base: egui::Color32,
    pub(super) bg_card: egui::Color32,
    /// A hovered, not-yet-selected source card: between resting and
    /// selected, so it reads as "about to be clickable".
    pub(super) bg_hover: egui::Color32,
    pub(super) bg_selected: egui::Color32,
    pub(super) border: egui::Color32,
    pub(super) border_hover: egui::Color32,
    pub(super) border_sel: egui::Color32,
    pub(super) accent_secondary: egui::Color32,
    pub(super) text_primary: egui::Color32,
    pub(super) text_muted: egui::Color32,
    pub(super) accent_rec: egui::Color32,
    pub(super) accent_idle: egui::Color32,
    pub(super) accent_pause: egui::Color32,
    pub(super) bg_btn_stop: egui::Color32,
    pub(super) bg_btn_idle: egui::Color32,
    /// The small pause button next to STOP.
    pub(super) bg_btn_pause: egui::Color32,
    /// The recording HUD overlay's translucent backdrop (premultiplied
    /// alpha). The HUDs sit over games, so they're dark in every theme.
    pub(super) bg_overlay: egui::Color32,
    /// Buttons' and checkboxes' fills: resting, hovered, pressed.
    pub(super) widget_inactive: egui::Color32,
    pub(super) widget_hovered: egui::Color32,
    pub(super) widget_active: egui::Color32,
    /// Selected toggles (H264/H265, ...) and the text on them.
    pub(super) selection: egui::Color32,
    pub(super) selection_text: egui::Color32,
    /// Whether egui's dark widgets are the base.
    pub(super) dark: bool,
}

const fn rgb(r: u8, g: u8, b: u8) -> egui::Color32 {
    egui::Color32::from_rgb(r, g, b)
}

impl Palette {
    /// WCAG 2.2 AA (see `palette_tests`). `text_muted` is 140/140/165, not
    /// 130/130/155: the darker value passed on panels but only hit 4.49:1 on
    /// a button's fill.
    pub(super) const DARK: Self = Self {
        bg_deep: rgb(10, 10, 16),
        bg_window: rgb(22, 22, 34),
        bg_faint: rgb(14, 14, 22),
        bg_base: rgb(18, 18, 28),
        bg_card: rgb(26, 26, 40),
        bg_hover: rgb(32, 32, 53),
        bg_selected: rgb(38, 38, 66),
        border: rgb(40, 40, 60),
        border_hover: rgb(65, 65, 125),
        border_sel: rgb(90, 90, 190),
        accent_secondary: rgb(0x86, 0x86, 0xCF),
        text_primary: rgb(220, 220, 235),
        text_muted: rgb(140, 140, 165),
        accent_rec: rgb(248, 80, 80),
        accent_idle: rgb(74, 222, 128),
        accent_pause: rgb(224, 178, 56),
        bg_btn_stop: rgb(52, 18, 18),
        bg_btn_idle: rgb(18, 46, 28),
        bg_btn_pause: rgb(30, 30, 46),
        bg_overlay: egui::Color32::from_rgba_premultiplied(20, 20, 20, 200),
        widget_inactive: rgb(28, 28, 44),
        widget_hovered: rgb(38, 38, 58),
        widget_active: rgb(48, 48, 72),
        selection: rgb(0, 92, 128),
        selection_text: rgb(210, 210, 210),
        dark: true,
    };

    /// Windows 11's light colours: its light-theme backgrounds, and its
    /// success/caution/critical colours (#0F7B0F, #9D5D00, #C42B1C, on
    /// #DFF6DD/#FDE7E9) for REC, Paused and STOP. AA like `DARK`.
    pub(super) const LIGHT: Self = Self {
        bg_deep: rgb(255, 255, 255),
        bg_window: rgb(249, 249, 249),
        bg_faint: rgb(250, 250, 250),
        bg_base: rgb(243, 243, 243),
        bg_card: rgb(251, 251, 251),
        bg_hover: rgb(240, 240, 245),
        bg_selected: rgb(230, 234, 245),
        border: rgb(224, 224, 224),
        border_hover: rgb(180, 180, 200),
        border_sel: rgb(0, 95, 184),
        accent_secondary: rgb(0, 84, 166),
        text_primary: rgb(27, 27, 27),
        text_muted: rgb(96, 96, 96),
        accent_rec: rgb(196, 43, 28),
        accent_idle: rgb(15, 123, 15),
        accent_pause: rgb(157, 93, 0),
        bg_btn_stop: rgb(253, 231, 233),
        bg_btn_idle: rgb(223, 246, 221),
        bg_btn_pause: rgb(235, 235, 235),
        bg_overlay: Self::DARK.bg_overlay,
        widget_inactive: rgb(251, 251, 251),
        widget_hovered: rgb(245, 245, 245),
        widget_active: rgb(238, 238, 238),
        selection: rgb(0, 95, 184),
        selection_text: rgb(255, 255, 255),
        dark: false,
    };

    /// The palette for Windows' theme (`dark`), or its own colours when a
    /// high-contrast theme is on -- which wins whatever the theme. Windows'
    /// accent colour, if known, marks what's selected.
    pub(super) fn choose(
        dark: bool,
        high_contrast: Option<SystemColors>,
        accent: Option<egui::Color32>,
    ) -> Self {
        if let Some(system) = high_contrast {
            return Self::high_contrast(system);
        }
        let base = if dark { Self::DARK } else { Self::LIGHT };
        match accent {
            Some(accent) => Self {
                border_sel: accent,
                selection: shade_for_text(accent, base.text_primary, dark),
                selection_text: base.text_primary,
                ..base
            },
            None => base,
        }
    }

    /// Windows' high-contrast colours, and nothing else: no dimmed text,
    /// borders in the text colour so every edge shows.
    fn high_contrast(c: SystemColors) -> Self {
        Self {
            bg_deep: c.window,
            bg_window: c.window,
            bg_faint: c.window,
            bg_base: c.window,
            bg_card: c.window,
            bg_hover: c.window,
            bg_selected: c.window,
            border: c.window_text,
            border_hover: c.hotlight,
            border_sel: c.highlight,
            accent_secondary: c.hotlight,
            text_primary: c.window_text,
            text_muted: c.window_text,
            accent_rec: c.window_text,
            accent_idle: c.window_text,
            accent_pause: c.window_text,
            bg_btn_stop: c.window,
            bg_btn_idle: c.window,
            bg_btn_pause: c.window,
            bg_overlay: Self::DARK.bg_overlay,
            widget_inactive: c.window,
            widget_hovered: c.window,
            widget_active: c.window,
            selection: c.highlight,
            selection_text: c.highlight_text,
            dark: luminance(c.window) < 0.5,
        }
    }
}

/// A colour's relative luminance (WCAG), 0 (black) to 1 (white).
fn luminance(c: egui::Color32) -> f32 {
    let lin = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// WCAG 2.2's contrast ratio between two opaque colours.
fn contrast(a: egui::Color32, b: egui::Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `accent`, darkened on a `dark` theme (lightened on a light one) just
/// enough for `text` to read on it at WCAG AA. egui draws a selected toggle's
/// label in the theme's text colour, so the fill has to suit that colour --
/// Windows' own accent shades assume the opposite.
fn shade_for_text(accent: egui::Color32, text: egui::Color32, dark: bool) -> egui::Color32 {
    let toward = if dark {
        egui::Color32::BLACK
    } else {
        egui::Color32::WHITE
    };
    (0..=20)
        .map(|step| accent.lerp_to_gamma(toward, step as f32 / 20.0))
        .find(|&fill| contrast(fill, text) >= 4.5)
        .unwrap_or(toward)
}

/// Windows' own colours for a high-contrast theme (`GetSysColor`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct SystemColors {
    pub(super) window: egui::Color32,
    pub(super) window_text: egui::Color32,
    pub(super) highlight: egui::Color32,
    pub(super) highlight_text: egui::Color32,
    pub(super) button_face: egui::Color32,
    pub(super) button_text: egui::Color32,
    pub(super) hotlight: egui::Color32,
}

thread_local! {
    /// The palette in use; set on the UI thread by `ThemeWatch`.
    static PALETTE: std::cell::Cell<Palette> = const { std::cell::Cell::new(Palette::DARK) };
}

/// The colours to draw with.
pub(super) fn pal() -> Palette {
    PALETTE.with(std::cell::Cell::get)
}

/// Whether Windows' "Animation effects" setting is on (`apply_motion`).
static MOTION: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// egui's animation time while animations are on, its own default.
const ANIMATION_TIME: f32 = 1.0 / 12.0;

/// Turns egui's animations, and PolyRec's own (`motion`), on or off with
/// Windows' "Animation effects" setting.
pub(super) fn apply_motion(ctx: &egui::Context, animations: bool) {
    MOTION.store(animations, std::sync::atomic::Ordering::Relaxed);
    let mut style = (*ctx.global_style()).clone();
    style.animation_time = if animations { ANIMATION_TIME } else { 0.0 };
    ctx.set_global_style(style);
}

/// Whether to animate -- e.g. pulse the recording dot.
pub(super) fn motion() -> bool {
    MOTION.load(std::sync::atomic::Ordering::Relaxed)
}

/// Follows Windows' theme while PolyRec runs: light or dark (what egui
/// hears from the window), a high-contrast theme, the accent colour, and
/// whether animations are on. The last three are asked of Windows every
/// `CHECK_EVERY`.
pub(super) struct ThemeWatch {
    settings: Option<windows::UI::ViewManagement::UISettings>,
    checked_at: Option<std::time::Instant>,
    high_contrast: Option<SystemColors>,
    accent: Option<egui::Color32>,
    animations: bool,
    applied: Option<Palette>,
}

impl ThemeWatch {
    const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

    pub(super) fn new() -> Self {
        Self {
            settings: windows::UI::ViewManagement::UISettings::new()
                .inspect_err(|e| {
                    tracing::warn!("Windows' accent and animation settings aren't followed: {e}")
                })
                .ok(),
            checked_at: None,
            high_contrast: None,
            accent: None,
            animations: true,
            applied: None,
        }
    }

    /// Runs every frame; changes the dashboard's colours when Windows'
    /// change.
    pub(super) fn apply(&mut self, ctx: &egui::Context) {
        if self
            .checked_at
            .is_none_or(|at| at.elapsed() >= Self::CHECK_EVERY)
        {
            self.checked_at = Some(std::time::Instant::now());
            self.high_contrast = system_high_contrast();
            self.accent = self.settings.as_ref().and_then(accent);
            let animations = self
                .settings
                .as_ref()
                .and_then(|s| s.AnimationsEnabled().ok())
                .unwrap_or(true);
            if animations != self.animations || self.applied.is_none() {
                self.animations = animations;
                apply_motion(ctx, animations);
            }
        }
        let dark = ctx.input(|i| i.raw.system_theme) != Some(egui::Theme::Light);

        let palette = Palette::choose(dark, self.high_contrast, self.accent);
        if self.applied != Some(palette) {
            PALETTE.with(|p| p.set(palette));
            ctx.set_visuals(visuals_for(&palette));
            self.applied = Some(palette);
        }
    }
}

/// Windows' accent colour (Settings -> Personalization -> Colors).
fn accent(settings: &windows::UI::ViewManagement::UISettings) -> Option<egui::Color32> {
    let c = settings
        .GetColorValue(windows::UI::ViewManagement::UIColorType::Accent)
        .ok()?;
    Some(egui::Color32::from_rgb(c.R, c.G, c.B))
}

/// Windows' own colours, if a high-contrast theme is on.
fn system_high_contrast() -> Option<SystemColors> {
    use windows::Win32::Graphics::Gdi::{
        COLOR_BTNFACE, COLOR_BTNTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_HOTLIGHT,
        COLOR_WINDOW, COLOR_WINDOWTEXT, GetSysColor,
    };
    use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
    };
    let mut hc = HIGHCONTRASTW {
        cbSize: size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            hc.cbSize,
            Some((&raw mut hc).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .ok()?;
    if !hc.dwFlags.contains(HCF_HIGHCONTRASTON) {
        return None;
    }
    // COLORREF is 0x00BBGGRR.
    let color = |index| {
        let c = unsafe { GetSysColor(index) };
        egui::Color32::from_rgb(c as u8, (c >> 8) as u8, (c >> 16) as u8)
    };
    Some(SystemColors {
        window: color(COLOR_WINDOW),
        window_text: color(COLOR_WINDOWTEXT),
        highlight: color(COLOR_HIGHLIGHT),
        highlight_text: color(COLOR_HIGHLIGHTTEXT),
        button_face: color(COLOR_BTNFACE),
        button_text: color(COLOR_BTNTEXT),
        hotlight: color(COLOR_HOTLIGHT),
    })
}

/// Larger, pill-like rounding used only for the primary REC/STOP/RESUME
/// action buttons, so they read as the one clearly primary action instead
/// of blending into every other rounded rectangle in the UI.
pub(super) const ROUNDING_PRIMARY_BTN: u8 = 14;
/// Every other rounded rectangle (source cards, the mini pause button) --
/// one deliberate secondary tier instead of incidentally falling through to
/// whatever the theme's own default happens to be.
pub(super) const CORNER_CONTROL: u8 = 6;

// Named type scale, on Windows 11's type ramp ("Typography in Windows":
// caption 12, body 14, body large 18, title large 40 epx). 12 px is the
// smallest Windows considers legible; captions used to be 11 and egui's own
// body text 12.5. Shown through `text_px`, which follows Windows' text size
// setting.
pub(super) const TEXT_CAPTION: f32 = 12.0; // status lines, tooltips, secondary info
pub(super) const TEXT_BODY: f32 = 14.0; // popup body copy, prompts
pub(super) const TEXT_SUBTITLE: f32 = 14.0; // section headers
pub(super) const TEXT_BUTTON: f32 = 18.0; // primary action button labels
pub(super) const TEXT_DISPLAY: f32 = 40.0; // the recording timer

/// Related sub-lines within one status concept (e.g. the Highlight-save
/// outcome lines) -- Fluent 2's own spacing scale has a 2px/XXS step too,
/// this just names it instead of leaving it as an unexplained literal.
pub(super) const SPACE_TIGHT: f32 = 4.0;
/// Between distinct concepts/sections that aren't tightly related.
pub(super) const SPACE_NORMAL: f32 = 8.0;
/// Inside a card or box, between its edge and its content (Windows uses 12
/// between content areas and 16 to a surface's edge; 12 suits these small
/// cards).
pub(super) const SPACE_GROUP: f32 = 12.0;
/// Between the dashboard's sections.
pub(super) const SPACE_SECTION: f32 = 16.0;

/// Windows' text size setting (Settings -> Accessibility -> Text size,
/// 1.0 to 2.25) as `f32` bits, applied by `apply_text_scale`.
static TEXT_SCALE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3F80_0000);

/// `size` grown by Windows' text size setting `factor` the way its own
/// controls do it: small text by the whole factor, text above 20 px by half
/// as much, so a large timer doesn't crowd everything else out.
fn scaled(size: f32, factor: f32) -> f32 {
    let weight = if size <= 20.0 { 1.0 } else { 0.5 };
    size * (1.0 + (factor - 1.0) * weight)
}

/// A text size (`TEXT_*`) as shown, at Windows' text size setting.
pub(super) fn text_px(size: f32) -> f32 {
    scaled(
        size,
        f32::from_bits(TEXT_SCALE.load(std::sync::atomic::Ordering::Relaxed)),
    )
}

/// Sizes egui's own text -- buttons, checkboxes, plain labels -- on the
/// Windows type ramp at text size setting `factor`, and makes `text_px` use
/// `factor` too.
pub(super) fn apply_text_scale(ctx: &egui::Context, factor: f32) {
    TEXT_SCALE.store(factor.to_bits(), std::sync::atomic::Ordering::Relaxed);
    let mut style = (*ctx.global_style()).clone();
    for (text_style, size) in [
        (egui::TextStyle::Small, 12.0),
        (egui::TextStyle::Body, 14.0),
        (egui::TextStyle::Button, 14.0),
        (egui::TextStyle::Monospace, 13.0),
        (egui::TextStyle::Heading, 20.0),
    ] {
        if let Some(font) = style.text_styles.get_mut(&text_style) {
            font.size = scaled(size, factor);
        }
    }
    ctx.set_global_style(style);
}

/// Follows Windows' text size setting while PolyRec runs (`UISettings`
/// tells when it changes).
pub(super) struct TextScaleWatch {
    settings: windows::UI::ViewManagement::UISettings,
    changed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl TextScaleWatch {
    /// `None` where Windows doesn't offer the setting; text then stays at
    /// its own size.
    pub(super) fn start(ctx: &egui::Context) -> Option<Self> {
        let settings = windows::UI::ViewManagement::UISettings::new()
            .inspect_err(|e| tracing::warn!("Windows' text size setting isn't followed: {e}"))
            .ok()?;
        // Set: the first `apply` reads the current setting.
        let changed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (flag, wake) = (changed.clone(), ctx.clone());
        settings
            .TextScaleFactorChanged(&windows::Foundation::TypedEventHandler::new(move |_, _| {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
                wake.request_repaint();
                Ok(())
            }))
            .inspect_err(|e| tracing::warn!("text size changes won't be followed: {e}"))
            .ok();
        Some(Self { settings, changed })
    }

    /// Applies the setting, if it changed since the last call. Runs every
    /// frame.
    pub(super) fn apply(&self, ctx: &egui::Context) {
        if self
            .changed
            .swap(false, std::sync::atomic::Ordering::Relaxed)
            && let Ok(factor) = self.settings.TextScaleFactor()
        {
            apply_text_scale(ctx, factor as f32);
        }
    }
}

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
    super::icons::load_icon_font(&mut fonts);
    fonts
}

pub(super) fn setup_theme(ctx: &egui::Context) {
    ctx.set_visuals(visuals_for(&pal()));
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
    apply_text_scale(ctx, 1.0);
}

/// egui's visuals in `p`'s colours.
fn visuals_for(p: &Palette) -> egui::Visuals {
    let mut v = if p.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };

    // Background layers
    v.panel_fill = p.bg_base;
    v.window_fill = p.bg_window;
    v.extreme_bg_color = p.bg_deep;
    v.faint_bg_color = p.bg_faint;
    v.override_text_color = Some(p.text_primary);
    v.selection.bg_fill = p.selection;
    v.selection.stroke.color = p.selection_text;

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
    v.widgets.inactive.weak_bg_fill = p.widget_inactive;
    v.widgets.hovered.weak_bg_fill = p.widget_hovered;
    v.widgets.active.weak_bg_fill = p.widget_active;
    v
}

#[cfg(test)]
mod type_tests {
    use super::*;

    #[test]
    fn no_text_is_smaller_than_windows_smallest_legible_size() {
        // 12 px regular is Windows 11's minimum ("Typography in Windows").
        for size in [
            TEXT_CAPTION,
            TEXT_BODY,
            TEXT_SUBTITLE,
            TEXT_BUTTON,
            TEXT_DISPLAY,
        ] {
            assert!(size >= 12.0, "{size}");
        }
    }

    fn style_size(ctx: &egui::Context, style: egui::TextStyle) -> f32 {
        ctx.global_style().text_styles[&style].size
    }

    #[test]
    fn egui_s_own_text_follows_the_windows_type_ramp() {
        // Buttons, checkboxes and plain labels use egui's styles, which
        // were left at egui's defaults (12.5 px body, 9 px small).
        let ctx = egui::Context::default();
        apply_text_scale(&ctx, 1.0);
        assert_eq!(style_size(&ctx, egui::TextStyle::Body), 14.0);
        assert_eq!(style_size(&ctx, egui::TextStyle::Button), 14.0);
        assert_eq!(style_size(&ctx, egui::TextStyle::Small), 12.0);
        assert_eq!(style_size(&ctx, egui::TextStyle::Heading), 20.0);
    }

    #[test]
    fn windows_text_size_setting_grows_small_text_fully_and_large_text_less() {
        // Windows' own controls scale smaller text more than larger text.
        assert_eq!(scaled(14.0, 1.0), 14.0);
        assert_eq!(scaled(14.0, 1.5), 21.0);
        assert_eq!(scaled(12.0, 2.25), 27.0);
        assert_eq!(scaled(40.0, 1.5), 50.0, "half as much above 20 px");
        let ctx = egui::Context::default();
        apply_text_scale(&ctx, 1.5);
        assert_eq!(style_size(&ctx, egui::TextStyle::Body), 21.0);
        assert_eq!(text_px(TEXT_CAPTION), 18.0);
        apply_text_scale(&ctx, 1.0);
        assert_eq!(text_px(TEXT_CAPTION), TEXT_CAPTION);
    }
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

#[cfg(test)]
mod palette_tests {
    use super::*;

    /// Every text colour against every background it's drawn on, at WCAG AA
    /// (4.5:1 for normal text).
    fn assert_readable(p: &Palette, name: &str) {
        let backgrounds = [
            ("base", p.bg_base),
            ("card", p.bg_card),
            ("window", p.bg_window),
            ("faint", p.bg_faint),
            ("button", p.widget_inactive),
        ];
        for (text_name, text) in [
            ("primary", p.text_primary),
            ("muted", p.text_muted),
            ("secondary", p.accent_secondary),
            ("rec", p.accent_rec),
            ("idle", p.accent_idle),
            ("pause", p.accent_pause),
        ] {
            for (bg_name, bg) in backgrounds {
                let ratio = contrast(text, bg);
                assert!(
                    ratio >= 4.5,
                    "{name}: {text_name} on {bg_name} is {ratio:.2}:1"
                );
            }
        }
        for (text, bg, what) in [
            (p.accent_rec, p.bg_btn_stop, "STOP"),
            (p.accent_idle, p.bg_btn_idle, "REC"),
            (p.text_muted, p.bg_btn_pause, "pause"),
            (p.selection_text, p.selection, "selection"),
        ] {
            let ratio = contrast(text, bg);
            assert!(ratio >= 4.5, "{name}: {what} button is {ratio:.2}:1");
        }
    }

    #[test]
    fn the_dark_and_light_palettes_are_readable() {
        assert_readable(&Palette::DARK, "dark");
        assert_readable(&Palette::LIGHT, "light");
        const { assert!(Palette::DARK.dark && !Palette::LIGHT.dark) };
    }

    #[test]
    fn the_palette_follows_windows_theme_and_its_accent() {
        let green = egui::Color32::from_rgb(16, 137, 62);
        assert_eq!(Palette::choose(true, None, None), Palette::DARK);
        assert_eq!(Palette::choose(false, None, None), Palette::LIGHT);
        // The selected card's border is the accent itself; a selected
        // toggle's fill is the accent shaded until the theme's text -- which
        // egui draws every label in -- reads on it.
        let accented = Palette::choose(true, None, Some(green));
        assert_eq!(accented.border_sel, green);
        assert_eq!(accented.selection_text, Palette::DARK.text_primary);
        assert_ne!(
            accented.selection,
            Palette::DARK.selection,
            "the accent, not the default"
        );
        assert_readable(&accented, "dark with green accent");
        let navy = egui::Color32::from_rgb(0, 40, 120);
        let light = Palette::choose(false, None, Some(navy));
        assert_eq!(light.selection_text, Palette::LIGHT.text_primary);
        assert_readable(&light, "light with navy accent");
        for accent in [
            egui::Color32::YELLOW,
            egui::Color32::BLACK,
            egui::Color32::WHITE,
        ] {
            assert_readable(
                &Palette::choose(true, None, Some(accent)),
                "dark, any accent",
            );
            assert_readable(
                &Palette::choose(false, None, Some(accent)),
                "light, any accent",
            );
        }
    }

    #[test]
    fn a_high_contrast_theme_uses_windows_own_colours() {
        let black = egui::Color32::BLACK;
        let white = egui::Color32::WHITE;
        let yellow = egui::Color32::from_rgb(255, 255, 0);
        let cyan = egui::Color32::from_rgb(0, 255, 255);
        let system = SystemColors {
            window: black,
            window_text: white,
            highlight: cyan,
            highlight_text: black,
            button_face: black,
            button_text: white,
            hotlight: yellow,
        };
        // Whatever Windows' theme is, high contrast wins.
        let p = Palette::choose(false, Some(system), Some(egui::Color32::RED));
        assert_eq!(p.bg_base, black);
        assert_eq!(p.text_primary, white);
        assert_eq!(p.text_muted, white, "no dimmed text in high contrast");
        assert_eq!(p.selection, cyan);
        assert_eq!(p.selection_text, black);
        assert_eq!(p.accent_secondary, yellow);
        assert!(p.dark, "a black window is a dark theme");
        assert_readable(&p, "high contrast");
    }

    #[test]
    fn reduced_motion_turns_off_animation() {
        let ctx = egui::Context::default();
        apply_motion(&ctx, false);
        assert_eq!(ctx.global_style().animation_time, 0.0);
        assert!(!motion());
        apply_motion(&ctx, true);
        assert!(ctx.global_style().animation_time > 0.0);
        assert!(motion());
    }
}
