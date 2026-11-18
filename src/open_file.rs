//! Email files (.eml): Poczta offers itself in Explorer's "Open with" for them (under HKCU, no
//! administrator rights), and a file given on the command line opens in a reading window. Each
//! start leaves a note in a folder next to the log, so a second start (which only wakes the
//! running copy, see `another_copy_shown`) hands its file over the same way the first one does.

use std::path::PathBuf;

/// The folder of notes waiting to be opened (next to sync.log: each data folder has its own).
fn queue_dir() -> PathBuf {
    match std::env::var("POCZTA_DATA_DIR") {
        Ok(d) => PathBuf::from(d).join("open-files"),
        Err(_) => crate::model::cache_dir().parent().map(|p| p.join("open-files")).unwrap_or_else(|| "open-files".into()),
    }
}

/// The .eml files named on the command line (Explorer passes one per start), as full paths: the
/// running copy that opens them has another working folder.
pub fn from_args(args: &[String]) -> Vec<String> {
    args.iter()
        .skip(1)
        .filter(|a| a.to_lowercase().ends_with(".eml"))
        .filter_map(|a| std::path::absolute(a).ok())
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

/// Leaves a note for the copy that runs (this one, or one already running). Written under a
/// temporary name and then renamed, so the reader never sees half of it.
pub fn hand_over(files: &[String]) {
    if files.is_empty() {
        return;
    }
    let dir = queue_dir();
    let _ = std::fs::create_dir_all(&dir);
    let name = format!("{}-{}", std::process::id(), crate::share::new_token());
    let tmp = dir.join(format!("{name}.tmp"));
    let done = std::fs::write(&tmp, files.join("\n")).and_then(|_| std::fs::rename(&tmp, dir.join(format!("{name}.txt"))));
    if let Err(e) = done {
        crate::diag::log(format!("email files: could not hand the file over ({e})"));
    }
}

/// The files waiting to be opened; their notes are removed. A note older than two minutes is
/// dropped unopened: its start found a copy that was closing, or crashed meanwhile.
pub fn take() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(queue_dir()) else { return vec![] };
    let mut files = vec![];
    for e in entries.flatten() {
        let p = e.path();
        // (only the notes `hand_over` writes; a clock set back counts as fresh)
        let Some(ext) = p.extension().and_then(|x| x.to_str()).filter(|x| *x == "txt" || *x == "tmp") else { continue };
        let fresh = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_none_or(|age| age.as_secs() < 120);
        let note = ext == "txt";
        if note && fresh {
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            files.extend(text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from));
        }
        // (a fresh .tmp is a note still being written)
        if note || !fresh {
            let _ = std::fs::remove_file(&p);
        }
    }
    files
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
    // (the command last: it marks the entries as done, so a failed write is tried again)
    let ok = set(r"Software\Classes\.eml\OpenWithProgids", "Poczta.eml", "")
        && set(r"Software\Classes\Poczta.eml", "", &t!("core.open_file.type_name"))
        && set(r"Software\Classes\Poczta.eml\DefaultIcon", "", &format!("\"{}\",0", exe.display()))
        && set(r"Software\Classes\Poczta.eml\Application", "ApplicationName", "Poczta")
        && set(open, "", &command);
    if ok {
        unsafe {
            windows::Win32::UI::Shell::SHChangeNotify(windows::Win32::UI::Shell::SHCNE_ASSOCCHANGED, windows::Win32::UI::Shell::SHCNF_IDLIST, None, None);
        }
    }
    crate::diag::log(if ok { "email files: Poczta added to \"Open with\" for .eml" } else { "email files: could not add Poczta to \"Open with\" for .eml" });
}

#[cfg(not(windows))]
pub fn register() {}
