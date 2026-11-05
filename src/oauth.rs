//! Signing in to Google with OAuth 2.0 (installed-app flow with PKCE).
//!
//! The browser opens Google's own sign-in page; Google sends the answer back to a tiny
//! one-time listener on this computer (127.0.0.1). The app keeps only the long-lived
//! "refresh token" and exchanges it for a short-lived access token whenever it talks to Gmail.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const SCOPES: &str = "https://mail.google.com/ https://www.googleapis.com/auth/calendar https://www.googleapis.com/auth/contacts openid email profile";

pub struct LoginResult {
    pub refresh_token: String,
    pub email: String,
    pub name: String,
    /// what the person allowed (mail, calendar, contacts…), separated by spaces
    pub scopes: String,
}

fn agent() -> Result<ureq::Agent, String> {
    let tls = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
    Ok(ureq::AgentBuilder::new()
        .tls_connector(Arc::new(tls))
        .timeout(Duration::from_secs(30))
        .build())
}

fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    if getrandom::fill(&mut buf).is_err() {
        // Extremely unlikely; fall back to time-based bytes so sign-in still works.
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (t >> ((i % 16) * 8)) as u8 ^ (i as u8).wrapping_mul(97);
        }
    }
    URL_SAFE_NO_PAD.encode(buf)
}

fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn dec(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 2;
                } else {
                    out.push(b'%');
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Reads the client file Google Cloud lets you download ("client_secret_….json"),
/// or a plain {"client_id": …, "client_secret": …}.
pub fn parse_client_json(text: &str) -> Option<(String, String)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let inner = v.get("installed").or_else(|| v.get("web")).unwrap_or(&v);
    let id = inner.get("client_id")?.as_str()?.trim().to_string();
    let secret = inner
        .get("client_secret")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if id.ends_with(".apps.googleusercontent.com") {
        Some((id, secret))
    } else {
        None
    }
}

/// A Google client set up once for this copy of the app: built in at compile time,
/// or a "google-client.json" file placed next to the program.
pub fn builtin_client() -> Option<(String, String)> {
    if let Some(id) = option_env!("POCZTA_GOOGLE_CLIENT_ID") {
        if !id.trim().is_empty() {
            return Some((id.trim().to_string(), option_env!("POCZTA_GOOGLE_CLIENT_SECRET").unwrap_or("").to_string()));
        }
    }
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    for name in ["google-client.json", "client_secret.json"] {
        if let Ok(text) = std::fs::read_to_string(dir.join(name)) {
            if let Some(c) = parse_client_json(&text) {
                return Some(c);
            }
        }
    }
    None
}

/// The newest "client_secret_….json" in the Downloads folder, if Google's file was just downloaded.
pub fn find_downloaded_client() -> Option<(String, String)> {
    let home = std::env::var("USERPROFILE").ok()?;
    let dir = std::path::Path::new(&home).join("Downloads");
    let mut best: Option<(std::time::SystemTime, String, String)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if !(name.starts_with("client_secret") && name.ends_with(".json")) {
            continue;
        }
        let modified = entry.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
        if let Ok(text) = std::fs::read_to_string(entry.path()) {
            if let Some((id, secret)) = parse_client_json(&text) {
                if best.as_ref().map(|b| modified > b.0).unwrap_or(true) {
                    best = Some((modified, id, secret));
                }
            }
        }
    }
    best.map(|b| (b.1, b.2))
}

pub fn open_url(url: &str) {
    let _ = open_browser(url);
}

fn open_browser(url: &str) -> Result<(), String> {
    // rundll32 hands the address to the default browser without going through a shell,
    // so the "&" characters in it are safe.
    std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Nie udało się otworzyć przeglądarki: {e}"))
}

fn error_text(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let msg = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| {
                    let a = v.get("error_description").and_then(|x| x.as_str()).map(String::from);
                    a.or_else(|| v.get("error").and_then(|x| x.as_str()).map(String::from))
                })
                .unwrap_or(body);
            format!("Google odpowiedział {code}: {msg}")
        }
        other => other.to_string(),
    }
}

/// The whole sign-in: browser, wait for Google's answer, swap the code for tokens.
pub fn login(client_id: &str, client_secret: &str) -> Result<LoginResult, String> {
    let client_id = client_id.trim();
    if client_id.is_empty() {
        return Err("Wpisz identyfikator klienta (Client ID) z Google Cloud.".into());
    }
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect = format!("http://127.0.0.1:{port}");
    let verifier = random_token(48);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_token(16);
    let url = format!(
        "{AUTH_URL}?client_id={}&redirect_uri={}&response_type=code&scope={}&code_challenge={}&code_challenge_method=S256&state={}&access_type=offline&prompt=consent&include_granted_scopes=true",
        enc(client_id),
        enc(&redirect),
        enc(SCOPES),
        challenge,
        state
    );
    open_browser(&url)?;

    // Wait up to five minutes for the browser to come back.
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(300);
    let code = loop {
        if Instant::now() > deadline {
            return Err("Upłynął czas na zalogowanie w przeglądarce.".into());
        }
        let (mut stream, _) = match listener.accept() {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(150));
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]).to_string();
        let path = req.lines().next().unwrap_or("").split_whitespace().nth(1).unwrap_or("").to_string();
        let query = path.split_once('?').map(|x| x.1).unwrap_or("");
        let mut got_code = None;
        let mut got_state = None;
        let mut got_error = None;
        for pair in query.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                match k {
                    "code" => got_code = Some(dec(v)),
                    "state" => got_state = Some(dec(v)),
                    "error" => got_error = Some(dec(v)),
                    _ => {}
                }
            }
        }
        if got_code.is_none() && got_error.is_none() {
            // e.g. the browser asking for /favicon.ico
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            continue;
        }
        let ok = got_error.is_none() && got_state.as_deref() == Some(state.as_str());
        let page = if ok {
            "<html><head><meta charset=utf-8><title>Poczta</title></head><body style=\"font-family:Segoe UI,sans-serif;text-align:center;padding-top:80px;color:#222\"><h2 style=\"color:#1E6FD9\">Zalogowano ✓</h2><p>Możesz zamknąć tę kartę i wrócić do aplikacji Poczta.</p></body></html>"
        } else {
            "<html><head><meta charset=utf-8><title>Poczta</title></head><body style=\"font-family:Segoe UI,sans-serif;text-align:center;padding-top:80px;color:#222\"><h2 style=\"color:#E03E3E\">Logowanie nie powiodło się</h2><p>Wróć do aplikacji Poczta i spróbuj ponownie.</p></body></html>"
        };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            page.len(),
            page
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
        if let Some(e) = got_error {
            return Err(if e == "access_denied" { "Odmówiono dostępu w Google.".into() } else { format!("Google: {e}") });
        }
        if got_state.as_deref() != Some(state.as_str()) {
            return Err("Odpowiedź z przeglądarki nie pasuje do tego logowania.".into());
        }
        break got_code.unwrap_or_default();
    };

    let mut form: Vec<(&str, &str)> = vec![
        ("code", code.as_str()),
        ("client_id", client_id),
        ("redirect_uri", redirect.as_str()),
        ("grant_type", "authorization_code"),
        ("code_verifier", verifier.as_str()),
    ];
    if !client_secret.trim().is_empty() {
        form.push(("client_secret", client_secret.trim()));
    }
    let v: serde_json::Value = agent()?
        .post(TOKEN_URL)
        .send_form(&form)
        .map_err(error_text)?
        .into_json()
        .map_err(|e| e.to_string())?;
    let refresh_token = v
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if refresh_token.is_empty() {
        return Err("Google nie zwrócił trwałego dostępu (refresh token). Spróbuj ponownie.".into());
    }
    // The id_token tells us which Google account was used.
    let (email, name) = v
        .get("id_token")
        .and_then(|x| x.as_str())
        .and_then(|jwt| jwt.split('.').nth(1).map(String::from))
        .and_then(|payload| URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .map(|claims| {
            (
                claims.get("email").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                claims.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            )
        })
        .unwrap_or_default();
    let scopes = v.get("scope").and_then(|x| x.as_str()).unwrap_or("").to_string();
    Ok(LoginResult { refresh_token, email, name, scopes })
}

/// Access tokens last an hour: each is kept (per refresh token) until a few minutes before it
/// runs out, instead of asking Google for a new one for every connection (0.3–0.5 s each).
static TOKENS: std::sync::Mutex<std::collections::BTreeMap<String, (String, std::time::Instant)>> = std::sync::Mutex::new(std::collections::BTreeMap::new());

/// A token was refused (e.g. revoked before its time): fresh ones are asked for next.
pub fn forget_tokens() {
    if let Ok(mut t) = TOKENS.lock() {
        t.clear();
    }
}

/// A short-lived access token from the saved refresh token (kept while it is valid).
pub fn access_token(client_id: &str, client_secret: &str, refresh_token: &str) -> Result<String, String> {
    if let Some((token, until)) = TOKENS.lock().ok().and_then(|t| t.get(refresh_token).cloned()) {
        if std::time::Instant::now() < until {
            return Ok(token);
        }
    }
    let mut form: Vec<(&str, &str)> = vec![
        ("client_id", client_id.trim()),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
    ];
    if !client_secret.trim().is_empty() {
        form.push(("client_secret", client_secret.trim()));
    }
    let v: serde_json::Value = agent()?
        .post(TOKEN_URL)
        .send_form(&form)
        .map_err(|e| {
            let t = error_text(e);
            if t.contains("invalid_grant") || t.contains("expired") || t.contains("revoked") {
                format!("{t} — zaloguj się ponownie przez Google w ustawieniach konta.")
            } else {
                t
            }
        })?
        .into_json()
        .map_err(|e| e.to_string())?;
    let token: String = v.get("access_token").and_then(|x| x.as_str()).map(String::from).ok_or("Google nie zwrócił tokenu dostępu.")?;
    let life = v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(3600);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(life.saturating_sub(300));
    if let Ok(mut t) = TOKENS.lock() {
        t.insert(refresh_token.to_string(), (token.clone(), until));
    }
    Ok(token)
}

/// The SASL XOAUTH2 string Gmail expects for IMAP.
pub struct XOAuth2 {
    pub user: String,
    pub token: String,
}

impl imap::Authenticator for XOAuth2 {
    type Response = String;
    fn process(&self, _challenge: &[u8]) -> Self::Response {
        format!("user={}\x01auth=Bearer {}\x01\x01", self.user, self.token)
    }
}
