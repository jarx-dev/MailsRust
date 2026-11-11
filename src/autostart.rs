//! Starting together with Windows: the app's entry under
//! HKCU\Software\Microsoft\Windows\CurrentVersion\Run (no administrator rights needed). It starts
//! minimized, so mail and its notes keep coming without a window popping up at sign-in. The
//! updater replaces the EXE in place, so the entry stays valid after an update.

use std::sync::atomic::{AtomicU8, Ordering};

const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const NAME: &str = "Poczta";

/// 0: not read yet, 1: off, 2: on (the menu asks every frame while it is open).
static STATE: AtomicU8 = AtomicU8::new(0);

/// The command Windows runs at sign-in: this EXE, minimized.
fn command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\" --minimized", exe.display()))
}

/// On when the entry exists and starts this EXE (an entry for a moved or other copy counts as off).
pub fn is_on() -> bool {
    match STATE.load(Ordering::Relaxed) {
        0 => {
            let on = imp::read().zip(command()).is_some_and(|(v, c)| v.eq_ignore_ascii_case(&c));
            STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
        s => s == 2,
    }
}

pub fn set(on: bool) -> Result<(), String> {
    let result = if on { command().ok_or_else(|| t!("core.autostart.unknown_path").into_owned()).and_then(|c| imp::write(&c)) } else { imp::delete() };
    STATE.store(0, Ordering::Relaxed);
    result
}

#[cfg(windows)]
mod imp {
    use super::{KEY, NAME};
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn read() -> Option<String> {
        let (key, name) = (wide(KEY), wide(NAME));
        let mut buf = [0u16; 1024];
        let mut len = std::mem::size_of_val(&buf) as u32;
        let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len) };
        if r != ERROR_SUCCESS {
            return None;
        }
        let chars = (len as usize / 2).min(buf.len());
        Some(String::from_utf16_lossy(&buf[..chars]).trim_end_matches('\0').to_string())
    }

    pub fn write(value: &str) -> Result<(), String> {
        let (key, name, data) = (wide(KEY), wide(NAME), wide(value));
        let r = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), REG_SZ, data.as_ptr().cast(), (data.len() * 2) as u32) };
        if r == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(t!("core.autostart.registry_write", code = r).into_owned())
        }
    }

    pub fn delete() -> Result<(), String> {
        let (key, name) = (wide(KEY), wide(NAME));
        match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) } {
            ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
            r => Err(t!("core.autostart.registry_delete", code = r).into_owned()),
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn read() -> Option<String> {
        None
    }
    pub fn write(_: &str) -> Result<(), String> {
        Err(t!("core.autostart.windows_only").into_owned())
    }
    pub fn delete() -> Result<(), String> {
        Ok(())
    }
}
