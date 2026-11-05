//! Diagnostics: a timed log of what the sync does, step by step.
//! Lines go to %LOCALAPPDATA%\Poczta\sync.log and are kept in memory for the
//! "Diagnostyka" window.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::Mutex;
use std::time::Instant;

static LINES: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

fn log_path() -> std::path::PathBuf {
    if let Ok(d) = std::env::var("POCZTA_DATA_DIR") {
        return std::path::PathBuf::from(d).join("sync.log");
    }
    crate::model::cache_dir().parent().map(|p| p.join("sync.log")).unwrap_or_else(|| "sync.log".into())
}

/// Adds one line to the log (with the time of day).
pub fn log(text: impl AsRef<str>) {
    let line = format!("{}  {}", chrono::Local::now().format("%H:%M:%S%.3f"), text.as_ref());
    // one writer at a time, so lines from parallel connections don't run into each other
    let Ok(mut l) = LINES.lock() else { return };
    l.push_back(line.clone());
    while l.len() > 2000 {
        l.pop_front();
    }
    let path = log_path();
    // keep the file small: start over when it passes 2 MB
    if std::fs::metadata(&path).map(|m| m.len() > 2_000_000).unwrap_or(false) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(format!("{line}\r\n").as_bytes());
    }
}

/// Logs how long something took since `start`, with extra details.
pub fn took(what: &str, start: Instant, details: impl AsRef<str>) {
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    let d = details.as_ref();
    if d.is_empty() {
        log(format!("{what}: {ms:.0} ms"));
    } else {
        log(format!("{what}: {ms:.0} ms — {d}"));
    }
}

pub fn lines() -> Vec<String> {
    LINES.lock().map(|l| l.iter().cloned().collect()).unwrap_or_default()
}

pub fn file() -> std::path::PathBuf {
    log_path()
}

pub fn kb(bytes: usize) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else {
        format!("{} KB", bytes / 1024)
    }
}

/// Memory this program uses now (private, in MB), for the speed log.
pub fn mem_mb() -> u64 {
    mem_both().0
}

/// (private, in RAM now): what the program holds, and the part of it in RAM (the working set,
/// as Task Manager shows it).
fn mem_both() -> (u64, u64) {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let mut c: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
        c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c as *mut _ as *mut _, c.cb) != 0 {
            return ((c.PrivateUsage / 1_048_576) as u64, (c.WorkingSetSize / 1_048_576) as u64);
        }
    }
    (0, 0)
}

/// A line in the log with the memory used now.
pub fn mem(what: &str) {
    let (private, ram) = mem_both();
    log(format!("memory {what}: {private} MB ({ram} MB in RAM)"));
}

/// Gives memory that was used for a while (e.g. while loading) and is free again back to
/// Windows, so the program doesn't keep holding it.
pub fn give_back_memory() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Memory::{GetProcessHeap, HeapCompact};
        HeapCompact(GetProcessHeap(), 0);
    }
}

/// The app went to the background: memory it isn't using is handed to Windows (for other
/// programs; what the app touches again comes back from Windows' list in a moment).
pub fn trim_memory() {
    give_back_memory();
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::{ProcessStatus::K32EmptyWorkingSet, Threading::GetCurrentProcess};
        K32EmptyWorkingSet(GetCurrentProcess());
    }
}

/// Puts the current thread at low priority (background work: the window, the email view and
/// the mail list stay quick while it runs).
pub fn background_thread() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_LOWEST};
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST);
    }
}
