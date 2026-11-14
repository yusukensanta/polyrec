//! The Media Foundation media types and samples the sink writer is given,
//! and the encoder settings reached through `ICodecAPI`.

use super::*;

/// The `ICodecAPI` of the encoder behind `stream`, if it exposes one (hardware
/// and Microsoft's own encoders do). The encoder exists once the stream's
/// input type is set.
pub(super) fn video_codec_api(writer: &IMFSinkWriter, stream: u32) -> Option<ICodecAPI> {
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

/// Asks the encoder for no B-frames, and returns how many it uses as far as
/// it says: one that turns the request down is asked its count, and one
/// that won't say is taken to use none -- the default Media Foundation has
/// encoders use (the property is optional, and the encoders tried use none
/// anyway).
pub(super) fn b_frames(api: &ICodecAPI) -> u32 {
    let asked = unsafe { api.SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &variant_u32(0)) };
    let Err(e) = asked else { return 0 };
    let count = unsafe { api.GetValue(&CODECAPI_AVEncMPVDefaultBPictureCount) }
        .ok()
        .and_then(|mut value| {
            let count = variant_as_u32(&value);
            // SAFETY: `value` came from GetValue and is ours to clear (the
            // Win32 VARIANT has no Drop); one that isn't a VT_UI4 may hold
            // memory of its own.
            let _ = unsafe { VariantClear(&mut value) };
            count
        })
        .unwrap_or(0);
    tracing::info!("video encoder kept its own B-frame count, {count} ({e})");
    count
}

/// Pins the encoder's GOP to `frames`, so its own keyframes fall where
/// `KeyframeSchedule` expects them. Best-effort: the encoders measured default
/// to one second's worth of frames anyway.
/// Whether the encoder took `frames` as its GOP size.
pub(super) fn set_gop_size(api: &ICodecAPI, frames: u32) -> bool {
    match unsafe { api.SetValue(&CODECAPI_AVEncMPVGOPSize, &variant_u32(frames)) } {
        Ok(()) => true,
        Err(e) => {
            tracing::info!("video encoder kept its own GOP size ({e}); keyframes go by time");
            false
        }
    }
}

/// `value`'s number, if it holds a `u32` (`VT_UI4`).
pub(super) fn variant_as_u32(value: &VARIANT) -> Option<u32> {
    // SAFETY: reading the union member the tag says is there.
    unsafe {
        let inner = &value.Anonymous.Anonymous;
        (inner.vt == VT_UI4).then(|| inner.Anonymous.ulVal)
    }
}

pub(super) fn variant_u32(value: u32) -> VARIANT {
    let mut variant = VARIANT::default();
    // SAFETY: writing the tag and the matching union member of a fresh VARIANT.
    unsafe {
        let inner = &mut variant.Anonymous.Anonymous;
        inner.vt = VT_UI4;
        inner.Anonymous.ulVal = value;
    }
    variant
}

pub(super) unsafe fn make_video_output_type(
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

pub(super) unsafe fn make_video_input_type(
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

pub(super) fn pack_u64(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}
