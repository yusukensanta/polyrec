//! The dashboard's icons: Windows' own icon font (Segoe Fluent Icons on
//! Windows 11, Segoe MDL2 Assets on Windows 10), read from the Windows fonts
//! folder at start-up -- it isn't PolyRec's to ship. Where neither is
//! installed, each icon falls back to an emoji, as before.

use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// The egui font family the icon font is loaded under.
const ICON_FAMILY: &str = "icons";

/// Whether the icon font was loaded (`load_icon_font`).
static ICON_FONT: AtomicBool = AtomicBool::new(false);

/// The icon font in `fonts_dir`: Windows 11's, else Windows 10's.
fn icon_font_path(fonts_dir: &Path) -> Option<PathBuf> {
    ["SegoeIcons.ttf", "segmdl2.ttf"]
        .into_iter()
        .map(|name| fonts_dir.join(name))
        .find(|path| path.is_file())
}

/// Adds Windows' icon font to `fonts` as the `icons` family, if installed.
pub(super) fn load_icon_font(fonts: &mut egui::FontDefinitions) {
    let fonts_dir = std::env::var_os("WINDIR")
        .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
        .join("Fonts");
    let Some(bytes) = icon_font_path(&fonts_dir).and_then(|path| {
        std::fs::read(&path)
            .inspect_err(|e| tracing::warn!("icon font {}: {e}", path.display()))
            .ok()
    }) else {
        tracing::info!("no Windows icon font; icons are emoji");
        return;
    };
    fonts.font_data.insert(
        ICON_FAMILY.to_owned(),
        egui::FontData::from_owned(bytes).into(),
    );
    fonts.families.insert(
        egui::FontFamily::Name(ICON_FAMILY.into()),
        vec![ICON_FAMILY.to_owned()],
    );
    ICON_FONT.store(true, Ordering::Relaxed);
}

/// An icon the dashboard shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Icon {
    Settings,
    Keyboard,
    Speaker,
    Microphone,
    Monitor,
    Record,
    Stop,
    Pause,
    Play,
    Folder,
    Close,
    Update,
}

impl Icon {
    /// The icon font's character (the same in both fonts).
    fn codepoint(self) -> char {
        match self {
            Self::Settings => '\u{E713}',
            Self::Keyboard => '\u{E765}',
            Self::Speaker => '\u{E767}',
            Self::Microphone => '\u{E720}',
            Self::Monitor => '\u{E7F4}',
            Self::Record => '\u{E7C8}',
            Self::Stop => '\u{E71A}',
            Self::Pause => '\u{E769}',
            Self::Play => '\u{E768}',
            Self::Folder => '\u{E838}',
            Self::Close => '\u{E711}',
            Self::Update => '\u{E896}',
        }
    }

    /// The emoji shown without the icon font, and in plain text like a
    /// notice.
    pub(super) fn emoji(self) -> &'static str {
        match self {
            Self::Settings => "⚙",
            Self::Keyboard => "⌨",
            Self::Speaker => "🔊",
            Self::Microphone => "🎙",
            Self::Monitor => "🖥",
            Self::Record => "⏺",
            Self::Stop => "⏹",
            Self::Pause => "⏸",
            Self::Play => "▶",
            Self::Folder => "📁",
            Self::Close => "✕",
            Self::Update => "⬆",
        }
    }

    /// The icon at `size`: the icon font's glyph, else the emoji.
    pub(super) fn rich(self, size: f32) -> egui::RichText {
        if ICON_FONT.load(Ordering::Relaxed) {
            egui::RichText::new(self.codepoint().to_string())
                .family(egui::FontFamily::Name(ICON_FAMILY.into()))
                .size(size)
        } else {
            egui::RichText::new(self.emoji()).size(size)
        }
    }

    /// The icon at `size`, for painting.
    pub(super) fn font_and_text(self, size: f32) -> (egui::FontId, String) {
        if ICON_FONT.load(Ordering::Relaxed) {
            (
                egui::FontId::new(size, egui::FontFamily::Name(ICON_FAMILY.into())),
                self.codepoint().to_string(),
            )
        } else {
            (egui::FontId::proportional(size), self.emoji().to_owned())
        }
    }
}

/// A button with `icon` before `text`. Add it with `add_named`, so screen
/// readers hear `text` rather than the icon font's private character.
pub(super) fn icon_button(
    icon: Icon,
    text: &str,
    color: egui::Color32,
    size: f32,
) -> egui::Button<'static> {
    // A touch larger than the text, as Windows pairs 16 px icons with 14 px
    // text.
    egui::Button::new((
        icon.rich(size + 2.0).color(color),
        egui::RichText::new(text.to_owned()).size(size).color(color),
    ))
}

/// Adds `button` (enabled or not), named `name` for screen readers and UI
/// Automation.
pub(super) fn add_named(
    ui: &mut egui::Ui,
    enabled: bool,
    button: egui::Button<'_>,
    name: &str,
) -> egui::Response {
    let response = ui.add_enabled(enabled, button);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, name));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_windows_11_icon_font_is_preferred_then_windows_10_s() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(icon_font_path(dir.path()), None);
        std::fs::write(dir.path().join("segmdl2.ttf"), b"").unwrap();
        assert_eq!(
            icon_font_path(dir.path()),
            Some(dir.path().join("segmdl2.ttf"))
        );
        std::fs::write(dir.path().join("SegoeIcons.ttf"), b"").unwrap();
        assert_eq!(
            icon_font_path(dir.path()),
            Some(dir.path().join("SegoeIcons.ttf"))
        );
    }

    #[test]
    fn each_icon_is_its_documented_glyph_with_an_emoji_to_fall_back_on() {
        // From "Segoe Fluent Icons font" on Microsoft Learn.
        for (icon, codepoint) in [
            (Icon::Settings, '\u{E713}'),
            (Icon::Keyboard, '\u{E765}'),
            (Icon::Speaker, '\u{E767}'),
            (Icon::Microphone, '\u{E720}'),
            (Icon::Monitor, '\u{E7F4}'),
            (Icon::Record, '\u{E7C8}'),
            (Icon::Stop, '\u{E71A}'),
            (Icon::Pause, '\u{E769}'),
            (Icon::Play, '\u{E768}'),
            (Icon::Folder, '\u{E838}'),
            (Icon::Close, '\u{E711}'),
            (Icon::Update, '\u{E896}'),
        ] {
            assert_eq!(icon.codepoint(), codepoint, "{icon:?}");
            assert!(!icon.emoji().is_empty(), "{icon:?}");
        }
    }
}
