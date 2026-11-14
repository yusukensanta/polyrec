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
/// reported end-of-stream. `do_remux`'s pure-passthrough copy; `export_pass`
/// has a loop of its own, as it interleaves its merged PCM chunks with the
/// passthrough samples by time, which this shape can't.
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
    let mut done_streams: HashSet<u32> = HashSet::new();
    loop {
        match read_next(reader, ANY_STREAM, label)? {
            Read::EndOfStream(stream) => {
                done_streams.insert(stream);
                if done_streams.len() >= total_streams {
                    break;
                }
            }
            Read::Sample(stream, Some(sample), timestamp) => on_sample(stream, sample, timestamp)?,
            Read::Sample(_, None, _) => {}
        }
    }
    Ok(())
}

/// What one `ReadSample` gave.
pub(crate) enum Read {
    /// From the stream: a sample and its timestamp, or no sample (a gap).
    Sample(u32, Option<IMFSample>, i64),
    EndOfStream(u32),
}

/// Reads `stream` (or `ANY_STREAM`) once. A stream reporting
/// `MF_SOURCE_READERF_ERROR` is an error: the reader delivers nothing after
/// it, and Media Foundation says not to call `ReadSample` again -- read on,
/// it would spin. `label`: see `read_error_context`.
pub(crate) fn read_next(
    reader: &IMFSourceReader,
    stream: u32,
    label: Option<&str>,
) -> Result<Read, AppError> {
    let mut actual = 0u32;
    let mut flags = 0u32;
    let mut timestamp = 0i64;
    let mut sample = None;
    let read = unsafe {
        reader.ReadSample(
            stream,
            0,
            Some(&mut actual),
            Some(&mut flags),
            Some(&mut timestamp),
            Some(&mut sample),
        )
    };
    if let Err(e) = read {
        return Err(e).encode_ctx(read_error_context(label).0);
    }
    if flags & (MF_SOURCE_READERF_ERROR.0 as u32) != 0 {
        let suffix = read_error_context(label).1;
        return Err(AppError::Encode(format!(
            "ReadSample: stream {actual} reported error{suffix}"
        )));
    }
    if flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
        return Ok(Read::EndOfStream(actual));
    }
    Ok(Read::Sample(actual, sample, timestamp))
}

pub fn remux(
    input: &Path,
    output: &Path,
    audio_track_indices: &[usize],
) -> Result<PathBuf, AppError> {
    export_to(input, output, |output| {
        let _mf = MfSession::start_mta()?;
        unsafe { do_remux(input, output, audio_track_indices) }.map(drop)
    })
}

/// Whether `a` and `b` name the same existing file, however they spell it
/// (case, `.` and `..`, a different route to the same folder).
pub fn is_same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Runs `export`, which writes an export of `input` to the path it's given:
/// a temporary file beside `output` (`<name>.tmp.mp4`, so Media Foundation
/// still sees an .mp4), moved into place only once it's complete. An export
/// that fails leaves nothing half-written under the chosen name, and a file
/// the user chose to replace stays as it was.
///
/// Refuses an `output` that is `input` itself: Media Foundation would
/// truncate the recording while still reading it.
fn export_to(
    input: &Path,
    output: &Path,
    export: impl FnOnce(&Path) -> Result<(), AppError>,
) -> Result<PathBuf, AppError> {
    if is_same_file(input, output) {
        return Err(AppError::Encode(format!(
            "can't export {} over itself",
            input.display()
        )));
    }
    let partial = output.with_extension("tmp.mp4");
    export(&partial)
        .and_then(|()| {
            std::fs::rename(&partial, output)
                .encode_ctx(format_args!("moving the export to {}", output.display()))
        })
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&partial);
        })?;
    Ok(output.to_path_buf())
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
    export_to(input, output, |output| {
        let _mf = MfSession::start_mta()?;
        unsafe { do_export_grouped(input, output, groups) }.map(drop)
    })
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
        let label = format!("audio {idx}");
        unsafe {
            loop {
                let (sample, timestamp) = match read_next(&self.reader, self.src_idx, Some(&label))?
                {
                    Read::EndOfStream(_) => return Ok(None),
                    Read::Sample(_, None, _) => continue,
                    Read::Sample(_, Some(sample), timestamp) => (sample, timestamp),
                };
                let buffer = sample
                    .ConvertToContiguousBuffer()
                    .encode_ctx(format_args!("ConvertToContiguousBuffer(audio {idx})"))?;
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut cur_len = 0u32;
                buffer
                    .Lock(&mut data, None, Some(&mut cur_len))
                    .encode_ctx(format_args!("Lock(audio {idx})"))?;
                // An empty buffer may lock to no pointer at all, which
                // `from_raw_parts` mustn't be given.
                let bytes: &[u8] = if cur_len == 0 {
                    &[]
                } else {
                    std::slice::from_raw_parts(data, cur_len as usize)
                };
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
/// at its call site in `export_pass`) and hardware transforms enabled.
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
mod tests;
