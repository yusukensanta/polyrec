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
use crate::capture::frame_rate::{FPS_PRESETS, LiveFps, effective_fps, highest_encodable_fps};
use crate::capture::video::{
    CaptureTarget, display_refresh_hz, query_capture_size, query_display_size, run_video_capture,
};
use crate::config::{BitrateMode, EncoderMode, ResolutionMode};
use crate::encode::actor::{spawn_audio_pump, spawn_video_pump};
use crate::encode::gpu::{GpuFrames, GpuInput};
use crate::encode::writer::{video_bitrate_bps, video_encoder_accepts};
use crate::encode::{EncoderParams, RecordingCommand};
use crate::error::AppError;
use crate::types::{AppAudioSource, AudioDevice, AudioLevel, CaptureKind, CaptureSource, TrackId};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const VIDEO_CHANNEL_CAPACITY: usize = 4;
/// Frames one video track may have queued for its actor at once -- about a
/// second at 60 fps. See `spawn_video_pump`.
const MAX_VIDEO_FRAMES_IN_FLIGHT: usize = 64;
/// `MAX_VIDEO_FRAMES_IN_FLIGHT` for frames that stay on the GPU, where each
/// is a texture of its own in video memory (15 MB at 1440p): about 130 ms at
/// 60 fps. 64 of them let an encoder that fell behind tie up 1 GB of video
/// memory, which a game being recorded may need.
const MAX_GPU_FRAMES_IN_FLIGHT: usize = 8;
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

/// Capture-side staging size, encoder output size, bitrate and frame rate for
/// one source.
#[derive(Debug, Clone)]
pub(super) struct CaptureDimensions {
    pub capture: (u32, u32),
    pub output: (u32, u32),
    pub bitrate_bps: u32,
    /// Both the capture loop's fps cap and the encoder's frame rate: the
    /// requested rate clamped to the display (`effective_fps`), lowered by
    /// the writer actor if the encoder turns it down (`LiveFps`).
    pub fps: LiveFps,
    /// The device frames stay on, when they do -- see
    /// `EncodeConfig::gpu_frames` and `gpu_frames_for`.
    pub gpu: Option<GpuFrames>,
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
    ///
    /// The frame rate starts at `encode.fps`, lowered when the source's
    /// display can't refresh that fast (`effective_fps`). Whether the encoder
    /// takes it is only found out by the writer actor, on its own thread (see
    /// `RecordingWriter::new_at_highest_fps`), so this never waits on an
    /// encoder.
    pub(super) fn resolve(source: &CaptureSource, encode: &EncodeSettings) -> Self {
        let target = CaptureTarget::from_raw(source.kind, source.hwnd);
        let (capture, output) = resolve_sizes(source, target, encode);
        let display_hz = display_refresh_hz(target);
        let fps = effective_fps(encode.fps, display_hz);
        if fps != encode.fps {
            tracing::info!(
                "recording at {fps} fps instead of {}: the display refreshes at {display_hz:?} Hz",
                encode.fps
            );
        }
        let bitrate_bps = bitrate_bps(encode, output, fps);
        let gpu = encode.gpu_frames.then(|| gpu_frames_for(encode)).flatten();
        if gpu.is_some() {
            tracing::info!(
                "frames stay on the GPU ({capture:?} scaled to {output:?} by the encoder)"
            );
        }
        Self {
            capture,
            output,
            bitrate_bps,
            fps: LiveFps::new(fps),
            gpu,
        }
    }

    /// The writer config the recording/highlight actor builds its sink
    /// writer(s) from.
    pub(super) fn encoder_params(&self, inputs: &CaptureInputs) -> EncoderParams {
        EncoderParams {
            audio_tracks: inputs.audio_track_specs(),
            gpu: self.gpu.clone().map(|frames| GpuInput {
                frames,
                size: self.capture,
                direct: true,
            }),
            ..video_params(
                &inputs.encode,
                self.output,
                self.fps.get(),
                self.bitrate_bps,
            )
        }
    }
}

/// What the Quality popup needs to offer only the fps presets a source can
/// really be recorded at with the chosen codec, encoder and resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FpsSupport {
    /// The source's display refresh rate, if Windows reports one.
    pub display_hz: Option<u32>,
    /// The size frames would be encoded at.
    pub output: (u32, u32),
    /// Rates above 60 the encoder accepts at `output`, out of the presets and
    /// the display's own rate.
    encodable: Vec<u32>,
}

impl FpsSupport {
    /// Whether the encoder takes `fps` at `output`.
    pub fn encoder_accepts(&self, fps: u32) -> bool {
        highest_encodable_fps(fps, |fps| self.encodable.contains(&fps)) == fps
    }

    /// The rate a recording set to `requested` really runs at -- the same
    /// steps `CaptureDimensions::resolve` takes.
    pub fn recorded_fps(&self, requested: u32) -> u32 {
        highest_encodable_fps(effective_fps(requested, self.display_hz), |fps| {
            self.encodable.contains(&fps)
        })
    }
}

/// Probes what `source` can be recorded at with `encode`'s codec, encoder
/// and resolution settings. Creates real (scratch) encoders, so run it off
/// the UI thread; answers are cached (see `video_encoder_accepts`).
pub fn probe_fps_support(source: &CaptureSource, encode: &EncodeSettings) -> FpsSupport {
    let target = CaptureTarget::from_raw(source.kind, source.hwnd);
    let (_, output) = resolve_sizes(source, target, encode);
    let display_hz = display_refresh_hz(target);
    let mut candidates: Vec<u32> = FPS_PRESETS.into_iter().chain(display_hz).collect();
    candidates.sort_unstable();
    candidates.dedup();
    let encodable = candidates
        .into_iter()
        .filter(|&fps| fps > 60 && encoder_accepts(encode, output, fps))
        .collect();
    FpsSupport {
        display_hz,
        output,
        encodable,
    }
}

fn bitrate_bps(encode: &EncodeSettings, (width, height): (u32, u32), fps: u32) -> u32 {
    match encode.bitrate_mode {
        BitrateMode::Auto => video_bitrate_bps(width, height, fps),
        BitrateMode::Manual(bps) => bps,
    }
}

/// The video half of the encoder config (no audio tracks).
fn video_params(
    encode: &EncodeSettings,
    (width, height): (u32, u32),
    fps: u32,
    bitrate_bps: u32,
) -> EncoderParams {
    EncoderParams {
        width,
        height,
        fps,
        codec: encode.codec,
        bitrate_bps,
        audio_tracks: Vec::new(),
        allow_hardware_encode: matches!(encode.encoder_mode, EncoderMode::Hardware),
        gpu: None,
    }
}

/// The device for keeping a recording's frames on the GPU (see
/// `EncodeConfig::gpu_frames`), if that can work here: only a hardware
/// encoder takes textures. The device is shared, and normally created
/// already (see `GpuFrames::warm_up`). Whether the encoder takes the
/// textures is found out by the recorder, on its own thread, which reads
/// them back itself if not (see `RecordingWriter::start`). `None` keeps
/// frames in system memory.
fn gpu_frames_for(encode: &EncodeSettings) -> Option<GpuFrames> {
    if !matches!(encode.encoder_mode, EncoderMode::Hardware) {
        tracing::info!("frames stay in system memory: GPU frames need the hardware encoder");
        return None;
    }
    GpuFrames::shared()
        .inspect_err(|e| tracing::warn!("frames stay in system memory: {e}"))
        .ok()
}

/// Whether the encoder `encode` selects takes video at `output` and `fps`.
fn encoder_accepts(encode: &EncodeSettings, output: (u32, u32), fps: u32) -> bool {
    video_encoder_accepts(&video_params(
        encode,
        output,
        fps,
        bitrate_bps(encode, output, fps),
    ))
}

/// The capture-side staging size and the encoder output size for `source`
/// -- see `CaptureDimensions::resolve`.
fn resolve_sizes(
    source: &CaptureSource,
    target: CaptureTarget,
    encode: &EncodeSettings,
) -> ((u32, u32), (u32, u32)) {
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
    (capture, output)
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
    /// Each audio track's live peak level, indexed by track id -- see
    /// `audio_levels`.
    audio_levels: Vec<AudioLevel>,
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
            audio_levels: Vec::new(),
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
        let max_in_flight = if dims.gpu.is_some() {
            MAX_GPU_FRAMES_IN_FLIGHT
        } else {
            MAX_VIDEO_FRAMES_IN_FLIGHT
        };
        let (kind, handle) = (source.kind, source.hwnd);
        let ctrl = self.ctrl.clone();
        let capture = spawn_capture_thread(format!("{log_prefix}VideoCapture"), move || {
            run_video_capture(
                CaptureTarget::from_raw(kind, handle),
                dims.capture,
                dims.output,
                dims.fps.clone(),
                ctrl,
                video_tx,
                show_border,
                dims.gpu,
            )
        });
        let pump = spawn_video_pump(video_rx, dest_tx.clone(), frame_count, max_in_flight);
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
            let pump = spawn_audio_pump(audio_rx, dest_tx.clone(), self.new_audio_level());
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
                let pump = spawn_audio_pump(audio_rx, dest_tx.clone(), self.new_audio_level());
                self.track(capture, pump);
            }
        }
    }

    fn track(&mut self, capture: JoinHandle<()>, pump: JoinHandle<()>) {
        self.capture_handles.push(capture);
        self.pump_handles.push(pump);
    }

    /// Each audio track's live peak level, indexed by track id. Levels are
    /// created in the order tracks are spawned, which is track-id order
    /// (devices, then app tracks).
    pub(super) fn audio_levels(&self) -> &[AudioLevel] {
        &self.audio_levels
    }

    /// A level for the next audio track spawned.
    fn new_audio_level(&mut self) -> AudioLevel {
        let level = AudioLevel::default();
        self.audio_levels.push(level.clone());
        level
    }

    /// A pipeline whose one capture loop never ends, as if stuck sending
    /// to an actor that has stopped taking frames.
    #[cfg(test)]
    pub(super) fn stuck() -> Self {
        Self {
            ctrl: CaptureControl::new(),
            capture_handles: vec![tokio::spawn(std::future::pending())],
            pump_handles: Vec::new(),
            audio_levels: Vec::new(),
        }
    }

    /// Signals every capture loop to exit, then waits for each loop to
    /// actually return and for the pump tasks forwarding their output to
    /// drain and exit in turn (a pump exits once its capture loop's sender is
    /// dropped, which happens when the loop returns). Normally on the order
    /// of one frame -- but a loop blocked sending to an actor that has
    /// stopped taking frames only returns once that actor does, so this is
    /// never waited on from the dashboard's thread (see
    /// `SessionManager::stop_capture`).
    pub(super) async fn shutdown(self) {
        self.ctrl.request_stop();
        for h in self.capture_handles {
            let _ = h.await;
        }
        for h in self.pump_handles {
            let _ = h.await;
        }
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

    /// End-to-end: probes the primary display with the default encode
    /// settings. Creates real encoders, so it's ignored by default.
    #[test]
    #[ignore]
    fn probe_fps_support_reports_the_primary_displays_limits() {
        let source = crate::sources::enumerate_sources()
            .into_iter()
            .find(|s| s.kind == CaptureKind::Monitor)
            .expect("no monitor source");
        let start = std::time::Instant::now();
        let support = probe_fps_support(&source, &EncodeSettings::default());
        println!("probed in {:?}", start.elapsed());
        let start = std::time::Instant::now();
        let _ = probe_fps_support(&source, &EncodeSettings::default());
        println!("again (cached encoders) in {:?}", start.elapsed());
        println!("{support:?}");
        assert!(support.display_hz.is_some());
        assert!(support.encoder_accepts(60));
    }

    fn fps_support(display_hz: Option<u32>, encodable: &[u32]) -> FpsSupport {
        FpsSupport {
            display_hz,
            output: (2560, 1440),
            encodable: encodable.to_vec(),
        }
    }

    #[test]
    fn fps_support_accepts_60_and_below_and_only_probed_rates_above() {
        let support = fps_support(Some(239), &[120, 144]);
        assert!(support.encoder_accepts(30));
        assert!(support.encoder_accepts(60));
        assert!(support.encoder_accepts(144));
        assert!(!support.encoder_accepts(240));
    }

    #[test]
    fn fps_support_recorded_fps_takes_the_same_steps_as_recording_start() {
        // H264 at 1440p on a 240 Hz display: 240 isn't encodable, 144 is.
        assert_eq!(fps_support(Some(239), &[120, 144]).recorded_fps(240), 144);
        // A 60 Hz display caps it before the encoder matters.
        assert_eq!(
            fps_support(Some(60), &[120, 144, 240]).recorded_fps(144),
            60
        );
        // A 165 Hz display whose own rate the encoder takes.
        assert_eq!(
            fps_support(Some(165), &[120, 144, 165]).recorded_fps(240),
            165
        );
        assert_eq!(fps_support(None, &[]).recorded_fps(60), 60);
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
            fps: LiveFps::new(144),
            gpu: None,
        };
        let params = dims.encoder_params(&inputs);
        assert_eq!((params.width, params.height), (1280, 720));
        assert_eq!(params.bitrate_bps, 5_000_000);
        assert_eq!(
            params.fps,
            dims.fps.get(),
            "the encoder starts at the capture cap"
        );
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
