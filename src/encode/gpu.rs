//! Keeping captured frames on the GPU (experimental, off by default -- see
//! `EncodeConfig::gpu_frames`).
//!
//! The default path copies every frame from video memory into system memory
//! (a staging-texture readback), scales it on the CPU when the output size
//! differs, and hands the encoder a memory buffer that its hardware MFT then
//! uploads straight back to the GPU -- about 15 MB each way per 1440p frame.
//! Here the capture loop and the sink writer share one Direct3D 11 device
//! instead: each frame is copied into a texture of its own and the encoder
//! gets that texture (`MFCreateDXGISurfaceBuffer`), with Media Foundation's
//! GPU video processor doing the colour conversion and any scaling.

use crate::capture::frame_pool::FrameBuf;
use crate::capture::video::copy_rows;
use crate::error::{AppError, ResultExt};
use std::sync::{Mutex, PoisonError};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
    ID3D11Multithread, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::{IMFDXGIDeviceManager, MFCreateDXGIDeviceManager};
use windows::core::Interface;

/// The Direct3D 11 device a recording's capture loop and encoder share, and
/// the DXGI device manager that hands it to Media Foundation. Cheap to
/// clone (reference-counted COM pointers).
#[derive(Clone)]
pub struct GpuFrames {
    pub(crate) device: ID3D11Device,
    pub(crate) context: ID3D11DeviceContext,
    pub(crate) manager: IMFDXGIDeviceManager,
}

// SAFETY: an ID3D11Device and the DXGI device manager are free-threaded. The
// immediate context isn't, but `new` turns on the device's multithread
// protection, which serializes every call on it -- the capture loop (copies)
// and Media Foundation's video processor and encoder (on the writer's
// thread) use it at the same time.
unsafe impl Send for GpuFrames {}
unsafe impl Sync for GpuFrames {}

/// The device recordings keep their frames on -- see `GpuFrames::shared`.
static SHARED: Mutex<Option<GpuFrames>> = Mutex::new(None);

impl std::fmt::Debug for GpuFrames {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GpuFrames")
    }
}

impl GpuFrames {
    /// The device recordings keep their frames on: created the first time
    /// it's asked for, then shared by every recording and Highlight session
    /// for the rest of the run -- and created again if the GPU was reset or
    /// removed since (a driver update, say). Each recording used to create
    /// one of its own, on the dashboard's thread. Creating it takes a
    /// moment, so `warm_up` does that ahead of the first recording.
    pub fn shared() -> Result<Self, AppError> {
        let mut shared = SHARED.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(frames) = shared.as_ref().filter(|frames| frames.usable()) {
            return Ok(frames.clone());
        }
        let frames = Self::new()?;
        *shared = Some(frames.clone());
        Ok(frames)
    }

    /// Creates the shared device (see `shared`) on a thread of its own.
    pub fn warm_up() {
        let spawned = std::thread::Builder::new()
            .name("GpuFramesWarmUp".into())
            .spawn(|| {
                if let Err(e) = Self::shared() {
                    tracing::warn!("frames will stay in system memory: {e}");
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("couldn't start creating the GPU frames device: {e}");
        }
    }

    /// Whether the device still works: it isn't lost to a GPU reset or
    /// removal.
    fn usable(&self) -> bool {
        unsafe { self.device.GetDeviceRemovedReason() }.is_ok()
    }

    pub fn new() -> Result<Self, AppError> {
        let (mut device, mut context) = (None, None);
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                windows::Win32::Foundation::HMODULE::default(),
                // Video support: the encoder and video processor run on it.
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .encode_ctx("D3D11CreateDevice (video)")?;
        }
        let (device, context): (ID3D11Device, ID3D11DeviceContext) = device
            .zip(context)
            .ok_or_else(|| AppError::Encode("D3D11CreateDevice returned no device".into()))?;
        let multithread: ID3D11Multithread = device.cast().encode_ctx("ID3D11Multithread")?;
        unsafe {
            let _ = multithread.SetMultithreadProtected(true);
        }
        let mut reset_token = 0;
        let mut manager = None;
        unsafe {
            MFCreateDXGIDeviceManager(&mut reset_token, &mut manager)
                .encode_ctx("MFCreateDXGIDeviceManager")?;
        }
        let manager = manager
            .ok_or_else(|| AppError::Encode("MFCreateDXGIDeviceManager returned None".into()))?;
        unsafe {
            manager
                .ResetDevice(&device, reset_token)
                .encode_ctx("IMFDXGIDeviceManager::ResetDevice")?;
        }
        Ok(Self {
            device,
            context,
            manager,
        })
    }

    /// A new BGRA texture of `width`x`height` the encoder's video processor
    /// can read.
    pub(crate) fn new_texture(&self, width: u32, height: u32) -> Result<ID3D11Texture2D, AppError> {
        self.create_texture(width, height, None)
    }

    /// A texture holding `bgra` (`width`x`height`, rows packed).
    #[cfg(test)]
    pub(crate) fn texture_from_bgra(
        &self,
        width: u32,
        height: u32,
        bgra: &[u8],
    ) -> Result<ID3D11Texture2D, AppError> {
        assert_eq!(bgra.len(), width as usize * height as usize * 4);
        self.create_texture(
            width,
            height,
            Some(&D3D11_SUBRESOURCE_DATA {
                pSysMem: bgra.as_ptr().cast(),
                SysMemPitch: width * 4,
                SysMemSlicePitch: 0,
            }),
        )
    }

    fn create_texture(
        &self,
        width: u32,
        height: u32,
        initial: Option<&D3D11_SUBRESOURCE_DATA>,
    ) -> Result<ID3D11Texture2D, AppError> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture = None;
        unsafe {
            self.device
                .CreateTexture2D(&desc, initial.map(std::ptr::from_ref), Some(&mut texture))
                .encode_ctx("CreateTexture2D (frame)")?;
        }
        texture.ok_or_else(|| AppError::Encode("CreateTexture2D returned None".into()))
    }

    /// A copy of `source` (same size and format) in a texture of its own, so
    /// the capture can hand its frame back while the encoder still reads it.
    pub(crate) fn copy_of(&self, source: &ID3D11Texture2D) -> Result<ID3D11Texture2D, AppError> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { source.GetDesc(&mut desc) };
        let copy = self.new_texture(desc.Width, desc.Height)?;
        unsafe {
            let (dst, src): (ID3D11Resource, ID3D11Resource) = (
                copy.cast().encode_ctx("texture as resource")?,
                source.cast().encode_ctx("texture as resource")?,
            );
            self.context.CopyResource(&dst, &src);
        }
        Ok(copy)
    }

    /// A black texture of `width`x`height` -- what a minimized window
    /// records as (see `capture::pacer::STILL_FRAME_REPEAT`).
    pub(crate) fn black_texture(
        &self,
        width: u32,
        height: u32,
    ) -> Result<ID3D11Texture2D, AppError> {
        let texture = self.new_texture(width, height)?;
        let mut view = None;
        unsafe {
            self.device
                .CreateRenderTargetView(&texture, None, Some(&mut view))
                .encode_ctx("CreateRenderTargetView")?;
            if let Some(view) = view {
                self.context
                    .ClearRenderTargetView(&view, &[0.0, 0.0, 0.0, 1.0]);
            }
        }
        Ok(texture)
    }
}

/// What a sink writer needs to take frames as textures (`FrameData::Gpu`):
/// the device they're on and the size they come in at -- the capture size;
/// the encoder scales them to its own.
#[derive(Debug, Clone)]
pub struct GpuInput {
    pub frames: GpuFrames,
    pub size: (u32, u32),
    /// Whether the encoder gets the textures as they are. If not, the writer
    /// reads each back into memory first (`TextureReadback`): slower, but
    /// the recording goes on -- see `RecordingWriter::start`.
    pub direct: bool,
}

/// Reads frames that came as textures back into memory, for an encoder that
/// doesn't take them as textures after all (see `GpuInput::direct`) -- the
/// same copy the capture loop makes itself when frames don't stay on the
/// GPU.
pub(crate) struct TextureReadback {
    frames: GpuFrames,
    staging: ID3D11Resource,
    /// The frames' size.
    pub(crate) size: (u32, u32),
}

impl TextureReadback {
    pub(crate) fn new(frames: &GpuFrames, (width, height): (u32, u32)) -> Result<Self, AppError> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging = None;
        unsafe {
            frames
                .device
                .CreateTexture2D(&desc, None, Some(&mut staging))
                .encode_ctx("CreateTexture2D (readback)")?;
        }
        let staging = staging
            .ok_or_else(|| AppError::Encode("CreateTexture2D returned None".into()))?
            .cast()
            .encode_ctx("staging texture as resource")?;
        Ok(Self {
            frames: frames.clone(),
            staging,
            size: (width, height),
        })
    }

    /// The pixels of `texture`, which must be of `size`, with its rows
    /// packed. Waits for the GPU to finish the copy.
    pub(crate) fn read(&self, texture: &ID3D11Texture2D) -> Result<FrameBuf, AppError> {
        let (width, height) = (self.size.0 as usize, self.size.1 as usize);
        let mut pixels = vec![0u8; width * 4 * height];
        unsafe {
            let source: ID3D11Resource = texture.cast().encode_ctx("texture as resource")?;
            let context = &self.frames.context;
            context.CopyResource(&self.staging, &source);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .encode_ctx("Map (readback)")?;
            // SAFETY: a successful Map of a `height`-row texture makes
            // `height` rows of `RowPitch` bytes readable at `pData`.
            copy_rows(
                mapped.pData as *const u8,
                mapped.RowPitch as usize,
                width * 4,
                height,
                &mut pixels,
            );
            context.Unmap(&self.staging, 0);
        }
        Ok(pixels.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every recording gets the same device, made once. Needs a GPU, so
    /// it's ignored by default.
    #[test]
    #[ignore]
    fn the_shared_device_is_made_once() {
        let first = GpuFrames::shared().expect("shared");
        let second = GpuFrames::shared().expect("shared");
        assert_eq!(first.device.as_raw(), second.device.as_raw());
        assert!(first.usable());
    }

    /// A texture read back comes out as the pixels it holds, rows packed.
    /// Needs a GPU, so it's ignored by default.
    #[test]
    #[ignore]
    fn a_texture_reads_back_as_its_pixels() {
        // An odd width, so the GPU pads the rows it maps.
        let (width, height) = (37, 5);
        let pixels: Vec<u8> = (0..width * height * 4).map(|i| (i % 251) as u8).collect();
        let gpu = GpuFrames::new().expect("GpuFrames");
        let texture = gpu
            .texture_from_bgra(width, height, &pixels)
            .expect("texture");
        let readback = TextureReadback::new(&gpu, (width, height)).expect("readback");
        assert_eq!(&*readback.read(&texture).expect("read"), pixels.as_slice());
    }
}
