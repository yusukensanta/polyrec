pub mod clock;
pub mod state;

use crate::capture::audio::{
    run_audio_capture, run_process_loopback_capture, TARGET_CHANNELS, TARGET_SAMPLE_RATE,
};
use crate::capture::video::{query_capture_size, query_display_size, run_video_capture};
use crate::config::{BitrateMode, EncoderMode, ResolutionMode};
use crate::encode::actor::{spawn_audio_pump, spawn_recording_actor, spawn_video_pump};
use crate::encode::highlight_export;
use crate::encode::writer::video_bitrate_bps;
use crate::encode::RecordingCommand;
use crate::error::AppError;
use crate::highlight::{spawn_highlight_actor, SaveNowRequest, SegmentInfo, HIGHLIGHT_SEGMENT_SECONDS};
use crate::session::clock::RecordingClock;
use crate::types::{AudioDevice, CaptureSource, SessionState, TrackId};
use state::{transition, SessionAction};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const VIDEO_CHANNEL_CAPACITY: usize = 4;
const AUDIO_CHANNEL_CAPACITY: usize = 64;

/// Resolved encoder settings for one recording — built by the caller (the dashboard,
/// from `Config::encode`) and passed into `start_capture`.
#[derive(Debug, Clone)]
pub struct EncodeSettings {
    pub codec: String,
    pub fps: u32,
    pub resolution_mode: ResolutionMode,
    pub bitrate_mode: BitrateMode,
    pub encoder_mode: EncoderMode,
}

impl Default for EncodeSettings {
    fn default() -> Self {
        let d = crate::config::Config::default().encode;
        let resolution_mode = d.resolution_mode();
        let bitrate_mode = d.bitrate_mode();
        let encoder_mode = d.encoder_mode();
        Self {
            codec: d.codec,
            fps: d.fps,
            resolution_mode,
            bitrate_mode,
            encoder_mode,
        }
    }
}

pub struct ActiveCapture {
    capture_handles: Vec<JoinHandle<()>>,
    pump_handles: Vec<JoinHandle<()>>,
    pub recorder_handle: JoinHandle<Result<PathBuf, AppError>>,
    pub recording_tx: mpsc::Sender<RecordingCommand>,
    pub clock: Arc<RecordingClock>,
    pub pause_flag: Arc<AtomicBool>,
    /// Signals capture loops (running on their own `spawn_blocking` OS threads) to
    /// exit their loop on the next iteration. `capture_handles.abort()` alone does
    /// NOT stop these -- `abort()` has no effect on an already-running blocking
    /// closure; see `stop_capture`.
    pub stop_flag: Arc<AtomicBool>,
    pub output_path: PathBuf,
    /// Set by the recorder actor if it stopped itself early because free disk
    /// space dropped below `disk_space::MIN_FREE_BYTES` — the file up to that
    /// point is still finalized normally, this just tells the caller *why* the
    /// recording ended without the user pressing stop, so it can be surfaced.
    pub disk_full_flag: Arc<AtomicBool>,
    /// The window this recording is capturing -- used to position the overlay
    /// HUD on whichever monitor that window is actually on (see
    /// `render_overlay_viewport`), instead of assuming the primary display.
    pub hwnd: usize,
}

/// The rolling "Highlight" background buffer -- entirely separate from
/// `ActiveCapture`, on its own capture threads, and NOT part of the
/// Idle/Recording/Paused state machine (`session::state`): it's a background
/// subsystem that runs whenever enabled and no manual recording is active,
/// not a session state of its own. See the Highlight buffer design plan for
/// the full lifecycle rules this implements.
pub struct ActiveHighlight {
    capture_handles: Vec<JoinHandle<()>>,
    pump_handles: Vec<JoinHandle<()>>,
    highlight_handle: JoinHandle<Result<(), AppError>>,
    highlight_tx: mpsc::Sender<RecordingCommand>,
    save_now_tx: mpsc::UnboundedSender<SaveNowRequest>,
    stop_flag: Arc<AtomicBool>,
    segments: Arc<Mutex<VecDeque<SegmentInfo>>>,
    /// How much of the buffer `save_highlight` should try to save -- captured
    /// at start time since it's this buffering session's own setting, not
    /// necessarily whatever `config.highlight.buffer_seconds` reads right now.
    buffer_seconds: u32,
    /// Set if the highlight actor stopped itself early because free disk
    /// space on the segment directory's volume dropped too low -- checked by
    /// the same poll loop that detects `active`'s `disk_full_flag`.
    pub disk_full_flag: Arc<AtomicBool>,
    /// The window this buffer is currently following -- the dashboard's
    /// foreground-window poll compares against this to detect a switch and
    /// restart buffering for the new window (see lifecycle rule 2).
    pub hwnd: usize,
}

pub struct SessionManager {
    state: SessionState,
    pub active: Option<ActiveCapture>,
    pub highlight: Option<ActiveHighlight>,
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

    pub fn apply(&mut self, action: SessionAction) -> bool {
        match transition(&self.state, &action) {
            Some(next) => {
                self.state = next;
                true
            }
            None => false,
        }
    }

    /// Starts capture and recording actors.
    /// `frame_count` is incremented by the video pump for each frame forwarded to the encoder.
    pub fn start_capture(
        &mut self,
        source: CaptureSource,
        audio_devices: Vec<AudioDevice>,
        app_audio_only: bool,
        frame_count: Arc<AtomicU64>,
        output_dir: &std::path::Path,
        encode: EncodeSettings,
    ) -> Result<PathBuf, AppError> {
        let clock = RecordingClock::new();
        let pause_flag = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let app_name = app_name_from_exe(&source.exe_name);
        let (output_path, polyrec_dir) = prepare_recording_paths(output_dir, &app_name);

        let free = crate::disk_space::free_bytes(&polyrec_dir)?;
        if free < crate::disk_space::MIN_FREE_BYTES {
            return Err(AppError::DiskFull(polyrec_dir));
        }

        // All captured audio is downmixed/resampled to this fixed target in
        // run_audio_capture, regardless of each device's native mix format.
        let audio_specs: Vec<(u32, u16)> = audio_devices
            .iter()
            .map(|_| (TARGET_SAMPLE_RATE, TARGET_CHANNELS))
            .collect();

        let real_hwnd = windows::Win32::Foundation::HWND(
            source.hwnd as *mut core::ffi::c_void,
        );
        let (capture_width, capture_height, output_width, output_height, bitrate_bps) =
            resolve_capture_and_output_dimensions(real_hwnd, source.hwnd, &encode);

        // Spawn RecordingActor
        let disk_full_flag = Arc::new(AtomicBool::new(false));
        let (recording_tx, recorder_handle) = spawn_recording_actor(
            output_path.clone(),
            polyrec_dir,
            app_name,
            output_width,
            output_height,
            encode.fps,
            encode.codec.clone(),
            bitrate_bps,
            audio_specs,
            Arc::clone(&disk_full_flag),
            matches!(encode.encoder_mode, EncoderMode::Hardware),
        );

        // Spawn video capture + pump
        let (video_tx, video_rx) = mpsc::channel(VIDEO_CHANNEL_CAPACITY);
        let hwnd_val = source.hwnd;
        let video_clock = Arc::clone(&clock);
        let video_pause = Arc::clone(&pause_flag);
        let video_stop = Arc::clone(&stop_flag);
        let video_capture_handle = tokio::task::spawn_blocking(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("failed to build video capture runtime: {e}");
                    return;
                }
            };
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let hwnd = windows::Win32::Foundation::HWND(
                    hwnd_val as *mut core::ffi::c_void,
                );
                if let Err(e) = run_video_capture(hwnd, capture_width, capture_height, output_width, output_height, video_clock, video_pause, video_stop, video_tx).await {
                    tracing::error!("VideoCapture error: {e}");
                }
            });
        });
        let video_pump_handle =
            spawn_video_pump(video_rx, recording_tx.clone(), frame_count);

        // Spawn audio capture + pump (one per device)
        let mut capture_handles = vec![video_capture_handle];
        let mut pump_handles = vec![video_pump_handle];

        for (i, dev) in audio_devices.into_iter().enumerate() {
            let track_id = TrackId::new(i as u32);
            let (audio_tx, audio_rx) = mpsc::channel(AUDIO_CHANNEL_CAPACITY);
            let audio_clock = Arc::clone(&clock);
            let audio_pause = Arc::clone(&pause_flag);
            let audio_stop = Arc::clone(&stop_flag);
            let dev_id = dev.id.clone();
            let is_loopback = dev.is_loopback;
            let use_process_loopback = is_loopback && app_audio_only;
            let target_pid = source.process_id;
            let capture_handle = tokio::task::spawn_blocking(move || {
                let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::error!("failed to build audio capture runtime: {e}");
                        return;
                    }
                };
                let local = tokio::task::LocalSet::new();
                local.block_on(&rt, async move {
                    let result = if use_process_loopback {
                        run_process_loopback_capture(
                            target_pid, true, track_id, audio_clock, audio_pause, audio_stop, audio_tx,
                        )
                        .await
                    } else {
                        run_audio_capture(
                            dev_id, track_id, is_loopback, audio_clock, audio_pause, audio_stop, audio_tx,
                        )
                        .await
                    };
                    if let Err(e) = result {
                        tracing::error!("AudioCapture[{track_id:?}] error: {e}");
                    }
                });
            });
            let pump_handle = spawn_audio_pump(audio_rx, recording_tx.clone());
            capture_handles.push(capture_handle);
            pump_handles.push(pump_handle);
        }

        self.active = Some(ActiveCapture {
            capture_handles,
            pump_handles,
            recorder_handle,
            recording_tx,
            clock,
            pause_flag,
            stop_flag,
            output_path: output_path.clone(),
            disk_full_flag,
            hwnd: source.hwnd,
        });

        Ok(output_path)
    }

    /// Stops all capture and recording actors. Sends Stop to the recorder so it finalizes.
    /// Returns the recorder's `JoinHandle` so the caller can wait for finalization to complete
    /// before treating the output file as ready (see recorder-finalize-race design spec).
    pub fn stop_capture(&mut self) -> Option<JoinHandle<Result<PathBuf, AppError>>> {
        if let Some(active) = self.active.take() {
            // Signal capture loops directly -- abort() below has no effect on the
            // spawn_blocking closures actually running the loops (see stop_flag doc).
            active.stop_flag.store(true, Ordering::SeqCst);
            // Abort capture sources first (stops new frame production)
            for h in active.capture_handles {
                h.abort();
            }
            // Abort pump tasks (stops forwarding)
            for h in active.pump_handles {
                h.abort();
            }
            // Deliver Stop to recorder; blocking_send is safe from non-async context
            let _ = active.recording_tx.blocking_send(RecordingCommand::Stop);
            Some(active.recorder_handle)
        } else {
            None
        }
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
        self.highlight.as_ref().is_some_and(|h| h.highlight_handle.is_finished())
    }

    /// Starts (or restarts, for a different window) the Highlight background
    /// buffer against `source`. Deliberately mirrors `start_capture`'s
    /// capture-thread setup rather than sharing a helper with it -- keeping
    /// the two paths fully independent means a bug in one can't reach the
    /// other, per the design's "no degradation of existing recording"
    /// requirement. Own capture threads, own clock, own (never-toggled)
    /// pause flag -- Highlight buffering doesn't support pausing.
    #[allow(clippy::too_many_arguments)]
    pub fn start_highlight_buffering(
        &mut self,
        source: &CaptureSource,
        audio_devices: Vec<AudioDevice>,
        app_audio_only: bool,
        output_dir: &Path,
        encode: EncodeSettings,
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

        let clock = RecordingClock::new();
        let pause_flag = Arc::new(AtomicBool::new(false)); // never toggled -- no pause support
        let stop_flag = Arc::new(AtomicBool::new(false));

        let audio_specs: Vec<(u32, u16)> = audio_devices
            .iter()
            .map(|_| (TARGET_SAMPLE_RATE, TARGET_CHANNELS))
            .collect();

        let real_hwnd = windows::Win32::Foundation::HWND(source.hwnd as *mut core::ffi::c_void);
        let (capture_width, capture_height, output_width, output_height, bitrate_bps) =
            resolve_capture_and_output_dimensions(real_hwnd, source.hwnd, &encode);

        let max_segments = buffer_seconds.max(1).div_ceil(HIGHLIGHT_SEGMENT_SECONDS).max(1) as usize;
        let segments: Arc<Mutex<VecDeque<SegmentInfo>>> = Arc::new(Mutex::new(VecDeque::new()));
        let disk_full_flag = Arc::new(AtomicBool::new(false));
        let (highlight_tx, save_now_tx, highlight_handle) = spawn_highlight_actor(
            segment_dir,
            output_width,
            output_height,
            encode.fps,
            encode.codec.clone(),
            bitrate_bps,
            audio_specs,
            HIGHLIGHT_SEGMENT_SECONDS,
            max_segments,
            Arc::clone(&segments),
            Arc::clone(&disk_full_flag),
            matches!(encode.encoder_mode, EncoderMode::Hardware),
        );

        let (video_tx, video_rx) = mpsc::channel(VIDEO_CHANNEL_CAPACITY);
        let hwnd_val = source.hwnd;
        let video_clock = Arc::clone(&clock);
        let video_pause = Arc::clone(&pause_flag);
        let video_stop = Arc::clone(&stop_flag);
        let video_capture_handle = tokio::task::spawn_blocking(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("failed to build highlight video capture runtime: {e}");
                    return;
                }
            };
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let hwnd = windows::Win32::Foundation::HWND(hwnd_val as *mut core::ffi::c_void);
                if let Err(e) = run_video_capture(hwnd, capture_width, capture_height, output_width, output_height, video_clock, video_pause, video_stop, video_tx).await {
                    tracing::error!("Highlight VideoCapture error: {e}");
                }
            });
        });
        let video_pump_handle = spawn_video_pump(video_rx, highlight_tx.clone(), Arc::new(AtomicU64::new(0)));

        let mut capture_handles = vec![video_capture_handle];
        let mut pump_handles = vec![video_pump_handle];

        for (i, dev) in audio_devices.into_iter().enumerate() {
            let track_id = TrackId::new(i as u32);
            let (audio_tx, audio_rx) = mpsc::channel(AUDIO_CHANNEL_CAPACITY);
            let audio_clock = Arc::clone(&clock);
            let audio_pause = Arc::clone(&pause_flag);
            let audio_stop = Arc::clone(&stop_flag);
            let dev_id = dev.id.clone();
            let is_loopback = dev.is_loopback;
            let use_process_loopback = is_loopback && app_audio_only;
            let target_pid = source.process_id;
            let capture_handle = tokio::task::spawn_blocking(move || {
                let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::error!("failed to build highlight audio capture runtime: {e}");
                        return;
                    }
                };
                let local = tokio::task::LocalSet::new();
                local.block_on(&rt, async move {
                    let result = if use_process_loopback {
                        run_process_loopback_capture(
                            target_pid, true, track_id, audio_clock, audio_pause, audio_stop, audio_tx,
                        )
                        .await
                    } else {
                        run_audio_capture(
                            dev_id, track_id, is_loopback, audio_clock, audio_pause, audio_stop, audio_tx,
                        )
                        .await
                    };
                    if let Err(e) = result {
                        tracing::error!("Highlight AudioCapture[{track_id:?}] error: {e}");
                    }
                });
            });
            let pump_handle = spawn_audio_pump(audio_rx, highlight_tx.clone());
            capture_handles.push(capture_handle);
            pump_handles.push(pump_handle);
        }

        self.highlight = Some(ActiveHighlight {
            capture_handles,
            pump_handles,
            highlight_handle,
            highlight_tx,
            save_now_tx,
            stop_flag,
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
    pub fn stop_highlight_buffering(&mut self, discard: bool) {
        if let Some(active) = self.highlight.take() {
            active.stop_flag.store(true, Ordering::SeqCst);
            for h in active.capture_handles {
                h.abort();
            }
            for h in active.pump_handles {
                h.abort();
            }
            let _ = active.highlight_tx.blocking_send(RecordingCommand::Stop);
            if discard {
                let _ = tokio::runtime::Handle::current().block_on(active.highlight_handle);
                crate::highlight::discard_segments(&active.segments);
            }
        }
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
            reply_rx
                .await
                .map_err(|_| AppError::Encode("highlight actor stopped before confirming save".into()))?;

            let snapshot: Vec<SegmentInfo> = {
                // Recover rather than propagate a second panic -- poisoning only
                // means some *other* holder of this lock already panicked, and
                // every operation under this lock (here and in highlight/mod.rs)
                // is infallible Vec/Deque bookkeeping, so the data itself can't
                // be left in a torn state.
                let guard = segments_arc.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                guard.iter().cloned().collect()
            };

            tokio::task::spawn_blocking(move || {
                let saved_dir = highlight_saved_dir(&output_dir);
                std::fs::create_dir_all(&saved_dir)?;
                let finish_stamp = chrono::Local::now().format("%Y-%m-%d-%H-%M-%S");
                let output_path = saved_dir.join(format!("{app_name}_{finish_stamp}.mp4"));
                highlight_export::concat_and_trim(&snapshot, buffer_seconds, &output_path)
            })
            .await
            .map_err(|e| AppError::Encode(format!("save_highlight join error: {e}")))?
        });

        Ok(handle)
    }

    pub fn pause_capture(&mut self) {
        if let Some(active) = &self.active {
            active.pause_flag.store(true, Ordering::SeqCst);
            active.clock.pause();
        }
        self.apply(SessionAction::Pause);
    }

    pub fn resume_capture(&mut self) {
        if let Some(active) = &self.active {
            active.pause_flag.store(false, Ordering::SeqCst);
            active.clock.resume();
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
    let trimmed = if exe_name.len() >= 4 && exe_name[exe_name.len() - 4..].eq_ignore_ascii_case(".exe") {
        &exe_name[..exe_name.len() - 4]
    } else {
        exe_name
    };
    let sanitized: String = trimmed
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if sanitized.is_empty() {
        "recording".to_string()
    } else {
        sanitized
    }
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
        tracing::warn!("Failed to create output directory {}: {e}", polyrec_dir.display());
    }
    let start_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let temp_path = polyrec_dir.join(format!("{app_name}_recording_{start_ts}.tmp.mp4"));
    (temp_path, polyrec_dir)
}

/// Resolves the capture-side staging size, encoder output size, and bitrate --
/// shared by `start_capture` and `start_highlight_buffering`, which otherwise
/// deliberately don't share their capture-thread setup (see
/// `start_highlight_buffering`'s doc comment): this part runs before either
/// spawns any capture threads and is pure query + math, so extracting it
/// doesn't compromise that independence.
///
/// Queries the size Windows.Graphics.Capture will actually deliver frames at —
/// NOT GetClientRect, which excludes the title bar/borders and doesn't match
/// WGC's window capture size. Used to size the capture-side staging texture;
/// does NOT need to match the encoder (frames are scaled to output_width/height),
/// only itself internally (frame pool vs. staging texture). The display size is
/// only queried when the resolution mode is `Display` — the non-default mode
/// (see the resolution-regression fix) — to avoid a wasted syscall otherwise.
fn resolve_capture_and_output_dimensions(
    real_hwnd: windows::Win32::Foundation::HWND,
    hwnd_val: usize,
    encode: &EncodeSettings,
) -> (u32, u32, u32, u32, u32) {
    let (capture_width, capture_height) = match query_capture_size(real_hwnd) {
        Ok((w, h)) => (w.max(2) & !1, h.max(2) & !1),
        Err(e) => {
            tracing::warn!("query_capture_size failed for hwnd {:x}: {e}; using 1920x1080", hwnd_val);
            (1920u32, 1080u32)
        }
    };
    let display_size = if matches!(encode.resolution_mode, ResolutionMode::Display) {
        match query_display_size(real_hwnd) {
            Ok((w, h)) => Some((w.max(2) & !1, h.max(2) & !1)),
            Err(e) => {
                tracing::warn!("query_display_size failed for hwnd {:x}: {e}; using capture size", hwnd_val);
                None
            }
        }
    } else {
        None
    };
    let (output_width, output_height) =
        resolve_output_size(&encode.resolution_mode, (capture_width, capture_height), display_size);
    let bitrate_bps = match encode.bitrate_mode {
        BitrateMode::Auto => video_bitrate_bps(output_width, output_height, encode.fps),
        BitrateMode::Manual(bps) => bps,
    };
    (capture_width, capture_height, output_width, output_height, bitrate_bps)
}

/// Pure resolution-mode resolution — no I/O, so it's directly unit-testable without
/// touching real monitor/capture APIs. `display_size` is `None` when the caller didn't
/// query it (mode != Display) or the query failed.
fn resolve_output_size(
    mode: &ResolutionMode,
    capture_size: (u32, u32),
    display_size: Option<(u32, u32)>,
) -> (u32, u32) {
    match mode {
        ResolutionMode::Native => capture_size,
        ResolutionMode::Display => display_size.unwrap_or(capture_size),
        ResolutionMode::Custom(w, h) => (*w, *h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `enumerate_sources()` can return transient shell popups (e.g. volume/brightness
    /// OSD "PopupHost" windows) with a degenerate client rect that breaks H264 setup —
    /// a separate, pre-existing issue unrelated to whatever this test is actually
    /// exercising. Skip past those so tests aren't flaky based on unrelated desktop state.
    fn pick_source_with_real_client_rect(sources: Vec<CaptureSource>) -> Option<CaptureSource> {
        sources.into_iter().find(|s| unsafe {
            let hwnd = windows::Win32::Foundation::HWND(s.hwnd as *mut core::ffi::c_void);
            let mut rect = windows::Win32::Foundation::RECT::default();
            windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rect).is_ok()
                && (rect.right - rect.left) >= 100
                && (rect.bottom - rect.top) >= 100
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
        assert!(polyrec_dir.is_dir(), "polyrec subdirectory should be created");
        assert_eq!(temp_path.parent(), Some(polyrec_dir.as_path()));
        assert_eq!(temp_path.extension().and_then(|e| e.to_str()), Some("mp4"));
        assert!(temp_path.file_name().unwrap().to_str().unwrap().starts_with("vivaldi_"));
    }

    #[test]
    fn resolve_output_size_native_uses_capture_size() {
        let size = resolve_output_size(&crate::config::ResolutionMode::Native, (1280, 720), Some((2560, 1440)));
        assert_eq!(size, (1280, 720));
    }

    #[test]
    fn resolve_output_size_display_uses_display_size_when_available() {
        let size = resolve_output_size(&crate::config::ResolutionMode::Display, (1280, 720), Some((2560, 1440)));
        assert_eq!(size, (2560, 1440));
    }

    #[test]
    fn resolve_output_size_display_falls_back_to_capture_size_when_query_failed() {
        let size = resolve_output_size(&crate::config::ResolutionMode::Display, (1280, 720), None);
        assert_eq!(size, (1280, 720));
    }

    #[test]
    fn resolve_output_size_custom_uses_explicit_values() {
        let size = resolve_output_size(&crate::config::ResolutionMode::Custom(1920, 1080), (1280, 720), Some((2560, 1440)));
        assert_eq!(size, (1920, 1080));
    }

    #[test]
    fn encode_settings_default_matches_config_default() {
        let settings = EncodeSettings::default();
        let config_default = crate::config::Config::default().encode;
        assert_eq!(settings.fps, config_default.fps);
        assert_eq!(settings.codec, config_default.codec);
        assert!(matches!(settings.resolution_mode, crate::config::ResolutionMode::Native));
        assert!(matches!(settings.bitrate_mode, crate::config::BitrateMode::Auto));
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
        use windows::Win32::Media::MediaFoundation::{MFCreateSourceReaderFromURL, MF_MT_FRAME_SIZE};

        let sources = enumerate_sources();
        let source = pick_source_with_real_client_rect(sources).expect("no usable capture source found");
        let hwnd = windows::Win32::Foundation::HWND(source.hwnd as *mut core::ffi::c_void);
        let (expected_w, expected_h) = query_capture_size(hwnd).expect("query_capture_size failed");

        let audio_devices = enumerate_audio_devices().unwrap_or_default();
        let dir = tempfile::tempdir().unwrap();
        let mut sm = SessionManager::new();
        let frame_count = Arc::new(AtomicU64::new(0));
        sm.start_capture(source, audio_devices, false, Arc::clone(&frame_count), dir.path(), EncodeSettings::default())
            .expect("start_capture failed");
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let handle = tokio::task::block_in_place(|| sm.stop_capture()).expect("stop_capture returned None");
        let finalized_path = handle.await.expect("recorder task panicked").expect("finalize failed");

        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
            let _ = windows::Win32::Media::MediaFoundation::MFStartup(
                windows::Win32::Media::MediaFoundation::MF_VERSION,
                windows::Win32::Media::MediaFoundation::MFSTARTUP_FULL,
            );
            use windows::Win32::Media::MediaFoundation::{MFMediaType_Video, MF_MT_MAJOR_TYPE};
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
            let packed = video_type.GetUINT64(&MF_MT_FRAME_SIZE).expect("GetUINT64 frame_size");
            let actual_w = (packed >> 32) as u32;
            let actual_h = (packed & 0xFFFF_FFFF) as u32;
            println!("window: {expected_w}x{expected_h}, encoded: {actual_w}x{actual_h}");
            assert_eq!(actual_w, expected_w.max(2) & !1);
            assert_eq!(actual_h, expected_h.max(2) & !1);
        }
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
        let source = pick_source_with_real_client_rect(sources).expect("no usable capture source found");
        let audio_devices = enumerate_audio_devices().unwrap_or_default();

        let dir = tempfile::tempdir().unwrap();
        let mut sm = SessionManager::new();
        let frame_count = Arc::new(AtomicU64::new(0));
        let temp_path = sm.start_capture(source, audio_devices, false, Arc::clone(&frame_count), dir.path(), EncodeSettings::default())
            .expect("start_capture failed");

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let handle = tokio::task::block_in_place(|| sm.stop_capture())
            .expect("stop_capture returned None while active");
        let result = handle.await.expect("recorder task panicked/aborted");
        let finalized_path = result.expect("finalize() returned an error");

        // The temp path start_capture returned gets renamed to its finish-timestamped
        // final name inside polyrec/ once the recording completes — they're not equal.
        assert_ne!(finalized_path, temp_path);
        assert!(!temp_path.exists(), "temp recording file should have been renamed away");
        assert_eq!(finalized_path.parent(), Some(dir.path().join("polyrec")).as_deref());
        let metadata = std::fs::metadata(&finalized_path).expect("output file missing");
        assert!(metadata.len() > 0, "output file is empty: {}", finalized_path.display());
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
            MFAudioFormat_PCM, MFCreateMediaType, MFCreateSourceReaderFromURL, MFMediaType_Audio,
            MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SOURCE_READER_FIRST_AUDIO_STREAM,
        };

        let sources = enumerate_sources();
        let source = pick_source_with_real_client_rect(sources).expect("no usable capture source found");
        let audio_devices: Vec<_> = enumerate_audio_devices()
            .unwrap_or_default()
            .into_iter()
            .filter(|d| d.is_loopback)
            .collect();
        assert!(!audio_devices.is_empty(), "need a loopback audio device");

        let dir = tempfile::tempdir().unwrap();
        let mut sm = SessionManager::new();
        let frame_count = Arc::new(AtomicU64::new(0));
        sm.start_capture(source, audio_devices, false, Arc::clone(&frame_count), dir.path(), EncodeSettings::default())
            .expect("start_capture failed");

        // Deliberately fire-and-forget: PlaySync() blocks *inside* the spawned
        // process until the sound finishes, but we want it playing concurrently
        // with the recording below, not blocking this thread until done.
        #[allow(clippy::zombie_processes)]
        std::process::Command::new("powershell")
            .args(["-c", "(New-Object Media.SoundPlayer 'C:\\Windows\\Media\\Alarm01.wav').PlaySync()"])
            .spawn()
            .expect("failed to spawn powershell sound player");

        tokio::time::sleep(std::time::Duration::from_secs(4)).await;

        let handle = tokio::task::block_in_place(|| sm.stop_capture())
            .expect("stop_capture returned None while active");
        let result = handle.await.expect("recorder task panicked/aborted");
        let finalized_path = result.expect("finalize() returned an error");
        println!("output: {}", finalized_path.display());

        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
            let _ = windows::Win32::Media::MediaFoundation::MFStartup(
                windows::Win32::Media::MediaFoundation::MF_VERSION,
                windows::Win32::Media::MediaFoundation::MFSTARTUP_FULL,
            );
            let url = windows::core::HSTRING::from(finalized_path.to_str().unwrap());
            let reader = MFCreateSourceReaderFromURL(&url, None).expect("MFCreateSourceReaderFromURL");

            // Force decode to PCM so we can inspect raw samples.
            let pcm_type = MFCreateMediaType().unwrap();
            pcm_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).unwrap();
            pcm_type.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM).unwrap();
            reader
                .SetCurrentMediaType(MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32, None, &pcm_type)
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
                let buffer = sample.ConvertToContiguousBuffer().expect("ConvertToContiguousBuffer");
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut len = 0u32;
                buffer.Lock(&mut data, None, Some(&mut len)).expect("Lock");
                let bytes = std::slice::from_raw_parts(data, len as usize);
                for chunk in bytes.chunks_exact(2) {
                    let v = i16::from_le_bytes([chunk[0], chunk[1]]) as i32;
                    if v.abs() > peak {
                        peak = v.abs();
                    }
                }
                total_bytes += bytes.len();
                buffer.Unlock().expect("Unlock");
            }

            println!("DIAG: total_audio_bytes={total_bytes} peak_pcm16={peak}");
            assert!(total_bytes > 0, "audio track had zero bytes decoded");
            assert!(peak > 300, "peak PCM16 sample {peak} looks like silence in the recorded file");
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
        let mut source = pick_source_with_real_client_rect(sources).expect("no usable capture source found");
        source.process_id = std::process::id();
        let audio_devices = enumerate_audio_devices().unwrap_or_default();
        assert!(
            audio_devices.iter().any(|d| d.is_loopback),
            "need a loopback device to exercise app_audio_only"
        );

        let dir = tempfile::tempdir().unwrap();
        let mut sm = SessionManager::new();
        let frame_count = Arc::new(AtomicU64::new(0));
        let temp_path = sm.start_capture(source, audio_devices, true, Arc::clone(&frame_count), dir.path(), EncodeSettings::default())
            .expect("start_capture failed");

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let handle = tokio::task::block_in_place(|| sm.stop_capture())
            .expect("stop_capture returned None while active");
        let result = handle.await.expect("recorder task panicked/aborted");
        let finalized_path = result.expect("finalize() returned an error");

        assert_ne!(finalized_path, temp_path);
        assert_eq!(finalized_path.parent(), Some(dir.path().join("polyrec")).as_deref());
        let metadata = std::fs::metadata(&finalized_path).expect("output file missing");
        assert!(metadata.len() > 0, "output file is empty: {}", finalized_path.display());
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
        use crate::capture::audio::enumerate_audio_devices;
        use crate::sources::enumerate_sources;

        let sources = enumerate_sources();
        let source = pick_source_with_real_client_rect(sources).expect("no usable capture source found");
        let audio_devices = enumerate_audio_devices().unwrap_or_default();

        let dir = tempfile::tempdir().unwrap();
        let mut sm = SessionManager::new();
        sm.start_highlight_buffering(&source, audio_devices, false, dir.path(), EncodeSettings::default(), 30)
            .expect("start_highlight_buffering failed");
        assert!(sm.is_highlighting());
        assert_eq!(sm.highlight_hwnd(), Some(source.hwnd));

        // Past the 30s minimum buffer and 3+ internal 10s segment rotations.
        tokio::time::sleep(std::time::Duration::from_secs(35)).await;

        let segment_dir = dir.path().join("polyrec").join("_highlight_buffer");
        let segments_before_save = std::fs::read_dir(&segment_dir)
            .expect("segment directory should exist by now")
            .count();
        assert!(segments_before_save > 0, "expected at least one rotated segment file on disk");

        let handle = sm.save_highlight(dir.path(), "e2e_highlight_test").expect("save_highlight failed");
        let saved_path = handle.await.expect("save_highlight task panicked").expect("concat_and_trim failed");

        let metadata = std::fs::metadata(&saved_path).expect("saved highlight file missing");
        assert!(metadata.len() > 0, "saved highlight file is empty: {}", saved_path.display());
        println!("highlight saved: {} ({} bytes)", saved_path.display(), metadata.len());

        // Confirms the file is a real, readable container (not just non-empty
        // bytes) -- same verification remux.rs's own tests rely on.
        let audio_tracks = crate::encode::remux::count_audio_tracks(&saved_path)
            .expect("saved highlight file isn't a readable MP4");
        println!("highlight audio tracks: {audio_tracks}");

        tokio::task::block_in_place(|| sm.stop_highlight_buffering(true));
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
        let source = pick_source_with_real_client_rect(sources).expect("no usable capture source found");
        let audio_devices = enumerate_audio_devices().unwrap_or_default();

        let dir = tempfile::tempdir().unwrap();
        let segment_dir = dir.path().join("polyrec").join("_highlight_buffer");
        let mut sm = SessionManager::new();

        sm.start_highlight_buffering(&source, audio_devices.clone(), false, dir.path(), EncodeSettings::default(), 30)
            .expect("first start_highlight_buffering failed");
        tokio::time::sleep(std::time::Duration::from_secs(12)).await;
        let files_before_pause = std::fs::read_dir(&segment_dir).unwrap().count();
        assert!(files_before_pause > 0, "expected at least one segment file before pausing");

        // Pause (discard: false) -- as `refresh_highlight_buffering` does
        // right before a manual recording starts.
        tokio::task::block_in_place(|| sm.stop_highlight_buffering(false));
        assert!(!sm.is_highlighting());
        let files_immediately_after_pause = std::fs::read_dir(&segment_dir).unwrap().count();
        assert_eq!(
            files_immediately_after_pause, files_before_pause,
            "pausing (discard: false) must not delete anything by itself"
        );

        // Resume -- as `refresh_highlight_buffering` does right after a
        // manual recording stops.
        sm.start_highlight_buffering(&source, audio_devices, false, dir.path(), EncodeSettings::default(), 30)
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

        tokio::task::block_in_place(|| sm.stop_highlight_buffering(true));
    }
}
