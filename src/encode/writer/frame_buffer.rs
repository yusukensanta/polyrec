//! A captured frame's picture as the encoder takes it: a buffer over the
//! bytes already read back, or a texture.

use super::*;

/// A frame's picture as it goes to the encoder, kept to fill the slots after
/// it (see `CfrTimeline`). Nothing writes to it once it's made, so one
/// picture goes in as many samples as it fills slots, without a copy.
#[derive(Clone)]
pub(super) enum Picture {
    /// In memory (see `frame_buffer`).
    Memory(IMFMediaBuffer),
    /// On the GPU, for an encoder that takes textures.
    Texture(ID3D11Texture2D),
}

/// A captured video frame's bytes as a buffer for the encoder, without a
/// copy (see `FrameMediaBuffer`).
pub(super) fn frame_buffer(data: FrameBuf) -> Result<IMFMediaBuffer, AppError> {
    Ok(FrameMediaBuffer::new(data)?.into())
}

/// `make_sample` for a frame already in a buffer of its own (see
/// `frame_buffer`), without a copy.
pub(super) unsafe fn make_buffer_sample(
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
pub(super) unsafe fn make_texture_sample(
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
pub(super) struct FrameMediaBuffer {
    data: std::cell::UnsafeCell<FrameBuf>,
    max_len: u32,
    current_len: std::sync::atomic::AtomicU32,
}

impl FrameMediaBuffer {
    pub(super) fn new(data: FrameBuf) -> Result<Self, AppError> {
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
