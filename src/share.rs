//! "Copy for AI": single emails shared over Tailscale, so an AI agent elsewhere in the
//! person's private Tailscale network can read them and leave a reply draft.
//!
//! - A small web server listens only on this computer's Tailscale address (100.x.y.z), never
//!   on the local network or the internet.
//! - Only emails the person shared can be read, each under its own random address; nothing
//!   lists them.
//! - An agent can read the email as text (Markdown), JSON or its formatted (HTML) version,
//!   download its attachments, and send back a reply draft, which lands in Drafts. Nothing is
//!   ever sent by itself.

use crate::model::{Account, Folder, Message};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// The ports tried for the server (on the Tailscale address only); if all are taken by other
/// programs, Windows picks a free one. The one in use is remembered so links keep working.
pub const PORTS: std::ops::RangeInclusive<u16> = 8765..=8775;
/// How long a shared link keeps working.
pub const DAYS: i64 = 7;

/// What an agent can see of one shared email (a copy taken when it was shared).
#[derive(Clone)]
pub struct Shared {
    pub id: u64,
    subject: String,
    from: String,
    to: String,
    cc: String,
    date: String,
    labels: Vec<String>,
    body: String,
    html_path: String,
    /// (name, cached file)
    attachments: Vec<(String, String)>,
    /// pictures inside the email (screenshots etc.): (content id, cached file)
    images: Vec<(String, String)>,
    partial: bool,
    /// the message's number in the server's inbox, to fetch the original .eml
    uid: Option<u32>,
    /// for rebuilding the .eml when the original can't be fetched
    from_name: String,
    from_addr: String,
    when: chrono::NaiveDateTime,
}

impl Shared {
    pub fn of(m: &Message) -> Self {
        let from = if m.from_name.is_empty() || m.from_name == m.from_addr { m.from_addr.clone() } else { format!("{} <{}>", m.from_name, m.from_addr) };
        Shared {
            id: m.id,
            subject: m.subject.clone(),
            from,
            to: m.to.clone(),
            cc: m.cc.clone(),
            date: m.date.format("%Y-%m-%d %H:%M").to_string(),
            labels: m.labels.iter().map(|l| crate::theme::display_label(l)).collect(),
            body: m.body.clone(),
            html_path: m.html_path.clone(),
            attachments: m.attachments.iter().cloned().zip(m.attachment_files.iter().cloned().chain(std::iter::repeat(String::new()))).collect(),
            images: m.inline.clone(),
            partial: m.partial,
            uid: if m.folder == Folder::Inbox { m.uid } else { None },
            from_name: m.from_name.clone(),
            from_addr: m.from_addr.clone(),
            when: m.date,
        }
    }

    /// The email as plain text for pasting into an AI chat.
    pub fn plain(&self) -> String {
        let mut s = format!("Temat: {}\nOd: {}\n", self.subject.trim(), self.from);
        if !self.to.trim().is_empty() {
            s += &format!("Do: {}\n", self.to.trim());
        }
        if !self.cc.trim().is_empty() {
            s += &format!("DW: {}\n", self.cc.trim());
        }
        s += &format!("Data: {}\n", self.date);
        if !self.attachments.is_empty() {
            s += &format!("Załączniki: {}\n", self.attachments.iter().map(|a| a.0.as_str()).collect::<Vec<_>>().join(", "));
        }
        s += "\n";
        s += self.body.trim();
        s += "\n";
        s
    }

    /// The email as Markdown, with links to everything else an agent can do with it.
    pub fn markdown(&self, base: &str) -> String {
        let mut s = format!("# {}\n\n", if self.subject.trim().is_empty() { "(bez tematu)" } else { self.subject.trim() });
        s += &format!("- Od: {}\n", self.from);
        if !self.to.trim().is_empty() {
            s += &format!("- Do: {}\n", self.to.trim());
        }
        if !self.cc.trim().is_empty() {
            s += &format!("- DW: {}\n", self.cc.trim());
        }
        s += &format!("- Data: {}\n", self.date);
        if !self.labels.is_empty() {
            s += &format!("- Tagi: {}\n", self.labels.join(", "));
        }
        if !self.attachments.is_empty() {
            s += "- Załączniki:\n";
            for (i, (name, file)) in self.attachments.iter().enumerate() {
                if file.is_empty() {
                    s += &format!("  - {name} (niedostępny)\n");
                } else {
                    s += &format!("  - [{name}]({base}/a/{i})\n");
                }
            }
        }
        // pictures inside the email that aren't already listed as attachments
        let extra: Vec<(usize, &(String, String))> =
            self.images.iter().enumerate().filter(|(_, (_, f))| !self.attachments.iter().any(|(_, a)| a == f)).collect();
        if !extra.is_empty() {
            s += "- Obrazy w treści (np. zrzuty ekranu):\n";
            for (i, (cid, _)) in extra {
                s += &format!("  - [{}]({base}/i/{i})\n", image_name(cid, i));
            }
        }
        if !self.html_path.is_empty() {
            s += &format!("- Wersja sformatowana (HTML, z obrazami): {base}/html\n");
        }
        s += &format!("- Cała wiadomość jako plik .eml (z załącznikami i obrazami): {base}.eml\n");
        s += &format!("- Ta sama wiadomość jako JSON: {base}.json\n");
        s += "\n---\n\n";
        if self.partial && self.body.trim().is_empty() {
            s += "(Treść tej wiadomości nie została jeszcze pobrana — otwórz ją w aplikacji i pobierz link ponownie.)\n";
        } else {
            s += self.body.trim_end();
            s += "\n";
        }
        s += &format!(
            "\n---\n\nSzkic odpowiedzi: wyślij POST z treścią odpowiedzi (zwykły tekst, UTF-8) na {base}/draft — \
             trafi do Szkiców w aplikacji pocztowej jako odpowiedź na ten e-mail. Nic nie zostanie wysłane automatycznie.\n"
        );
        s
    }

    fn json(&self, base: &str) -> String {
        let atts: Vec<serde_json::Value> = self
            .attachments
            .iter()
            .enumerate()
            .map(|(i, (name, file))| serde_json::json!({ "name": name, "url": if file.is_empty() { None } else { Some(format!("{base}/a/{i}")) } }))
            .collect();
        let images: Vec<serde_json::Value> = self
            .images
            .iter()
            .enumerate()
            .map(|(i, (cid, _))| serde_json::json!({ "name": image_name(cid, i), "url": format!("{base}/i/{i}") }))
            .collect();
        serde_json::json!({
            "images": images,
            "subject": self.subject,
            "from": self.from,
            "to": self.to,
            "cc": self.cc,
            "date": self.date,
            "tags": self.labels,
            "body": self.body,
            "complete": !(self.partial && self.body.trim().is_empty()),
            "html_url": if self.html_path.is_empty() { None } else { Some(format!("{base}/html")) },
            "attachments": atts,
            "draft_url": format!("{base}/draft"),
        })
        .to_string()
    }
}

/// A reply draft an agent sent: (email id, text).
pub type Draft = (u64, String);

/// The sharing server and the emails it serves.
pub struct Hub {
    shares: Arc<Mutex<HashMap<String, Shared>>>,
    drafts_tx: Sender<Draft>,
    pub drafts: Receiver<Draft>,
    /// where the server listens, once started
    pub addr: Option<SocketAddr>,
    /// this computer's name in the Tailscale network (MagicDNS), used in the links
    pub name: Option<String>,
    ctx: Option<eframe::egui::Context>,
    /// the mail account, to fetch original messages (.eml) from the server
    account: Arc<Mutex<Option<Account>>>,
    /// originals already fetched: message id → .eml
    raw: Arc<Mutex<HashMap<u64, Arc<Vec<u8>>>>>,
}

impl Hub {
    pub fn new(ctx: Option<eframe::egui::Context>) -> Self {
        let (drafts_tx, drafts) = channel();
        Hub { shares: Default::default(), drafts_tx, drafts, addr: None, name: None, ctx, account: Default::default(), raw: Default::default() }
    }

    /// The account used to fetch original messages for .eml links (only if it can connect).
    pub fn set_account(&self, acc: &Account) {
        if let Ok(mut a) = self.account.lock() {
            *a = acc.can_connect().then(|| acc.clone());
        }
    }

    /// Starts the server on the given address (normally the Tailscale one), once: on the port
    /// used last time if possible, else the first free one.
    pub fn start(&mut self, ip: Ipv4Addr, last_port: u16) -> Result<SocketAddr, String> {
        if let Some(a) = self.addr {
            return Ok(a);
        }
        let mut tries: Vec<u16> = Vec::new();
        if last_port != 0 {
            tries.push(last_port);
        }
        tries.extend(PORTS);
        tries.push(0);
        let mut last_err = String::new();
        let listener = tries.into_iter().find_map(|p| match TcpListener::bind((ip, p)) {
            Ok(l) => Some(l),
            Err(e) => {
                last_err = e.to_string();
                None
            }
        });
        let listener = listener.ok_or_else(|| t!("cal.share.cannot_listen", ip = ip, err = last_err).into_owned())?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        let shares = self.shares.clone();
        let drafts = self.drafts_tx.clone();
        let ctx = self.ctx.clone();
        let (account, raw) = (self.account.clone(), self.raw.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let (shares, drafts, ctx) = (shares.clone(), drafts.clone(), ctx.clone());
                let (account, raw) = (account.clone(), raw.clone());
                std::thread::spawn(move || {
                    let _ = handle(conn, &shares, &drafts, ctx.as_ref(), &account, &raw);
                });
            }
        });
        crate::diag::log(format!("copy for AI: sharing server listening on {addr}"));
        self.addr = Some(addr);
        self.name = tailscale_name();
        crate::diag::log(format!("copy for AI: links use {}", self.name.as_deref().unwrap_or("the Tailscale address (no name found)")));
        Ok(addr)
    }

    /// Fetches the original .eml in the background right away, so the agent gets it at once.
    pub fn warm(&self, s: &Shared) {
        let (s, account, raw) = (s.clone(), self.account.clone(), self.raw.clone());
        std::thread::spawn(move || {
            let _ = eml_of(&s, &account, &raw);
        });
    }

    pub fn put(&self, token: &str, s: Shared) {
        if let Ok(mut m) = self.shares.lock() {
            m.insert(token.to_string(), s);
        }
    }

    /// Refreshes the shared copy of an email (e.g. after its full text arrived).
    pub fn update(&self, m: &Message) {
        if let Ok(mut map) = self.shares.lock() {
            for s in map.values_mut().filter(|s| s.id == m.id) {
                *s = Shared::of(m);
            }
        }
    }

    pub fn clear(&self) {
        if let Ok(mut m) = self.shares.lock() {
            m.clear();
        }
    }

    /// The link for one shared email: by this computer's Tailscale name (e.g.
    /// laptop.tailnet.ts.net), or by its Tailscale address when the name isn't known.
    pub fn url(&self, token: &str) -> Option<String> {
        let a = self.addr?;
        Some(match &self.name {
            Some(n) => format!("http://{n}:{}/m/{token}", a.port()),
            None => format!("http://{a}/m/{token}"),
        })
    }
}

/// A new random link name (128 bits, so it can't be guessed).
pub fn new_token() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// This computer's full name in the Tailscale network (MagicDNS), as Tailscale itself reports
/// it (e.g. "laptop.tail1234.ts.net"). None if Tailscale's command-line tool isn't found or
/// MagicDNS is off.
pub fn tailscale_name() -> Option<String> {
    let candidates = [
        std::path::PathBuf::from(std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into())).join("Tailscale").join("tailscale.exe"),
        std::path::PathBuf::from("tailscale"),
    ];
    for exe in candidates {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["status", "--json"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // no console window flashing up
        }
        let Ok(out) = cmd.output() else { continue };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else { continue };
        let magic = v["CurrentTailnet"]["MagicDNSEnabled"].as_bool().unwrap_or(true);
        let name = v["Self"]["DNSName"].as_str().unwrap_or("").trim_end_matches('.').to_string();
        if magic && name.contains('.') {
            return Some(name);
        }
        return None;
    }
    None
}

/// This computer's Tailscale address, if Tailscale is on: the address Windows would use to
/// reach Tailscale's own DNS server (100.100.100.100). No data is sent.
pub fn tailscale_ip() -> Option<Ipv4Addr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("100.100.100.100:53").ok()?;
    match sock.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(ip) if ip.octets()[0] == 100 && (64..128).contains(&ip.octets()[1]) => Some(ip),
        _ => None,
    }
}

fn respond(conn: &mut TcpStream, status: &str, kind: &str, extra: &str, body: &[u8]) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n{extra}\r\n",
        body.len()
    );
    conn.write_all(head.as_bytes())?;
    conn.write_all(body)?;
    conn.flush()
}

fn handle(
    mut conn: TcpStream,
    shares: &Mutex<HashMap<String, Shared>>,
    drafts: &Sender<Draft>,
    ctx: Option<&eframe::egui::Context>,
    account: &Mutex<Option<Account>>,
    raw: &Mutex<HashMap<u64, Arc<Vec<u8>>>>,
) -> std::io::Result<()> {
    conn.set_read_timeout(Some(std::time::Duration::from_secs(15)))?;
    // the request head (up to 16 KB), then a body of up to 1 MB for drafts
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = conn.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
        if buf.len() > 16 * 1024 {
            return respond(&mut conn, "431 Request Header Fields Too Large", "text/plain; charset=utf-8", "", b"");
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or("").split_whitespace();
    let method = first.next().unwrap_or("").to_string();
    let path = first.next().unwrap_or("/").split('?').next().unwrap_or("/").to_string();
    let mut length = 0usize;
    let mut host = String::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                length = v.trim().parse().unwrap_or(0);
            } else if k.eq_ignore_ascii_case("host") {
                host = v.trim().to_string();
            }
        }
    }
    let text = "text/plain; charset=utf-8";
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    if parts.first() != Some(&"m") || parts.len() < 2 {
        return respond(&mut conn, "404 Not Found", text, "", "Nie ma tu nic. Użyj linku skopiowanego z aplikacji pocztowej.\n".as_bytes());
    }
    let (token, format) = match parts[1].rsplit_once('.') {
        Some((t, f)) => (t.to_string(), f.to_string()),
        None => (parts[1].to_string(), String::new()),
    };
    let Some(shared) = shares.lock().ok().and_then(|m| m.get(&token).cloned()) else {
        return respond(&mut conn, "404 Not Found", text, "", "Ten link wygasł albo udostępnianie zostało wyłączone.\n".as_bytes());
    };
    let base = format!("http://{}/m/{token}", if host.is_empty() { conn.local_addr().map(|a| a.to_string()).unwrap_or_default() } else { host });
    match (method.as_str(), parts.get(2).copied(), parts.get(3).copied()) {
        ("GET", None, _) if format == "eml" => {
            let eml = eml_of(&shared, account, raw);
            // where to leave a reply draft, as an extra header line at the very top
            let mut out = format!("X-Reply-Draft-URL: {base}/draft\r\n").into_bytes();
            out.extend_from_slice(&eml);
            let name = format!("{}.eml", if shared.subject.trim().is_empty() { "wiadomosc" } else { shared.subject.trim() });
            let name: String = name.chars().map(|c| if "\\/:*?<>|".contains(c) { '_' } else { c }).collect();
            respond(&mut conn, "200 OK", "message/rfc822", &disposition("inline", &name), &out)
        }
        ("GET", None, _) if format == "json" => respond(&mut conn, "200 OK", "application/json; charset=utf-8", "", shared.json(&base).as_bytes()),
        ("GET", None, _) => respond(&mut conn, "200 OK", "text/markdown; charset=utf-8", "", shared.markdown(&base).as_bytes()),
        ("GET", Some("html"), _) => match std::fs::read_to_string(&shared.html_path) {
            Ok(mut html) if !shared.html_path.is_empty() => {
                // pictures inside the email point at this server, so they show for the agent too
                for (n, (cid, _)) in shared.images.iter().enumerate() {
                    for prefix in ["cid:", "CID:", "Cid:"] {
                        html = html.replace(&format!("{prefix}{cid}"), &format!("{base}/i/{n}"));
                    }
                }
                respond(&mut conn, "200 OK", "text/html; charset=utf-8", "", html.as_bytes())
            }
            _ => respond(&mut conn, "404 Not Found", text, "", "Ta wiadomość nie ma wersji sformatowanej.\n".as_bytes()),
        },
        ("GET", Some("i"), Some(i)) => {
            let found = i.parse::<usize>().ok().and_then(|n| shared.images.get(n).map(|x| (n, x)));
            match found.and_then(|(n, (cid, f))| std::fs::read(f).ok().map(|b| (image_name(cid, n), b))) {
                Some((name, bytes)) => respond(&mut conn, "200 OK", mime(&name), &disposition("inline", &name), &bytes),
                None => respond(&mut conn, "404 Not Found", text, "", "Nie ma takiego obrazu.\n".as_bytes()),
            }
        }
        ("GET", Some("a"), Some(i)) => {
            let found = i.parse::<usize>().ok().and_then(|i| shared.attachments.get(i)).filter(|(_, f)| !f.is_empty());
            match found.and_then(|(name, f)| std::fs::read(f).ok().map(|b| (name.clone(), b))) {
                Some((name, bytes)) => {
                    // pictures and PDFs open in place; everything else downloads
                    let how = if mime(&name).starts_with("image/") || mime(&name) == "application/pdf" { "inline" } else { "attachment" };
                    respond(&mut conn, "200 OK", mime(&name), &disposition(how, &name), &bytes)
                }
                None => respond(&mut conn, "404 Not Found", text, "", "Nie ma takiego załącznika.\n".as_bytes()),
            }
        }
        ("POST", Some("draft"), _) => {
            if length > 1024 * 1024 {
                return respond(&mut conn, "413 Payload Too Large", text, "", "Szkic jest za długi (najwyżej 1 MB).\n".as_bytes());
            }
            let mut body = buf[head_end..].to_vec();
            while body.len() < length {
                let n = conn.read(&mut chunk)?;
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..n]);
            }
            body.truncate(length);
            let draft = String::from_utf8_lossy(&body).trim().to_string();
            if draft.is_empty() {
                return respond(&mut conn, "400 Bad Request", text, "", "Pusty szkic — wyślij treść odpowiedzi jako zwykły tekst.\n".as_bytes());
            }
            let _ = drafts.send((shared.id, draft));
            if let Some(c) = ctx {
                c.request_repaint();
            }
            crate::diag::log("copy for AI: a reply draft arrived");
            respond(&mut conn, "201 Created", text, "", "Szkic odpowiedzi zapisany w aplikacji (folder Szkice). Nic nie zostało wysłane.\n".as_bytes())
        }
        _ => respond(&mut conn, "404 Not Found", text, "", "Nieznany adres. Dostępne: GET …, GET ….json, GET …/html, GET …/a/N, POST …/draft\n".as_bytes()),
    }
}

fn percent(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// A readable name for a picture inside the email ("image001.jpg@01DC…" → "image001.jpg").
fn image_name(cid: &str, n: usize) -> String {
    let base = cid.split('@').next().unwrap_or("").trim();
    if base.contains('.') && base.len() <= 80 {
        base.to_string()
    } else {
        format!("obraz-{}", n + 1)
    }
}

fn mime(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "txt" | "csv" | "log" => "text/plain; charset=utf-8",
        "html" | "htm" => "text/html; charset=utf-8",
        "json" => "application/json",
        "xml" => "application/xml",
        "zip" => "application/zip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "eml" => "message/rfc822",
        _ => "application/octet-stream",
    }
}

fn disposition(how: &str, name: &str) -> String {
    let safe: String = name.chars().map(|c| if c == '"' || c.is_control() { '_' } else { c }).collect();
    let ascii: String = safe.chars().map(|c| if c.is_ascii() { c } else { '_' }).collect();
    format!("Content-Disposition: {how}; filename=\"{ascii}\"; filename*=UTF-8''{}\r\n", percent(&safe))
}

/// The email as a .eml file: the original from the server when it's an inbox message (fetched
/// once, read-only, without marking it as read), otherwise rebuilt from what the app keeps.
fn eml_of(s: &Shared, account: &Mutex<Option<Account>>, raw: &Mutex<HashMap<u64, Arc<Vec<u8>>>>) -> Arc<Vec<u8>> {
    if let Some(r) = raw.lock().ok().and_then(|m| m.get(&s.id).cloned()) {
        return r;
    }
    let acc = account.lock().ok().and_then(|a| a.clone());
    if let (Some(uid), Some(acc)) = (s.uid, acc) {
        match crate::net::fetch_raw(&acc, uid) {
            Ok(bytes) => {
                let bytes = Arc::new(bytes);
                if let Ok(mut m) = raw.lock() {
                    m.insert(s.id, bytes.clone());
                }
                crate::diag::log("copy for AI: original .eml fetched from the server");
                return bytes;
            }
            Err(e) => crate::diag::log(format!("copy for AI: original .eml not available ({e}), rebuilding it")),
        }
    }
    Arc::new(build_eml(s))
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    let e = base64::engine::general_purpose::STANDARD.encode(bytes);
    let mut out = String::with_capacity(e.len() + e.len() / 76 * 2 + 2);
    for (i, ch) in e.as_bytes().chunks(76).enumerate() {
        if i > 0 {
            out.push_str("\r\n");
        }
        out.push_str(std::str::from_utf8(ch).unwrap_or(""));
    }
    out
}

/// A header value that may contain Polish letters (RFC 2047).
fn word(s: &str) -> String {
    if s.is_ascii() {
        s.to_string()
    } else {
        use base64::Engine;
        format!("=?UTF-8?B?{}?=", base64::engine::general_purpose::STANDARD.encode(s.as_bytes()))
    }
}

fn address(name: &str, addr: &str) -> String {
    let name = name.trim();
    if name.is_empty() || name == addr {
        addr.to_string()
    } else if name.is_ascii() {
        format!("\"{}\" <{addr}>", name.replace('"', "'"))
    } else {
        format!("{} <{addr}>", word(name))
    }
}

fn address_list(list: &str) -> String {
    list.split(',')
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            let (n, a) = crate::model::split_address(p);
            address(&n, &a)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Rebuilds a standard .eml (MIME) file: text and formatted versions, the pictures inside the
/// email and the attachments.
pub fn build_eml(s: &Shared) -> Vec<u8> {
    use chrono::TimeZone;
    let token = new_token();
    let (mixed, related, alt) = (format!("mixed-{token}"), format!("related-{token}"), format!("alt-{token}"));
    let date = chrono::Local
        .from_local_datetime(&s.when)
        .earliest()
        .map(|d| d.to_rfc2822())
        .unwrap_or_else(|| chrono::Local::now().to_rfc2822());
    let mut out = String::new();
    out += &format!("From: {}\r\n", address(&s.from_name, &s.from_addr));
    if !s.to.trim().is_empty() {
        out += &format!("To: {}\r\n", address_list(&s.to));
    }
    if !s.cc.trim().is_empty() {
        out += &format!("Cc: {}\r\n", address_list(&s.cc));
    }
    out += &format!("Subject: {}\r\nDate: {date}\r\nMIME-Version: 1.0\r\n", word(s.subject.trim()));
    out += &format!("Content-Type: multipart/mixed; boundary=\"{mixed}\"\r\n\r\n");
    // text, formatted version and the pictures it shows
    out += &format!("--{mixed}\r\nContent-Type: multipart/related; boundary=\"{related}\"\r\n\r\n");
    out += &format!("--{related}\r\nContent-Type: multipart/alternative; boundary=\"{alt}\"\r\n\r\n");
    out += &format!("--{alt}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n", b64(s.body.as_bytes()));
    if let Ok(html) = std::fs::read(&s.html_path) {
        if !s.html_path.is_empty() {
            out += &format!("--{alt}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n", b64(&html));
        }
    }
    out += &format!("--{alt}--\r\n");
    for (n, (cid, file)) in s.images.iter().enumerate() {
        let Ok(bytes) = std::fs::read(file) else { continue };
        let name = image_name(cid, n);
        out += &format!(
            "--{related}\r\nContent-Type: {}; name=\"{}\"\r\nContent-ID: <{cid}>\r\nContent-Disposition: inline; filename=\"{}\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n",
            mime(&name),
            word(&name),
            word(&name),
            b64(&bytes)
        );
    }
    out += &format!("--{related}--\r\n");
    for (name, file) in &s.attachments {
        let Ok(bytes) = std::fs::read(file) else { continue };
        out += &format!(
            "--{mixed}\r\nContent-Type: {}; name=\"{}\"\r\nContent-Disposition: attachment; filename=\"{}\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n",
            mime(name),
            word(name),
            word(name),
            b64(&bytes)
        );
    }
    out += &format!("--{mixed}--\r\n");
    out.into_bytes()
}
