//! Shows whether PolyRec is recording without the dashboard having to be in
//! view: a red dot (amber while paused) on its taskbar button and on its
//! tray icon. While a game has focus, those are what's left on screen.

use super::theme::Palette;
use crate::i18n::Strings;
use crate::types::SessionState;
use eframe::egui;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, ReleaseDC,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{ITaskbarList3, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIcon, DestroyIcon, GetIconInfo, GetSystemMetrics, HICON, ICONINFO, IMAGE_ICON,
    LR_DEFAULTCOLOR, LoadImageW, SM_CXSMICON,
};
use windows::core::{HSTRING, PCWSTR};

/// What the badge says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Badge {
    None,
    Recording,
    Paused,
}

impl Badge {
    pub(super) fn for_state(state: &SessionState) -> Self {
        match state {
            SessionState::Idle => Self::None,
            SessionState::Recording => Self::Recording,
            SessionState::Paused => Self::Paused,
        }
    }

    fn color(self) -> Option<egui::Color32> {
        match self {
            Self::None => None,
            Self::Recording => Some(Palette::DARK.danger),
            Self::Paused => Some(Palette::DARK.warn),
        }
    }

    /// What a screen reader says about the taskbar button's badge.
    fn description(self, s: &Strings) -> &'static str {
        match self {
            Self::None => "",
            Self::Recording => s.state_recording,
            Self::Paused => s.state_paused,
        }
    }
}

/// A `size`×`size` RGBA picture (straight alpha), the layout Windows' icons
/// and the tray's `Icon::from_rgba` take.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Pixels {
    pub(super) rgba: Vec<u8>,
    pub(super) size: u32,
}

impl Pixels {
    fn transparent(size: u32) -> Self {
        Self {
            rgba: vec![0; (size * size * 4) as usize],
            size,
        }
    }

    /// `color`'s dot, `radius` pixels, centred at `center`, with a
    /// transparent gap `gap` pixels wide cut out around it -- so on top of
    /// the app icon it reads as a badge rather than a smudge, on a light or
    /// a dark taskbar alike. Edges are anti-aliased.
    fn draw_dot(&mut self, center: (f32, f32), radius: f32, gap: f32, color: egui::Color32) {
        let cut = radius + gap;
        for y in 0..self.size {
            for x in 0..self.size {
                let dx = x as f32 + 0.5 - center.0;
                let dy = y as f32 + 0.5 - center.1;
                let distance = (dx * dx + dy * dy).sqrt();
                let cut_cover = (cut + 0.5 - distance).clamp(0.0, 1.0);
                if cut_cover == 0.0 {
                    continue;
                }
                let i = ((y * self.size + x) * 4) as usize;
                let px = &mut self.rgba[i..i + 4];
                px[3] = (f32::from(px[3]) * (1.0 - cut_cover)).round() as u8;
                over(px, color, (radius + 0.5 - distance).clamp(0.0, 1.0));
            }
        }
    }
}

/// Paints `color` at `cover` (0..=1) over the straight-alpha pixel `px`.
fn over(px: &mut [u8], color: egui::Color32, cover: f32) {
    if cover == 0.0 {
        return;
    }
    let below = f32::from(px[3]) / 255.0 * (1.0 - cover);
    let alpha = cover + below;
    for (channel, value) in px[..3].iter_mut().zip([color.r(), color.g(), color.b()]) {
        *channel = ((f32::from(value) * cover + f32::from(*channel) * below) / alpha).round() as u8;
    }
    px[3] = (alpha * 255.0).round() as u8;
}

/// The tray icon with `badge`'s dot in its bottom-right corner, covering a
/// quarter of it; `icon` itself for `Badge::None`.
pub(super) fn tray_icon_with(icon: &Pixels, badge: Badge) -> Pixels {
    let mut pixels = icon.clone();
    if let Some(color) = badge.color() {
        let size = icon.size as f32;
        let radius = size / 4.0;
        let gap = (size / 16.0).max(1.0);
        pixels.draw_dot((size - radius, size - radius), radius, gap, color);
    }
    pixels
}

/// The taskbar button's overlay for `badge`: just the dot, which Windows
/// puts on the button's bottom-right corner.
fn overlay_icon(badge: Badge, size: u32) -> Option<Pixels> {
    let color = badge.color()?;
    let mut pixels = Pixels::transparent(size);
    let half = size as f32 / 2.0;
    pixels.draw_dot((half, half), half * 0.75, 0.0, color);
    Some(pixels)
}

/// The dashboard window's handle, for its taskbar button.
pub(super) fn dashboard_hwnd(cc: &eframe::CreationContext<'_>) -> Option<HWND> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(HWND(handle.hwnd.get() as *mut _)),
        _ => None,
    }
}

/// The size Windows draws small icons at (16 at 100% scale, 32 at 200%) --
/// the tray's, and the taskbar overlay's.
pub(super) fn small_icon_size() -> u32 {
    match unsafe { GetSystemMetrics(SM_CXSMICON) } {
        size if size > 0 => size as u32,
        _ => 16,
    }
}

/// The application icon `build.rs` embeds (winresource's default id), at
/// `size`: the size drawn for it in the .ico, rather than a scaled one.
/// `None` if it can't be read, e.g. in a build without the resource.
pub(super) fn app_icon_pixels(size: u32) -> Option<Pixels> {
    const APP_ICON_RESOURCE: usize = 1;
    unsafe {
        let module = GetModuleHandleW(None).ok()?;
        let handle = LoadImageW(
            Some(module.into()),
            PCWSTR(APP_ICON_RESOURCE as *const u16),
            IMAGE_ICON,
            size as i32,
            size as i32,
            LR_DEFAULTCOLOR,
        )
        .ok()?;
        let icon = HICON(handle.0);
        let pixels = icon_pixels(icon, size);
        let _ = DestroyIcon(icon);
        pixels
    }
}

/// `icon`'s colour bitmap as RGBA, assuming it's `size`×`size`.
unsafe fn icon_pixels(icon: HICON, size: u32) -> Option<Pixels> {
    let mut info = ICONINFO::default();
    unsafe { GetIconInfo(icon, &mut info) }.ok()?;
    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size as i32,
            // Negative: top-down rows.
            biHeight: -(size as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bgra = vec![0u8; (size * size * 4) as usize];
    let lines = unsafe {
        let dc = GetDC(None);
        let lines = GetDIBits(
            dc,
            info.hbmColor,
            0,
            size,
            Some(bgra.as_mut_ptr().cast()),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        ReleaseDC(None, dc);
        let _ = DeleteObject(info.hbmColor.into());
        let _ = DeleteObject(info.hbmMask.into());
        lines
    };
    if lines != size as i32 {
        return None;
    }
    for px in bgra.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    Some(Pixels { rgba: bgra, size })
}

/// An icon Windows can show, made from `pixels`. Destroyed on drop.
struct OwnedIcon(HICON);

impl OwnedIcon {
    fn new(pixels: &Pixels) -> windows::core::Result<Self> {
        let size = pixels.size as i32;
        let bgra: Vec<u8> = pixels
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|&[r, g, b, a]| [b, g, r, a])
            .collect();
        // Every pixel "keep": the colour bitmap's alpha decides what shows.
        // Rows of the mask are padded to 16 bits.
        let mask = vec![0u8; (pixels.size.div_ceil(16) * 2 * pixels.size) as usize];
        unsafe { CreateIcon(None, size, size, 1, 32, mask.as_ptr(), bgra.as_ptr()) }.map(Self)
    }
}

impl Drop for OwnedIcon {
    fn drop(&mut self) {
        let _ = unsafe { DestroyIcon(self.0) };
    }
}

/// The dashboard's taskbar button's badge (`ITaskbarList3::SetOverlayIcon`).
pub(super) struct TaskbarBadge {
    hwnd: HWND,
    list: ITaskbarList3,
    recording: OwnedIcon,
    paused: OwnedIcon,
    /// What the button shows now, as far as is known: `None` until set, and
    /// again after the button was gone (see `forget`).
    shown: Option<Badge>,
}

impl TaskbarBadge {
    /// For the window `hwnd`. Must be made on the UI thread, which winit has
    /// already set up for COM.
    pub(super) fn new(hwnd: HWND) -> windows::core::Result<Self> {
        let size = small_icon_size();
        let icon = |badge| OwnedIcon::new(&overlay_icon(badge, size).expect("a badge with a dot"));
        let list: ITaskbarList3 =
            unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER)? };
        unsafe { list.HrInit()? };
        Ok(Self {
            hwnd,
            list,
            recording: icon(Badge::Recording)?,
            paused: icon(Badge::Paused)?,
            shown: None,
        })
    }

    /// Puts `badge` on the button, if it isn't on it already. A button
    /// that isn't there yet (Windows makes it after the window appears) is
    /// tried again next time.
    pub(super) fn show(&mut self, badge: Badge, s: &Strings) {
        if self.shown == Some(badge) {
            return;
        }
        let icon = match badge {
            Badge::None => HICON::default(),
            Badge::Recording => self.recording.0,
            Badge::Paused => self.paused.0,
        };
        let description = HSTRING::from(badge.description(s));
        match unsafe { self.list.SetOverlayIcon(self.hwnd, icon, &description) } {
            Ok(()) => {
                tracing::debug!("taskbar badge: {badge:?}");
                self.shown = Some(badge);
            }
            Err(e) => tracing::debug!("taskbar badge: {e}"),
        }
    }

    /// The button went away -- the dashboard was hidden in the tray -- and
    /// comes back without its badge.
    pub(super) fn forget(&mut self) {
        self.shown = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

    const BLUE: [u8; 4] = [0, 0, 255, 255];

    fn filled(size: u32, px: [u8; 4]) -> Pixels {
        Pixels {
            rgba: px.repeat((size * size) as usize),
            size,
        }
    }

    fn at(pixels: &Pixels, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * pixels.size + x) * 4) as usize;
        pixels.rgba[i..i + 4].try_into().unwrap()
    }

    fn rgba(color: egui::Color32) -> [u8; 4] {
        [color.r(), color.g(), color.b(), 255]
    }

    #[test]
    fn the_state_decides_the_badge() {
        assert_eq!(Badge::for_state(&SessionState::Idle), Badge::None);
        assert_eq!(Badge::for_state(&SessionState::Recording), Badge::Recording);
        assert_eq!(Badge::for_state(&SessionState::Paused), Badge::Paused);
    }

    #[test]
    fn the_tray_badge_is_a_dot_in_the_corner_cut_out_of_the_icon() {
        for size in [16, 20, 24, 32] {
            let icon = filled(size, BLUE);
            let badged = tray_icon_with(&icon, Badge::Recording);
            // The dot's middle, a quarter of the way in from the corner.
            let mid = size - size / 4;
            assert_eq!(
                at(&badged, mid, mid),
                rgba(Palette::DARK.danger),
                "size {size}"
            );
            // The rest of the icon is untouched.
            assert_eq!(at(&badged, 0, 0), BLUE, "size {size}");
            assert_eq!(at(&badged, size - 1, 0), BLUE, "size {size}");
            // Between the two, the icon is cut away (a 1 px ring at 16 px,
            // so its anti-aliased edge keeps a trace).
            let gap = mid - size / 4 - 1;
            assert!(at(&badged, gap, mid)[3] < 32, "size {size}");
        }
    }

    #[test]
    fn paused_is_amber_and_idle_leaves_the_icon_as_it_is() {
        let icon = filled(16, BLUE);
        assert_eq!(tray_icon_with(&icon, Badge::None), icon);
        assert_eq!(
            at(&tray_icon_with(&icon, Badge::Paused), 12, 12),
            rgba(Palette::DARK.warn)
        );
    }

    #[test]
    fn the_overlay_is_just_the_dot_and_idle_has_none() {
        assert_eq!(overlay_icon(Badge::None, 16), None);
        let dot = overlay_icon(Badge::Recording, 16).unwrap();
        assert_eq!(at(&dot, 8, 8), rgba(Palette::DARK.danger));
        assert_eq!(at(&dot, 0, 0)[3], 0, "transparent around it");
    }

    #[test]
    fn the_dots_edge_blends_into_whats_under_it() {
        let icon = filled(32, BLUE);
        let badged = tray_icon_with(&icon, Badge::Recording);
        let edge = (0..32)
            .flat_map(|y| (0..32).map(move |x| (x, y)))
            .map(|(x, y)| at(&badged, x, y))
            .filter(|px| px[3] > 0 && px[3] < 255)
            .count();
        assert!(edge > 0, "anti-aliased, not jagged");
    }

    #[test]
    fn an_icon_made_from_pixels_is_that_size() {
        let icon = OwnedIcon::new(&overlay_icon(Badge::Recording, 24).unwrap()).unwrap();
        let back = unsafe { icon_pixels(icon.0, 24) }.unwrap();
        assert_eq!(at(&back, 12, 12), rgba(Palette::DARK.danger));
        assert_eq!(at(&back, 0, 0)[3], 0);
    }

    #[test]
    fn the_badge_is_described_in_every_language() {
        for lang in [Lang::En, Lang::Ja] {
            let s = lang.strings();
            assert_eq!(Badge::Recording.description(s), s.state_recording);
            assert_eq!(Badge::Paused.description(s), s.state_paused);
            assert_eq!(Badge::None.description(s), "");
        }
    }
}
