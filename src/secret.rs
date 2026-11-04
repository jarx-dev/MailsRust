//! Keeping the mail password on disk safely: Windows encrypts it with the signed-in
//! user's own key (DPAPI), so only the same Windows account on this computer can read it.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    fn run(data: &[u8], protect: bool) -> Option<Vec<u8>> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: the input blob points at `data`, which outlives the call; Windows allocates the
        // output, which is copied and then released with LocalFree.
        unsafe {
            let ok = if protect {
                CryptProtectData(
                    &input,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            } else {
                CryptUnprotectData(
                    &input,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            };
            if ok == 0 || output.pbData.is_null() {
                return None;
            }
            let out = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
            LocalFree(output.pbData as _);
            Some(out)
        }
    }

    pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
        run(data, true)
    }

    pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
        run(data, false)
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn protect(_: &[u8]) -> Option<Vec<u8>> {
        None
    }
    pub fn unprotect(_: &[u8]) -> Option<Vec<u8>> {
        None
    }
}

/// Encrypted, printable form of a password ("" if it can't be protected).
pub fn protect(password: &str) -> String {
    if password.is_empty() {
        return String::new();
    }
    imp::protect(password.as_bytes())
        .map(|b| STANDARD.encode(b))
        .unwrap_or_default()
}

/// The password back from its encrypted form.
pub fn unprotect(saved: &str) -> Option<String> {
    if saved.is_empty() {
        return None;
    }
    let bytes = STANDARD.decode(saved).ok()?;
    String::from_utf8(imp::unprotect(&bytes)?).ok()
}
