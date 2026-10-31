//! The notification-area (tray) icon. Clicking it brings the dashboard up;
//! its menu shows or hides the dashboard, starts or stops a recording and
//! quits -- so PolyRec can stay out of the taskbar while it waits for a
//! hotkey. Closing the dashboard still quits, as before.

use super::App;
use crate::i18n::Strings;
use crate::types::SessionState;
use eframe::egui;
use std::sync::mpsc;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// The application icon `build.rs` embeds (winresource's default id).
const APP_ICON_RESOURCE: u16 = 1;

/// Longest source title the Start item shows before cutting it off.
const MAX_SOURCE_CHARS: usize = 40;

const MENU_WINDOW: &str = "window";
const MENU_RECORD: &str = "record";
const MENU_QUIT: &str = "quit";

/// What the user asked for through the tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TrayAction {
    /// The icon was clicked: bring the dashboard up.
    Show,
    /// The Show/Hide item.
    ToggleWindow,
    /// The Start/Stop item.
    StartStop,
    Quit,
}

/// What the tray shows -- worked out from the app's state every frame by
/// `tray_view`, but handed to Windows only when it changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TrayView {
    tooltip: String,
    window_item: &'static str,
    record_item: String,
    record_enabled: bool,
    quit_item: &'static str,
}

pub(super) struct Tray {
    icon: TrayIcon,
    window_item: MenuItem,
    record_item: MenuItem,
    quit_item: MenuItem,
    actions: mpsc::Receiver<TrayAction>,
    shown: Option<TrayView>,
}

impl Tray {
    /// Puts the icon in the notification area. Must be called on the thread
    /// running the event loop (eframe's main thread): the icon's clicks
    /// arrive as window messages there. `wake` is called after each action,
    /// so the app gets to handle it straight away.
    pub(super) fn new(wake: impl Fn() + Send + Sync + 'static) -> Result<Self, String> {
        let window_item = MenuItem::with_id(MENU_WINDOW, "", true, None);
        let record_item = MenuItem::with_id(MENU_RECORD, "", false, None);
        let quit_item = MenuItem::with_id(MENU_QUIT, "", true, None);
        let menu = Menu::with_items(&[
            &window_item,
            &PredefinedMenuItem::separator(),
            &record_item,
            &PredefinedMenuItem::separator(),
            &quit_item,
        ])
        .map_err(|e| format!("tray menu: {e}"))?;
        let icon = TrayIconBuilder::new()
            .with_icon(app_icon()?)
            .with_tooltip("PolyRec")
            .with_menu(Box::new(menu))
            // Left click brings the dashboard up; the menu is on right click.
            .with_menu_on_left_click(false)
            .build()
            .map_err(|e| format!("tray icon: {e}"))?;

        let (tx, actions) = mpsc::channel();
        let wake = std::sync::Arc::new(wake);
        let (menu_tx, menu_wake) = (tx.clone(), wake.clone());
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let action = match event.id.as_ref() {
                MENU_WINDOW => TrayAction::ToggleWindow,
                MENU_RECORD => TrayAction::StartStop,
                MENU_QUIT => TrayAction::Quit,
                _ => return,
            };
            let _ = menu_tx.send(action);
            menu_wake();
        }));
        TrayIconEvent::set_event_handler(Some(move |event| {
            if is_left_click(&event) {
                let _ = tx.send(TrayAction::Show);
                wake();
            }
        }));

        Ok(Self {
            icon,
            window_item,
            record_item,
            quit_item,
            actions,
            shown: None,
        })
    }

    pub(super) fn try_recv(&self) -> Option<TrayAction> {
        self.actions.try_recv().ok()
    }

    /// Updates the tooltip and menu to `view`, if that's not what they
    /// already show.
    fn show(&mut self, view: TrayView) {
        if self.shown.as_ref() == Some(&view) {
            return;
        }
        if let Err(e) = self.icon.set_tooltip(Some(&view.tooltip)) {
            tracing::warn!("tray tooltip: {e}");
        }
        self.window_item.set_text(view.window_item);
        self.record_item.set_text(&view.record_item);
        self.record_item.set_enabled(view.record_enabled);
        self.quit_item.set_text(view.quit_item);
        self.shown = Some(view);
    }
}

/// The app icon at the notification area's size, from the exe's own icon
/// resource -- which has the small sizes drawn, rather than scaled down.
fn app_icon() -> Result<Icon, String> {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON, SM_CYSMICON};
    let size = unsafe { (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON)) };
    let size = (size.0 > 0 && size.1 > 0).then_some((size.0 as u32, size.1 as u32));
    Icon::from_resource(APP_ICON_RESOURCE, size).map_err(|e| format!("tray icon image: {e}"))
}

fn is_left_click(event: &TrayIconEvent) -> bool {
    matches!(
        event,
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } | TrayIconEvent::DoubleClick {
            button: MouseButton::Left,
            ..
        }
    )
}

/// What the tray should show. `start_source` is the selected source's title
/// when a recording can start (see `App::rec_blocked_reason`), `None` when
/// it can't.
fn tray_view(
    s: &Strings,
    state: &SessionState,
    window_shown: bool,
    start_source: Option<&str>,
) -> TrayView {
    let tooltip = match state {
        SessionState::Idle => "PolyRec".to_owned(),
        SessionState::Recording => format!("PolyRec — {}", s.state_recording),
        SessionState::Paused => format!("PolyRec — {}", s.state_paused),
    };
    let (record_item, record_enabled) = match (state, start_source) {
        (SessionState::Recording | SessionState::Paused, _) => (s.tray_stop.to_owned(), true),
        (SessionState::Idle, Some(source)) => (
            s.tray_start_source_template
                .replace("{source}", &shorten(source, MAX_SOURCE_CHARS)),
            true,
        ),
        (SessionState::Idle, None) => (s.tray_start.to_owned(), false),
    };
    TrayView {
        tooltip,
        window_item: if window_shown {
            s.tray_hide
        } else {
            s.tray_show
        },
        record_item,
        record_enabled,
        quit_item: s.tray_quit,
    }
}

/// `text` cut to `max` characters, with "…" in place of the rest. A menu
/// item can't wrap, and window titles run long.
fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(max.saturating_sub(1)).collect();
    short.push('…');
    short
}

impl App {
    /// Handles what was asked through the tray since the last frame, then
    /// brings the tray's menu and tooltip up to date.
    pub(super) fn poll_tray(&mut self, ctx: &egui::Context, s: &'static Strings) {
        while let Some(action) = self.tray.as_ref().and_then(Tray::try_recv) {
            match action {
                TrayAction::Show => self.set_dashboard_shown(ctx, true),
                TrayAction::ToggleWindow => {
                    let shown = self.dashboard_shown(ctx);
                    self.set_dashboard_shown(ctx, !shown);
                }
                TrayAction::StartStop => {
                    let is_recording = self.session.is_recording();
                    let can_stop = is_recording || self.session.is_paused();
                    if can_stop || self.rec_blocked_reason(s).is_none() {
                        self.handle_rec_button(is_recording);
                    }
                    // A start that didn't happen says why on the dashboard
                    // (an error, or REC's tooltip), which may be hidden.
                    if !can_stop && !self.session.is_recording() {
                        self.set_dashboard_shown(ctx, true);
                    }
                }
                TrayAction::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }

        let start_source = self
            .rec_blocked_reason(s)
            .is_none()
            .then(|| self.selected_source.and_then(|i| self.sources.get(i)))
            .flatten()
            .map(|source| source.window_title.as_str());
        let view = tray_view(
            s,
            self.session.state(),
            self.dashboard_shown(ctx),
            start_source,
        );
        if let Some(tray) = &mut self.tray {
            tray.show(view);
        }
    }

    /// Whether the dashboard is on screen: neither hidden to the tray nor
    /// minimized.
    fn dashboard_shown(&self, ctx: &egui::Context) -> bool {
        !self.dashboard_hidden && ctx.input(|i| i.viewport().minimized) != Some(true)
    }

    pub(super) fn set_dashboard_shown(&mut self, ctx: &egui::Context, shown: bool) {
        self.dashboard_hidden = !shown;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(shown));
        if shown {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

    #[test]
    fn idle_with_a_source_offers_to_record_it() {
        let s = Lang::En.strings();
        let view = tray_view(s, &SessionState::Idle, true, Some("Game"));
        assert_eq!(view.tooltip, "PolyRec");
        assert_eq!(view.window_item, s.tray_hide);
        assert_eq!(view.record_item, "Start recording: Game");
        assert!(view.record_enabled);
    }

    #[test]
    fn idle_without_a_source_disables_start() {
        let s = Lang::En.strings();
        let view = tray_view(s, &SessionState::Idle, false, None);
        assert_eq!(view.window_item, s.tray_show);
        assert_eq!(view.record_item, s.tray_start);
        assert!(!view.record_enabled);
    }

    #[test]
    fn recording_or_paused_offers_stop_and_says_so_in_the_tooltip() {
        let s = Lang::En.strings();
        let recording = tray_view(s, &SessionState::Recording, false, None);
        assert_eq!(
            recording.tooltip,
            format!("PolyRec — {}", s.state_recording)
        );
        assert_eq!(recording.record_item, s.tray_stop);
        assert!(recording.record_enabled);
        let paused = tray_view(s, &SessionState::Paused, false, Some("Game"));
        assert_eq!(paused.tooltip, format!("PolyRec — {}", s.state_paused));
        assert_eq!(paused.record_item, s.tray_stop);
    }

    #[test]
    fn every_language_fills_in_the_source() {
        for lang in [Lang::En, Lang::Ja] {
            let view = tray_view(lang.strings(), &SessionState::Idle, true, Some("Game"));
            assert!(view.record_item.contains("Game"), "{lang:?}");
            assert!(!view.record_item.contains('{'), "{lang:?}");
        }
    }

    #[test]
    fn long_titles_are_cut_with_an_ellipsis() {
        assert_eq!(shorten("short", 10), "short");
        assert_eq!(shorten("exactly10!", 10), "exactly10!");
        assert_eq!(shorten("eleven char", 10), "eleven ch…");
        // By character, not byte: no panic or split inside a character.
        assert_eq!(shorten("録画中のウィンドウ", 5), "録画中の…");
    }

    #[test]
    fn left_clicks_bring_the_dashboard_up_and_nothing_else_does() {
        let rect = tray_icon::Rect::default();
        let position = Default::default();
        let click = |button, button_state| TrayIconEvent::Click {
            id: Default::default(),
            position,
            rect,
            button,
            button_state,
        };
        assert!(is_left_click(&click(
            MouseButton::Left,
            MouseButtonState::Up
        )));
        assert!(!is_left_click(&click(
            MouseButton::Left,
            MouseButtonState::Down
        )));
        assert!(!is_left_click(&click(
            MouseButton::Right,
            MouseButtonState::Up
        )));
        assert!(is_left_click(&TrayIconEvent::DoubleClick {
            id: Default::default(),
            position,
            rect,
            button: MouseButton::Left,
        }));
    }
}
