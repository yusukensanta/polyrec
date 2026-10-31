//! Keeping a lost graphics device -- a driver update or reset, or a game
//! that crashed the GPU -- from taking PolyRec down. egui's renderer can't
//! go on without its device and has no way to get a new one, so PolyRec
//! closes as on any exit, saving a recording that was running, and a new
//! copy starts on a new device (see `restart`).

use eframe::egui_wgpu::wgpu;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The dashboard's graphics device, watched for being lost.
pub(super) struct GraphicsDevice {
    /// `None` without a wgpu renderer, which never happens in practice.
    device: Option<wgpu::Device>,
    /// Set by wgpu's device-lost callback.
    lost: Arc<AtomicBool>,
}

impl GraphicsDevice {
    /// Starts watching the dashboard's device.
    ///
    /// wgpu's errors are logged rather than panicked on, as wgpu does by
    /// default: on a lost device every call errors, and a panic with a frame
    /// in flight panicked again while unwinding (the frame was still held),
    /// which aborted PolyRec on the spot -- without saving the recording.
    pub(super) fn watch(cc: &eframe::CreationContext<'_>) -> Self {
        let lost = Arc::new(AtomicBool::new(false));
        let Some(render_state) = &cc.wgpu_render_state else {
            return Self { device: None, lost };
        };
        let device = render_state.device.clone();
        device.on_uncaptured_error(Arc::new(|e: wgpu::Error| {
            tracing::error!("graphics error: {e}");
        }));
        let flag = Arc::clone(&lost);
        let ctx = cc.egui_ctx.clone();
        device.set_device_lost_callback(move |reason, message| {
            tracing::error!("graphics device lost ({reason:?}): {message}");
            flag.store(true, Ordering::Relaxed);
            ctx.request_repaint();
        });
        Self {
            device: Some(device),
            lost,
        }
    }

    /// Whether the device has been lost, checked anew: wgpu notices some
    /// losses only while tidying up after finished work, which a poll (not
    /// waiting for anything) does -- so it's seen before the next frame is
    /// drawn on a device that's gone. Once a frame.
    pub(super) fn check(&self) -> bool {
        if let Some(device) = &self.device {
            let _ = device.poll(wgpu::PollType::Poll);
        }
        self.was_lost()
    }

    /// Whether the device has been lost, as of the last `check` (or the
    /// device-lost callback).
    pub(super) fn was_lost(&self) -> bool {
        self.lost.load(Ordering::Relaxed)
    }
}
