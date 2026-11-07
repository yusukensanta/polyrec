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

        // Recordings cut off last time, finished off the dashboard's thread
        // (see `start_recovery`).
        if let Some(rx) = &self.recovery_rx
            && let Ok(recovered) = rx.try_recv()
        {
            self.recovery_rx = None;
            if let Some(notice) = recovered_notice(&recovered, s) {
                self.notices.push(notice);
                self.refresh_recent();
            }
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
                HotkeyEvent::StartStop => self.handle_hotkey_start_stop(s),
                HotkeyEvent::Pause => self.handle_pause_button(),
                HotkeyEvent::ToggleOverlay => {
                    self.config.overlay.enabled = !self.config.overlay.enabled;
                    self.save_config_or_report(s);
                }
                HotkeyEvent::SaveHighlight => self.handle_save_highlight_hotkey(s),
            }
            let highlight_saving = matches!(self.highlight_save.state, HighlightSaveState::Saving);
            if let Some(text) = hotkey_toast(
                event,
                &before,
                self.session.state(),
                self.config.overlay.enabled,
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
        if self.finalizing.handle.is_none()
            && self.session.active().is_some_and(|a| a.recorder_finished())
        {
            self.stop_recording();
        }

        // Show export controls (inline in the status panel) once recorder has
        // finished writing the file
        if let Some(handle) = take_if_finished(&mut self.finalizing.handle) {
            let stopped_itself = std::mem::take(&mut self.finalizing.stopped_itself);
            let (disk_full, gave_up) = (stopped_itself.disk_full(), stopped_itself.gave_up());
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
                    self.last_recording.saved = Some(path);
                    self.refresh_recent();
                    let lost = std::mem::take(&mut self.finalizing.lost_sources);
                    if let Some(why) = gave_up {
                        self.notices.push(recording_gave_up_message(s, &why));
                    } else if disk_full {
                        self.notices.push(s.disk_full_mid_recording);
                    } else if let Some(msg) =
                        lost_sources_message(s, &lost, &self.last_recording.audio_labels)
                    {
                        self.notices.push(msg);
                    } else if let Some(msg) = audio_tracks_missing_message(
                        s,
                        self.export.available_tracks,
                        self.last_recording.audio_labels.len(),
                    ) {
                        self.notices.push(msg);
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!("recording finalize failed: {e}");
                    // What made it give up says more than finishing the file
                    // failing too.
                    let why = gave_up.unwrap_or_else(|| e.to_string());
                    self.error_message = Some(format!("{}{why}", s.recording_failed_prefix));
                }
                Err(e) => {
                    tracing::error!("recorder task did not complete cleanly: {e}");
                    self.error_message =
                        Some(format!("{}{e}", s.recording_ended_unexpectedly_prefix));
                }
            }
        }
    }

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
    /// target? The fps the status panel shows is a real throughput signal --
    /// a frame only counts once it clears the whole pipeline through to the
    /// recording channel -- but nothing previously surfaced a sustained
    /// shortfall to the user; it just silently degraded. Waits past `WARMUP`
    /// before checking, so brief
    /// startup/encoder-warmup noise (and the first few seconds of a normal
    /// recording, where elapsed is small enough for count/elapsed to be
    /// noisy) doesn't false-positive.
    fn check_fps_shortfall(&mut self, s: &Strings) {
        if self.fps_shortfall_warned || !self.session.is_recording() {
            return;
        }
        let Some(active) = self.session.active() else {
            return;
        };
        // The rate this recording really runs at, which can be below the
        // setting when the display refreshes slower or the encoder turns the
        // rate down -- see `LiveFps`.
        let target_fps = f64::from(active.fps.get());
        if let Some(achieved_fps) = fps_shortfall(
            self.frame_count.load(Ordering::Relaxed),
            active.elapsed(),
            target_fps,
        ) {
            self.fps_shortfall_warned = true;
            self.notices.push(
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

/// The notice for recordings cut off last time and saved now (see
/// `session::recovery`), if there are any.
fn recovered_notice(recovered: &[std::path::PathBuf], s: &'static Strings) -> Option<String> {
    let names: Vec<_> = recovered
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy())
        .collect();
    (!names.is_empty()).then(|| {
        s.notice_recovered_template
            .replace("{files}", &names.join(", "))
    })
}

/// What a recording lost before it was stopped, if anything -- see
/// `capture::LostSources`. `labels` names its audio tracks, in track order.
fn lost_sources_message(
    s: &Strings,
    lost: &crate::capture::LostSources,
    labels: &[impl std::fmt::Display],
) -> Option<String> {
    let names = |tracks: &[crate::types::TrackId]| {
        tracks
            .iter()
            .map(|track| {
                labels
                    .get(track.0 as usize)
                    .map_or_else(|| format!("#{}", track.0 + 1), ToString::to_string)
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut parts = Vec::new();
    if lost.video {
        parts.push(s.notice_video_source_lost.to_owned());
    }
    for (tracks, template) in [
        (&lost.audio, s.notice_audio_source_lost_template),
        (
            &lost.audio_interrupted,
            s.notice_audio_source_interrupted_template,
        ),
    ] {
        if !tracks.is_empty() {
            parts.push(template.replace("{tracks}", &names(tracks)));
        }
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// The frame rate a recording achieved over `elapsed` of recording (pauses
/// left out: no frames come then) if it's persistently short of
/// `target_fps`. Judged only past a warm-up, when `frames / elapsed` is no
/// longer noise. The time since Start used to be the measure, so a pause
/// counted as frames missed: 5 s recorded, a minute paused, and resuming
/// said the PC managed 5 of 60 frames a second.
fn fps_shortfall(frames: u64, elapsed: std::time::Duration, target_fps: f64) -> Option<f64> {
    const WARMUP: std::time::Duration = std::time::Duration::from_secs(10);
    const SHORTFALL_THRESHOLD: f64 = 0.75;
    if elapsed < WARMUP {
        return None;
    }
    let achieved = frames as f64 / elapsed.as_secs_f64();
    (achieved < target_fps * SHORTFALL_THRESHOLD).then_some(achieved)
}

/// `None` when `actual >= expected` (nothing missing, or the recording
/// legitimately ended up with more tracks than expected -- see
/// `track_label_fallback`'s doc comment for that case, which this doesn't
/// need to warn about). Otherwise fills in `s.audio_tracks_missing_template`'s
/// `{actual}`/`{expected}` placeholders.
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

/// The notice for a recording the recorder stopped because writing it kept
/// failing (`why`) -- see `encode::actor::GaveUp`.
fn recording_gave_up_message(s: &Strings, why: &str) -> String {
    s.notice_recording_gave_up.replace("{error}", why)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

    #[test]
    fn recovered_recordings_are_named_in_one_notice() {
        let s = Lang::En.strings();
        assert_eq!(recovered_notice(&[], s), None);
        let recovered = [
            std::path::PathBuf::from(r"D:\videos\polyrec\game_2026-11-05-01-02-03_recovered.mp4"),
            std::path::PathBuf::from(r"D:\videos\polyrec\app_2026-11-04-20-00-00_recovered.mp4"),
        ];
        let notice = recovered_notice(&recovered, s).unwrap();
        assert!(
            notice.contains(
                "game_2026-11-05-01-02-03_recovered.mp4, app_2026-11-04-20-00-00_recovered.mp4"
            ),
            "{notice}"
        );
        assert!(!notice.contains("{files}"));
    }

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
    fn a_frame_rate_is_judged_on_the_time_spent_recording() {
        let secs = std::time::Duration::from_secs;
        // Too early to tell.
        assert_eq!(fps_shortfall(90, secs(9), 60.0), None);
        // 60 a second over 10 s recorded -- however long it was paused for.
        assert_eq!(fps_shortfall(600, secs(10), 60.0), None);
        assert_eq!(fps_shortfall(400, secs(10), 60.0), Some(40.0));
        // Under three quarters is short; three quarters isn't.
        assert_eq!(fps_shortfall(450, secs(10), 60.0), None);
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
            audio_interrupted: vec![],
        };
        assert_eq!(
            lost_sources_message(s, &audio, &labels).unwrap(),
            s.notice_audio_source_lost_template
                .replace("{tracks}", "🎙 Mic, Discord")
        );

        let both = LostSources {
            video: true,
            audio: vec![TrackId::new(7)],
            audio_interrupted: vec![],
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

        let back = LostSources {
            video: false,
            audio: vec![],
            audio_interrupted: vec![TrackId::new(0)],
        };
        assert_eq!(
            lost_sources_message(s, &back, &labels).unwrap(),
            s.notice_audio_source_interrupted_template
                .replace("{tracks}", "🔊 Speakers")
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

    /// Says why, in every language.
    #[test]
    fn a_recording_that_gave_up_says_why() {
        for lang in [Lang::En, Lang::Ja] {
            let msg = recording_gave_up_message(lang.strings(), "device removed");
            assert!(msg.contains("device removed"), "{lang:?}: {msg}");
            assert!(!msg.contains('{'), "{lang:?}: {msg}");
        }
    }
}
