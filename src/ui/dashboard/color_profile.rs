//! The dashboard's colours on the monitor it's on.
//!
//! The palette is sRGB, as colours on the web are. A browser converts them
//! through the monitor's colour profile before they reach the screen; what
//! PolyRec draws went to the monitor as it was. On a wide-gamut monitor
//! (an Alienware AW2726DM's native gamut is half again as wide as sRGB's)
//! the same RGB then shows more saturated than the browser showed the
//! palette. `MonitorColors` converts the palette the way a browser does,
//! through the profile Windows has for the dashboard's monitor, with
//! Windows' own colour engine.

use super::theme::Palette;
use eframe::egui;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::UI::ColorSystem::{
    BEST_MODE, COLOR, COLOR_RGB, CloseColorProfile, CreateMultiProfileTransform,
    DeleteColorTransform, INDEX_DONT_CARE, INTENT_RELATIVE_COLORIMETRIC, LCS_sRGB,
    OpenColorProfileW, PROFILE, PROFILE_FILENAME, PROFILE_READ, RGBCOLOR, TranslateColors,
};

/// Follows the monitor the dashboard is on, and the profile Windows has for
/// it, and converts the palette through that profile.
#[derive(Default)]
pub(super) struct MonitorColors {
    monitor: Option<isize>,
    checked_at: Option<Instant>,
    profile: Option<PathBuf>,
    transform: Option<ColorTransform>,
}

impl MonitorColors {
    /// How often the profile is asked for again on the same monitor: it
    /// changes only in Settings.
    const CHECK_EVERY: Duration = Duration::from_secs(2);

    /// Called every frame with the dashboard's monitor.
    pub(super) fn update(&mut self, monitor: Option<HMONITOR>) {
        let id = monitor.map(|m| m.0 as isize);
        if id == self.monitor
            && self
                .checked_at
                .is_some_and(|at| at.elapsed() < Self::CHECK_EVERY)
        {
            return;
        }
        self.monitor = id;
        self.checked_at = Some(Instant::now());
        let profile = monitor.and_then(monitor_profile);
        if profile == self.profile {
            return;
        }
        self.transform = profile.as_deref().and_then(ColorTransform::srgb_to);
        match (&profile, &self.transform) {
            (Some(p), Some(_)) => tracing::info!("dashboard colours: through {}", p.display()),
            (Some(p), None) => tracing::warn!(
                "dashboard colours: no transform to {}; drawn as they are",
                p.display()
            ),
            (None, _) => tracing::info!(
                "dashboard colours: drawn as they are (no profile, or Windows manages colour)"
            ),
        }
        self.profile = profile;
    }

    /// `palette` for the dashboard's monitor.
    pub(super) fn convert(&self, palette: Palette) -> Palette {
        match &self.transform {
            Some(t) => palette.through(|c| t.apply(c)),
            None => palette,
        }
    }
}

/// sRGB to one monitor profile: a transform of Windows' colour engine
/// (mscms), relative colorimetric, as browsers convert.
pub(super) struct ColorTransform(isize);

impl ColorTransform {
    /// From sRGB to the ICC profile at `profile`; `None` if Windows can't
    /// open it or build the transform.
    pub(super) fn srgb_to(profile: &Path) -> Option<Self> {
        let srgb = OpenProfile::new(&standard_srgb_profile()?)?;
        let monitor = OpenProfile::new(profile)?;
        let intent = INTENT_RELATIVE_COLORIMETRIC;
        let handle = unsafe {
            CreateMultiProfileTransform(
                &[srgb.0, monitor.0],
                &[intent, intent],
                BEST_MODE,
                INDEX_DONT_CARE,
            )
        };
        (handle != 0).then_some(Self(handle))
    }

    /// `c` converted. Its alpha stays as it was.
    pub(super) fn apply(&self, c: egui::Color32) -> egui::Color32 {
        let [r, g, b, a] = c.to_srgba_unmultiplied();
        let wide = |v: u8| u16::from(v) * 257;
        let input = COLOR {
            rgb: RGBCOLOR {
                red: wide(r),
                green: wide(g),
                blue: wide(b),
            },
        };
        let mut output = COLOR::default();
        let translated =
            unsafe { TranslateColors(self.0, &input, 1, COLOR_RGB, &mut output, COLOR_RGB) };
        if !translated.as_bool() {
            return c;
        }
        let rgb = unsafe { output.rgb };
        let narrow = |v: u16| ((u32::from(v) + 128) / 257) as u8;
        egui::Color32::from_rgba_unmultiplied(
            narrow(rgb.red),
            narrow(rgb.green),
            narrow(rgb.blue),
            a,
        )
    }
}

impl Drop for ColorTransform {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteColorTransform(self.0);
        }
    }
}

/// An ICC profile opened for reading, closed when dropped.
struct OpenProfile(isize);

impl OpenProfile {
    fn new(path: &Path) -> Option<Self> {
        const FILE_SHARE_READ: u32 = 1;
        const OPEN_EXISTING: u32 = 3;
        if !path.is_file() {
            return None;
        }
        let name: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let profile = PROFILE {
            dwType: PROFILE_FILENAME,
            pProfileData: name.as_ptr() as *mut _,
            cbDataSize: (name.len() * size_of::<u16>()) as u32,
        };
        let handle =
            unsafe { OpenColorProfileW(&profile, PROFILE_READ, FILE_SHARE_READ, OPEN_EXISTING) };
        (handle != 0).then_some(Self(handle))
    }
}

impl Drop for OpenProfile {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseColorProfile(Some(self.0));
        }
    }
}

/// Windows' own sRGB profile.
fn standard_srgb_profile() -> Option<PathBuf> {
    use windows::Win32::UI::ColorSystem::GetStandardColorSpaceProfileW;
    let mut buffer = [0u16; 260];
    let mut size = (buffer.len() * size_of::<u16>()) as u32;
    let found = unsafe {
        GetStandardColorSpaceProfileW(
            None,
            LCS_sRGB.0 as u32,
            Some(windows::core::PWSTR(buffer.as_mut_ptr())),
            &mut size,
        )
    };
    found.as_bool().then(|| wide_path(&buffer))
}

/// The profile Windows has for `monitor` -- unless Windows manages colour
/// for it itself (HDR, or "Automatically manage color for apps"): it then
/// converts what PolyRec draws already, and converting here as well would
/// do it twice.
fn monitor_profile(monitor: HMONITOR) -> Option<PathBuf> {
    use windows::Win32::Graphics::Gdi::{
        CreateDCW, DeleteDC, GetMonitorInfoW, MONITORINFO, MONITORINFOEXW,
    };
    use windows::Win32::UI::ColorSystem::GetICMProfileW;
    let mut info = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }.as_bool() {
        return None;
    }
    let device = info.szDevice;
    if windows_manages_colour(&device) {
        return None;
    }
    unsafe {
        let dc = CreateDCW(
            windows::core::w!("DISPLAY"),
            windows::core::PCWSTR(device.as_ptr()),
            None,
            None,
        );
        if dc.is_invalid() {
            return None;
        }
        let mut buffer = [0u16; 260];
        let mut size = buffer.len() as u32;
        let found = GetICMProfileW(
            dc,
            &mut size,
            Some(windows::core::PWSTR(buffer.as_mut_ptr())),
        );
        let _ = DeleteDC(dc);
        found.as_bool().then(|| wide_path(&buffer))
    }
}

/// Whether Windows' advanced colour (HDR, or automatic colour management)
/// is on for the display whose GDI name is `device`.
fn windows_manages_colour(device: &[u16; 32]) -> bool {
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
        DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
        DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
        QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
    };
    let (mut paths, mut modes) = (0u32, 0u32);
    if unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut paths, &mut modes) }
        .is_err()
    {
        return false;
    }
    let mut path_info = vec![DISPLAYCONFIG_PATH_INFO::default(); paths as usize];
    let mut mode_info = vec![DISPLAYCONFIG_MODE_INFO::default(); modes as usize];
    if unsafe {
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut paths,
            path_info.as_mut_ptr(),
            &mut modes,
            mode_info.as_mut_ptr(),
            None,
        )
    }
    .is_err()
    {
        return false;
    }
    path_info.truncate(paths as usize);
    for path in path_info {
        let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                adapterId: path.sourceInfo.adapterId,
                id: path.sourceInfo.id,
            },
            ..Default::default()
        };
        if unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } != 0
            || source.viewGdiDeviceName != *device
        {
            continue;
        }
        let mut colour = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                size: size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32,
                adapterId: path.targetInfo.adapterId,
                id: path.targetInfo.id,
            },
            ..Default::default()
        };
        if unsafe { DisplayConfigGetDeviceInfo(&mut colour.header) } != 0 {
            return false;
        }
        // Bit 1 is advancedColorEnabled.
        return unsafe { colour.Anonymous.value } & 0b10 != 0;
    }
    false
}

/// A NUL-terminated wide string, as a path.
fn wide_path(buffer: &[u16]) -> PathBuf {
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    PathBuf::from(std::ffi::OsString::from_wide(&buffer[..len]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows' own sRGB profile, present on every Windows.
    fn srgb_profile() -> PathBuf {
        standard_srgb_profile().expect("Windows has an sRGB profile")
    }

    /// A Display P3 monitor profile (ICC v2, matrix and curves), written
    /// out: a wide-gamut monitor whose numbers are known, on any machine.
    fn display_p3_profile(dir: &Path) -> PathBuf {
        fn s15f16(v: f64) -> [u8; 4] {
            ((v * 65536.0).round() as i32).to_be_bytes()
        }
        fn xyz(x: f64, y: f64, z: f64) -> Vec<u8> {
            let mut t = b"XYZ \0\0\0\0".to_vec();
            for v in [x, y, z] {
                t.extend(s15f16(v));
            }
            t
        }
        // The sRGB curve, which Display P3 shares.
        let mut curve = b"curv\0\0\0\0".to_vec();
        curve.extend(1024u32.to_be_bytes());
        for i in 0..1024 {
            let v = f64::from(i) / 1023.0;
            let linear = if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            };
            curve.extend(((linear * 65535.0).round() as u16).to_be_bytes());
        }
        let mut desc = b"desc\0\0\0\0".to_vec();
        desc.extend(11u32.to_be_bytes());
        desc.extend(b"Display P3\0");
        desc.extend([0u8; 4 + 4 + 2 + 1 + 67]);
        let mut cprt = b"text\0\0\0\0".to_vec();
        cprt.extend(b"none\0");
        // Primaries adapted to the D50 of the profile connection space.
        let tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
            (b"desc", desc),
            (b"cprt", cprt),
            (b"wtpt", xyz(0.9642, 1.0, 0.8249)),
            (b"rXYZ", xyz(0.5151, 0.2412, -0.0011)),
            (b"gXYZ", xyz(0.2920, 0.6922, 0.0419)),
            (b"bXYZ", xyz(0.1571, 0.0666, 0.7841)),
            (b"rTRC", curve.clone()),
            (b"gTRC", curve.clone()),
            (b"bTRC", curve),
        ];
        let mut table = (tags.len() as u32).to_be_bytes().to_vec();
        let mut data = Vec::new();
        let mut offset = 128 + 4 + 12 * tags.len();
        for (sig, bytes) in &tags {
            table.extend(*sig);
            table.extend((offset as u32).to_be_bytes());
            table.extend((bytes.len() as u32).to_be_bytes());
            data.extend(bytes);
            while data.len() % 4 != 0 {
                data.push(0);
            }
            offset = 128 + 4 + 12 * tags.len() + data.len();
        }
        let size = 128 + table.len() + data.len();
        let mut header = Vec::with_capacity(128);
        header.extend((size as u32).to_be_bytes());
        header.extend([0u8; 4]);
        header.extend([2, 0x10, 0, 0]);
        header.extend(b"mntrRGB XYZ ");
        for v in [2026u16, 1, 1, 0, 0, 0] {
            header.extend(v.to_be_bytes());
        }
        header.extend(b"acspMSFT");
        header.extend([0u8; 4 + 4 + 4 + 8 + 4]);
        for v in [0.9642, 1.0, 0.8249] {
            header.extend(s15f16(v));
        }
        header.resize(128, 0);
        let path = dir.join("display-p3.icc");
        std::fs::write(&path, [header, table, data].concat()).unwrap();
        path
    }

    fn assert_near(actual: egui::Color32, expected: [u8; 3], tolerance: u8) {
        let [r, g, b, _] = actual.to_array();
        for (a, e) in [r, g, b].into_iter().zip(expected) {
            assert!(a.abs_diff(e) <= tolerance, "{actual:?} vs {expected:?}");
        }
    }

    #[test]
    fn srgb_to_srgb_changes_nothing() {
        let t = ColorTransform::srgb_to(&srgb_profile()).expect("a transform");
        for c in [
            [0x14, 0x15, 0x1d],
            [0x5b, 0x61, 0xe6],
            [0xc9, 0x34, 0x3a],
            [255, 255, 255],
        ] {
            assert_near(t.apply(egui::Color32::from_rgb(c[0], c[1], c[2])), c, 1);
        }
    }

    /// What a browser shows for sRGB red on a Display P3 monitor: the RGB
    /// that makes the monitor show sRGB's red, not its own deeper one.
    #[test]
    fn a_wide_gamut_monitor_gets_the_colours_a_browser_would_send_it() {
        let dir = tempfile::tempdir().unwrap();
        let t = ColorTransform::srgb_to(&display_p3_profile(dir.path())).expect("a transform");
        assert_near(
            t.apply(egui::Color32::from_rgb(255, 0, 0)),
            [234, 51, 35],
            3,
        );
        assert_near(
            t.apply(egui::Color32::from_rgb(0xc9, 0x34, 0x3a)),
            [185, 65, 64],
            3,
        );
        // Greys are the same in both: still grey.
        assert_near(t.apply(egui::Color32::from_gray(128)), [128, 128, 128], 1);
        // Alpha is left alone.
        let translucent = egui::Color32::from_rgba_unmultiplied(255, 0, 0, 128);
        assert_eq!(t.apply(translucent).a(), 128);
    }

    #[test]
    fn a_profile_that_is_not_there_gives_no_transform() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ColorTransform::srgb_to(&dir.path().join("missing.icc")).is_none());
    }
}
