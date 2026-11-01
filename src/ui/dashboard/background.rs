use super::App;
use super::overlay::overlay_viewport_id;
use super::state::{ExportState, HighlightSaveState};
use super::toast::{Toast, hotkey_toast, toast_viewport_id};
use super::util::take_if_finished;
use crate::hotkeys::HotkeyEvent;
use crate::i18n::Strings;
use eframe::egui;
use std::sync::atomic::Ordering;

impl App {
    pub(super) fn poll_background_work(&mut self, ctx: &egui::Context, s: &'static Strings) {
        let is_recording = self.session.is_recording();

        self.refresh_free_space();
        self.refresh_sources_and_audio_if_due(ctx);
        self.poll_self_update_result(ctx);

        // Poll update-check result (one-shot; None result also clears the receiver
        // so we stop polling a channel whose sender has already sent its one message)
        if let Some(rx) = &self.update_check_rx
            && let Ok(result) = rx.try_recv()
        {
            self.update_available = result;
            self.update_check_rx = None;
        }

        // Poll the add-app picker's background Start Menu scan -- see
        // `add_app.installed_rx`'s doc comment for why this isn't done
        // synchronously on the click that opens the picker.
        if let Some(rx) = &self.add_app.installed_rx
            && let Ok(apps) = rx.try_recv()
        {
            self.add_app.installed = apps;
            self.add_app.installed_rx = None;
        }

        // Poll export result channel
        let export_result = self
            .export
            .result_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        if let Some(result) = export_result {
            self.export.state = match result {
                Ok(path) => ExportState::Done(path),
                Err(msg) => ExportState::Failed(msg),
            };
            self.export.result_rx = None;
        }

        // Poll hotkey events (non-blocking)
        while let Some(event) = self.hotkey_listener.as_ref().and_then(|h| h.try_recv()) {
            let before = self.session.state().clone();
            match event {
                HotkeyEvent::StartStop => self.handle_hotkey_start_stop(is_recording),
                HotkeyEvent::Pause => self.handle_pause_button(),
                HotkeyEvent::ToggleOverlay => {
                    self.overlay_enabled = !self.overlay_enabled;
                    self.config.overlay.enabled = self.overlay_enabled;
                }
                HotkeyEvent::SaveHighlight => self.handle_save_highlight_hotkey(s),
            }
            let highlight_saving = matches!(self.highlight_save.state, HighlightSaveState::Saving);
            if let Some(text) = hotkey_toast(
                event,
                &before,
                self.session.state(),
                self.overlay_enabled,
                highlight_saving,
                s,
            ) {
                self.toast = Some(Toast::new(text));
            }
        }

        self.refresh_highlight_buffering();
        self.poll_highlight_save_result();
        self.check_fps_shortfall(s);

        // The recorder can stop itself early (disk full — see disk_space.rs)
        // without the user pressing stop. Detect that and run the normal stop
        // sequence so the now-pointless capture/pump tasks get aborted and the
        // result flows through the same finalize handling below as any other
        // stop, instead of the UI being stuck showing "Recording" forever for
        // a capture that already ended on its own.
        if (is_recording || self.session.is_paused())
            && self.finalizing.handle.is_none()
            && self.session.active().is_some_and(|a| a.recorder_finished())
        {
            self.stop_recording();
        }

        // Show export controls (inline in the status panel) once recorder has
        // finished writing the file
        if let Some(handle) = take_if_finished(&mut self.finalizing.handle) {
            self.finalizing.path = None;
            let disk_full = self
                .finalizing
                .disk_full
                .take()
                .is_some_and(|f| f.load(Ordering::Relaxed));
            match tokio::runtime::Handle::current().block_on(handle) {
                Ok(Ok(path)) => {
                    // Probed from the file itself, not trusted from the
                    // pre-recording device selection -- see field doc on
                    // export.available_tracks.
                    self.export.available_tracks = crate::encode::remux::count_audio_tracks(&path)
                        .inspect_err(|e| {
                            tracing::warn!(
                                "count_audio_tracks failed, export controls will stay hidden: {e}"
                            )
                        })
                        .unwrap_or(0);
                    tracing::info!(
                        "recording finalized: {} ({} audio track(s))",
                        path.display(),
                        self.export.available_tracks
                    );
                    self.export.track_selection = vec![true; self.export.available_tracks];
                    self.export.state = ExportState::Idle;
                    self.export.result_rx = None;
                    self.last_output_path = Some(path);
                    self.refresh_recent();
                    let lost = std::mem::take(&mut self.finalizing.lost_sources);
                    if disk_full {
                        self.notice = Some(s.disk_full_mid_recording.to_string());
                    } else if let Some(msg) =
                        lost_sources_message(s, &lost, &self.last_recording_audio_labels)
                    {
                        self.notice = Some(msg);
                    } else if let Some(msg) = audio_tracks_missing_message(
                        s,
                        self.export.available_tracks,
                        self.last_recording_audio_labels.len(),
                    ) {
                        self.notice = Some(msg);
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!("recording finalize failed: {e}");
                    self.error_message = Some(format!("{}{e}", s.recording_failed_prefix));
                }
                Err(e) => {
                    tracing::error!("recorder task did not complete cleanly: {e}");
                    self.error_message =
                        Some(format!("{}{e}", s.recording_ended_unexpectedly_prefix));
                }
            }
        }
    }

    // Takes `&mut egui::Ui` (the root Ui from App::ui), not `&egui::Context` --
    // Panel's old ctx-based top-level `.show(ctx, ...)` (what
    // TopBottomPanel/SidePanel used to be deprecated aliases for) was removed
    // entirely in egui 0.35; the replacement needs an existing Ui to nest
    // inside. CentralPanel takes the same shape for consistency.

    pub(super) fn request_repaints(&self, ctx: &egui::Context) {
        // Unconditional, independent of every other state below -- without
        // this, refresh_sources_and_audio_if_due's 2s timer only actually
        // fires when something else happens to wake the UI (mouse move,
        // click), same class of bug as the Highlight-save repaint gap: the
        // background check would run, but the app would sit fully idle and
        // never repaint to pick up its result.
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
        let (focused, minimized) = ctx.input(|i| (i.viewport().focused, i.viewport().minimized));
        if self.session.is_recording()
            && recording_needs_fast_repaint(focused, minimized, self.overlay_visible())
        {
            // 33 ms ≈ 30 fps; needed for smooth pulsing dot animation
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
        if matches!(self.export.state, ExportState::Running) {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if self.session.is_paused() {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        if self.finalizing.handle.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        // Without this, a Highlight save that finishes in the background
        // between frames never gets picked up by poll_highlight_save_result
        // until something else happens to wake the UI (e.g. another
        // keypress) -- looking like the save silently did nothing on its
        // first press.
        if matches!(self.highlight_save.state, HighlightSaveState::Saving) {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    /// The overlay HUD and the hotkey toast are drawn from `ui`, which eframe
    /// skips while the dashboard is minimized -- unless one of them is up
    /// already. So when one is wanted but not up yet (a hotkey pressed with
    /// the dashboard minimized), ask for a `ui` pass anyway, which puts it up.
    pub(super) fn request_ui_for_new_windows_while_minimized(&self, ctx: &egui::Context) {
        let (minimized, up) = ctx.input(|i| {
            let up = |id| i.raw.viewports.contains_key(&id);
            (
                i.viewport().minimized == Some(true),
                [up(overlay_viewport_id()), up(toast_viewport_id())],
            )
        });
        let wanted = [self.overlay_visible(), self.toast.is_some()];
        if needs_ui_while_minimized(minimized, wanted, up) {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
        }
    }

    pub(super) fn poll_highlight_save_result(&mut self) {
        let Some(handle) = take_if_finished(&mut self.highlight_save.handle) else {
            return;
        };
        self.highlight_save.state = match tokio::runtime::Handle::current().block_on(handle) {
            Ok(Ok(path)) => {
                self.refresh_recent();
                HighlightSaveState::Done(path)
            }
            Ok(Err(e)) => HighlightSaveState::Failed(e.to_string()),
            Err(e) => HighlightSaveState::Failed(e.to_string()),
        };
    }

    /// One-shot-per-recording check: has the achieved frame rate
    /// (frame_count / elapsed) fallen persistently short of the configured
    /// target? The "frames" counter next to "tracks" in the status panel is
    /// a real throughput signal -- it only increments once a frame clears
    /// the whole pipeline through to the recording channel -- but nothing
    /// previously surfaced a sustained shortfall to the user; it just
    /// silently degraded. Waits past WARMUP_SECS before checking, so brief
    /// startup/encoder-warmup noise (and the first few seconds of a normal
    /// recording, where elapsed is small enough for count/elapsed to be
    /// noisy) doesn't false-positive.
    fn check_fps_shortfall(&mut self, s: &Strings) {
        const WARMUP_SECS: f64 = 10.0;
        const SHORTFALL_THRESHOLD: f64 = 0.75;

        if self.fps_shortfall_warned || !self.session.is_recording() {
            return;
        }
        let Some(start) = self.recording_start else {
            return;
        };
        let elapsed_secs = start.elapsed().as_secs_f64();
        if elapsed_secs < WARMUP_SECS {
            return;
        }
        let achieved_fps = self.frame_count.load(Ordering::Relaxed) as f64 / elapsed_secs;
        // The rate this recording really runs at, which can be below the
        // setting when the display refreshes slower or the encoder turns the
        // rate down -- see `LiveFps`.
        let Some(target_fps) = self.session.active().map(|a| f64::from(a.fps.get())) else {
            return;
        };
        if achieved_fps < target_fps * SHORTFALL_THRESHOLD {
            self.fps_shortfall_warned = true;
            self.notice = Some(
                s.fps_shortfall_template
                    .replace("{achieved}", &format!("{achieved_fps:.0}"))
                    .replace("{target}", &format!("{target_fps:.0}")),
            );
        }
    }
}

/// Whether a recording's ~30 fps repaint (the pulsing dot) is worth it. Not
/// while the dashboard is minimized or another window has focus (typically
/// the game being recorded): nobody's watching the dot, the 1 s repaint still
/// ticks the timer, and 30 fps of UI rendering only competes with the game
/// for the GPU. The overlay HUD is the exception: it renders with the
/// dashboard, and its every-frame topmost re-assertion is what keeps a
/// fullscreen game from covering it, so it keeps the full rate. `None` (not
/// reported yet) counts as focused and not minimized.
fn recording_needs_fast_repaint(
    focused: Option<bool>,
    minimized: Option<bool>,
    overlay_visible: bool,
) -> bool {
    overlay_visible || (focused != Some(false) && minimized != Some(true))
}

/// Whether a minimized dashboard needs a `ui` pass to put up a window of its
/// own: one that's `wanted` but not `up` yet (by position: overlay, toast).
fn needs_ui_while_minimized(minimized: bool, wanted: [bool; 2], up: [bool; 2]) -> bool {
    minimized && wanted.into_iter().zip(up).any(|(wanted, up)| wanted && !up)
}

/// `None` when `actual >= expected` (nothing missing, or the recording
/// legitimately ended up with more tracks than expected -- see
/// `track_label_fallback`'s doc comment for that case, which this doesn't
/// need to warn about). Otherwise fills in `s.audio_tracks_missing_template`'s
/// `{actual}`/`{expected}` placeholders.
/// What a recording lost before it was stopped, if anything -- see
/// `capture::LostSources`. `labels` names its audio tracks, in track order.
fn lost_sources_message(
    s: &Strings,
    lost: &crate::capture::LostSources,
    labels: &[impl std::fmt::Display],
) -> Option<String> {
    let mut parts = Vec::new();
    if lost.video {
        parts.push(s.notice_video_source_lost.to_owned());
    }
    if !lost.audio.is_empty() {
        let tracks: Vec<String> = lost
            .audio
            .iter()
            .map(|track| {
                labels
                    .get(track.0 as usize)
                    .map_or_else(|| format!("#{}", track.0 + 1), ToString::to_string)
            })
            .collect();
        parts.push(
            s.notice_audio_source_lost_template
                .replace("{tracks}", &tracks.join(", ")),
        );
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn audio_tracks_missing_message(s: &Strings, actual: usize, expected: usize) -> Option<String> {
    if actual >= expected {
        return None;
    }
    Some(
        s.audio_tracks_missing_template
            .replace("{actual}", &actual.to_string())
            .replace("{expected}", &expected.to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

    #[test]
    fn fast_repaint_only_while_someone_can_see_the_animation() {
        // Focused (or not reported yet): full rate.
        assert!(recording_needs_fast_repaint(Some(true), Some(false), false));
        assert!(recording_needs_fast_repaint(None, None, false));
        // In the background or minimized: the 1 s tick is enough...
        assert!(!recording_needs_fast_repaint(
            Some(false),
            Some(false),
            false
        ));
        assert!(!recording_needs_fast_repaint(Some(true), Some(true), false));
        // ...unless the overlay HUD is up.
        assert!(recording_needs_fast_repaint(Some(false), Some(false), true));
        assert!(recording_needs_fast_repaint(Some(false), Some(true), true));
    }

    #[test]
    fn minimized_dashboard_gets_a_ui_pass_only_to_put_up_a_missing_window() {
        // Overlay wanted but not up yet (a hotkey started a recording).
        assert!(needs_ui_while_minimized(
            true,
            [true, false],
            [false, false]
        ));
        // Toast wanted but not up yet.
        assert!(needs_ui_while_minimized(
            true,
            [false, true],
            [false, false]
        ));
        // Already up: eframe keeps running `ui` for it on its own.
        assert!(!needs_ui_while_minimized(true, [true, true], [true, true]));
        // Nothing wanted.
        assert!(!needs_ui_while_minimized(
            true,
            [false, false],
            [false, false]
        ));
        // Not minimized: `ui` runs anyway.
        assert!(!needs_ui_while_minimized(
            false,
            [true, true],
            [false, false]
        ));
    }

    #[test]
    fn lost_sources_name_the_tracks_that_went_quiet() {
        use crate::capture::LostSources;
        use crate::types::TrackId;
        let s = Lang::En.strings();
        let labels = [
            "🔊 Speakers".to_string(),
            "🎙 Mic".to_string(),
            "Discord".to_string(),
        ];
        assert_eq!(
            lost_sources_message(s, &LostSources::default(), &labels),
            None
        );

        let audio = LostSources {
            video: false,
            audio: vec![TrackId::new(1), TrackId::new(2)],
        };
        assert_eq!(
            lost_sources_message(s, &audio, &labels).unwrap(),
            s.notice_audio_source_lost_template
                .replace("{tracks}", "🎙 Mic, Discord")
        );

        let both = LostSources {
            video: true,
            audio: vec![TrackId::new(7)],
        };
        assert_eq!(
            lost_sources_message(s, &both, &labels).unwrap(),
            format!(
                "{} {}",
                s.notice_video_source_lost,
                s.notice_audio_source_lost_template
                    .replace("{tracks}", "#8")
            )
        );
    }

    #[test]
    fn audio_tracks_missing_message_none_when_all_tracks_present() {
        let s = Lang::En.strings();
        assert!(audio_tracks_missing_message(s, 3, 3).is_none());
    }

    #[test]
    fn audio_tracks_missing_message_none_when_actual_exceeds_expected() {
        let s = Lang::En.strings();
        assert!(audio_tracks_missing_message(s, 4, 3).is_none());
    }

    #[test]
    fn audio_tracks_missing_message_none_when_nothing_was_expected() {
        let s = Lang::En.strings();
        assert!(audio_tracks_missing_message(s, 0, 0).is_none());
    }

    #[test]
    fn audio_tracks_missing_message_fills_in_both_counts() {
        let s = Lang::En.strings();
        let msg = audio_tracks_missing_message(s, 2, 4).expect("expected a message");
        assert!(msg.contains("2 of 4"), "message was: {msg}");
        assert!(!msg.contains('{'), "unreplaced placeholder in: {msg}");
    }

    #[test]
    fn audio_tracks_missing_message_has_no_unreplaced_placeholders_in_every_language() {
        for lang in [Lang::En, Lang::Ja] {
            let msg =
                audio_tracks_missing_message(lang.strings(), 1, 2).expect("expected a message");
            assert!(
                !msg.contains('{'),
                "{lang:?}: unreplaced placeholder in: {msg}"
            );
        }
    }
}
