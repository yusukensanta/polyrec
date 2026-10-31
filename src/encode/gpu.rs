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

use crate::error::{AppError, ResultExt};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11_SUBRESOURCE_DATA,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11Multithread, ID3D11Resource, ID3D11Texture2D,
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

impl std::fmt::Debug for GpuFrames {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GpuFrames")
    }
}

impl GpuFrames {
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
}
