use crate::types::CaptureSource;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::System::ProcessStatus::GetModuleFileNameExW;
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
};

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

    let title_len = GetWindowTextLengthW(hwnd);
    if title_len == 0 {
        return BOOL(1);
    }

    let mut title_buf = vec![0u16; (title_len + 1) as usize];
    GetWindowTextW(hwnd, &mut title_buf);
    let window_title = String::from_utf16_lossy(&title_buf[..title_len as usize]);

    let mut process_id = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut process_id));

    let exe_name = get_exe_name(process_id).unwrap_or_else(|| "Unknown".into());

    sources.push(CaptureSource {
        process_id,
        window_title,
        exe_name,
        hwnd: hwnd.0 as usize,
    });

    BOOL(1)
}

fn get_exe_name(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid).ok()?;
        let mut buf = vec![0u16; 260];
        let len = GetModuleFileNameExW(handle, None, &mut buf);
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        if len == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        std::path::Path::new(&path)
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
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
}
