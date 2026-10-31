use crate::capture::audio::{TARGET_CHANNELS, TARGET_SAMPLE_RATE};
use crate::encode::mf::MfSession;
use crate::encode::writer::{make_audio_input_type, make_audio_output_type, make_sample};
use crate::error::{AppError, ResultExt};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaType, IMFSample, IMFSinkWriter, IMFSourceReader,
    MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND,
    MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS,
    MF_SINK_WRITER_DISABLE_THROTTLING, MF_SINK_WRITER_STATISTICS, MF_SOURCE_READER_ALL_STREAMS,
    MF_SOURCE_READER_ANY_STREAM, MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READERF_ERROR,
    MFAudioFormat_PCM, MFCreateAttributes, MFCreateMediaType, MFCreateSinkWriterFromURL,
    MFCreateSourceReaderFromURL, MFMediaType_Audio, MFMediaType_Video,
};
use windows::core::HSTRING;

/// Media Foundation's sink writer does not guarantee output track order matches
/// `AddStream` call order — it can depend on which encoder actually flushes first.
/// Never assume "stream 0 = video, 1.. = audio"; always find streams by major type.
#[derive(Clone)]
pub(crate) struct StreamLayout {
    pub(crate) video: u32,
    pub(crate) audio: Vec<u32>,
}

pub(crate) unsafe fn discover_stream_layout(
    reader: &IMFSourceReader,
) -> Result<StreamLayout, AppError> {
    unsafe {
        let mut video = None;
        let mut audio = Vec::new();
        for i in 0.. {
            let t: IMFMediaType = match reader.GetNativeMediaType(i, 0) {
                Ok(t) => t,
                Err(_) => break,
            };
            let major = t
                .GetGUID(&MF_MT_MAJOR_TYPE)
                .encode_ctx(format_args!("GetGUID major_type (stream {i})"))?;
            if major == MFMediaType_Video {
                video = video.or(Some(i));
            } else if major == MFMediaType_Audio {
                audio.push(i);
            }
        }
        let video =
            video.ok_or_else(|| AppError::Encode("no video stream found in input".into()))?;
        Ok(StreamLayout { video, audio })
    }
}

/// Builds an `IMFSinkWriter` for `output_url`, adds `video_type` as one stream
/// and each of `audio_types` as a subsequent stream (compressed passthrough --
/// same media type in and out, no transcoding), then calls `BeginWriting`.
/// Shared by `remux` and `highlight_export::concat_and_trim`, which otherwise
/// use the returned sink stream ids differently: `remux` maps them back to
/// source stream indices for its interleaved copy loop, `concat_and_trim`
/// just needs them in the same order as `audio_types` to write each segment's
/// samples in turn.
pub(crate) unsafe fn build_passthrough_sink_writer(
    output_url: &HSTRING,
    video_type: &IMFMediaType,
    audio_types: &[IMFMediaType],
) -> Result<(IMFSinkWriter, u32, Vec<u32>), AppError> {
    unsafe {
        let writer: IMFSinkWriter = MFCreateSinkWriterFromURL(output_url, None, None)
            .encode_ctx("MFCreateSinkWriterFromURL")?;

        let video_sink = writer
            .AddStream(video_type)
            .encode_ctx("AddStream(video)")?;
        writer
            .SetInputMediaType(video_sink, video_type, None)
            .encode_ctx("SetInputMediaType(video)")?;

        let mut audio_sinks = Vec::with_capacity(audio_types.len());
        for (i, audio_type) in audio_types.iter().enumerate() {
            let asink = writer
                .AddStream(audio_type)
                .encode_ctx(format_args!("AddStream(audio[{i}])"))?;
            writer
                .SetInputMediaType(asink, audio_type, None)
                .encode_ctx(format_args!("SetInputMediaType(audio[{i}])"))?;
            audio_sinks.push(asink);
        }

        writer.BeginWriting().encode_ctx("BeginWriting")?;

        Ok((writer, video_sink, audio_sinks))
    }
}

/// `MF_SOURCE_READER_ANY_STREAM` / `MF_SOURCE_READER_ALL_STREAMS` as the
/// `u32` stream index `ReadSample`/`SetStreamSelection` take (windows-rs
/// exposes them as typed `i32` constants; both are 0xFFFFFFFE).
pub(crate) const ANY_STREAM: u32 = MF_SOURCE_READER_ANY_STREAM.0 as u32;
pub(crate) const ALL_STREAMS: u32 = MF_SOURCE_READER_ALL_STREAMS.0 as u32;

/// Copies compressed samples from every stream `reader` has selected into
/// `writer`, mapping each source stream index to its sink stream index via
/// `source_to_sink`, until all `total_streams` enabled source streams have
/// reported end-of-stream. Shared by `do_remux`'s pure-passthrough copy and
/// `do_export_grouped`'s video+passthrough-audio portion (its separately
/// merged/transcoded audio groups are written afterward, in their own loop
/// -- they're PCM chunks, not samples read from a reader, so they don't fit
/// this shape).
unsafe fn copy_passthrough_samples(
    reader: &IMFSourceReader,
    writer: &IMFSinkWriter,
    source_to_sink: &HashMap<u32, u32>,
    total_streams: usize,
) -> Result<(), AppError> {
    unsafe {
        for_each_sample(reader, total_streams, None, |stream, sample, _timestamp| {
            if let Some(&sink_idx) = source_to_sink.get(&stream) {
                writer
                    .WriteSample(sink_idx, &sample)
                    .encode_ctx(format_args!("WriteSample(stream {stream})"))?;
            }
            Ok(())
        })
    }
}

/// `for_each_sample`'s `ReadSample` failure context and the suffix of its
/// "stream N reported error" message, for `label`.
fn read_error_context(label: Option<&str>) -> (String, String) {
    match label {
        None => ("ReadSample".to_string(), String::new()),
        Some(label) => (format!("ReadSample({label})"), format!(" in {label}")),
    }
}

/// Reads every stream `reader` has selected (`ANY_STREAM`) until all
/// `total_streams` of them have reported end-of-stream, handing each
/// delivered sample to `on_sample` with its source stream index and
/// timestamp (ticks without a sample are skipped). A stream reporting
/// `MF_SOURCE_READERF_ERROR` fails the whole read. `label` names the input
/// in error messages ("ReadSample(<label>)", "... reported error in
/// <label>"); `None` when there's only one obvious input.
///
/// Shared by `copy_passthrough_samples` (samples copied as-is) and
/// `highlight_export::concat_and_trim`'s per-segment copy (timestamps
/// rebased across segment seams) -- the read/EOS/error handling is the
/// same, only what happens to each sample differs.
pub(crate) unsafe fn for_each_sample(
    reader: &IMFSourceReader,
    total_streams: usize,
    label: Option<&str>,
    mut on_sample: impl FnMut(u32, IMFSample, i64) -> Result<(), AppError>,
) -> Result<(), AppError> {
    let (read_ctx, error_suffix) = read_error_context(label);
    unsafe {
        let mut done_streams: HashSet<u32> = HashSet::new();
        loop {
            let mut actual_idx: u32 = 0;
            let mut stream_flags: u32 = 0;
            let mut timestamp: i64 = 0;
            let mut sample: Option<IMFSample> = None;

            reader
                .ReadSample(
                    ANY_STREAM,
                    0,
                    Some(&mut actual_idx),
                    Some(&mut stream_flags),
                    Some(&mut timestamp),
                    Some(&mut sample),
                )
                .encode_ctx(&read_ctx)?;

            if stream_flags & (MF_SOURCE_READERF_ERROR.0 as u32) != 0 {
                return Err(AppError::Encode(format!(
                    "ReadSample: stream {actual_idx} reported error{error_suffix}"
                )));
            }

            if stream_flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
                done_streams.insert(actual_idx);
                if done_streams.len() >= total_streams {
                    break;
                }
                continue;
            }

            if let Some(sample) = sample {
                on_sample(actual_idx, sample, timestamp)?;
            }
        }
        Ok(())
    }
}

pub fn remux(
    input: &Path,
    output: &Path,
    audio_track_indices: &[usize],
) -> Result<PathBuf, AppError> {
    let _mf = MfSession::start_mta()?;
    unsafe { do_remux(input, output, audio_track_indices) }
}

/// How many audio streams a finished recording actually contains -- read
/// directly from the file itself rather than trusted from whatever devices
/// were selected before recording started, since a device selected at record
/// time isn't a guarantee it actually produced a stream in the output (e.g. a
/// device disconnecting mid-recording). The export UI uses this to build its
/// track-selection checkboxes and to decide whether exporting is even
/// meaningful (nothing to select between with fewer than two tracks).
pub fn count_audio_tracks(path: &Path) -> Result<usize, AppError> {
    let _mf = MfSession::start_mta()?;
    unsafe {
        let url = HSTRING::from(path);
        let reader: IMFSourceReader =
            MFCreateSourceReaderFromURL(&url, None).encode_ctx("MFCreateSourceReaderFromURL")?;
        Ok(discover_stream_layout(&reader)?.audio.len())
    }
}

/// Like `remux`, but decodes every selected audio track to PCM and sums
/// them into a single mixed track instead of keeping each as its own
/// stream -- for platforms (e.g. YouTube) that only ever play one audio
/// track from an uploaded file and silently ignore the rest. Video is
/// still a lossless compressed passthrough copy, same as `remux`.
///
/// A thin wrapper over `export_grouped` with everything selected in one
/// group.
pub fn mix_tracks(
    input: &Path,
    output: &Path,
    audio_track_indices: &[usize],
) -> Result<PathBuf, AppError> {
    export_grouped(input, output, &[audio_track_indices.to_vec()])
}

/// Produces one output audio track per entry in `groups` -- a group with a
/// single index is a lossless compressed passthrough copy (like `remux`);
/// a group with 2+ indices gets those specific tracks decoded to PCM,
/// summed into one, and re-encoded (like `mix_tracks`, but scoped to just
/// that group instead of everything selected). Lets e.g. a multi-process
/// app's several tracks collapse into one "app" track while a separately
/// selected device stays its own independent track in the same export,
/// without requiring the caller to choose one all-or-nothing mixing mode
/// for the whole file.
///
/// Video is always a lossless compressed passthrough copy, same as
/// `remux`/`mix_tracks`.
pub fn export_grouped(
    input: &Path,
    output: &Path,
    groups: &[Vec<usize>],
) -> Result<PathBuf, AppError> {
    if groups.iter().all(|g| g.len() <= 1) {
        let flat: Vec<usize> = groups.iter().flatten().copied().collect();
        return remux(input, output, &flat);
    }
    let _mf = MfSession::start_mta()?;
    unsafe { do_export_grouped(input, output, groups) }
}

/// Frames mixed (and written) at a time: 100 ms at `TARGET_SAMPLE_RATE`.
const MIX_CHUNK_FRAMES: usize = 4800;

/// A piece of a track's PCM: the frame it starts at, and its interleaved
/// samples -- see `PcmPieces`.
type PcmPiece = (usize, Vec<i16>);

/// One audio track of a merge group as 16-bit interleaved PCM, a piece at a
/// time and in order -- see `TrackDecoder`.
trait PcmPieces {
    /// The next piece: the frame its first sample belongs at, and its
    /// samples. Only the first piece's frame counts; the rest follow on from
    /// it. `None` at the end of the track.
    fn next_piece(&mut self) -> Result<Option<PcmPiece>, AppError>;
}

/// A track being mixed: what it has decoded and not mixed yet, and the frame
/// that starts at -- so a track is held a piece at a time, not whole.
struct MixInput<P> {
    pieces: P,
    pending: VecDeque<i16>,
    pending_from: usize,
    started: bool,
    ended: bool,
}

impl<P: PcmPieces> MixInput<P> {
    fn new(pieces: P) -> Self {
        Self {
            pieces,
            pending: VecDeque::new(),
            pending_from: 0,
            started: false,
            ended: false,
        }
    }

    /// Reads until the track has a frame of its own, or has ended without
    /// one -- so a group of tracks with nothing in them is known before
    /// anything is written (see `open_merge_groups`).
    fn prime(&mut self, channels: usize) -> Result<(), AppError> {
        self.fill_to(1, channels)
    }

    /// The frame just past what the track has decoded so far.
    fn end_frame(&self, channels: usize) -> usize {
        self.pending_from + self.pending.len() / channels
    }

    /// Reads until what it has decoded reaches `frame_end`, or it ends.
    fn fill_to(&mut self, frame_end: usize, channels: usize) -> Result<(), AppError> {
        while !self.ended && self.end_frame(channels) < frame_end {
            match self.pieces.next_piece()? {
                Some((at, samples)) => {
                    if !self.started {
                        self.started = true;
                        self.pending_from = at;
                    }
                    self.pending.extend(samples);
                }
                None => self.ended = true,
            }
        }
        Ok(())
    }

    /// Adds its samples for the frames `acc` covers, from `from` on, into
    /// `acc`, and lets go of everything before the end of those.
    fn mix_into(&mut self, acc: &mut [i32], from: usize, channels: usize) {
        let to = from + acc.len() / channels;
        let begin = self.pending_from.max(from);
        let end = self.end_frame(channels).min(to);
        if begin < end {
            let src = self.pending.range(
                (begin - self.pending_from) * channels..(end - self.pending_from) * channels,
            );
            for (sum, &sample) in acc[(begin - from) * channels..(end - from) * channels]
                .iter_mut()
                .zip(src)
            {
                *sum += i32::from(sample);
            }
        }
        let done = to
            .saturating_sub(self.pending_from)
            .min(self.pending.len() / channels);
        self.pending.drain(..done * channels);
        self.pending_from += done;
    }
}

/// A merge group's tracks (primed), mixed chunk by chunk from frame 0: the
/// same sum as adding up the whole decoded tracks, each aligned by where it
/// starts -- so tracks that began capturing at different moments (e.g.
/// process-loopback's async activation delay) still line up -- running as
/// far as whichever track reaches furthest, while holding only a chunk or so
/// of each.
struct Mix<P> {
    inputs: Vec<MixInput<P>>,
    channels: usize,
    /// See `scale_for`.
    scale: f64,
    /// The frame the next chunk starts at.
    pos: usize,
    /// The unscaled sum's peak so far.
    peak: u32,
}

impl<P: PcmPieces> Mix<P> {
    fn new(inputs: Vec<MixInput<P>>, channels: usize, scale: f64) -> Self {
        Self {
            inputs,
            channels,
            scale,
            pos: 0,
            peak: 0,
        }
    }

    /// The next chunk, scaled, and the frame it starts at; `None` once
    /// every track has run out.
    fn next_chunk(&mut self) -> Result<Option<(usize, Vec<i16>)>, AppError> {
        let channels = self.channels;
        let chunk_end = self.pos + MIX_CHUNK_FRAMES;
        for input in &mut self.inputs {
            input.fill_to(chunk_end, channels)?;
        }
        // A track that hasn't ended reaches `chunk_end` now; once they all
        // have, the mix ends with the one that reaches furthest.
        let reach = self
            .inputs
            .iter()
            .map(|input| input.end_frame(channels))
            .max()
            .unwrap_or(0);
        let end = reach.min(chunk_end);
        if end <= self.pos {
            return Ok(None);
        }
        let mut acc = vec![0i32; (end - self.pos) * channels];
        for input in &mut self.inputs {
            input.mix_into(&mut acc, self.pos, channels);
        }
        self.peak = self
            .peak
            .max(acc.iter().map(|v| v.unsigned_abs()).max().unwrap_or(0));
        let chunk = acc.iter().map(|&v| scaled(v, self.scale)).collect();
        let at = self.pos;
        self.pos = end;
        Ok(Some((at, chunk)))
    }

    /// Whether the sum has gone past full scale so far, so the mix has to be
    /// made again scaled down (see `scale_for`).
    fn clipped(&self) -> bool {
        self.peak > i16::MAX as u32
    }
}

/// The gain a merged track is written at, given its sum's `peak`: unity
/// unless the sum would clip, then just enough to bring the peak to full
/// scale -- so tracks that never overlap loudly (the common case) don't lose
/// volume for no reason.
fn scale_for(peak: u32) -> f64 {
    if peak > i16::MAX as u32 {
        i16::MAX as f64 / peak as f64
    } else {
        1.0
    }
}

fn scaled(sample: i32, scale: f64) -> i16 {
    ((sample as f64) * scale)
        .round()
        .clamp(i16::MIN as f64, i16::MAX as f64) as i16
}

/// Decodes one audio track (by its logical index into `layout.audio`) to
/// PCM, forcing the same target rate/channels this app always encodes at
/// so a group's tracks line up frame for frame without a separate resample
/// step. Hands it out a source-reader sample at a time (`PcmPieces`), with
/// the frame each starts at -- the first one's lets `Mix` align
/// tracks that started capturing at different moments. Used on the thread
/// it was opened on (see `DecodingThread`).
struct TrackDecoder {
    reader: IMFSourceReader,
    src_idx: u32,
    idx: usize,
    rate: u32,
}

impl TrackDecoder {
    unsafe fn open(
        input_url: &HSTRING,
        layout: &StreamLayout,
        idx: usize,
        mix_rate: u32,
        mix_channels: usize,
    ) -> Result<Self, AppError> {
        unsafe {
            let src_idx = *layout.audio.get(idx).ok_or_else(|| {
                AppError::Encode(format!("no audio track at logical index {idx}"))
            })?;

            let reader: IMFSourceReader = MFCreateSourceReaderFromURL(input_url, None)
                .encode_ctx(format_args!("MFCreateSourceReaderFromURL audio[{idx}]"))?;
            reader
                .SetStreamSelection(ALL_STREAMS, false)
                .encode_ctx("SetStreamSelection(all,false)")?;
            reader
                .SetStreamSelection(src_idx, true)
                .encode_ctx(format_args!("SetStreamSelection(audio {idx})"))?;

            let pcm_type = MFCreateMediaType().encode_ctx("MFCreateMediaType")?;
            pcm_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
                .encode_ctx("SetGUID major_type")?;
            pcm_type
                .SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)
                .encode_ctx("SetGUID PCM")?;
            pcm_type
                .SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, mix_rate)
                .encode_ctx("SetUINT32 rate")?;
            pcm_type
                .SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, mix_channels as u32)
                .encode_ctx("SetUINT32 channels")?;
            pcm_type
                .SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)
                .encode_ctx("SetUINT32 bits")?;
            reader
                .SetCurrentMediaType(src_idx, None, &pcm_type)
                .encode_ctx(format_args!("SetCurrentMediaType(audio {idx})"))?;

            Ok(Self {
                reader,
                src_idx,
                idx,
                rate: mix_rate,
            })
        }
    }
}

impl PcmPieces for TrackDecoder {
    fn next_piece(&mut self) -> Result<Option<PcmPiece>, AppError> {
        let idx = self.idx;
        unsafe {
            loop {
                let mut actual_idx: u32 = 0;
                let mut stream_flags: u32 = 0;
                let mut timestamp: i64 = 0;
                let mut sample: Option<IMFSample> = None;
                self.reader
                    .ReadSample(
                        self.src_idx,
                        0,
                        Some(&mut actual_idx),
                        Some(&mut stream_flags),
                        Some(&mut timestamp),
                        Some(&mut sample),
                    )
                    .encode_ctx(format_args!("ReadSample(audio {idx})"))?;

                if stream_flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
                    return Ok(None);
                }
                let Some(sample) = sample else { continue };
                let buffer = sample
                    .ConvertToContiguousBuffer()
                    .encode_ctx(format_args!("ConvertToContiguousBuffer(audio {idx})"))?;
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut cur_len = 0u32;
                buffer
                    .Lock(&mut data, None, Some(&mut cur_len))
                    .encode_ctx(format_args!("Lock(audio {idx})"))?;
                let bytes = std::slice::from_raw_parts(data, cur_len as usize);
                let samples: Vec<i16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&chunk| i16::from_le_bytes(chunk))
                    .collect();
                buffer
                    .Unlock()
                    .encode_ctx(format_args!("Unlock(audio {idx})"))?;

                let at = (timestamp * self.rate as i64 / 10_000_000).max(0) as usize;
                return Ok(Some((at, samples)));
            }
        }
    }
}

/// A group of 1 (or 0) index needs no decode/mix -- passthrough-copy it
/// exactly like `remux` does. Only groups of 2+ need the PCM decode-and-sum
/// treatment. Returns (passthrough track indices, merge groups), each in
/// `groups` order.
fn split_export_groups(groups: &[Vec<usize>]) -> (Vec<usize>, Vec<&Vec<usize>>) {
    let mut passthrough_indices: Vec<usize> = Vec::new();
    let mut merge_groups: Vec<&Vec<usize>> = Vec::new();
    for g in groups {
        if g.len() <= 1 {
            passthrough_indices.extend(g.iter().copied());
        } else {
            merge_groups.push(g);
        }
    }
    (passthrough_indices, merge_groups)
}

/// How many decoded pieces a `DecodingThread` may get ahead of the mix by
/// -- a few thousand frames each, so a fraction of a second of audio.
const DECODE_AHEAD: usize = 8;

/// A track decoded on a thread of its own (`TrackDecoder`) and handed over a
/// piece at a time through a short queue: a merge group's tracks decode side
/// by side, and alongside the mixing and encoding, rather than one after
/// another. Decoding is most of an export's work, so this roughly halves a
/// three-track mix's time. The thread stops at the end of the track, or as
/// soon as this is dropped.
struct DecodingThread {
    pieces: std::sync::mpsc::Receiver<Result<Option<PcmPiece>, AppError>>,
}

impl DecodingThread {
    fn spawn(
        input_url: &HSTRING,
        layout: &StreamLayout,
        idx: usize,
        mix_rate: u32,
        mix_channels: usize,
    ) -> Result<Self, AppError> {
        let (tx, pieces) = std::sync::mpsc::sync_channel(DECODE_AHEAD);
        let (input_url, layout) = (input_url.clone(), layout.clone());
        std::thread::Builder::new()
            .name(format!("ExportDecode[{idx}]"))
            .spawn(move || {
                let decoder = MfSession::start_mta().and_then(|mf| {
                    let decoder = unsafe {
                        TrackDecoder::open(&input_url, &layout, idx, mix_rate, mix_channels)?
                    };
                    Ok((mf, decoder))
                });
                let (_mf, mut decoder) = match decoder {
                    Ok(opened) => opened,
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                };
                loop {
                    let piece = decoder.next_piece();
                    let last = !matches!(piece, Ok(Some(_)));
                    if tx.send(piece).is_err() || last {
                        return;
                    }
                }
            })
            .map_err(|e| AppError::Encode(format!("starting a decode thread: {e}")))?;
        Ok(Self { pieces })
    }
}

impl PcmPieces for DecodingThread {
    fn next_piece(&mut self) -> Result<Option<PcmPiece>, AppError> {
        self.pieces
            .recv()
            .unwrap_or_else(|_| Err(AppError::Encode("a decode thread stopped early".into())))
    }
}

/// Opens each merge group's tracks for mixing (`Mix`), each
/// decoding on a thread of its own. A group
/// that turns out entirely empty (e.g. every process in it was silent the
/// whole recording) is dropped rather than becoming a zero-length audio
/// stream.
fn open_merge_groups(
    input_url: &HSTRING,
    layout: &StreamLayout,
    merge_groups: &[&Vec<usize>],
    mix_rate: u32,
    mix_channels: usize,
) -> Result<Vec<Vec<MixInput<DecodingThread>>>, AppError> {
    let mut opened = Vec::new();
    for group in merge_groups {
        let mut inputs: Vec<MixInput<DecodingThread>> = group
            .iter()
            .map(|&idx| {
                DecodingThread::spawn(input_url, layout, idx, mix_rate, mix_channels)
                    .map(MixInput::new)
            })
            .collect::<Result<_, _>>()?;
        for input in &mut inputs {
            input.prime(mix_channels)?;
        }
        if inputs.iter().any(|input| input.end_frame(mix_channels) > 0) {
            opened.push(inputs);
        }
    }
    Ok(opened)
}

/// How much of a merged track may wait in the sink writer for its encoder.
/// The writer here is unthrottled (see `create_unthrottled_sink_writer`), so
/// it takes samples as fast as they come -- faster than the AAC encoder turns
/// them over, with decoding on threads of its own -- and held the rest of the
/// track's PCM meanwhile: about 190 KB a second of it, 2 GB for a 3-hour mix.
const MAX_QUEUED_BYTES: u32 = 4 * 1024 * 1024;

/// How long `wait_for_encoder` waits for a queue that isn't going down
/// before going on anyway: it's only there to bound memory.
const ENCODER_STALL: std::time::Duration = std::time::Duration::from_secs(2);

/// Waits while more than `MAX_QUEUED_BYTES` of `stream`'s samples are queued
/// in `writer`. Best-effort: without the writer's statistics, or with a queue
/// that stops going down, it goes on.
unsafe fn wait_for_encoder(writer: &IMFSinkWriter, stream: u32) {
    let mut last = (u32::MAX, std::time::Instant::now());
    loop {
        let mut stats = MF_SINK_WRITER_STATISTICS {
            cb: std::mem::size_of::<MF_SINK_WRITER_STATISTICS>() as u32,
            ..Default::default()
        };
        if unsafe { writer.GetStatistics(stream, &mut stats) }.is_err()
            || stats.dwByteCountQueued <= MAX_QUEUED_BYTES
        {
            return;
        }
        if stats.dwByteCountQueued < last.0 {
            last = (stats.dwByteCountQueued, std::time::Instant::now());
        } else if last.1.elapsed() >= ENCODER_STALL {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// A sink writer with `MF_SINK_WRITER_DISABLE_THROTTLING` (see the comment
/// at `do_export_grouped`'s call site) and hardware transforms enabled.
unsafe fn create_unthrottled_sink_writer(output_url: &HSTRING) -> Result<IMFSinkWriter, AppError> {
    unsafe {
        let mut attrs_opt: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs_opt, 2).encode_ctx("MFCreateAttributes")?;
        let attrs =
            attrs_opt.ok_or_else(|| AppError::Encode("MFCreateAttributes returned None".into()))?;
        attrs
            .SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)
            .encode_ctx("SetUINT32 disable_throttling")?;
        attrs
            .SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)
            .encode_ctx("SetUINT32 hw_transforms")?;
        MFCreateSinkWriterFromURL(output_url, None, Some(&attrs))
            .encode_ctx("MFCreateSinkWriterFromURL")
    }
}

/// Mixes each merge group at unity gain, tracking its peak; if a mix would
/// clip, the export is made again with that mix scaled to fit
/// (`scale_for`) -- the same output as summing the whole decoded tracks in
/// memory first, which is what this used to do, at the cost of decoding
/// twice, but only for a mix that clips. Holding whole tracks took about
/// 1 GB per quarter hour of a three-track mix, and running out of memory
/// on a long recording aborted PolyRec.
unsafe fn do_export_grouped(
    input: &Path,
    output: &Path,
    groups: &[Vec<usize>],
) -> Result<PathBuf, AppError> {
    unsafe {
        let peaks = export_pass(input, output, groups, None)?;
        if peaks.iter().any(|&peak| peak > i16::MAX as u32) {
            let scales: Vec<f64> = peaks.iter().map(|&peak| scale_for(peak)).collect();
            export_pass(input, output, groups, Some(&scales))?;
        }
        Ok(output.to_path_buf())
    }
}

/// One go at `do_export_grouped`'s output, each merged track scaled by its
/// entry in `scales` (unity for `None`). Returns each merged track's peak,
/// unscaled, in order.
unsafe fn export_pass(
    input: &Path,
    output: &Path,
    groups: &[Vec<usize>],
    scales: Option<&[f64]>,
) -> Result<Vec<u32>, AppError> {
    unsafe {
        let input_url = HSTRING::from(input);
        let output_url = HSTRING::from(output);

        let probe_reader: IMFSourceReader = MFCreateSourceReaderFromURL(&input_url, None)
            .encode_ctx("MFCreateSourceReaderFromURL probe")?;
        let layout = discover_stream_layout(&probe_reader)?;
        let video_type: IMFMediaType = probe_reader
            .GetNativeMediaType(layout.video, 0)
            .encode_ctx("GetNativeMediaType(video)")?;

        let mix_rate = TARGET_SAMPLE_RATE;
        let mix_channels = TARGET_CHANNELS as usize;

        let (passthrough_indices, merge_groups) = split_export_groups(groups);
        let merged = open_merge_groups(&input_url, &layout, &merge_groups, mix_rate, mix_channels)?;

        if passthrough_indices.is_empty() && merged.is_empty() {
            do_remux(input, output, &[])?;
            return Ok(Vec::new());
        }

        // ── Sink writer: video passthrough + one stream per passthrough
        // index (compressed passthrough, like `remux`) + one stream per
        // non-empty merge group (transcoded PCM->AAC, like the old
        // `mix_tracks`). See the throttling comment below -- applies
        // globally to the writer, so it's set whenever any transcoding is
        // involved at all, harmless for the passthrough streams sharing it.
        //
        // Unlike `do_remux`'s pure compressed-passthrough sink writer (no
        // encoder MFT involved, so this never bites it), a writer with any
        // transcoded stream defaults to throttling WriteSample to real-time
        // pace, on the assumption the caller is a live recording. For this
        // offline batch job that meant writing a mixed track took as long
        // as the recording itself (confirmed against a real ~5.5min file:
        // decode + mix finished in 1.3s, then the write loop alone ran past
        // a minute with ~0s of CPU time consumed in that window -- sleeping,
        // not computing). Disabling it here writes as fast as the CPU can go.
        let writer = create_unthrottled_sink_writer(&output_url)?;

        let video_sink = writer
            .AddStream(&video_type)
            .encode_ctx("AddStream(video)")?;
        writer
            .SetInputMediaType(video_sink, &video_type, None)
            .encode_ctx("SetInputMediaType(video)")?;

        let mut passthrough_sinks: Vec<(u32, u32)> = Vec::new(); // (src_idx, sink_idx)
        for &idx in &passthrough_indices {
            let src_idx = *layout.audio.get(idx).ok_or_else(|| {
                AppError::Encode(format!("no audio track at logical index {idx}"))
            })?;
            let native_type: IMFMediaType = probe_reader
                .GetNativeMediaType(src_idx, 0)
                .encode_ctx(format_args!("GetNativeMediaType(audio {idx})"))?;
            let sink_idx = writer
                .AddStream(&native_type)
                .encode_ctx(format_args!("AddStream(audio {idx})"))?;
            writer
                .SetInputMediaType(sink_idx, &native_type, None)
                .encode_ctx(format_args!("SetInputMediaType(audio {idx})"))?;
            passthrough_sinks.push((src_idx, sink_idx));
        }

        let mut merge_sinks: Vec<u32> = Vec::new();
        for _ in &merged {
            let audio_out = make_audio_output_type(mix_rate, mix_channels as u16)?;
            let audio_in = make_audio_input_type(mix_rate, mix_channels as u16)?;
            let sink_idx = writer
                .AddStream(&audio_out)
                .encode_ctx("AddStream(merged audio)")?;
            writer
                .SetInputMediaType(sink_idx, &audio_in, None)
                .encode_ctx("SetInputMediaType(merged audio)")?;
            merge_sinks.push(sink_idx);
        }

        writer.BeginWriting().encode_ctx("BeginWriting")?;

        // Video + passthrough audio: interleaved compressed-copy loop,
        // same technique as `do_remux`.
        probe_reader
            .SetStreamSelection(ALL_STREAMS, false)
            .encode_ctx("SetStreamSelection(all,false)")?;
        probe_reader
            .SetStreamSelection(layout.video, true)
            .encode_ctx("SetStreamSelection(video)")?;
        for &(src_idx, _) in &passthrough_sinks {
            probe_reader
                .SetStreamSelection(src_idx, true)
                .encode_ctx("SetStreamSelection(audio)")?;
        }
        let mut source_to_sink: HashMap<u32, u32> = HashMap::new();
        source_to_sink.insert(layout.video, video_sink);
        for &(src_idx, sink_idx) in &passthrough_sinks {
            source_to_sink.insert(src_idx, sink_idx);
        }
        let total_enabled = 1 + passthrough_sinks.len();

        // The merged tracks are written as the passthrough copy goes, each up
        // to the time of the sample about to be copied: written after the
        // whole copy instead, the MP4 sink held all of the video in memory
        // to interleave it with audio that hadn't come yet -- the size of
        // the recording.
        let mut mixes: Vec<MergedTrack> = merged
            .into_iter()
            .zip(merge_sinks)
            .enumerate()
            .map(|(i, (inputs, sink))| MergedTrack {
                mix: Mix::new(inputs, mix_channels, scales.map_or(1.0, |scales| scales[i])),
                sink,
                next: None,
            })
            .collect();
        // Once a mix has clipped on the unity-gain pass, this pass's file
        // won't be kept (see `do_export_grouped`): writing stops, and it
        // only goes on to find every mix's peak.
        let writing =
            |mixes: &[MergedTrack]| scales.is_some() || !mixes.iter().any(|m| m.mix.clipped());
        for_each_sample(
            &probe_reader,
            total_enabled,
            None,
            |stream, sample, timestamp| {
                let write = writing(&mixes);
                write_merged_until(&writer, &mut mixes, Some(timestamp), write, mix_rate)?;
                if writing(&mixes)
                    && let Some(&sink_idx) = source_to_sink.get(&stream)
                {
                    writer
                        .WriteSample(sink_idx, &sample)
                        .encode_ctx(format_args!("WriteSample(stream {stream})"))?;
                }
                Ok(())
            },
        )?;
        let write = writing(&mixes);
        write_merged_until(&writer, &mut mixes, None, write, mix_rate)?;

        if writing(&mixes) {
            writer.Finalize().encode_ctx("Finalize")?;
        }
        Ok(mixes.iter().map(|m| m.mix.peak).collect())
    }
}

/// A merged track being written: its mix, its stream in the sink writer, and
/// the chunk mixed but not written yet.
struct MergedTrack {
    mix: Mix<DecodingThread>,
    sink: u32,
    next: Option<(usize, Vec<i16>)>,
}

/// Writes each merged track's chunks that start before `until` (in 100 ns
/// units; `None`: to the end), mixing them as it goes -- or, unless
/// `writing`, just mixes them, for their peaks.
unsafe fn write_merged_until(
    writer: &IMFSinkWriter,
    mixes: &mut [MergedTrack],
    until: Option<i64>,
    writing: bool,
    mix_rate: u32,
) -> Result<(), AppError> {
    let to_hns = |frames: usize| frames as i64 * 10_000_000 / i64::from(mix_rate);
    for merged in mixes {
        loop {
            if merged.next.is_none() {
                merged.next = merged.mix.next_chunk()?;
            }
            let Some((frame, chunk)) = &merged.next else {
                break;
            };
            let pts_hns = to_hns(*frame);
            if until.is_some_and(|until| pts_hns >= until) {
                break;
            }
            if writing {
                let chunk_bytes: Vec<u8> = chunk.iter().flat_map(|s| s.to_le_bytes()).collect();
                let duration_hns = to_hns(chunk.len() / merged.mix.channels);
                unsafe {
                    let sample = make_sample(&chunk_bytes, pts_hns, duration_hns)?;
                    writer
                        .WriteSample(merged.sink, &sample)
                        .encode_ctx("WriteSample(merged audio)")?;
                    wait_for_encoder(writer, merged.sink);
                }
            }
            merged.next = None;
        }
    }
    Ok(())
}

unsafe fn do_remux(
    input: &Path,
    output: &Path,
    audio_track_indices: &[usize],
) -> Result<PathBuf, AppError> {
    unsafe {
        let input_url = HSTRING::from(input);
        let output_url = HSTRING::from(output);

        // ── Source reader ────────────────────────────────────────────────────────
        let reader: IMFSourceReader = MFCreateSourceReaderFromURL(&input_url, None)
            .encode_ctx("MFCreateSourceReaderFromURL")?;

        let layout = discover_stream_layout(&reader)?;

        // Disable all streams, then re-enable desired ones
        reader
            .SetStreamSelection(ALL_STREAMS, false)
            .encode_ctx("SetStreamSelection(all,false)")?;
        reader
            .SetStreamSelection(layout.video, true)
            .encode_ctx("SetStreamSelection(video)")?;
        // audio_track_indices are 0-based logical indices into the discovered audio
        // stream list (in the order they appear in the source), NOT raw stream indices —
        // the sink writer does not guarantee AddStream order matches output track order.
        for &idx in audio_track_indices {
            let src_idx = *layout.audio.get(idx).ok_or_else(|| {
                AppError::Encode(format!("no audio track at logical index {idx}"))
            })?;
            reader
                .SetStreamSelection(src_idx, true)
                .encode_ctx(format_args!("SetStreamSelection(audio {idx})"))?;
        }

        // Configure each enabled stream for compressed passthrough.
        // GetNativeMediaType(stream, 0) returns the compressed type.
        // SetCurrentMediaType with that same type tells the reader to emit
        // compressed bytes without decoding.
        let video_type: IMFMediaType = reader
            .GetNativeMediaType(layout.video, 0)
            .encode_ctx("GetNativeMediaType(video)")?;
        reader
            .SetCurrentMediaType(layout.video, None, &video_type)
            .encode_ctx("SetCurrentMediaType(video)")?;

        let mut audio_types: Vec<(u32, IMFMediaType)> = Vec::new();
        for &idx in audio_track_indices {
            let src_idx = layout.audio[idx];
            let t: IMFMediaType = reader
                .GetNativeMediaType(src_idx, 0)
                .encode_ctx(format_args!("GetNativeMediaType(audio {idx})"))?;
            reader
                .SetCurrentMediaType(src_idx, None, &t)
                .encode_ctx(format_args!("SetCurrentMediaType(audio {idx})"))?;
            audio_types.push((src_idx, t));
        }

        // ── Sink writer ──────────────────────────────────────────────────────────
        let audio_sink_types: Vec<IMFMediaType> =
            audio_types.iter().map(|(_, t)| t.clone()).collect();
        let (writer, vsink, audio_sinks) =
            build_passthrough_sink_writer(&output_url, &video_type, &audio_sink_types)?;

        // source stream index → sink stream index
        let mut source_to_sink: HashMap<u32, u32> = HashMap::new();
        source_to_sink.insert(layout.video, vsink);
        for ((src_idx, _), asink) in audio_types.iter().zip(audio_sinks.iter()) {
            source_to_sink.insert(*src_idx, *asink);
        }

        // ── Read / write loop ────────────────────────────────────────────────────
        let total_enabled = 1 + audio_track_indices.len();
        copy_passthrough_samples(&reader, &writer, &source_to_sink, total_enabled)?;

        writer.Finalize().encode_ctx("Finalize")?;

        Ok(output.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::Codec;
    use crate::encode::{EncoderParams, RecordingWriter};
    use crate::types::{AudioSamples, TrackId, VideoFrame};
    use std::time::Duration;

    fn make_test_mp4(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("source.mp4");
        let writer = RecordingWriter::new(
            &path,
            &EncoderParams::tiny_test(Codec::H264, &[(48000u32, 2u16)]),
        )
        .expect("RecordingWriter::new");
        writer.begin_writing().expect("begin_writing");
        writer
            .write_video(VideoFrame {
                pts: Duration::ZERO,
                data: vec![0u8; 64 * 64 * 4].into(),
            })
            .expect("write_video");
        writer
            .write_audio(
                0,
                AudioSamples {
                    track_id: TrackId::new(0),
                    pts: Duration::ZERO,
                    samples: vec![0.0f32; 480 * 2],
                    sample_rate: 48000,
                    channels: 2,
                },
            )
            .expect("write_audio");
        writer.finalize().expect("finalize")
    }

    fn make_two_track_test_mp4(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("two_track_source.mp4");
        let writer = RecordingWriter::new(
            &path,
            &EncoderParams::tiny_test(Codec::H264, &[(48000u32, 2u16), (48000u32, 2u16)]),
        )
        .expect("RecordingWriter::new");
        writer.begin_writing().expect("begin_writing");
        writer
            .write_video(VideoFrame {
                pts: Duration::ZERO,
                data: vec![0u8; 64 * 64 * 4].into(),
            })
            .expect("write_video");
        for track_idx in 0..2 {
            writer
                .write_audio(
                    track_idx,
                    AudioSamples {
                        track_id: TrackId::new(track_idx as u32),
                        pts: Duration::ZERO,
                        samples: vec![0.1f32; 480 * 2],
                        sample_rate: 48000,
                        channels: 2,
                    },
                )
                .expect("write_audio");
        }
        writer.finalize().expect("finalize")
    }

    fn make_three_track_test_mp4(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("three_track_source.mp4");
        let writer = RecordingWriter::new(
            &path,
            &EncoderParams::tiny_test(
                Codec::H264,
                &[(48000u32, 2u16), (48000u32, 2u16), (48000u32, 2u16)],
            ),
        )
        .expect("RecordingWriter::new");
        writer.begin_writing().expect("begin_writing");
        writer
            .write_video(VideoFrame {
                pts: Duration::ZERO,
                data: vec![0u8; 64 * 64 * 4].into(),
            })
            .expect("write_video");
        for track_idx in 0..3 {
            writer
                .write_audio(
                    track_idx,
                    AudioSamples {
                        track_id: TrackId::new(track_idx as u32),
                        pts: Duration::ZERO,
                        samples: vec![0.1f32; 480 * 2],
                        sample_rate: 48000,
                        channels: 2,
                    },
                )
                .expect("write_audio");
        }
        writer.finalize().expect("finalize")
    }

    #[test]
    fn export_grouped_merges_within_a_group_but_keeps_groups_separate() {
        let dir = tempfile::tempdir().unwrap();
        let source = make_three_track_test_mp4(dir.path());
        let dest = dir.path().join("grouped.mp4");
        // Tracks 0 and 1 (e.g. a multi-process app) merge into one output
        // track; track 2 (e.g. the mic) stays its own separate track.
        let result = export_grouped(&source, &dest, &[vec![0, 1], vec![2]]);
        assert!(result.is_ok(), "export_grouped failed: {:?}", result.err());
        assert_eq!(
            count_audio_tracks(&dest).expect("count_audio_tracks failed"),
            2,
            "expected one merged track + one separate track"
        );
    }

    #[test]
    fn export_grouped_with_only_single_track_groups_behaves_like_remux() {
        let dir = tempfile::tempdir().unwrap();
        let source = make_three_track_test_mp4(dir.path());
        let dest = dir.path().join("all_separate.mp4");
        let result = export_grouped(&source, &dest, &[vec![0], vec![1], vec![2]]);
        assert!(result.is_ok(), "export_grouped failed: {:?}", result.err());
        assert_eq!(
            count_audio_tracks(&dest).expect("count_audio_tracks failed"),
            3,
            "no group has 2+ indices, so nothing should get merged"
        );
    }

    #[test]
    fn mix_tracks_with_two_selected_produces_exactly_one_audio_track() {
        let dir = tempfile::tempdir().unwrap();
        let source = make_two_track_test_mp4(dir.path());
        let dest = dir.path().join("mixed.mp4");
        let result = mix_tracks(&source, &dest, &[0, 1]);
        assert!(result.is_ok(), "mix_tracks failed: {:?}", result.err());
        assert!(dest.exists(), "output file not created");
        assert!(dest.metadata().unwrap().len() > 0, "output file is empty");
        assert_eq!(
            count_audio_tracks(&dest).expect("count_audio_tracks failed"),
            1,
            "mixed output should have exactly one audio track"
        );
    }

    /// Regression test for a real bug: `mix_tracks`'s sink writer left
    /// `MF_SINK_WRITER_DISABLE_THROTTLING` unset, so `IMFSinkWriter`
    /// defaulted to pacing `WriteSample` to real-time speed (its assumption
    /// for any pipeline with an active encoder MFT, which the mixed audio
    /// track always has). A real ~5.5min recording took 15+ minutes and
    /// never finished before this was found and fixed -- decode+mix alone
    /// took 1.3s, confirming the write loop itself was the bottleneck.
    /// Bounded by a timeout thread (not an inline call) so a real
    /// regression fails fast instead of hanging the test run.
    #[test]
    fn mix_tracks_does_not_throttle_to_real_time_on_a_longer_recording() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("longer_two_track_source.mp4");
        let writer = RecordingWriter::new(
            &path,
            &EncoderParams::tiny_test(Codec::H264, &[(48000u32, 2u16), (48000u32, 2u16)]),
        )
        .expect("RecordingWriter::new");
        writer.begin_writing().expect("begin_writing");
        writer
            .write_video(VideoFrame {
                pts: Duration::ZERO,
                data: vec![0u8; 64 * 64 * 4].into(),
            })
            .expect("write_video");
        // ~3s of audio per track -- long enough that real-time throttling
        // (if it regresses) makes this an actually slow test, not just a
        // theoretical concern.
        const CHUNKS: u64 = 30;
        for chunk in 0..CHUNKS {
            let pts = Duration::from_millis(chunk * 100);
            for track_idx in 0..2 {
                writer
                    .write_audio(
                        track_idx,
                        AudioSamples {
                            track_id: TrackId::new(track_idx as u32),
                            pts,
                            samples: vec![0.1f32; 4800 * 2],
                            sample_rate: 48000,
                            channels: 2,
                        },
                    )
                    .expect("write_audio");
            }
        }
        let source = writer.finalize().expect("finalize");

        let dest = dir.path().join("mixed_longer.mp4");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let start = std::time::Instant::now();
            let result = mix_tracks(&source, &dest, &[0, 1]);
            let _ = tx.send((result, start.elapsed()));
        });
        let (result, elapsed) = rx
            .recv_timeout(std::time::Duration::from_secs(20))
            .expect("mix_tracks did not complete within 20s -- real-time throttling regression?");
        result.expect("mix_tracks failed");
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "mix_tracks took {elapsed:.2?} for a ~3s recording -- should be near-instant, not throttled to real time"
        );
    }

    #[test]
    fn mix_tracks_with_fewer_than_two_selected_delegates_to_remux() {
        let dir = tempfile::tempdir().unwrap();
        let source = make_two_track_test_mp4(dir.path());

        let one_track_dest = dir.path().join("one_track.mp4");
        mix_tracks(&source, &one_track_dest, &[0]).expect("mix_tracks with 1 index failed");
        assert_eq!(
            count_audio_tracks(&one_track_dest).expect("count_audio_tracks failed"),
            1
        );

        let no_track_dest = dir.path().join("no_track.mp4");
        mix_tracks(&source, &no_track_dest, &[]).expect("mix_tracks with 0 indices failed");
        assert_eq!(
            count_audio_tracks(&no_track_dest).expect("count_audio_tracks failed"),
            0
        );
    }

    /// A track's pieces, from memory.
    struct FakePieces(VecDeque<(usize, Vec<i16>)>);

    impl PcmPieces for FakePieces {
        fn next_piece(&mut self) -> Result<Option<PcmPiece>, AppError> {
            Ok(self.0.pop_front())
        }
    }

    /// Mixes `tracks` (each a list of pieces) the way an export does: at
    /// unity gain, and again scaled if that clipped.
    fn mix_whole(tracks: &[Vec<(usize, Vec<i16>)>], channels: usize) -> Vec<i16> {
        let run = |scale: f64| {
            let inputs: Vec<MixInput<FakePieces>> = tracks
                .iter()
                .map(|pieces| {
                    let mut input = MixInput::new(FakePieces(pieces.clone().into()));
                    input.prime(channels).unwrap();
                    input
                })
                .collect();
            let mut mix = Mix::new(inputs, channels, scale);
            let mut out = Vec::new();
            while let Some((frame, chunk)) = mix.next_chunk().unwrap() {
                assert_eq!(frame * channels, out.len(), "chunks aren't contiguous");
                out.extend_from_slice(&chunk);
            }
            (mix.peak, out)
        };
        let (peak, out) = run(1.0);
        if peak > i16::MAX as u32 {
            run(scale_for(peak)).1
        } else {
            out
        }
    }

    /// How a merged track was made before `Mix`: every track
    /// decoded whole, summed (each aligned by its start), then scaled down
    /// to full scale only if the sum would clip. The streaming mix has to
    /// come out the same, sample for sample.
    fn reference_mix(tracks: &[(usize, Vec<i16>)], channels: usize) -> Vec<i16> {
        let mut acc: Vec<i32> = Vec::new();
        for (frame_offset, samples) in tracks {
            let start = frame_offset * channels;
            let needed = start + samples.len();
            if acc.len() < needed {
                acc.resize(needed, 0);
            }
            for (i, &s) in samples.iter().enumerate() {
                acc[start + i] += s as i32;
            }
        }
        let peak = acc.iter().map(|&v| v.unsigned_abs()).max().unwrap_or(0);
        acc.iter().map(|&v| scaled(v, scale_for(peak))).collect()
    }

    fn whole(offset: usize, samples: Vec<i16>) -> Vec<(usize, Vec<i16>)> {
        vec![(offset, samples)]
    }

    #[test]
    fn mix_sums_overlapping_tracks_at_unity_gain_when_no_clipping() {
        let tracks = vec![
            whole(0, vec![1000i16, -1000, 500, -500]),
            whole(0, vec![2000i16, -2000, 300, -300]),
        ];
        assert_eq!(mix_whole(&tracks, 2), vec![3000, -3000, 800, -800]);
    }

    #[test]
    fn mix_aligns_tracks_by_frame_offset() {
        // Track 1 starts at frame 0 (2 stereo frames = 4 samples), track 2
        // starts 1 frame later (offset 1 -> sample index 2) -- they should
        // only overlap on the second frame of track 1 / first frame of track 2.
        let tracks = vec![
            whole(0, vec![100i16, 100, 200, 200]),
            whole(1, vec![50i16, 50]),
        ];
        assert_eq!(mix_whole(&tracks, 2), vec![100, 100, 250, 250]);
    }

    #[test]
    fn mix_scales_down_only_when_sum_would_clip() {
        let tracks = vec![whole(0, vec![30000i16]), whole(0, vec![30000i16])];
        // 30000 + 30000 = 60000, clips i16 (max 32767) -- must be scaled
        // down to exactly the max, not left clipped/wrapped.
        assert_eq!(mix_whole(&tracks, 1), vec![i16::MAX]);
    }

    #[test]
    fn mix_of_nothing_is_nothing() {
        assert!(mix_whole(&[], 2).is_empty());
        assert!(mix_whole(&[vec![], whole(0, vec![])], 2).is_empty());
    }

    /// The streaming mix against the whole-track one it replaced, over
    /// tracks of every shape: several, starting late, ending early, longer
    /// than many chunks, split into pieces of every size (including empty
    /// ones), loud enough to clip or not.
    #[test]
    fn mix_matches_summing_whole_tracks_sample_for_sample() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = |below: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % below.max(1)
        };
        let channels = 2;
        for case in 0..200 {
            let track_count = next(4) as usize + 1;
            let loud = case % 3 == 0;
            let mut whole_tracks = Vec::new();
            let mut piece_tracks = Vec::new();
            for _ in 0..track_count {
                let offset = if next(3) == 0 {
                    0
                } else {
                    next(3 * MIX_CHUNK_FRAMES as u64) as usize
                };
                let frames = next(4 * MIX_CHUNK_FRAMES as u64) as usize;
                let amplitude = if loud { 30000 } else { 9000 };
                let samples: Vec<i16> = (0..frames * channels)
                    .map(|_| (next(2 * amplitude) as i64 - amplitude as i64) as i16)
                    .collect();
                let mut pieces = Vec::new();
                let mut at = 0;
                while at < samples.len() || pieces.is_empty() {
                    let len = (next(3000) as usize * channels).min(samples.len() - at);
                    // Only the first piece's frame counts.
                    let frame = if pieces.is_empty() {
                        offset
                    } else {
                        next(1_000_000) as usize
                    };
                    pieces.push((frame, samples[at..at + len].to_vec()));
                    at += len;
                }
                whole_tracks.push((offset, samples));
                piece_tracks.push(pieces);
            }
            assert_eq!(
                mix_whole(&piece_tracks, channels),
                reference_mix(&whole_tracks, channels),
                "case {case}"
            );
        }
    }

    /// A mix that would clip is made again scaled to fit (see
    /// `do_export_grouped`), not written hard-clipped: two copies of a loud
    /// tone sum to nearly twice full scale. Clipped, most of each cycle
    /// would sit at full scale; scaled, only its crests come near it.
    #[test]
    fn mixing_tracks_that_would_clip_scales_them_to_fit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loud.mp4");
        let writer = RecordingWriter::new(
            &path,
            &EncoderParams::tiny_test(Codec::H264, &[(48000u32, 2u16), (48000u32, 2u16)]),
        )
        .expect("RecordingWriter::new");
        writer.begin_writing().expect("begin_writing");
        writer
            .write_video(VideoFrame {
                pts: Duration::ZERO,
                data: vec![0u8; 64 * 64 * 4].into(),
            })
            .expect("write_video");
        for chunk in 0..10u32 {
            for track in 0..2 {
                let samples = (0..4800 * 2)
                    .map(|i| ((chunk * 4800 + i / 2) as f32 * 0.05).sin() * 0.9)
                    .collect();
                writer
                    .write_audio(
                        track,
                        AudioSamples {
                            track_id: TrackId::new(track as u32),
                            pts: Duration::from_millis(u64::from(chunk) * 100),
                            samples,
                            sample_rate: 48000,
                            channels: 2,
                        },
                    )
                    .expect("write_audio");
            }
        }
        let source = writer.finalize().expect("finalize");
        let dest = dir.path().join("mixed.mp4");
        mix_tracks(&source, &dest, &[0, 1]).expect("mix_tracks");

        let _mf = MfSession::start_mta().unwrap();
        let url = HSTRING::from(dest.as_path());
        let reader: IMFSourceReader = unsafe { MFCreateSourceReaderFromURL(&url, None).unwrap() };
        let layout = unsafe { discover_stream_layout(&reader).unwrap() };
        let mut decoder = unsafe { TrackDecoder::open(&url, &layout, 0, 48000, 2).unwrap() };
        let mut samples = Vec::new();
        while let Some((_, piece)) = decoder.next_piece().unwrap() {
            samples.extend(piece);
        }
        let near_full_scale = samples.iter().filter(|s| s.unsigned_abs() > 31000).count();
        let share = near_full_scale as f64 / samples.len() as f64;
        assert!(
            samples.len() > 40_000,
            "too little audio: {}",
            samples.len()
        );
        assert!(
            share < 0.25,
            "{:.0}% of samples near full scale: clipped",
            share * 100.0
        );
        let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(peak > 26_000, "scaled down too far: peak {peak}");
    }

    #[test]
    fn remux_video_only_creates_output_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = make_test_mp4(dir.path());
        let dest = dir.path().join("video_only.mp4");
        let result = remux(&source, &dest, &[]);
        assert!(result.is_ok(), "remux failed: {:?}", result.err());
        assert!(dest.exists(), "output file not created");
        assert!(dest.metadata().unwrap().len() > 0, "output file is empty");
    }

    #[test]
    fn remux_with_audio_track_creates_output_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = make_test_mp4(dir.path());
        let dest = dir.path().join("with_audio.mp4");
        let result = remux(&source, &dest, &[0]);
        assert!(result.is_ok(), "remux failed: {:?}", result.err());
        assert!(dest.exists(), "output file not created");
        assert!(dest.metadata().unwrap().len() > 0, "output file is empty");
    }

    #[test]
    fn count_audio_tracks_finds_the_single_audio_track() {
        let dir = tempfile::tempdir().unwrap();
        let source = make_test_mp4(dir.path());
        assert_eq!(
            count_audio_tracks(&source).expect("count_audio_tracks failed"),
            1
        );
    }

    #[test]
    fn count_audio_tracks_is_zero_for_a_video_only_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("video_only_source.mp4");
        let writer = RecordingWriter::new(&path, &EncoderParams::tiny_test(Codec::H264, &[]))
            .expect("RecordingWriter::new");
        writer.begin_writing().expect("begin_writing");
        // Several frames, not one -- a single WriteSample followed immediately
        // by Finalize occasionally raced Media Foundation's async software
        // encoder on CI's (virtualized, no real GPU) runner: Finalize decided
        // the stream had processed zero samples (MF_E_SINK_NO_SAMPLES_PROCESSED,
        // 0xC00D4A44) because the lone sample hadn't reached the encoder MFT
        // yet. A real recording never hits this -- it always spans real
        // wall-clock time across many frames -- so this only ever showed up in
        // this exact "write once, finalize immediately" test shape.
        for i in 0..3u32 {
            writer
                .write_video(VideoFrame {
                    pts: Duration::from_millis(33 * i as u64),
                    data: vec![0u8; 64 * 64 * 4].into(),
                })
                .expect("write_video");
        }
        let path = writer.finalize().expect("finalize");

        assert_eq!(
            count_audio_tracks(&path).expect("count_audio_tracks failed"),
            0
        );
    }

    #[test]
    fn read_error_context_matches_the_messages_each_caller_used_to_build() {
        // copy_passthrough_samples' former literals:
        let (ctx, suffix) = read_error_context(None);
        assert_eq!(ctx, "ReadSample");
        assert_eq!(
            format!("ReadSample: stream 3 reported error{suffix}"),
            "ReadSample: stream 3 reported error"
        );
        // concat_and_trim's former format strings:
        let (ctx, suffix) = read_error_context(Some(r"segment C:\seg_0.mp4"));
        assert_eq!(ctx, r"ReadSample(segment C:\seg_0.mp4)");
        assert_eq!(
            format!("ReadSample: stream 3 reported error{suffix}"),
            r"ReadSample: stream 3 reported error in segment C:\seg_0.mp4"
        );
    }

    /// Opens `path` with every stream selected, the way `do_remux` does.
    unsafe fn open_all_streams(path: &std::path::Path) -> (IMFSourceReader, StreamLayout) {
        unsafe {
            let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path), None).unwrap();
            reader.SetStreamSelection(ALL_STREAMS, true).unwrap();
            let layout = discover_stream_layout(&reader).unwrap();
            (reader, layout)
        }
    }

    #[test]
    fn for_each_sample_delivers_every_stream_until_all_reach_end_of_stream() {
        let _mf = MfSession::start_mta().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = make_test_mp4(dir.path());
        unsafe {
            let (reader, layout) = open_all_streams(&path);
            let mut per_stream: HashMap<u32, usize> = HashMap::new();
            for_each_sample(&reader, 1 + layout.audio.len(), None, |stream, _s, _ts| {
                *per_stream.entry(stream).or_default() += 1;
                Ok(())
            })
            .unwrap();
            assert!(per_stream.get(&layout.video).copied().unwrap_or(0) >= 1);
            assert!(per_stream.get(&layout.audio[0]).copied().unwrap_or(0) >= 1);
            assert_eq!(per_stream.len(), 2, "only the two streams in the file");
        }
    }

    #[test]
    fn for_each_sample_stops_at_the_first_callback_error() {
        let _mf = MfSession::start_mta().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = make_test_mp4(dir.path());
        unsafe {
            let (reader, layout) = open_all_streams(&path);
            let mut calls = 0;
            let result = for_each_sample(&reader, 1 + layout.audio.len(), None, |_, _, _| {
                calls += 1;
                Err(AppError::Encode("stop".into()))
            });
            assert!(matches!(result, Err(AppError::Encode(msg)) if msg == "stop"));
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn split_export_groups_passes_singletons_through_and_merges_the_rest() {
        let groups = vec![vec![0], vec![1, 2], vec![], vec![3], vec![4, 5, 6]];
        let (passthrough, merge) = split_export_groups(&groups);
        assert_eq!(passthrough, vec![0, 3]);
        assert_eq!(merge, vec![&vec![1, 2], &vec![4, 5, 6]]);
    }
}
