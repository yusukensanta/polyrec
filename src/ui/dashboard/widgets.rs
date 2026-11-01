use super::icons::Icon;
use super::theme::*;
use crate::config::Config;
use crate::types::AudioDevice;
use eframe::egui;

pub(super) fn accent_button(text: &str, color: egui::Color32) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.to_string()).color(color))
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

pub(super) fn section_header(ui: &mut egui::Ui, title: &str) {
    // 14pt: up from 10pt (too small to read comfortably as a section label),
    // still clearly below the "PolyRec" heading's default 18pt so it reads as
    // a subordinate label, not competing with it.
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(title)
            .size(text_px(TEXT_SUBTITLE))
            .color(TEXT_MUTED)
            .strong(),
    );
    ui.add_space(SPACE_TIGHT);
    ui.separator();
    ui.add_space(SPACE_TIGHT);
}

/// Icon for an audio-device checkbox label — used in the source panel's
/// device list, and by `actions::start_recording_with_source` to build the
/// export dialog's per-track labels at record-start time.
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
/// ScrollArea reserves a fixed height regardless (see render_source_panel),
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
