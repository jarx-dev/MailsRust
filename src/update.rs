//! Self-update over Tailscale, as in HyperRust. The NUC publishes `latest.json` next to the
//! built EXE (`scripts/publish.sh`); a background thread polls it, downloads a newer build and
//! verifies it. The app swaps it in only while nothing is going on (see `MailApp::install_update`):
//! the running EXE is renamed to `*.exe.old` (Windows allows renaming a running program), the new
//! one takes its place and starts with `--after-update <pid>`, so it waits for this process to
//! save and exit (the mailbox opens in one process only).
//!
//! The previous build also starts itself from `*.exe.old` as a guard (`--update-guard`). If the
//! new build exits or doesn't load the mailbox in time, the guard puts the previous EXE back,
//! remembers the failed build's SHA-256 so it is not installed again, and starts the old one.
//!
//! Plain HTTP is accepted only for loopback and Tailscale addresses (WireGuard authenticates and
//! encrypts the peer); anything else must be HTTPS. Redirects are followed only from HTTPS and
//! only to HTTPS (GitHub's `releases/latest/download/latest.json` redirects twice).
//! `POCZTA_UPDATE_URL` points elsewhere; `POCZTA_UPDATE_URL=off` switches updates off.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};
use url::{Host, Url};

/// Where updates come from, set when the build is made (scripts/publish.sh, from a settings file
/// outside the repository); without it a build does not update itself.
const DEFAULT_URL: Option<&str> = option_env!("POCZTA_DEFAULT_UPDATE_URL");
/// This build, embedded by build.rs (the publish script sets it, so EXE and manifest agree).
const BUILD: &str = env!("POCZTA_BUILD");
/// Only builds made by scripts/publish.sh update themselves: a local build's identity can be
/// stale, and a test run must not swap the program under the test.
const PUBLISHED: bool = !env!("POCZTA_PUBLISHED").is_empty();
const MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;
const FIRST_CHECK: Duration = Duration::from_secs(20);
const EVERY: Duration = Duration::from_secs(60);
const AFTER_UPDATE_ARG: &str = "--after-update";
const GUARD_ARG: &str = "--update-guard";
/// How long the previous build may take to save and exit before it is stopped (a stop could
/// lose its last changes, so only a hung process waits that long).
const OLD_EXIT_TIMEOUT: Duration = Duration::from_secs(600);
/// How long a new build has to load the mailbox, counted from the previous one's exit.
const STARTUP_GRACE: Duration = Duration::from_secs(180);

static AFTER_UPDATE: AtomicBool = AtomicBool::new(false);

fn built_at() -> u64 {
    env!("POCZTA_BUILT_AT").parse().unwrap_or(0)
}

/// The short version for the menu, from the build time ("v26.1104.2135": year, month and day,
/// hour and minute), so after an update it shows a later number; and the build id with its commit.
pub fn version() -> (String, &'static str) {
    let short = chrono::DateTime::from_timestamp(built_at() as i64, 0).map(|t| t.with_timezone(&chrono::Local).format("v%y.%m%d.%H%M").to_string()).unwrap_or_default();
    // a build that doesn't update itself
    (if PUBLISHED { short } else { format!("{short} {}", t!("core.update.local_build")) }, BUILD)
}

/// `latest.json` written by the publish script.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    /// unique build id, e.g. `a1b2c3d-1760512345`
    pub build: String,
    /// Unix seconds; only strictly newer builds are installed (no downgrades)
    pub built_at: u64,
    /// hex SHA-256 of the EXE
    pub sha256: String,
    /// the EXE, relative to the manifest or absolute
    pub url: String,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub notes: String,
}

impl Manifest {
    fn is_newer_than(&self, build: &str, built_at: u64) -> bool {
        self.built_at > built_at && self.build != build
    }
}

fn parse_manifest(bytes: &[u8]) -> Result<Manifest, String> {
    let mut m: Manifest = serde_json::from_slice(bytes).map_err(|e| format!("manifest: {e}"))?;
    m.sha256 = m.sha256.trim().to_ascii_lowercase();
    if m.sha256.len() != 64 || !m.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("manifest: zła suma sha256".into());
    }
    if m.build.trim().is_empty() || m.url.trim().is_empty() {
        return Err("manifest: brak pola build lub url".into());
    }
    Ok(m)
}

/// `https://` anywhere, `http://` only for loopback and the tailnet.
fn validate_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw.trim()).map_err(|e| format!("adres aktualizacji „{raw}”: {e}"))?;
    let ok = match url.scheme() {
        "https" => url.host().is_some(),
        "http" => url.host().is_some_and(|h| match h {
            Host::Domain(d) => {
                let d = d.trim_end_matches('.').to_ascii_lowercase();
                d == "localhost" || d.ends_with(".ts.net")
            }
            Host::Ipv4(ip) => ip.is_loopback() || is_tailscale_v4(ip),
            Host::Ipv6(ip) => ip.is_loopback() || is_tailscale_v6(ip),
        }),
        _ => false,
    };
    if ok {
        Ok(url)
    } else {
        Err(format!("adres aktualizacji „{raw}” musi być https:// albo http:// w sieci Tailscale lub lokalny"))
    }
}

/// Tailscale's range 100.64.0.0/10.
fn is_tailscale_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    a == 100 && (b & 0xC0) == 64
}

/// Tailscale's prefix fd7a:115c:a1e0::/48.
fn is_tailscale_v6(ip: Ipv6Addr) -> bool {
    ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0]
}

fn download_url(manifest_url: &Url, m: &Manifest) -> Result<Url, String> {
    let joined = manifest_url.join(m.url.trim()).map_err(|e| format!("manifest: url: {e}"))?;
    validate_url(joined.as_str())
}

/// A downloaded EXE against its manifest: size, SHA-256 and the EXE header.
fn verify(bytes: &[u8], m: &Manifest) -> Result<(), String> {
    if let Some(size) = m.size {
        if bytes.len() as u64 != size {
            return Err(format!("rozmiar {} B zamiast {size} B", bytes.len()));
        }
    }
    let actual = sha256_hex(bytes);
    if actual != m.sha256 {
        return Err(format!("suma sha256 {actual} nie zgadza się z manifestem"));
    }
    if !bytes.starts_with(b"MZ") {
        return Err("to nie jest plik EXE".into());
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Via a temporary file, so a crash never leaves half a program under the final name.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("part");
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn agent(https: bool) -> Result<ureq::Agent, String> {
    let tls = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
    Ok(ureq::AgentBuilder::new()
        .tls_connector(std::sync::Arc::new(tls))
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(180))
        // from the tailnet a redirect could lead to plain HTTP on the internet; from HTTPS
        // every hop must stay HTTPS (ureq checks https_only on each one)
        .https_only(https)
        .redirects(if https { 5 } else { 0 })
        .user_agent(concat!("Poczta/", env!("CARGO_PKG_VERSION")))
        .build())
}

/// GET with a size limit; anything but 200 (also a refused redirect) is an error.
fn get(agent: &ureq::Agent, url: &Url, limit: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let resp = agent.get(url.as_str()).set("Cache-Control", "no-cache").call().map_err(|e| e.to_string())?;
    if resp.status() != 200 {
        return Err(format!("serwer odpowiedział {}", resp.status()));
    }
    let mut bytes = Vec::new();
    resp.into_reader().take(limit + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("plik przekracza limit rozmiaru".into());
    }
    Ok(bytes)
}

fn staged_path(exe: &Path) -> PathBuf {
    exe.with_extension("update.exe")
}

fn old_path(exe: &Path) -> PathBuf {
    exe.with_extension("exe.old")
}

/// Written by a freshly installed build once the mailbox is loaded; the guard waits for it.
fn ready_marker(exe: &Path) -> PathBuf {
    exe.with_extension("update-ok")
}

/// SHA-256 of a build that failed to start; it is never installed again.
fn skip_file(exe: &Path) -> PathBuf {
    exe.with_extension("update-skip")
}

fn failed_path(exe: &Path) -> PathBuf {
    exe.with_extension("failed.exe")
}

/// Starts the background checks; a verified newer EXE, ready to swap in, comes out of the
/// receiver (once). None when updates are off.
pub fn start() -> Option<Receiver<PathBuf>> {
    // (no address at all: a build not made for updating)
    let raw = std::env::var("POCZTA_UPDATE_URL").ok().or(DEFAULT_URL.map(String::from))?;
    // a separate test data folder means a test run; the swap and rollback need a *.exe name
    let exe_named = std::env::current_exe().is_ok_and(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")));
    if raw.trim().eq_ignore_ascii_case("off") || !cfg!(windows) || !PUBLISHED || !exe_named || std::env::var("POCZTA_DATA_DIR").is_ok() {
        return None;
    }
    let url = match validate_url(&raw) {
        Ok(u) => u,
        Err(e) => {
            crate::diag::log(format!("update: {e}"));
            return None;
        }
    };
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        crate::diag::background_thread();
        std::thread::sleep(FIRST_CHECK);
        let mut said = String::new();
        loop {
            match check_and_download(&url) {
                Ok(Some(staged)) => {
                    let _ = tx.send(staged);
                    return;
                }
                Ok(None) => {}
                // the same problem is noted once, not every minute
                Err(e) if e != said => {
                    crate::diag::log(format!("update: {e}"));
                    said = e;
                }
                Err(_) => {}
            }
            std::thread::sleep(EVERY);
        }
    });
    Some(rx)
}

/// The path of a verified, staged EXE when a newer build exists.
fn check_and_download(url: &Url) -> Result<Option<PathBuf>, String> {
    let agent = agent(url.scheme() == "https")?;
    let m = parse_manifest(&get(&agent, url, 64 * 1024)?)?;
    if !m.is_newer_than(BUILD, built_at()) {
        return Ok(None);
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if std::fs::read_to_string(skip_file(&exe)).unwrap_or_default().trim() == m.sha256 {
        return Err(format!("build {} nie uruchomił się i został wycofany — opublikuj nowszy", m.build));
    }
    let staged = staged_path(&exe);
    if !std::fs::read(&staged).is_ok_and(|b| verify(&b, &m).is_ok()) {
        let bytes = get(&agent, &download_url(url, &m)?, MAX_DOWNLOAD_BYTES)?;
        verify(&bytes, &m)?;
        write_atomically(&staged, &bytes).map_err(|e| format!("zapis {}: {e}", staged.display()))?;
    }
    crate::diag::log(format!("update: {} ready ({})", m.build, m.notes));
    Ok(Some(staged))
}

/// Swaps the EXE and starts the new build and its guard; on error everything is put back.
/// `start`: how the new build starts (`--tray`, `--minimized`), as this one is.
pub fn apply(staged: &Path, start: Option<&str>) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let old = old_path(&exe);
    let _ = std::fs::remove_file(&old);
    let _ = std::fs::remove_file(ready_marker(&exe));
    std::fs::rename(&exe, &old).map_err(|e| format!("zmiana nazwy bieżącego programu: {e}"))?;
    if let Err(e) = std::fs::rename(staged, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(format!("podmiana programu: {e}"));
    }
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg(AFTER_UPDATE_ARG).arg(std::process::id().to_string());
    if let Some(a) = start {
        cmd.arg(a);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::rename(&exe, staged);
            let _ = std::fs::rename(&old, &exe);
            return Err(format!("uruchomienie nowej wersji: {e}"));
        }
    };
    // without a guard a broken build couldn't be rolled back, so the install is abandoned
    // (the new process is still waiting for this one to exit)
    if let Err(e) = std::process::Command::new(&old).arg(GUARD_ARG).arg(child.id().to_string()).arg(std::process::id().to_string()).spawn() {
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::rename(&exe, staged);
        let _ = std::fs::rename(&old, &exe);
        return Err(format!("uruchomienie strażnika aktualizacji: {e}"));
    }
    Ok(())
}

/// `--update-guard <new pid> <previous pid>`, run from the previous build's `*.exe.old`.
/// True when this process was the guard and must end.
pub fn run_guard(args: &[String]) -> bool {
    let Some(pos) = args.iter().position(|a| a == GUARD_ARG) else { return false };
    let pid = |i: usize| args.get(pos + i).and_then(|p| p.parse::<u32>().ok());
    let (Some(new_pid), Some(previous_pid)) = (pid(1), pid(2)) else { return true };
    let Ok(old) = std::env::current_exe() else { return true };
    let exe = old.with_extension(""); // Poczta.exe.old -> Poczta.exe
    let marker = ready_marker(&exe);
    // the time to start counts once the previous build has saved and gone
    platform::wait_for_exit(previous_pid, OLD_EXIT_TIMEOUT);
    match platform::watch_startup(new_pid, || marker.exists(), STARTUP_GRACE) {
        platform::Startup::Ready => {
            let _ = std::fs::remove_file(&marker);
        }
        outcome => {
            crate::diag::log(format!("update: the new build didn't start ({outcome:?}), putting the previous one back"));
            // the restored build must not meet the previous process still holding the mailbox
            platform::wait_for_exit(previous_pid, OLD_EXIT_TIMEOUT);
            if let Err(e) = roll_back(&exe, &old) {
                crate::diag::log(format!("update: rollback: {e}"));
            }
        }
    }
    true
}

fn roll_back(exe: &Path, old: &Path) -> Result<(), String> {
    if let Ok(bytes) = std::fs::read(exe) {
        let _ = std::fs::write(skip_file(exe), sha256_hex(&bytes));
    }
    let failed = failed_path(exe);
    let _ = std::fs::remove_file(&failed);
    std::fs::rename(exe, &failed).map_err(|e| format!("odsunięcie nieudanej wersji: {e}"))?;
    std::fs::rename(old, exe).map_err(|e| format!("przywrócenie poprzedniej wersji: {e}"))?;
    std::process::Command::new(exe).spawn().map_err(|e| format!("uruchomienie poprzedniej wersji: {e}"))?;
    Ok(())
}

/// First thing in `main` of a freshly installed build: wait until the previous process (which
/// still holds the mailbox) has saved and exited.
pub fn finish_pending_update(args: &[String]) {
    let Some(pos) = args.iter().position(|a| a == AFTER_UPDATE_ARG) else { return };
    AFTER_UPDATE.store(true, Ordering::Relaxed);
    if let Some(pid) = args.get(pos + 1).and_then(|p| p.parse::<u32>().ok()) {
        platform::wait_for_exit(pid, OLD_EXIT_TIMEOUT);
    }
    crate::diag::log(format!("update: started as {BUILD}"));
}

/// The mailbox is loaded (`loaded`: read from the database, not replaced by the sample): tell
/// the guard, forget a previously failed build and remove the leftovers of the swap once the
/// guard has let them go.
pub fn startup_complete(loaded: bool) {
    let Ok(exe) = std::env::current_exe() else { return };
    if AFTER_UPDATE.load(Ordering::Relaxed) && !loaded {
        // it would show (and save) the sample mailbox: end before anything is written, the
        // guard sees it and puts the previous build back
        crate::diag::log("update: this build can't read the mailbox; ending for the rollback");
        std::process::exit(1);
    }
    if AFTER_UPDATE.load(Ordering::Relaxed) {
        if let Err(e) = std::fs::write(ready_marker(&exe), BUILD) {
            crate::diag::log(format!("update: ready marker: {e}"));
        }
        let _ = std::fs::remove_file(skip_file(&exe));
    }
    let leftovers: Vec<PathBuf> = [old_path(&exe), failed_path(&exe)].into_iter().filter(|p| p.exists()).collect();
    if leftovers.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if leftovers.iter().all(|p| std::fs::remove_file(p).is_ok() || !p.exists()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

#[cfg(windows)]
mod platform {
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        PROCESS_TERMINATE,
    };

    #[derive(Debug)]
    pub enum Startup {
        Ready,
        Exited,
        TimedOut,
    }

    /// Only our own builds are ever stopped (a reused process number must not be): the
    /// program must be exactly this EXE or its `*.exe.old`.
    fn is_ours(handle: HANDLE) -> bool {
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        if unsafe { QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) } == 0 {
            return false;
        }
        let image = String::from_utf16_lossy(&buf[..len as usize]).to_lowercase();
        let Ok(current) = std::env::current_exe() else { return false };
        let current = current.to_string_lossy().to_lowercase();
        let exe = current.strip_suffix(".old").unwrap_or(&current);
        image == exe || image == format!("{exe}.old")
    }

    fn open(pid: u32) -> Option<HANDLE> {
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        (!handle.is_null()).then_some(handle)
    }

    pub fn wait_for_exit(pid: u32, timeout: Duration) {
        let Some(handle) = open(pid) else { return }; // already gone
        unsafe {
            if WaitForSingleObject(handle, timeout.as_millis() as u32) == WAIT_TIMEOUT && is_ours(handle) {
                // it would keep the mailbox and leave the new build read-only
                TerminateProcess(handle, 1);
                WaitForSingleObject(handle, 5_000);
            }
            CloseHandle(handle);
        }
    }

    pub fn watch_startup(pid: u32, ready: impl Fn() -> bool, grace: Duration) -> Startup {
        let Some(handle) = open(pid) else {
            return if ready() { Startup::Ready } else { Startup::Exited };
        };
        let deadline = Instant::now() + grace;
        let outcome = loop {
            if ready() {
                break Startup::Ready;
            }
            if unsafe { WaitForSingleObject(handle, 200) } == WAIT_OBJECT_0 {
                break if ready() { Startup::Ready } else { Startup::Exited };
            }
            if Instant::now() >= deadline {
                unsafe {
                    if is_ours(handle) {
                        TerminateProcess(handle, 1);
                        WaitForSingleObject(handle, 5_000);
                    }
                }
                break Startup::TimedOut;
            }
        };
        unsafe { CloseHandle(handle) };
        outcome
    }
}

#[cfg(not(windows))]
mod platform {
    use std::time::Duration;

    #[derive(Debug)]
    pub enum Startup {
        Ready,
        #[allow(dead_code)]
        Exited,
    }

    pub fn wait_for_exit(_pid: u32, _timeout: Duration) {}

    pub fn watch_startup(_pid: u32, _ready: impl Fn() -> bool, _grace: Duration) -> Startup {
        Startup::Ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(bytes: &[u8]) -> Manifest {
        Manifest {
            build: "abc1234-1760000100".into(),
            built_at: 1_760_000_100,
            sha256: sha256_hex(bytes),
            url: "Poczta-abc1234-1760000100.exe".into(),
            size: Some(bytes.len() as u64),
            notes: String::new(),
        }
    }

    #[test]
    fn parses_publish_script_output() {
        let json = br#"{"version":"0.1.0","build":"a1b2c3d-1760512345","built_at":1760512345,
            "sha256":"E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855",
            "url":"Poczta-a1b2c3d-1760512345.exe","size":0,"notes":"feat: x"}"#;
        let m = parse_manifest(json).unwrap();
        assert_eq!(m.sha256, sha256_hex(b""));
        assert_eq!(m.notes, "feat: x");
        assert!(parse_manifest(b"{}").is_err());
        assert!(parse_manifest(br#"{"build":"b","built_at":1,"sha256":"zz","url":"x"}"#).is_err());
    }

    #[test]
    fn only_strictly_newer_builds_install() {
        let (build, at) = ("old-1760000000", 1_760_000_000);
        let mut m = manifest(b"MZ");
        assert!(m.is_newer_than(build, at));
        m.built_at = at;
        assert!(!m.is_newer_than(build, at), "same time");
        m.built_at = at - 1;
        assert!(!m.is_newer_than(build, at), "downgrade");
        m.built_at = at + 1;
        m.build = build.into();
        assert!(!m.is_newer_than(build, at), "same build");
    }

    #[test]
    fn http_only_inside_the_tailnet() {
        for ok in ["http://100.100.1.2:8790/mailrust/latest.json", "http://100.127.255.1/latest.json", "http://nuc.example.ts.net:8790/x.json", "http://127.0.0.1/x", "http://[fd7a:115c:a1e0::1]/x", "https://example.com/x"] {
            assert!(validate_url(ok).is_ok(), "{ok}");
        }
        for bad in ["http://example.com/x", "http://100.128.0.1/x", "http://100.63.255.255/x", "http://192.168.1.10/x", "http://evil.ts.net.example.com/x", "ftp://100.100.1.2/x", "file:///C:/x", "nonsense"] {
            assert!(validate_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn exe_url_is_resolved_and_validated() {
        let base = validate_url("http://100.100.1.2:8790/mailrust/latest.json").unwrap();
        let mut m = manifest(b"MZ");
        assert_eq!(download_url(&base, &m).unwrap().as_str(), "http://100.100.1.2:8790/mailrust/Poczta-abc1234-1760000100.exe");
        m.url = "http://example.com/x.exe".into();
        assert!(download_url(&base, &m).is_err());
    }

    #[test]
    fn verification_checks_hash_size_and_header() {
        let exe = b"MZ\x90\x00rest";
        let m = manifest(exe);
        assert!(verify(exe, &m).is_ok());
        assert!(verify(b"MZ\x90\x00resT", &m).is_err(), "hash");
        assert!(verify(b"MZ", &m).is_err(), "size");
        assert!(verify(b"<html>", &manifest(b"<html>")).is_err(), "header");
    }

    #[test]
    fn version_is_short_and_from_the_build_date() {
        let (v, build) = version();
        assert_eq!(build, BUILD);
        let digits: String = v.chars().filter(|c| c.is_ascii_digit()).collect();
        assert!(v.starts_with('v') && v.len() >= 13 && digits.len() == 10, "{v}");
    }

    #[test]
    fn redirects_are_refused_from_http() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://example.com/latest.json\r\nContent-Length: 0\r\n\r\n");
        });
        let url = validate_url(&format!("http://{addr}/latest.json")).unwrap();
        let err = get(&agent(false).unwrap(), &url, 1024).unwrap_err();
        assert!(err.contains("302"), "{err}");
    }

    #[test]
    fn https_agent_never_goes_plain_http() {
        // the same check stops a redirect hop from HTTPS to HTTP
        let err = get(&agent(true).unwrap(), &Url::parse("http://127.0.0.1:9/latest.json").unwrap(), 1024).unwrap_err();
        assert!(err.contains("https_only"), "{err}");
    }
}
