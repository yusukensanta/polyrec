use eframe::egui;

/// The dashboard's colours, for the theme Windows is in -- see `pal` and
/// `ThemeWatch`. PolyRec's own indigo throughout: indigo marks what's
/// selected and what you can press, red means recording (and errors),
/// amber warns, green only ever says something went well.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Palette {
    /// Text fields' and checkboxes' insides.
    pub(super) bg_deep: egui::Color32,
    /// Pop-ups.
    pub(super) bg_window: egui::Color32,
    /// Status boxes.
    pub(super) bg_faint: egui::Color32,
    /// Panels -- the window itself.
    pub(super) bg_base: egui::Color32,
    pub(super) bg_card: egui::Color32,
    /// A hovered, not-yet-selected source card: between resting and
    /// selected, so it reads as "about to be clickable".
    pub(super) bg_hover: egui::Color32,
    pub(super) bg_selected: egui::Color32,
    /// Separators and cards' edges.
    pub(super) border: egui::Color32,
    pub(super) border_hover: egui::Color32,
    /// Every button's outline at rest: at least 3:1 against what it sits
    /// on, so a button reads as one -- the Close buttons' shapes used to all
    /// but vanish (1.07:1) -- but not much more, or a dashboard full of
    /// them is harsh on the eye (they were 6:1).
    pub(super) outline: egui::Color32,
    /// The outline of the button under the pointer, and the selected
    /// source card's.
    pub(super) outline_strong: egui::Color32,
    /// A button's fill at rest: a shade off what it sits on, so its shape
    /// shows without a loud outline.
    pub(super) bg_button: egui::Color32,
    /// A button's text, in the outline's indigo.
    pub(super) link: egui::Color32,
    /// A button's fill while hovered.
    pub(super) hover_tint: egui::Color32,
    pub(super) text_primary: egui::Color32,
    pub(super) text_muted: egui::Color32,
    /// The main actions' fill (REC, RESUME, Export) and a selected toggle's;
    /// `on_accent` is their text.
    pub(super) accent: egui::Color32,
    pub(super) on_accent: egui::Color32,
    /// Indigo as text, e.g. PAUSED.
    pub(super) accent_text: egui::Color32,
    /// REC's record dot, on its indigo fill.
    pub(super) rec_dot: egui::Color32,
    /// Recording live -- the dot, RECORDING -- and errors.
    pub(super) danger: egui::Color32,
    /// STOP's fill, and its text.
    pub(super) danger_fill: egui::Color32,
    pub(super) on_danger: egui::Color32,
    /// REC's fill and text while it can't be pressed: flat and grey, where
    /// egui's half-transparent fade made its indigo look hazy.
    pub(super) disabled_fill: egui::Color32,
    pub(super) disabled_text: egui::Color32,
    /// Warnings: a notice's edge, and warning lines' text; a notice's fill.
    pub(super) warn: egui::Color32,
    pub(super) warn_bg: egui::Color32,
    /// Something done: a recording saved, an app running.
    pub(super) success: egui::Color32,
    /// A level meter's bar below -12 dBFS (`warn`, then `danger`, above).
    pub(super) meter: egui::Color32,
    /// The recording HUD overlay's translucent backdrop (premultiplied
    /// alpha). The HUDs sit over games, so they're dark in every theme.
    pub(super) bg_overlay: egui::Color32,
    /// Whether egui's dark widgets are the base.
    pub(super) dark: bool,
}

const fn rgb(r: u8, g: u8, b: u8) -> egui::Color32 {
    egui::Color32::from_rgb(r, g, b)
}

impl Palette {
    /// Deep ink with indigo. WCAG 2.2 AA throughout (see `palette_tests`).
    pub(super) const DARK: Self = Self {
        bg_deep: rgb(0x0f, 0x10, 0x17),
        bg_window: rgb(0x19, 0x1a, 0x25),
        bg_faint: rgb(0x18, 0x19, 0x23),
        bg_base: rgb(0x14, 0x15, 0x1d),
        bg_card: rgb(0x1c, 0x1d, 0x28),
        bg_hover: rgb(0x22, 0x24, 0x3a),
        bg_selected: rgb(0x25, 0x28, 0x56),
        border: rgb(0x2d, 0x2f, 0x40),
        border_hover: rgb(0x4a, 0x4f, 0x8a),
        outline: rgb(0x61, 0x66, 0xb0),
        outline_strong: rgb(0x8b, 0x91, 0xff),
        bg_button: rgb(0x1e, 0x20, 0x30),
        link: rgb(0xc3, 0xc6, 0xff),
        hover_tint: rgb(0x25, 0x28, 0x56),
        text_primary: rgb(0xee, 0xee, 0xf6),
        text_muted: rgb(0xa7, 0xa9, 0xbe),
        accent: rgb(0x5b, 0x61, 0xe6),
        on_accent: rgb(0xff, 0xff, 0xff),
        accent_text: rgb(0x9e, 0xa3, 0xff),
        rec_dot: rgb(0xff, 0x8a, 0x8a),
        danger: rgb(0xff, 0x6b, 0x6b),
        danger_fill: rgb(0xc9, 0x34, 0x3a),
        on_danger: rgb(0xff, 0xff, 0xff),
        disabled_fill: rgb(0x26, 0x28, 0x38),
        disabled_text: rgb(0x8a, 0x8d, 0xa3),
        warn: rgb(0xe8, 0xa2, 0x3c),
        warn_bg: rgb(0x2a, 0x22, 0x19),
        success: rgb(0x5d, 0xd3, 0x9e),
        meter: rgb(0x8b, 0x91, 0xff),
        bg_overlay: egui::Color32::from_rgba_premultiplied(20, 20, 20, 200),
        dark: true,
    };

    /// Soft blue-white with indigo. AA like `DARK`.
    pub(super) const LIGHT: Self = Self {
        bg_deep: rgb(0xff, 0xff, 0xff),
        bg_window: rgb(0xff, 0xff, 0xff),
        bg_faint: rgb(0xf9, 0xf9, 0xfd),
        bg_base: rgb(0xf4, 0xf5, 0xfa),
        bg_card: rgb(0xff, 0xff, 0xff),
        bg_hover: rgb(0xf0, 0xf1, 0xfb),
        bg_selected: rgb(0xec, 0xee, 0xff),
        border: rgb(0xdf, 0xe1, 0xec),
        border_hover: rgb(0xa9, 0xac, 0xd9),
        outline: rgb(0x81, 0x84, 0xe2),
        outline_strong: rgb(0x4b, 0x50, 0xd6),
        bg_button: rgb(0xff, 0xff, 0xff),
        link: rgb(0x3d, 0x42, 0xc4),
        hover_tint: rgb(0xec, 0xee, 0xff),
        text_primary: rgb(0x1d, 0x1e, 0x2c),
        text_muted: rgb(0x5b, 0x5e, 0x73),
        accent: rgb(0x4b, 0x50, 0xd6),
        on_accent: rgb(0xff, 0xff, 0xff),
        accent_text: rgb(0x3d, 0x42, 0xc4),
        rec_dot: rgb(0xff, 0x8a, 0x8a),
        danger: rgb(0xc6, 0x2f, 0x35),
        danger_fill: rgb(0xc9, 0x34, 0x3a),
        on_danger: rgb(0xff, 0xff, 0xff),
        disabled_fill: rgb(0xe4, 0xe5, 0xee),
        disabled_text: rgb(0x77, 0x7a, 0x8f),
        warn: rgb(0x9a, 0x5b, 0x00),
        warn_bg: rgb(0xff, 0xf4, 0xe0),
        success: rgb(0x1f, 0x7a, 0x45),
        meter: rgb(0x4b, 0x50, 0xd6),
        bg_overlay: Self::DARK.bg_overlay,
        dark: false,
    };

    /// Every colour converted by `f`: for the monitor (`color_profile`).
    pub(super) fn through(self, mut f: impl FnMut(egui::Color32) -> egui::Color32) -> Self {
        // Every field by name, so a colour added later can't be left out.
        let Self {
            bg_deep,
            bg_window,
            bg_faint,
            bg_base,
            bg_card,
            bg_hover,
            bg_selected,
            border,
            border_hover,
            outline,
            outline_strong,
            bg_button,
            link,
            hover_tint,
            text_primary,
            text_muted,
            accent,
            on_accent,
            accent_text,
            rec_dot,
            danger,
            danger_fill,
            on_danger,
            disabled_fill,
            disabled_text,
            warn,
            warn_bg,
            success,
            meter,
            bg_overlay,
            dark,
        } = self;
        Self {
            bg_deep: f(bg_deep),
            bg_window: f(bg_window),
            bg_faint: f(bg_faint),
            bg_base: f(bg_base),
            bg_card: f(bg_card),
            bg_hover: f(bg_hover),
            bg_selected: f(bg_selected),
            border: f(border),
            border_hover: f(border_hover),
            outline: f(outline),
            outline_strong: f(outline_strong),
            bg_button: f(bg_button),
            link: f(link),
            hover_tint: f(hover_tint),
            text_primary: f(text_primary),
            text_muted: f(text_muted),
            accent: f(accent),
            on_accent: f(on_accent),
            accent_text: f(accent_text),
            rec_dot: f(rec_dot),
            danger: f(danger),
            danger_fill: f(danger_fill),
            on_danger: f(on_danger),
            disabled_fill: f(disabled_fill),
            disabled_text: f(disabled_text),
            warn: f(warn),
            warn_bg: f(warn_bg),
            success: f(success),
            meter: f(meter),
            bg_overlay: f(bg_overlay),
            dark,
        }
    }

    /// Every colour, in field order.
    #[cfg(test)]
    fn colours(&self) -> Vec<egui::Color32> {
        let mut colours = Vec::new();
        self.through(|c| {
            colours.push(c);
            c
        });
        colours
    }

    /// The palette for Windows' theme (`dark`), or its own colours when a
    /// high-contrast theme is on -- which wins whatever the theme. Windows'
    /// accent colour isn't used: PolyRec keeps its indigo.
    pub(super) fn choose(dark: bool, high_contrast: Option<SystemColors>) -> Self {
        match high_contrast {
            Some(system) => Self::high_contrast(system),
            None if dark => Self::DARK,
            None => Self::LIGHT,
        }
    }

    /// Windows' high-contrast colours, and nothing else: no dimmed text,
    /// outlines in the text colour so every edge shows.
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
            outline: c.window_text,
            outline_strong: c.hotlight,
            bg_button: c.window,
            link: c.hotlight,
            hover_tint: c.window,
            text_primary: c.window_text,
            text_muted: c.window_text,
            accent: c.highlight,
            on_accent: c.highlight_text,
            accent_text: c.hotlight,
            rec_dot: Self::DARK.rec_dot,
            danger: c.window_text,
            danger_fill: c.highlight,
            on_danger: c.highlight_text,
            disabled_fill: c.window,
            disabled_text: c.window_text,
            warn: c.window_text,
            warn_bg: c.window,
            success: c.window_text,
            meter: c.highlight,
            bg_overlay: Self::DARK.bg_overlay,
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
#[cfg(test)]
fn contrast(a: egui::Color32, b: egui::Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
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
/// hears from the window), a high-contrast theme, and whether animations
/// are on. The last two are asked of Windows every `CHECK_EVERY`. The
/// palette is converted for the dashboard's monitor (`MonitorColors`).
pub(super) struct ThemeWatch {
    settings: Option<windows::UI::ViewManagement::UISettings>,
    checked_at: Option<std::time::Instant>,
    high_contrast: Option<SystemColors>,
    animations: bool,
    monitor_colors: super::color_profile::MonitorColors,
    applied: Option<Palette>,
}

impl ThemeWatch {
    const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

    pub(super) fn new() -> Self {
        Self {
            settings: windows::UI::ViewManagement::UISettings::new()
                .inspect_err(|e| tracing::warn!("Windows' animation setting isn't followed: {e}"))
                .ok(),
            checked_at: None,
            high_contrast: None,
            animations: true,
            monitor_colors: Default::default(),
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

        self.monitor_colors
            .update(super::hud_place::dashboard_monitor());
        let palette = match self.high_contrast {
            // Windows' own colours, as Windows gives them.
            Some(_) => Palette::choose(dark, self.high_contrast),
            None => self.monitor_colors.convert(Palette::choose(dark, None)),
        };
        if self.applied != Some(palette) {
            PALETTE.with(|p| p.set(palette));
            ctx.set_visuals(visuals_for(&palette));
            self.applied = Some(palette);
        }
    }
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

/// egui's visuals in `p`'s colours. Every button and checkbox is outlined
/// quietly in `outline` at rest, on `bg_button`, and in `outline_strong`
/// under the pointer; text takes each widget state's colour rather than one
/// for everything, so a selected toggle can be `on_accent` on `accent`.
fn visuals_for(p: &Palette) -> egui::Visuals {
    let mut v = if p.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };

    // Background layers
    v.panel_fill = p.bg_base;
    v.window_fill = p.bg_window;
    v.window_stroke.color = p.border;
    v.extreme_bg_color = p.bg_deep;
    v.faint_bg_color = p.bg_faint;
    v.override_text_color = None;
    v.hyperlink_color = p.link;
    v.selection.bg_fill = p.accent;
    v.selection.stroke.color = p.on_accent;

    // Window chrome
    v.window_corner_radius = egui::CornerRadius::same(10);

    let w = &mut v.widgets;
    // Labels and separators.
    w.noninteractive.fg_stroke.color = p.text_primary;
    w.noninteractive.bg_stroke.color = p.border;
    // Buttons, checkboxes, sliders, text fields: resting, hovered, pressed.
    for (state, fill, outline) in [
        (&mut w.inactive, p.bg_button, p.outline),
        (&mut w.hovered, p.hover_tint, p.outline_strong),
        (&mut w.active, p.bg_selected, p.outline_strong),
        (&mut w.open, p.bg_selected, p.outline_strong),
    ] {
        state.weak_bg_fill = fill;
        state.bg_stroke = egui::Stroke::new(1.0, outline);
        state.fg_stroke.color = p.text_primary;
    }
    // A checkbox's box and a slider's rail.
    w.inactive.bg_fill = p.bg_deep;
    w.hovered.bg_fill = p.hover_tint;
    w.active.bg_fill = p.bg_selected;
    w.open.bg_fill = p.bg_selected;

    // Widget rounding — consistent across all interaction states
    let r = egui::CornerRadius::same(5);
    for state in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        state.corner_radius = r;
    }
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

    /// `TEXT_SCALE` is one for the whole process, and tests run in
    /// parallel: those that set it take turns, or one's 1.5 lands in the
    /// middle of another's 1.0.
    fn text_scale_turn() -> std::sync::MutexGuard<'static, ()> {
        static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
        TURN.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn egui_s_own_text_follows_the_windows_type_ramp() {
        // Buttons, checkboxes and plain labels use egui's styles, which
        // were left at egui's defaults (12.5 px body, 9 px small).
        let _turn = text_scale_turn();
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
        let _turn = text_scale_turn();
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
    /// (4.5:1 for normal text), and every control's shape against what it
    /// sits on at 3:1 (WCAG's rule for controls).
    fn assert_readable(p: &Palette, name: &str) {
        let backgrounds = [
            ("base", p.bg_base),
            ("card", p.bg_card),
            ("window", p.bg_window),
            ("faint", p.bg_faint),
            ("selected", p.bg_selected),
            ("hover", p.hover_tint),
            ("button", p.bg_button),
        ];
        for (text_name, text) in [
            ("primary", p.text_primary),
            ("muted", p.text_muted),
            ("button", p.link),
            ("accent", p.accent_text),
            ("danger", p.danger),
            ("warn", p.warn),
            ("success", p.success),
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
            (p.on_accent, p.accent, "REC/RESUME/selected toggle"),
            (p.on_danger, p.danger_fill, "STOP"),
            (p.text_primary, p.warn_bg, "notice"),
        ] {
            let ratio = contrast(text, bg);
            assert!(ratio >= 4.5, "{name}: {what} text is {ratio:.2}:1");
        }
        for (shape_name, shape) in [
            ("outline", p.outline),
            ("hovered outline", p.outline_strong),
            ("accent", p.accent),
            ("STOP", p.danger_fill),
        ] {
            for (bg_name, bg) in &backgrounds[..4] {
                let ratio = contrast(shape, *bg);
                assert!(
                    ratio >= 3.0,
                    "{name}: {shape_name} against {bg_name} is {ratio:.2}:1"
                );
            }
        }
    }

    #[test]
    fn the_dark_and_light_palettes_are_readable() {
        assert_readable(&Palette::DARK, "dark");
        assert_readable(&Palette::LIGHT, "light");
        const { assert!(Palette::DARK.dark && !Palette::LIGHT.dark) };
    }

    /// A conversion for the monitor (`color_profile`) has to reach every
    /// colour, REC's dot included, or that one shows unconverted.
    #[test]
    fn a_conversion_reaches_every_colour() {
        let magenta = egui::Color32::from_rgb(255, 0, 255);
        for p in [Palette::DARK, Palette::LIGHT] {
            let converted = p.through(|_| magenta);
            assert!(converted.colours().iter().all(|&c| c == magenta));
            assert_eq!(converted.dark, p.dark);
            assert_eq!(converted.rec_dot, magenta);
        }
    }

    #[test]
    fn the_palette_follows_windows_theme_and_keeps_polyrec_s_indigo() {
        assert_eq!(Palette::choose(true, None), Palette::DARK);
        assert_eq!(Palette::choose(false, None), Palette::LIGHT);
    }

    #[test]
    fn every_button_is_outlined_and_a_selected_toggle_is_white_on_indigo() {
        for p in [Palette::DARK, Palette::LIGHT] {
            let v = visuals_for(&p);
            // The Close buttons' shapes all but vanished: no outline, and a
            // fill within 1.07:1 of the pop-up behind them.
            assert_eq!(v.widgets.inactive.bg_stroke.color, p.outline);
            assert_eq!(v.widgets.inactive.bg_stroke.width, 1.0);
            // A single text colour for everything used to paint a selected
            // toggle's label in it, whatever its fill.
            assert_eq!(v.override_text_color, None);
            assert_eq!(v.selection.bg_fill, p.accent);
            assert_eq!(v.selection.stroke.color, p.on_accent);
            assert_eq!(v.widgets.noninteractive.fg_stroke.color, p.text_primary);
        }
    }

    /// Outlined at 6:1, a dashboard of buttons was harsh on the eye: at
    /// rest they're outlined just past the 3:1 a control needs to read as
    /// one, on a fill of their own, and light up when the pointer is on one.
    #[test]
    fn buttons_rest_quietly_and_light_up_under_the_pointer() {
        for (p, name) in [(Palette::DARK, "dark"), (Palette::LIGHT, "light")] {
            for bg in [p.bg_base, p.bg_card, p.bg_window, p.bg_faint] {
                let ratio = contrast(p.outline, bg);
                assert!(ratio <= 4.0, "{name}: resting outline is {ratio:.2}:1");
            }
            let v = visuals_for(&p);
            assert_eq!(v.widgets.inactive.weak_bg_fill, p.bg_button);
            assert_ne!(p.bg_button, p.bg_base, "{name}: a button has a body");
            for state in [&v.widgets.hovered, &v.widgets.active, &v.widgets.open] {
                assert_eq!(state.bg_stroke.color, p.outline_strong);
            }
            assert!(contrast(p.outline_strong, p.bg_base) > contrast(p.outline, p.bg_base));
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
        let p = Palette::choose(false, Some(system));
        assert_eq!(p.bg_base, black);
        assert_eq!(p.text_primary, white);
        assert_eq!(p.text_muted, white, "no dimmed text in high contrast");
        assert_eq!(p.accent, cyan);
        assert_eq!(p.on_accent, black);
        assert_eq!(p.link, yellow);
        assert_eq!(p.outline, white);
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
