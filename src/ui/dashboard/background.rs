use super::{App, ExportState, HighlightSaveState};
use crate::hotkeys::HotkeyEvent;
use crate::i18n::Strings;
use eframe::egui;
use std::sync::atomic::Ordering;

impl App {
    pub(super) fn poll_background_work(&mut self, ctx: &egui::Context, s: &'static Strings) {
        let is_recording = self.session.is_recording();

        self.refresh_free_space();
        self.refresh_sources_and_audio_if_due();
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
        // `add_app_installed_rx`'s doc comment for why this isn't done
        // synchronously on the click that opens the picker.
        if let Some(rx) = &self.add_app_installed_rx
            && let Ok(apps) = rx.try_recv()
        {
            self.add_app_installed = apps;
            self.add_app_installed_rx = None;
        }

        // Poll export result channel
        let export_result = self
            .export_result_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        if let Some(result) = export_result {
            self.export_state = match result {
                Ok(path) => ExportState::Done(path),
                Err(msg) => ExportState::Failed(msg),
            };
            self.export_result_rx = None;
        }

        // Poll hotkey events (non-blocking)
        while let Some(event) = self.hotkey_listener.as_ref().and_then(|h| h.try_recv()) {
            match event {
                HotkeyEvent::StartStop => self.handle_hotkey_start_stop(is_recording),
                HotkeyEvent::Pause => self.handle_pause_button(),
                HotkeyEvent::ToggleOverlay => {
                    self.overlay_enabled = !self.overlay_enabled;
                    self.config.overlay.enabled = self.overlay_enabled;
                }
                HotkeyEvent::SaveHighlight => self.handle_save_highlight_hotkey(s),
            }
        }

        self.refresh_highlight_buffering();
        self.poll_highlight_save_result();

        // The recorder can stop itself early (disk full — see disk_space.rs)
        // without the user pressing stop. Detect that and run the normal stop
        // sequence so the now-pointless capture/pump tasks get aborted and the
        // result flows through the same finalize handling below as any other
        // stop, instead of the UI being stuck showing "Recording" forever for
        // a capture that already ended on its own.
        if (is_recording || self.session.is_paused())
            && self.finalizing_handle.is_none()
            && self
                .session
                .active
                .as_ref()
                .is_some_and(|a| a.recorder_handle.is_finished())
        {
            self.stop_recording();
        }

        // Show export controls (inline in the status panel) once recorder has
        // finished writing the file
        if self
            .finalizing_handle
            .as_ref()
            .is_some_and(|h| h.is_finished())
        {
            let handle = self.finalizing_handle.take().unwrap();
            self.finalizing_path = None;
            let disk_full = self
                .finalizing_disk_full
                .take()
                .is_some_and(|f| f.load(Ordering::Relaxed));
            match tokio::runtime::Handle::current().block_on(handle) {
                Ok(Ok(path)) => {
                    // Probed from the file itself, not trusted from the
                    // pre-recording device selection -- see field doc on
                    // export_available_tracks.
                    self.export_available_tracks = crate::encode::remux::count_audio_tracks(&path)
                        .inspect_err(|e| {
                            tracing::warn!(
                                "count_audio_tracks failed, export controls will stay hidden: {e}"
                            )
                        })
                        .unwrap_or(0);
                    tracing::info!(
                        "recording finalized: {} ({} audio track(s))",
                        path.display(),
                        self.export_available_tracks
                    );
                    self.export_track_selection = vec![true; self.export_available_tracks];
                    self.export_state = ExportState::Idle;
                    self.export_result_rx = None;
                    self.last_output_path = Some(path);
                    if disk_full {
                        self.error_message = Some(s.disk_full_mid_recording.to_string());
                    } else if let Some(msg) = audio_tracks_missing_message(
                        s,
                        self.export_available_tracks,
                        self.last_recording_audio_labels.len(),
                    ) {
                        self.error_message = Some(msg);
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
        if self.session.is_recording() {
            // 33 ms ≈ 30 fps; needed for smooth pulsing dot animation
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
        if matches!(self.export_state, ExportState::Running) {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if self.session.is_paused() {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        if self.finalizing_handle.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        // Without this, a Highlight save that finishes in the background
        // between frames never gets picked up by poll_highlight_save_result
        // until something else happens to wake the UI (e.g. another
        // keypress) -- looking like the save silently did nothing on its
        // first press.
        if matches!(self.highlight_save_state, HighlightSaveState::Saving) {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    pub(super) fn poll_highlight_save_result(&mut self) {
        if !self
            .highlight_save_handle
            .as_ref()
            .is_some_and(|h| h.is_finished())
        {
            return;
        }
        let handle = self.highlight_save_handle.take().unwrap();
        self.highlight_save_state = match tokio::runtime::Handle::current().block_on(handle) {
            Ok(Ok(path)) => HighlightSaveState::Done(path),
            Ok(Err(e)) => HighlightSaveState::Failed(e.to_string()),
            Err(e) => HighlightSaveState::Failed(e.to_string()),
        };
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

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
