use crate::capture::device::D3dDevice;
use crate::error::AppError;
use crate::session::clock::RecordingClock;
use crate::types::VideoFrame;
use std::sync::Arc;
use tokio::sync::mpsc;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_STAGING, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::Interface;

/// A window or a monitor -- the two things Windows.Graphics.Capture can
/// create a `GraphicsCaptureItem` for (`CreateForWindow` / `CreateForMonitor`
/// respectively). Everything downstream of item creation (frame pool,
/// encoding) is identical either way; this only matters at the one call site
/// in each of `query_capture_size` and `run_video_capture` that creates the
/// item itself.
#[derive(Clone, Copy)]
pub enum CaptureTarget {
    Window(HWND),
    Monitor(HMONITOR),
}

impl CaptureTarget {
    unsafe fn create_item(
        self,
        interop: &IGraphicsCaptureItemInterop,
    ) -> windows::core::Result<GraphicsCaptureItem> {
        unsafe {
            match self {
                CaptureTarget::Window(hwnd) => interop.CreateForWindow(hwnd),
                CaptureTarget::Monitor(hmonitor) => interop.CreateForMonitor(hmonitor),
            }
        }
    }
}

/// Query the actual size Windows.Graphics.Capture will deliver frames at for this
/// target. This is what `run_video_capture` below captures at — NOT `GetClientRect`,
/// which excludes the title bar/borders and does not match WGC's window capture size.
/// Callers must use this (not GetClientRect) to size the encoder, or captured frames
/// will be written at the wrong stride and appear skewed/distorted.
pub fn query_capture_size(target: CaptureTarget) -> Result<(u32, u32), AppError> {
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_MULTITHREADED,
        );
        let interop: IGraphicsCaptureItemInterop =
            windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().map_err(
                |e| AppError::Capture(format!("IGraphicsCaptureItemInterop factory: {e}")),
            )?;
        let item: GraphicsCaptureItem = target
            .create_item(&interop)
            .map_err(|e| AppError::Capture(format!("CreateFor{{Window,Monitor}}: {e}")))?;
        let size = item
            .Size()
            .map_err(|e| AppError::Capture(format!("item.Size: {e}")))?;
        Ok((size.Width as u32, size.Height as u32))
    }
}

/// Query the resolution of the monitor a window is on. Only used when the user
/// explicitly picks resolution_mode = "display" — NOT the default (see the
/// resolution-regression fix: forcing this by default caused nearest-neighbor
/// upscale artifacts combined with an under-provisioned bitrate).
pub fn query_display_size(hwnd: HWND) -> Result<(u32, u32), AppError> {
    let rect = query_monitor_rect(hwnd)?;
    let w = (rect.right - rect.left) as u32;
    let h = (rect.bottom - rect.top) as u32;
    Ok((w, h))
}

/// Full bounds (not just size) of the monitor nearest `hwnd` -- used by the
/// overlay HUD to position itself on whichever monitor the recorded window
/// is actually on, instead of assuming the primary display.
pub fn query_monitor_rect(hwnd: HWND) -> Result<windows::Win32::Foundation::RECT, AppError> {
    unsafe { monitor_rect(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST)) }
}

/// Full bounds of `hmonitor` directly -- the `Monitor`-source counterpart to
/// `query_monitor_rect`'s window-based lookup, used by the overlay HUD when
/// the recording's own target already *is* a monitor (see
/// `render_overlay_viewport`), so there's no window to resolve one from.
pub fn monitor_rect(hmonitor: HMONITOR) -> Result<windows::Win32::Foundation::RECT, AppError> {
    unsafe {
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(hmonitor, &mut info).as_bool() {
            return Err(AppError::Capture("GetMonitorInfoW failed".into()));
        }
        Ok(info.rcMonitor)
    }
}

/// Nearest-neighbor resize of an interleaved BGRA8 buffer. `run_video_capture` sizes
/// the encoder to match the capture size 1:1, so this is normally a no-op; kept as a
/// safety net for whatever mismatch might occur between the two. Takes `src` by value
/// so the identity (common/default) path returns it straight back with no copy at
/// all, instead of cloning a full frame (several MB at 1080p+) every frame for
/// nothing -- only the actual mismatch path needs to allocate a new buffer.
fn scale_bgra(src: Vec<u8>, src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    if src_w == dst_w && src_h == dst_h {
        return src;
    }
    let (src_w, src_h, dst_w, dst_h) = (
        src_w as usize,
        src_h as usize,
        dst_w as usize,
        dst_h as usize,
    );
    let mut dst = vec![0u8; dst_w * dst_h * 4];
    for y in 0..dst_h {
        let src_y = (y * src_h) / dst_h;
        for x in 0..dst_w {
            let src_x = (x * src_w) / dst_w;
            let src_idx = (src_y * src_w + src_x) * 4;
            let dst_idx = (y * dst_w + x) * 4;
            dst[dst_idx..dst_idx + 4].copy_from_slice(&src[src_idx..src_idx + 4]);
        }
    }
    dst
}

// Each param here is independently resolved by the caller (capture vs. output
// size are deliberately separate concepts, see scale_bgra) -- a param struct
// would just rename this same list without adding clarity at this call boundary.
#[allow(clippy::too_many_arguments)]
pub async fn run_video_capture(
    target: CaptureTarget,
    capture_width: u32,
    capture_height: u32,
    output_width: u32,
    output_height: u32,
    clock: Arc<RecordingClock>,
    pause_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    stop_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
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

    // Obtain the IGraphicsCaptureItemInterop factory and create item from the target.
    let interop: IGraphicsCaptureItemInterop =
        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|e| AppError::Capture(format!("IGraphicsCaptureItemInterop factory: {e}")))?;

    let item: GraphicsCaptureItem = unsafe {
        target
            .create_item(&interop)
            .map_err(|e| AppError::Capture(format!("CreateFor{{Window,Monitor}}: {e}")))?
    };

    // Use the caller-supplied (already-clamped-even) size for the frame pool, not
    // item.Size() — this must match exactly what the encoder was configured with.
    // WGC crops/pads delivered frames to fit a requested size different from the
    // item's natural size, so this is safe even if it differs by a pixel or two.
    let size = SizeInt32 {
        Width: capture_width as i32,
        Height: capture_height as i32,
    };

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

    // WGC's documented signal that the capture target is gone (the captured
    // window was closed/destroyed, or its monitor was disconnected) --
    // without this, TryGetNextFrame below just keeps erroring forever with
    // nothing to distinguish "target closed" from the ordinary "no frame
    // ready yet" case it also returns, so the loop spun indefinitely at
    // ~1000Hz with no way to notice or log that capture had permanently died
    // (found via the same audio-device-invalidated pattern fixed earlier;
    // this is the video-side equivalent of that gap).
    let item_closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let item_closed_handler = Arc::clone(&item_closed);
    item.Closed(&TypedEventHandler::new(move |_, _| {
        item_closed_handler.store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }))
    .map_err(|e| AppError::Capture(format!("GraphicsCaptureItem::Closed: {e}")))?;

    // Staging texture — created once here and reused every frame via CopyResource.
    // This used to be recreated inside the loop despite this comment already
    // claiming otherwise: allocating a fresh GPU resource every frame (at 60fps)
    // is a driver round-trip per frame for no reason, since CopyResource just
    // overwrites the same staging texture's contents each time regardless.
    let staging_desc = D3D11_TEXTURE2D_DESC {
        Width: capture_width,
        Height: capture_height,
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
    let staging_res: ID3D11Resource = unsafe {
        let mut staging: Option<ID3D11Texture2D> = None;
        device
            .d3d_device
            .CreateTexture2D(&staging_desc, None, Some(&mut staging))
            .map_err(|e| AppError::Capture(format!("CreateTexture2D staging: {e}")))?;
        staging
            .expect("CreateTexture2D succeeded but staging is None")
            .cast()
            .map_err(|e| AppError::Capture(format!("cast staging to ID3D11Resource: {e}")))?
    };

    loop {
        if stop_flag.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        if item_closed.load(std::sync::atomic::Ordering::Relaxed) {
            tracing::warn!(
                "capture item closed (captured window destroyed or its display disconnected), stopping video capture"
            );
            break;
        }

        let frame = match frame_pool.TryGetNextFrame() {
            Ok(f) => f,
            Err(_) => {
                tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                continue;
            }
        };

        // Discard frame when paused — skip GPU readback to save CPU/memory bandwidth.
        if pause_flag.load(std::sync::atomic::Ordering::Relaxed) {
            tokio::time::sleep(tokio::time::Duration::from_millis(16)).await;
            continue;
        }

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
            let src_res: ID3D11Resource = texture
                .cast()
                .map_err(|e| AppError::Capture(format!("cast texture to ID3D11Resource: {e}")))?;

            device.d3d_context.CopyResource(&staging_res, &src_res);

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            device
                .d3d_context
                .Map(&staging_res, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| AppError::Capture(format!("Map: {e}")))?;

            let row_pitch = mapped.RowPitch as usize;
            let packed_row = capture_width as usize * 4;
            let mut pixel_data = vec![0u8; capture_height as usize * packed_row];
            for row in 0..capture_height as usize {
                let src = (mapped.pData as *const u8).add(row * row_pitch);
                let dst = pixel_data.as_mut_ptr().add(row * packed_row);
                std::ptr::copy_nonoverlapping(src, dst, packed_row);
            }
            device.d3d_context.Unmap(&staging_res, 0);

            pts = clock.elapsed();
            data = scale_bgra(
                pixel_data,
                capture_width,
                capture_height,
                output_width,
                output_height,
            );
        }

        let video_frame = VideoFrame { pts, data };
        if tx.send(video_frame).await.is_err() {
            break;
        }
    }

    let _ = session.Close();
    let _ = frame_pool.Close();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_bgra_identity_when_sizes_match() {
        let src = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        let expected = src.clone();
        let out = scale_bgra(src, 2, 1, 2, 1);
        assert_eq!(out, expected);
    }

    #[test]
    fn scale_bgra_upscales_2x() {
        // 1x1 BGRA pixel -> 2x2, every output pixel should equal the source pixel
        let src = vec![10u8, 20, 30, 40];
        let out = scale_bgra(src, 1, 1, 2, 2);
        assert_eq!(out.len(), 2 * 2 * 4);
        for chunk in out.chunks(4) {
            assert_eq!(chunk, &[10, 20, 30, 40]);
        }
    }

    #[test]
    fn scale_bgra_downscales() {
        // 2x2 -> 1x1: nearest-neighbor picks one of the four source pixels
        let src: Vec<u8> = (0..16).collect();
        let expected = src.clone();
        let out = scale_bgra(src, 2, 2, 1, 1);
        assert_eq!(out.len(), 4);
        assert!(expected.chunks(4).any(|px| px == out.as_slice()));
    }

    #[test]
    fn query_display_size_returns_positive_dimensions() {
        use windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow;
        let hwnd = unsafe { GetDesktopWindow() };
        let (w, h) = query_display_size(hwnd).expect("query_display_size failed");
        assert!(
            w > 0 && h > 0,
            "expected positive display dimensions, got {w}x{h}"
        );
    }
}
