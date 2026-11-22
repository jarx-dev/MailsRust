//! Crash reports: after a panic, one small anonymous report goes to the author's own error tracker
//! (Rustrak, Sentry-compatible, https://errors.jarx.pl). It holds exactly this and nothing more:
//! - the build number (when the app was built, in UTC, e.g. `poczta@26.1104.2135`),
//! - the Windows version (e.g. `10.0.26100`),
//! - the place in the source code where the app crashed (`src/app/list.rs:123`).
//!
//! Never sent: the panic message (it can hold an address, a subject or a piece of a message),
//! file names or paths of the person, the computer or user name, mail, accounts, settings, logs,
//! environment variables or any install or user id. A library's path keeps only the crate part,
//! so a path from the build machine never goes out either.
//!
//! Only release builds send, at most one report per run (the first panic, also one the app
//! recovers from), waiting at most 3 seconds, every error ignored. It is on by default and turned
//! off in ☰ → "Send crash reports" (`Prefs::crash_reports`); until the saved settings are read
//! (and in test runs) nothing is sent.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const URL: &str = "https://errors.jarx.pl/api/10/envelope/";
/// The project's public ingest key (it only lets one send reports, rate-limited by the server).
const KEY: &str = "99ad8ac454ab4997867302e7cc06ef98";

static ENABLED: AtomicBool = AtomicBool::new(false);
static SENT: AtomicBool = AtomicBool::new(false);

/// Turns reports on or off at once (from the saved settings and the ☰ menu).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Called from the panic hook in main.rs; waits for at most 3 seconds, never panics.
pub fn report(location: Option<&std::panic::Location>) {
    if cfg!(debug_assertions) || !ENABLED.load(Ordering::Relaxed) || SENT.swap(true, Ordering::Relaxed) {
        return;
    }
    let place = location.map(|l| format!("{}:{}", short_path(l.file()), l.line())).unwrap_or_else(|| "unknown".into());
    let version = build_version(crate::update::built_at());
    let body = envelope(&event_id(), &version, &windows_version(), &place);
    let (done, wait) = std::sync::mpsc::channel();
    // (on its own thread: a hung name lookup is not covered by ureq's timeouts)
    let sending = std::thread::Builder::new().spawn(move || {
        if let Ok(tls) = native_tls::TlsConnector::new() {
            let ua = format!("poczta/{version}");
            let agent = ureq::AgentBuilder::new()
                .tls_connector(std::sync::Arc::new(tls))
                .timeout_connect(Duration::from_secs(2))
                .timeout(Duration::from_secs(3))
                .redirects(0)
                .user_agent(&ua)
                .build();
            let _ = agent
                .post(URL)
                .set("Content-Type", "application/x-sentry-envelope")
                .set("X-Sentry-Auth", &format!("Sentry sentry_version=7, sentry_key={KEY}, sentry_client={ua}"))
                .send_string(&body);
        }
        let _ = done.send(());
    });
    if sending.is_ok() {
        let _ = wait.recv_timeout(Duration::from_secs(3));
    }
}

/// "26.1104.2135": the build time in UTC (the menu shows it in local time, which a report must not reveal).
fn build_version(built_at: u64) -> String {
    chrono::DateTime::from_timestamp(built_at as i64, 0).map(|t| t.format("%y.%m%d.%H%M").to_string()).unwrap_or_default()
}

/// The Sentry envelope: a header line, an item line and the event, each ending with a newline.
fn envelope(event_id: &str, version: &str, os_version: &str, place: &str) -> String {
    let event = serde_json::json!({
        "event_id": event_id,
        "level": "fatal",
        "platform": "native",
        "release": format!("poczta@{version}"),
        "environment": "production",
        "contexts": {"os": {"name": "Windows", "version": os_version}},
        "exception": {"values": [{"type": "panic", "value": place, "mechanism": {"type": "panic", "handled": false}}]},
        "fingerprint": [place],
    });
    format!("{}\n{}\n{event}\n", serde_json::json!({ "event_id": event_id }), serde_json::json!({"type": "event"}))
}

/// The app's own files stay as they are (`src/main.rs`). A library from cargo's registry keeps its
/// crate folder on (`ureq-2.12.1/src/pool.rs`), a git checkout its revision on, the standard
/// library the part after `library/` (`core/src/option.rs`); any other path only its file name.
fn short_path(file: &str) -> String {
    let file = file.replace('\\', "/");
    let absolute = file.starts_with('/') || file.as_bytes().get(1) == Some(&b':');
    if !absolute && !file.split('/').any(|p| p == "..") {
        return file;
    }
    // the folder after these is the index or the checkout, then comes the crate ("ureq-2.12.1") or
    // the git revision (hex); anything else could be a folder of the build machine's user
    let crate_dir = |d: &str| d.chars().all(|c| c.is_ascii_alphanumeric() || "_.+-".contains(c)) && d.as_bytes().windows(2).skip(1).any(|w| w[0] == b'-' && w[1].is_ascii_digit());
    let revision = |d: &str| d.len() >= 7 && d.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    for (marker, kept) in [("/registry/src/", &crate_dir as &dyn Fn(&str) -> bool), ("/git/checkouts/", &revision)] {
        if let Some(i) = file.rfind(marker) {
            if let Some((_, rest)) = file[i + marker.len()..].split_once('/') {
                if rest.split('/').next().is_some_and(kept) {
                    return rest.to_string();
                }
            }
        }
    }
    if let Some(i) = file.find("/rustc/") {
        if let Some(j) = file[i..].find("/library/") {
            return file[i + j + "/library/".len()..].to_string();
        }
    }
    file.rsplit('/').next().unwrap_or_default().to_string()
}

fn event_id() -> String {
    let mut b = [0u8; 16];
    if getrandom::fill(&mut b).is_err() {
        // (unlikely) the time and the process id are unique enough for one report
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        b = (now ^ ((std::process::id() as u128) << 96)).to_le_bytes();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// "10.0.26100" from the registry (the same numbers `winver` shows).
#[cfg(windows)]
fn windows_version() -> String {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let key = wide(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    let dword = |name: &str| {
        let (n, mut v, mut len) = (wide(name), 0u32, 4u32);
        let r = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), n.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), (&mut v as *mut u32).cast(), &mut len) };
        (r == ERROR_SUCCESS).then_some(v)
    };
    let mut buf = [0u16; 32];
    let mut len = std::mem::size_of_val(&buf) as u32;
    let n = wide("CurrentBuildNumber");
    let r = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), n.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len) };
    // only a number goes out; anything else in that value is left out
    let build = (r == ERROR_SUCCESS).then(|| String::from_utf16_lossy(&buf[..(len as usize / 2).min(buf.len())])).and_then(|s| s.trim_end_matches('\0').trim().parse::<u32>().ok());
    match (dword("CurrentMajorVersionNumber"), dword("CurrentMinorVersionNumber"), build) {
        (Some(major), Some(minor), Some(b)) => format!("{major}.{minor}.{b}"),
        (Some(major), Some(minor), None) => format!("{major}.{minor}"),
        (_, _, b) => b.map(|b| b.to_string()).unwrap_or_default(),
    }
}

#[cfg(not(windows))]
fn windows_version() -> String {
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_has_only_version_windows_and_place() {
        let place = format!("{}:42", short_path(r"C:\Users\jan.kowalski\.cargo\registry\src\index.crates.io-1949cf8c6b5b557f\ureq-2.12.1\src\pool.rs"));
        assert_eq!(place, "ureq-2.12.1/src/pool.rs:42");
        let id = event_id();
        assert_eq!(id.len(), 32);
        // 2025-11-04 21:35 UTC, whatever the time zone of the machine running the test
        let version = build_version(1762292100);
        assert_eq!(version, "25.1104.2135");
        let body = envelope(&id, &version, "10.0.26100", &place);
        assert!(!body.contains("jan.kowalski") && !body.contains("Users") && !body.contains("registry"));
        // the only '@' is the release's: no address can come in, the panic message is not an input
        assert_eq!(body.matches('@').count(), 1);
        let lines: Vec<&str> = body.split_terminator('\n').collect();
        assert!(body.ends_with('\n'));
        assert_eq!(lines.len(), 3);
        let head: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(head, serde_json::json!({ "event_id": id }));
        assert_eq!(serde_json::from_str::<serde_json::Value>(lines[1]).unwrap(), serde_json::json!({"type": "event"}));
        let event: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        let keys: Vec<&str> = event.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(keys.len(), 8, "{keys:?}");
        assert_eq!(event["release"], "poczta@25.1104.2135");
        assert_eq!(event["contexts"], serde_json::json!({"os": {"name": "Windows", "version": "10.0.26100"}}));
        assert_eq!(event["exception"]["values"][0]["value"], place);
        assert_eq!(event["fingerprint"], serde_json::json!([place]));
    }

    #[test]
    fn paths_never_keep_the_home_folder() {
        assert_eq!(short_path("src/app/list.rs"), "src/app/list.rs");
        assert_eq!(short_path(r"vendor\blitz-dom\src\node.rs"), "vendor/blitz-dom/src/node.rs");
        assert_eq!(short_path("/rustc/abc123/library/core/src/option.rs"), "core/src/option.rs");
        assert_eq!(short_path("/home/jan/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/ureq-2.12.1/src/pool.rs"), "ureq-2.12.1/src/pool.rs");
        assert_eq!(short_path("/home/jan/.cargo/git/checkouts/imap-1a2b3c/0123abc/src/client.rs"), "0123abc/src/client.rs");
        assert_eq!(short_path("/home/jan/.cargo/git/checkouts/imap-1a2b3c/jan/src/client.rs"), "client.rs");
        assert_eq!(short_path(r"\\?\UNC\buildhost\registry\src\agents\alice\work\main.rs"), "main.rs");
        assert_eq!(short_path("D:/registry/src/Users/jan/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/ureq-2.12.1/src/pool.rs"), "ureq-2.12.1/src/pool.rs");
        assert_eq!(short_path(r"C:\Users\someone\x.rs"), "x.rs");
        assert_eq!(short_path("/home/demo/src/gen.rs"), "gen.rs");
        assert_eq!(short_path("../../x/src/a.rs"), "a.rs");
    }
}
