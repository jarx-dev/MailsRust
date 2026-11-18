//! Email files (.eml): Poczta offers itself in Explorer's "Open with" for them (under HKCU, no
//! administrator rights), and a file given on the command line opens in a reading window. The
//! file goes through a short list next to the log, so a second start (which only wakes the
//! running copy, see `another_copy_shown`) hands its file over the same way the first one does.

use std::path::PathBuf;

/// The list of files waiting to be opened (next to sync.log: each data folder has its own).
fn list_path() -> PathBuf {
    match std::env::var("POCZTA_DATA_DIR") {
        Ok(d) => PathBuf::from(d).join("open-files.txt"),
        Err(_) => crate::model::cache_dir().parent().map(|p| p.join("open-files.txt")).unwrap_or_else(|| "open-files.txt".into()),
    }
}

/// The .eml files named on the command line (Explorer passes one per start).
pub fn from_args(args: &[String]) -> Vec<String> {
    args.iter().skip(1).filter(|a| a.to_lowercase().ends_with(".eml") && std::path::Path::new(a).is_file()).cloned().collect()
}

/// Puts files on the list, for the copy that runs (this one, or one already running).
pub fn hand_over(files: &[String]) {
    use std::io::Write;
    if files.is_empty() {
        return;
    }
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(list_path()) else { return };
    for p in files {
        let _ = writeln!(f, "{p}");
    }
}

/// The files waiting to be opened; the list is emptied.
pub fn take() -> Vec<String> {
    let path = list_path();
    // (moved away first: a file handed over meanwhile starts a new list instead of being lost)
    let taken = path.with_extension("taken");
    if std::fs::rename(&path, &taken).is_err() {
        return vec![];
    }
    let text = std::fs::read_to_string(&taken).unwrap_or_default();
    let _ = std::fs::remove_file(&taken);
    text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
}

/// Adds Poczta to "Open with" for .eml files (the person makes it the default there once;
/// Windows lets no program take that by itself). Written only when missing or pointing at
/// another EXE, so a start normally only reads one value.
#[cfg(windows)]
pub fn register() {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ};
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    // (an empty value name is the key's default value)
    fn set(key: &str, name: &str, value: &str) -> bool {
        let (k, n, v) = (wide(key), wide(name), wide(value));
        unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr(), REG_SZ, v.as_ptr().cast(), (v.len() * 2) as u32) == ERROR_SUCCESS }
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let command = format!("\"{}\" \"%1\"", exe.display());
    let open = r"Software\Classes\Poczta.eml\shell\open\command";
    let (k, n) = (wide(open), wide(""));
    let mut buf = [0u16; 1024];
    let mut len = std::mem::size_of_val(&buf) as u32;
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len) };
    let now = (r == ERROR_SUCCESS).then(|| String::from_utf16_lossy(&buf[..(len as usize / 2).min(buf.len())]).trim_end_matches('\0').to_string());
    if now.as_deref() == Some(command.as_str()) {
        return;
    }
    let ok = set(r"Software\Classes\Poczta.eml", "", &t!("core.open_file.type_name"))
        && set(r"Software\Classes\Poczta.eml\DefaultIcon", "", &format!("\"{}\",0", exe.display()))
        && set(r"Software\Classes\Poczta.eml\Application", "ApplicationName", "Poczta")
        && set(open, "", &command)
        && set(r"Software\Classes\.eml\OpenWithProgids", "Poczta.eml", "");
    unsafe {
        windows::Win32::UI::Shell::SHChangeNotify(windows::Win32::UI::Shell::SHCNE_ASSOCCHANGED, windows::Win32::UI::Shell::SHCNF_IDLIST, None, None);
    }
    crate::diag::log(if ok { "email files: Poczta added to \"Open with\" for .eml" } else { "email files: could not add Poczta to \"Open with\" for .eml" });
}

#[cfg(not(windows))]
pub fn register() {}
