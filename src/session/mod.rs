pub mod clock;
mod pipeline;
pub mod recovery;
pub mod state;

pub use pipeline::{CaptureInputs, FpsSupport, estimated_bytes_per_sec, probe_fps_support};

use crate::capture::frame_rate::LiveFps;
use crate::config::{BitrateMode, Codec, EncodeConfig, EncoderMode, ResolutionMode};
use crate::encode::RecordingCommand;
use crate::encode::actor::{StoppedItself, spawn_recording_actor};
use crate::encode::highlight_export;
use crate::error::{AppError, ResultExt};
use crate::highlight::{
    HIGHLIGHT_SEGMENT_SECONDS, Retention, SaveNowRequest, SegmentInfo, spawn_highlight_actor,
};
use crate::recording_naming::{RecordingNaming, reserve_finished_recording_path};
use crate::types::{CaptureKind, CaptureSource, SessionState};
use pipeline::{CaptureDimensions, CapturePipeline};
use state::{SessionAction, transition};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
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

impl From<&EncodeConfig> for EncodeSettings {
    fn from(config: &EncodeConfig) -> Self {
        Self {
            codec: config.codec,
            fps: config.fps(),
            resolution_mode: config.resolution_mode(),
            bitrate_mode: config.bitrate_mode(),
            encoder_mode: config.encoder_mode,
            gpu_frames: config.gpu_frames,
        }
    }
}

impl Default for EncodeSettings {
    fn default() -> Self {
        (&crate::config::Config::default().encode).into()
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
    /// Why the recorder actor stopped itself, if it did, without the user
    /// pressing stop: the disk ran low, or writes kept failing -- see
    /// `encode::actor::StoppedItself`.
    pub stopped_itself: StoppedItself,
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

    /// What the recording has lost so far -- see `capture::LostSources`.
    pub fn lost_sources(&self) -> crate::capture::LostSources {
        self.pipeline.ctrl.lost_sources()
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
    /// Why the highlight actor stopped itself, if it did -- see
    /// `SessionManager::highlight_stopped_itself`.
    stopped: StoppedItself,
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
    ///
    /// Refused while a recording is already running (or paused): replacing
    /// it would leave it capturing with nothing left to stop it.
    pub fn start_capture(
        &mut self,
        source: CaptureSource,
        inputs: CaptureInputs,
        show_border: bool,
        frame_count: Arc<AtomicU64>,
        output_dir: &Path,
        naming: RecordingNaming,
    ) -> Result<PathBuf, AppError> {
        if self.active.is_some() {
            return Err(AppError::Capture(
                "a recording is already in progress".into(),
            ));
        }
        let app_name = app_name_from_exe(&source.exe_name);
        let (output_path, polyrec_dir) = prepare_recording_paths(output_dir, &app_name);

        let free = crate::disk_space::free_bytes(&polyrec_dir)?;
        if free < crate::disk_space::MIN_FREE_BYTES {
            return Err(AppError::DiskFull(polyrec_dir));
        }

        let dims = CaptureDimensions::resolve(&source, &inputs.encode);
        let fps = dims.fps.clone();
        let stopped_itself = StoppedItself::default();
        let (recording_tx, recorder_handle) = spawn_recording_actor(
            output_path.clone(),
            polyrec_dir.clone(),
            app_name.clone(),
            naming.clone(),
            dims.encoder_params(&inputs),
            stopped_itself.clone(),
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
            stopped_itself,
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

    /// A session in the middle of a manual recording that never finishes
    /// -- see `tests::stuck_recording`.
    #[cfg(test)]
    pub(crate) fn recording_stuck() -> Self {
        Self {
            state: SessionState::Recording,
            active: Some(tests::stuck_recording()),
            highlight: None,
        }
    }

    pub fn is_highlighting(&self) -> bool {
        self.highlight.is_some()
    }

    /// The window Highlight buffering is currently following, if active.
    pub fn highlight_hwnd(&self) -> Option<usize> {
        self.highlight.as_ref().map(|h| h.hwnd)
    }

    /// Once the highlight actor's background thread has exited on its own
    /// (its disk-full check tripped, or its writer gave out) rather than via
    /// `stop_highlight_buffering`: why, with buffering stopped and its
    /// segments discarded -- mirrors how `poll_background_work` detects
    /// manual recording stopping itself early via
    /// `active.recorder_handle.is_finished()`.
    pub fn highlight_stopped_itself(&mut self) -> Option<HighlightStopped> {
        let active = self.highlight.as_ref()?;
        if !active.highlight_handle.is_finished() {
            return None;
        }
        let why = if active.stopped.disk_full() {
            HighlightStopped::DiskFull
        } else {
            HighlightStopped::Failed(
                active
                    .stopped
                    .gave_up()
                    .unwrap_or_else(|| "the Highlight writer stopped".into()),
            )
        };
        self.stop_highlight_buffering(true);
        Some(why)
    }

    /// Whether the buffer holds anything yet: at least one segment has been
    /// written and finalized.
    pub fn highlight_buffered(&self) -> bool {
        self.highlight.as_ref().is_some_and(|h| {
            !h.segments
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_empty()
        })
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
        // real pause/resume run, not by inspection -- clear them instead of
        // orphaning them.
        crate::highlight::clear_leftover_segments(&segment_dir)?;

        let free = crate::disk_space::free_bytes(&segment_dir)?;
        if free < crate::disk_space::MIN_FREE_BYTES {
            return Err(AppError::DiskFull(segment_dir));
        }

        let dims = CaptureDimensions::resolve(source, &inputs.encode);
        let segments: Arc<Mutex<VecDeque<SegmentInfo>>> = Arc::new(Mutex::new(VecDeque::new()));
        let stopped = StoppedItself::default();
        let (highlight_tx, save_now_tx, highlight_handle) = spawn_highlight_actor(
            segment_dir,
            dims.encoder_params(&inputs),
            HIGHLIGHT_SEGMENT_SECONDS,
            Retention::new(buffer_seconds, HIGHLIGHT_SEGMENT_SECONDS),
            Arc::clone(&segments),
            stopped.clone(),
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
            stopped,
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
                match active.highlight_handle.await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::error!("Highlight buffering stopped: {e}"),
                    Err(e) => tracing::error!("Highlight actor: {e}"),
                }
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

/// Why Highlight buffering stopped by itself -- see
/// `SessionManager::highlight_stopped_itself`.
#[derive(Debug, Clone, PartialEq)]
pub enum HighlightStopped {
    /// Free disk space on the buffer's volume dropped too low.
    DiskFull,
    /// Its writer couldn't start, or gave out: the error.
    Failed(String),
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
mod tests;
