use crate::capture::device::D3dDevice;
use crate::error::AppError;
use crate::session::clock::RecordingClock;
use crate::types::VideoFrame;
use std::sync::Arc;
use tokio::sync::mpsc;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Resource, ID3D11Texture2D, D3D11_CPU_ACCESS_READ, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::Interface;

pub async fn run_video_capture(
    hwnd: HWND,
    clock: Arc<RecordingClock>,
    tx: mpsc::Sender<VideoFrame>,
) -> Result<(), AppError> {
    unsafe {
        windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_MULTITHREADED,
        )
        .ok()
        .map_err(|e| AppError::Capture(format!("CoInitializeEx: {e}")))?;
    }

    let device = D3dDevice::new()?;
    let dxgi_device = device.dxgi_device()?;

    // CreateDirect3D11DeviceFromDXGIDevice returns IInspectable; cast to IDirect3DDevice.
    let winrt_raw = unsafe {
        CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device)
            .map_err(|e| AppError::Capture(format!("CreateDirect3D11DeviceFromDXGIDevice: {e}")))?
    };
    let winrt_device: IDirect3DDevice = winrt_raw
        .cast()
        .map_err(|e| AppError::Capture(format!("cast to IDirect3DDevice: {e}")))?;

    // Obtain the IGraphicsCaptureItemInterop factory and create item from HWND.
    let interop: IGraphicsCaptureItemInterop =
        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|e| AppError::Capture(format!("IGraphicsCaptureItemInterop factory: {e}")))?;

    let item: GraphicsCaptureItem = unsafe {
        interop
            .CreateForWindow(hwnd)
            .map_err(|e| AppError::Capture(format!("CreateForWindow: {e}")))?
    };

    let size = item
        .Size()
        .map_err(|e| AppError::Capture(format!("item.Size: {e}")))?;
    let width = size.Width as u32;
    let height = size.Height as u32;

    let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &winrt_device,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        2,
        size,
    )
    .map_err(|e| AppError::Capture(format!("CreateFreeThreaded: {e}")))?;

    let session = frame_pool
        .CreateCaptureSession(&item)
        .map_err(|e| AppError::Capture(format!("CreateCaptureSession: {e}")))?;

    session
        .StartCapture()
        .map_err(|e| AppError::Capture(format!("StartCapture: {e}")))?;

    // Staging texture descriptor — created once, reused each frame.
    let staging_desc = D3D11_TEXTURE2D_DESC {
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
        // BindFlags, CPUAccessFlags, MiscFlags are plain u32 in windows-rs 0.58
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };

    loop {
        let frame = match frame_pool.TryGetNextFrame() {
            Ok(f) => f,
            Err(_) => {
                tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                continue;
            }
        };

        let surface = frame
            .Surface()
            .map_err(|e| AppError::Capture(format!("frame.Surface: {e}")))?;

        // Obtain underlying D3D11 texture via IDirect3DDxgiInterfaceAccess.
        let dxgi_access: IDirect3DDxgiInterfaceAccess = surface
            .cast()
            .map_err(|e| AppError::Capture(format!("cast to IDirect3DDxgiInterfaceAccess: {e}")))?;
        let texture: ID3D11Texture2D = unsafe {
            dxgi_access
                .GetInterface()
                .map_err(|e| AppError::Capture(format!("GetInterface ID3D11Texture2D: {e}")))?
        };

        let pts;
        let data;
        unsafe {
            let mut staging: Option<ID3D11Texture2D> = None;
            device
                .d3d_device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging))
                .map_err(|e| AppError::Capture(format!("CreateTexture2D staging: {e}")))?;
            let staging = staging.unwrap();

            // Cast textures to ID3D11Resource for CopyResource / Map / Unmap.
            let staging_res: ID3D11Resource = staging
                .cast()
                .map_err(|e| AppError::Capture(format!("cast staging to ID3D11Resource: {e}")))?;
            let src_res: ID3D11Resource = texture
                .cast()
                .map_err(|e| AppError::Capture(format!("cast texture to ID3D11Resource: {e}")))?;

            device
                .d3d_context
                .CopyResource(&staging_res, &src_res);

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            device
                .d3d_context
                .Map(&staging_res, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| AppError::Capture(format!("Map: {e}")))?;

            let row_pitch = mapped.RowPitch as usize;
            let mut pixel_data = vec![0u8; height as usize * row_pitch];
            std::ptr::copy_nonoverlapping(
                mapped.pData as *const u8,
                pixel_data.as_mut_ptr(),
                pixel_data.len(),
            );
            device.d3d_context.Unmap(&staging_res, 0);

            pts = clock.elapsed();
            data = pixel_data;
        }

        let video_frame = VideoFrame {
            pts,
            width,
            height,
            data,
        };
        if tx.send(video_frame).await.is_err() {
            break;
        }
    }

    let _ = session.Close();
    let _ = frame_pool.Close();

    Ok(())
}
