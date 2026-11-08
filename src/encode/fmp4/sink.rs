//! The Media Foundation media sink that hands the sink writer's encoded
//! samples to the `Muxer` -- the place of the MP4 sink
//! `MFCreateSinkWriterFromURL` would load. A pull-model sink (see "Media
//! Sinks" in the Media Foundation docs): each stream asks for a sample
//! (`MEStreamSinkRequestSample`) and gets it in `ProcessSample`; markers
//! are answered once everything before them is in; finalizing writes the
//! last fragment.
//!
//! Nothing is written until the first sample comes: the video stream's
//! configuration (its parameter sets) goes in the file's header, and the
//! encoder reports it with its media type, or failing that in its first
//! keyframe.

use super::boxes::{TrackInfo, TrackKind};
use super::muxer::{Muxer, Sample};
use super::nal::{self, VideoCodec};
use crate::error::{AppError, ResultExt};
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use windows::Win32::Foundation::{E_FAIL, S_OK};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::core::{
    ComObject, GUID, HRESULT, IUnknown, IUnknownImpl, Interface, OutRef, Ref, Result, implement,
};

/// A media sink writing a fragmented MP4 to `path`, created now. Until it's
/// finished the file is shared for reading only: nothing else -- another
/// copy of PolyRec recovering recordings it takes for cut off, say -- can
/// delete it or write to it.
pub fn create(path: &Path) -> std::result::Result<IMFMediaSink, AppError> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;

    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(path)
        .encode_ctx(format_args!("creating {}", path.display()))?;
    let sink: IMFFinalizableMediaSink = Fmp4Sink {
        streams: Mutex::new(Vec::new()),
        clock: Mutex::new(None),
        shared: Arc::new(Mutex::new(Shared {
            kinds: Vec::new(),
            file: Some(file),
            held: Vec::new(),
            writing: None,
            shut_down: false,
        })),
    }
    .into();
    sink.cast().encode_ctx("IMFMediaSink")
}

/// What every stream of the sink shares.
struct Shared {
    /// What each stream holds, from its media type, in the order they
    /// were added. (The media types themselves stay with the streams.)
    kinds: Vec<Option<Kind>>,
    /// The file, until the muxer takes it.
    file: Option<File>,
    /// Samples that came before the file's header could be written.
    held: Vec<(usize, Sample)>,
    writing: Option<Writing>,
    shut_down: bool,
}

struct Writing {
    muxer: Muxer<BufWriter<File>>,
    video: usize,
    codec: VideoCodec,
    /// In the header already: left out of the samples.
    parameter_sets: Vec<Vec<u8>>,
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn failed(what: impl std::fmt::Display) -> windows::core::Error {
    windows::core::Error::new(E_FAIL, what.to_string())
}

impl Shared {
    /// Takes stream `index`'s next sample.
    fn take(&mut self, index: usize, sample: Sample) -> Result<()> {
        if self.writing.is_none() {
            self.held.push((index, sample));
            if !self.start()? {
                return Ok(());
            }
            for (index, sample) in std::mem::take(&mut self.held) {
                self.push(index, sample)?;
            }
            return Ok(());
        }
        self.push(index, sample)
    }

    fn push(&mut self, index: usize, mut sample: Sample) -> Result<()> {
        let writing = self.writing.as_mut().ok_or_else(|| failed("not writing"))?;
        if index == writing.video {
            sample.data =
                nal::to_length_prefixed(writing.codec, &sample.data, &writing.parameter_sets);
        }
        writing.muxer.push(index, sample).map_err(failed)
    }

    /// Writes the file's header, once the video's configuration is known:
    /// whether it has been.
    fn start(&mut self) -> Result<bool> {
        let mut tracks = Vec::with_capacity(self.kinds.len());
        let mut video = None;
        for (index, kind) in self.kinds.iter().enumerate() {
            let kind = kind.clone().ok_or_else(|| failed("a stream has no type"))?;
            let id = index as u32 + 1;
            let track = match kind {
                Kind::Video {
                    codec,
                    width,
                    height,
                    header,
                } => {
                    // The encoder's report, or its first keyframe's sets.
                    let first_frame = self.held.iter().find(|(i, _)| *i == index);
                    let Some(header) = header.or_else(|| first_frame.map(|(_, s)| s.data.clone()))
                    else {
                        return Ok(false);
                    };
                    let (config, sets) =
                        nal::configuration_record(codec, &header).map_err(failed)?;
                    video = Some((index, codec, sets));
                    TrackInfo {
                        id,
                        timescale: 90_000,
                        kind: TrackKind::Video {
                            codec,
                            width,
                            height,
                            config,
                        },
                    }
                }
                Kind::Audio {
                    sample_rate,
                    channels,
                    audio_specific_config,
                } => TrackInfo {
                    id,
                    timescale: sample_rate,
                    kind: TrackKind::Audio {
                        channels,
                        sample_rate,
                        audio_specific_config,
                    },
                },
            };
            tracks.push(track);
        }
        let (video, codec, parameter_sets) = video.ok_or_else(|| failed("no video stream"))?;
        let file = self.file.take().ok_or_else(|| failed("no file"))?;
        let muxer = Muxer::new(BufWriter::with_capacity(1 << 20, file), tracks).map_err(failed)?;
        self.writing = Some(Writing {
            muxer,
            video,
            codec,
            parameter_sets,
        });
        Ok(true)
    }

    /// Writes what's left, and closes the file.
    fn finish(&mut self) -> Result<()> {
        let writing = self.writing.take().ok_or(MF_E_SINK_NO_SAMPLES_PROCESSED)?;
        let out = writing.muxer.finish().map_err(failed)?;
        out.into_inner().map_err(|e| failed(e.error()))?;
        Ok(())
    }
}

/// What a stream holds, from its media type.
#[derive(Clone)]
enum Kind {
    Video {
        codec: VideoCodec,
        width: u16,
        height: u16,
        /// The encoder's parameter sets, if it reported them.
        header: Option<Vec<u8>>,
    },
    Audio {
        sample_rate: u32,
        channels: u16,
        audio_specific_config: Vec<u8>,
    },
}

impl Kind {
    /// The stream's kind once `newer` is its type, from `old`: the size it
    /// was added with stays. The encoder's own type has its coded size --
    /// rounded up to whole 16-pixel blocks, 1408 rows for 1392 -- which the
    /// file's tracks would then say; the size asked for is what Media
    /// Foundation's MP4 sink writes too.
    fn updated(old: Option<&Kind>, newer: Kind) -> Kind {
        match (old, newer) {
            (Some(&Kind::Video { width, height, .. }), Kind::Video { codec, header, .. }) => {
                Kind::Video {
                    codec,
                    width,
                    height,
                    header,
                }
            }
            (_, newer) => newer,
        }
    }
}

fn track_kind(media_type: &IMFMediaType) -> std::result::Result<Kind, String> {
    unsafe {
        let blob = |key: &GUID| -> Option<Vec<u8>> {
            let size = media_type.GetBlobSize(key).ok()?;
            let mut data = vec![0; size as usize];
            media_type.GetBlob(key, &mut data, None).ok()?;
            Some(data)
        };
        let subtype = media_type
            .GetGUID(&MF_MT_SUBTYPE)
            .map_err(|e| e.to_string())?;
        if subtype == MFVideoFormat_H264 || subtype == MFVideoFormat_HEVC {
            let size = media_type
                .GetUINT64(&MF_MT_FRAME_SIZE)
                .map_err(|e| e.to_string())?;
            Ok(Kind::Video {
                codec: if subtype == MFVideoFormat_HEVC {
                    VideoCodec::Hevc
                } else {
                    VideoCodec::H264
                },
                width: (size >> 32) as u16,
                height: size as u16,
                header: blob(&MF_MT_MPEG_SEQUENCE_HEADER).filter(|h| !h.is_empty()),
            })
        } else if subtype == MFAudioFormat_AAC {
            // HEAACWAVEINFO's 12 bytes after WAVEFORMATEX, then the
            // AudioSpecificConfig.
            let user_data = blob(&MF_MT_USER_DATA).unwrap_or_default();
            let audio_specific_config = user_data.get(12..).unwrap_or_default().to_vec();
            if audio_specific_config.is_empty() {
                return Err("AAC without its AudioSpecificConfig".into());
            }
            Ok(Kind::Audio {
                sample_rate: media_type
                    .GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)
                    .map_err(|e| e.to_string())?,
                channels: media_type
                    .GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)
                    .map_err(|e| e.to_string())? as u16,
                audio_specific_config,
            })
        } else {
            Err(format!("can't write a {subtype:?} stream"))
        }
    }
}

#[implement(IMFFinalizableMediaSink, IMFClockStateSink)]
struct Fmp4Sink {
    streams: Mutex<Vec<ComObject<Fmp4Stream>>>,
    clock: Mutex<Option<IMFPresentationClock>>,
    shared: Arc<Mutex<Shared>>,
}

impl Fmp4Sink {
    /// Tells every stream `event`, then asks for a sample if `request`.
    fn tell_streams(&self, event: MF_EVENT_TYPE, request: bool) -> Result<()> {
        for stream in self.streams.lock().unwrap().iter() {
            stream.queue_event(event)?;
            if request {
                stream.queue_event(MEStreamSinkRequestSample)?;
            }
        }
        Ok(())
    }

    fn check_running(&self) -> Result<()> {
        if lock(&self.shared).shut_down {
            return Err(MF_E_SHUTDOWN.into());
        }
        Ok(())
    }
}

impl IMFMediaSink_Impl for Fmp4Sink_Impl {
    fn GetCharacteristics(&self) -> Result<u32> {
        self.check_running()?;
        Ok(MEDIASINK_RATELESS)
    }

    fn AddStreamSink(&self, id: u32, media_type: Ref<IMFMediaType>) -> Result<IMFStreamSink> {
        self.check_running()?;
        let mut streams = self.streams.lock().unwrap();
        if streams.iter().any(|s| s.id == id) {
            return Err(MF_E_STREAMSINK_EXISTS.into());
        }
        // The type the stream is added with may not be all there yet --
        // AAC's lacks its AudioSpecificConfig until the encoder sets the
        // stream's type (`SetCurrentMediaType`).
        let kind = media_type.as_ref().and_then(|t| track_kind(t).ok());
        let index = {
            let mut shared = lock(&self.shared);
            if shared.writing.is_some() {
                return Err(MF_E_STREAMSINKS_FIXED.into());
            }
            shared.kinds.push(kind);
            shared.kinds.len() - 1
        };
        let me: IMFMediaSink = self.to_interface::<IMFFinalizableMediaSink>().cast()?;
        let stream = ComObject::new(Fmp4Stream {
            id,
            index,
            shared: Arc::clone(&self.shared),
            queue: unsafe { MFCreateEventQueue()? },
            media_type: Mutex::new(media_type.cloned()),
            sink: Mutex::new(Some(me)),
        });
        let interface: IMFStreamSink = stream.to_interface();
        streams.push(stream);
        Ok(interface)
    }

    fn RemoveStreamSink(&self, _id: u32) -> Result<()> {
        Err(MF_E_STREAMSINKS_FIXED.into())
    }

    fn GetStreamSinkCount(&self) -> Result<u32> {
        self.check_running()?;
        Ok(self.streams.lock().unwrap().len() as u32)
    }

    fn GetStreamSinkByIndex(&self, index: u32) -> Result<IMFStreamSink> {
        self.check_running()?;
        let streams = self.streams.lock().unwrap();
        let stream = streams.get(index as usize).ok_or(MF_E_INVALIDINDEX)?;
        Ok(stream.to_interface())
    }

    fn GetStreamSinkById(&self, id: u32) -> Result<IMFStreamSink> {
        self.check_running()?;
        let streams = self.streams.lock().unwrap();
        let stream = streams
            .iter()
            .find(|s| s.id == id)
            .ok_or(MF_E_INVALIDSTREAMNUMBER)?;
        Ok(stream.to_interface())
    }

    fn SetPresentationClock(&self, clock: Ref<IMFPresentationClock>) -> Result<()> {
        self.check_running()?;
        let me: IMFClockStateSink = self.to_interface();
        let mut current = self.clock.lock().unwrap();
        unsafe {
            if let Some(old) = current.as_ref() {
                old.RemoveClockStateSink(&me)?;
            }
            if let Some(new) = clock.as_ref() {
                new.AddClockStateSink(&me)?;
            }
        }
        *current = clock.cloned();
        Ok(())
    }

    fn GetPresentationClock(&self) -> Result<IMFPresentationClock> {
        self.check_running()?;
        self.clock
            .lock()
            .unwrap()
            .clone()
            .ok_or(MF_E_NO_CLOCK.into())
    }

    fn Shutdown(&self) -> Result<()> {
        let mut shared = lock(&self.shared);
        if shared.shut_down {
            return Err(MF_E_SHUTDOWN.into());
        }
        shared.shut_down = true;
        // A sink never finalized (the recording failed) leaves what it
        // wrote; dropping the muxer here closes the file.
        shared.writing = None;
        shared.file = None;
        drop(shared);
        for stream in self.streams.lock().unwrap().drain(..) {
            stream.shut_down();
        }
        if let Some(clock) = self.clock.lock().unwrap().take() {
            let me: IMFClockStateSink = self.to_interface();
            let _ = unsafe { clock.RemoveClockStateSink(&me) };
        }
        Ok(())
    }
}

impl IMFFinalizableMediaSink_Impl for Fmp4Sink_Impl {
    fn BeginFinalize(&self, callback: Ref<IMFAsyncCallback>, state: Ref<IUnknown>) -> Result<()> {
        self.check_running()?;
        let status = match lock(&self.shared).finish() {
            Ok(()) => S_OK,
            Err(e) => {
                tracing::error!("finishing the fragmented MP4: {e}");
                e.code()
            }
        };
        unsafe {
            let result = MFCreateAsyncResult(None, callback.as_ref(), state.as_ref())?;
            result.SetStatus(status)?;
            MFInvokeCallback(&result)
        }
    }

    fn EndFinalize(&self, result: Ref<IMFAsyncResult>) -> Result<()> {
        unsafe { result.ok()?.GetStatus() }
    }
}

impl IMFClockStateSink_Impl for Fmp4Sink_Impl {
    fn OnClockStart(&self, _system_time: i64, _offset: i64) -> Result<()> {
        self.tell_streams(MEStreamSinkStarted, true)
    }
    fn OnClockStop(&self, _system_time: i64) -> Result<()> {
        self.tell_streams(MEStreamSinkStopped, false)
    }
    fn OnClockPause(&self, _system_time: i64) -> Result<()> {
        self.tell_streams(MEStreamSinkPaused, false)
    }
    fn OnClockRestart(&self, _system_time: i64) -> Result<()> {
        self.tell_streams(MEStreamSinkStarted, true)
    }
    fn OnClockSetRate(&self, _system_time: i64, _rate: f32) -> Result<()> {
        Ok(())
    }
}

#[implement(IMFStreamSink, IMFMediaTypeHandler)]
struct Fmp4Stream {
    id: u32,
    /// Its place among the sink's streams: its track, less one.
    index: usize,
    shared: Arc<Mutex<Shared>>,
    queue: IMFMediaEventQueue,
    media_type: Mutex<Option<IMFMediaType>>,
    /// Let go at shutdown: the sink holds its streams, and each its sink.
    sink: Mutex<Option<IMFMediaSink>>,
}

impl Fmp4Stream {
    fn queue_event(&self, event: MF_EVENT_TYPE) -> Result<()> {
        unsafe {
            self.queue
                .QueueEventParamVar(event.0 as u32, &GUID::zeroed(), S_OK, std::ptr::null())
        }
    }

    fn shut_down(&self) {
        let _ = unsafe { self.queue.Shutdown() };
        self.sink.lock().unwrap().take();
    }

    fn check_running(&self) -> Result<()> {
        if self.sink.lock().unwrap().is_none() {
            return Err(MF_E_SHUTDOWN.into());
        }
        Ok(())
    }
}

impl IMFMediaEventGenerator_Impl for Fmp4Stream_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        unsafe { self.queue.GetEvent(flags.0) }
    }
    fn BeginGetEvent(&self, callback: Ref<IMFAsyncCallback>, state: Ref<IUnknown>) -> Result<()> {
        unsafe { self.queue.BeginGetEvent(callback.as_ref(), state.as_ref()) }
    }
    fn EndGetEvent(&self, result: Ref<IMFAsyncResult>) -> Result<IMFMediaEvent> {
        unsafe { self.queue.EndGetEvent(result.as_ref()) }
    }
    fn QueueEvent(
        &self,
        event: u32,
        extended: *const GUID,
        status: HRESULT,
        value: *const PROPVARIANT,
    ) -> Result<()> {
        unsafe {
            self.queue
                .QueueEventParamVar(event, extended, status, value)
        }
    }
}

impl IMFStreamSink_Impl for Fmp4Stream_Impl {
    fn GetMediaSink(&self) -> Result<IMFMediaSink> {
        self.sink
            .lock()
            .unwrap()
            .clone()
            .ok_or(MF_E_SHUTDOWN.into())
    }

    fn GetIdentifier(&self) -> Result<u32> {
        self.check_running()?;
        Ok(self.id)
    }

    fn GetMediaTypeHandler(&self) -> Result<IMFMediaTypeHandler> {
        self.check_running()?;
        Ok(self.to_interface())
    }

    fn ProcessSample(&self, sample: Ref<IMFSample>) -> Result<()> {
        self.check_running()?;
        let sample = unsafe { read_sample(sample.ok()?)? };
        lock(&self.shared).take(self.index, sample)?;
        self.queue_event(MEStreamSinkRequestSample)
    }

    fn PlaceMarker(
        &self,
        _kind: MFSTREAMSINK_MARKER_TYPE,
        _value: *const PROPVARIANT,
        context: *const PROPVARIANT,
    ) -> Result<()> {
        self.check_running()?;
        // Every sample before it is in already: samples are taken as they
        // come, not queued.
        unsafe {
            self.queue.QueueEventParamVar(
                MEStreamSinkMarker.0 as u32,
                &GUID::zeroed(),
                S_OK,
                context,
            )
        }
    }

    fn Flush(&self) -> Result<()> {
        self.check_running()
    }
}

/// `sample`'s time, duration, whether it's a keyframe, and its bytes.
unsafe fn read_sample(sample: &IMFSample) -> Result<Sample> {
    unsafe {
        let buffer = sample.ConvertToContiguousBuffer()?;
        let mut data = std::ptr::null_mut();
        let mut len = 0u32;
        buffer.Lock(&mut data, None, Some(&mut len))?;
        // An empty buffer may lock to no pointer at all, which
        // `from_raw_parts` mustn't be given.
        let bytes = if len == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(data, len as usize).to_vec()
        };
        buffer.Unlock()?;
        Ok(Sample {
            time: sample.GetSampleTime()?,
            duration: sample.GetSampleDuration().unwrap_or(0),
            keyframe: sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0,
            data: bytes,
        })
    }
}

impl IMFMediaTypeHandler_Impl for Fmp4Stream_Impl {
    fn IsMediaTypeSupported(
        &self,
        media_type: Ref<IMFMediaType>,
        _closest: OutRef<IMFMediaType>,
    ) -> Result<()> {
        track_kind(media_type.ok()?)
            .map(|_| ())
            .map_err(|_| MF_E_INVALIDMEDIATYPE.into())
    }

    fn GetMediaTypeCount(&self) -> Result<u32> {
        Ok(u32::from(self.media_type.lock().unwrap().is_some()))
    }

    fn GetMediaTypeByIndex(&self, index: u32) -> Result<IMFMediaType> {
        match (index, self.media_type.lock().unwrap().clone()) {
            (0, Some(media_type)) => Ok(media_type),
            _ => Err(MF_E_NO_MORE_TYPES.into()),
        }
    }

    fn SetCurrentMediaType(&self, media_type: Ref<IMFMediaType>) -> Result<()> {
        self.check_running()?;
        let media_type = media_type.ok()?;
        let kind = track_kind(media_type)
            .map_err(|_| windows::core::Error::from(MF_E_INVALIDMEDIATYPE))?;
        let mut shared = lock(&self.shared);
        if shared.writing.is_some() {
            return Err(MF_E_INVALIDREQUEST.into());
        }
        let kind = Kind::updated(shared.kinds[self.index].as_ref(), kind);
        shared.kinds[self.index] = Some(kind);
        *self.media_type.lock().unwrap() = Some(media_type.clone());
        Ok(())
    }

    fn GetCurrentMediaType(&self) -> Result<IMFMediaType> {
        self.media_type
            .lock()
            .unwrap()
            .clone()
            .ok_or(MF_E_NOT_INITIALIZED.into())
    }

    fn GetMajorType(&self) -> Result<GUID> {
        let media_type = self.GetCurrentMediaType()?;
        unsafe { media_type.GetGUID(&MF_MT_MAJOR_TYPE) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(width: u16, height: u16, header: Option<Vec<u8>>) -> Kind {
        Kind::Video {
            codec: VideoCodec::Hevc,
            width,
            height,
            header,
        }
    }

    /// The encoder's own type has its coded size -- whole 16-pixel blocks,
    /// 1408 rows for a 1392-row window -- which the file's tracks then said.
    #[test]
    fn the_encoders_type_brings_its_header_but_not_its_coded_size() {
        let added = video(2560, 1392, None);
        let encoders = video(2560, 1408, Some(vec![0, 0, 0, 1, 0x40]));
        match Kind::updated(Some(&added), encoders) {
            Kind::Video {
                width,
                height,
                header,
                ..
            } => {
                assert_eq!((width, height), (2560, 1392));
                assert_eq!(header, Some(vec![0, 0, 0, 1, 0x40]));
            }
            Kind::Audio { .. } => panic!("video"),
        }
        // With nothing to keep, the newer type is taken as it is.
        assert!(matches!(
            Kind::updated(None, video(64, 64, None)),
            Kind::Video {
                width: 64,
                height: 64,
                ..
            }
        ));
    }
}
