//! Starting PolyRec when the user signs in to Windows, hidden in the
//! notification area (tray): Highlight and the hotkeys only "never miss the
//! moment" if PolyRec is running, and until now it ran only once opened.
//!
//! It's the per-user `Run` key that says so, not `config.toml`: Windows
//! reads the key, and the user can change it elsewhere (Task Manager's
//! Startup apps, the installer's uninstall). The entry names this copy's
//! exe, so a copy built from source or kept in another folder doesn't
//! count as on because another one is.

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::core::{HSTRING, PCWSTR};

/// The command-line flag the `Run` entry starts PolyRec with: no window,
/// only the tray icon (see `main`).
pub const IN_TRAY_ARG: &str = "--in-tray";

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "PolyRec";

/// What Windows runs at sign-in for the copy at `exe`.
fn command_line(exe: &std::path::Path) -> String {
    format!("\"{}\" {IN_TRAY_ARG}", exe.display())
}

/// Whether Windows starts this copy of PolyRec at sign-in.
pub fn is_on() -> bool {
    this_command().is_some_and(|ours| stored(RUN_KEY).as_deref() == Some(ours.as_str()))
}

/// Makes Windows start this copy of PolyRec at sign-in, or stops it
/// starting any.
pub fn set(on: bool) -> windows::core::Result<()> {
    match (on, this_command()) {
        (true, Some(ours)) => store(RUN_KEY, &ours),
        (true, None) => Err(windows::core::Error::from(ERROR_FILE_NOT_FOUND)),
        (false, _) => remove(RUN_KEY),
    }
}

fn this_command() -> Option<String> {
    std::env::current_exe().ok().map(|exe| command_line(&exe))
}

fn stored(key: &str) -> Option<String> {
    let (key, value) = (HSTRING::from(key), HSTRING::from(VALUE));
    let mut bytes = 0u32;
    let sized = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &key,
            &value,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut bytes),
        )
    };
    if sized != ERROR_SUCCESS {
        return None;
    }
    let mut text = vec![0u16; (bytes as usize).div_ceil(2)];
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &key,
            &value,
            RRF_RT_REG_SZ,
            None,
            Some(text.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    };
    if read != ERROR_SUCCESS {
        return None;
    }
    let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    Some(String::from_utf16_lossy(&text[..end]))
}

fn store(key: &str, command: &str) -> windows::core::Result<()> {
    let text: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            &HSTRING::from(VALUE),
            REG_SZ.0,
            Some(text.as_ptr().cast()),
            (text.len() * 2) as u32,
        )
    }
    .ok()
}

fn remove(key: &str) -> windows::core::Result<()> {
    let removed = unsafe {
        RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            PCWSTR(HSTRING::from(VALUE).as_ptr()),
        )
    };
    if removed == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    removed.ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Registry::RegDeleteTreeW;

    #[test]
    fn the_entry_starts_this_exe_in_the_tray_whatever_its_path() {
        assert_eq!(
            command_line(std::path::Path::new(
                r"C:\Program Files\PolyRec\polyrec.exe"
            )),
            r#""C:\Program Files\PolyRec\polyrec.exe" --in-tray"#
        );
    }

    /// Against a key of the test's own, never the real `Run` key.
    #[test]
    fn an_entry_is_stored_read_back_and_removed() {
        let key = format!(r"Software\PolyRec-test-{}", std::process::id());
        assert_eq!(stored(&key), None);
        // Removing what isn't there is fine: off is off.
        remove(&key).unwrap();

        let command = command_line(std::path::Path::new(r"D:\ポリレック\polyrec.exe"));
        store(&key, &command).unwrap();
        assert_eq!(stored(&key).as_deref(), Some(command.as_str()));

        remove(&key).unwrap();
        assert_eq!(stored(&key), None);
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key));
        }
    }
}
