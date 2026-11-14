use crate::capture::frame_pool::FrameBuf;
use crate::capture::frame_rate::highest_encodable_fps;
use crate::capture::video::scale_bgra;
use crate::config::Codec;
use crate::encode::cfr::{CfrStats, CfrTimeline};
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
    CODECAPI_AVEncMPVDefaultBPictureCount, CODECAPI_AVEncMPVGOPSize,
    CODECAPI_AVEncVideoForceKeyFrame, ICodecAPI, IMF2DBuffer, IMFMediaBuffer, IMFMediaBuffer_Impl,
    IMFMediaType, IMFSinkWriter, MF_MT_AUDIO_AVG_BYTES_PER_SECOND, MF_MT_AUDIO_BITS_PER_SAMPLE,
    MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND,
    MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SINK_WRITER_D3D_MANAGER,
    MFAudioFormat_AAC, MFAudioFormat_PCM, MFCreateDXGISurfaceBuffer, MFCreateMediaType,
    MFCreateMemoryBuffer, MFCreateSample, MFCreateSinkWriterFromMediaSink,
    MFCreateSinkWriterFromURL, MFMediaType_Audio, MFMediaType_Video, MFVideoFormat_ARGB32,
    MFVideoFormat_H264, MFVideoFormat_HEVC, MFVideoInterlace_Progressive,
};
use windows::Win32::System::Variant::{VARIANT, VT_UI4, VariantClear};
use windows::core::HSTRING;
use windows::core::implement;
use windows::core::{GUID, Interface};

mod frame_buffer;
mod media_types;
mod probe;

use frame_buffer::*;
pub(crate) use media_types::*;
pub(crate) use probe::*;

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

/// Why `RecordingWriter::try_new` failed: the video encoder turned the
/// settings down -- what `ENCODER_ANSWERS` remembers -- or something else
/// did (the file couldn't be created, the audio encoder, the GPU), which
/// says nothing about what the encoder takes.
enum WriterError {
    Rejected(AppError),
    Other(AppError),
}

impl From<AppError> for WriterError {
    fn from(e: AppError) -> Self {
        Self::Other(e)
    }
}

impl From<WriterError> for AppError {
    fn from(e: WriterError) -> Self {
        match e {
            WriterError::Rejected(e) | WriterError::Other(e) => e,
        }
    }
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
    /// The file written -- see `Container`.
    pub container: Container,
}

/// What kind of MP4 `RecordingWriter` writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Container {
    /// Media Foundation's MP4 sink: the file is indexed when it's
    /// finalized, and can't be read at all until then.
    #[default]
    Mp4,
    /// Fragmented, by PolyRec's own sink (see `encode::fmp4`): readable up
    /// to its last fragment, whatever happens to the recording.
    FragmentedMp4,
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
            container: Container::Mp4,
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
    /// The time (in 100 ns units) of the earliest sample written, of
    /// either kind -- see `earliest_written`.
    earliest_written: std::cell::Cell<Option<i64>>,
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
    /// PolyRec's own media sink, writing a fragmented MP4 (see
    /// `Container`). After `writer`, so it shuts down once that's let go,
    /// as the sink writer wants.
    _own_sink: Option<OwnSink>,
    /// Declared last so it drops last -- `writer` must be released before
    /// Media Foundation shuts down (see `MfSession`).
    _mf: MfSession,
}

/// A media sink the sink writer was made from, shut down when dropped:
/// the sink writer doesn't shut down one it was given.
struct OwnSink(windows::Win32::Media::MediaFoundation::IMFMediaSink);

impl Drop for OwnSink {
    fn drop(&mut self) {
        let _ = unsafe { self.0.Shutdown() };
    }
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
    ///
    /// Only the encoder turning a rate down counts, and only once a lower
    /// rate has worked: had none, whatever is wrong (a driver reset, say)
    /// isn't the rate. Anything else -- the file can't be created -- is
    /// returned at once, the same at every rate.
    pub fn new_at_highest_fps(
        output_path: &Path,
        params: &EncoderParams,
    ) -> Result<(Self, u32), AppError> {
        let at = |fps| EncoderParams {
            fps,
            ..params.clone()
        };
        let mut writer = None;
        let mut turned_down = Vec::new();
        let mut failed = None;
        let fps = highest_encodable_fps(params.fps, |fps| {
            let attempt = at(fps);
            let key = encoder_answer_key(&attempt);
            if failed.is_some() || encoder_answers().get(&key) == Some(&false) {
                return false;
            }
            match Self::try_new(output_path, &attempt) {
                Ok(created) => {
                    writer = Some(created);
                    true
                }
                Err(WriterError::Rejected(e)) => {
                    tracing::warn!("encoder rejects {fps} fps ({e}), trying a lower rate");
                    turned_down.push(key);
                    false
                }
                Err(WriterError::Other(e)) => {
                    failed = Some(e);
                    false
                }
            }
        });
        if let Some(e) = failed {
            return Err(e);
        }
        let writer = match writer {
            Some(writer) => writer,
            None => Self::new(output_path, &at(fps))?,
        };
        let mut answers = encoder_answers();
        answers.extend(turned_down.into_iter().map(|key| (key, false)));
        answers.insert(encoder_answer_key(&at(fps)), true);
        Ok((writer, fps))
    }

    pub fn new(output_path: &Path, params: &EncoderParams) -> Result<Self, AppError> {
        Ok(Self::try_new(output_path, params)?)
    }

    /// `new`, telling the video encoder turning `params` down apart from
    /// anything else failing -- see `WriterError`.
    fn try_new(output_path: &Path, params: &EncoderParams) -> Result<Self, WriterError> {
        let EncoderParams {
            width,
            height,
            fps,
            codec,
            bitrate_bps,
            ref audio_tracks,
            allow_hardware_encode,
            ref gpu,
            container,
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
            let own_sink = match container {
                Container::Mp4 => None,
                Container::FragmentedMp4 => {
                    Some(OwnSink(crate::encode::fmp4::sink::create(output_path)?))
                }
            };
            let writer: IMFSinkWriter = match &own_sink {
                None => MFCreateSinkWriterFromURL(&url, None, Some(&attrs))
                    .encode_ctx("MFCreateSinkWriterFromURL")?,
                Some(sink) => MFCreateSinkWriterFromMediaSink(&sink.0, Some(&attrs))
                    .encode_ctx("MFCreateSinkWriterFromMediaSink")?,
            };

            let requested_subtype = match codec {
                Codec::H265 => MFVideoFormat_HEVC,
                Codec::H264 => MFVideoFormat_H264,
            };
            let video_out =
                make_video_output_type(width, height, fps, requested_subtype, bitrate_bps)?;
            // From here to the video stream's input type, a failure is the
            // video encoder turning these settings down.
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
                        .encode_ctx("AddStream video (H264 fallback)")
                        .map_err(WriterError::Rejected)?
                }
                Err(e) => {
                    return Err(WriterError::Rejected(AppError::Encode(format!(
                        "AddStream video: {e}"
                    ))));
                }
            };
            let (in_width, in_height) = direct.map_or((width, height), |gpu| gpu.size);
            let video_in = make_video_input_type(in_width, in_height, fps)?;
            writer
                .SetInputMediaType(video_stream, &video_in, None)
                .encode_ctx("SetInputMediaType video")
                .map_err(WriterError::Rejected)?;
            let codec_api = video_codec_api(&writer, video_stream);
            let gop_set = codec_api.as_ref().is_some_and(|api| set_gop_size(api, fps));
            // Asked of every encoder: a Highlight save copies its segments'
            // frames as they are (see `highlight_export::decode_time`).
            let b_frames = codec_api.as_ref().map_or(0, b_frames);
            if container == Container::FragmentedMp4 && b_frames > 0 {
                // The fragments take the frames in the order the encoder
                // hands them out as their order in time (see
                // `fmp4::muxer`), which B-frames break. Media Foundation's
                // own MP4 sink sorts them out: the recording plays right,
                // but isn't crash-safe.
                tracing::warn!(
                    "the video encoder uses B-frames: recording to a plain MP4, not crash-safe"
                );
                drop(writer);
                drop(own_sink);
                return Self::try_new(
                    output_path,
                    &EncoderParams {
                        container: Container::Mp4,
                        ..params.clone()
                    },
                );
            }

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
                earliest_written: std::cell::Cell::new(None),
                codec_api,
                keyframes: std::cell::RefCell::new(KeyframeSchedule::new(gop_set.then_some(fps))),
                input_size: (in_width, in_height),
                takes_textures: std::cell::Cell::new(direct.is_some()),
                readback,
                _own_sink: own_sink,
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
        let written = match picture {
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
        };
        self.note_written(written, pts_hns)
    }

    /// Notes that a sample of time `pts_hns` was `written`, if it was.
    fn note_written(&self, written: Result<(), AppError>, pts_hns: i64) -> Result<(), AppError> {
        if written.is_ok() {
            let earliest = self
                .earliest_written
                .get()
                .map_or(pts_hns, |t| t.min(pts_hns));
            self.earliest_written.set(Some(earliest));
        }
        written
    }

    /// The time of the earliest sample written, of either kind, whichever
    /// was written first: Media Foundation's MP4 sink moves every sample
    /// back by it, putting it at the file's 0 (see `highlight::segment_base`).
    pub(crate) fn earliest_written(&self) -> Option<std::time::Duration> {
        let hns = self.earliest_written.get()?;
        Some(std::time::Duration::from_nanos(hns.max(0) as u64 * 100))
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
        let duration_hns = samples.duration().as_nanos() as i64 / 100;

        let pcm: Vec<u8> = samples
            .samples
            .iter()
            .flat_map(|&s| {
                let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                v.to_le_bytes()
            })
            .collect();

        let written = unsafe {
            let sample = make_sample(&pcm, pts_hns, duration_hns)?;
            self.writer
                .WriteSample(stream_idx, &sample)
                .encode_ctx(format_args!("WriteSample audio[{track_idx}]"))
        };
        self.note_written(written, pts_hns)
    }

    /// How steady the video has been so far -- see `CfrStats`.
    pub(crate) fn video_stats(&self) -> CfrStats {
        self.timeline.borrow().stats()
    }

    /// Starts the video at slot `slot` of its frame grid rather than 0,
    /// before any frame is written -- see `CfrTimeline::start_at`.
    pub(crate) fn start_video_at(&self, slot: u64) {
        self.timeline.borrow_mut().start_at(slot);
    }

    /// The slot after the video's last frame, once one has been written.
    pub(crate) fn video_end(&self) -> Option<u64> {
        self.timeline.borrow().end()
    }

    pub fn finalize(self) -> Result<PathBuf, AppError> {
        unsafe {
            self.writer.Finalize().encode_ctx("Finalize")?;
        }
        Ok(self.output_path)
    }
}

#[cfg(test)]
pub(crate) mod tests;
