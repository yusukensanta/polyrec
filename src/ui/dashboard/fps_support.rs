//! What the Quality popup knows about the fps the selected source can really
//! be recorded at (see `session::probe_fps_support`). Probing creates real
//! encoders, so it runs on a background thread whenever the popup is open and
//! the source or an encode setting that matters changes.

use crate::config::{BitrateMode, Codec, EncoderMode, ResolutionMode};
use crate::session::{EncodeSettings, FpsSupport, probe_fps_support};
use crate::types::{CaptureKind, CaptureSource};
use eframe::egui;
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// Everything a probe's answer depends on. The fps setting itself isn't
/// part of it: one probe answers for every preset.
type ProbeKey = (
    usize,
    CaptureKind,
    Codec,
    EncoderMode,
    ResolutionMode,
    BitrateMode,
);

#[derive(Default)]
pub(super) struct FpsSupportProbe {
    /// What the latest probe (finished or in flight) was for.
    key: Option<ProbeKey>,
    rx: Option<Receiver<FpsSupport>>,
    result: Option<FpsSupport>,
}

impl FpsSupportProbe {
    /// The answer for the current source and settings; `None` while a probe
    /// is still running, with no source selected, or if a probe died.
    pub(super) fn result(&self) -> Option<&FpsSupport> {
        self.result.as_ref()
    }

    /// Starts a probe if `source` or `encode` changed since the last one, and
    /// collects a finished one. Call every frame the popup is open.
    pub(super) fn update(
        &mut self,
        ctx: &egui::Context,
        source: Option<&CaptureSource>,
        encode: &EncodeSettings,
    ) {
        let Some(source) = source else {
            *self = Self::default();
            return;
        };
        let key = (
            source.hwnd,
            source.kind,
            encode.codec,
            encode.encoder_mode,
            encode.resolution_mode,
            encode.bitrate_mode,
        );
        if self.key != Some(key) {
            self.start(ctx, source.clone(), encode.clone());
            self.key = Some(key);
        }
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(support) => {
                    self.result = Some(support);
                    self.rx = None;
                }
                Err(TryRecvError::Empty) => {}
                // The probe thread died. Leave every preset enabled: recording
                // start still steps down to what the encoder accepts.
                Err(TryRecvError::Disconnected) => self.rx = None,
            }
        }
    }

    fn start(&mut self, ctx: &egui::Context, source: CaptureSource, encode: EncodeSettings) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("polyrec-fps-probe".into())
            .spawn(move || {
                let _ = tx.send(probe_fps_support(&source, &encode));
                ctx.request_repaint();
            });
        // A stale answer must not linger while the new one is pending.
        self.result = None;
        self.rx = spawned.is_ok().then_some(rx);
    }
}
