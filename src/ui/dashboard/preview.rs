//! A picture of the selected source in its card, so it's plain which window
//! or display REC is about to record before recording it.

use super::App;
use super::theme::{SPACE_NORMAL, TEXT_MUTED};
use super::widgets::caption;
use crate::capture::video::{CaptureTarget, capture_still};
use crate::i18n::Strings;
use crate::types::{CaptureKind, CaptureSource};
use eframe::egui;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

/// The largest the picture is kept at: a source card is 200-380 px wide.
const PREVIEW_MAX: (u32, u32) = (360, 240);

/// The tallest the picture is shown, so a portrait window doesn't push the
/// rest of the list away.
const PREVIEW_MAX_HEIGHT: f32 = 160.0;

/// How long to wait for a source's first frame.
const FRAME_TIMEOUT: Duration = Duration::from_millis(800);

/// A picture taken off the UI thread: RGBA, its size, and whether Windows
/// left its capture border out while taking it.
struct Picture {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    border_left_out: bool,
}

/// The selected source's picture (`App::update_preview`, shown by
/// `App::show_preview`).
#[derive(Default)]
pub(super) struct SourcePreview {
    /// The source the picture is of.
    key: Option<(usize, CaptureKind)>,
    /// Whether the dashboard had focus last frame.
    focused: bool,
    rx: Option<Receiver<Option<Picture>>>,
    texture: Option<egui::TextureHandle>,
    /// The latest attempt got nothing, e.g. the window is minimized.
    unavailable: bool,
    /// Whether pictures can be taken without Windows flashing its capture
    /// border; assumed until a picture says otherwise.
    border_flashes: bool,
}

/// `bgra` (`width` x `height`) shrunk to fit `max`, keeping its shape and
/// never growing it, as opaque RGBA: each pixel the average of the block it
/// covers, so text and edges stay readable rather than aliasing.
fn thumbnail(bgra: &[u8], width: u32, height: u32, max: (u32, u32)) -> (Vec<u8>, u32, u32) {
    let scale = (f64::from(max.0) / f64::from(width))
        .min(f64::from(max.1) / f64::from(height))
        .min(1.0);
    let out_w = ((f64::from(width) * scale).round() as u32).max(1);
    let out_h = ((f64::from(height) * scale).round() as u32).max(1);
    let (w, h, ow, oh) = (
        width as usize,
        height as usize,
        out_w as usize,
        out_h as usize,
    );
    let mut rgba = Vec::with_capacity(ow * oh * 4);
    for oy in 0..oh {
        let (y0, y1) = (oy * h / oh, ((oy + 1) * h / oh).max(oy * h / oh + 1));
        for ox in 0..ow {
            let (x0, x1) = (ox * w / ow, ((ox + 1) * w / ow).max(ox * w / ow + 1));
            let mut sum = [0u32; 3];
            for y in y0..y1 {
                for px in bgra[(y * w + x0) * 4..(y * w + x1) * 4].as_chunks::<4>().0 {
                    for (total, &value) in sum.iter_mut().zip(px) {
                        *total += u32::from(value);
                    }
                }
            }
            let count = ((y1 - y0) * (x1 - x0)) as u32;
            let [b, g, r] = sum.map(|total| ((total + count / 2) / count) as u8);
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    (rgba, out_w, out_h)
}

/// Whether to take the picture: for a newly selected source, and again when
/// the dashboard comes back to the front -- where that wouldn't flash
/// Windows' capture border (`quiet`). Not on a timer: each picture starts a
/// capture, and a dashboard left open shouldn't keep doing that.
fn refresh_due(changed: bool, focus_gained: bool, quiet: bool) -> bool {
    changed || (focus_gained && quiet)
}

/// Takes `source`'s picture, shrunk for the card; `None` if there's none to
/// take (a minimized window).
fn take_picture(source: &CaptureSource) -> Option<Picture> {
    let target = CaptureTarget::from_raw(source.kind, source.hwnd);
    if let CaptureTarget::Window(hwnd) = target
        && unsafe { windows::Win32::UI::WindowsAndMessaging::IsIconic(hwnd) }.as_bool()
    {
        return None;
    }
    let still = capture_still(target, FRAME_TIMEOUT)
        .inspect_err(|e| tracing::debug!("no preview: {e}"))
        .ok()?;
    let (rgba, width, height) = thumbnail(&still.bgra, still.width, still.height, PREVIEW_MAX);
    Some(Picture {
        rgba,
        width,
        height,
        border_left_out: still.border_left_out,
    })
}

impl App {
    /// Keeps the selected source's picture up to date while nothing is being
    /// recorded. Runs every frame; the pictures are taken on a thread of
    /// their own.
    pub(super) fn update_preview(&mut self, ctx: &egui::Context) {
        let preview = &mut self.preview;
        if let Some(rx) = &preview.rx {
            match rx.try_recv() {
                Ok(picture) => {
                    preview.rx = None;
                    preview.unavailable = picture.is_none();
                    if let Some(picture) = picture {
                        preview.border_flashes = !picture.border_left_out;
                        let image = egui::ColorImage::from_rgba_unmultiplied(
                            [picture.width as usize, picture.height as usize],
                            &picture.rgba,
                        );
                        match &mut preview.texture {
                            Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
                            None => {
                                preview.texture = Some(ctx.load_texture(
                                    "source_preview",
                                    image,
                                    egui::TextureOptions::LINEAR,
                                ));
                            }
                        }
                    }
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => preview.rx = None,
            }
        }
        let recording = self.session.active().is_some();
        let Some(source) = self.selected_source.and_then(|i| self.sources.get(i)) else {
            *preview = SourcePreview::default();
            return;
        };
        if recording {
            return;
        }
        let key = (source.hwnd, source.kind);
        let changed = preview.key != Some(key);
        let focused = ctx.input(|i| i.viewport().focused) == Some(true);
        let focus_gained = focused && !preview.focused;
        preview.focused = focused;
        if !refresh_due(changed, focus_gained, !preview.border_flashes) {
            return;
        }
        if changed {
            // The old picture is of something else.
            preview.texture = None;
            preview.unavailable = false;
        }
        let (tx, rx) = mpsc::channel();
        let (source, wake) = (source.clone(), ctx.clone());
        let spawned = std::thread::Builder::new()
            .name("SourcePreview".into())
            .spawn(move || {
                let _ = tx.send(take_picture(&source));
                wake.request_repaint();
            });
        if spawned.is_ok() {
            preview.rx = Some(rx);
        }
        preview.key = Some(key);
    }

    /// The picture, as wide as `ui`, in the selected source's card -- or a
    /// line saying why there's none.
    pub(super) fn show_preview(&self, ui: &mut egui::Ui, s: &Strings) {
        let preview = &self.preview;
        match &preview.texture {
            Some(texture) => {
                let [w, h] = texture.size().map(|v| v as f32);
                let width = ui.available_width().min(PREVIEW_MAX_HEIGHT * w / h);
                ui.add_space(SPACE_NORMAL);
                ui.add(
                    egui::Image::new((texture.id(), egui::vec2(width, width * h / w)))
                        .corner_radius(4u8),
                );
            }
            None if preview.unavailable => {
                ui.add_space(4.0);
                ui.label(caption(s.preview_unavailable, TEXT_MUTED));
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bgra(pixels: &[[u8; 4]]) -> Vec<u8> {
        pixels.concat()
    }

    #[test]
    fn a_thumbnail_averages_each_block_and_comes_out_rgba() {
        // 4x2, scaled to fit 2x2: each 2x2 block averages to one pixel.
        let blue = [255, 0, 0, 255]; // BGRA
        let red = [0, 0, 255, 255];
        let src = bgra(&[blue, blue, red, red, blue, blue, red, [0, 0, 155, 255]]);
        let (rgba, w, h) = thumbnail(&src, 4, 2, (2, 2));
        assert_eq!((w, h), (2, 1), "keeps the 2:1 shape");
        assert_eq!(&rgba[0..4], &[0, 0, 255, 255], "blue, as RGBA");
        assert_eq!(&rgba[4..8], &[230, 0, 0, 255], "red, averaged");
    }

    #[test]
    fn a_thumbnail_fits_the_box_and_never_grows() {
        let src = vec![0u8; 1920 * 1080 * 4];
        let (rgba, w, h) = thumbnail(&src, 1920, 1080, (320, 240));
        assert_eq!((w, h), (320, 180));
        assert_eq!(rgba.len(), 320 * 180 * 4);
        let tall = vec![0u8; 100 * 400 * 4];
        let (_, w, h) = thumbnail(&tall, 100, 400, (320, 240));
        assert_eq!((w, h), (60, 240));
        let small = vec![0u8; 80 * 60 * 4];
        let (_, w, h) = thumbnail(&small, 80, 60, (320, 240));
        assert_eq!((w, h), (80, 60), "already small enough");
    }

    #[test]
    fn the_preview_is_taken_for_a_new_source_and_when_the_dashboard_is_back_in_front() {
        // A newly selected source: always.
        assert!(refresh_due(true, false, true));
        assert!(refresh_due(true, false, false));
        // The same source: again when the dashboard comes back to the front
        // -- the window may have changed meanwhile -- and not on a timer, so
        // a dashboard left open costs nothing.
        assert!(refresh_due(false, true, true));
        assert!(!refresh_due(false, false, true));
        // Where Windows can't leave its capture border out, each picture
        // would flash it: only once per selection.
        assert!(!refresh_due(false, true, false));
    }
}
