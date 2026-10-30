use crate::capture::CaptureControl;
use crate::capture::device::D3dDevice;
use crate::capture::frame_pool::{FrameBuf, FramePool};
use crate::capture::frame_rate::{FrameRateLimiter, LiveFps};
use crate::error::{AppError, ResultExt};
use crate::types::{CaptureKind, VideoFrame};
use std::sync::Arc;
use tokio::sync::mpsc;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
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
    DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetMonitorInfoW, HMONITOR,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW, MonitorFromWindow,
};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::{Interface, PCWSTR};

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
    /// Rebuilds the typed handle from `CaptureSource`'s `(kind, hwnd)` pair,
    /// which stores either an HWND or an HMONITOR as a plain `usize` so it can
    /// cross threads (the raw handle types aren't `Send`).
    pub fn from_raw(kind: CaptureKind, handle: usize) -> Self {
        let ptr = handle as *mut core::ffi::c_void;
        match kind {
            CaptureKind::Window => CaptureTarget::Window(HWND(ptr)),
            CaptureKind::Monitor => CaptureTarget::Monitor(HMONITOR(ptr)),
        }
    }

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
            windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
                .capture_ctx("IGraphicsCaptureItemInterop factory")?;
        let item: GraphicsCaptureItem = target
            .create_item(&interop)
            .capture_ctx("CreateFor{{Window,Monitor}}")?;
        let size = item.Size().capture_ctx("item.Size")?;
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

/// Refresh rate (Hz) of the display `target` is shown on: the most frames per
/// second Windows.Graphics.Capture can deliver for it. `None` if Windows
/// doesn't report a number.
pub fn display_refresh_hz(target: CaptureTarget) -> Option<u32> {
    unsafe {
        let hmonitor = match target {
            CaptureTarget::Window(hwnd) => MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST),
            CaptureTarget::Monitor(hmonitor) => hmonitor,
        };
        let mut info = MONITORINFOEXW {
            monitorInfo: MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        // `cbSize` tells GetMonitorInfoW it may fill the whole EX struct.
        let info_ptr = (&raw mut info).cast::<MONITORINFO>();
        if !GetMonitorInfoW(hmonitor, info_ptr).as_bool() {
            return None;
        }
        let mut mode = DEVMODEW {
            dmSize: std::mem::size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        if !EnumDisplaySettingsW(
            PCWSTR(info.szDevice.as_ptr()),
            ENUM_CURRENT_SETTINGS,
            &mut mode,
        )
        .as_bool()
        {
            return None;
        }
        // 0 and 1 both mean "the hardware's default rate", not an actual number.
        (mode.dmDisplayFrequency > 1).then_some(mode.dmDisplayFrequency)
    }
}

/// Nearest-neighbor resize of an interleaved BGRA8 buffer. `run_video_capture` sizes
/// the encoder to match the capture size 1:1, so this is normally a no-op; kept as a
/// safety net for whatever mismatch might occur between the two. Takes `src` by value
/// so the identity (common/default) path returns it straight back with no copy at
/// all, instead of cloning a full frame (several MB at 1080p+) every frame for
/// nothing -- only the actual mismatch path needs to allocate a new buffer.
fn scale_bgra(src: FrameBuf, src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> FrameBuf {
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
    FrameBuf::from(dst)
}

/// Which staging slot to copy the newly-captured frame into, and which slot to
/// read back this iteration (the other one, copied ~1 iteration ago) -- pulled
/// out of `run_video_capture` because the alternation itself is easy to get
/// off-by-one on and is worth pinning down with a test independent of D3D11.
fn ping_pong_indices(frame_counter: u64) -> (usize, usize) {
    let write_idx = (frame_counter % 2) as usize;
    (write_idx, 1 - write_idx)
}

/// Copies `height` rows of `packed_row` bytes each out of a mapped texture
/// whose rows start `row_pitch` bytes apart (drivers may pad each row past
/// its pixels), into the tightly packed `dst`.
///
/// # Safety
/// `src` must be valid for reads of `(height - 1) * row_pitch + packed_row`
/// bytes (zero bytes when `height == 0`).
unsafe fn copy_rows(
    src: *const u8,
    row_pitch: usize,
    packed_row: usize,
    height: usize,
    dst: &mut [u8],
) {
    for (row, dst_row) in dst.chunks_exact_mut(packed_row).take(height).enumerate() {
        // SAFETY: row < height, so this row lies within the readable range
        // the caller guarantees.
        let src_row = unsafe { std::slice::from_raw_parts(src.add(row * row_pitch), packed_row) };
        dst_row.copy_from_slice(src_row);
    }
}

/// Everything one video capture holds open for its whole run: the D3D11
/// device, the WGC item / frame pool / session, the `Closed` signal, and the
/// two ping-pong staging textures (see `ping_pong_indices`/`map_staging`).
struct CaptureResources {
    device: D3dDevice,
    _item: GraphicsCaptureItem,
    frame_pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    /// Set by WGC's `GraphicsCaptureItem::Closed` -- see `open`.
    item_closed: Arc<std::sync::atomic::AtomicBool>,
    staging: [ID3D11Resource; 2],
    /// The caller-supplied capture size the frame pool and staging
    /// textures were created at.
    width: u32,
    height: u32,
}

impl CaptureResources {
    /// Creates the device, starts the capture session for `target`, and
    /// creates both staging textures. Caller must have initialized COM.
    fn open(
        target: CaptureTarget,
        (capture_width, capture_height): (u32, u32),
        show_border: bool,
        min_update_interval: std::time::Duration,
    ) -> Result<Self, AppError> {
        let device = D3dDevice::new()?;
        let dxgi_device = device.dxgi_device()?;

        // CreateDirect3D11DeviceFromDXGIDevice returns IInspectable; cast to IDirect3DDevice.
        let winrt_raw = unsafe {
            CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device)
                .capture_ctx("CreateDirect3D11DeviceFromDXGIDevice")?
        };
        let winrt_device: IDirect3DDevice =
            winrt_raw.cast().capture_ctx("cast to IDirect3DDevice")?;

        // Obtain the IGraphicsCaptureItemInterop factory and create item from the target.
        let interop: IGraphicsCaptureItemInterop =
            windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
                .capture_ctx("IGraphicsCaptureItemInterop factory")?;

        let item: GraphicsCaptureItem = unsafe {
            target
                .create_item(&interop)
                .capture_ctx("CreateFor{{Window,Monitor}}")?
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
        .capture_ctx("CreateFreeThreaded")?;

        let session = frame_pool
            .CreateCaptureSession(&item)
            .capture_ctx("CreateCaptureSession")?;

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

        // Asks WGC itself to deliver fewer frames on a high-refresh display, so
        // the frames `FrameRateLimiter` would drop anyway aren't composed and
        // handed over in the first place. Only a hint: the limiter still does
        // the exact capping. Like the border flag, `IGraphicsCaptureSession5`
        // only exists on Windows 11 24H2+, and this fails harmlessly before that.
        let _ = session.SetMinUpdateInterval(windows::Foundation::TimeSpan {
            Duration: (min_update_interval.as_nanos() / 100) as i64,
        });

        session.StartCapture().capture_ctx("StartCapture")?;

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
        .capture_ctx("GraphicsCaptureItem::Closed")?;

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
                    .capture_ctx("CreateTexture2D staging")?;
                staging
                    .expect("CreateTexture2D succeeded but staging is None")
                    .cast()
                    .capture_ctx("cast staging to ID3D11Resource")
            }
        };
        let staging: [ID3D11Resource; 2] = [create_staging(&device)?, create_staging(&device)?];

        Ok(Self {
            device,
            _item: item,
            frame_pool,
            session,
            item_closed,
            staging,
            width: capture_width,
            height: capture_height,
        })
    }

    fn item_closed(&self) -> bool {
        self.item_closed.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// GPU-copies a captured frame's texture into staging slot `slot`.
    fn copy_into_staging(
        &self,
        slot: usize,
        frame: &windows::Graphics::Capture::Direct3D11CaptureFrame,
    ) -> Result<(), AppError> {
        let surface = frame.Surface().capture_ctx("frame.Surface")?;

        // Obtain underlying D3D11 texture via IDirect3DDxgiInterfaceAccess.
        let dxgi_access: IDirect3DDxgiInterfaceAccess = surface
            .cast()
            .capture_ctx("cast to IDirect3DDxgiInterfaceAccess")?;
        let texture: ID3D11Texture2D = unsafe {
            dxgi_access
                .GetInterface()
                .capture_ctx("GetInterface ID3D11Texture2D")?
        };

        unsafe {
            let src_res: ID3D11Resource = texture
                .cast()
                .capture_ctx("cast texture to ID3D11Resource")?;
            self.device
                .d3d_context
                .CopyResource(&self.staging[slot], &src_res);
        }
        Ok(())
    }

    /// Maps staging slot `slot` for CPU read and copies its contents out
    /// row-by-row (handles driver-added row padding via `RowPitch`) into a
    /// buffer from `pool`. `map_flags` is normally
    /// `D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32` from the steady-state ping-pong
    /// loop in `run_video_capture` -- the GPU copy that filled the slot was
    /// issued a full iteration ago, so it has almost always already finished,
    /// and this way a miss returns `Ok(None)` instead of blocking the capture
    /// thread on the GPU. Callers must not treat `Ok(None)` as "drop this
    /// frame" -- see `read_back`.
    unsafe fn map_staging(
        &self,
        slot: usize,
        map_flags: u32,
        pool: &Arc<FramePool>,
    ) -> Result<Option<FrameBuf>, AppError> {
        unsafe {
            let staging = &self.staging[slot];
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            match self.device.d3d_context.Map(
                staging,
                0,
                D3D11_MAP_READ,
                map_flags,
                Some(&mut mapped),
            ) {
                Ok(()) => {}
                Err(e) if e.code() == DXGI_ERROR_WAS_STILL_DRAWING => return Ok(None),
                Err(e) => return Err(AppError::Capture(format!("Map: {e}"))),
            }

            let packed_row = self.width as usize * 4;
            let mut pixel_data = pool.take(self.height as usize * packed_row);
            // SAFETY: a successful Map of a `height`-row texture makes
            // `height` rows of `RowPitch` bytes readable at `pData`.
            copy_rows(
                mapped.pData as *const u8,
                mapped.RowPitch as usize,
                packed_row,
                self.height as usize,
                &mut pixel_data,
            );
            self.device.d3d_context.Unmap(staging, 0);

            Ok(Some(pixel_data))
        }
    }

    /// Reads back staging slot `slot`. With `nonblocking_first`, tries
    /// `D3D11_MAP_FLAG_DO_NOT_WAIT` first and only falls back to a blocking
    /// Map if the GPU hasn't finished the copy yet; otherwise maps blocking
    /// straight away. Either way the frame is never silently lost, only ever
    /// delayed: dropping on a not-yet-finished copy instead of falling back
    /// previously produced near-empty recordings under sustained GPU
    /// contention (PTS still advanced in real time, but frame data did not).
    unsafe fn read_back(
        &self,
        slot: usize,
        nonblocking_first: bool,
        pool: &Arc<FramePool>,
    ) -> Result<Option<FrameBuf>, AppError> {
        unsafe {
            if nonblocking_first
                && let Some(pixel_data) =
                    self.map_staging(slot, D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32, pool)?
            {
                return Ok(Some(pixel_data));
            }
            self.map_staging(slot, 0, pool)
        }
    }

    fn close(self) {
        let _ = self.session.Close();
        let _ = self.frame_pool.Close();
    }
}

/// Scales a read-back frame to the output size and sends it as the frame at
/// `pts`. `false` once the receiver is gone.
async fn send_frame(
    tx: &mpsc::Sender<VideoFrame>,
    pts: std::time::Duration,
    pixel_data: FrameBuf,
    (capture_width, capture_height): (u32, u32),
    (output_width, output_height): (u32, u32),
) -> bool {
    let data = scale_bgra(
        pixel_data,
        capture_width,
        capture_height,
        output_width,
        output_height,
    );
    tx.send(VideoFrame { pts, data }).await.is_ok()
}

/// Captures `target` and sends its frames, at most `fps` per second (see
/// `FrameRateLimiter`), until `ctrl` is stopped or the target goes away. The
/// cap follows `fps` if the writer lowers it while running.
pub async fn run_video_capture(
    target: CaptureTarget,
    capture_size: (u32, u32),
    output_size: (u32, u32),
    fps: LiveFps,
    ctrl: CaptureControl,
    tx: mpsc::Sender<VideoFrame>,
    show_border: bool,
) -> Result<(), AppError> {
    unsafe {
        windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_MULTITHREADED,
        )
        .ok()
        .capture_ctx("CoInitializeEx")?;
    }

    let mut cap = fps.get();
    let mut limiter = FrameRateLimiter::new(cap);
    // Half a slot, not a whole one: a minimum interval of exactly 1/fps would
    // make WGC skip a frame whenever one lands a hair early against the
    // display's vsync, halving a 60 Hz source recorded at 60 fps.
    let res = CaptureResources::open(target, capture_size, show_border, limiter.interval() / 2)?;
    // Recycles each frame's pixel buffer once the encoder is done with it --
    // see `FramePool`.
    let pool = FramePool::new();
    // pending[slot] is the pts of the frame most recently copied into that slot
    // and not yet read back -- None means there's nothing unread there.
    let mut pending: [Option<std::time::Duration>; 2] = [None, None];
    let mut frame_counter: u64 = 0;

    loop {
        if ctrl.is_stopped() {
            break;
        }
        if res.item_closed() {
            tracing::warn!(
                "capture item closed (captured window destroyed or its display disconnected), stopping video capture"
            );
            break;
        }

        let frame = match res.frame_pool.TryGetNextFrame() {
            Ok(f) => f,
            Err(_) => {
                tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                continue;
            }
        };

        // Discard frame when paused — skip GPU readback to save CPU/memory bandwidth.
        if ctrl.is_paused() {
            tokio::time::sleep(tokio::time::Duration::from_millis(16)).await;
            continue;
        }

        // Over the fps cap: drop it before any GPU copy or readback.
        if fps.get() != cap {
            cap = fps.get();
            limiter = FrameRateLimiter::new(cap);
        }
        let pts = ctrl.clock.elapsed();
        if !limiter.admit(pts) {
            continue;
        }

        let (write_idx, read_idx) = ping_pong_indices(frame_counter);
        frame_counter += 1;

        res.copy_into_staging(write_idx, &frame)?;
        pending[write_idx] = Some(pts);

        // Read back the slot copied last iteration -- see the ping-pong comment
        // in `CaptureResources::open`. Non-blocking first (the GPU copy from
        // last iteration has almost always finished by now), falling back to a
        // blocking Map when the GPU genuinely hasn't caught up yet -- e.g.
        // under heavy GPU load from a demanding game, exactly the case this is
        // meant to help with. This fallback never blocks more than the
        // original single-buffer implementation did on every frame; it only
        // avoids the block when the GPU has already caught up.
        if let Some(pts) = pending[read_idx].take()
            && let Some(pixel_data) = unsafe { res.read_back(read_idx, true, &pool)? }
            && !send_frame(&tx, pts, pixel_data, capture_size, output_size).await
        {
            break;
        }
    }

    // The ping-pong readback above trails one iteration behind by design (see
    // above), so the last frame copied before stopping was never read back in
    // the loop -- flush it now. Blocking Map is fine here since we're already
    // stopping, not racing a game's frame time.
    let mut leftover: Vec<(std::time::Duration, usize)> = pending
        .iter()
        .enumerate()
        .filter_map(|(idx, pts)| pts.map(|pts| (pts, idx)))
        .collect();
    leftover.sort_by_key(|(pts, _)| *pts);
    for (pts, idx) in leftover {
        if let Some(pixel_data) = unsafe { res.read_back(idx, false, &pool)? } {
            let _ = send_frame(&tx, pts, pixel_data, capture_size, output_size).await;
        }
    }

    res.close();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_pong_indices_alternate_and_never_collide() {
        for frame_counter in 0..8u64 {
            let (write_idx, read_idx) = ping_pong_indices(frame_counter);
            assert_ne!(
                write_idx, read_idx,
                "must never write and read the same slot"
            );
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
        let out = scale_bgra(src.into(), 2, 1, 2, 1);
        assert_eq!(&*out, expected.as_slice());
    }

    #[test]
    fn scale_bgra_identity_returns_the_same_pooled_buffer_without_copying() {
        let pool = FramePool::new();
        let src = pool.take(8);
        let src_ptr = src.as_ptr();
        let out = scale_bgra(src, 2, 1, 2, 1);
        assert_eq!(out.as_ptr(), src_ptr);
    }

    #[test]
    fn scale_bgra_upscales_2x() {
        // 1x1 BGRA pixel -> 2x2, every output pixel should equal the source pixel
        let src = vec![10u8, 20, 30, 40];
        let out = scale_bgra(src.into(), 1, 1, 2, 2);
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
        let out = scale_bgra(src.into(), 2, 2, 1, 1);
        assert_eq!(out.len(), 4);
        assert!(expected.chunks(4).any(|px| px == &*out));
    }

    #[test]
    fn copy_rows_drops_driver_row_padding() {
        // 2 rows of 3 pixels (12 bytes) each, mapped with a 16-byte pitch:
        // the 4 padding bytes per row must not reach the packed output.
        let (packed_row, row_pitch, height) = (12usize, 16usize, 2usize);
        let mut mapped = vec![0xEEu8; row_pitch * height];
        for row in 0..height {
            for i in 0..packed_row {
                mapped[row * row_pitch + i] = (row * 100 + i) as u8;
            }
        }
        let mut dst = vec![0u8; packed_row * height];
        unsafe { copy_rows(mapped.as_ptr(), row_pitch, packed_row, height, &mut dst) };
        let expected: Vec<u8> = (0..height)
            .flat_map(|row| (0..packed_row).map(move |i| (row * 100 + i) as u8))
            .collect();
        assert_eq!(dst, expected);
        assert!(!dst.contains(&0xEE));
    }

    #[test]
    fn copy_rows_with_an_unpadded_pitch_is_a_straight_copy() {
        let mapped: Vec<u8> = (0..32).collect();
        let mut dst = vec![0u8; 32];
        unsafe { copy_rows(mapped.as_ptr(), 8, 8, 4, &mut dst) };
        assert_eq!(dst, mapped);
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

    /// A small topmost window with a white bar sliding across it about once
    /// a millisecond, so DWM -- and WGC -- produce a new frame on every display
    /// refresh. Closed on drop.
    struct SlidingBarWindow {
        hwnd: usize,
        stop: Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl SlidingBarWindow {
        const W: i32 = 320;
        const H: i32 = 240;

        fn spawn() -> Self {
            use windows::Win32::Graphics::Gdi::{BLACKNESS, GetDC, PatBlt, ReleaseDC, WHITENESS};
            use windows::Win32::UI::WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, DispatchMessageW, MSG, PM_REMOVE, PeekMessageW,
                WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
            };
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let thread_stop = Arc::clone(&stop);
            let (tx, rx) = std::sync::mpsc::channel();
            let thread = std::thread::spawn(move || unsafe {
                let hwnd = CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                    windows::core::w!("STATIC"),
                    windows::core::w!("polyrec fps test"),
                    WS_POPUP | WS_VISIBLE,
                    0,
                    0,
                    Self::W,
                    Self::H,
                    None,
                    None,
                    None,
                    None,
                )
                .expect("CreateWindowExW");
                tx.send(hwnd.0 as usize).unwrap();
                let bar = 8;
                let mut msg = MSG::default();
                let mut x = 0;
                while !thread_stop.load(std::sync::atomic::Ordering::Relaxed) {
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        DispatchMessageW(&msg);
                    }
                    let dc = GetDC(Some(hwnd));
                    let _ = PatBlt(dc, x, 0, bar, Self::H, BLACKNESS);
                    x = (x + 1) % (Self::W - bar);
                    let _ = PatBlt(dc, x, 0, bar, Self::H, WHITENESS);
                    ReleaseDC(Some(hwnd), dc);
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                let _ = DestroyWindow(hwnd);
            });
            let hwnd = rx.recv().expect("window thread died");
            Self {
                hwnd,
                stop,
                thread: Some(thread),
            }
        }
    }

    impl Drop for SlidingBarWindow {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// Process CPU time (user + kernel) so far.
    fn process_cpu_time() -> std::time::Duration {
        use windows::Win32::Foundation::FILETIME;
        use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
            .expect("GetProcessTimes");
        }
        let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
        std::time::Duration::from_nanos((ticks(kernel) + ticks(user)) * 100)
    }

    /// Captures `target` (as `CaptureTarget::from_raw` takes it) for `secs` at
    /// `fps` and returns (frames delivered, process CPU time spent meanwhile).
    fn capture_for(
        (kind, handle): (CaptureKind, usize),
        fps: u32,
        secs: u64,
    ) -> (u64, std::time::Duration) {
        let target = CaptureTarget::from_raw(kind, handle);
        let (w, h) = query_capture_size(target).expect("query_capture_size");
        let size = (w & !1, h & !1);
        let ctrl = CaptureControl::new();
        let (tx, mut rx) = mpsc::channel(256);
        let capture_ctrl = ctrl.clone();
        let cpu_before = process_cpu_time();
        let capture = std::thread::spawn(move || {
            let target = CaptureTarget::from_raw(kind, handle);
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(run_video_capture(
                    target,
                    size,
                    size,
                    LiveFps::new(fps),
                    capture_ctrl,
                    tx,
                    false,
                ))
        });
        let counter = std::thread::spawn(move || {
            let mut frames = 0u64;
            while rx.blocking_recv().is_some() {
                frames += 1;
            }
            frames
        });
        std::thread::sleep(std::time::Duration::from_secs(secs));
        ctrl.request_stop();
        capture.join().unwrap().expect("run_video_capture failed");
        let frames = counter.join().unwrap();
        (frames, process_cpu_time() - cpu_before)
    }

    /// End-to-end fps cap: a window that changes every millisecond makes WGC
    /// deliver a frame of its monitor on every display refresh. Uncapped (what
    /// every recording did before the cap) that's the refresh rate; capped at
    /// 30 fps it must be about 30 (30, not 60, so a 60 Hz monitor can still
    /// show the difference). Also prints a 60 fps run for comparison. Shows a
    /// small window in the top-left corner for ~10 s, so it's ignored by
    /// default.
    #[test]
    #[ignore]
    fn capture_is_capped_to_the_configured_fps() {
        use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
        const SECS: u64 = 3;
        const FPS: u32 = 30;
        let window = SlidingBarWindow::spawn();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let hwnd = HWND(window.hwnd as *mut core::ffi::c_void);
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        let target = (CaptureKind::Monitor, monitor.0 as usize);

        let per_sec = |n: u64| n as f64 / SECS as f64;
        let cpu_ms_per_sec = |d: std::time::Duration| d.as_secs_f64() * 1000.0 / SECS as f64;
        let mut runs = Vec::new();
        for fps in [1000, 60, FPS] {
            let (frames, cpu) = capture_for(target, fps, SECS);
            println!(
                "cap {fps:>4}: {:>5.1} fps delivered, {:>4.0} ms CPU/s",
                per_sec(frames),
                cpu_ms_per_sec(cpu)
            );
            runs.push(frames);
        }
        let (uncapped, capped) = (runs[0], runs[2]);

        let budget = u64::from(FPS) * SECS;
        assert!(
            per_sec(uncapped) > f64::from(FPS) * 1.5,
            "the display didn't refresh fast enough to test a {FPS} fps cap"
        );
        assert!(
            capped <= budget + 2,
            "{capped} frames in {SECS} s at {FPS} fps"
        );
        assert!(
            capped >= budget * 8 / 10,
            "{capped} frames in {SECS} s at {FPS} fps"
        );
    }
}
