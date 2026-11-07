use super::icons::Icon;
use super::theme::*;
use crate::config::Config;
use crate::types::AudioDevice;
use eframe::egui;

/// A button to press: `color` text (normally `Palette::link`), outlined like
/// every button (see `theme::visuals_for`).
pub(super) fn accent_button(text: &str, color: egui::Color32) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.to_string()).color(color))
}

/// The main action where it appears (Export, Update now): solid indigo.
pub(super) fn primary_button(text: &str) -> egui::Button<'static> {
    let p = pal();
    egui::Button::new(egui::RichText::new(text.to_string()).color(p.on_accent))
        .fill(p.accent)
        .stroke(egui::Stroke::new(1.0, p.accent))
}

/// Caption-size text in `color` -- the dashboard's small status/hint line
/// style (timer stats, free space, Highlight status, ...).
pub(super) fn caption(text: impl Into<String>, color: egui::Color32) -> egui::RichText {
    egui::RichText::new(text)
        .size(text_px(TEXT_CAPTION))
        .color(color)
}

/// Renders `add_contents` in a `row_height`-tall, `left_to_right` row
/// explicitly centered within `ui`'s full available width. Used for the
/// REC/Resume/Stop+Pause action row so all three states land at the same X
/// *and* Y.
///
/// Plain `ui.horizontal` + the panel's outer `Layout::bottom_up(Align::Center)`
/// does NOT center a multi-widget group despite appearances: `ui.horizontal`
/// claims the full available width for its own rect rather than shrinking to
/// its children's combined size, so the outer layout centering that
/// already-full-width rect is a no-op, and the children default to
/// left-aligned within it.
///
/// Must use `allocate_ui_with_layout` (which reserves an exact-sized slot
/// from the *current* layout, respecting `bottom_up` stacking) rather than
/// `ui.scope_builder` without an explicit `max_rect` -- that defaults to
/// `available_rect_before_wrap()`, which in a `bottom_up` layout is the
/// entire remaining (tall) region above the cursor, not just this row's
/// height, so `Align::Center`'s cross-axis (vertical, here) centering
/// re-centers the row within that whole region instead of stacking it
/// snugly against the previous item -- moved the row (and thus REC, not
/// just Stop+Pause) up from where it used to sit.
///
/// `content_width` is the caller's precomputed total width of what it's
/// about to add, so this doesn't need to run `add_contents` twice just to
/// measure it.
pub(super) fn centered_action_row(
    ui: &mut egui::Ui,
    content_width: f32,
    row_height: f32,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let available_width = ui.available_width();
    let left_pad = ((available_width - content_width) / 2.0).max(0.0);
    ui.allocate_ui_with_layout(
        egui::vec2(available_width, row_height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.add_space(left_pad);
            add_contents(ui);
        },
    );
}

/// A pop-up (Audio, Quality, Hotkeys, an error...), centred on the
/// dashboard, with its title in `on_accent` on the `bg_title` band. egui
/// fills only the topmost window's title bar (`theme::visuals_for`), so the
/// title frame carries the band too: one pop-up behind another keeps it.
pub(super) fn popup_window(ctx: &egui::Context, title: &str) -> egui::Window<'static> {
    let p = pal();
    egui::Window::new(egui::RichText::new(title).color(p.on_accent))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .title_frame(egui::Frame::window(&ctx.global_style()).fill(p.bg_title))
}

/// A row of choices in a pop-up, e.g. Native (window) / Match display /
/// Custom: wrapped onto another line where it doesn't fit (another
/// language, a larger text size) rather than stretching the pop-up past its
/// title bar.
pub(super) fn choice_row<R>(
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    ui.horizontal_wrapped(add_contents)
}

pub(super) fn section_header(ui: &mut egui::Ui, title: &str) {
    // `TEXT_SUBTITLE`, muted: up from 10pt (too small to read comfortably as
    // a section label), still clearly below the heading size so it reads as
    // a subordinate label, not competing with it.
    ui.add_space(SPACE_TIGHT);
    ui.label(section_label(title));
    ui.add_space(SPACE_TIGHT);
    ui.separator();
    ui.add_space(SPACE_TIGHT);
}

fn section_label(title: &str) -> egui::RichText {
    egui::RichText::new(title)
        .size(text_px(TEXT_SUBTITLE))
        .color(pal().text_muted)
        .strong()
}

/// `section_header` in a two-column `egui::Grid`, e.g. the Hotkeys popup's:
/// the title, and its line in the next column. No added space -- a Grid
/// can't place it (egui asserts in a debug build; a release build laid the
/// rows out wider than the popup).
pub(super) fn grid_section_header(ui: &mut egui::Ui, title: &str) {
    ui.label(section_label(title));
    ui.separator();
}

/// An icon's texture (`icon`: RGBA pixels, width, height), loaded into
/// `cache` under `key` the first time it's asked for, as `name()`. `None`
/// for no icon.
pub(super) fn cached_icon_texture<K: std::hash::Hash + Eq>(
    ctx: &egui::Context,
    cache: &mut std::collections::HashMap<K, egui::TextureHandle>,
    key: K,
    name: impl FnOnce() -> String,
    icon: Option<&(Vec<u8>, u32, u32)>,
) -> Option<egui::TextureId> {
    use std::collections::hash_map::Entry;
    match cache.entry(key) {
        Entry::Occupied(texture) => Some(texture.get().id()),
        Entry::Vacant(slot) => {
            let (rgba, w, h) = icon?;
            let image = egui::ColorImage::from_rgba_unmultiplied([*w as usize, *h as usize], rgba);
            let texture = ctx.load_texture(name(), image, egui::TextureOptions::LINEAR);
            Some(slot.insert(texture).id())
        }
    }
}

/// Whether a search box's text (`filter`, already lowercased) matches any of
/// `fields`, case-insensitively: everything matches an empty one.
pub(super) fn matches_search(filter: &str, fields: &[&str]) -> bool {
    filter.is_empty() || fields.iter().any(|f| f.to_lowercase().contains(filter))
}

/// Icon for an audio-device checkbox label, in the Audio pop-up's device
/// list.
pub(super) fn audio_device_icon(dev: &AudioDevice) -> Icon {
    if dev.is_loopback {
        Icon::Speaker
    } else {
        Icon::Microphone
    }
}

/// Size of the icon column at the start of a source, device or track row.
const ICON_SLOT: egui::Vec2 = egui::vec2(16.0, 16.0);

/// What goes in a row's icon column (`icon_slot`).
#[derive(Clone, Copy)]
pub(super) enum RowIcon {
    /// Nothing; the column stays, so the name still lines up.
    None,
    /// A picture, e.g. an app's own icon.
    Image(egui::TextureId),
    /// One of the dashboard's icons, e.g. a speaker.
    Icon(Icon),
}

/// The icon column at the start of a row: the same width whatever `icon` is,
/// or with none, so the names after it line up. A glyph written in front of
/// the name instead started each name wherever that glyph ended -- 🔊 and 🎙
/// aren't the same width.
pub(super) fn icon_slot(ui: &mut egui::Ui, icon: RowIcon) {
    let (rect, _) = ui.allocate_exact_size(ICON_SLOT, egui::Sense::hover());
    match icon {
        RowIcon::None => {}
        RowIcon::Image(texture) => {
            egui::Image::new((texture, ICON_SLOT)).paint_at(ui, rect);
        }
        RowIcon::Icon(icon) => {
            let (font, text) = icon.font_and_text(ICON_SLOT.y);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                text,
                font,
                ui.visuals().text_color(),
            );
        }
    }
}

/// Renders an icon + checkbox row, then -- if checked -- a 0-100% volume
/// slider under the checkbox, reading/writing
/// `Config::audio_device_gain` via `gain_key`. Shared by the AUDIO/SYSTEM
/// device list (🔊/🎙 glyph) and the APPLICATIONS list (app icon) -- same
/// interaction and persistence pattern, different backing data (a WASAPI
/// endpoint's stable id vs. `Config::app_audio_gain_key`'s `"app:"`-prefixed
/// exe name).
///
/// This function owns both rows itself (icon+checkbox, then slider)
/// rather than a caller wrapping it in `ui.horizontal` -- that would make a
/// checked row wider instead of taller, which doesn't fit well next to a
/// device/app name in a 200-380px panel. Toggling a checkbox does change
/// this section's content height (one row vs. two), but the section's own
/// ScrollArea reserves a fixed height regardless (see `render_audio_popup`),
/// so that never resizes the surrounding panel -- it just means less/more
/// of the section's own scroll room is used.
#[allow(clippy::too_many_arguments)] // icon + the config-save trio + the per-row identity trio are each independent, not a natural single struct
pub(super) fn checkbox_with_volume_slider(
    ui: &mut egui::Ui,
    icon: RowIcon,
    config: &mut Config,
    error_message: &mut Option<String>,
    config_save_failed_prefix: &str,
    checked: &mut bool,
    label: String,
    gain_key: &str,
    checkbox_tooltip: Option<&str>,
) {
    ui.horizontal(|ui| {
        icon_slot(ui, icon);
        let response = ui.checkbox(checked, label);
        if let Some(tooltip) = checkbox_tooltip {
            response.on_hover_text(tooltip);
        }
    });
    // Only meaningful (and shown) once checked -- an unselected source's
    // volume has nothing to apply to.
    if *checked {
        let mut gain_percent = (config.audio_gain(gain_key) * 100.0).round() as i32;
        ui.horizontal(|ui| {
            // Under the checkbox, past the icon column.
            ui.add_space(ICON_SLOT.x + ui.spacing().item_spacing.x);
            let response = ui.add(
                egui::Slider::new(
                    &mut gain_percent,
                    crate::config::AUDIO_GAIN_MIN_PERCENT..=crate::config::AUDIO_GAIN_MAX_PERCENT,
                )
                .suffix("%"),
            );
            // Saves on every changed() frame rather than gating on
            // drag_stopped() -- found via a live restart-and-check that
            // drag_stopped() only fires for an actual pointer-drag release,
            // so a keyboard-driven change (arrow keys after focusing the
            // slider) or assistive tech (UI Automation's RangeValuePattern)
            // updated the live value but silently never persisted.
            // changed() only fires on an actual value change (bounded by
            // the slider's discrete steps, not once per frame), so this
            // isn't the write-heavy cost gating on drag_stopped() was meant
            // to avoid.
            if response.changed() {
                config
                    .audio_device_gain
                    .insert(gain_key.to_string(), gain_percent as f32 / 100.0);
                super::util::save_config_or_report(
                    config,
                    error_message,
                    config_save_failed_prefix,
                );
            }
        });
    }
}

/// Formats a byte count as a human-readable GB/MB string (free space, a
/// recording's size so far)
/// -- GB with one decimal once it's large enough for that decimal to
/// be meaningful, otherwise a whole-number MB (matches how Windows' own disk
/// space displays don't bother with GB fractions for small values).
pub(super) fn format_bytes(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = MB * 1024.0;
    let bytes = bytes as f64;
    if bytes >= GB {
        format!("{:.1} GB", bytes / GB)
    } else {
        format!("{:.0} MB", bytes / MB)
    }
}

#[cfg(test)]
mod free_space_display_tests {
    use super::*;

    #[test]
    fn format_bytes_uses_mb_below_one_gb() {
        assert_eq!(format_bytes(500 * 1024 * 1024), "500 MB");
    }

    #[test]
    fn format_bytes_uses_gb_with_one_decimal_at_or_above_one_gb() {
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GB");
        // 2.5 GB exactly: 2.5 * 1024^3 = 2684354560, an exact integer.
        assert_eq!(format_bytes(2_684_354_560), "2.5 GB");
    }
}

#[cfg(test)]
mod icon_column_tests {
    use super::*;

    #[test]
    fn names_line_up_whatever_icon_a_row_has() {
        // 🔊 and 🎙 are different widths: written in front of the name, they
        // started Speakers and Microphone at different places.
        let ctx = egui::Context::default();
        let mut starts = Vec::new();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            for icon in [
                RowIcon::Icon(Icon::Speaker),
                RowIcon::Icon(Icon::Microphone),
                RowIcon::Image(egui::TextureId::User(1)),
                RowIcon::None,
            ] {
                ui.horizontal(|ui| {
                    icon_slot(ui, icon);
                    starts.push(ui.label("name").rect.min.x);
                });
            }
        });
        output.textures_delta.clear();
        assert!(starts.windows(2).all(|w| w[0] == w[1]), "{starts:?}");
    }
}

#[cfg(test)]
mod popup_tests {
    use super::*;
    use crate::i18n::Lang;

    fn flatten(shapes: Vec<egui::Shape>) -> Vec<egui::Shape> {
        shapes
            .into_iter()
            .flat_map(|shape| match shape {
                egui::Shape::Vec(inner) => flatten(inner),
                shape => vec![shape],
            })
            .collect()
    }

    /// Every shape `show` paints in palette `p`, with the product fonts at
    /// Windows' text size `text_scale`, once its pop-ups have sized
    /// themselves (unseen, on the first pass).
    fn painted(
        p: Palette,
        text_scale: f32,
        mut show: impl FnMut(&egui::Context),
    ) -> Vec<egui::Shape> {
        use_palette(p);
        let _turn = text_scale_turn();
        let ctx = egui::Context::default();
        setup_theme(&ctx);
        apply_text_scale(&ctx, text_scale);
        setup_fonts(&ctx);
        // No fade-in: its colours are part-way there.
        let mut style = (*ctx.global_style()).clone();
        style.animation_time = 0.0;
        ctx.set_global_style(style);
        let mut shapes = Vec::new();
        for _ in 0..3 {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| show(ui.ctx()));
            output.textures_delta.clear();
            shapes = output.shapes.into_iter().map(|c| c.shape).collect();
        }
        flatten(shapes)
    }

    /// The colour `text` is painted in.
    fn text_colour(shapes: &[egui::Shape], text: &str) -> Option<egui::Color32> {
        shapes.iter().find_map(|shape| match shape {
            egui::Shape::Text(t) if t.galley.text() == text => {
                let own = t.galley.job.sections.first()?.format.color;
                let colour = if own == egui::Color32::PLACEHOLDER {
                    t.fallback_color
                } else {
                    own
                };
                Some(t.override_text_color.unwrap_or(colour))
            }
            _ => None,
        })
    }

    fn title_bands(shapes: &[egui::Shape], p: &Palette) -> usize {
        shapes
            .iter()
            .filter(|shape| matches!(shape, egui::Shape::Rect(r) if r.fill == p.bg_title))
            .count()
    }

    /// A pop-up's title was egui's text colour on the dark indigo of a
    /// pressed button, and read as a highlight rather than a title. In the
    /// light theme its band was 1.15:1 from the white pop-up below.
    #[test]
    fn a_popup_title_is_on_accent_on_its_own_band() {
        for (p, name) in [(Palette::DARK, "dark"), (Palette::LIGHT, "light")] {
            let shapes = painted(p, 1.0, |ctx| {
                popup_window(ctx, "Title").show(ctx, |ui| ui.label("body"));
            });
            assert_eq!(text_colour(&shapes, "Title"), Some(p.on_accent), "{name}");
            assert_eq!(title_bands(&shapes, &p), 1, "{name}");
        }
    }

    /// One pop-up over another, e.g. an error over Quality: the one behind
    /// keeps its band, or its title would be white on white in the light
    /// theme (egui fills only the topmost window's title bar itself).
    #[test]
    fn a_popup_behind_another_keeps_its_title_band() {
        for (p, name) in [(Palette::DARK, "dark"), (Palette::LIGHT, "light")] {
            let shapes = painted(p, 1.0, |ctx| {
                popup_window(ctx, "Behind").show(ctx, |ui| ui.label("body"));
                popup_window(ctx, "In front").show(ctx, |ui| ui.label("body"));
            });
            assert_eq!(title_bands(&shapes, &p), 2, "{name}");
        }
    }

    /// The Quality pop-up's resolution choices, in `lang` at Windows' text
    /// size `text_scale`: the pop-up's width, and where each choice's top
    /// is.
    fn resolution_choices(lang: Lang, text_scale: f32) -> (f32, Vec<f32>) {
        let s = lang.strings();
        let mut laid_out = (0.0, Vec::new());
        painted(Palette::DARK, text_scale, |ctx| {
            let mut tops = Vec::new();
            let shown = popup_window(ctx, s.quality_title)
                .min_width(POPUP_WIDTH)
                .max_width(POPUP_WIDTH)
                .show(ctx, |ui| {
                    let mut chosen = 0;
                    choice_row(ui, |ui| {
                        for (i, choice) in [
                            s.resolution_native,
                            s.resolution_display,
                            s.resolution_custom,
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            tops.push(ui.selectable_value(&mut chosen, i, choice).rect.top());
                        }
                    });
                });
            laid_out = (shown.unwrap().response.rect.width(), tops);
        });
        laid_out
    }

    /// The Quality pop-up's Native (window) / Match display / Custom was
    /// wider than the pop-up: it stretched the pop-up past its title bar,
    /// which stopped short of the right edge. Where it doesn't fit, it
    /// wraps.
    #[test]
    fn a_row_of_choices_wraps_inside_its_popup() {
        for (lang, text_scale) in [(Lang::En, 1.5), (Lang::Ja, 1.0), (Lang::Ja, 1.5)] {
            let (width, ..) = resolution_choices(lang, text_scale);
            assert!(
                width <= POPUP_WIDTH + 2.0,
                "{lang:?} at {text_scale}: {width}"
            );
        }
    }

    /// The pop-ups are wide enough for those choices on one line, in
    /// English at the default text size.
    #[test]
    fn the_resolution_choices_fit_on_one_line() {
        let (_, tops) = resolution_choices(Lang::En, 1.0);
        assert!(tops.iter().all(|&top| top == tops[0]), "{tops:?}");
    }

    /// The Hotkeys popup's headers went through `section_header`, whose
    /// spacing a Grid can't take: egui asserts in a debug build, and a
    /// release build laid the rows out wider than the popup, past its title
    /// bar.
    #[test]
    fn a_section_header_in_a_grid_stays_inside_its_popup() {
        let s = Lang::En.strings();
        let mut width = 0.0;
        painted(Palette::DARK, 1.0, |ctx| {
            let shown = popup_window(ctx, s.hotkeys_title)
                .min_width(POPUP_WIDTH)
                .max_width(POPUP_WIDTH)
                .show(ctx, |ui| {
                    egui::Grid::new("hotkeys").num_columns(2).show(ui, |ui| {
                        grid_section_header(ui, s.hotkey_start_stop_header);
                        ui.end_row();
                        ui.label("ALT+SHIFT+F9");
                        let _ = ui.button(s.hotkey_change_button);
                        ui.end_row();
                    });
                });
            width = shown.unwrap().response.rect.width();
        });
        assert!(width <= POPUP_WIDTH + 2.0, "{width}");
    }
}
