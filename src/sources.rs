use crate::types::{CaptureKind, CaptureSource};
use std::os::windows::ffi::OsStrExt;
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, EnumDisplayMonitors, GetDC,
    GetDIBits, GetMonitorInfoW, GetObjectW, HDC, HMONITOR, MONITORINFO, ReleaseDC,
};
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_SMALLICON, SHGetFileInfoW};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetIconInfo, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
    ICONINFO, IsWindowVisible, MONITORINFOF_PRIMARY,
};
use windows::core::{BOOL, Owned};
use windows::core::{PCWSTR, PWSTR};

/// What a display's title starts with, standing in for the icon a window's
/// exe gives it.
pub const MONITOR_GLYPH: &str = "🖥";

/// Displays first, then windows -- a monitor is the "record everything"
/// fallback a user reaches for before narrowing down to a specific window,
/// so it reads better leading the list than buried after however many
/// windows happen to be open.
pub fn enumerate_sources() -> Vec<CaptureSource> {
    let mut sources = enumerate_monitors();

    let windows_ptr = &mut sources as *mut Vec<CaptureSource> as isize;
    unsafe {
        let _ = EnumWindows(Some(enum_window_callback), LPARAM(windows_ptr));
    }

    sources
}

/// One `CaptureSource` per connected display, via `EnumDisplayMonitors` --
/// each capturable independently through Windows.Graphics.Capture's
/// `CreateForMonitor` (see `capture::video::CaptureTarget`), since WGC has no
/// single "all monitors combined" capture item of its own.
fn enumerate_monitors() -> Vec<CaptureSource> {
    let mut monitors: Vec<CaptureSource> = Vec::new();
    let monitors_ptr = &mut monitors as *mut Vec<CaptureSource> as isize;

    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitor_callback),
            LPARAM(monitors_ptr),
        );
    }

    monitors
}

unsafe extern "system" fn enum_monitor_callback(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    unsafe {
        let monitors = &mut *(lparam.0 as *mut Vec<CaptureSource>);

        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(hmonitor, &mut info).as_bool() {
            return BOOL(1);
        }

        let width = info.rcMonitor.right - info.rcMonitor.left;
        let height = info.rcMonitor.bottom - info.rcMonitor.top;
        let primary_suffix = if info.dwFlags & MONITORINFOF_PRIMARY != 0 {
            " (Primary)"
        } else {
            ""
        };
        let window_title = format!(
            "🖥 Display {}{primary_suffix} ({width}×{height})",
            monitors.len() + 1
        );

        monitors.push(CaptureSource {
            kind: CaptureKind::Monitor,
            process_id: 0,
            window_title,
            exe_name: String::new(),
            hwnd: hmonitor.0 as usize,
            icon_rgba: None,
        });

        BOOL(1)
    }
}

unsafe extern "system" fn enum_window_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
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
            .and_then(exe_file_name)
            .unwrap_or_else(|| "Unknown".into());
        let icon_rgba = exe_path.as_deref().and_then(extract_exe_icon_rgba);

        CaptureSource {
            kind: CaptureKind::Window,
            process_id,
            window_title,
            exe_name,
            hwnd: hwnd.0 as usize,
            icon_rgba,
        }
    }
}

/// `pub(crate)` (not just used within this module) -- also used by
/// `capture::audio::enumerate_app_audio_sessions` to resolve a per-app audio
/// session's exe name/icon from its process id, the same way a capture
/// source's exe name/icon are resolved from its window's process id here.
pub(crate) fn get_exe_path(pid: u32) -> Option<String> {
    unsafe {
        // PROCESS_QUERY_LIMITED_INFORMATION is all QueryFullProcessImageNameW
        // needs. Windows' integrity-level and protected-process rules still
        // grant it for elevated and protected processes (where their DACL
        // allows the caller at all) -- unlike the PROCESS_QUERY_INFORMATION |
        // PROCESS_VM_READ that GetModuleFileNameExW needed, which those rules
        // deny for exactly the processes this app most wants to name (games
        // under anti-cheat, elevated launchers), leaving them "Unknown" with
        // no icon.
        let handle = Owned::new(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?);
        // Starts at MAX_PATH (the common case) and doubles up to Windows'
        // long-path limit on ERROR_INSUFFICIENT_BUFFER, so an exe under a
        // long node_modules-style or deeply-nested OneDrive path isn't lost.
        let mut buf_len: u32 = 260;
        loop {
            let mut buf = vec![0u16; buf_len as usize];
            let mut len = buf_len;
            match QueryFullProcessImageNameW(
                *handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            ) {
                Ok(()) => return Some(String::from_utf16_lossy(&buf[..len as usize])),
                Err(e) if e.code() == ERROR_INSUFFICIENT_BUFFER.to_hresult() && buf_len < 32768 => {
                    buf_len *= 2;
                }
                Err(_) => return None,
            }
        }
    }
}

/// The exe file name of `pid`'s image (e.g. "Discord.exe"), or `None` if
/// the process can't be opened (already exited, or access denied) -- the
/// identity both capture sources and per-app audio sessions are keyed on.
pub(crate) fn exe_name_for_pid(pid: u32) -> Option<String> {
    get_exe_path(pid).as_deref().and_then(exe_file_name)
}

/// The file-name part of an exe path (e.g. `C:\...\Discord.exe` ->
/// "Discord.exe").
pub(crate) fn exe_file_name(exe_path: &str) -> Option<String> {
    std::path::Path::new(exe_path)
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
}

/// `name` without a trailing `.exe` in any letter case (e.g. "Discord.Exe"
/// -> "Discord"), unchanged if it has none. Shared by every exe-name-derived
/// label and filename prefix so the convention lives in one place. Never
/// slices inside a multi-byte character, whatever `name` contains.
pub(crate) fn strip_exe_suffix(name: &str) -> &str {
    match name.len().checked_sub(4) {
        Some(i)
            if name
                .get(i..)
                .is_some_and(|ext| ext.eq_ignore_ascii_case(".exe")) =>
        {
            &name[..i]
        }
        _ => name,
    }
}

/// Display label for an exe filename (e.g. "Discord.exe" -> "Discord") --
/// used by every exe_name-to-display_name derivation in the app-audio path
/// (`capture::audio::enumerate_app_audio_sessions`,
/// `ui::dashboard::actions::build_app_audio_sources`, the add-app picker's
/// open-window candidates).
pub(crate) fn display_name_from_exe_name(exe_name: &str) -> String {
    strip_exe_suffix(exe_name).to_string()
}

/// Extracts the exe's small shell icon as straight-alpha, top-down RGBA bytes.
/// Best-effort: returns `None` on any failure rather than propagating an error —
/// a missing icon just means the source list row shows no icon, not a broken list.
/// `pub(crate)` -- see `get_exe_path`'s doc comment.
///
/// Memoized per exe path: the periodic source refresh asks for the icon of
/// every open window every 2 s, and the shell extraction dominated that
/// refresh's cost. A cached icon is only reused while the file's size and
/// modification time still match what they were when it was extracted, so
/// an exe replaced on disk (an app update) is re-extracted, exactly as if
/// nothing were cached. Failures are never cached -- they're retried on the
/// next call, same as before the cache existed.
pub(crate) fn extract_exe_icon_rgba(exe_path: &str) -> Option<(Vec<u8>, u32, u32)> {
    static CACHE: std::sync::OnceLock<IconCache> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(IconCache::default)
        .get_or_extract(exe_path, extract_exe_icon_rgba_uncached)
}

type Icon = (Vec<u8>, u32, u32);
/// A file's (modification time, size) -- what a cached icon is keyed on
/// besides its path.
type FileStamp = (std::time::SystemTime, u64);

/// `extract_exe_icon_rgba`'s memo, separate from the shell call it wraps so
/// its reuse/invalidation rules can be tested without one.
#[derive(Default)]
struct IconCache {
    entries: std::sync::Mutex<std::collections::HashMap<String, (FileStamp, Icon)>>,
}

impl IconCache {
    fn get_or_extract(
        &self,
        exe_path: &str,
        extract: impl FnOnce(&str) -> Option<Icon>,
    ) -> Option<Icon> {
        let Some(stamp) = std::fs::metadata(exe_path)
            .ok()
            .and_then(|m| Some((m.modified().ok()?, m.len())))
        else {
            return extract(exe_path);
        };
        if let Some((cached_stamp, icon)) = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(exe_path)
            && *cached_stamp == stamp
        {
            return Some(icon.clone());
        }
        // Extracted with the lock released -- the shell call is the slow part
        // and callers on other threads shouldn't queue behind it.
        let icon = extract(exe_path)?;
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(exe_path.to_owned(), (stamp, icon.clone()));
        Some(icon)
    }
}

fn extract_exe_icon_rgba_uncached(exe_path: &str) -> Option<(Vec<u8>, u32, u32)> {
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
        // Every handle below is owned from here on and released on every
        // return path (DestroyIcon / DeleteObject), early returns included.
        let hicon = Owned::new(info.hIcon);

        let mut icon_info = ICONINFO::default();
        GetIconInfo(*hicon, &mut icon_info).ok()?;
        let _mask = Owned::new(icon_info.hbmMask);
        let color = Owned::new(icon_info.hbmColor);

        let mut bmp = BITMAP::default();
        let bmp_size = std::mem::size_of::<BITMAP>() as i32;
        if GetObjectW(
            (*color).into(),
            bmp_size,
            Some(&mut bmp as *mut _ as *mut _),
        ) == 0
        {
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
            return None;
        }
        let buffer_len = (width as usize)
            .checked_mul(height as usize)
            .and_then(|px| px.checked_mul(4))?;

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
            *color,
            0,
            height,
            Some(pixels.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        ReleaseDC(None, hdc);

        if lines == 0 {
            return None;
        }

        // GetDIBits fills BGRA (classic DIB order) — swap to RGBA.
        for px in pixels.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }

        Some((pixels, width, height))
    }
}

/// An app found by resolving Start Menu shortcuts -- not necessarily
/// running. Used by the Audio popup's add-app picker (see
/// `enumerate_installed_apps`) so an app can be pinned for future recording
/// without needing to already be open.
pub struct InstalledApp {
    pub display_name: String,
    /// e.g. "Discord.exe" -- derived from `exe_path`, used as the identity
    /// key for `Config::register_app_audio`, same convention as
    /// `AppAudioSource::exe_name`.
    pub exe_name: String,
    pub exe_path: String,
    pub icon_rgba: Option<(Vec<u8>, u32, u32)>,
}

/// Finds installed desktop apps by resolving every `.lnk` shortcut under the
/// per-user and all-users Start Menu ("Programs") folders to its target exe
/// -- the same set Windows' own Start Menu search draws from. Deliberately
/// does NOT need any app to be running: unlike `enumerate_sources` (open
/// windows) or `capture::audio::enumerate_app_audio_sessions` (live audio
/// sessions), this lets the Audio popup's add-app picker find an app the
/// user hasn't launched yet, so pinning it doesn't require opening it first
/// just to be findable.
///
/// Best-effort throughout: an unreadable Start Menu folder, a shortcut that
/// fails to resolve, or a target that isn't an existing `.exe` is silently
/// skipped rather than surfaced as an error -- same convention as
/// `extract_exe_icon_rgba`. Deduped by resolved exe path (case-insensitive),
/// since multiple shortcuts commonly point at the same exe (e.g. a "Safe
/// Mode" variant alongside the normal one).
pub fn enumerate_installed_apps() -> Vec<InstalledApp> {
    // Resolving shortcuts takes COM. The apartment is left again on the way
    // out: this runs on tokio's blocking pool, whose threads the capture
    // loops reuse, and they need a multithreaded one.
    let _com = SingleThreadedApartment::join();

    let mut start_menu_dirs = Vec::new();
    if let Some(appdata) = dirs::data_dir() {
        start_menu_dirs.push(
            appdata
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs"),
        );
    }
    if let Ok(program_data) = std::env::var("ProgramData") {
        start_menu_dirs.push(
            std::path::PathBuf::from(program_data)
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs"),
        );
    }

    let mut lnk_paths = Vec::new();
    for dir in &start_menu_dirs {
        collect_lnk_files(dir, &mut lnk_paths);
    }

    let mut seen_exe_paths = std::collections::HashSet::new();
    let mut apps = Vec::new();
    for lnk_path in lnk_paths {
        let Some(exe_path) = resolve_shortcut_target(&lnk_path) else {
            continue;
        };
        if !exe_path.to_lowercase().ends_with(".exe") || !std::path::Path::new(&exe_path).exists() {
            continue;
        }
        if !seen_exe_paths.insert(exe_path.to_lowercase()) {
            continue;
        }
        let Some(display_name) = lnk_path
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let Some(exe_name) = std::path::Path::new(&exe_path)
            .file_name()
            .and_then(|s| s.to_str())
        else {
            continue;
        };
        apps.push(InstalledApp {
            display_name: display_name.to_string(),
            exe_name: exe_name.to_string(),
            icon_rgba: extract_exe_icon_rgba(&exe_path),
            exe_path,
        });
    }
    apps.sort_by_key(|a| a.display_name.to_lowercase());
    apps
}

/// Recursively collects every `.lnk` file under `dir` into `out` -- a
/// missing or unreadable directory (e.g. the all-users Start Menu folder
/// under a restricted profile) is silently skipped, same best-effort
/// convention as the rest of this module's shell-facing code.
fn collect_lnk_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_lnk_files(&path, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
        {
            out.push(path);
        }
    }
}

/// Resolves a `.lnk` shortcut to its target path via the standard
/// `IShellLinkW` + `IPersistFile` COM pattern.
fn resolve_shortcut_target(lnk_path: &std::path::Path) -> Option<String> {
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile, STGM_READ,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::core::Interface;

    let (target_path, arguments) = unsafe {
        let shell_link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let persist_file: IPersistFile = shell_link.cast().ok()?;
        let wide_lnk: Vec<u16> = lnk_path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        persist_file
            .Load(PCWSTR(wide_lnk.as_ptr()), STGM_READ)
            .ok()?;

        let mut path_buf = [0u16; 260]; // MAX_PATH
        let mut find_data = windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW::default();
        shell_link.GetPath(&mut path_buf, &mut find_data, 0).ok()?;
        let path_end = path_buf
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(path_buf.len());
        if path_end == 0 {
            return None;
        }
        let target_path = String::from_utf16_lossy(&path_buf[..path_end]);

        let mut args_buf = [0u16; 260];
        let arguments = if shell_link.GetArguments(&mut args_buf).is_ok() {
            let args_end = args_buf
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(args_buf.len());
            String::from_utf16_lossy(&args_buf[..args_end])
        } else {
            String::new()
        };
        (target_path, arguments)
    };

    // Squirrel (the updater framework Discord, Slack, and many other
    // Electron apps use) points its Start Menu shortcut at `Update.exe
    // --processStart <AppExe>` rather than the real app directly --
    // Update.exe is a launcher stub that spawns the real exe (from
    // whichever `app-<version>` folder is current) and exits immediately,
    // so pinning it as-is would scope audio capture to a process that's
    // already gone by the time anything could produce sound. Resolve
    // through to the real exe instead of returning the stub; if that fails,
    // treat the shortcut as unresolvable rather than returning a target
    // known to be useless.
    if std::path::Path::new(&target_path)
        .file_name()
        .and_then(|f| f.to_str())
        .is_some_and(|f| f.eq_ignore_ascii_case("Update.exe"))
    {
        return parse_process_start_arg(&arguments).and_then(|app_exe| {
            let parent = std::path::Path::new(&target_path).parent()?;
            resolve_squirrel_app_exe(parent, &app_exe)
        });
    }

    Some(target_path)
}

/// Extracts the exe name from a Squirrel shortcut's `--processStart
/// <AppExe>` argument (optionally quoted) -- see `resolve_shortcut_target`.
fn parse_process_start_arg(arguments: &str) -> Option<String> {
    let mut tokens = arguments.split_whitespace();
    while let Some(tok) = tokens.next() {
        if tok.eq_ignore_ascii_case("--processStart") {
            return tokens.next().map(|s| s.trim_matches('"').to_string());
        }
    }
    None
}

/// Finds `<app_exe_name>` under whichever `app-<version>` sibling folder of
/// `parent` has it most recently modified -- Squirrel keeps the live app in
/// a versioned folder next to `Update.exe` and typically only the current
/// version's folder survives an update, but picking by mtime rather than
/// assuming exactly one folder or trying to parse/compare version strings
/// handles both the common case and a stale leftover folder from an
/// interrupted update. Takes the parent directory directly (rather than
/// deriving it from an `Update.exe` path) so `resolve_possibly_stale_exe_path`
/// can reuse this same scan starting from an already-registered exe's own
/// path instead of a shortcut's `Update.exe` target.
fn resolve_squirrel_app_exe(parent: &std::path::Path, app_exe_name: &str) -> Option<String> {
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for entry in std::fs::read_dir(parent).ok()?.flatten() {
        let dir = entry.path();
        let is_app_dir = dir.is_dir()
            && dir
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("app-"));
        if !is_app_dir {
            continue;
        }
        let candidate = dir.join(app_exe_name);
        let Ok(modified) = std::fs::metadata(&candidate).and_then(|m| m.modified()) else {
            continue;
        };
        if best.as_ref().is_none_or(|(t, _)| modified > *t) {
            best = Some((modified, candidate));
        }
    }
    best.map(|(_, path)| path.to_string_lossy().into_owned())
}

/// `exe_path` if it still exists, otherwise a best-effort re-resolution for
/// the common case that breaks it: a Squirrel-updated app (Discord, Slack,
/// and other Electron apps using the same updater) moving itself into a new
/// `app-<version>` sibling folder and removing the old one out from under an
/// already-registered `RegisteredApp::exe_path` -- see
/// `ui::dashboard::actions::build_app_audio_sources`'s doc comment for where
/// this matters (a registered-but-not-currently-running app's icon has
/// nothing else to extract from). Falls back to `exe_path` unchanged if
/// re-resolution doesn't find anything -- same "best effort, never worse
/// than before" convention as `extract_exe_icon_rgba`.
pub(crate) fn resolve_possibly_stale_exe_path(exe_path: &str) -> String {
    if std::path::Path::new(exe_path).exists() {
        return exe_path.to_string();
    }
    let resolved = (|| {
        let exe_name = std::path::Path::new(exe_path).file_name()?.to_str()?;
        // `exe_path` is expected to look like `.../<App>/app-<old-version>/Foo.exe`
        // -- its grandparent (`.../<App>/`) is where Squirrel's sibling
        // `app-<version>` folders live, the same shape `resolve_squirrel_app_exe`
        // already scans starting from `Update.exe`'s own parent.
        let grandparent = std::path::Path::new(exe_path).parent()?.parent()?;
        resolve_squirrel_app_exe(grandparent, exe_name)
    })();
    resolved.unwrap_or_else(|| exe_path.to_string())
}

/// A single-threaded COM apartment, joined for as long as this lives -- the
/// one shell calls (`SHGetFileInfoW`, `IShellLinkW`) expect -- and left again
/// when it's dropped. A thread that was already in a multithreaded apartment
/// stays there (joining fails), which those calls cope with.
pub(crate) struct SingleThreadedApartment {
    joined: bool,
}

impl SingleThreadedApartment {
    pub(crate) fn join() -> Self {
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
        // S_FALSE (already joined) counts too: each success needs its own
        // CoUninitialize.
        let joined = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        Self { joined }
    }
}

impl Drop for SingleThreadedApartment {
    fn drop(&mut self) {
        if self.joined {
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `enumerate_installed_apps` runs on tokio's blocking pool (the Audio
    /// popup's app picker), whose threads are reused for a recording's
    /// capture loops -- which need a multithreaded COM apartment. It used to
    /// leave its thread in a single-threaded one, and a capture started on
    /// that thread then failed before its first frame: a recording with no
    /// video.
    #[test]
    fn enumerating_installed_apps_leaves_the_thread_free_for_a_capture_loop() {
        use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
        std::thread::spawn(|| {
            let _ = enumerate_installed_apps();
            let joined = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            assert!(
                joined.is_ok(),
                "the thread was left in another apartment: {joined:?}"
            );
            unsafe { CoUninitialize() };
        })
        .join()
        .unwrap();
    }

    /// The previous implementation needed PROCESS_QUERY_INFORMATION |
    /// PROCESS_VM_READ (for GetModuleFileNameExW). Integrity-level and
    /// protected-process rules deny VM_READ far more often than
    /// QUERY_LIMITED_INFORMATION, so every process that old access level
    /// could open must still resolve now -- on a real desktop the new path
    /// additionally names elevated/protected ones (Taskmgr.exe, audiodg.exe,
    /// ...) the old one couldn't.
    #[test]
    fn get_exe_path_resolves_every_process_the_old_access_level_could_open() {
        use windows::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        };
        use windows::Win32::System::Threading::{PROCESS_QUERY_INFORMATION, PROCESS_VM_READ};

        let mut checked = 0;
        unsafe {
            let snapshot = Owned::new(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).unwrap());
            let mut entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            Process32FirstW(*snapshot, &mut entry).unwrap();
            loop {
                let pid = entry.th32ProcessID;
                let old_access_ok =
                    OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid)
                        .map(|h| Owned::new(h))
                        .is_ok();
                // A process can exit between the two calls -- only a pid
                // that's still around afterwards counts as a regression.
                if old_access_ok && get_exe_path(pid).is_none() {
                    let still_running = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
                        .map(|h| Owned::new(h))
                        .is_ok();
                    assert!(!still_running, "get_exe_path lost pid {pid}");
                }
                checked += old_access_ok as u32;
                if Process32NextW(*snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        assert!(
            checked > 0,
            "expected at least this test process to be openable"
        );
    }

    #[test]
    fn get_exe_path_resolves_this_process() {
        let path = get_exe_path(std::process::id()).expect("get_exe_path failed for self");
        let expected = std::env::current_exe().unwrap();
        assert_eq!(
            std::path::Path::new(&path).file_name(),
            expected.file_name()
        );
    }

    // Ignored: assumes a real desktop with actual installed apps and a
    // populated Start Menu, which GitHub's hosted windows-latest CI runner
    // may not have in a comparable shape (headless, minimal image) --
    // same reasoning as this codebase's other real-hardware/real-environment
    // #[ignore]d tests. Run with `--ignored`.
    #[test]
    #[ignore]
    fn enumerate_installed_apps_finds_at_least_one_and_has_no_duplicate_exe_paths() {
        let apps = enumerate_installed_apps();
        assert!(
            !apps.is_empty(),
            "expected at least one resolvable Start Menu shortcut on a real desktop"
        );
        let mut paths: Vec<String> = apps.iter().map(|a| a.exe_path.to_lowercase()).collect();
        paths.sort_unstable();
        let mut deduped = paths.clone();
        deduped.dedup();
        assert_eq!(paths, deduped, "expected no duplicate exe paths");
        for app in &apps {
            assert!(!app.display_name.is_empty());
            assert!(!app.exe_name.is_empty());
            assert!(
                app.exe_path.to_lowercase().ends_with(".exe"),
                "exe_path should end in .exe: {}",
                app.exe_path
            );
        }
    }

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
            assert_ne!(
                s.hwnd, 0usize,
                "HWND should not be null for '{}'",
                s.window_title
            );
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
        //
        // Explicitly a `Window` entry, not just `.first()` -- monitors sort
        // first in `enumerate_sources()`, and `capture_source_for_hwnd` treats
        // its argument as a real HWND, which an HMONITOR value isn't.
        let sources = enumerate_sources();
        let sample = sources
            .iter()
            .find(|s| s.kind == CaptureKind::Window)
            .expect("expected at least one visible window");
        let hwnd = windows::Win32::Foundation::HWND(sample.hwnd as *mut core::ffi::c_void);
        let rebuilt = capture_source_for_hwnd(hwnd);
        assert_eq!(rebuilt.hwnd, sample.hwnd);
        assert_eq!(
            rebuilt.process_id, sample.process_id,
            "process identity must be stable even if the title changed"
        );
    }

    #[test]
    fn enumerate_monitors_returns_at_least_one_display() {
        let monitors = enumerate_monitors();
        assert!(!monitors.is_empty(), "expected at least one display");
        for m in &monitors {
            assert_eq!(m.kind, CaptureKind::Monitor);
            assert_eq!(m.process_id, 0);
            assert!(m.exe_name.is_empty());
            assert!(m.icon_rgba.is_none());
            assert_ne!(m.hwnd, 0usize, "HMONITOR should not be null");
        }
    }

    #[test]
    fn enumerate_sources_lists_monitors_before_windows() {
        let sources = enumerate_sources();
        let first_window_idx = sources
            .iter()
            .position(|s| s.kind == CaptureKind::Window)
            .expect("expected at least one visible window");
        assert!(
            sources[..first_window_idx]
                .iter()
                .all(|s| s.kind == CaptureKind::Monitor),
            "every entry before the first window should be a monitor"
        );
    }

    #[test]
    fn extracted_icon_has_matching_buffer_length() {
        let sources = enumerate_sources();
        for s in &sources {
            if let Some((rgba, w, h)) = &s.icon_rgba {
                assert_eq!(
                    rgba.len(),
                    (*w * *h * 4) as usize,
                    "RGBA buffer length must be width*height*4"
                );
            }
        }
    }

    #[test]
    fn resolve_possibly_stale_exe_path_returns_existing_path_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let exe_path = dir.path().join("App.exe");
        std::fs::write(&exe_path, b"stub").unwrap();
        let exe_path_str = exe_path.to_string_lossy().into_owned();
        assert_eq!(resolve_possibly_stale_exe_path(&exe_path_str), exe_path_str);
    }

    #[test]
    fn resolve_possibly_stale_exe_path_finds_the_new_squirrel_version_folder() {
        // Simulates a Squirrel-updated app: the registered path
        // (.../App/app-1.0.0/App.exe) no longer exists on disk, having been
        // replaced by a newer sibling version folder -- the same shape
        // Discord/Slack/other Electron apps update themselves into.
        let root = tempfile::tempdir().unwrap();
        let old_dir = root.path().join("app-1.0.0");
        let new_dir = root.path().join("app-1.0.1");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::create_dir_all(&new_dir).unwrap();
        let new_exe = new_dir.join("App.exe");
        std::fs::write(&new_exe, b"stub").unwrap();

        let stale_path = old_dir.join("App.exe").to_string_lossy().into_owned();
        let resolved = resolve_possibly_stale_exe_path(&stale_path);
        assert_eq!(resolved, new_exe.to_string_lossy().into_owned());
    }

    #[test]
    fn resolve_possibly_stale_exe_path_falls_back_unchanged_when_unresolvable() {
        // No sibling app-* folder contains the exe at all -- genuinely gone,
        // not just moved -- so the best-effort fallback is the original
        // (still-nonexistent) path, not a panic or an unrelated guess.
        let root = tempfile::tempdir().unwrap();
        let old_dir = root.path().join("app-1.0.0");
        std::fs::create_dir_all(&old_dir).unwrap();
        let stale_path = old_dir.join("App.exe").to_string_lossy().into_owned();
        assert_eq!(resolve_possibly_stale_exe_path(&stale_path), stale_path);
    }

    #[test]
    fn strip_exe_suffix_is_case_insensitive() {
        assert_eq!(strip_exe_suffix("Discord.exe"), "Discord");
        assert_eq!(strip_exe_suffix("Notepad.EXE"), "Notepad");
        assert_eq!(strip_exe_suffix("Game.Exe"), "Game");
        assert_eq!(strip_exe_suffix(".exe"), "");
    }

    #[test]
    fn strip_exe_suffix_leaves_other_names_alone() {
        assert_eq!(strip_exe_suffix("Discord"), "Discord");
        assert_eq!(strip_exe_suffix("exe"), "exe");
        assert_eq!(strip_exe_suffix(""), "");
        assert_eq!(strip_exe_suffix("archive.zip"), "archive.zip");
    }

    #[test]
    fn strip_exe_suffix_never_splits_a_multibyte_character() {
        // 9 bytes: slicing at len - 4 would land inside "ー" and panic.
        assert_eq!(strip_exe_suffix("ゲーム"), "ゲーム");
        assert_eq!(strip_exe_suffix("ゲーム.exe"), "ゲーム");
    }

    #[test]
    fn exe_file_name_takes_the_last_path_component() {
        assert_eq!(
            exe_file_name(r"C:\Program Files\Discord\Discord.exe").as_deref(),
            Some("Discord.exe")
        );
        assert_eq!(exe_file_name("").as_deref(), None);
    }

    fn fake_icon(byte: u8) -> Icon {
        (vec![byte; 4], 1, 1)
    }

    #[test]
    fn icon_cache_serves_a_repeat_request_without_extracting_again() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("app.exe");
        std::fs::write(&exe, b"v1").unwrap();
        let exe = exe.to_str().unwrap();
        let cache = IconCache::default();
        let calls = std::cell::Cell::new(0);
        let extract = |_: &str| {
            calls.set(calls.get() + 1);
            Some(fake_icon(7))
        };
        assert_eq!(cache.get_or_extract(exe, extract), Some(fake_icon(7)));
        assert_eq!(cache.get_or_extract(exe, extract), Some(fake_icon(7)));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn icon_cache_re_extracts_once_the_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("app.exe");
        std::fs::write(&exe, b"v1").unwrap();
        let exe_str = exe.to_str().unwrap();
        let cache = IconCache::default();
        assert_eq!(
            cache.get_or_extract(exe_str, |_| Some(fake_icon(1))),
            Some(fake_icon(1))
        );
        // A different size changes the stamp even if the mtime resolution
        // doesn't move -- an updated exe.
        std::fs::write(&exe, b"version two").unwrap();
        assert_eq!(
            cache.get_or_extract(exe_str, |_| Some(fake_icon(2))),
            Some(fake_icon(2))
        );
    }

    #[test]
    fn icon_cache_never_caches_a_failed_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("app.exe");
        std::fs::write(&exe, b"v1").unwrap();
        let exe = exe.to_str().unwrap();
        let cache = IconCache::default();
        assert_eq!(cache.get_or_extract(exe, |_| None), None);
        // Retried on the next call, which can now succeed.
        assert_eq!(
            cache.get_or_extract(exe, |_| Some(fake_icon(3))),
            Some(fake_icon(3))
        );
    }

    #[test]
    fn icon_cache_passes_unreadable_paths_straight_through() {
        let cache = IconCache::default();
        let calls = std::cell::Cell::new(0);
        let extract = |_: &str| {
            calls.set(calls.get() + 1);
            Some(fake_icon(4))
        };
        let missing = r"C:\definitely\not\here\nothing.exe";
        assert_eq!(cache.get_or_extract(missing, extract), Some(fake_icon(4)));
        assert_eq!(cache.get_or_extract(missing, extract), Some(fake_icon(4)));
        assert_eq!(calls.get(), 2, "no stamp to key on, so nothing is cached");
    }

    #[test]
    fn real_shell_icon_through_the_cache_matches_a_fresh_extraction() {
        // SHGetFileInfoW expects COM on the calling thread -- every production
        // caller joins a single-threaded apartment first.
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            );
        }
        // Retried: racing other tests' concurrent shell calls in a fresh test
        // process, SHGetFileInfoW occasionally reports success with no icon
        // (about 1 run in 7). Best-effort by design (a missing icon just
        // shows no icon), and never cached, so a retry is what the app does
        // on its next refresh too.
        let path = r"C:\Windows\explorer.exe";
        let retry = |f: &dyn Fn() -> Option<Icon>| (0..10).find_map(|_| f());
        let fresh =
            retry(&|| extract_exe_icon_rgba_uncached(path)).expect("explorer.exe has an icon");
        let cached = retry(&|| extract_exe_icon_rgba(path)).expect("cached path");
        assert_eq!(cached, fresh);
    }
}
