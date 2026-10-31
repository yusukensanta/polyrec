pub mod clock;
mod pipeline;
pub mod state;

pub use pipeline::{CaptureInputs, FpsSupport, probe_fps_support};

use crate::capture::frame_rate::LiveFps;
use crate::config::{BitrateMode, Codec, EncoderMode, ResolutionMode};
use crate::encode::RecordingCommand;
use crate::encode::actor::spawn_recording_actor;
use crate::encode::highlight_export;
use crate::error::{AppError, ResultExt};
use crate::highlight::{
    HIGHLIGHT_SEGMENT_SECONDS, SaveNowRequest, SegmentInfo, spawn_highlight_actor,
};
use crate::recording_naming::{RecordingNaming, reserve_finished_recording_path};
use crate::types::{CaptureKind, CaptureSource, SessionState};
use pipeline::{CaptureDimensions, CapturePipeline};
use state::{SessionAction, transition};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Resolved encoder settings for one recording — built by the caller (the dashboard,
/// from `Config::encode`) and passed into `start_capture`.
#[derive(Debug, Clone)]
pub struct EncodeSettings {
    pub codec: Codec,
    pub fps: u32,
    pub resolution_mode: ResolutionMode,
    pub bitrate_mode: BitrateMode,
    pub encoder_mode: EncoderMode,
    /// See `EncodeConfig::gpu_frames`.
    pub gpu_frames: bool,
}

impl Default for EncodeSettings {
    fn default() -> Self {
        let d = crate::config::Config::default().encode;
        Self {
            codec: d.codec,
            fps: d.fps,
            resolution_mode: d.resolution_mode(),
            bitrate_mode: d.bitrate_mode(),
            encoder_mode: d.encoder_mode,
            gpu_frames: d.gpu_frames,
        }
    }
}

pub struct ActiveCapture {
    pipeline: CapturePipeline,
    recorder_handle: JoinHandle<Result<PathBuf, AppError>>,
    recording_tx: mpsc::Sender<RecordingCommand>,
    pub output_path: PathBuf,
    /// Folder `output_path`'s finished file will be renamed into (see
    /// `encode::actor::spawn_recording_actor`) -- along with `app_name` and
    /// `naming` below, lets the UI preview the eventual saved filename
    /// (`recording_naming::resolve_finished_recording_stem`) instead of
    /// showing `output_path`'s own name, which is only ever the in-progress
    /// `.tmp.mp4` file.
    pub output_dir: PathBuf,
    pub app_name: String,
    pub naming: RecordingNaming,
    /// Set by the recorder actor if it stopped itself early because free disk
    /// space dropped below `disk_space::MIN_FREE_BYTES` — the file up to that
    /// point is still finalized normally, this just tells the caller *why* the
    /// recording ended without the user pressing stop, so it can be surfaced.
    pub disk_full_flag: Arc<AtomicBool>,
    /// The window or monitor this recording is capturing -- used to position
    /// the overlay HUD on whichever monitor is relevant (see
    /// `render_overlay_viewport`), instead of assuming the primary display.
    pub hwnd: usize,
    /// Whether `hwnd` above is really an HWND or an HMONITOR -- see
    /// `CaptureKind`. `render_overlay_viewport` needs this to know whether it
    /// can call `MonitorFromWindow(hwnd)` (only valid for `Window`) or should
    /// use `hwnd` directly as the monitor (`Monitor`).
    pub kind: CaptureKind,
    /// The frame rate this recording actually runs at: the configured fps,
    /// lowered to what the display and then the encoder can do (see
    /// `effective_fps` and `LiveFps`). What the fps-shortfall check measures
    /// against.
    pub fps: LiveFps,
}

impl ActiveCapture {
    /// Recorded time so far, excluding paused spans.
    pub fn elapsed(&self) -> Duration {
        self.pipeline.ctrl.clock.elapsed()
    }

    /// Each audio track's live peak level, indexed by track id.
    pub fn audio_levels(&self) -> &[crate::types::AudioLevel] {
        self.pipeline.audio_levels()
    }

    /// True once the recorder actor has exited on its own (e.g. its
    /// disk-full check tripped) rather than via `SessionManager::stop_capture`.
    pub fn recorder_finished(&self) -> bool {
        self.recorder_handle.is_finished()
    }
}

/// The rolling "Highlight" background buffer -- entirely separate from
/// `ActiveCapture`, on its own capture threads, and NOT part of the
/// Idle/Recording/Paused state machine (`session::state`): it's a background
/// subsystem that runs whenever enabled and no manual recording is active,
/// not a session state of its own. See the Highlight buffer design plan for
/// the full lifecycle rules this implements.
pub struct ActiveHighlight {
    pipeline: CapturePipeline,
    highlight_handle: JoinHandle<Result<(), AppError>>,
    highlight_tx: mpsc::Sender<RecordingCommand>,
    save_now_tx: mpsc::UnboundedSender<SaveNowRequest>,
    segments: Arc<Mutex<VecDeque<SegmentInfo>>>,
    /// How much of the buffer `save_highlight` should try to save -- captured
    /// at start time since it's this buffering session's own setting, not
    /// necessarily whatever `config.highlight.buffer_seconds` reads right now.
    buffer_seconds: u32,
    /// Set if the highlight actor stopped itself early because free disk
    /// space on the segment directory's volume dropped too low -- see
    /// `SessionManager::highlight_stopped_for_disk_space`.
    disk_full_flag: Arc<AtomicBool>,
    /// The window this buffer is currently following -- the dashboard's
    /// foreground-window poll compares against this to detect a switch and
    /// restart buffering for the new window (see lifecycle rule 2).
    hwnd: usize,
}

/// Owns the manual recording (`active`) and the Highlight buffer
/// (`highlight`), plus the Idle/Recording/Paused state machine. Every state
/// transition happens inside the method that changes what's running --
/// `start_capture`, `pause_capture`, `resume_capture`, `stop_capture` -- so
/// the state can never disagree with whether a recording actually exists.
pub struct SessionManager {
    state: SessionState,
    active: Option<ActiveCapture>,
    highlight: Option<ActiveHighlight>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            state: SessionState::Idle,
            active: None,
            highlight: None,
        }
    }

    pub fn state(&self) -> &SessionState {
        &self.state
    }

    /// The manual recording in progress (recording or paused), if any.
    pub fn active(&self) -> Option<&ActiveCapture> {
        self.active.as_ref()
    }

    fn apply(&mut self, action: SessionAction) -> bool {
        match transition(&self.state, &action) {
            Some(next) => {
                self.state = next;
                true
            }
            None => false,
        }
    }

    /// Starts capture and recording actors and moves to `Recording` -- only
    /// once everything is spawned, so a refusal (e.g. disk full) leaves the
    /// session `Idle` instead of showing "Recording" for a capture that never
    /// started. `frame_count` is incremented by the video pump for each frame
    /// forwarded to the encoder.
    pub fn start_capture(
        &mut self,
        source: CaptureSource,
        inputs: CaptureInputs,
        show_border: bool,
        frame_count: Arc<AtomicU64>,
        output_dir: &Path,
        naming: RecordingNaming,
    ) -> Result<PathBuf, AppError> {
        let app_name = app_name_from_exe(&source.exe_name);
        let (output_path, polyrec_dir) = prepare_recording_paths(output_dir, &app_name);

        let free = crate::disk_space::free_bytes(&polyrec_dir)?;
        if free < crate::disk_space::MIN_FREE_BYTES {
            return Err(AppError::DiskFull(polyrec_dir));
        }

        let dims = CaptureDimensions::resolve(&source, &inputs.encode);
        let fps = dims.fps.clone();
        let disk_full_flag = Arc::new(AtomicBool::new(false));
        let (recording_tx, recorder_handle) = spawn_recording_actor(
            output_path.clone(),
            polyrec_dir.clone(),
            app_name.clone(),
            naming.clone(),
            dims.encoder_params(&inputs),
            Arc::clone(&disk_full_flag),
            fps.clone(),
        );
        let pipeline = CapturePipeline::spawn(
            &source,
            inputs,
            dims,
            &recording_tx,
            frame_count,
            show_border,
            "",
        );

        self.active = Some(ActiveCapture {
            pipeline,
            recorder_handle,
            recording_tx,
            output_path: output_path.clone(),
            output_dir: polyrec_dir,
            app_name,
            naming,
            disk_full_flag,
            hwnd: source.hwnd,
            kind: source.kind,
            fps,
        });
        self.apply(SessionAction::Start);

        Ok(output_path)
    }

    /// Stops all capture and recording actors and moves back to `Idle`. Sends Stop to the
    /// recorder so it finalizes. Returns a `JoinHandle` that finishes with the recorder, so
    /// the caller can wait for finalization to complete before treating the output file as
    /// ready (see recorder-finalize-race design spec).
    ///
    /// Returns at once: the stopping itself runs in the background. It used to run right
    /// here, on the dashboard's thread -- so when the recorder stalled (as it did on a
    /// still window, #204), waiting for it froze the whole app on Stop, and the save with
    /// it.
    pub fn stop_capture(&mut self) -> Option<JoinHandle<Result<PathBuf, AppError>>> {
        self.apply(SessionAction::Stop);
        let active = self.active.take()?;
        Some(tokio::spawn(async move {
            // The capture loops must have fully drained into the recorder
            // *before* `Stop` is sent below: `Stop` used to be sent
            // immediately, which could reach the recorder and trigger
            // `finalize()` before a still-in-flight frame had been forwarded
            // by its pump, silently dropping it from the output. Video
            // capture trails its last-captured frame by up to one iteration
            // by design (see the ping-pong buffering in `capture::video`), so
            // without this wait that final frame was frequently lost --
            // reproduced by a real short recording ending up with zero video
            // frames.
            active.pipeline.shutdown().await;
            let _ = active.recording_tx.send(RecordingCommand::Stop).await;
            match active.recorder_handle.await {
                Ok(result) => result,
                // Re-raised, so the caller sees a panicked recorder as one,
                // the same as before this ran in a task of its own.
                Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
                Err(e) => Err(AppError::Encode(format!("recorder task: {e}"))),
            }
        }))
    }

    pub fn is_recording(&self) -> bool {
        matches!(self.state, SessionState::Recording)
    }

    pub fn is_highlighting(&self) -> bool {
        self.highlight.is_some()
    }

    /// The window Highlight buffering is currently following, if active.
    pub fn highlight_hwnd(&self) -> Option<usize> {
        self.highlight.as_ref().map(|h| h.hwnd)
    }

    /// True once the highlight actor's background thread has exited on its
    /// own (e.g. the disk-full check inside it tripped) rather than via
    /// `stop_highlight_buffering` -- mirrors how `poll_background_work`
    /// detects manual recording stopping itself early via
    /// `active.recorder_handle.is_finished()`.
    pub fn highlight_actor_finished(&self) -> bool {
        self.highlight
            .as_ref()
            .is_some_and(|h| h.highlight_handle.is_finished())
    }

    /// Whether the Highlight actor stopped itself because free disk space
    /// on the segment directory's volume dropped too low.
    pub fn highlight_stopped_for_disk_space(&self) -> bool {
        self.highlight
            .as_ref()
            .is_some_and(|h| h.disk_full_flag.load(Ordering::Relaxed))
    }

    /// Starts (or restarts, for a different window) the Highlight background
    /// buffer against `source`, on a `CapturePipeline` of its own -- own
    /// capture threads, own clock, own (never-toggled) pause flag; nothing is
    /// shared with a manual recording's pipeline but the code that builds it
    /// (see `pipeline`'s module doc). Highlight buffering doesn't support
    /// pausing.
    pub fn start_highlight_buffering(
        &mut self,
        source: &CaptureSource,
        inputs: CaptureInputs,
        output_dir: &Path,
        buffer_seconds: u32,
    ) -> Result<(), AppError> {
        let segment_dir = highlight_segment_dir(output_dir);
        std::fs::create_dir_all(&segment_dir)?;
        // Each call starts a brand-new, empty tracking deque -- any files
        // left over from a *previous* buffering session (e.g. one that was
        // paused via `stop_highlight_buffering(false)` for a manual
        // recording, rather than stopped-and-discarded) would otherwise sit
        // in this directory forever: untracked by the new deque, so never
        // eligible for eviction or inclusion in a future save. Found via a
        // real pause/resume run, not by inspection -- clear the directory
        // instead of orphaning it.
        if let Ok(entries) = std::fs::read_dir(&segment_dir) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }

        let free = crate::disk_space::free_bytes(&segment_dir)?;
        if free < crate::disk_space::MIN_FREE_BYTES {
            return Err(AppError::DiskFull(segment_dir));
        }

        let dims = CaptureDimensions::resolve(source, &inputs.encode);
        let max_segments = buffer_seconds
            .max(1)
            .div_ceil(HIGHLIGHT_SEGMENT_SECONDS)
            .max(1) as usize;
        let segments: Arc<Mutex<VecDeque<SegmentInfo>>> = Arc::new(Mutex::new(VecDeque::new()));
        let disk_full_flag = Arc::new(AtomicBool::new(false));
        let (highlight_tx, save_now_tx, highlight_handle) = spawn_highlight_actor(
            segment_dir,
            dims.encoder_params(&inputs),
            HIGHLIGHT_SEGMENT_SECONDS,
            max_segments,
            Arc::clone(&segments),
            Arc::clone(&disk_full_flag),
            dims.fps.clone(),
        );
        let pipeline = CapturePipeline::spawn(
            source,
            inputs,
            dims,
            &highlight_tx,
            Arc::new(AtomicU64::new(0)),
            false,
            "Highlight ",
        );

        self.highlight = Some(ActiveHighlight {
            pipeline,
            highlight_handle,
            highlight_tx,
            save_now_tx,
            segments,
            buffer_seconds,
            disk_full_flag,
            hwnd: source.hwnd,
        });

        Ok(())
    }

    /// Stops Highlight buffering. `discard` deletes every segment file on
    /// disk (used when the setting is disabled, or the foreground window
    /// changed to one with a different resolution -- see lifecycle rule 2);
    /// pass `false` to leave files in place without waiting for the actor to
    /// exit (e.g. briefly pausing buffering so a manual recording can start,
    /// per lifecycle rule 3) -- purely a "don't bother deleting them right
    /// now" optimization, not a promise they'll be usable later:
    /// `start_highlight_buffering` clears any leftover files from a prior
    /// session as soon as it (re)starts, so buffering effectively begins
    /// fresh each time, whether resuming after a pause or following a
    /// different window.
    ///
    /// `discard: false` doesn't wait for the background actor thread to
    /// exit -- its segments are already durable files on disk, unlike manual
    /// recording's finalize (whose output IS the point of waiting).
    /// `discard: true` DOES wait (bounded by one segment's finalize time,
    /// well under a second in practice) -- otherwise the actor can still be
    /// mid-finalize (or about to push one more segment onto the shared
    /// deque) when `discard_segments` reads it, deleting an incomplete
    /// snapshot while the actor goes on to write one more file to a
    /// directory the caller believes is now empty (caught via a real
    /// end-to-end run, not just the isolated unit tests).
    ///
    /// Like `stop_capture`, returns at once and stops in the background; the
    /// returned handle finishes once that's done (segments discarded too,
    /// with `discard`).
    pub fn stop_highlight_buffering(&mut self, discard: bool) -> Option<JoinHandle<()>> {
        let active = self.highlight.take()?;
        Some(tokio::spawn(async move {
            // See the matching comment in `stop_capture` -- waited on (not
            // aborted) so the capture loops' final in-flight frames reach the
            // highlight actor before `Stop` does, instead of being silently
            // dropped by a `Stop` that got there first.
            active.pipeline.shutdown().await;
            let _ = active.highlight_tx.send(RecordingCommand::Stop).await;
            if discard {
                let _ = active.highlight_handle.await;
                crate::highlight::discard_segments(&active.segments);
            }
        }))
    }

    /// Forces the currently-open segment to finalize immediately (so the
    /// save includes everything up to this exact moment, see
    /// `highlight::SaveNowRequest`), then concatenates+trims the buffer down
    /// to `buffer_seconds` into a new file under `output_dir`. Errors if
    /// Highlight buffering isn't currently active.
    pub fn save_highlight(
        &self,
        output_dir: &Path,
        app_name: &str,
        naming: RecordingNaming,
    ) -> Result<JoinHandle<Result<PathBuf, AppError>>, AppError> {
        let active = self
            .highlight
            .as_ref()
            .ok_or_else(|| AppError::Encode("Highlight buffering is not active".into()))?;
        let save_now_tx = active.save_now_tx.clone();
        let segments_arc = Arc::clone(&active.segments);
        let buffer_seconds = active.buffer_seconds;
        let output_dir = output_dir.to_path_buf();
        let app_name = app_name.to_string();

        let handle = tokio::spawn(async move {
            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            save_now_tx
                .send(reply_tx)
                .map_err(|_| AppError::Encode("highlight actor is not running".into()))?;
            reply_rx.await.map_err(|_| {
                AppError::Encode("highlight actor stopped before confirming save".into())
            })?;

            let snapshot: Vec<SegmentInfo> = {
                // Recover rather than propagate a second panic -- poisoning only
                // means some *other* holder of this lock already panicked, and
                // every operation under this lock (here and in highlight/mod.rs)
                // is infallible Vec/Deque bookkeeping, so the data itself can't
                // be left in a torn state.
                let guard = segments_arc
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                guard.iter().cloned().collect()
            };

            tokio::task::spawn_blocking(move || {
                let saved_dir = highlight_saved_dir(&output_dir);
                std::fs::create_dir_all(&saved_dir)?;
                let output_path = reserve_finished_recording_path(&naming, &app_name, &saved_dir)?;
                // Writes over the empty file that reserved the name.
                highlight_export::concat_and_trim(&snapshot, buffer_seconds, &output_path)
                    .inspect_err(|_| {
                        let _ = std::fs::remove_file(&output_path);
                    })
            })
            .await
            .encode_ctx("save_highlight join error")?
        });

        Ok(handle)
    }

    pub fn pause_capture(&mut self) {
        if let Some(active) = &self.active {
            active.pipeline.ctrl.pause();
        }
        self.apply(SessionAction::Pause);
    }

    pub fn resume_capture(&mut self) {
        if let Some(active) = &self.active {
            active.pipeline.ctrl.resume();
        }
        self.apply(SessionAction::Resume);
    }

    pub fn is_paused(&self) -> bool {
        matches!(self.state, SessionState::Paused)
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Derives a filesystem-safe app name from a window's exe name (e.g. "vivaldi.exe"
/// -> "vivaldi"), used as the recording filename's prefix. Falls back to "recording"
/// if the exe name is empty or sanitizes away to nothing. `pub(crate)` so the
/// dashboard can reuse the exact same sanitization for Highlight save filenames.
pub(crate) fn app_name_from_exe(exe_name: &str) -> String {
    crate::recording_naming::sanitize_filename_component(crate::sources::strip_exe_suffix(exe_name))
}

/// Directory the Highlight buffer's rotating segment files live in -- kept
/// separate from the main `polyrec/` recording directory so it's trivially
/// distinguishable (and safe to bulk-delete via `discard_segments`) from the
/// user's actual finished recordings.
fn highlight_segment_dir(output_dir: &Path) -> PathBuf {
    output_dir.join("polyrec").join("_highlight_buffer")
}

/// Directory a `save_highlight` result is written to -- a sibling of the
/// main `polyrec/` recording directory (and of `highlight_segment_dir`'s
/// internal segment-rotation folder), under the same drive/directory the
/// user has configured for recordings, so saved highlights are easy to find
/// alongside normal recordings without being mixed into either.
fn highlight_saved_dir(output_dir: &Path) -> PathBuf {
    output_dir.join("polyrec").join("highlights")
}

/// Creates `<base_dir>/polyrec/` and returns (temp_recording_path, polyrec_dir).
/// The temp path is what the encoder actually writes to — its name doesn't matter
/// beyond being unique, since `spawn_recording_actor` renames it to
/// `<app_name>_<finish timestamp>.mp4` once the recording finishes.
fn prepare_recording_paths(base_dir: &std::path::Path, app_name: &str) -> (PathBuf, PathBuf) {
    let polyrec_dir = base_dir.join("polyrec");
    if let Err(e) = std::fs::create_dir_all(&polyrec_dir) {
        tracing::warn!(
            "Failed to create output directory {}: {e}",
            polyrec_dir.display()
        );
    }
    let start_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let temp_path = polyrec_dir.join(format!("{app_name}_recording_{start_ts}.tmp.mp4"));
    (temp_path, polyrec_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AppAudioSource, AudioDevice};

    /// Every device/app at unity gain with the default encode settings --
    /// what the e2e tests below record with.
    /// A manual recording whose capture loop and recorder never finish --
    /// the shape of every recorder stall so far, like the still-window one
    /// fixed in #204.
    fn stuck_recording() -> ActiveCapture {
        let (recording_tx, _recording_rx) = mpsc::channel(1);
        ActiveCapture {
            pipeline: CapturePipeline::stuck(),
            recorder_handle: tokio::spawn(std::future::pending()),
            recording_tx,
            output_path: PathBuf::from("stuck.tmp.mp4"),
            output_dir: PathBuf::new(),
            app_name: String::new(),
            naming: RecordingNaming::ProcessName,
            disk_full_flag: Arc::new(AtomicBool::new(false)),
            hwnd: 0,
            kind: CaptureKind::Window,
            fps: LiveFps::new(60),
        }
    }

    /// Stop must hand back at once even when the recorder is stuck: it runs
    /// on the dashboard's thread, which used to wait for the capture loops
    /// and the recorder there -- so a stalled recorder froze the whole app
    /// on Stop, and the save with it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_returns_at_once_even_with_a_stuck_recorder() {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        tokio::task::spawn_blocking(move || {
            let mut sm = SessionManager::new();
            sm.active = Some(stuck_recording());
            sm.state = SessionState::Recording;
            let handle = sm.stop_capture();
            let _ = done_tx.send((handle.is_some(), sm.state().clone()));
            // Kept alive so the test checks the stop itself, not cleanup.
            std::mem::forget(handle);
        });
        let (got_handle, state) = done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("stop_capture blocked on the stuck recorder");
        assert!(got_handle);
        assert_eq!(state, SessionState::Idle);
    }

    fn inputs(
        devices: Vec<AudioDevice>,
        app_sources: Vec<AppAudioSource>,
        app_audio_only: bool,
    ) -> CaptureInputs {
        CaptureInputs {
            devices: devices.into_iter().map(|d| (d, 1.0)).collect(),
            app_sources: app_sources.into_iter().map(|s| (s, 1.0)).collect(),
            app_audio_only,
            encode: EncodeSettings::default(),
        }
    }

    /// `enumerate_sources()` can return transient shell popups (e.g. volume/brightness
    /// OSD "PopupHost" windows) with a degenerate client rect that breaks H264 setup —
    /// a separate, pre-existing issue unrelated to whatever this test is actually
    /// exercising. Skip past those so tests aren't flaky based on unrelated desktop state.
    fn pick_source_with_real_client_rect(sources: Vec<CaptureSource>) -> Option<CaptureSource> {
        sources.into_iter().find(|s| {
            // Monitor entries store an HMONITOR in `hwnd`, not a real window
            // handle -- GetClientRect would just fail on one anyway, but
            // filtering explicitly is clearer than relying on that.
            s.kind == CaptureKind::Window
                && unsafe {
                    let hwnd = windows::Win32::Foundation::HWND(s.hwnd as *mut core::ffi::c_void);
                    let mut rect = windows::Win32::Foundation::RECT::default();
                    windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rect).is_ok()
                        && (rect.right - rect.left) >= 100
                        && (rect.bottom - rect.top) >= 100
                }
        })
    }

    #[test]
    fn new_session_is_idle() {
        let sm = SessionManager::new();
        assert!(matches!(sm.state(), SessionState::Idle));
    }

    #[test]
    fn start_transitions_to_recording() {
        let mut sm = SessionManager::new();
        assert!(sm.apply(SessionAction::Start));
        assert!(sm.is_recording());
    }

    #[test]
    fn illegal_action_returns_false() {
        let mut sm = SessionManager::new();
        assert!(!sm.apply(SessionAction::Stop));
        assert!(matches!(sm.state(), SessionState::Idle));
    }

    #[test]
    fn stop_capture_without_a_recording_stays_idle_and_returns_none() {
        let mut sm = SessionManager::new();
        assert!(sm.stop_capture().is_none());
        assert!(matches!(sm.state(), SessionState::Idle));
        assert!(sm.active().is_none());
    }

    #[test]
    fn app_name_from_exe_strips_exe_suffix() {
        assert_eq!(app_name_from_exe("vivaldi.exe"), "vivaldi");
        assert_eq!(app_name_from_exe("Notepad.EXE"), "Notepad");
    }

    #[test]
    fn app_name_from_exe_sanitizes_invalid_filename_chars() {
        assert_eq!(app_name_from_exe("weird name!.exe"), "weird_name_");
    }

    #[test]
    fn app_name_from_exe_empty_falls_back_to_recording() {
        assert_eq!(app_name_from_exe(""), "recording");
        assert_eq!(app_name_from_exe(".exe"), "recording");
    }

    #[test]
    fn prepare_recording_paths_creates_polyrec_subdir_with_temp_mp4() {
        let dir = tempfile::tempdir().unwrap();
        let (temp_path, polyrec_dir) = prepare_recording_paths(dir.path(), "vivaldi");
        assert_eq!(polyrec_dir, dir.path().join("polyrec"));
        assert!(
            polyrec_dir.is_dir(),
            "polyrec subdirectory should be created"
        );
        assert_eq!(temp_path.parent(), Some(polyrec_dir.as_path()));
        assert_eq!(temp_path.extension().and_then(|e| e.to_str()), Some("mp4"));
        assert!(
            temp_path
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("vivaldi_")
        );
    }

    #[test]
    fn encode_settings_default_matches_config_default() {
        let settings = EncodeSettings::default();
        let config_default = crate::config::Config::default().encode;
        assert_eq!(settings.fps, config_default.fps);
        assert_eq!(settings.codec, config_default.codec);
        assert!(matches!(
            settings.resolution_mode,
            crate::config::ResolutionMode::Native
        ));
        assert!(matches!(
            settings.bitrate_mode,
            crate::config::BitrateMode::Auto
        ));
    }

    /// Verifies the recorded video's actual encoded resolution matches the captured
    /// window's own native size (not upscaled/downscaled to the display's resolution —
    /// that upscaling was reverted because nearest-neighbor scaling to a much larger,
    /// non-integer-ratio display size produced visible blocky/grainy artifacts).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn recording_resolution_matches_window() {
        use crate::capture::audio::enumerate_audio_devices;
        use crate::capture::video::query_capture_size;
        use crate::sources::enumerate_sources;
        use windows::Win32::Media::MediaFoundation::{
            MF_MT_FRAME_SIZE, MFCreateSourceReaderFromURL,
        };

        let sources = enumerate_sources();
        let source =
            pick_source_with_real_client_rect(sources).expect("no usable capture source found");
        let hwnd = windows::Win32::Foundation::HWND(source.hwnd as *mut core::ffi::c_void);
        let (expected_w, expected_h) =
            query_capture_size(crate::capture::video::CaptureTarget::Window(hwnd))
                .expect("query_capture_size failed");

        let audio_devices = enumerate_audio_devices().unwrap_or_default();
        let dir = tempfile::tempdir().unwrap();
        let mut sm = SessionManager::new();
        let frame_count = Arc::new(AtomicU64::new(0));
        sm.start_capture(
            source,
            inputs(audio_devices, vec![], false),
            true,
            Arc::clone(&frame_count),
            dir.path(),
            RecordingNaming::ProcessName,
        )
        .expect("start_capture failed");
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let handle =
            tokio::task::block_in_place(|| sm.stop_capture()).expect("stop_capture returned None");
        let finalized_path = handle
            .await
            .expect("recorder task panicked")
            .expect("finalize failed");

        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_MULTITHREADED,
            );
            let _ = windows::Win32::Media::MediaFoundation::MFStartup(
                windows::Win32::Media::MediaFoundation::MF_VERSION,
                windows::Win32::Media::MediaFoundation::MFSTARTUP_FULL,
            );
            use windows::Win32::Media::MediaFoundation::{MF_MT_MAJOR_TYPE, MFMediaType_Video};
            let url = windows::core::HSTRING::from(finalized_path.to_str().unwrap());
            let reader: windows::Win32::Media::MediaFoundation::IMFSourceReader =
                MFCreateSourceReaderFromURL(&url, None).expect("MFCreateSourceReaderFromURL");
            // Container stream order isn't guaranteed — find the video stream by type.
            let mut video_type = None;
            for i in 0..4u32 {
                if let Ok(t) = reader.GetNativeMediaType(i, 0)
                    && t.GetGUID(&MF_MT_MAJOR_TYPE).unwrap_or_default() == MFMediaType_Video
                {
                    video_type = Some(t);
                    break;
                }
            }
            let video_type = video_type.expect("no video stream found");
            let packed = video_type
                .GetUINT64(&MF_MT_FRAME_SIZE)
                .expect("GetUINT64 frame_size");
            let actual_w = (packed >> 32) as u32;
            let actual_h = (packed & 0xFFFF_FFFF) as u32;
            println!("window: {expected_w}x{expected_h}, encoded: {actual_w}x{actual_h}");
            assert_eq!(actual_w, expected_w.max(2) & !1);
            assert_eq!(actual_h, expected_h.max(2) & !1);
        }
    }

    /// End-to-end regression for the stall a quiet loopback track caused:
    /// with nothing playing, desktop loopback delivers no packets. The MP4
    /// sink then held every other stream's data in memory (the file stayed
    /// at its 79-byte header), throttled each write to about a second, and
    /// stopping took minutes. Records a display with every audio device
    /// (run it with nothing playing to reproduce the original case) and
    /// checks the file grows while recording and stopping finishes promptly.
    /// Records real devices but plays nothing; ignored by default.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn quiet_loopback_track_does_not_stall_the_recording() {
        use crate::capture::audio::enumerate_audio_devices;
        use crate::sources::enumerate_sources;

        let source = enumerate_sources()
            .into_iter()
            .find(|s| s.kind == CaptureKind::Monitor)
            .expect("no display source");
        let devices = enumerate_audio_devices().unwrap_or_default();
        assert!(
            devices.iter().any(|d| d.is_loopback),
            "needs a loopback device"
        );
        let dir = tempfile::tempdir().unwrap();
        let mut sm = SessionManager::new();
        let temp_path = sm
            .start_capture(
                source,
                inputs(devices, vec![], false),
                false,
                Arc::new(AtomicU64::new(0)),
                dir.path(),
                RecordingNaming::ProcessName,
            )
            .expect("start_capture failed");

        tokio::time::sleep(Duration::from_secs(6)).await;
        let size_while_recording = std::fs::metadata(&temp_path).map_or(0, |m| m.len());
        let handle = tokio::task::block_in_place(|| sm.stop_capture()).expect("stop_capture");
        let stop_started = std::time::Instant::now();
        let finalized = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("finalizing took over 5 s -- the recording stalled")
            .expect("recorder task panicked")
            .expect("finalize failed");
        println!(
            "while recording: {size_while_recording} bytes; stopped in {:?}; final {} bytes",
            stop_started.elapsed(),
            std::fs::metadata(&finalized).unwrap().len()
        );
        assert!(
            size_while_recording > 1024,
            "nothing reached the file while recording ({size_while_recording} bytes)"
        );
    }

    /// End-to-end: real window capture + real audio devices, through the same
    /// start_capture/stop_capture path the GUI uses. Needs a display and audio
    /// endpoints, so it's ignored by default — run with `--ignored --nocapture`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn full_capture_produces_nonempty_file() {
        use crate::capture::audio::enumerate_audio_devices;
        use crate::sources::enumerate_sources;

        let sources = enumerate_sources();
        let source =
            pick_source_with_real_client_rect(sources).expect("no usable capture source found");
        let audio_devices = enumerate_audio_devices().unwrap_or_default();

        let dir = tempfile::tempdir().unwrap();
        let mut sm = SessionManager::new();
        let frame_count = Arc::new(AtomicU64::new(0));
        let temp_path = sm
            .start_capture(
                source,
                inputs(audio_devices, vec![], false),
                true,
                Arc::clone(&frame_count),
                dir.path(),
                RecordingNaming::ProcessName,
            )
            .expect("start_capture failed");
        assert!(sm.is_recording(), "start_capture should move to Recording");

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let handle = tokio::task::block_in_place(|| sm.stop_capture())
            .expect("stop_capture returned None while active");
        assert!(matches!(sm.state(), SessionState::Idle));
        let result = handle.await.expect("recorder task panicked/aborted");
        let finalized_path = result.expect("finalize() returned an error");

        // The temp path start_capture returned gets renamed to its finish-timestamped
        // final name inside polyrec/ once the recording completes — they're not equal.
        assert_ne!(finalized_path, temp_path);
        assert!(
            !temp_path.exists(),
            "temp recording file should have been renamed away"
        );
        assert_eq!(
            finalized_path.parent(),
            Some(dir.path().join("polyrec")).as_deref()
        );
        let metadata = std::fs::metadata(&finalized_path).expect("output file missing");
        assert!(
            metadata.len() > 0,
            "output file is empty: {}",
            finalized_path.display()
        );
        println!("frames captured: {}", frame_count.load(Ordering::Relaxed));
        println!("output size: {} bytes", metadata.len());

        // Confirm the remux stream-type discovery fix: exporting with all audio
        // tracks selected must still succeed and produce a valid, non-empty file
        // regardless of physical stream order in the container.
        let export_out = dir.path().join("export_check.mp4");
        let export_result = crate::encode::remux::remux(&finalized_path, &export_out, &[0, 1])
            .expect("remux with both audio tracks failed");
        let export_meta = std::fs::metadata(&export_result).expect("remuxed export file missing");
        println!("export size: {} bytes", export_meta.len());
        assert!(export_meta.len() > 0, "exported file is empty");
    }

    /// End-to-end: two real recordings in a row with `RecordingNaming::CustomPrefix`
    /// through the same start_capture/stop_capture path the GUI uses, proving the
    /// sequence number is resolved from what's actually on disk (not an in-memory
    /// counter) across separate recordings -- needs a display, so it's ignored by
    /// default -- run with `--ignored --nocapture`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn full_capture_with_custom_prefix_produces_sequential_files() {
        use crate::sources::enumerate_sources;

        let sources = enumerate_sources();
        let source =
            pick_source_with_real_client_rect(sources).expect("no usable capture source found");

        let dir = tempfile::tempdir().unwrap();
        let mut sm = SessionManager::new();
        let frame_count = Arc::new(AtomicU64::new(0));
        let naming = RecordingNaming::CustomPrefix("e2e_stream".into());

        let temp_path = sm
            .start_capture(
                source.clone(),
                inputs(vec![], vec![], false),
                true,
                Arc::clone(&frame_count),
                dir.path(),
                naming.clone(),
            )
            .expect("start_capture failed (first recording)");
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let handle = tokio::task::block_in_place(|| sm.stop_capture())
            .expect("stop_capture returned None while active");
        let first_path = handle
            .await
            .expect("recorder task panicked/aborted")
            .expect("finalize() returned an error");
        assert_ne!(first_path, temp_path);
        assert_eq!(
            first_path.file_name().and_then(|n| n.to_str()),
            Some("e2e_stream_001.mp4"),
            "first custom-prefix recording should start the sequence at 001"
        );
        assert!(
            std::fs::metadata(&first_path)
                .expect("first output missing")
                .len()
                > 0
        );

        let frame_count2 = Arc::new(AtomicU64::new(0));
        let temp_path2 = sm
            .start_capture(
                source,
                inputs(vec![], vec![], false),
                true,
                Arc::clone(&frame_count2),
                dir.path(),
                naming,
            )
            .expect("start_capture failed (second recording)");
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let handle2 = tokio::task::block_in_place(|| sm.stop_capture())
            .expect("stop_capture returned None while active");
        let second_path = handle2
            .await
            .expect("recorder task panicked/aborted")
            .expect("finalize() returned an error");
        assert_ne!(second_path, temp_path2);
        assert_eq!(
            second_path.file_name().and_then(|n| n.to_str()),
            Some("e2e_stream_002.mp4"),
            "second custom-prefix recording in the same dir should continue the sequence at 002, scanned from disk"
        );
        assert!(
            std::fs::metadata(&second_path)
                .expect("second output missing")
                .len()
                > 0
        );
    }

    /// DIAGNOSTIC (temporary): records real window + default audio devices while a
    /// system WAV plays, then decodes the finalized MP4's audio track back to PCM via
    /// Media Foundation and reports the peak sample magnitude actually stored in the
    /// file. Isolates whether silence is introduced downstream of audio.rs (which
    /// diag_default_loopback_captures_real_signal already proved captures real signal).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn diag_recorded_audio_track_has_signal() {
        use crate::capture::audio::enumerate_audio_devices;
        use crate::sources::enumerate_sources;
        use windows::Win32::Media::MediaFoundation::{
            MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SOURCE_READER_FIRST_AUDIO_STREAM,
            MFAudioFormat_PCM, MFCreateMediaType, MFCreateSourceReaderFromURL, MFMediaType_Audio,
        };

        let sources = enumerate_sources();
        let source =
            pick_source_with_real_client_rect(sources).expect("no usable capture source found");
        let audio_devices: Vec<_> = enumerate_audio_devices()
            .unwrap_or_default()
            .into_iter()
            .filter(|d| d.is_loopback)
            .collect();
        assert!(!audio_devices.is_empty(), "need a loopback audio device");

        let dir = tempfile::tempdir().unwrap();
        let mut sm = SessionManager::new();
        let frame_count = Arc::new(AtomicU64::new(0));
        sm.start_capture(
            source,
            inputs(audio_devices, vec![], false),
            true,
            Arc::clone(&frame_count),
            dir.path(),
            RecordingNaming::ProcessName,
        )
        .expect("start_capture failed");

        // Deliberately fire-and-forget: PlaySync() blocks *inside* the spawned
        // process until the sound finishes, but we want it playing concurrently
        // with the recording below, not blocking this thread until done.
        #[allow(clippy::zombie_processes)]
        std::process::Command::new("powershell")
            .args([
                "-c",
                "(New-Object Media.SoundPlayer 'C:\\Windows\\Media\\Alarm01.wav').PlaySync()",
            ])
            .spawn()
            .expect("failed to spawn powershell sound player");

        tokio::time::sleep(std::time::Duration::from_secs(4)).await;

        let handle = tokio::task::block_in_place(|| sm.stop_capture())
            .expect("stop_capture returned None while active");
        let result = handle.await.expect("recorder task panicked/aborted");
        let finalized_path = result.expect("finalize() returned an error");
        println!("output: {}", finalized_path.display());

        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_MULTITHREADED,
            );
            let _ = windows::Win32::Media::MediaFoundation::MFStartup(
                windows::Win32::Media::MediaFoundation::MF_VERSION,
                windows::Win32::Media::MediaFoundation::MFSTARTUP_FULL,
            );
            let url = windows::core::HSTRING::from(finalized_path.to_str().unwrap());
            let reader =
                MFCreateSourceReaderFromURL(&url, None).expect("MFCreateSourceReaderFromURL");

            // Force decode to PCM so we can inspect raw samples.
            let pcm_type = MFCreateMediaType().unwrap();
            pcm_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
                .unwrap();
            pcm_type
                .SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)
                .unwrap();
            reader
                .SetCurrentMediaType(
                    MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32,
                    None,
                    &pcm_type,
                )
                .expect("SetCurrentMediaType PCM failed — no audio stream in file?");

            let mut peak = 0i32;
            let mut total_bytes = 0usize;
            loop {
                let mut stream_index = 0u32;
                let mut flags = 0u32;
                let mut timestamp = 0i64;
                let mut sample = None;
                reader
                    .ReadSample(
                        MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32,
                        0,
                        Some(&mut stream_index),
                        Some(&mut flags),
                        Some(&mut timestamp),
                        Some(&mut sample),
                    )
                    .expect("ReadSample failed");

                const MF_SOURCE_READERF_ENDOFSTREAM: u32 = 0x2;
                if flags & MF_SOURCE_READERF_ENDOFSTREAM != 0 {
                    break;
                }
                let Some(sample) = sample else { continue };
                let buffer = sample
                    .ConvertToContiguousBuffer()
                    .expect("ConvertToContiguousBuffer");
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut len = 0u32;
                buffer.Lock(&mut data, None, Some(&mut len)).expect("Lock");
                let bytes = std::slice::from_raw_parts(data, len as usize);
                for chunk in bytes.as_chunks::<2>().0 {
                    let v = i16::from_le_bytes(*chunk) as i32;
                    if v.abs() > peak {
                        peak = v.abs();
                    }
                }
                total_bytes += bytes.len();
                buffer.Unlock().expect("Unlock");
            }

            println!("DIAG: total_audio_bytes={total_bytes} peak_pcm16={peak}");
            assert!(total_bytes > 0, "audio track had zero bytes decoded");
            assert!(
                peak > 300,
                "peak PCM16 sample {peak} looks like silence in the recorded file"
            );
        }
    }

    /// Same as `full_capture_produces_nonempty_file` but with `app_audio_only = true`,
    /// exercising the process-loopback path through the real start_capture/stop_capture
    /// wiring (not just the raw capture function). Uses our own process id as the
    /// loopback target so a device is guaranteed to exist regardless of which visible
    /// window gets picked for video.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn full_capture_app_audio_only_produces_nonempty_file() {
        use crate::capture::audio::enumerate_audio_devices;
        use crate::sources::enumerate_sources;

        let sources = enumerate_sources();
        let mut source =
            pick_source_with_real_client_rect(sources).expect("no usable capture source found");
        source.process_id = std::process::id();
        let audio_devices = enumerate_audio_devices().unwrap_or_default();
        assert!(
            audio_devices.iter().any(|d| d.is_loopback),
            "need a loopback device to exercise app_audio_only"
        );

        let dir = tempfile::tempdir().unwrap();
        let mut sm = SessionManager::new();
        let frame_count = Arc::new(AtomicU64::new(0));
        let temp_path = sm
            .start_capture(
                source,
                inputs(audio_devices, vec![], true),
                true,
                Arc::clone(&frame_count),
                dir.path(),
                RecordingNaming::ProcessName,
            )
            .expect("start_capture failed");

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let handle = tokio::task::block_in_place(|| sm.stop_capture())
            .expect("stop_capture returned None while active");
        let result = handle.await.expect("recorder task panicked/aborted");
        let finalized_path = result.expect("finalize() returned an error");

        assert_ne!(finalized_path, temp_path);
        assert_eq!(
            finalized_path.parent(),
            Some(dir.path().join("polyrec")).as_deref()
        );
        let metadata = std::fs::metadata(&finalized_path).expect("output file missing");
        assert!(
            metadata.len() > 0,
            "output file is empty: {}",
            finalized_path.display()
        );
        println!("output size: {} bytes", metadata.len());
    }

    /// DIAGNOSTIC (temporary): reproduces the reported "Saving recording..."
    /// hang -- 2 real devices + 1 app-audio source (3 audio tracks total),
    /// stopped almost immediately (well inside process-loopback's ~5s async
    /// activation window, so the app-audio track likely never receives a
    /// single sample). Times `stop_capture()` + finalize under an overall
    /// `tokio::time::timeout` so a genuine hang fails the test instead of
    /// wedging the run forever.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn diag_stop_soon_after_start_with_three_audio_tracks_does_not_hang() {
        use crate::capture::audio::enumerate_audio_devices;
        use crate::sources::enumerate_sources;
        use crate::types::AppAudioSource;

        let sources = enumerate_sources();
        let source =
            pick_source_with_real_client_rect(sources).expect("no usable capture source found");
        let audio_devices = enumerate_audio_devices().unwrap_or_default();
        println!("real audio devices: {}", audio_devices.len());

        // Our own test process: guaranteed running, guaranteed to emit zero
        // audio -- the app-audio slot that never gets a first buffer.
        let app_audio_sources = vec![AppAudioSource {
            process_ids: vec![std::process::id()],
            exe_name: "diag-self.exe".to_string(),
            display_name: "diag-self".to_string(),
            icon_rgba: None,
        }];

        let dir = tempfile::tempdir().unwrap();
        let mut sm = SessionManager::new();
        let frame_count = Arc::new(AtomicU64::new(0));
        sm.start_capture(
            source,
            inputs(audio_devices, app_audio_sources, false),
            true,
            Arc::clone(&frame_count),
            dir.path(),
            RecordingNaming::ProcessName,
        )
        .expect("start_capture failed");

        // Stop almost immediately -- well inside the ~5s process-loopback
        // activation window.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let stop_started = std::time::Instant::now();
        let handle = tokio::task::block_in_place(|| sm.stop_capture())
            .expect("stop_capture returned None while active");

        let result = tokio::time::timeout(std::time::Duration::from_secs(30), handle)
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "stop+finalize did not complete within 30s (elapsed before timeout: {:.1}s) -- reproduces the reported hang",
                    stop_started.elapsed().as_secs_f32()
                )
            })
            .expect("recorder task panicked/aborted");
        let finalized_path = result.expect("finalize() returned an error");

        println!(
            "stop+finalize took {:.2}s",
            stop_started.elapsed().as_secs_f32()
        );
        let metadata = std::fs::metadata(&finalized_path).expect("output file missing");
        println!("output size: {} bytes", metadata.len());
    }

    /// DIAGNOSTIC (temporary): same quick-stop shape, but the app-audio slot
    /// is one `AppAudioSource` with TWO process_ids (mimics a real
    /// multi-process app like Discord, where audio comes from more than one
    /// top-level process) -- 2 devices + 2 app-audio pids = 4 real audio
    /// tracks, stopped almost immediately.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn diag_stop_soon_after_start_with_four_tracks_multi_pid_app_does_not_hang() {
        use crate::capture::audio::enumerate_audio_devices;
        use crate::sources::enumerate_sources;
        use crate::types::AppAudioSource;

        let sources = enumerate_sources();
        let source =
            pick_source_with_real_client_rect(sources).expect("no usable capture source found");
        let audio_devices = enumerate_audio_devices().unwrap_or_default();
        println!("real audio devices: {}", audio_devices.len());

        #[allow(clippy::zombie_processes)]
        let mut player = std::process::Command::new("powershell")
            .args([
                "-c",
                "for ($i=0; $i -lt 20; $i++) { (New-Object Media.SoundPlayer 'C:\\Windows\\Media\\Alarm01.wav').PlaySync() }",
            ])
            .spawn()
            .expect("failed to spawn powershell sound player");
        let player_pid = player.id();
        println!(
            "sound player pid: {player_pid}, silent pid: {}",
            std::process::id()
        );

        // One AppAudioSource, two process_ids: one real active player, one
        // guaranteed-silent (our own process) -- mirrors a multi-process app
        // where only one of its processes is actually the one making sound.
        let app_audio_sources = vec![AppAudioSource {
            process_ids: vec![player_pid, std::process::id()],
            exe_name: "diag-multi.exe".to_string(),
            display_name: "diag-multi".to_string(),
            icon_rgba: None,
        }];

        let dir = tempfile::tempdir().unwrap();
        let mut sm = SessionManager::new();
        let frame_count = Arc::new(AtomicU64::new(0));
        sm.start_capture(
            source,
            inputs(audio_devices, app_audio_sources, false),
            true,
            Arc::clone(&frame_count),
            dir.path(),
            RecordingNaming::ProcessName,
        )
        .expect("start_capture failed");

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let stop_started = std::time::Instant::now();
        let handle = tokio::task::block_in_place(|| sm.stop_capture())
            .expect("stop_capture returned None while active");

        let result = tokio::time::timeout(std::time::Duration::from_secs(30), handle).await;
        let _ = player.kill();
        let result = result
            .unwrap_or_else(|_| {
                panic!(
                    "stop+finalize did not complete within 30s (elapsed before timeout: {:.1}s) -- reproduces the reported hang",
                    stop_started.elapsed().as_secs_f32()
                )
            })
            .expect("recorder task panicked/aborted");
        let finalized_path = result.expect("finalize() returned an error");

        println!(
            "stop+finalize took {:.2}s",
            stop_started.elapsed().as_secs_f32()
        );
        let metadata = std::fs::metadata(&finalized_path).expect("output file missing");
        println!("output size: {} bytes", metadata.len());
    }

    /// DIAGNOSTIC (temporary): same shape as the "quick stop" repro above,
    /// but the app-audio track is a real, continuously sound-producing
    /// process (a looped WAV via a spawned powershell) for a longer (~10s)
    /// recording -- covers the case the quick-stop version can't: 3
    /// genuinely active concurrent audio encodes (2 devices + 1 app) plus
    /// hardware video, long enough for a real recording_tx backlog to build
    /// if encoding can't keep up with real time on this machine.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn diag_stop_after_longer_recording_with_three_active_audio_tracks() {
        use crate::capture::audio::enumerate_audio_devices;
        use crate::sources::enumerate_sources;
        use crate::types::AppAudioSource;

        let sources = enumerate_sources();
        let source =
            pick_source_with_real_client_rect(sources).expect("no usable capture source found");
        let audio_devices = enumerate_audio_devices().unwrap_or_default();
        println!("real audio devices: {}", audio_devices.len());

        // Loops a short WAV for the whole test so the app-audio track has
        // continuous real signal, not a one-shot blip.
        #[allow(clippy::zombie_processes)]
        let mut player = std::process::Command::new("powershell")
            .args([
                "-c",
                "for ($i=0; $i -lt 20; $i++) { (New-Object Media.SoundPlayer 'C:\\Windows\\Media\\Alarm01.wav').PlaySync() }",
            ])
            .spawn()
            .expect("failed to spawn powershell sound player");
        let player_pid = player.id();
        println!("sound player pid: {player_pid}");

        let app_audio_sources = vec![AppAudioSource {
            process_ids: vec![player_pid],
            exe_name: "diag-player.exe".to_string(),
            display_name: "diag-player".to_string(),
            icon_rgba: None,
        }];

        let dir = tempfile::tempdir().unwrap();
        let mut sm = SessionManager::new();
        let frame_count = Arc::new(AtomicU64::new(0));
        sm.start_capture(
            source,
            inputs(audio_devices, app_audio_sources, false),
            true,
            Arc::clone(&frame_count),
            dir.path(),
            RecordingNaming::ProcessName,
        )
        .expect("start_capture failed");

        tokio::time::sleep(std::time::Duration::from_secs(10)).await;

        let stop_started = std::time::Instant::now();
        let handle = tokio::task::block_in_place(|| sm.stop_capture())
            .expect("stop_capture returned None while active");

        let result = tokio::time::timeout(std::time::Duration::from_secs(60), handle).await;
        let _ = player.kill();
        let result = result
            .unwrap_or_else(|_| {
                panic!(
                    "stop+finalize did not complete within 60s (elapsed before timeout: {:.1}s) -- reproduces the reported hang",
                    stop_started.elapsed().as_secs_f32()
                )
            })
            .expect("recorder task panicked/aborted");
        let finalized_path = result.expect("finalize() returned an error");

        println!(
            "stop+finalize took {:.2}s",
            stop_started.elapsed().as_secs_f32()
        );
        let metadata = std::fs::metadata(&finalized_path).expect("output file missing");
        println!("output size: {} bytes", metadata.len());
    }

    /// End-to-end: real window + real audio devices through the actual
    /// `start_highlight_buffering`/`save_highlight` wiring the GUI uses (not
    /// just the isolated `highlight`/`encode::highlight_export` unit tests).
    /// Runs long enough (past the minimum 30s buffer, well past several
    /// internal 10s segment rotations) to exercise real segment rotation,
    /// then verifies Save Highlight produces a real, playable, non-empty
    /// file and that stopping buffering cleans up the segment directory.
    /// Needs a display and audio endpoints, so it's ignored by default --
    /// run with `--ignored --nocapture` (takes ~35s real time).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn highlight_buffering_saves_a_playable_file() {
        highlight_saves_a_playable_file(false).await;
    }

    /// `highlight_buffering_saves_a_playable_file` with the frames kept on
    /// the GPU (`EncodeConfig::gpu_frames`): every rotated segment's writer
    /// takes them as textures. Ignored by default for the same reasons.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn highlight_buffering_with_gpu_frames_saves_a_playable_file() {
        highlight_saves_a_playable_file(true).await;
    }

    async fn highlight_saves_a_playable_file(gpu_frames: bool) {
        use crate::capture::audio::enumerate_audio_devices;
        use crate::sources::enumerate_sources;

        let sources = enumerate_sources();
        let source =
            pick_source_with_real_client_rect(sources).expect("no usable capture source found");
        let audio_devices = enumerate_audio_devices().unwrap_or_default();

        let dir = tempfile::tempdir().unwrap();
        let mut sm = SessionManager::new();
        let mut inputs = inputs(audio_devices, vec![], false);
        inputs.encode.gpu_frames = gpu_frames;
        sm.start_highlight_buffering(&source, inputs, dir.path(), 30)
            .expect("start_highlight_buffering failed");
        assert!(sm.is_highlighting());
        assert_eq!(sm.highlight_hwnd(), Some(source.hwnd));

        // Past the 30s minimum buffer and 3+ internal 10s segment rotations.
        tokio::time::sleep(std::time::Duration::from_secs(35)).await;

        let segment_dir = dir.path().join("polyrec").join("_highlight_buffer");
        let segments_before_save = std::fs::read_dir(&segment_dir)
            .expect("segment directory should exist by now")
            .count();
        assert!(
            segments_before_save > 0,
            "expected at least one rotated segment file on disk"
        );

        let handle = sm
            .save_highlight(
                dir.path(),
                "e2e_highlight_test",
                RecordingNaming::ProcessName,
            )
            .expect("save_highlight failed");
        let saved_path = handle
            .await
            .expect("save_highlight task panicked")
            .expect("concat_and_trim failed");

        let metadata = std::fs::metadata(&saved_path).expect("saved highlight file missing");
        assert!(
            metadata.len() > 0,
            "saved highlight file is empty: {}",
            saved_path.display()
        );
        println!(
            "highlight saved: {} ({} bytes)",
            saved_path.display(),
            metadata.len()
        );

        // Confirms the file is a real, readable container (not just non-empty
        // bytes) -- same verification remux.rs's own tests rely on.
        let audio_tracks = crate::encode::remux::count_audio_tracks(&saved_path)
            .expect("saved highlight file isn't a readable MP4");
        println!("highlight audio tracks: {audio_tracks}");

        tokio::task::block_in_place(|| sm.stop_highlight_buffering(true))
            .expect("was buffering")
            .await
            .expect("stopping panicked");
        assert!(!sm.is_highlighting());
        assert!(
            !segment_dir.exists() || std::fs::read_dir(&segment_dir).unwrap().count() == 0,
            "segment files should be gone after stop_highlight_buffering(discard: true)"
        );
    }

    /// Regression test for a real bug caught via a live manual run: pausing
    /// Highlight buffering with `stop_highlight_buffering(false)` (e.g. so a
    /// manual recording can start) then resuming with a fresh
    /// `start_highlight_buffering` call used to leave the *previous*
    /// session's segment files sitting in the segment directory forever --
    /// untracked by the new session's empty deque, so never eligible for
    /// eviction or cleanup. `start_highlight_buffering` now clears any
    /// leftover files from a prior session as soon as it (re)starts.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn pausing_then_resuming_highlight_buffering_does_not_orphan_segment_files() {
        use crate::capture::audio::enumerate_audio_devices;
        use crate::sources::enumerate_sources;

        let sources = enumerate_sources();
        let source =
            pick_source_with_real_client_rect(sources).expect("no usable capture source found");
        let audio_devices = enumerate_audio_devices().unwrap_or_default();

        let dir = tempfile::tempdir().unwrap();
        let segment_dir = dir.path().join("polyrec").join("_highlight_buffer");
        let mut sm = SessionManager::new();

        sm.start_highlight_buffering(
            &source,
            inputs(audio_devices.clone(), vec![], false),
            dir.path(),
            30,
        )
        .expect("first start_highlight_buffering failed");
        tokio::time::sleep(std::time::Duration::from_secs(12)).await;
        let files_before_pause = std::fs::read_dir(&segment_dir).unwrap().count();
        assert!(
            files_before_pause > 0,
            "expected at least one segment file before pausing"
        );

        // Pause (discard: false) -- as `refresh_highlight_buffering` does
        // right before a manual recording starts.
        tokio::task::block_in_place(|| sm.stop_highlight_buffering(false))
            .expect("was buffering")
            .await
            .expect("stopping panicked");
        assert!(!sm.is_highlighting());
        let files_immediately_after_pause = std::fs::read_dir(&segment_dir).unwrap().count();
        assert_eq!(
            files_immediately_after_pause, files_before_pause,
            "pausing (discard: false) must not delete anything by itself"
        );

        // Resume -- as `refresh_highlight_buffering` does right after a
        // manual recording stops.
        sm.start_highlight_buffering(
            &source,
            inputs(audio_devices, vec![], false),
            dir.path(),
            30,
        )
        .expect("resumed start_highlight_buffering failed");

        // The pre-pause files must be gone immediately on resume, not just
        // eventually -- they're never tracked by the new session's deque, so
        // nothing else will ever clean them up.
        let files_right_after_resume: Vec<_> = std::fs::read_dir(&segment_dir).unwrap().collect();
        assert!(
            files_right_after_resume.len() <= 1,
            "expected the pre-pause segments to be cleared on resume (at most the brand-new in-progress one), found {}",
            files_right_after_resume.len()
        );

        tokio::task::block_in_place(|| sm.stop_highlight_buffering(true))
            .expect("was buffering")
            .await
            .expect("stopping panicked");
    }
}
