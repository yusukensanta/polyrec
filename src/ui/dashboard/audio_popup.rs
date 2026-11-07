use super::App;
use super::icons::Icon;
use super::state::Popup;
use super::theme::{POPUP_WIDTH, SPACE_NORMAL, SPACE_TIGHT, pal};
use super::widgets::{
    RowIcon, accent_button, audio_device_icon, cached_icon_texture, caption,
    checkbox_with_volume_slider, matches_search, popup_window, section_header,
};
use crate::config::{Config, RegisteredApp};
use crate::i18n::Strings;
use crate::sources::InstalledApp;
use crate::types::{CaptureKind, CaptureSource};
use eframe::egui;
use rfd::FileDialog;
use std::sync::mpsc;

/// Where the add-app picker found a candidate -- determines how its exe
/// path gets resolved at pick time (see `render_add_app_picker`).
#[derive(Debug, Clone, PartialEq)]
enum AddAppSource {
    /// A live window's process id -- path resolved via `get_exe_path` when
    /// picked, not eagerly, since it's only needed once.
    OpenWindow(u32),
    /// An installed-but-not-running app's already-resolved exe path (from
    /// its Start Menu shortcut).
    Installed(String),
}

/// One row in the add-app picker's search results.
/// One row of the add-app picker, borrowing from the window list or the
/// installed apps it's found in: the picker rebuilds its rows every frame
/// it's open, and used to copy every candidate's icon pixels each time.
#[derive(Debug, PartialEq)]
struct AddAppCandidate<'a> {
    display_name: String,
    exe_name: &'a str,
    source: AddAppSource,
    icon_rgba: Option<&'a (Vec<u8>, u32, u32)>,
}

impl AddAppCandidate<'_> {
    /// Whether this candidate is currently running -- shown as a "● Running"
    /// tag (see `render_add_app_picker`) so the user can tell at a glance
    /// which candidates will start capturing immediately versus wait for
    /// launch, without needing to already know that from the exe name alone.
    fn is_running(&self) -> bool {
        matches!(self.source, AddAppSource::OpenWindow(_))
    }
}

impl App {
    pub(super) fn render_audio_popup(&mut self, ctx: &egui::Context, s: &'static Strings) {
        if self.popup != Popup::Audio {
            return;
        }
        let mut close = false;
        popup_window(ctx, s.audio_title)
            // Same fixed-width convention as Quality/Hotkeys -- see
            // POPUP_WIDTH's doc comment for why this is pinned rather than
            // left to auto-size.
            .default_width(POPUP_WIDTH)
            .min_width(POPUP_WIDTH)
            .max_width(POPUP_WIDTH)
            .show(ctx, |ui| {
                // Caps the popup's height below a typical app window's, so a
                // machine with many audio devices/apps scrolls inside the
                // popup instead of forcing it taller than its parent.
                egui::ScrollArea::vertical()
                    .max_height(400.0)
                    .show(ui, |ui| {
                        self.render_system_audio_rows(ui, s);

                        ui.add_space(SPACE_NORMAL);
                        self.render_app_audio_rows(ui, s);

                        ui.add_space(SPACE_NORMAL);
                        self.render_app_audio_only_toggle(ui, s);
                    }); // end settings ScrollArea -- Close button stays outside so it's never scrolled out of view

                ui.add_space(SPACE_NORMAL);
                if ui.add(accent_button(s.close_button, pal().link)).clicked() {
                    close = true;
                }
            });
        if close {
            self.close_popup(s);
        }
    }

    /// SYSTEM section: one checkbox + volume slider per audio device.
    fn render_system_audio_rows(&mut self, ui: &mut egui::Ui, s: &'static Strings) {
        section_header(ui, s.system_audio_header);
        if self.audio_devices.is_empty() {
            ui.label(caption(s.no_audio_devices, pal().text_muted));
        }
        // Iterated from cloned (label, id) pairs, not
        // `self.audio_devices` directly -- persisting a
        // toggle below needs `&mut self`, which a live
        // borrow of `self.audio_devices` here would conflict
        // with (same reasoning as the Applications loop's
        // `app_audio_sources.clone()` below).
        let device_rows: Vec<(Icon, String, String)> = self
            .audio_devices
            .iter()
            .map(|dev| (audio_device_icon(dev), dev.name.clone(), dev.id.clone()))
            .collect();
        for (i, (icon, label, dev_id)) in device_rows.into_iter().enumerate() {
            let was_checked = self.selected_audio[i];
            checkbox_with_volume_slider(
                ui,
                RowIcon::Icon(icon),
                &mut self.config,
                &mut self.error_message,
                s.config_save_failed_prefix,
                &mut self.selected_audio[i],
                label,
                &dev_id,
                None,
            );
            // Persisted immediately on toggle so it survives
            // an app restart -- see
            // `actions::persist_selected_audio_devices`.
            if was_checked != self.selected_audio[i] {
                self.persist_selected_audio_devices(s);
            }
        }
    }

    /// APPLICATIONS section: one row per registered app, then the
    /// "+ Add app" button or, once clicked, the picker.
    fn render_app_audio_rows(&mut self, ui: &mut egui::Ui, s: &'static Strings) {
        section_header(ui, s.applications_header);
        if self.app_audio_sources.is_empty() {
            ui.label(caption(s.no_app_audio_sources, pal().text_muted));
        }
        // An unchecked app is unregistered after the rows are drawn: doing it
        // in the loop rebuilds the list being drawn.
        let mut unchecked = None;
        for (i, src) in self.app_audio_sources.iter().enumerate() {
            let icon = cached_icon_texture(
                ui.ctx(),
                &mut self.app_audio_icon_textures,
                i,
                || format!("app_audio_icon_{i}"),
                src.icon_rgba.as_ref(),
            );
            let mut checked = true;
            checkbox_with_volume_slider(
                ui,
                icon.map_or(RowIcon::None, RowIcon::Image),
                &mut self.config,
                &mut self.error_message,
                s.config_save_failed_prefix,
                &mut checked,
                src.display_name.clone(),
                &Config::app_audio_gain_key(&src.exe_name),
                Some(s.remove_registered_app_tooltip),
            );
            // Every entry here is already registered (see
            // `actions::build_app_audio_sources`) -- the
            // checkbox is the only pin/unpin control, no
            // separate remove button. Unchecking
            // immediately un-registers and drops the row,
            // whether or not the app is currently running
            // (there's no "checked but idle" state to fall
            // back to -- being in this list at all already
            // means "record this").
            if !checked {
                unchecked = Some(src.exe_name.clone());
                break;
            }
        }
        if let Some(exe_name) = unchecked {
            self.config.unregister_app_audio(&exe_name);
            self.save_config_or_report(s);
            self.rebuild_app_audio_sources_now();
        }
        // The Applications list is entirely opt-in via this
        // button -- unlike SYSTEM devices, it never shows an
        // app just because it's currently making sound (see
        // `actions::build_app_audio_sources`'s doc comment).
        // A fresh install shows nothing here but this button.
        if !self.add_app.open {
            if ui
                .add(accent_button(s.add_app_button, pal().link))
                .clicked()
            {
                self.add_app.open = true;
                self.add_app.search.clear();
                // Cleared, not left stale, so a previous
                // open's results don't briefly show before
                // this scan's real results arrive.
                self.add_app.installed = Vec::new();
                let (tx, rx) = mpsc::channel();
                self.add_app.installed_rx = Some(rx);
                tokio::task::spawn_blocking(move || {
                    let apps = crate::sources::enumerate_installed_apps();
                    let _ = tx.send(apps);
                });
            }
        } else {
            self.render_add_app_picker(ui, s);
        }
    }

    /// The "App audio only" checkbox -- only meaningful with a loopback
    /// device selected and a window (not a monitor) as the source.
    fn render_app_audio_only_toggle(&mut self, ui: &mut egui::Ui, s: &'static Strings) {
        let loopback_selected = self
            .audio_devices
            .iter()
            .zip(self.selected_audio.iter())
            .any(|(dev, &sel)| dev.is_loopback && sel);
        let has_loopback_device = self.audio_devices.iter().any(|d| d.is_loopback);
        // A `Monitor` source has no owning process to scope
        // loopback to (see `session::start_capture`'s
        // `use_process_loopback` gate), so this only ever
        // does anything for a `Window` source.
        let selected_is_window = self
            .selected_source
            .and_then(|i| self.sources.get(i))
            .is_some_and(|src| src.kind == crate::types::CaptureKind::Window);
        ui.add_enabled_ui(loopback_selected && selected_is_window, |ui| {
            let response = ui
                .checkbox(
                    &mut self.config.default_app_audio_only,
                    egui::RichText::new(s.app_audio_only_label).color(pal().link),
                )
                .on_hover_text(if !has_loopback_device {
                    s.tooltip_no_loopback_device
                } else if !loopback_selected {
                    s.tooltip_check_loopback_first
                } else if !selected_is_window {
                    s.tooltip_app_audio_only_needs_window
                } else {
                    s.tooltip_app_audio_only
                });
            // Saved at once: it's what the next recording uses, a
            // hotkey-started one too, and the default at next launch.
            if response.changed() {
                self.save_config_or_report(s);
            }
        });
    }

    /// Inline search-and-pick panel shown in place of the "+ Add app"
    /// button once clicked. Searches two merged sources so an app doesn't
    /// need to be open, let alone actively making sound, to be found:
    /// currently open windows (`self.sources`, already available, so these
    /// show immediately) and installed-but-not-running apps
    /// (`self.add_app.installed`, populated asynchronously -- see
    /// `add_app.installed_rx`'s doc comment -- so these appear a moment
    /// later on a machine with many Start Menu shortcuts). An exe already
    /// open wins the dedup over the same exe found only via a Start Menu
    /// shortcut. Registers on click -- an open window's path is derived
    /// from its live process, an installed app's is already known from its
    /// resolved shortcut. "Browse for .exe instead…" remains for the rare
    /// case neither source finds (unregistered, no shortcut).
    fn render_add_app_picker(&mut self, ui: &mut egui::Ui, s: &'static Strings) {
        ui.add(
            egui::TextEdit::singleline(&mut self.add_app.search)
                .hint_text(s.add_app_search_placeholder)
                .desired_width(f32::INFINITY),
        );
        ui.add_space(SPACE_TIGHT);

        let filter = self.add_app.search.to_lowercase();
        let candidates = add_app_candidates(
            &self.sources,
            &self.add_app.installed,
            &self.config.registered_app_audio,
            &filter,
        );

        // Shown even once open-window candidates are already visible above
        // -- distinct from "no matches", it's telling the user more
        // (installed-only) results may still be on the way.
        if self.add_app.installed_rx.is_some() {
            ui.label(caption(s.add_app_scanning, pal().text_muted));
        }
        if candidates.is_empty() && self.add_app.installed_rx.is_none() {
            let msg = if filter.is_empty() {
                s.add_app_none_running
            } else {
                s.add_app_no_matches
            };
            ui.label(caption(msg, pal().text_muted));
        }
        let picked =
            render_add_app_candidate_list(ui, s, &candidates, &mut self.add_app.icon_textures);
        if let Some((exe_name, source)) = picked {
            let exe_path = match source {
                AddAppSource::OpenWindow(pid) => {
                    crate::sources::get_exe_path(pid).unwrap_or_default()
                }
                AddAppSource::Installed(path) => path,
            };
            self.register_app_and_close_picker(exe_name, exe_path, s);
        }

        ui.add_space(SPACE_TIGHT);
        ui.horizontal(|ui| {
            if ui
                .add(accent_button(s.add_app_browse_button, pal().link))
                .clicked()
                && let Some(path) = FileDialog::new()
                    .add_filter(s.exe_file_type, &["exe"])
                    .pick_file()
                && let Some(exe_name) = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
            {
                self.register_app_and_close_picker(
                    exe_name,
                    path.to_string_lossy().into_owned(),
                    s,
                );
            }
            if ui.add(accent_button(s.close_button, pal().link)).clicked() {
                self.add_app.open = false;
            }
        });
    }

    /// Pins `exe_name`/`exe_path`, rebuilds the Applications list to reflect
    /// it immediately, and closes the add-app picker -- the common tail of
    /// both ways the picker can register an app (picked from the search
    /// list, or via the "Browse for .exe instead…" fallback).
    fn register_app_and_close_picker(&mut self, exe_name: String, exe_path: String, s: &Strings) {
        self.config.register_app_audio(exe_name, exe_path);
        self.save_config_or_report(s);
        self.rebuild_app_audio_sources_now();
        self.add_app.open = false;
    }
}

/// The picker's search results for `filter` (already lowercased): open
/// windows (`sources`) first, then `installed` apps, one row per exe, minus
/// anything already `registered`, sorted by name.
fn add_app_candidates<'a>(
    sources: &'a [CaptureSource],
    installed: &'a [InstalledApp],
    registered: &[RegisteredApp],
    filter: &str,
) -> Vec<AddAppCandidate<'a>> {
    let registered: std::collections::HashSet<&str> =
        registered.iter().map(|r| r.exe_name.as_str()).collect();
    let mut seen_exe_names = std::collections::HashSet::new();
    let mut candidates: Vec<AddAppCandidate> = Vec::new();
    for src in sources
        .iter()
        .filter(|src| src.kind == CaptureKind::Window && !src.exe_name.is_empty())
    {
        if registered.contains(src.exe_name.as_str())
            || !matches_search(filter, &[&src.window_title, &src.exe_name])
        {
            continue;
        }
        // One row per exe, not per window -- a multi-window app (several
        // File Explorer windows, several browser windows) would otherwise
        // list the same exe repeatedly.
        if !seen_exe_names.insert(src.exe_name.to_lowercase()) {
            continue;
        }
        candidates.push(AddAppCandidate {
            display_name: crate::sources::display_name_from_exe_name(&src.exe_name),
            exe_name: &src.exe_name,
            source: AddAppSource::OpenWindow(src.process_id),
            icon_rgba: src.icon_rgba.as_ref(),
        });
    }
    for app in installed {
        if registered.contains(app.exe_name.as_str())
            || !matches_search(filter, &[&app.display_name, &app.exe_name])
            || !seen_exe_names.insert(app.exe_name.to_lowercase())
        {
            continue;
        }
        candidates.push(AddAppCandidate {
            display_name: app.display_name.clone(),
            exe_name: &app.exe_name,
            source: AddAppSource::Installed(app.exe_path.clone()),
            icon_rgba: app.icon_rgba.as_ref(),
        });
    }
    candidates.sort_by_key(|c| c.display_name.to_lowercase());
    candidates
}

/// The picker's scrollable candidate rows, their icons loaded into
/// `icon_textures`; returns the one clicked this frame.
fn render_add_app_candidate_list(
    ui: &mut egui::Ui,
    s: &'static Strings,
    candidates: &[AddAppCandidate],
    icon_textures: &mut std::collections::HashMap<String, egui::TextureHandle>,
) -> Option<(String, AddAppSource)> {
    let mut picked: Option<(String, AddAppSource)> = None;
    egui::ScrollArea::vertical()
        .max_height(220.0)
        .id_salt("add_app_picker_scroll")
        .show(ui, |ui| {
            for candidate in candidates {
                let icon = cached_icon_texture(
                    ui.ctx(),
                    icon_textures,
                    candidate.exe_name.to_lowercase(),
                    || format!("add_app_icon_{}", candidate.exe_name),
                    candidate.icon_rgba,
                );
                ui.horizontal(|ui| {
                    if let Some(icon) = icon {
                        ui.image((icon, egui::vec2(16.0, 16.0)));
                    }
                    // `.truncate()` -- an installed app's display name
                    // (derived from its Start Menu shortcut's filename, not
                    // curated) can be long enough to force this button wider
                    // than the popup's fixed width (see POPUP_WIDTH), pushing
                    // content past the window's right edge instead of the
                    // window resizing to fit -- a plain `ui.button(text)`
                    // doesn't wrap or clip its label to the available row
                    // width on its own.
                    if ui
                        .add(egui::Button::new(&candidate.display_name).truncate())
                        .clicked()
                    {
                        picked = Some((candidate.exe_name.to_owned(), candidate.source.clone()));
                    }
                    if candidate.is_running() {
                        ui.label(caption(s.add_app_running_tag, pal().success));
                    }
                });
            }
        });
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(title: &str, exe: &str, pid: u32) -> CaptureSource {
        CaptureSource {
            kind: CaptureKind::Window,
            process_id: pid,
            window_title: title.into(),
            exe_name: exe.into(),
            hwnd: pid as usize,
            icon_rgba: Some((vec![1, 2, 3, 4], 1, 1)),
        }
    }

    fn installed(name: &str, exe: &str) -> InstalledApp {
        InstalledApp {
            display_name: name.into(),
            exe_name: exe.into(),
            exe_path: format!(r"C:\Apps\{exe}"),
            icon_rgba: None,
        }
    }

    /// Open windows and installed apps, one row per exe -- a running app as
    /// its window, not again as installed -- minus what's registered, sorted
    /// by name, the icons borrowed from where they were found.
    #[test]
    fn the_picker_lists_each_app_once_running_first() {
        let sources = [
            window("Chat - Discord", "Discord.exe", 10),
            window("Another Discord window", "Discord.exe", 11),
            window("Notes", "notepad.exe", 12),
        ];
        let apps = [
            installed("Discord", "Discord.exe"),
            installed("Audacity", "Audacity.exe"),
            installed("Notepad", "notepad.exe"),
        ];
        let registered = [RegisteredApp {
            exe_name: "notepad.exe".into(),
            exe_path: String::new(),
        }];
        let rows = add_app_candidates(&sources, &apps, &registered, "");
        let names: Vec<_> = rows.iter().map(|c| (c.exe_name, c.is_running())).collect();
        assert_eq!(names, [("Audacity.exe", false), ("Discord.exe", true)]);
        assert!(std::ptr::eq(
            rows[1].icon_rgba.unwrap(),
            sources[0].icon_rgba.as_ref().unwrap()
        ));
    }

    /// The search matches a window's title or exe, an installed app's name
    /// or exe; the filter comes in lowercased.
    #[test]
    fn the_picker_search_matches_title_name_or_exe() {
        let sources = [window("Chat - Discord", "Discord.exe", 10)];
        let apps = [installed("Audacity", "audacity.exe")];
        let found = |filter| {
            add_app_candidates(&sources, &apps, &[], filter)
                .iter()
                .map(|c| c.exe_name.to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(found("chat"), ["Discord.exe"]);
        assert_eq!(found("discord.exe"), ["Discord.exe"]);
        assert_eq!(found("auda"), ["audacity.exe"]);
        assert!(found("zzz").is_empty());
    }
}
