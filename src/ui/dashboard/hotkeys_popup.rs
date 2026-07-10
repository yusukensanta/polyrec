use super::{accent_button, section_header, App, ACCENT_PAUSE, ACCENT_SECONDARY, POPUP_WIDTH, TEXT_CAPTION, TEXT_MUTED, TEXT_PRIMARY};
use crate::hotkeys::HotkeyListener;
use crate::i18n::Strings;
use eframe::egui;

/// Which hotkey binding is currently waiting for a keypress, when the user
/// has clicked "Change" in the Hotkeys popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HotkeySlot {
    StartStop,
    Pause,
    ToggleOverlay,
    SaveHighlight,
}

impl App {
    pub(super) fn render_hotkeys_popup(&mut self, ctx: &egui::Context, s: &'static Strings) {
        if !self.show_hotkeys_popup {
            return;
        }

        // While waiting for a keypress, consume this frame's key events
        // directly rather than via any widget -- capture works no matter
        // where focus happens to be in the popup. Escape cancels; anything
        // else not registerable as a global hotkey (a bare modifier, or a key
        // the egui/hotkeys mapping doesn't cover) is ignored so the prompt
        // keeps waiting instead of silently eating the keypress.
        if let Some(slot) = self.recording_hotkey {
            ctx.input(|i| {
                for event in &i.events {
                    if let egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. } = event {
                        if *key == egui::Key::Escape {
                            self.recording_hotkey = None;
                            return;
                        }
                        let Some(base_name) = egui_key_to_hotkey_string(*key) else {
                            continue;
                        };
                        // Canonical modifier order (CTRL, ALT, SHIFT) regardless of
                        // which order the user physically held them down in --
                        // parse_hotkey accepts any order, but a fixed display order
                        // keeps the saved string (and the on-screen binding) consistent.
                        let mut combo = String::new();
                        if modifiers.ctrl {
                            combo.push_str("CTRL+");
                        }
                        if modifiers.alt {
                            combo.push_str("ALT+");
                        }
                        if modifiers.shift {
                            combo.push_str("SHIFT+");
                        }
                        combo.push_str(base_name);

                        let Some((vk, mods)) = crate::hotkeys::parse_hotkey(&combo) else {
                            continue;
                        };
                        if crate::hotkeys::try_register(vk, mods) {
                            match slot {
                                HotkeySlot::StartStop => self.config.hotkeys.start_stop = combo.clone(),
                                HotkeySlot::Pause => self.config.hotkeys.pause = combo.clone(),
                                HotkeySlot::ToggleOverlay => self.config.hotkeys.toggle_overlay = combo.clone(),
                                HotkeySlot::SaveHighlight => self.config.hotkeys.save_highlight = combo.clone(),
                            }
                            self.hotkey_capture_warning = None;
                        } else {
                            self.hotkey_capture_warning = Some(format!(
                                "{}{combo}{}",
                                s.hotkey_unavailable_prefix, s.hotkey_unavailable_suffix,
                            ));
                        }
                        self.recording_hotkey = None;
                        return;
                    }
                }
            });
        }

        let mut close = false;
        egui::Window::new(s.hotkeys_title)
            .collapsible(false)
            .resizable(false)
            // Explicit width instead of relying on auto-sizing -- the Grid
            // below computes its column width from the widest cell, and
            // section_header's separator (which fills "available width")
            // would otherwise stretch the Grid -- and the whole Window -- out
            // to the full app window's width, since an unconstrained floating
            // Window's content area has no real upper bound of its own.
            // min/max pinned to the same value (shared with the Quality
            // popup) so egui's Resize area can't widen permanently the first
            // time the "press any key" prompt -- wider than any bound-key
            // label -- gets measured; egui only ever grows a Window's
            // remembered size to fit its widest frame, never shrinks it back.
            .default_width(POPUP_WIDTH)
            .min_width(POPUP_WIDTH)
            .max_width(POPUP_WIDTH)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                // Caps the popup's height below a typical app window's, so a
                // short window (or a future 5th/6th hotkey row) scrolls
                // instead of forcing the popup taller than its parent.
                egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                // Grid (not four independent ui.horizontal calls) so the
                // current-binding label and Change button land in the same
                // two columns across all four rows structurally -- previously
                // each row's Change button drifted left/right depending on
                // how wide that row's own binding text happened to render.
                egui::Grid::new("hotkeys_grid")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        self.render_hotkey_row(ui, s, s.hotkey_start_stop_header, HotkeySlot::StartStop);
                        self.render_hotkey_row(ui, s, s.hotkey_pause_header, HotkeySlot::Pause);
                        self.render_hotkey_row(ui, s, s.hotkey_overlay_header, HotkeySlot::ToggleOverlay);
                        self.render_hotkey_row(ui, s, s.hotkey_save_highlight_header, HotkeySlot::SaveHighlight);
                    });

                let bindings = [
                    &self.config.hotkeys.start_stop,
                    &self.config.hotkeys.pause,
                    &self.config.hotkeys.toggle_overlay,
                    &self.config.hotkeys.save_highlight,
                ];
                let has_collision = bindings
                    .iter()
                    .enumerate()
                    .any(|(i, a)| bindings.iter().skip(i + 1).any(|b| a == b));
                if has_collision {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(s.hotkey_collision_warning)
                            .size(TEXT_CAPTION)
                            .color(ACCENT_PAUSE),
                    );
                }

                if let Some(warning) = &self.hotkey_capture_warning {
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(warning).size(TEXT_CAPTION).color(ACCENT_PAUSE));
                }
                }); // end settings ScrollArea -- Close button stays outside so it's never scrolled out of view

                ui.add_space(8.0);
                if ui.add(accent_button(s.close_button, TEXT_MUTED)).clicked() {
                    close = true;
                }
            });
        if close {
            self.show_hotkeys_popup = false;
            self.recording_hotkey = None;
            self.hotkey_capture_warning = None;
            if let Err(e) = self.config.save() {
                tracing::error!("failed to save config: {e}");
                self.error_message = Some(format!("{}{e}", s.config_save_failed_prefix));
            }
            // The listener registers its hotkeys once at spawn time, so a rebind
            // needs a fresh thread — stop() unregisters the old bindings before
            // the new listener registers the (possibly changed) ones, avoiding a
            // stuck registration on a key the user just reassigned elsewhere.
            if let Some(old) = self.hotkey_listener.take() {
                old.stop();
            }
            let wake_ctx = ctx.clone();
            self.hotkey_listener = Some(HotkeyListener::spawn(
                &self.config.hotkeys.start_stop,
                &self.config.hotkeys.pause,
                &self.config.hotkeys.toggle_overlay,
                &self.config.hotkeys.save_highlight,
                move || wake_ctx.request_repaint(),
            ));
        }
    }

    /// One hotkey's two grid rows: a header spanning the row, then the
    /// currently bound key (or a "press any key" prompt while
    /// `self.recording_hotkey == Some(slot)`) in column 0 and a Change
    /// button in column 1 -- called from inside an `egui::Grid::show`
    /// closure, so every row's button lands in the same column position.
    fn render_hotkey_row(&mut self, ui: &mut egui::Ui, s: &'static Strings, header: &str, slot: HotkeySlot) {
        section_header(ui, header);
        ui.end_row();

        let current = match slot {
            HotkeySlot::StartStop => &self.config.hotkeys.start_stop,
            HotkeySlot::Pause => &self.config.hotkeys.pause,
            HotkeySlot::ToggleOverlay => &self.config.hotkeys.toggle_overlay,
            HotkeySlot::SaveHighlight => &self.config.hotkeys.save_highlight,
        }
        .clone();
        if self.recording_hotkey == Some(slot) {
            // Stacked, not side-by-side -- the prompt string alone is close to
            // the popup's full pinned width, so keeping "(Esc to cancel)" on
            // the same line would overflow it rather than wrap.
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new(s.hotkey_press_any_key_prompt)
                        .color(ACCENT_SECONDARY)
                        .strong(),
                );
                ui.label(egui::RichText::new(s.hotkey_press_esc_to_cancel).size(TEXT_CAPTION).color(TEXT_MUTED));
            });
        } else {
            ui.label(egui::RichText::new(&current).strong().color(TEXT_PRIMARY));
            if ui.add(accent_button(s.hotkey_change_button, ACCENT_SECONDARY)).on_hover_text(s.hotkey_change_tooltip).clicked() {
                self.recording_hotkey = Some(slot);
                self.hotkey_capture_warning = None;
            }
        }
        ui.end_row();
    }
}

/// Maps an egui key-press event (from the Hotkeys popup's "press any key to
/// bind" capture) to the base-key string form `hotkeys::parse_hotkey` expects
/// (modifiers are prefixed separately by the capture logic that calls this).
/// Returns
/// `None` for keys that can't be a standalone global hotkey binding: `Escape`
/// (reserved as this capture flow's own cancel gesture) and bare modifier
/// keys (Ctrl/Shift/Alt/Super alone aren't meaningful `RegisterHotKey`
/// targets without a companion key -- this app only supports single-key,
/// no-modifier bindings, matching `hotkeys::run_hotkey_loop`'s `MOD_NOREPEAT`
/// -only registration).
fn egui_key_to_hotkey_string(key: egui::Key) -> Option<&'static str> {
    use egui::Key;
    Some(match key {
        Key::F1 => "F1", Key::F2 => "F2", Key::F3 => "F3", Key::F4 => "F4",
        Key::F5 => "F5", Key::F6 => "F6", Key::F7 => "F7", Key::F8 => "F8",
        Key::F9 => "F9", Key::F10 => "F10", Key::F11 => "F11", Key::F12 => "F12",
        Key::F13 => "F13", Key::F14 => "F14", Key::F15 => "F15", Key::F16 => "F16",
        Key::F17 => "F17", Key::F18 => "F18", Key::F19 => "F19", Key::F20 => "F20",
        Key::F21 => "F21", Key::F22 => "F22", Key::F23 => "F23", Key::F24 => "F24",
        Key::A => "A", Key::B => "B", Key::C => "C", Key::D => "D", Key::E => "E",
        Key::F => "F", Key::G => "G", Key::H => "H", Key::I => "I", Key::J => "J",
        Key::K => "K", Key::L => "L", Key::M => "M", Key::N => "N", Key::O => "O",
        Key::P => "P", Key::Q => "Q", Key::R => "R", Key::S => "S", Key::T => "T",
        Key::U => "U", Key::V => "V", Key::W => "W", Key::X => "X", Key::Y => "Y",
        Key::Z => "Z",
        Key::Num0 => "0", Key::Num1 => "1", Key::Num2 => "2", Key::Num3 => "3",
        Key::Num4 => "4", Key::Num5 => "5", Key::Num6 => "6", Key::Num7 => "7",
        Key::Num8 => "8", Key::Num9 => "9",
        Key::Space => "SPACE",
        Key::Tab => "TAB",
        Key::Enter => "ENTER",
        Key::Backspace => "BACKSPACE",
        Key::Delete => "DELETE",
        Key::Insert => "INSERT",
        Key::Home => "HOME",
        Key::End => "END",
        Key::PageUp => "PAGEUP",
        Key::PageDown => "PAGEDOWN",
        Key::ArrowUp => "UP",
        Key::ArrowDown => "DOWN",
        Key::ArrowLeft => "LEFT",
        Key::ArrowRight => "RIGHT",
        _ => return None,
    })
}

#[cfg(test)]
mod hotkey_capture_tests {
    use super::*;

    #[test]
    fn egui_key_to_hotkey_string_covers_common_keys() {
        assert_eq!(egui_key_to_hotkey_string(egui::Key::F9), Some("F9"));
        assert_eq!(egui_key_to_hotkey_string(egui::Key::A), Some("A"));
        assert_eq!(egui_key_to_hotkey_string(egui::Key::Num0), Some("0"));
        assert_eq!(egui_key_to_hotkey_string(egui::Key::Space), Some("SPACE"));
        assert_eq!(egui_key_to_hotkey_string(egui::Key::ArrowUp), Some("UP"));
    }

    #[test]
    fn egui_key_to_hotkey_string_excludes_escape() {
        // Escape is reserved as the capture flow's own cancel gesture.
        assert_eq!(egui_key_to_hotkey_string(egui::Key::Escape), None);
    }

    #[test]
    fn every_mapped_hotkey_string_round_trips_through_parse_hotkey() {
        // Every string this mapping can produce must actually be recognized by
        // hotkeys::parse_hotkey, or a captured key would silently fail to bind.
        let keys = [
            egui::Key::F1, egui::Key::F13, egui::Key::F24, egui::Key::A, egui::Key::Z,
            egui::Key::Num0, egui::Key::Num9, egui::Key::Space, egui::Key::Tab,
            egui::Key::Enter, egui::Key::Backspace, egui::Key::Delete, egui::Key::Insert,
            egui::Key::Home, egui::Key::End, egui::Key::PageUp, egui::Key::PageDown,
            egui::Key::ArrowUp, egui::Key::ArrowDown, egui::Key::ArrowLeft, egui::Key::ArrowRight,
        ];
        for key in keys {
            let name = egui_key_to_hotkey_string(key).unwrap_or_else(|| panic!("{key:?} should map to a hotkey string"));
            assert!(
                crate::hotkeys::parse_hotkey(name).is_some(),
                "hotkeys::parse_hotkey doesn't recognize \"{name}\" (produced from {key:?})"
            );
            // And with modifiers prefixed, matching what the real capture flow builds.
            let with_mods = format!("CTRL+ALT+SHIFT+{name}");
            assert!(
                crate::hotkeys::parse_hotkey(&with_mods).is_some(),
                "hotkeys::parse_hotkey doesn't recognize \"{with_mods}\""
            );
        }
    }
}
