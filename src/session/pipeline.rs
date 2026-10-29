//! Capture-thread plumbing shared by manual recording and Highlight
//! buffering: resolve sizes/bitrate for a source, then spawn one video track
//! plus every audio track, all feeding a single actor.
//!
//! The two callers share this *code*, never any *state*: every
//! `CapturePipeline::spawn` builds an entirely new set of threads, channels,
//! handles and control signals, so a manual recording and a Highlight buffer
//! can't reach into each other's pipeline -- the design's "no degradation of
//! existing recording" requirement.

use super::EncodeSettings;
use crate::capture::CaptureControl;
use crate::capture::audio::{
    TARGET_CHANNELS, TARGET_SAMPLE_RATE, run_audio_capture, run_process_loopback_capture,
};
use crate::capture::video::{
    CaptureTarget, query_capture_size, query_display_size, run_video_capture,
};
use crate::config::{BitrateMode, EncoderMode, ResolutionMode};
use crate::encode::actor::{spawn_audio_pump, spawn_video_pump};
use crate::encode::writer::video_bitrate_bps;
use crate::encode::{EncoderParams, RecordingCommand};
use crate::error::AppError;
use crate::types::{AppAudioSource, AudioDevice, CaptureKind, CaptureSource, TrackId};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const VIDEO_CHANNEL_CAPACITY: usize = 4;
/// Frames one video track may have queued for its actor at once -- about a
/// second at 60 fps. See `spawn_video_pump`.
const MAX_VIDEO_FRAMES_IN_FLIGHT: usize = 64;
const AUDIO_CHANNEL_CAPACITY: usize = 64;

/// What to record, resolved by the dashboard from its checkboxes and
/// settings and handed to `SessionManager::start_capture` /
/// `start_highlight_buffering`. Each audio source carries its own gain (a
/// linear multiplier, see `Config::audio_gain`), so a source and its gain
/// can't fall out of index alignment the way two parallel `Vec`s could.
#[derive(Debug, Clone, Default)]
pub struct CaptureInputs {
    /// Recorded as tracks `0..devices.len()`, in this order.
    pub devices: Vec<(AudioDevice, f32)>,
    /// Recorded after the devices -- see
    /// `CapturePipeline::spawn_app_audio_tracks` for how many tracks each
    /// one becomes.
    pub app_sources: Vec<(AppAudioSource, f32)>,
    /// Scope loopback devices to the video source's own process tree
    /// instead of the whole desktop mix -- see `CapturePipeline::spawn`.
    pub app_audio_only: bool,
    pub encode: EncodeSettings,
}

impl CaptureInputs {
    /// One `(TARGET_SAMPLE_RATE, TARGET_CHANNELS)` encoder stream spec per
    /// audio track the pipeline will spawn: one per device, plus one per
    /// process id across every app source (see `AppAudioSource::process_ids`).
    /// Every track is normalized to that same format before it reaches the
    /// encoder, so the *count* is the only thing that has to agree with the
    /// pipeline's track-id assignment -- deriving it here, next to the data
    /// the pipeline iterates, keeps the two from silently drifting apart.
    fn audio_track_specs(&self) -> Vec<(u32, u16)> {
        let app_tracks: usize = self
            .app_sources
            .iter()
            .map(|(source, _)| source.process_ids.len())
            .sum();
        vec![(TARGET_SAMPLE_RATE, TARGET_CHANNELS); self.devices.len() + app_tracks]
    }
}

/// Capture-side staging size, encoder output size and bitrate for one source.
#[derive(Debug, Clone, Copy)]
pub(super) struct CaptureDimensions {
    pub capture: (u32, u32),
    pub output: (u32, u32),
    pub bitrate_bps: u32,
}

impl CaptureDimensions {
    /// Queries the size Windows.Graphics.Capture will actually deliver frames
    /// at -- NOT GetClientRect, which excludes the title bar/borders and
    /// doesn't match WGC's window capture size. Used to size the capture-side
    /// staging texture; does NOT need to match the encoder (frames are scaled
    /// to `output`), only itself internally (frame pool vs. staging texture).
    /// The display size is only queried when the resolution mode is `Display`
    /// -- the non-default mode (see the resolution-regression fix) -- to avoid
    /// a wasted syscall otherwise.
    ///
    /// For a `Monitor` source, "capture size" and "display size" are the same
    /// thing by construction (you're capturing the monitor itself), so
    /// `query_display_size` -- which resolves a *window*'s monitor via
    /// `MonitorFromWindow` -- is skipped entirely; the already-queried capture
    /// size is reused as the display size too.
    pub(super) fn resolve(source: &CaptureSource, encode: &EncodeSettings) -> Self {
        let target = CaptureTarget::from_raw(source.kind, source.hwnd);
        let capture = match query_capture_size(target) {
            Ok((w, h)) => (even_at_least_2(w), even_at_least_2(h)),
            Err(e) => {
                tracing::warn!(
                    "query_capture_size failed for handle {:x}: {e}; using 1920x1080",
                    source.hwnd
                );
                (1920, 1080)
            }
        };
        let display_size = match target {
            CaptureTarget::Monitor(_) => Some(capture),
            CaptureTarget::Window(hwnd)
                if matches!(encode.resolution_mode, ResolutionMode::Display) =>
            {
                match query_display_size(hwnd) {
                    Ok((w, h)) => Some((even_at_least_2(w), even_at_least_2(h))),
                    Err(e) => {
                        tracing::warn!(
                            "query_display_size failed for hwnd {:x}: {e}; using capture size",
                            source.hwnd
                        );
                        None
                    }
                }
            }
            CaptureTarget::Window(_) => None,
        };
        let output = resolve_output_size(&encode.resolution_mode, capture, display_size);
        let bitrate_bps = match encode.bitrate_mode {
            BitrateMode::Auto => video_bitrate_bps(output.0, output.1, encode.fps),
            BitrateMode::Manual(bps) => bps,
        };
        Self {
            capture,
            output,
            bitrate_bps,
        }
    }

    /// The writer config the recording/highlight actor builds its sink
    /// writer(s) from.
    pub(super) fn encoder_params(&self, inputs: &CaptureInputs) -> EncoderParams {
        EncoderParams {
            width: self.output.0,
            height: self.output.1,
            fps: inputs.encode.fps,
            codec: inputs.encode.codec,
            bitrate_bps: self.bitrate_bps,
            audio_tracks: inputs.audio_track_specs(),
            allow_hardware_encode: matches!(inputs.encode.encoder_mode, EncoderMode::Hardware),
        }
    }
}

/// Encoders need even dimensions; WGC can report odd (or degenerate) ones.
fn even_at_least_2(v: u32) -> u32 {
    v.max(2) & !1
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

/// The capture threads and pump tasks feeding one actor, plus the control
/// signals they all share.
pub(super) struct CapturePipeline {
    /// The capture loops run on their own `spawn_blocking` OS threads, so
    /// aborting their `JoinHandle`s does NOT stop them -- `abort()` has no
    /// effect on an already-running blocking closure; they only exit once
    /// `ctrl.request_stop()` is observed (see `shutdown`).
    pub(super) ctrl: CaptureControl,
    capture_handles: Vec<JoinHandle<()>>,
    pump_handles: Vec<JoinHandle<()>>,
}

impl CapturePipeline {
    /// Spawns the video track and every audio track in `inputs`, all
    /// forwarding into `dest_tx`. `frame_count` is incremented by the video
    /// pump for each frame it forwards. `log_prefix` ("" for manual
    /// recording, "Highlight " for the highlight buffer) tags every capture
    /// thread's log lines.
    pub(super) fn spawn(
        source: &CaptureSource,
        inputs: CaptureInputs,
        dims: CaptureDimensions,
        dest_tx: &mpsc::Sender<RecordingCommand>,
        frame_count: Arc<AtomicU64>,
        show_border: bool,
        log_prefix: &'static str,
    ) -> Self {
        let mut pipeline = Self {
            ctrl: CaptureControl::new(),
            capture_handles: Vec::new(),
            pump_handles: Vec::new(),
        };
        pipeline.spawn_video_track(source, dims, dest_tx, frame_count, show_border, log_prefix);

        // A `Monitor` source has no owning process to scope loopback to
        // (`source.process_id` is 0 for one -- see `CaptureSource::process_id`'s
        // doc comment), so "App audio only" can never apply there regardless
        // of the checkbox's own state.
        let loopback_scope_pid = (inputs.app_audio_only && source.kind == CaptureKind::Window)
            .then_some(source.process_id);
        let first_app_track_id = inputs.devices.len() as u32;
        pipeline.spawn_device_audio_tracks(inputs.devices, loopback_scope_pid, dest_tx, log_prefix);
        pipeline.spawn_app_audio_tracks(
            inputs.app_sources,
            first_app_track_id,
            dest_tx,
            log_prefix,
        );
        pipeline
    }

    fn spawn_video_track(
        &mut self,
        source: &CaptureSource,
        dims: CaptureDimensions,
        dest_tx: &mpsc::Sender<RecordingCommand>,
        frame_count: Arc<AtomicU64>,
        show_border: bool,
        log_prefix: &'static str,
    ) {
        let (video_tx, video_rx) = mpsc::channel(VIDEO_CHANNEL_CAPACITY);
        let (kind, handle) = (source.kind, source.hwnd);
        let ctrl = self.ctrl.clone();
        let capture = spawn_capture_thread(format!("{log_prefix}VideoCapture"), move || {
            run_video_capture(
                CaptureTarget::from_raw(kind, handle),
                dims.capture,
                dims.output,
                ctrl,
                video_tx,
                show_border,
            )
        });
        let pump = spawn_video_pump(
            video_rx,
            dest_tx.clone(),
            frame_count,
            MAX_VIDEO_FRAMES_IN_FLIGHT,
        );
        self.track(capture, pump);
    }

    /// One capture thread + one pump task per device, as tracks
    /// `0..devices.len()`. A loopback device records through process-loopback
    /// capture scoped to `loopback_scope_pid`'s process tree when that's set
    /// ("App audio only"), and through plain endpoint loopback otherwise.
    fn spawn_device_audio_tracks(
        &mut self,
        devices: Vec<(AudioDevice, f32)>,
        loopback_scope_pid: Option<u32>,
        dest_tx: &mpsc::Sender<RecordingCommand>,
        log_prefix: &'static str,
    ) {
        for (i, (dev, gain)) in devices.into_iter().enumerate() {
            let track_id = TrackId::new(i as u32);
            let (audio_tx, audio_rx) = mpsc::channel(AUDIO_CHANNEL_CAPACITY);
            let ctrl = self.ctrl.clone();
            let process_loopback_pid = loopback_scope_pid.filter(|_| dev.is_loopback);
            let capture = spawn_capture_thread(
                format!("{log_prefix}AudioCapture[{track_id:?}]"),
                move || async move {
                    match process_loopback_pid {
                        Some(pid) => {
                            run_process_loopback_capture(pid, true, track_id, ctrl, audio_tx, gain)
                                .await
                        }
                        None => {
                            run_audio_capture(
                                dev.id,
                                track_id,
                                dev.is_loopback,
                                ctrl,
                                audio_tx,
                                gain,
                            )
                            .await
                        }
                    }
                },
            );
            let pump = spawn_audio_pump(audio_rx, dest_tx.clone());
            self.track(capture, pump);
        }
    }

    /// Per-app audio capture + pump -- independent of "App audio only" and of
    /// whichever process is the video source; each selected app is its own
    /// explicit target, always via process loopback capture since that's the
    /// only way to isolate one app's audio (see `AppAudioSource`'s doc
    /// comment). One app source can still expand to several tracks: WASAPI's
    /// process-loopback capture only targets one process tree per stream, and
    /// genuinely mixing separately captured PCM streams into one track needs
    /// a real-time mixer this doesn't have -- so multiple genuinely
    /// independent top-level process trees of the same exe (see
    /// `AppAudioSource::process_ids`, and `enumerate_app_audio_sessions`'s
    /// `canonical_root_pid`, which already collapses a single app's
    /// parent/child helper processes -- e.g. Discord's GPU/renderer/utility
    /// processes -- down to one entry) each get their own track instead. A
    /// registered-but-inactive entry (empty `process_ids`) contributes no
    /// tracks at all. Track ids start at `first_track_id`, continuing on from
    /// the device tracks.
    fn spawn_app_audio_tracks(
        &mut self,
        app_sources: Vec<(AppAudioSource, f32)>,
        first_track_id: u32,
        dest_tx: &mpsc::Sender<RecordingCommand>,
        log_prefix: &'static str,
    ) {
        let mut next_track_id = first_track_id;
        for (app_source, gain) in app_sources {
            tracing::info!(
                "AppAudio diagnostic: source={} process_ids={:?} (empty means no capture task spawns for it at all)",
                app_source.exe_name,
                app_source.process_ids
            );
            for &target_pid in &app_source.process_ids {
                let track_id = TrackId::new(next_track_id);
                next_track_id += 1;
                let (audio_tx, audio_rx) = mpsc::channel(AUDIO_CHANNEL_CAPACITY);
                let ctrl = self.ctrl.clone();
                let capture = spawn_capture_thread(
                    format!("{log_prefix}AppAudioCapture[{track_id:?}] pid={target_pid}"),
                    move || {
                        run_process_loopback_capture(
                            target_pid, true, track_id, ctrl, audio_tx, gain,
                        )
                    },
                );
                let pump = spawn_audio_pump(audio_rx, dest_tx.clone());
                self.track(capture, pump);
            }
        }
    }

    fn track(&mut self, capture: JoinHandle<()>, pump: JoinHandle<()>) {
        self.capture_handles.push(capture);
        self.pump_handles.push(pump);
    }

    /// Signals every capture loop to exit, then waits for each loop to
    /// actually return and for the pump tasks forwarding their output to
    /// drain and exit in turn (a pump exits once its capture loop's sender is
    /// dropped, which happens when the loop returns). Bounded by the capture
    /// loops' own shutdown time -- at most on the order of one frame, not the
    /// recording's length -- not blocking indefinitely.
    pub(super) fn shutdown(self) {
        self.ctrl.request_stop();
        tokio::runtime::Handle::current().block_on(async {
            for h in self.capture_handles {
                let _ = h.await;
            }
            for h in self.pump_handles {
                let _ = h.await;
            }
        });
    }
}

/// Runs the capture loop `make_future` builds on its own blocking OS thread,
/// inside a current-thread runtime + `LocalSet` -- the WASAPI/WGC COM objects
/// the loops hold aren't `Send`, so they have to be created and driven on one
/// thread. Logs (rather than returns) the loop's error, prefixed with `label`.
fn spawn_capture_thread<F, Fut>(label: String, make_future: F) -> JoinHandle<()>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), AppError>>,
{
    tokio::task::spawn_blocking(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                tracing::error!("{label}: failed to build capture runtime: {e}");
                return;
            }
        };
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async move {
            if let Err(e) = make_future().await {
                tracing::error!("{label} error: {e}");
            }
        });
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_source(process_ids: Vec<u32>) -> AppAudioSource {
        AppAudioSource {
            process_ids,
            exe_name: "Game.exe".into(),
            display_name: "Game".into(),
            icon_rgba: None,
        }
    }

    fn device() -> AudioDevice {
        AudioDevice {
            id: "mic".into(),
            name: "Mic".into(),
            is_loopback: false,
        }
    }

    #[test]
    fn audio_track_specs_counts_one_track_per_device_and_per_app_process() {
        let inputs = CaptureInputs {
            devices: vec![(device(), 1.0), (device(), 0.5)],
            app_sources: vec![(app_source(vec![10, 20]), 1.0), (app_source(vec![]), 1.0)],
            ..Default::default()
        };
        assert_eq!(
            inputs.audio_track_specs(),
            vec![(TARGET_SAMPLE_RATE, TARGET_CHANNELS); 4]
        );
    }

    #[test]
    fn audio_track_specs_empty_when_nothing_selected() {
        assert!(CaptureInputs::default().audio_track_specs().is_empty());
    }

    #[test]
    fn encoder_params_carry_output_size_bitrate_and_encode_settings() {
        let inputs = CaptureInputs {
            devices: vec![(device(), 1.0)],
            ..Default::default()
        };
        let dims = CaptureDimensions {
            capture: (1282, 722),
            output: (1280, 720),
            bitrate_bps: 5_000_000,
        };
        let params = dims.encoder_params(&inputs);
        assert_eq!((params.width, params.height), (1280, 720));
        assert_eq!(params.bitrate_bps, 5_000_000);
        assert_eq!(params.fps, inputs.encode.fps);
        assert_eq!(params.codec, inputs.encode.codec);
        assert_eq!(params.audio_tracks.len(), 1);
        assert!(
            params.allow_hardware_encode,
            "default encoder mode is Hardware"
        );
    }

    #[test]
    fn even_at_least_2_rounds_down_to_even_and_floors_at_2() {
        assert_eq!(even_at_least_2(1921), 1920);
        assert_eq!(even_at_least_2(1080), 1080);
        assert_eq!(even_at_least_2(1), 2);
        assert_eq!(even_at_least_2(0), 2);
    }

    #[test]
    fn resolve_output_size_native_uses_capture_size() {
        let size = resolve_output_size(&ResolutionMode::Native, (1280, 720), Some((2560, 1440)));
        assert_eq!(size, (1280, 720));
    }

    #[test]
    fn resolve_output_size_display_uses_display_size_when_available() {
        let size = resolve_output_size(&ResolutionMode::Display, (1280, 720), Some((2560, 1440)));
        assert_eq!(size, (2560, 1440));
    }

    #[test]
    fn resolve_output_size_display_falls_back_to_capture_size_when_query_failed() {
        let size = resolve_output_size(&ResolutionMode::Display, (1280, 720), None);
        assert_eq!(size, (1280, 720));
    }

    #[test]
    fn resolve_output_size_custom_uses_explicit_values() {
        let size = resolve_output_size(
            &ResolutionMode::Custom(1920, 1080),
            (1280, 720),
            Some((2560, 1440)),
        );
        assert_eq!(size, (1920, 1080));
    }
}
