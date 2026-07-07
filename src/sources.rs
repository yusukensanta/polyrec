use crate::types::CaptureSource;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, DIB_RGB_COLORS,
};
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::System::ProcessStatus::GetModuleFileNameExW;
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ};
use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_SMALLICON};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, EnumWindows, GetIconInfo, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsWindowVisible, ICONINFO,
};
use windows::core::PCWSTR;

pub fn enumerate_sources() -> Vec<CaptureSource> {
    let mut sources: Vec<CaptureSource> = Vec::new();
    let sources_ptr = &mut sources as *mut Vec<CaptureSource> as isize;

    unsafe {
        let _ = EnumWindows(Some(enum_window_callback), LPARAM(sources_ptr));
    }

    sources
}

unsafe extern "system" fn enum_window_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let sources = &mut *(lparam.0 as *mut Vec<CaptureSource>);

    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1);
    }

    if GetWindowTextLengthW(hwnd) == 0 {
        return BOOL(1);
    }

    sources.push(capture_source_for_hwnd(hwnd));

    BOOL(1)
}

/// Builds a `CaptureSource` for an arbitrary window handle — used both by the
/// enumeration above (for windows that already passed its visible/titled filter)
/// and directly for a specific window the caller already knows about (e.g. the
/// current foreground window for a hotkey-triggered recording), where there's no
/// other candidate to fall back to if the title happens to be empty.
pub fn capture_source_for_hwnd(hwnd: HWND) -> CaptureSource {
    unsafe {
        let title_len = GetWindowTextLengthW(hwnd);
        let window_title = if title_len > 0 {
            let mut title_buf = vec![0u16; (title_len + 1) as usize];
            GetWindowTextW(hwnd, &mut title_buf);
            String::from_utf16_lossy(&title_buf[..title_len as usize])
        } else {
            String::new()
        };

        let mut process_id = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut process_id));

        let exe_path = get_exe_path(process_id);
        let exe_name = exe_path
            .as_deref()
            .and_then(|p| std::path::Path::new(p).file_name())
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "Unknown".into());
        let icon_rgba = exe_path.as_deref().and_then(extract_exe_icon_rgba);

        CaptureSource {
            process_id,
            window_title,
            exe_name,
            hwnd: hwnd.0 as usize,
            icon_rgba,
        }
    }
}

fn get_exe_path(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid).ok()?;
        let mut buf = vec![0u16; 260];
        let len = GetModuleFileNameExW(handle, None, &mut buf);
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        if len == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// Extracts the exe's small shell icon as straight-alpha, top-down RGBA bytes.
/// Best-effort: returns `None` on any failure rather than propagating an error —
/// a missing icon just means the source list row shows no icon, not a broken list.
fn extract_exe_icon_rgba(exe_path: &str) -> Option<(Vec<u8>, u32, u32)> {
    unsafe {
        let wide: Vec<u16> = exe_path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut info = SHFILEINFOW::default();
        let result = SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_SMALLICON,
        );
        if result == 0 || info.hIcon.is_invalid() {
            return None;
        }
        let hicon = info.hIcon;

        let mut icon_info = ICONINFO::default();
        if GetIconInfo(hicon, &mut icon_info).is_err() {
            let _ = DestroyIcon(hicon);
            return None;
        }
        let _ = DeleteObject(icon_info.hbmMask);

        let mut bmp = BITMAP::default();
        let bmp_size = std::mem::size_of::<BITMAP>() as i32;
        if GetObjectW(icon_info.hbmColor, bmp_size, Some(&mut bmp as *mut _ as *mut _)) == 0 {
            let _ = DeleteObject(icon_info.hbmColor);
            let _ = DestroyIcon(hicon);
            return None;
        }
        // BITMAP's width/height are signed (LONG); bmHeight in particular is
        // legitimately negative for top-down DIBs, which is the normal
        // representation for 32-bit alpha-channel icons. `as u32` on a negative
        // value reinterprets the two's-complement bits into a huge positive
        // number instead of erroring, which would then flow into the buffer-size
        // multiplication below and the GetDIBits scanline count. Shell icons are
        // always small, so bound them rather than trust an unusual value.
        let width = bmp.bmWidth.unsigned_abs();
        let height = bmp.bmHeight.unsigned_abs();
        const MAX_ICON_DIMENSION: u32 = 512;
        if width == 0 || height == 0 || width > MAX_ICON_DIMENSION || height > MAX_ICON_DIMENSION {
            let _ = DeleteObject(icon_info.hbmColor);
            let _ = DestroyIcon(hicon);
            return None;
        }
        let Some(buffer_len) = (width as usize)
            .checked_mul(height as usize)
            .and_then(|px| px.checked_mul(4))
        else {
            let _ = DeleteObject(icon_info.hbmColor);
            let _ = DestroyIcon(hicon);
            return None;
        };

        let hdc = GetDC(None);
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32), // negative = top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u8; buffer_len];
        let lines = GetDIBits(
            hdc,
            icon_info.hbmColor,
            0,
            height,
            Some(pixels.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        ReleaseDC(None, hdc);
        let _ = DeleteObject(icon_info.hbmColor);
        let _ = DestroyIcon(hicon);

        if lines == 0 {
            return None;
        }

        // GetDIBits fills BGRA (classic DIB order) — swap to RGBA.
        for px in pixels.chunks_exact_mut(4) {
            px.swap(0, 2);
        }

        Some((pixels, width, height))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerate_returns_at_least_one_window() {
        let sources = enumerate_sources();
        assert!(!sources.is_empty(), "expected at least one visible window");
    }

    #[test]
    fn capture_source_has_non_empty_title() {
        let sources = enumerate_sources();
        for s in &sources {
            assert!(!s.window_title.is_empty());
        }
    }

    #[test]
    fn capture_sources_have_nonzero_hwnd() {
        let sources = enumerate_sources();
        assert!(!sources.is_empty());
        for s in &sources {
            assert_ne!(s.hwnd, 0usize, "HWND should not be null for '{}'", s.window_title);
        }
    }

    #[test]
    fn at_least_one_source_has_an_extractable_icon() {
        // Best-effort: some system/shell windows won't resolve to a real exe path,
        // but on any normal desktop at least one visible window's exe icon extracts.
        let sources = enumerate_sources();
        assert!(
            sources.iter().any(|s| s.icon_rgba.is_some()),
            "expected at least one source with an extractable icon"
        );
    }

    #[test]
    fn capture_source_for_hwnd_matches_enumerated_entry() {
        // capture_source_for_hwnd should resolve the same window identity the
        // callback path found. Only hwnd is asserted exactly -- window_title/exe_name
        // are re-read live on a real, changing desktop a moment after enumeration,
        // so a title tick (e.g. a browser tab) between the two calls is a real,
        // possible outcome, not a bug in either path.
        let sources = enumerate_sources();
        let sample = sources.first().expect("expected at least one visible window");
        let hwnd = windows::Win32::Foundation::HWND(sample.hwnd as *mut core::ffi::c_void);
        let rebuilt = capture_source_for_hwnd(hwnd);
        assert_eq!(rebuilt.hwnd, sample.hwnd);
        assert_eq!(rebuilt.process_id, sample.process_id, "process identity must be stable even if the title changed");
    }

    #[test]
    fn extracted_icon_has_matching_buffer_length() {
        let sources = enumerate_sources();
        for s in &sources {
            if let Some((rgba, w, h)) = &s.icon_rgba {
                assert_eq!(rgba.len(), (*w * *h * 4) as usize, "RGBA buffer length must be width*height*4");
            }
        }
    }
}
