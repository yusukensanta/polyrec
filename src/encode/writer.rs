use crate::capture::frame_pool::FrameBuf;
use crate::capture::frame_rate::highest_encodable_fps;
use crate::capture::video::scale_bgra;
use crate::config::Codec;
use crate::encode::cfr::CfrTimeline;
use crate::encode::gpu::{GpuInput, TextureReadback};
use crate::encode::mf::MfSession;
use crate::error::{AppError, ResultExt};
use crate::types::{AudioSamples, FrameData, VideoFrame};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, PoisonError};
use windows::Win32::Foundation::{E_INVALIDARG, E_POINTER};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncVideoForceKeyFrame, ICodecAPI, IMF2DBuffer,
    IMFMediaBuffer, IMFMediaBuffer_Impl, IMFMediaType, IMFSinkWriter,
    MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT,
    MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_AVG_BITRATE,
    MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
    MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SINK_WRITER_D3D_MANAGER, MFAudioFormat_AAC,
    MFAudioFormat_PCM, MFCreateDXGISurfaceBuffer, MFCreateMediaType, MFCreateMemoryBuffer,
    MFCreateSample, MFCreateSinkWriterFromURL, MFMediaType_Audio, MFMediaType_Video,
    MFVideoFormat_ARGB32, MFVideoFormat_H264, MFVideoFormat_HEVC, MFVideoInterlace_Progressive,
};
use windows::Win32::System::Variant::{VARIANT, VT_UI4};
use windows::core::HSTRING;
use windows::core::implement;
use windows::core::{GUID, Interface};

pub(crate) const AUDIO_BITRATE_BPS: u32 = 192_000;

/// The longest stretch of recording time between keyframes -- see
/// `KeyframeSchedule`.
const KEYFRAME_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Keeps a keyframe at least every `KEYFRAME_INTERVAL` of recording time.
/// GPU encoders place keyframes every N *frames* (the GOP, set to the fps so
/// it's one second at the full rate), which on a still screen -- where Windows
/// only delivers a frame when the picture changes -- stretches to ten seconds
/// or more (measured: 3 frames a second at 30 fps nominal gave one keyframe in
/// ten). A player can only start decoding at a keyframe, so seeking got slow
/// and a Highlight save could only cut that coarsely. (Microsoft's software
/// encoders go by time, about one a second, so this rarely fires for them.)
///
/// It tracks where the encoder's own keyframes fall (every `gop` frames,
/// counted from the last keyframe -- a forced one restarts the count) and
/// forces one only when the interval has run out and the encoder isn't about
/// to place one on that frame anyway. At the full frame rate it never forces,
/// so it adds no keyframes there; forcing blindly by time used to add one
/// right after many of the encoder's own.
///
/// An encoder that wouldn't take the GOP size (`gop` of `None`) keeps its own,
/// which may be far longer, so then every keyframe is forced by time.
#[derive(Debug)]
struct KeyframeSchedule {
    gop: Option<u32>,
    /// Frames since the last keyframe (0 = the last frame was one).
    frames_since: u32,
    last_keyframe_pts: Option<std::time::Duration>,
}

impl KeyframeSchedule {
    fn new(gop: Option<u32>) -> Self {
        Self {
            gop: gop.map(|gop| gop.max(1)),
            frames_since: 0,
            last_keyframe_pts: None,
        }
    }

    /// The frame at `pts` is about to be encoded: whether to force it to be a
    /// keyframe.
    fn force_for(&mut self, pts: std::time::Duration) -> bool {
        let Some(last) = self.last_keyframe_pts else {
            // The first frame is always a keyframe.
            self.mark_keyframe(pts);
            return false;
        };
        self.frames_since += 1;
        let encoder_places_one = self.gop.is_some_and(|gop| self.frames_since >= gop);
        let overdue = pts.saturating_sub(last) >= KEYFRAME_INTERVAL;
        if encoder_places_one || overdue {
            self.mark_keyframe(pts);
        }
        overdue && !encoder_places_one
    }

    fn mark_keyframe(&mut self, pts: std::time::Duration) {
        self.frames_since = 0;
        self.last_keyframe_pts = Some(pts);
    }
}

/// Bits per pixel per frame at the target quality — mid-range for H264 screen/gaming
/// content with real motion. A flat bitrate regardless of resolution/fps previously
/// starved anything above ~720p60, and H264's in-loop deblocking filter over-smooths
/// detail when it can't hit the target bitrate, which reads as blur rather than the
/// blockiness you'd expect from under-provisioning.
const VIDEO_BITS_PER_PIXEL_PER_FRAME: f64 = 0.1;

pub(crate) fn video_bitrate_bps(width: u32, height: u32, fps: u32) -> u32 {
    let raw = width as f64 * height as f64 * fps as f64 * VIDEO_BITS_PER_PIXEL_PER_FRAME;
    raw.round() as u32
}

/// What the encoders have said about a video setting, for the whole process:
/// the installed encoders don't change while it runs, and asking one means
/// creating it (about half a second for a hardware encoder). Keyed by
/// (is HEVC, width, height, fps, hardware allowed, frames as textures); the
/// bitrate doesn't decide whether a frame rate is accepted.
type EncoderAnswerKey = (bool, u32, u32, u32, bool, bool);
static ENCODER_ANSWERS: Mutex<BTreeMap<EncoderAnswerKey, bool>> = Mutex::new(BTreeMap::new());

fn encoder_answer_key(params: &EncoderParams) -> EncoderAnswerKey {
    (
        params.codec == Codec::H265,
        params.width,
        params.height,
        params.fps,
        params.allow_hardware_encode,
        params.gpu.as_ref().is_some_and(|gpu| gpu.direct),
    )
}

fn encoder_answers() -> std::sync::MutexGuard<'static, BTreeMap<EncoderAnswerKey, bool>> {
    ENCODER_ANSWERS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Whether `RecordingWriter::new` would accept `params`' video settings.
/// Found by creating one for real (on a scratch file, deleted again): what an
/// encoder takes depends on the codec, the GPU vendor's encoder and H.264's
/// level limits (1440p tops out at 144 fps), and only it knows. Answers are
/// cached (see `ENCODER_ANSWERS`). Slow when uncached, so keep it off the UI
/// thread.
pub(crate) fn video_encoder_accepts(params: &EncoderParams) -> bool {
    static PROBE_ID: AtomicU32 = AtomicU32::new(0);

    let key = encoder_answer_key(params);
    if let Some(&accepted) = encoder_answers().get(&key) {
        return accepted;
    }
    let probe_path = std::env::temp_dir().join(format!(
        "polyrec-encoder-probe-{}-{}.mp4",
        std::process::id(),
        PROBE_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let video_only = EncoderParams {
        audio_tracks: Vec::new(),
        ..params.clone()
    };
    let accepted = match RecordingWriter::new(&probe_path, &video_only) {
        Ok(_writer) => true,
        Err(e) => {
            tracing::info!(
                "encoder rejects {:?} {}x{} at {} fps: {e}",
                params.codec,
                params.width,
                params.height,
                params.fps
            );
            false
        }
    };
    let _ = std::fs::remove_file(&probe_path);
    encoder_answers().insert(key, accepted);
    accepted
}

/// Everything `RecordingWriter::new` needs to configure its sink writer --
/// built once per recording (or Highlight buffering session) and handed to
/// the actor that owns the writer; the Highlight actor reuses the same
/// params for every segment it rotates through.
#[derive(Debug, Clone)]
pub struct EncoderParams {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// H265 falls back to H264 if no HEVC encoder is available.
    pub codec: Codec,
    pub bitrate_bps: u32,
    /// `(sample_rate, channels)` per audio track, in track-id order.
    pub audio_tracks: Vec<(u32, u16)>,
    pub allow_hardware_encode: bool,
    /// Frames come as textures (`FrameData::Gpu`) rather than memory
    /// buffers -- see `encode::gpu`.
    pub gpu: Option<GpuInput>,
}

#[cfg(test)]
impl EncoderParams {
    /// The tiny software-encoded 64x64@30fps configuration the writer,
    /// remux, export and highlight tests all use.
    pub(crate) fn tiny_test(codec: Codec, audio_tracks: &[(u32, u16)]) -> Self {
        Self {
            width: 64,
            height: 64,
            fps: 30,
            codec,
            bitrate_bps: 500_000,
            audio_tracks: audio_tracks.to_vec(),
            allow_hardware_encode: false,
            gpu: None,
        }
    }
}

pub struct RecordingWriter {
    writer: IMFSinkWriter,
    video_stream: u32,
    audio_streams: Vec<u32>,
    output_path: PathBuf,
    /// Where each frame goes in the file's constant-rate video -- see
    /// `CfrTimeline`.
    timeline: std::cell::RefCell<CfrTimeline>,
    /// The newest frame's picture, which every slot after it repeats until
    /// the next frame comes.
    held: std::cell::RefCell<Option<Picture>>,
    /// The video encoder's `ICodecAPI`, if it exposes one (hardware and
    /// Microsoft's own encoders do) -- see `video_codec_api`.
    codec_api: Option<ICodecAPI>,
    keyframes: std::cell::RefCell<KeyframeSchedule>,
    /// The size the video stream takes frames at: the capture size for an
    /// encoder that takes textures (it scales them itself), the output size
    /// otherwise.
    input_size: (u32, u32),
    /// Whether frames that come as textures go to the encoder as such --
    /// see `takes_textures`.
    takes_textures: std::cell::Cell<bool>,
    /// For frames that come as textures but go to the encoder through
    /// memory (see `write_read_back`).
    readback: Option<TextureReadback>,
    /// Declared last so it drops last -- `writer` must be released before
    /// Media Foundation shuts down (see `MfSession`).
    _mf: MfSession,
}

impl RecordingWriter {
    /// Creates the writer (at the highest frame rate the encoder takes --
    /// see `new_at_highest_fps`) and starts writing. If that fails with
    /// frames handed over as textures (`GpuInput::direct`), it tries again
    /// with the writer reading them back into memory itself, so the
    /// recording goes on the usual way instead of failing. Whether the
    /// encoder takes textures used to be asked on the dashboard's thread
    /// before every recording, without starting one for real.
    pub fn start(output_path: &Path, params: &EncoderParams) -> Result<(Self, u32), AppError> {
        Self::start_with(params, |params| {
            let (writer, fps) = Self::new_at_highest_fps(output_path, params)?;
            writer.begin_writing()?;
            Ok((writer, fps))
        })
    }

    /// `start`, with `open` creating the writer as `params` say and starting
    /// it -- separate so a test can stand in an encoder that turns textures
    /// down, which no encoder tried so far has done.
    fn start_with(
        params: &EncoderParams,
        open: impl Fn(&EncoderParams) -> Result<(Self, u32), AppError>,
    ) -> Result<(Self, u32), AppError> {
        match (&params.gpu, open(params)) {
            (Some(gpu), Err(e)) if gpu.direct => {
                tracing::warn!(
                    "the encoder doesn't take frames as textures ({e}); reading them back instead"
                );
                open(&EncoderParams {
                    gpu: Some(GpuInput {
                        direct: false,
                        ..gpu.clone()
                    }),
                    ..params.clone()
                })
            }
            (_, started) => started,
        }
    }

    /// Whether frames that come as textures go to the encoder as such (see
    /// `GpuInput::direct`), rather than being read back into memory first.
    /// Starts out as the writer was set up, and turns false for good if the
    /// encoder ever turns a texture down (see `write_video`).
    pub fn takes_textures(&self) -> bool {
        self.takes_textures.get()
    }

    /// `new`, but if the encoder turns `params.fps` down, steps down through
    /// the fps presets (`highest_encodable_fps`) to the highest rate it takes,
    /// and returns that rate with the writer. Rates already known to be
    /// rejected (`ENCODER_ANSWERS`) are skipped without asking again. Runs on
    /// the writer actor's thread, so recording start never waits on it.
    pub fn new_at_highest_fps(
        output_path: &Path,
        params: &EncoderParams,
    ) -> Result<(Self, u32), AppError> {
        let at = |fps| EncoderParams {
            fps,
            ..params.clone()
        };
        let mut writer = None;
        let fps = highest_encodable_fps(params.fps, |fps| {
            let attempt = at(fps);
            let key = encoder_answer_key(&attempt);
            if encoder_answers().get(&key) == Some(&false) {
                return false;
            }
            let created = Self::new(output_path, &attempt);
            if let Err(e) = &created {
                tracing::warn!("encoder rejects {fps} fps ({e}), trying a lower rate");
            }
            encoder_answers().insert(key, created.is_ok());
            writer = created.ok();
            writer.is_some()
        });
        let writer = match writer {
            Some(writer) => writer,
            None => Self::new(output_path, &at(fps))?,
        };
        Ok((writer, fps))
    }

    pub fn new(output_path: &Path, params: &EncoderParams) -> Result<Self, AppError> {
        let EncoderParams {
            width,
            height,
            fps,
            codec,
            bitrate_bps,
            ref audio_tracks,
            allow_hardware_encode,
            ref gpu,
        } = *params;
        let mf = MfSession::start()?;
        unsafe {
            let url = HSTRING::from(output_path);
            // `allow_hardware_encode` should be true for every real recording -- false
            // here previously (unconditionally, for every caller including the actual
            // shipped app) forced Media Foundation onto a pure-software encoder even
            // when a GPU hardware MFT (NVENC/QSV/AMF) was available, which is enough
            // CPU load at 1080p60+ to visibly steal frame time from whatever's being
            // recorded (e.g. a game). Only the test suite passes false here, since
            // some CI/headless runners lack a real GPU and this keeps them exercising
            // the same known-working path they always have.
            use windows::Win32::Media::MediaFoundation::IMFAttributes;
            use windows::Win32::Media::MediaFoundation::{
                MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MFCreateAttributes,
            };
            let mut attrs_opt: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs_opt, 2).encode_ctx("MFCreateAttributes")?;
            let attrs = attrs_opt
                .ok_or_else(|| AppError::Encode("MFCreateAttributes returned None".into()))?;
            attrs
                .SetUINT32(
                    &MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS,
                    allow_hardware_encode as u32,
                )
                .encode_ctx("SetUINT32 hw_transforms")?;
            let direct = gpu.as_ref().filter(|gpu| gpu.direct);
            if let Some(gpu) = direct {
                // The encoder (and the video processor the sink writer puts
                // in front of it to convert and scale) runs on the device the
                // frames' textures are on.
                attrs
                    .SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, &gpu.frames.manager)
                    .encode_ctx("SetUnknown d3d_manager")?;
            }
            let writer: IMFSinkWriter = MFCreateSinkWriterFromURL(&url, None, Some(&attrs))
                .encode_ctx("MFCreateSinkWriterFromURL")?;

            let requested_subtype = match codec {
                Codec::H265 => MFVideoFormat_HEVC,
                Codec::H264 => MFVideoFormat_H264,
            };
            let video_out =
                make_video_output_type(width, height, fps, requested_subtype, bitrate_bps)?;
            let video_stream = match writer.AddStream(&video_out) {
                Ok(idx) => idx,
                Err(e) if requested_subtype == MFVideoFormat_HEVC => {
                    tracing::warn!("HEVC AddStream failed ({e}), falling back to H264");
                    let fallback_out = make_video_output_type(
                        width,
                        height,
                        fps,
                        MFVideoFormat_H264,
                        bitrate_bps,
                    )?;
                    writer
                        .AddStream(&fallback_out)
                        .encode_ctx("AddStream video (H264 fallback)")?
                }
                Err(e) => return Err(AppError::Encode(format!("AddStream video: {e}"))),
            };
            let (in_width, in_height) = direct.map_or((width, height), |gpu| gpu.size);
            let video_in = make_video_input_type(in_width, in_height, fps)?;
            writer
                .SetInputMediaType(video_stream, &video_in, None)
                .encode_ctx("SetInputMediaType video")?;
            let codec_api = video_codec_api(&writer, video_stream);
            let gop_set = codec_api.as_ref().is_some_and(|api| set_gop_size(api, fps));

            let mut audio_streams = Vec::new();
            for (sample_rate, channels) in audio_tracks {
                let audio_out = make_audio_output_type(*sample_rate, *channels)?;
                let audio_in = make_audio_input_type(*sample_rate, *channels)?;
                let idx = writer.AddStream(&audio_out).encode_ctx("AddStream audio")?;
                writer
                    .SetInputMediaType(idx, &audio_in, None)
                    .encode_ctx("SetInputMediaType audio")?;
                audio_streams.push(idx);
            }

            // Needed for every frame when the encoder doesn't take textures,
            // and kept ready in case one that does turns them down later.
            let readback = match gpu {
                Some(gpu) if gpu.direct => TextureReadback::new(&gpu.frames, gpu.size)
                    .inspect_err(|e| tracing::warn!("no fallback for texture frames: {e}"))
                    .ok(),
                Some(gpu) => Some(TextureReadback::new(&gpu.frames, gpu.size)?),
                None => None,
            };

            Ok(Self {
                writer,
                video_stream,
                audio_streams,
                output_path: output_path.to_path_buf(),
                timeline: std::cell::RefCell::new(CfrTimeline::new(fps)),
                held: std::cell::RefCell::new(None),
                codec_api,
                keyframes: std::cell::RefCell::new(KeyframeSchedule::new(gop_set.then_some(fps))),
                input_size: (in_width, in_height),
                takes_textures: std::cell::Cell::new(direct.is_some()),
                readback,
                _mf: mf,
            })
        }
    }

    /// Asks the encoder to make the frame at `pts` a keyframe, when
    /// `KeyframeSchedule` says one is owed. Best-effort: an encoder that can't
    /// be asked keeps its own keyframe schedule, and a hardware encoder that
    /// has fallen behind applies the request to the next frame it takes,
    /// a little earlier than this one.
    fn force_keyframe_if_due(&self, pts: std::time::Duration) {
        if !self.keyframes.borrow_mut().force_for(pts) {
            return;
        }
        if let Some(api) = &self.codec_api {
            let _ = unsafe { api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &variant_u32(1)) };
        }
    }

    pub fn begin_writing(&self) -> Result<(), AppError> {
        unsafe { self.writer.BeginWriting().encode_ctx("BeginWriting") }
    }

    /// Writes a frame, in the slot of the file's constant-rate video that
    /// `CfrTimeline` gives it, after the picture before it in every slot
    /// left empty since. One that comes as a texture goes to the encoder as
    /// such while it takes them (`takes_textures`); if it turns one down,
    /// that frame and every later one are read back into memory instead
    /// (see `read_back`), rather than the video stopping there.
    pub fn write_video(&self, frame: VideoFrame) -> Result<(), AppError> {
        let Some(place) = self.timeline.borrow_mut().place(frame.pts) else {
            tracing::debug!("dropped a frame of {:?}, its slot taken", frame.pts);
            return Ok(());
        };
        let picture = match frame.data {
            FrameData::Cpu(pixels) => Picture::Memory(frame_buffer(pixels)?),
            FrameData::Gpu(texture) if self.takes_textures() => Picture::Texture(texture),
            FrameData::Gpu(texture) => Picture::Memory(self.read_back(&texture)?),
        };
        // The picture before this one -- or, at the start, this one.
        let before = self
            .held
            .replace(Some(picture.clone()))
            .unwrap_or_else(|| picture.clone());
        for slot in place.fill {
            self.write_picture(&before, slot)?;
        }
        self.write_picture(&picture, place.slot)
    }

    /// Writes `picture` as the frame in slot `slot`.
    fn write_picture(&self, picture: &Picture, slot: u64) -> Result<(), AppError> {
        let grid = self.timeline.borrow().grid();
        self.force_keyframe_if_due(grid.time(slot));
        let (pts_hns, duration_hns) = (grid.start_hns(slot), grid.duration_hns(slot));
        let write_buffer = |buffer: &IMFMediaBuffer| {
            let sample = unsafe { make_buffer_sample(buffer, pts_hns, duration_hns)? };
            self.write_video_sample(&sample)
        };
        match picture {
            Picture::Memory(buffer) => write_buffer(buffer),
            Picture::Texture(texture) if self.takes_textures() => {
                let written = unsafe { make_texture_sample(texture, pts_hns, duration_hns) }
                    .and_then(|sample| self.write_video_sample(&sample));
                match written {
                    Err(e) if self.readback.is_some() => {
                        tracing::warn!(
                            "the encoder turned a texture frame down ({e}); reading frames back from now on"
                        );
                        self.takes_textures.set(false);
                        write_buffer(&self.read_back(texture)?)
                    }
                    written => written,
                }
            }
            Picture::Texture(texture) => write_buffer(&self.read_back(texture)?),
        }
    }

    /// A frame that came as a texture, in memory. A stream set up for
    /// textures takes memory frames at their size just the same, and scales
    /// them itself; otherwise it's scaled to the output size here, as the
    /// capture loop would.
    fn read_back(&self, texture: &ID3D11Texture2D) -> Result<IMFMediaBuffer, AppError> {
        let readback = self.readback.as_ref().ok_or_else(|| {
            AppError::Encode("a frame came as a texture, with no way to read it back".into())
        })?;
        let pixels = readback.read(texture)?;
        let ((src_w, src_h), (dst_w, dst_h)) = (readback.size, self.input_size);
        frame_buffer(scale_bgra(pixels, src_w, src_h, dst_w, dst_h))
    }

    fn write_video_sample(
        &self,
        sample: &windows::Win32::Media::MediaFoundation::IMFSample,
    ) -> Result<(), AppError> {
        unsafe {
            self.writer
                .WriteSample(self.video_stream, sample)
                .encode_ctx("WriteSample video")
        }
    }

    pub fn write_audio(&self, track_idx: usize, samples: AudioSamples) -> Result<(), AppError> {
        let stream_idx = *self
            .audio_streams
            .get(track_idx)
            .ok_or_else(|| AppError::Encode(format!("no audio stream {track_idx}")))?;

        let pts_hns = samples.pts.as_nanos() as i64 / 100;
        let sample_count = samples.samples.len();
        let channels = samples.channels as usize;
        let frame_count = sample_count / channels.max(1);
        let sample_rate = samples.sample_rate as i64;
        let duration_hns = if sample_rate > 0 {
            (frame_count as i64 * 10_000_000) / sample_rate
        } else {
            0
        };

        let pcm: Vec<u8> = samples
            .samples
            .iter()
            .flat_map(|&s| {
                let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                v.to_le_bytes()
            })
            .collect();

        unsafe {
            let sample = make_sample(&pcm, pts_hns, duration_hns)?;
            self.writer
                .WriteSample(stream_idx, &sample)
                .encode_ctx(format_args!("WriteSample audio[{track_idx}]"))
        }
    }

    pub fn finalize(self) -> Result<PathBuf, AppError> {
        unsafe {
            self.writer.Finalize().encode_ctx("Finalize")?;
        }
        Ok(self.output_path)
    }
}

/// The `ICodecAPI` of the encoder behind `stream`, if it exposes one (hardware
/// and Microsoft's own encoders do). The encoder exists once the stream's
/// input type is set.
fn video_codec_api(writer: &IMFSinkWriter, stream: u32) -> Option<ICodecAPI> {
    let mut raw = std::ptr::null_mut();
    let result =
        unsafe { writer.GetServiceForStream(stream, &GUID::zeroed(), &ICodecAPI::IID, &mut raw) };
    match result {
        Ok(()) if !raw.is_null() => Some(unsafe { ICodecAPI::from_raw(raw) }),
        result => {
            tracing::info!(
                "video encoder exposes no ICodecAPI ({result:?}); keyframes stay on its own schedule"
            );
            None
        }
    }
}

/// Pins the encoder's GOP to `frames`, so its own keyframes fall where
/// `KeyframeSchedule` expects them. Best-effort: the encoders measured default
/// to one second's worth of frames anyway.
/// Whether the encoder took `frames` as its GOP size.
fn set_gop_size(api: &ICodecAPI, frames: u32) -> bool {
    match unsafe { api.SetValue(&CODECAPI_AVEncMPVGOPSize, &variant_u32(frames)) } {
        Ok(()) => true,
        Err(e) => {
            tracing::info!("video encoder kept its own GOP size ({e}); keyframes go by time");
            false
        }
    }
}

fn variant_u32(value: u32) -> VARIANT {
    let mut variant = VARIANT::default();
    // SAFETY: writing the tag and the matching union member of a fresh VARIANT.
    unsafe {
        let inner = &mut variant.Anonymous.Anonymous;
        inner.vt = VT_UI4;
        inner.Anonymous.ulVal = value;
    }
    variant
}

unsafe fn make_video_output_type(
    width: u32,
    height: u32,
    fps: u32,
    subtype: GUID,
    bitrate_bps: u32,
) -> Result<IMFMediaType, AppError> {
    unsafe {
        let t = MFCreateMediaType().encode_ctx("MFCreateMediaType")?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .encode_ctx("SetGUID MajorType")?;
        t.SetGUID(&MF_MT_SUBTYPE, &subtype)
            .encode_ctx("SetGUID subtype")?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))
            .encode_ctx("SetUINT64 frame_size")?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))
            .encode_ctx("SetUINT64 frame_rate")?;
        t.SetUINT32(&MF_MT_AVG_BITRATE, bitrate_bps)
            .encode_ctx("SetUINT32 bitrate")?;
        // MFVideoInterlace_Progressive = MFVideoInterlaceMode(2i32)
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .encode_ctx("SetUINT32 interlace")?;
        Ok(t)
    }
}

unsafe fn make_video_input_type(
    width: u32,
    height: u32,
    fps: u32,
) -> Result<IMFMediaType, AppError> {
    unsafe {
        let t = MFCreateMediaType().encode_ctx("MFCreateMediaType")?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .encode_ctx("SetGUID MajorType")?;
        // ARGB32 matches WGC BGRA output; MF topology inserts colour-space converter automatically
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_ARGB32)
            .encode_ctx("SetGUID ARGB32")?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))
            .encode_ctx("SetUINT64 frame_size")?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))
            .encode_ctx("SetUINT64 frame_rate")?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .encode_ctx("SetUINT32 interlace")?;
        // Our pixel buffer is top-down (D3D11's natural row order, preserved by the
        // straight row-by-row copy in capture/video.rs). Without this, the color
        // converter defaults to assuming bottom-up (classic DIB convention), which
        // flips the encoded video vertically. Positive stride = top-down.
        t.SetUINT32(&MF_MT_DEFAULT_STRIDE, width * 4)
            .encode_ctx("SetUINT32 default_stride")?;
        Ok(t)
    }
}

pub(crate) unsafe fn make_audio_output_type(
    sample_rate: u32,
    channels: u16,
) -> Result<IMFMediaType, AppError> {
    unsafe {
        let t = MFCreateMediaType().encode_ctx("MFCreateMediaType")?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
            .encode_ctx("SetGUID audio major")?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)
            .encode_ctx("SetGUID AAC")?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate)
            .encode_ctx("sample_rate out")?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels as u32)
            .encode_ctx("channels out")?;
        t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AUDIO_BITRATE_BPS / 8)
            .encode_ctx("audio bitrate out")?;
        Ok(t)
    }
}

pub(crate) unsafe fn make_audio_input_type(
    sample_rate: u32,
    channels: u16,
) -> Result<IMFMediaType, AppError> {
    unsafe {
        let block_align = channels as u32 * 2;
        let bytes_per_sec = sample_rate * block_align;
        let t = MFCreateMediaType().encode_ctx("MFCreateMediaType")?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
            .encode_ctx("SetGUID audio major")?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)
            .encode_ctx("SetGUID PCM")?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate)
            .encode_ctx("sample_rate in")?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels as u32)
            .encode_ctx("channels in")?;
        t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)
            .encode_ctx("bits_per_sample")?;
        t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, block_align)
            .encode_ctx("block_align")?;
        t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, bytes_per_sec)
            .encode_ctx("bytes_per_sec")?;
        Ok(t)
    }
}

pub(crate) unsafe fn make_sample(
    data: &[u8],
    pts_hns: i64,
    duration_hns: i64,
) -> Result<windows::Win32::Media::MediaFoundation::IMFSample, AppError> {
    unsafe {
        let buffer = MFCreateMemoryBuffer(data.len() as u32).encode_ctx("MFCreateMemoryBuffer")?;

        let mut buf_ptr: *mut u8 = std::ptr::null_mut();
        buffer.Lock(&mut buf_ptr, None, None).encode_ctx("Lock")?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), buf_ptr, data.len());
        buffer.Unlock().encode_ctx("Unlock")?;
        buffer
            .SetCurrentLength(data.len() as u32)
            .encode_ctx("SetCurrentLength")?;

        let sample = MFCreateSample().encode_ctx("MFCreateSample")?;
        sample.AddBuffer(&buffer).encode_ctx("AddBuffer")?;
        sample.SetSampleTime(pts_hns).encode_ctx("SetSampleTime")?;
        sample
            .SetSampleDuration(duration_hns)
            .encode_ctx("SetSampleDuration")?;
        Ok(sample)
    }
}

/// A frame's picture as it goes to the encoder, kept to fill the slots after
/// it (see `CfrTimeline`). Nothing writes to it once it's made, so one
/// picture goes in as many samples as it fills slots, without a copy.
#[derive(Clone)]
enum Picture {
    /// In memory (see `frame_buffer`).
    Memory(IMFMediaBuffer),
    /// On the GPU, for an encoder that takes textures.
    Texture(ID3D11Texture2D),
}

/// A captured video frame's bytes as a buffer for the encoder, without a
/// copy (see `FrameMediaBuffer`).
fn frame_buffer(data: FrameBuf) -> Result<IMFMediaBuffer, AppError> {
    Ok(FrameMediaBuffer::new(data)?.into())
}

/// `make_sample` for a frame already in a buffer of its own (see
/// `frame_buffer`), without a copy.
unsafe fn make_buffer_sample(
    buffer: &IMFMediaBuffer,
    pts_hns: i64,
    duration_hns: i64,
) -> Result<windows::Win32::Media::MediaFoundation::IMFSample, AppError> {
    unsafe {
        let sample = MFCreateSample().encode_ctx("MFCreateSample")?;
        sample.AddBuffer(buffer).encode_ctx("AddBuffer")?;
        sample.SetSampleTime(pts_hns).encode_ctx("SetSampleTime")?;
        sample
            .SetSampleDuration(duration_hns)
            .encode_ctx("SetSampleDuration")?;
        Ok(sample)
    }
}

/// The sample for a frame that's still on the GPU (`FrameData::Gpu`): a DXGI
/// surface buffer over the texture itself, which the encoder's video
/// processor reads in place. The buffer holds a reference to the texture
/// until the encoder is done with it.
unsafe fn make_texture_sample(
    texture: &ID3D11Texture2D,
    pts_hns: i64,
    duration_hns: i64,
) -> Result<windows::Win32::Media::MediaFoundation::IMFSample, AppError> {
    unsafe {
        let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, texture, 0, false)
            .encode_ctx("MFCreateDXGISurfaceBuffer")?;
        let length = buffer
            .cast::<IMF2DBuffer>()
            .encode_ctx("IMF2DBuffer")?
            .GetContiguousLength()
            .encode_ctx("GetContiguousLength")?;
        buffer
            .SetCurrentLength(length)
            .encode_ctx("SetCurrentLength")?;
        let sample = MFCreateSample().encode_ctx("MFCreateSample")?;
        sample.AddBuffer(&buffer).encode_ctx("AddBuffer")?;
        sample.SetSampleTime(pts_hns).encode_ctx("SetSampleTime")?;
        sample
            .SetSampleDuration(duration_hns)
            .encode_ctx("SetSampleDuration")?;
        Ok(sample)
    }
}

/// An `IMFMediaBuffer` over a captured frame's own bytes. `make_sample`
/// copies its input into a freshly allocated `MFCreateMemoryBuffer`; for
/// video that was a second full-frame copy (8.3 MB at 1080p, 33 MB at 4K)
/// plus a fresh large allocation on every frame. This hands the encoder the
/// bytes the capture loop already read back instead. Media Foundation
/// releases the buffer once the encoder is done with the sample, which
/// drops the `FrameBuf` and returns it to its `FramePool`.
///
/// Like `MFCreateMemoryBuffer`'s buffers, this is a plain (non-2D) system
/// memory buffer whose current length starts at its full size.
#[implement(IMFMediaBuffer)]
struct FrameMediaBuffer {
    data: std::cell::UnsafeCell<FrameBuf>,
    max_len: u32,
    current_len: std::sync::atomic::AtomicU32,
}

impl FrameMediaBuffer {
    fn new(data: FrameBuf) -> Result<Self, AppError> {
        let max_len = u32::try_from(data.len())
            .map_err(|_| AppError::Encode(format!("frame too large: {} bytes", data.len())))?;
        Ok(Self {
            data: std::cell::UnsafeCell::new(data),
            max_len,
            current_len: std::sync::atomic::AtomicU32::new(max_len),
        })
    }
}

impl IMFMediaBuffer_Impl for FrameMediaBuffer_Impl {
    fn Lock(
        &self,
        ppbbuffer: *mut *mut u8,
        pcbmaxlength: *mut u32,
        pcbcurrentlength: *mut u32,
    ) -> windows::core::Result<()> {
        if ppbbuffer.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: the out-pointers are the caller's, checked non-null (the
        // two lengths are optional per the IMFMediaBuffer contract). The
        // FrameBuf is never moved or resized while this object lives, so
        // the pointer stays valid until Media Foundation releases it.
        unsafe {
            *ppbbuffer = (*self.data.get()).as_mut_ptr();
            if !pcbmaxlength.is_null() {
                *pcbmaxlength = self.max_len;
            }
            if !pcbcurrentlength.is_null() {
                *pcbcurrentlength = self.current_len.load(std::sync::atomic::Ordering::Acquire);
            }
        }
        Ok(())
    }

    fn Unlock(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn GetCurrentLength(&self) -> windows::core::Result<u32> {
        Ok(self.current_len.load(std::sync::atomic::Ordering::Acquire))
    }

    fn SetCurrentLength(&self, cbcurrentlength: u32) -> windows::core::Result<()> {
        if cbcurrentlength > self.max_len {
            return Err(E_INVALIDARG.into());
        }
        self.current_len
            .store(cbcurrentlength, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    fn GetMaxLength(&self) -> windows::core::Result<u32> {
        Ok(self.max_len)
    }
}

fn pack_u64(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_bitrate_scales_with_resolution_and_fps() {
        let p1080_60 = video_bitrate_bps(1920, 1080, 60);
        let p1440_60 = video_bitrate_bps(2560, 1440, 60);
        let p1080_30 = video_bitrate_bps(1920, 1080, 30);
        assert!(
            p1440_60 > p1080_60,
            "1440p should need a higher bitrate than 1080p at the same fps"
        );
        assert!(
            p1080_60 > p1080_30,
            "60fps should need a higher bitrate than 30fps at the same resolution"
        );
        // Sanity check against common streaming-quality guidance for 1080p60 (~8-12 Mbps).
        assert!(
            p1080_60 > 8_000_000 && p1080_60 < 16_000_000,
            "1080p60 bitrate {p1080_60} out of expected range"
        );
    }

    #[test]
    fn writer_accepts_explicit_bitrate_and_h264_codec() {
        use crate::types::VideoFrame;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("h264_test.mp4");
        let writer = RecordingWriter::new(&output, &EncoderParams::tiny_test(Codec::H264, &[]))
            .expect("RecordingWriter::new with explicit bitrate failed");
        writer.begin_writing().expect("begin_writing failed");
        writer
            .write_video(VideoFrame {
                pts: Duration::ZERO,
                data: vec![0u8; 64 * 64 * 4].into(),
            })
            .expect("write_video failed");
        let path = writer.finalize().expect("finalize failed");
        assert!(path.metadata().unwrap().len() > 0, "output file is empty");
    }

    #[test]
    fn frames_go_on_the_constant_rate_grid() {
        assert_frames_go_on_the_grid(&EncoderParams::tiny_test(Codec::H264, &[]));
    }

    /// `frames_go_on_the_constant_rate_grid` through this PC's hardware
    /// encoders. Those kept whatever times frames came with (60 fps
    /// recordings came out at 59.94), where the software path already
    /// resampled them onto a grid of its own.
    #[test]
    #[ignore]
    fn frames_go_on_the_constant_rate_grid_through_the_hardware_encoder() {
        for codec in [Codec::H264, Codec::H265] {
            assert_frames_go_on_the_grid(&EncoderParams {
                width: 1920,
                height: 1080,
                bitrate_bps: video_bitrate_bps(1920, 1080, 30),
                allow_hardware_encode: true,
                ..EncoderParams::tiny_test(codec, &[])
            });
        }
    }

    /// Writes frames as Windows hands them out (at 30 fps, slots start every
    /// 33.3 ms): the first one 40 ms into the recording, a dropped one after
    /// 100 ms, a still picture sent again after 250 ms, and two nearest the
    /// same slot -- and checks the file has a frame in every slot from the
    /// start to the last frame's, at its exact time.
    fn assert_frames_go_on_the_grid(params: &EncoderParams) {
        use crate::types::VideoFrame;
        use std::time::Duration;

        assert_eq!(params.fps, 30);
        let _mf = MfSession::start_mta().expect("MFStartup");
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("cfr.mp4");
        let writer = RecordingWriter::new(&output, params).expect("RecordingWriter::new");
        writer.begin_writing().expect("begin_writing");
        let frame_len = params.width as usize * params.height as usize * 4;
        for ms in [40, 70, 100, 170, 420, 430] {
            writer
                .write_video(VideoFrame {
                    pts: Duration::from_millis(ms),
                    data: vec![0u8; frame_len].into(),
                })
                .expect("write_video");
        }
        let samples = read_video_samples(&writer.finalize().expect("finalize"));

        let slot = 10_000_000.0 / 30.0;
        let times: Vec<i64> = samples.iter().map(|(timestamp, _)| *timestamp).collect();
        assert_eq!(samples.len(), 15, "{:?}: {times:?}", params.codec);
        for (n, (timestamp, sample)) in samples.iter().enumerate() {
            let expected = n as f64 * slot;
            assert!(
                (*timestamp as f64 - expected).abs() <= 1.0,
                "{:?} slot {n}: {times:?}",
                params.codec
            );
            let duration = unsafe { sample.GetSampleDuration() }.expect("duration");
            assert!(
                (duration as f64 - slot).abs() <= 1.0,
                "{:?} slot {n} lasts {duration}",
                params.codec
            );
        }
    }

    /// HEVC MFT availability varies by Windows version/install — this exercises the
    /// real encoder (or its automatic H264 fallback) rather than asserting a specific
    /// codec landed in the file, since either outcome is a pass for this codebase.
    #[tokio::test]
    #[ignore]
    async fn writer_accepts_h265_codec_or_falls_back_cleanly() {
        use crate::types::VideoFrame;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("h265_test.mp4");
        let writer = RecordingWriter::new(&output, &EncoderParams::tiny_test(Codec::H265, &[]))
            .expect("RecordingWriter::new with h265 (or its fallback) failed");
        writer.begin_writing().expect("begin_writing failed");
        writer
            .write_video(VideoFrame {
                pts: Duration::ZERO,
                data: vec![0u8; 64 * 64 * 4].into(),
            })
            .expect("write_video failed");
        let path = writer.finalize().expect("finalize failed");
        assert!(path.metadata().unwrap().len() > 0, "output file is empty");
    }

    /// The high fps presets (120/144/240) at 1440p through the real hardware
    /// encoder. H.264 tops out at level 5.2, which caps 1440p at 144 fps, so
    /// not every preset is accepted. This checks `video_encoder_accepts`
    /// agrees with the writer on which are, and that a file at an accepted
    /// rate really carries that frame rate and every frame written. Needs a
    /// GPU encoder, so it's ignored by default.
    #[tokio::test]
    #[ignore]
    async fn writer_encodes_high_fps_presets_at_1440p() {
        use crate::capture::frame_rate::FPS_PRESETS;
        use crate::types::VideoFrame;
        use std::time::Duration;

        const W: u32 = 2560;
        const H: u32 = 1440;
        let dir = tempfile::tempdir().unwrap();
        let _mf = crate::encode::mf::MfSession::start_mta().expect("MFStartup");
        for codec in [Codec::H264, Codec::H265] {
            for fps in FPS_PRESETS.into_iter().filter(|&fps| fps > 60) {
                let output = dir.path().join(format!("{codec:?}_{fps}.mp4"));
                let params = EncoderParams {
                    width: W,
                    height: H,
                    fps,
                    codec,
                    bitrate_bps: video_bitrate_bps(W, H, fps),
                    audio_tracks: vec![],
                    allow_hardware_encode: true,
                    gpu: None,
                };
                if !video_encoder_accepts(&params) {
                    println!("{codec:?} {fps} fps: rejected by this PC's encoder");
                    assert!(
                        RecordingWriter::new(&output, &params).is_err(),
                        "{codec:?} {fps} fps: the probe rejected what the writer accepts"
                    );
                    continue;
                }
                let writer = RecordingWriter::new(&output, &params)
                    .unwrap_or_else(|e| panic!("{codec:?} {fps} fps: writer: {e}"));
                writer.begin_writing().expect("begin_writing failed");
                for i in 0..fps {
                    // A moving bar, so the encoder can't collapse every frame
                    // into a skip.
                    let mut data = vec![0u8; (W * H * 4) as usize];
                    // A 16 px (64 byte) bar sliding across the row.
                    let x = (i * (W - 16) / fps) as usize * 4;
                    for row in data.as_chunks_mut::<{ (W * 4) as usize }>().0 {
                        row[x..x + 64].fill(0xFF);
                    }
                    writer
                        .write_video(VideoFrame {
                            pts: Duration::from_secs(1) * i / fps,
                            data: data.into(),
                        })
                        .unwrap_or_else(|e| panic!("{codec:?} {fps} fps: frame {i}: {e}"));
                }
                let path = writer.finalize().expect("finalize failed");
                let (file_fps, samples) = read_video_rate_and_sample_count(&path);
                println!("{codec:?} {fps} fps: file says {file_fps} fps, {samples} samples");
                assert_eq!(file_fps, fps, "{codec:?}: frame rate in the file");
                assert_eq!(samples, fps, "{codec:?} {fps} fps: frames in the file");
            }
        }

        // What a recording at the highest preset ends up with: the highest
        // rate the encoder accepts, and a writer that works at it.
        for codec in [Codec::H264, Codec::H265] {
            let params = EncoderParams {
                width: W,
                height: H,
                fps: 240,
                codec,
                bitrate_bps: video_bitrate_bps(W, H, 240),
                audio_tracks: vec![],
                allow_hardware_encode: true,
                gpu: None,
            };
            let output = dir.path().join(format!("{codec:?}_stepped.mp4"));
            let (writer, fps) =
                RecordingWriter::new_at_highest_fps(&output, &params).expect("new_at_highest_fps");
            println!("{codec:?} asked for 240 fps, settled on {fps}");
            assert!(
                video_encoder_accepts(&EncoderParams {
                    fps,
                    ..params.clone()
                }),
                "{codec:?}: settled on a rate the probe rejects"
            );
            writer.begin_writing().expect("begin_writing failed");
            writer
                .write_video(VideoFrame {
                    pts: Duration::ZERO,
                    data: vec![0u8; (W * H * 4) as usize].into(),
                })
                .expect("write_video failed");
            let (file_fps, _) = read_video_rate_and_sample_count(&writer.finalize().unwrap());
            assert_eq!(file_fps, fps, "{codec:?}: stepped-down rate in the file");
        }
    }

    /// The video stream's `MF_MT_FRAME_RATE` (as whole fps) and its sample
    /// count, read back through a source reader.
    fn read_video_rate_and_sample_count(path: &std::path::Path) -> (u32, u32) {
        use windows::Win32::Media::MediaFoundation::MFCreateSourceReaderFromURL;
        let url = windows::core::HSTRING::from(path.to_str().unwrap());
        let reader = unsafe { MFCreateSourceReaderFromURL(&url, None) }.expect("source reader");
        let native = unsafe { reader.GetNativeMediaType(FIRST_VIDEO_STREAM, 0) }.expect("type");
        assert_eq!(
            unsafe { native.GetGUID(&MF_MT_MAJOR_TYPE) }.unwrap(),
            MFMediaType_Video
        );
        let packed = unsafe { native.GetUINT64(&MF_MT_FRAME_RATE) }.expect("frame rate");
        let fps = ((packed >> 32) as f64 / (packed & 0xFFFF_FFFF) as f64).round() as u32;
        (fps, read_video_samples(path).len() as u32)
    }

    const FIRST_VIDEO_STREAM: u32 =
        windows::Win32::Media::MediaFoundation::MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

    /// The video stream's samples with their timestamps (in 100 ns units),
    /// read back through a source reader.
    fn read_video_samples(
        path: &std::path::Path,
    ) -> Vec<(i64, windows::Win32::Media::MediaFoundation::IMFSample)> {
        use windows::Win32::Media::MediaFoundation::{
            MF_SOURCE_READERF_ENDOFSTREAM, MFCreateSourceReaderFromURL,
        };
        let url = windows::core::HSTRING::from(path.to_str().unwrap());
        let reader = unsafe { MFCreateSourceReaderFromURL(&url, None) }.expect("source reader");
        let mut samples = Vec::new();
        loop {
            let mut flags = 0u32;
            let mut timestamp = 0i64;
            let mut sample = None;
            unsafe {
                reader.ReadSample(
                    FIRST_VIDEO_STREAM,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut timestamp),
                    Some(&mut sample),
                )
            }
            .expect("ReadSample");
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                return samples;
            }
            if let Some(sample) = sample {
                samples.push((timestamp, sample));
            }
        }
    }

    /// The timestamps of the video stream's keyframes (samples flagged
    /// `MFSampleExtension_CleanPoint`), read back through a source reader.
    fn read_keyframe_times(path: &std::path::Path) -> Vec<std::time::Duration> {
        use windows::Win32::Media::MediaFoundation::MFSampleExtension_CleanPoint;
        read_video_samples(path)
            .into_iter()
            .filter(|(_, sample)| {
                unsafe { sample.GetUINT32(&MFSampleExtension_CleanPoint) }.unwrap_or(0) == 1
            })
            .map(|(timestamp, _)| std::time::Duration::from_nanos(timestamp as u64 * 100))
            .collect()
    }

    type Rgb = (u8, u8, u8);

    /// The size of the first video frame in `path` and its colour (R, G, B)
    /// at a quarter and three quarters of the way down, decoded to RGB32
    /// through a source reader (reading the rows the right way round
    /// whichever way the frame is stored).
    fn first_frame_colours(path: &std::path::Path) -> ((u32, u32), [Rgb; 2]) {
        use windows::Win32::Media::MediaFoundation::{
            MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
            MFCreateAttributes, MFCreateSourceReaderFromURL, MFVideoFormat_RGB32,
        };
        unsafe {
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 1).unwrap();
            let attrs = attrs.unwrap();
            attrs
                .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
                .unwrap();
            let url = windows::core::HSTRING::from(path.to_str().unwrap());
            let reader = MFCreateSourceReaderFromURL(&url, &attrs).expect("source reader");
            let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
            let rgb = MFCreateMediaType().unwrap();
            rgb.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).unwrap();
            rgb.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).unwrap();
            reader
                .SetCurrentMediaType(stream, None, &rgb)
                .expect("decode to RGB32");
            let current = reader.GetCurrentMediaType(stream).unwrap();
            let size = current.GetUINT64(&MF_MT_FRAME_SIZE).unwrap();
            let (width, height) = ((size >> 32) as u32, size as u32);
            // Negative: stored bottom-up.
            let stride = current
                .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                .map_or(width as i32 * 4, |s| s as i32);
            let mut flags = 0u32;
            let mut sample = None;
            reader
                .ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample))
                .expect("ReadSample");
            let buffer = sample
                .expect("a decoded frame")
                .ConvertToContiguousBuffer()
                .unwrap();
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None).unwrap();
            let pixel = |y: u32| {
                let row = if stride < 0 { height - 1 - y } else { y };
                let p = data
                    .add(row as usize * stride.unsigned_abs() as usize + (width / 2 * 4) as usize);
                (*p.add(2), *p.add(1), *p)
            };
            let colours = [pixel(height / 4), pixel(height * 3 / 4)];
            buffer.Unlock().unwrap();
            ((width, height), colours)
        }
    }

    /// Frames handed over as textures (`EncodeConfig::gpu_frames`) come out
    /// of the encoder at the output size and in their own colours, the
    /// right way up -- as frames handed over in memory do (checked side by
    /// side, so a swapped red/blue or a flip in either shows). So do
    /// textures the writer reads back into memory: for an encoder set up
    /// without them (`GpuInput::direct` false), for one that turned them
    /// down partway (see `RecordingWriter::write_video`), and for one that
    /// turned them down from the start (see `RecordingWriter::start`). Top
    /// half red, bottom half blue. Needs a GPU encoder, so it's ignored by
    /// default.
    #[test]
    #[ignore]
    fn texture_frames_encode_scaled_in_their_own_colours() {
        use crate::encode::gpu::{GpuFrames, GpuInput};
        use crate::types::{FrameData, VideoFrame};
        use std::time::Duration;

        const CAPTURE: (u32, u32) = (1280, 720);
        const OUTPUT: (u32, u32) = (640, 360);
        const FPS: u32 = 30;
        let red_over_blue = |(width, height): (u32, u32)| -> Vec<u8> {
            let row = width as usize * 4;
            let mut bgra = vec![0u8; row * height as usize];
            for (y, line) in bgra.chunks_exact_mut(row).enumerate() {
                let pixel = if y < height as usize / 2 {
                    [0, 0, 255, 255]
                } else {
                    [255, 0, 0, 255]
                };
                for px in line.as_chunks_mut::<4>().0 {
                    *px = pixel;
                }
            }
            bgra
        };

        let _mf = crate::encode::mf::MfSession::start_mta().expect("MFStartup");
        let dir = tempfile::tempdir().unwrap();
        let gpu = GpuFrames::new().expect("GpuFrames");
        let texture = gpu
            .texture_from_bgra(CAPTURE.0, CAPTURE.1, &red_over_blue(CAPTURE))
            .expect("texture");

        #[derive(Debug, Clone, Copy, PartialEq)]
        enum Input {
            Memory,
            Texture,
            ReadBack,
            ReadBackAfterRefusal,
            ReadBackAfterRefusalAtStart,
        }
        let mut outputs = Vec::new();
        for input in [
            Input::Memory,
            Input::Texture,
            Input::ReadBack,
            Input::ReadBackAfterRefusal,
            Input::ReadBackAfterRefusalAtStart,
        ] {
            let output = dir.path().join(format!("{input:?}.mp4"));
            let params = EncoderParams {
                width: OUTPUT.0,
                height: OUTPUT.1,
                fps: FPS,
                codec: Codec::H264,
                bitrate_bps: video_bitrate_bps(OUTPUT.0, OUTPUT.1, FPS),
                audio_tracks: vec![],
                allow_hardware_encode: true,
                gpu: (input != Input::Memory).then(|| GpuInput {
                    frames: gpu.clone(),
                    size: CAPTURE,
                    direct: input != Input::ReadBack,
                }),
            };
            let writer = if input == Input::ReadBackAfterRefusalAtStart {
                // An encoder that won't be set up for textures at all.
                let (writer, fps) = RecordingWriter::start_with(&params, |params| {
                    if params.gpu.as_ref().is_some_and(|gpu| gpu.direct) {
                        return Err(AppError::Encode("no textures here".into()));
                    }
                    RecordingWriter::start(&output, params)
                })
                .expect("start");
                assert_eq!(fps, FPS);
                writer
            } else {
                let writer = RecordingWriter::new(&output, &params).expect("writer");
                writer.begin_writing().expect("begin_writing");
                writer
            };
            if input == Input::ReadBackAfterRefusal {
                // What `write_video` does once the encoder turns a texture
                // down.
                writer.takes_textures.set(false);
            }
            assert_eq!(writer.takes_textures(), input == Input::Texture);
            for i in 0..FPS {
                let data = if input == Input::Memory {
                    red_over_blue(OUTPUT).into()
                } else {
                    FrameData::Gpu(texture.clone())
                };
                writer
                    .write_video(VideoFrame {
                        pts: Duration::from_secs(1) * i / FPS,
                        data,
                    })
                    .expect("write_video");
            }
            let path = writer.finalize().expect("finalize");
            let frames = read_keyframe_times(&path);
            let (size, colours) = first_frame_colours(&path);
            println!("{input:?}: {size:?}, top/bottom {colours:?}, keyframes {frames:?}");
            outputs.push((size, colours));
        }
        for (size, [top, bottom]) in outputs {
            assert_eq!(size, OUTPUT);
            let near = |(r, g, b): (u8, u8, u8), want: (u8, u8, u8)| {
                r.abs_diff(want.0) < 40 && g.abs_diff(want.1) < 40 && b.abs_diff(want.2) < 40
            };
            assert!(near(top, (255, 0, 0)), "top should be red: {top:?}");
            assert!(
                near(bottom, (0, 0, 255)),
                "bottom should be blue: {bottom:?}"
            );
        }
    }

    /// A keyframe at least every `KEYFRAME_INTERVAL` of recording time,
    /// through the real encoders (hardware and software, both codecs), with
    /// a still screen's sparse frames (every 10th frame of 30 fps, where the
    /// encoders on their own gave one keyframe in ten seconds) -- and no
    /// extra keyframes at the full frame rate. Needs a GPU encoder, so it's
    /// ignored by default.
    ///
    /// The sparse frames are written at their real pace (10 s per case): a
    /// hardware encoder works asynchronously, so a force request applies to
    /// whichever frame it takes next, and with frames written as fast as
    /// possible that's one queued well before the frame asked for.
    #[tokio::test]
    #[ignore]
    async fn writer_puts_a_keyframe_at_least_every_two_seconds() {
        use crate::types::VideoFrame;
        use std::time::Duration;

        const W: u32 = 1280;
        const H: u32 = 720;
        const FPS: u32 = 30;
        const SECS: u32 = 10;
        let _mf = crate::encode::mf::MfSession::start_mta().expect("MFStartup");
        let dir = tempfile::tempdir().unwrap();
        for (codec, hardware, step) in [Codec::H264, Codec::H265]
            .into_iter()
            .flat_map(|c| [true, false].map(|hw| (c, hw)))
            .flat_map(|(c, hw)| [1u32, 10].map(|step| (c, hw, step)))
        {
            let label = format!("{codec:?} hw={hardware} every {step} frame(s)");
            let output = dir.path().join(format!("{codec:?}_{hardware}_{step}.mp4"));
            let params = EncoderParams {
                width: W,
                height: H,
                fps: FPS,
                codec,
                bitrate_bps: video_bitrate_bps(W, H, FPS),
                audio_tracks: vec![],
                allow_hardware_encode: hardware,
                gpu: None,
            };
            let writer = RecordingWriter::new(&output, &params).expect("writer");
            writer.begin_writing().expect("begin_writing");
            let spacing = Duration::from_secs(1) * step / FPS;
            for i in (0..FPS * SECS).step_by(step as usize) {
                if step > 1 && i > 0 {
                    std::thread::sleep(spacing);
                }
                let mut data = vec![0u8; (W * H * 4) as usize];
                let x = (i % (W - 16)) as usize * 4;
                for row in data.as_chunks_mut::<{ (W * 4) as usize }>().0 {
                    row[x..x + 64].fill(0xFF);
                }
                writer
                    .write_video(VideoFrame {
                        pts: Duration::from_secs(1) * i / FPS,
                        data: data.into(),
                    })
                    .expect("write_video");
            }
            let keyframes = read_keyframe_times(&writer.finalize().expect("finalize"));
            let longest_gap = keyframes
                .windows(2)
                .map(|pair| pair[1] - pair[0])
                .chain(
                    keyframes
                        .last()
                        .map(|&last| Duration::from_secs(SECS.into()) - last),
                )
                .max()
                .unwrap_or(Duration::from_secs(SECS.into()));
            println!("{label}: keyframes at {keyframes:?}, longest gap {longest_gap:?}");
            assert!(
                longest_gap <= KEYFRAME_INTERVAL,
                "{label}: {longest_gap:?} between keyframes"
            );
            if step == 1 {
                // The encoder's own one a second, none forced on top.
                assert!(keyframes.len() <= SECS as usize, "{label}: {keyframes:?}");
            }
        }
    }

    /// The frames (by pts, in seconds) a `KeyframeSchedule` of `gop` forces
    /// to be keyframes, given frames at `pts_secs`.
    fn forced_keyframes(gop: u32, pts_secs: impl IntoIterator<Item = f64>) -> Vec<f64> {
        forced_keyframes_with(Some(gop), pts_secs)
    }

    fn forced_keyframes_with(
        gop: Option<u32>,
        pts_secs: impl IntoIterator<Item = f64>,
    ) -> Vec<f64> {
        let mut schedule = KeyframeSchedule::new(gop);
        pts_secs
            .into_iter()
            .filter(|&s| schedule.force_for(std::time::Duration::from_secs_f64(s)))
            .collect()
    }

    #[test]
    fn keyframe_schedule_never_forces_at_the_full_frame_rate() {
        let every_frame = (0..30 * 10).map(|i| f64::from(i) / 30.0);
        assert_eq!(forced_keyframes(30, every_frame), Vec::<f64>::new());
    }

    #[test]
    fn keyframe_schedule_forces_one_every_interval_on_sparse_frames() {
        // A still screen: every 10th frame of 30 fps -- the encoder alone
        // would place one keyframe in ten seconds.
        let every_tenth = (0..30 * 10).step_by(10).map(|i| f64::from(i) / 30.0);
        assert_eq!(forced_keyframes(30, every_tenth), [2.0, 4.0, 6.0, 8.0]);
    }

    #[test]
    fn keyframe_schedule_forces_one_every_interval_when_the_gop_is_unknown() {
        // The encoder turned the GOP size down, so its own keyframes could be
        // any distance apart: even at the full frame rate, one is forced
        // every interval.
        let every_frame = (0..30 * 10).map(|i| f64::from(i) / 30.0);
        assert_eq!(
            forced_keyframes_with(None, every_frame),
            [2.0, 4.0, 6.0, 8.0]
        );
    }

    #[test]
    fn keyframe_schedule_forces_the_first_frame_after_a_long_gap() {
        assert_eq!(forced_keyframes(30, [0.0, 0.5, 9.0, 9.5]), [9.0]);
    }

    #[test]
    fn keyframe_schedule_leaves_a_due_keyframe_to_the_encoder() {
        // One frame a second with a GOP of 2: the encoder's own keyframe
        // lands on the 2 s frame, exactly when the interval runs out.
        assert_eq!(
            forced_keyframes(2, (0..10).map(f64::from)),
            Vec::<f64>::new()
        );
    }

    #[test]
    fn keyframe_schedule_counts_the_gop_from_a_forced_keyframe() {
        // Forced at 2.0; the encoder restarts its count there, so its own
        // next keyframe is 5 frames later (3.5, not 2.9), which leaves 4.95
        // within the interval; 5.6 is owed by time.
        let frames = [0.0, 0.1, 2.0, 2.3, 2.6, 2.9, 3.2, 3.5, 4.95, 5.6];
        assert_eq!(forced_keyframes(5, frames), [2.0, 5.6]);
    }

    #[test]
    fn pack_u64_encodes_correctly() {
        assert_eq!(pack_u64(1920, 1080), 0x0000_0780_0000_0438);
        assert_eq!(pack_u64(60, 1), 0x0000_003C_0000_0001);
    }

    #[test]
    fn f32_to_pcm16_conversion_clamps() {
        let samples = [0.0f32, 1.0, -1.0, 2.0, -2.0];
        let pcm: Vec<i16> = samples
            .iter()
            .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
            .collect();
        assert_eq!(pcm[0], 0);
        assert_eq!(pcm[1], 32767);
        assert_eq!(pcm[2], -32767);
        assert_eq!(pcm[3], 32767);
        assert_eq!(pcm[4], -32767);
    }

    #[test]
    fn writer_creates_mp4_for_tiny_resolution() {
        use crate::types::{AudioSamples, TrackId, VideoFrame};
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("test.mp4");

        // 64×64 at 30fps, one stereo 48kHz audio track
        let audio_tracks = vec![(48000u32, 2u16)];
        let writer = RecordingWriter::new(
            &output,
            &EncoderParams::tiny_test(Codec::H264, &audio_tracks),
        );
        assert!(
            writer.is_ok(),
            "RecordingWriter::new failed: {:?}",
            writer.err()
        );
        let writer = writer.unwrap();

        writer.begin_writing().expect("begin_writing failed");

        // One blank BGRA frame (64×64×4 bytes)
        let frame = VideoFrame {
            pts: Duration::ZERO,
            data: vec![0u8; 64 * 64 * 4].into(),
        };
        writer.write_video(frame).expect("write_video failed");

        // One 10ms audio chunk (480 samples × 2 channels at 48kHz)
        let samples = AudioSamples {
            track_id: TrackId::new(0),
            pts: Duration::ZERO,
            samples: vec![0.0f32; 480 * 2],
            sample_rate: 48000,
            channels: 2,
        };
        writer.write_audio(0, samples).expect("write_audio failed");

        let path = writer.finalize().expect("finalize failed");
        assert!(path.exists(), "output file not created");
        assert!(path.metadata().unwrap().len() > 0, "output file is empty");
    }

    /// Reproduces a real-world "Saving recording..." hang: 3 selected audio
    /// sources (e.g. 2 devices + 1 app-audio process) but the user stops
    /// almost immediately, before the app-audio track's async
    /// process-loopback activation has delivered its first buffer -- so
    /// that track's stream never receives a single `WriteSample` call.
    /// Bounded by a timeout thread rather than calling `finalize()` inline,
    /// so if the hang is still present this fails with a clear panic after
    /// 15s instead of hanging the test run indefinitely.
    #[test]
    fn finalize_does_not_hang_when_one_of_several_audio_tracks_receives_no_samples() {
        use crate::types::{AudioSamples, TrackId, VideoFrame};
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("multi_track_test.mp4");

        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let audio_tracks = vec![(48000u32, 2u16), (48000u32, 2u16), (48000u32, 2u16)];
            let writer = RecordingWriter::new(
                &output,
                &EncoderParams::tiny_test(Codec::H264, &audio_tracks),
            )
            .expect("RecordingWriter::new failed");
            writer.begin_writing().expect("begin_writing failed");

            writer
                .write_video(VideoFrame {
                    pts: Duration::ZERO,
                    data: vec![0u8; 64 * 64 * 4].into(),
                })
                .expect("write_video failed");

            let sample_for = |track_idx: usize| AudioSamples {
                track_id: TrackId::new(track_idx as u32),
                pts: Duration::ZERO,
                samples: vec![0.0f32; 480 * 2],
                sample_rate: 48000,
                channels: 2,
            };
            // Tracks 0 and 1 (the two "devices") get a sample. Track 2 (the
            // "app-audio" slot) never does.
            writer
                .write_audio(0, sample_for(0))
                .expect("write_audio 0 failed");
            writer
                .write_audio(1, sample_for(1))
                .expect("write_audio 1 failed");

            let _ = done_tx.send(writer.finalize());
        });

        match done_rx.recv_timeout(Duration::from_secs(15)) {
            Ok(result) => {
                let path = result.expect("finalize failed");
                assert!(path.exists(), "output file not created");
            }
            Err(_) => panic!(
                "finalize() did not return within 15s -- an audio track with zero \
                 samples appears to hang IMFSinkWriter::Finalize"
            ),
        }
    }

    /// The sample bytes Media Foundation reads, via the generic
    /// ConvertToContiguousBuffer + Lock path every consumer uses.
    unsafe fn sample_bytes(sample: &windows::Win32::Media::MediaFoundation::IMFSample) -> Vec<u8> {
        unsafe {
            let buffer = sample.ConvertToContiguousBuffer().unwrap();
            let mut ptr: *mut u8 = std::ptr::null_mut();
            let mut len: u32 = 0;
            buffer.Lock(&mut ptr, None, Some(&mut len)).unwrap();
            let bytes = std::slice::from_raw_parts(ptr, len as usize).to_vec();
            buffer.Unlock().unwrap();
            bytes
        }
    }

    #[test]
    fn frame_sample_matches_the_copying_sample_byte_for_byte() {
        let _mf = MfSession::start_mta().unwrap();
        let pixels: Vec<u8> = (0..64 * 64 * 4).map(|i| (i % 251) as u8).collect();
        unsafe {
            let copied = make_sample(&pixels, 1234, 166_666).unwrap();
            let buffer = frame_buffer(pixels.clone().into()).unwrap();
            let zero_copy = make_buffer_sample(&buffer, 1234, 166_666).unwrap();
            assert_eq!(sample_bytes(&zero_copy), sample_bytes(&copied));
            assert_eq!(sample_bytes(&zero_copy), pixels);
            assert_eq!(
                zero_copy.GetSampleTime().unwrap(),
                copied.GetSampleTime().unwrap()
            );
            assert_eq!(
                zero_copy.GetSampleDuration().unwrap(),
                copied.GetSampleDuration().unwrap()
            );
            assert_eq!(
                zero_copy.GetTotalLength().unwrap(),
                copied.GetTotalLength().unwrap()
            );
        }
    }

    #[test]
    fn frame_media_buffer_reports_lengths_like_a_memory_buffer() {
        let buffer: IMFMediaBuffer = FrameMediaBuffer::new(vec![5u8; 16].into()).unwrap().into();
        unsafe {
            assert_eq!(buffer.GetMaxLength().unwrap(), 16);
            assert_eq!(buffer.GetCurrentLength().unwrap(), 16);
            buffer.SetCurrentLength(8).unwrap();
            assert_eq!(buffer.GetCurrentLength().unwrap(), 8);
            assert!(buffer.SetCurrentLength(17).is_err());

            let mut ptr: *mut u8 = std::ptr::null_mut();
            let mut max: u32 = 0;
            let mut cur: u32 = 0;
            buffer
                .Lock(&mut ptr, Some(&mut max), Some(&mut cur))
                .unwrap();
            assert_eq!((max, cur), (16, 8));
            assert_eq!(std::slice::from_raw_parts(ptr, 16), &[5u8; 16]);
            buffer.Unlock().unwrap();
        }
    }

    #[test]
    fn a_released_frame_sample_returns_its_buffer_to_the_pool() {
        let _mf = MfSession::start_mta().unwrap();
        let pool = crate::capture::frame_pool::FramePool::new();
        let frame = pool.take(64);
        let frame_ptr = frame.as_ptr();
        let sample = unsafe { make_buffer_sample(&frame_buffer(frame).unwrap(), 0, 1).unwrap() };
        drop(sample);
        assert_eq!(
            pool.take(64).as_ptr(),
            frame_ptr,
            "the sample's buffer should be back in the pool once released"
        );
    }
}
