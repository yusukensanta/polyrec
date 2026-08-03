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
    D3D11_CPU_ACCESS_READ, D3D11_MAP_FLAG_DO_NOT_WAIT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::DXGI_ERROR_WAS_STILL_DRAWING;
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

/// Which staging slot to copy the newly-captured frame into, and which slot to
/// read back this iteration (the other one, copied ~1 iteration ago) -- pulled
/// out of `run_video_capture` because the alternation itself is easy to get
/// off-by-one on and is worth pinning down with a test independent of D3D11.
fn ping_pong_indices(frame_counter: u64) -> (usize, usize) {
    let write_idx = (frame_counter % 2) as usize;
    (write_idx, 1 - write_idx)
}

/// Maps `staging` for CPU read and copies its contents out row-by-row (handles
/// driver-added row padding via `RowPitch`). `map_flags` is normally
/// `D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32` from the steady-state ping-pong loop
/// in `run_video_capture` -- the GPU copy that filled `staging` was issued a
/// full iteration ago, so it has almost always already finished, and this way
/// a miss returns `Ok(None)` instead of blocking the capture thread on the
/// GPU. Callers must not treat `Ok(None)` as "drop this frame" -- fall back to
/// a blocking call (`map_flags = 0`) so the frame is never silently lost, only
/// ever delayed (see the call site in `run_video_capture` for why: dropping
/// instead of falling back previously produced near-empty recordings under
/// sustained GPU contention).
unsafe fn map_staging(
    device: &D3dDevice,
    staging: &ID3D11Resource,
    width: u32,
    height: u32,
    map_flags: u32,
) -> Result<Option<Vec<u8>>, AppError> {
    unsafe {
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        match device
            .d3d_context
            .Map(staging, 0, D3D11_MAP_READ, map_flags, Some(&mut mapped))
        {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAS_STILL_DRAWING => return Ok(None),
            Err(e) => return Err(AppError::Capture(format!("Map: {e}"))),
        }

        let row_pitch = mapped.RowPitch as usize;
        let packed_row = width as usize * 4;
        let len = height as usize * packed_row;
        // SAFETY: every byte of `pixel_data` is written by the row-copy loop
        // below before it's ever read, so skipping `vec![0u8; len]`'s zero-init
        // (a full extra `len`-byte write -- several MB at 1080p+, every frame)
        // is sound.
        #[allow(clippy::uninit_vec)]
        let mut pixel_data: Vec<u8> = {
            let mut v = Vec::with_capacity(len);
            v.set_len(len);
            v
        };
        for row in 0..height as usize {
            let src = (mapped.pData as *const u8).add(row * row_pitch);
            let dst = pixel_data.as_mut_ptr().add(row * packed_row);
            std::ptr::copy_nonoverlapping(src, dst, packed_row);
        }
        device.d3d_context.Unmap(staging, 0);

        Ok(Some(pixel_data))
    }
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
    show_border: bool,
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

    // Windows draws its own colored border around a window/monitor with an
    // active capture session -- desired for a manual recording (`show_border
    // = true`, an explicit signal the user asked for), but highlight
    // buffering runs a capture session continuously in the background
    // whenever `config.highlight.enabled` is on, with no recording actually
    // started -- showing the same OS border for that made it indistinguishable
    // from "recording right now". `IGraphicsCaptureSession3` (and thus
    // `SetIsBorderRequired`) only exists on Windows 11 24H2+; the cast/call
    // fails harmlessly on older Windows, which just keeps the OS default
    // (border shown) since there's no way to suppress it there.
    let _ = session.SetIsBorderRequired(show_border);

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

    // Two staging textures, ping-ponged (see `ping_pong_indices`/`map_staging`),
    // created once here and reused every frame via CopyResource. A single staging
    // texture used to be Map()'d in the same iteration it was CopyResource'd into
    // -- D3D11_MAP_READ forces the CPU to block until that GPU copy finishes,
    // which stalls the GPU's command queue and was directly visible as stutter
    // in whatever's being recorded (a game sharing that same GPU). Ping-ponging
    // means the slot being read was copied a full iteration ago, so by the time
    // we map it the GPU is essentially always already done -- combined with
    // D3D11_MAP_FLAG_DO_NOT_WAIT below, this makes the capture thread never
    // actually block on the GPU in the steady state.
    let create_staging = |device: &D3dDevice| -> Result<ID3D11Resource, AppError> {
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
        unsafe {
            let mut staging: Option<ID3D11Texture2D> = None;
            device
                .d3d_device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging))
                .map_err(|e| AppError::Capture(format!("CreateTexture2D staging: {e}")))?;
            staging
                .expect("CreateTexture2D succeeded but staging is None")
                .cast()
                .map_err(|e| AppError::Capture(format!("cast staging to ID3D11Resource: {e}")))
        }
    };
    let staging: [ID3D11Resource; 2] = [create_staging(&device)?, create_staging(&device)?];
    // pending[slot] is the pts of the frame most recently copied into that slot
    // and not yet read back -- None means there's nothing unread there.
    let mut pending: [Option<std::time::Duration>; 2] = [None, None];
    let mut frame_counter: u64 = 0;

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

        let (write_idx, read_idx) = ping_pong_indices(frame_counter);
        frame_counter += 1;

        unsafe {
            let src_res: ID3D11Resource = texture
                .cast()
                .map_err(|e| AppError::Capture(format!("cast texture to ID3D11Resource: {e}")))?;
            device.d3d_context.CopyResource(&staging[write_idx], &src_res);
        }
        pending[write_idx] = Some(clock.elapsed());

        // Read back the slot copied last iteration -- see the ping-pong comment
        // above the staging textures. Try non-blocking first (the GPU copy from
        // last iteration has almost always finished by now); a `None` here means
        // the GPU genuinely hasn't caught up yet -- e.g. under heavy GPU load
        // from a demanding game, exactly the case this is meant to help with --
        // so fall back to a blocking Map rather than silently dropping the
        // frame. A dropped-and-never-recovered frame previously produced
        // recordings that were almost entirely empty under sustained GPU
        // contention (PTS still advanced in real time, but frame data did not).
        // This fallback never blocks more than the original single-buffer
        // implementation did on every frame; it only avoids the block when the
        // GPU has already caught up.
        if let Some(pts) = pending[read_idx].take() {
            let mut pixel_data = unsafe {
                map_staging(
                    &device,
                    &staging[read_idx],
                    capture_width,
                    capture_height,
                    D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32,
                )?
            };
            if pixel_data.is_none() {
                pixel_data = unsafe {
                    map_staging(&device, &staging[read_idx], capture_width, capture_height, 0)?
                };
            }
            if let Some(pixel_data) = pixel_data {
                let data = scale_bgra(
                    pixel_data,
                    capture_width,
                    capture_height,
                    output_width,
                    output_height,
                );
                if tx.send(VideoFrame { pts, data }).await.is_err() {
                    break;
                }
            }
        }
    }

    // The ping-pong readback above trails one iteration behind by design (see
    // above), so the last frame copied before stopping was never read back in
    // the loop -- flush it now. Blocking Map (map_flags=0) is fine here since
    // we're already stopping, not racing a game's frame time.
    let mut leftover: Vec<(std::time::Duration, usize)> = pending
        .iter()
        .enumerate()
        .filter_map(|(idx, pts)| pts.map(|pts| (pts, idx)))
        .collect();
    leftover.sort_by_key(|(pts, _)| *pts);
    for (pts, idx) in leftover {
        let pixel_data =
            unsafe { map_staging(&device, &staging[idx], capture_width, capture_height, 0)? };
        if let Some(pixel_data) = pixel_data {
            let data = scale_bgra(
                pixel_data,
                capture_width,
                capture_height,
                output_width,
                output_height,
            );
            let _ = tx.send(VideoFrame { pts, data }).await;
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
    fn ping_pong_indices_alternate_and_never_collide() {
        for frame_counter in 0..8u64 {
            let (write_idx, read_idx) = ping_pong_indices(frame_counter);
            assert_ne!(write_idx, read_idx, "must never write and read the same slot");
            assert!(write_idx < 2 && read_idx < 2);
            // write_idx alternates 0,1,0,1,...
            assert_eq!(write_idx, (frame_counter % 2) as usize);
        }
    }

    #[test]
    fn ping_pong_indices_reads_the_slot_written_last_iteration() {
        // Whatever slot iteration N writes to, iteration N+1 must read back --
        // that's the entire point of the ping-pong (read what was copied ~1
        // iteration ago, never the slot just written this iteration).
        for frame_counter in 0..8u64 {
            let (write_idx, _) = ping_pong_indices(frame_counter);
            let (_, next_read_idx) = ping_pong_indices(frame_counter + 1);
            assert_eq!(write_idx, next_read_idx);
        }
    }

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
