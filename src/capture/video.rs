use crate::capture::CaptureControl;
use crate::capture::device::D3dDevice;
use crate::capture::frame_pool::{FrameBuf, FramePool};
use crate::capture::frame_rate::{FrameRateLimiter, LiveFps};
use crate::capture::pacer::{FramePacer, Step};
use crate::encode::gpu::GpuFrames;
use crate::error::{AppError, ResultExt};
use crate::types::{CaptureKind, FrameData, VideoFrame};
use std::sync::Arc;
use tokio::sync::mpsc;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
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
pub(crate) fn scale_bgra(
    src: FrameBuf,
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> FrameBuf {
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
pub(crate) unsafe fn copy_rows(
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
/// device, the WGC item / frame pool / session, the `Closed` signal, and --
/// unless frames stay on the GPU -- the two ping-pong staging textures (see
/// `ping_pong_indices`/`map_staging`).
struct CaptureResources {
    device: D3dDevice,
    _item: GraphicsCaptureItem,
    frame_pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    /// Set by WGC's `GraphicsCaptureItem::Closed` -- see `open`.
    item_closed: Arc<std::sync::atomic::AtomicBool>,
    /// Empty when frames stay on the GPU (`FrameOutput::Gpu`), which never
    /// reads them back.
    staging: Vec<ID3D11Resource>,
    /// The caller-supplied capture size the frame pool and staging
    /// textures were created at.
    width: u32,
    height: u32,
    /// Whether Windows took `show_border` -- it can't leave the border out
    /// before Windows 11 24H2.
    border_as_asked: bool,
}

impl CaptureResources {
    /// Creates the device -- or uses `gpu`'s, which the encoder shares --,
    /// starts the capture session for `target`, and creates both staging
    /// textures unless frames stay on the GPU. Caller must have initialized
    /// COM.
    fn open(
        target: CaptureTarget,
        (capture_width, capture_height): (u32, u32),
        show_border: bool,
        min_update_interval: std::time::Duration,
        gpu: Option<&GpuFrames>,
    ) -> Result<Self, AppError> {
        let device = match gpu {
            Some(gpu) => D3dDevice {
                d3d_device: gpu.device.clone(),
                d3d_context: gpu.context.clone(),
            },
            None => D3dDevice::new()?,
        };
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
        let border_as_asked = session.SetIsBorderRequired(show_border).is_ok();

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
        let staging = if gpu.is_some() {
            Vec::new()
        } else {
            vec![create_staging(&device)?, create_staging(&device)?]
        };

        Ok(Self {
            device,
            _item: item,
            frame_pool,
            session,
            item_closed,
            staging,
            width: capture_width,
            height: capture_height,
            border_as_asked,
        })
    }

    fn item_closed(&self) -> bool {
        self.item_closed.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// GPU-copies a captured frame's texture into staging slot `slot`.
    fn copy_into_staging(
        &self,
        slot: usize,
        frame: &Direct3D11CaptureFrame,
    ) -> Result<(), AppError> {
        let texture = frame_texture(frame)?;
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

/// One picture of a capture target (`capture_still`): BGRA rows, tightly
/// packed.
pub struct Still {
    pub bgra: FrameBuf,
    pub width: u32,
    pub height: u32,
    /// Whether Windows left its capture border out. It can't before
    /// Windows 11 24H2, where taking a picture flashes the border around the
    /// target.
    pub border_left_out: bool,
}

/// One picture of `target` as a recording would see it, for the dashboard's
/// preview of the selected source. Runs a capture session only until the
/// first frame arrives, waiting at most `timeout` -- a minimized window sends
/// none. Joins the calling thread to COM's multithreaded apartment, so call
/// it from a thread of its own.
pub fn capture_still(
    target: CaptureTarget,
    timeout: std::time::Duration,
) -> Result<Still, AppError> {
    let (width, height) = query_capture_size(target)?;
    let size = (width.max(2) & !1, height.max(2) & !1);
    let res = CaptureResources::open(target, size, false, std::time::Duration::ZERO, None)?;
    let deadline = std::time::Instant::now() + timeout;
    let frame = loop {
        if let Ok(frame) = res.frame_pool.TryGetNextFrame() {
            break frame;
        }
        if res.item_closed() || std::time::Instant::now() >= deadline {
            res.close();
            return Err(AppError::Capture("no frame for a preview".into()));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let copied = res.copy_into_staging(0, &frame);
    drop(frame);
    let read = copied.and_then(|()| unsafe { res.read_back(0, false, &FramePool::new()) });
    let border_left_out = res.border_as_asked;
    res.close();
    let bgra = read?.ok_or_else(|| AppError::Capture("preview frame not read back".into()))?;
    Ok(Still {
        bgra,
        width: size.0,
        height: size.1,
        border_left_out,
    })
}

/// A captured frame's underlying D3D11 texture (WGC's own, recycled once
/// the frame is dropped).
fn frame_texture(frame: &Direct3D11CaptureFrame) -> Result<ID3D11Texture2D, AppError> {
    let surface = frame.Surface().capture_ctx("frame.Surface")?;
    let dxgi_access: IDirect3DDxgiInterfaceAccess = surface
        .cast()
        .capture_ctx("cast to IDirect3DDxgiInterfaceAccess")?;
    unsafe {
        dxgi_access
            .GetInterface()
            .capture_ctx("GetInterface ID3D11Texture2D")
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
    tx.send(VideoFrame {
        pts,
        data: data.into(),
    })
    .await
    .is_ok()
}

/// Captures `target` and sends its frames, at most `fps` per second, with
/// the newest one sent again while nothing changes (see `FramePacer`), until
/// `ctrl` is stopped or the target goes away. The cap follows `fps` if the
/// writer lowers it while running. With `gpu`, the
/// frames stay on the GPU (`FrameData::Gpu`, at `capture_size`) instead of
/// being read back and scaled to `output_size`.
#[allow(clippy::too_many_arguments)]
pub async fn run_video_capture(
    target: CaptureTarget,
    capture_size: (u32, u32),
    output_size: (u32, u32),
    fps: LiveFps,
    ctrl: CaptureControl,
    tx: mpsc::Sender<VideoFrame>,
    show_border: bool,
    gpu: Option<GpuFrames>,
) -> Result<(), AppError> {
    unsafe {
        windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_MULTITHREADED,
        )
        .ok()
        .capture_ctx("CoInitializeEx")?;
    }

    // Half a slot, not a whole one: a minimum interval of exactly 1/fps would
    // make WGC skip a frame whenever one lands a hair early against the
    // display's vsync, halving a 60 Hz source recorded at 60 fps.
    let res = CaptureResources::open(
        target,
        capture_size,
        show_border,
        FrameRateLimiter::new(fps.get()).interval() / 2,
        gpu.as_ref(),
    )?;
    let mut pacer = FramePacer::new(fps.get(), ctrl.clock.elapsed());
    let sizes = (capture_size, output_size);
    let mut output = match gpu {
        Some(frames) => FrameOutput::Gpu {
            frames,
            newest: None,
            failures: 0,
        },
        None => FrameOutput::Cpu {
            pool: FramePool::new(),
            pending: [None, None],
            frame_counter: 0,
            newest_slot: None,
        },
    };
    // Once the source is gone, the video goes on to the end of the recording
    // -- holding its last picture, or black once capturing has failed --
    // rather than stopping: a stream that stops getting frames stalls the
    // whole file (see `pacer::STILL_FRAME_REPEAT`), so Stop could never
    // finish saving it.
    let mut gone: Option<SourceGone> = None;
    loop {
        if ctrl.is_stopped() {
            break;
        }
        if gone.is_none() && res.item_closed() {
            tracing::warn!(
                "capture item closed (captured window destroyed or its display disconnected); the rest of the video holds its last picture"
            );
            ctrl.video_source_lost();
            gone = Some(SourceGone::Closed);
        }

        pacer.set_fps(fps.get());
        let paused = ctrl.is_paused();
        // A frame the pacer holds back (paused, or over the fps cap) gets no
        // GPU copy or readback unless it's recorded after all.
        let next = match gone {
            None => res.frame_pool.TryGetNextFrame().ok(),
            Some(_) => None,
        };
        let step = match next {
            Some(frame) => pacer.on_frame(frame, ctrl.clock.elapsed(), paused),
            None => match pacer.on_idle(ctrl.clock.elapsed(), paused) {
                Step::Wait => {
                    tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                    continue;
                }
                step => step,
            },
        };
        let sent = match step {
            Step::Record(frame, pts) => output.on_frame(&res, &frame, pts, &tx, sizes).await,
            Step::Repeat(now) if gone == Some(SourceGone::Failed) => {
                Ok(output.black(now, &tx, sizes).await)
            }
            Step::Repeat(now) => output.repeat(&res, now, &tx, sizes).await,
            Step::Wait => Ok(true),
        };
        let sent = sent.unwrap_or_else(|e| {
            tracing::error!("video capture failed ({e}); the rest of the video is black");
            ctrl.video_source_lost();
            gone = Some(SourceGone::Failed);
            true
        });
        if !sent {
            break;
        }
    }

    if gone != Some(SourceGone::Failed) {
        output.flush(&res, &tx, sizes).await?;
    }
    res.close();

    Ok(())
}

/// How `run_video_capture` turns captured frames into `VideoFrame`s.
enum FrameOutput {
    /// Read back into system memory and scaled to the output size on the
    /// CPU.
    Cpu {
        /// Recycles each frame's pixel buffer once the encoder is done with
        /// it -- see `FramePool`.
        pool: Arc<FramePool>,
        /// `pending[slot]` is the pts of the frame most recently copied into
        /// that staging slot and not yet read back -- None means there's
        /// nothing unread there.
        pending: [Option<std::time::Duration>; 2],
        frame_counter: u64,
        /// The slot holding the newest frame, if one has come yet.
        newest_slot: Option<usize>,
    },
    /// Copied into a texture of its own each and sent as that -- see
    /// `encode::gpu`.
    Gpu {
        frames: GpuFrames,
        /// The newest frame's texture (or a black one), if one has been sent.
        newest: Option<ID3D11Texture2D>,
        /// Frames that couldn't be copied since the last one that could --
        /// see `FrameOutput::skipped`.
        failures: u64,
    },
}

/// How the video source went away partway through a recording -- see
/// `run_video_capture`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceGone {
    /// The window was closed, or its display disconnected: the video holds
    /// its last picture.
    Closed,
    /// Capturing failed: the video is black.
    Failed,
}

/// (capture size, output size).
type FrameSizes = ((u32, u32), (u32, u32));

impl FrameOutput {
    /// Handles a newly captured frame, admitted as the frame at `pts`.
    /// `Ok(false)` once the receiver is gone.
    async fn on_frame(
        &mut self,
        res: &CaptureResources,
        frame: &Direct3D11CaptureFrame,
        pts: std::time::Duration,
        tx: &mpsc::Sender<VideoFrame>,
        (capture_size, output_size): FrameSizes,
    ) -> Result<bool, AppError> {
        match self {
            Self::Cpu {
                pool,
                pending,
                frame_counter,
                newest_slot,
            } => {
                let (write_idx, read_idx) = ping_pong_indices(*frame_counter);
                *frame_counter += 1;

                res.copy_into_staging(write_idx, frame)?;
                pending[write_idx] = Some(pts);
                *newest_slot = Some(write_idx);

                // Read back the slot copied last iteration -- see the
                // ping-pong comment in `CaptureResources::open`.
                // Non-blocking first (the GPU copy from last iteration has
                // almost always finished by now), falling back to a blocking
                // Map when the GPU genuinely hasn't caught up yet -- e.g.
                // under heavy GPU load from a demanding game, exactly the
                // case this is meant to help with. This fallback never
                // blocks more than the original single-buffer implementation
                // did on every frame; it only avoids the block when the GPU
                // has already caught up.
                if let Some(pts) = pending[read_idx].take()
                    && let Some(pixel_data) = unsafe { res.read_back(read_idx, true, pool)? }
                {
                    return Ok(send_frame(tx, pts, pixel_data, capture_size, output_size).await);
                }
                Ok(true)
            }
            Self::Gpu {
                frames,
                newest,
                failures,
            } => {
                // A copy, since WGC reuses its own texture for a later frame
                // while the encoder may still be reading this one.
                let texture = match frame_texture(frame).and_then(|t| frames.copy_of(&t)) {
                    Ok(texture) => texture,
                    Err(e) => return Ok(Self::skipped(failures, &e)),
                };
                if *failures > 0 {
                    tracing::info!("frames copy again, after {failures} that couldn't be");
                    *failures = 0;
                }
                *newest = Some(texture.clone());
                Ok(send_texture(tx, pts, texture).await)
            }
        }
    }

    /// Nothing new has come for `STILL_FRAME_REPEAT` (see `FramePacer`):
    /// keeps the video going with the newest frame -- as itself if it's
    /// still waiting to be read back (for a next frame that isn't coming),
    /// else again as a frame of `now` -- or, before any frame has come at all
    /// (a minimized window gives none), a black one. `Ok(false)` once the
    /// receiver is gone.
    async fn repeat(
        &mut self,
        res: &CaptureResources,
        now: std::time::Duration,
        tx: &mpsc::Sender<VideoFrame>,
        (capture_size, output_size): FrameSizes,
    ) -> Result<bool, AppError> {
        match self {
            Self::Cpu {
                pool,
                pending,
                newest_slot,
                ..
            } => {
                let pts = newest_slot
                    .and_then(|slot| pending[slot].take())
                    .unwrap_or(now);
                match *newest_slot {
                    Some(slot) => match unsafe { res.read_back(slot, false, pool)? } {
                        Some(pixels) => {
                            Ok(send_frame(tx, pts, pixels, capture_size, output_size).await)
                        }
                        None => Ok(true),
                    },
                    None => Ok(send_black_frame(pool, tx, pts, output_size).await),
                }
            }
            Self::Gpu {
                frames,
                newest,
                failures,
            } => {
                let texture = match newest {
                    Some(texture) => texture.clone(),
                    None => match frames.black_texture(capture_size.0, capture_size.1) {
                        Ok(black) => newest.insert(black).clone(),
                        Err(e) => return Ok(Self::skipped(failures, &e)),
                    },
                };
                // Sending the same texture again is fine: nothing writes to
                // a frame's texture once it's been sent.
                Ok(send_texture(tx, now, texture).await)
            }
        }
    }

    /// A black frame of `now`, once capturing has failed (see
    /// `run_video_capture`) -- made without the capture's device, which may
    /// be what failed. `false` once the receiver is gone.
    async fn black(
        &mut self,
        now: std::time::Duration,
        tx: &mpsc::Sender<VideoFrame>,
        (capture_size, output_size): FrameSizes,
    ) -> bool {
        match self {
            Self::Cpu { pool, .. } => send_black_frame(pool, tx, now, output_size).await,
            // The encoder shares the device here, so when it has failed the
            // encoder has too, and there's nothing better to send.
            Self::Gpu {
                frames, failures, ..
            } => match frames.black_texture(capture_size.0, capture_size.1) {
                Ok(black) => send_texture(tx, now, black).await,
                Err(e) => Self::skipped(failures, &e),
            },
        }
    }

    /// A frame that stays on the GPU couldn't be copied (or a black one
    /// made): out of video memory, say, with a game holding most of it.
    /// It's skipped, and the video goes on with the frames around it (or
    /// the newest one repeated) -- it used to end the video for the rest of
    /// the recording. Logs only the first of a run of failures.
    fn skipped(failures: &mut u64, error: &AppError) -> bool {
        if *failures == 0 {
            tracing::warn!("skipping frames that can't be copied on the GPU: {error}");
        }
        *failures += 1;
        true
    }

    /// Capture stopped: sends what's still waiting to be read back. The
    /// ping-pong readback trails one iteration behind by design, so the last
    /// frame copied before stopping was never read back in the loop.
    /// Blocking Map is fine here since we're already stopping, not racing a
    /// game's frame time.
    async fn flush(
        &mut self,
        res: &CaptureResources,
        tx: &mpsc::Sender<VideoFrame>,
        (capture_size, output_size): FrameSizes,
    ) -> Result<(), AppError> {
        let Self::Cpu { pool, pending, .. } = self else {
            return Ok(());
        };
        let mut leftover: Vec<(std::time::Duration, usize)> = pending
            .iter()
            .enumerate()
            .filter_map(|(idx, pts)| pts.map(|pts| (pts, idx)))
            .collect();
        leftover.sort_by_key(|(pts, _)| *pts);
        for (pts, idx) in leftover {
            if let Some(pixel_data) = unsafe { res.read_back(idx, false, pool)? } {
                let _ = send_frame(tx, pts, pixel_data, capture_size, output_size).await;
            }
        }
        Ok(())
    }
}

/// Sends a frame that stays on the GPU. `false` once the receiver is gone.
async fn send_texture(
    tx: &mpsc::Sender<VideoFrame>,
    pts: std::time::Duration,
    texture: ID3D11Texture2D,
) -> bool {
    tx.send(VideoFrame {
        pts,
        data: FrameData::Gpu(texture),
    })
    .await
    .is_ok()
}

/// Sends a black frame of `size`, as the frame at `pts`. `false` once the
/// receiver is gone.
async fn send_black_frame(
    pool: &Arc<FramePool>,
    tx: &mpsc::Sender<VideoFrame>,
    pts: std::time::Duration,
    (width, height): (u32, u32),
) -> bool {
    let mut black = pool.take(width as usize * height as usize * 4);
    black.fill(0);
    tx.send(VideoFrame {
        pts,
        data: black.into(),
    })
    .await
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::pacer::STILL_FRAME_REPEAT;

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
    /// refresh (or, from `spawn_still`, a bar that never moves). Closed on
    /// drop.
    struct SlidingBarWindow {
        hwnd: usize,
        stop: Arc<std::sync::atomic::AtomicBool>,
        /// The bar moves only once this is set.
        go: Arc<std::sync::atomic::AtomicBool>,
        /// Set once the bar has made all its moves.
        settled: Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl SlidingBarWindow {
        const W: i32 = 320;
        const H: i32 = 240;
        const BAR: i32 = 8;

        fn spawn() -> Self {
            Self::spawn_with(u32::MAX, true)
        }

        fn spawn_still() -> Self {
            Self::spawn_with(1, true)
        }

        /// The bar moves one pixel `steps` times, once `go` is called unless
        /// `going` already, and then stays put.
        fn spawn_with(steps: u32, going: bool) -> Self {
            use windows::Win32::Graphics::Gdi::{BLACKNESS, GetDC, PatBlt, ReleaseDC, WHITENESS};
            use windows::Win32::UI::WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, DispatchMessageW, MSG, PM_REMOVE, PeekMessageW,
                WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
            };
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let go = Arc::new(std::sync::atomic::AtomicBool::new(going));
            let settled = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (thread_stop, thread_go, thread_settled) =
                (Arc::clone(&stop), Arc::clone(&go), Arc::clone(&settled));
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
                let mut msg = MSG::default();
                let (mut x, mut moved) = (0, 0);
                while !thread_stop.load(std::sync::atomic::Ordering::Relaxed) {
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        DispatchMessageW(&msg);
                    }
                    if moved < steps && thread_go.load(std::sync::atomic::Ordering::Relaxed) {
                        let dc = GetDC(Some(hwnd));
                        let _ = PatBlt(dc, x, 0, Self::BAR, Self::H, BLACKNESS);
                        x = (x + 1) % (Self::W - Self::BAR);
                        let _ = PatBlt(dc, x, 0, Self::BAR, Self::H, WHITENESS);
                        ReleaseDC(Some(hwnd), dc);
                        moved += 1;
                        if moved == steps {
                            thread_settled.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                let _ = DestroyWindow(hwnd);
            });
            let hwnd = rx.recv().expect("window thread died");
            Self {
                hwnd,
                stop,
                go,
                settled,
                thread: Some(thread),
            }
        }

        fn go(&self) {
            self.go.store(true, std::sync::atomic::Ordering::Relaxed);
        }

        fn settled(&self) -> bool {
            self.settled.load(std::sync::atomic::Ordering::Relaxed)
        }

        /// Where the bar's left edge ends up after `steps` moves.
        fn bar_x_after(steps: u32) -> i32 {
            (steps % (Self::W - Self::BAR) as u32) as i32
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
        gpu: Option<GpuFrames>,
    ) -> (u64, std::time::Duration) {
        let target = CaptureTarget::from_raw(kind, handle);
        let (w, h) = query_capture_size(target).expect("query_capture_size");
        let size = (w & !1, h & !1);
        let ctrl = CaptureControl::new();
        let (tx, mut rx) = mpsc::channel(256);
        let capture_ctrl = ctrl.clone();
        let expect_gpu = gpu.is_some();
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
                    gpu,
                ))
        });
        // Only frames in the expected form count: textures with `gpu`,
        // memory buffers without.
        let counter = std::thread::spawn(move || {
            let mut frames = 0u64;
            while let Some(frame) = rx.blocking_recv() {
                if matches!(frame.data, FrameData::Gpu(_)) == expect_gpu {
                    frames += 1;
                }
            }
            frames
        });
        std::thread::sleep(std::time::Duration::from_secs(secs));
        ctrl.request_stop();
        capture.join().unwrap().expect("run_video_capture failed");
        let frames = counter.join().unwrap();
        (frames, process_cpu_time() - cpu_before)
    }

    /// A still window keeps the video flowing. Windows hands out a frame only
    /// when the picture changes, so a still window gives one and then nothing
    /// -- which used to leave the recording with no video at all (the one
    /// frame waited for a next one to be read back), stalling the MP4 sink
    /// and, on stop, the dashboard. Now the last frame is sent again every
    /// `STILL_FRAME_REPEAT`. Shows a small window in the top-left corner for
    /// ~3 s, so it's ignored by default.
    #[test]
    #[ignore]
    fn still_window_keeps_sending_frames() {
        const SECS: u64 = 3;
        let window = SlidingBarWindow::spawn_still();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let (frames, _) = capture_for((CaptureKind::Window, window.hwnd), 30, SECS, None);
        let expected = (std::time::Duration::from_secs(SECS).as_millis()
            / STILL_FRAME_REPEAT.as_millis()) as u64;
        println!("still window: {frames} frames in {SECS} s (repeats due: {expected})");
        assert!(
            frames >= expected - 2,
            "{frames} frames in {SECS} s from a still window"
        );
    }

    /// A minimized window gives no frame at all, so there's nothing to send
    /// again: black frames keep the video going instead. Minimizes a small
    /// window for ~3 s, so it's ignored by default.
    #[test]
    #[ignore]
    fn minimized_window_gets_black_frames() {
        use windows::Win32::UI::WindowsAndMessaging::{SW_MINIMIZE, ShowWindow};
        const SECS: u64 = 3;
        let window = SlidingBarWindow::spawn_still();
        let hwnd = HWND(window.hwnd as *mut core::ffi::c_void);
        let _ = unsafe { ShowWindow(hwnd, SW_MINIMIZE) };
        std::thread::sleep(std::time::Duration::from_millis(300));
        let (frames, _) = capture_for((CaptureKind::Window, window.hwnd), 30, SECS, None);
        let expected = (std::time::Duration::from_secs(SECS).as_millis()
            / STILL_FRAME_REPEAT.as_millis()) as u64;
        println!("minimized window: {frames} frames in {SECS} s (due: {expected})");
        assert!(
            frames >= expected - 2,
            "{frames} frames in {SECS} s from a minimized window"
        );
    }

    /// The left edge of the white bar a `SlidingBarWindow` frame of `width`
    /// pixels shows, if any, judged along its middle row.
    fn white_bar_x(frame: &[u8], (width, height): (u32, u32)) -> Option<usize> {
        let row = (height / 2) as usize * width as usize * 4;
        frame[row..row + width as usize * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .position(|px| px[..3].iter().all(|&c| c >= 250))
    }

    /// The last change before a window goes still makes it into the
    /// recording, even when it came too soon after the frame before it for
    /// the fps cap: it used to be dropped, and the frame before it repeated
    /// in its place for as long as the window stayed still. Slides the bar
    /// of a small window, stops it, and checks where the recording's last
    /// frame shows it -- four times, at 20 fps so most display refreshes are
    /// over the cap. Shows a small window for ~6 s, so it's ignored by
    /// default.
    #[test]
    #[ignore]
    fn the_last_change_before_a_still_picture_is_recorded() {
        for steps in [97, 131, 173, 211] {
            let window = SlidingBarWindow::spawn_with(steps, false);
            let handle = window.hwnd;
            let target = CaptureTarget::from_raw(CaptureKind::Window, handle);
            let (w, h) = query_capture_size(target).expect("query_capture_size");
            let size = (w & !1, h & !1);
            let ctrl = CaptureControl::new();
            let (tx, mut rx) = mpsc::channel(256);
            let capture_ctrl = ctrl.clone();
            let capture = std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(run_video_capture(
                        CaptureTarget::from_raw(CaptureKind::Window, handle),
                        size,
                        size,
                        LiveFps::new(20),
                        capture_ctrl,
                        tx,
                        false,
                        None,
                    ))
            });
            let last = std::thread::spawn(move || {
                let mut last = None;
                while let Some(frame) = rx.blocking_recv() {
                    if let FrameData::Cpu(pixels) = frame.data {
                        last = Some(pixels);
                    }
                }
                last
            });
            std::thread::sleep(std::time::Duration::from_millis(300));
            window.go();
            while !window.settled() {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            // A slot to record a held frame in, and a repeat to send it.
            std::thread::sleep(std::time::Duration::from_millis(600));
            ctrl.request_stop();
            capture.join().unwrap().expect("run_video_capture failed");
            let last = last.join().unwrap().expect("no frame recorded");

            // The window may be stretched for the display's scaling.
            let scale = f64::from(size.0) / f64::from(SlidingBarWindow::W);
            let expected = f64::from(SlidingBarWindow::bar_x_after(steps)) * scale;
            let found = white_bar_x(&last, size).expect("no bar in the last frame");
            println!("{steps} steps: bar at {found}, expected {expected:.0}");
            assert!(
                (found as f64 - expected).abs() <= scale.ceil() + 1.0,
                "after {steps} steps the recording ends with the bar at {found}, not {expected:.0}"
            );
        }
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
            let (frames, cpu) = capture_for(target, fps, SECS, None);
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

    /// Frames that stay on the GPU come through as textures (`capture_for`
    /// only counts those): at the capped rate from a window that changes
    /// every millisecond, and as repeats from a still one. Shows small
    /// windows in the top-left corner for ~6 s, so it's ignored by default.
    #[test]
    #[ignore]
    fn gpu_frames_arrive_as_textures() {
        use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
        const SECS: u64 = 3;
        const FPS: u32 = 30;
        let gpu = GpuFrames::new().expect("GpuFrames");

        let sliding = SlidingBarWindow::spawn();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let hwnd = HWND(sliding.hwnd as *mut core::ffi::c_void);
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        let (frames, cpu) = capture_for(
            (CaptureKind::Monitor, monitor.0 as usize),
            FPS,
            SECS,
            Some(gpu.clone()),
        );
        println!("gpu, changing display: {frames} frames in {SECS} s, {cpu:?} CPU");
        let budget = u64::from(FPS) * SECS;
        assert!(
            (budget * 8 / 10..=budget + 2).contains(&frames),
            "{frames} frames in {SECS} s at {FPS} fps"
        );
        drop(sliding);

        let still = SlidingBarWindow::spawn_still();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let (frames, _) = capture_for((CaptureKind::Window, still.hwnd), FPS, SECS, Some(gpu));
        let expected = (std::time::Duration::from_secs(SECS).as_millis()
            / STILL_FRAME_REPEAT.as_millis()) as u64;
        println!("gpu, still window: {frames} frames in {SECS} s (repeats due: {expected})");
        assert!(
            frames >= expected - 2,
            "{frames} frames from a still window"
        );
    }

    /// What keeping frames on the GPU saves: records the display the
    /// top-left corner is on (where a window changing every millisecond
    /// keeps frames coming at the full rate) through the real session, at
    /// the display's own resolution and 60 fps, once each way, and compares
    /// the process's CPU time. Both recordings must finish and hold video.
    /// Takes ~15 s and shows a small window, so it's ignored by default.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn gpu_frames_record_with_less_cpu() {
        use crate::session::{CaptureInputs, EncodeSettings, SessionManager};
        use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
        const SECS: u64 = 5;

        let sliding = SlidingBarWindow::spawn();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let hwnd = HWND(sliding.hwnd as *mut core::ffi::c_void);
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) }.0 as usize;
        let source = crate::sources::enumerate_sources()
            .into_iter()
            .find(|s| s.kind == CaptureKind::Monitor && s.hwnd == monitor)
            .expect("the display source");
        let dir = tempfile::tempdir().unwrap();
        let mut cpu_ms_per_sec = Vec::new();
        for gpu_frames in [false, true] {
            let frame_count = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let mut sm = SessionManager::new();
            let inputs = CaptureInputs {
                encode: EncodeSettings {
                    fps: 60,
                    gpu_frames,
                    ..EncodeSettings::default()
                },
                ..CaptureInputs::default()
            };
            sm.start_capture(
                source.clone(),
                inputs,
                false,
                Arc::clone(&frame_count),
                dir.path(),
                crate::recording_naming::RecordingNaming::CustomPrefix(format!("gpu_{gpu_frames}")),
            )
            .expect("start_capture");
            // Settle past the writer's start-up before measuring.
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let (frames_before, cpu_before) = (
                frame_count.load(std::sync::atomic::Ordering::Relaxed),
                process_cpu_time(),
            );
            tokio::time::sleep(std::time::Duration::from_secs(SECS)).await;
            let cpu = process_cpu_time() - cpu_before;
            let frames = frame_count.load(std::sync::atomic::Ordering::Relaxed) - frames_before;
            let handle = tokio::task::block_in_place(|| sm.stop_capture()).expect("stop_capture");
            let path = tokio::time::timeout(std::time::Duration::from_secs(10), handle)
                .await
                .expect("finalizing took over 10 s")
                .expect("recorder task panicked")
                .expect("finalize failed");
            let size = std::fs::metadata(&path).unwrap().len();
            let per_sec = cpu.as_secs_f64() * 1000.0 / SECS as f64;
            println!(
                "gpu_frames={gpu_frames}: {} fps recorded, {per_sec:.0} ms CPU/s, {size} bytes",
                frames / SECS
            );
            assert!(frames / SECS >= 40, "only {frames} frames in {SECS} s");
            assert!(size > 100_000, "{size}-byte recording");
            cpu_ms_per_sec.push(per_sec);
        }
        assert!(
            cpu_ms_per_sec[1] < cpu_ms_per_sec[0],
            "GPU frames took more CPU: {cpu_ms_per_sec:?} ms/s"
        );
    }
}
