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
            uid: if m.folder == Folder::Inbox && m.account == 0 { m.uid } else { None },
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
            "\n---\n\nSzkic odpowiedzi: wyślij POST z treścią odpowiedzi (Markdown, UTF-8: pogrubienia, nagłówki, listy, tabele; \
             bez podpisu i cytatu, które aplikacja dodaje sama) na {base}/draft — \
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

/// A draft an agent sent; the app puts it in Drafts.
pub struct Draft {
    /// the email answered (None: a new email)
    pub reply_to: Option<u64>,
    /// a new email: recipients, copies and subject (a reply: Poczta sets them itself)
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub body: String,
    /// a reply to everyone on the email, not only the sender
    pub reply_all: bool,
    /// a new email: the address of the account it goes from ("" for the main one)
    pub from: String,
    /// an AI draft saved before, rewritten in place (keeps its number) instead of a new one
    pub replace: Option<u64>,
    /// where the app says what it saved, or why it couldn't
    pub done: Option<Sender<Result<String, String>>>,
}

impl Draft {
    pub fn reply(id: u64, body: String) -> Self {
        Draft { reply_to: Some(id), to: String::new(), cc: String::new(), subject: String::new(), body, reply_all: false, from: String::new(), replace: None, done: None }
    }
}

/// What every connection of the server reaches.
#[derive(Clone)]
pub struct Env {
    pub(crate) shares: Arc<Mutex<HashMap<String, Shared>>>,
    pub(crate) drafts: Sender<Draft>,
    pub(crate) asks: Sender<crate::mcp::Ask>,
    pub(crate) ctx: Option<eframe::egui::Context>,
    /// the mail account, to fetch original messages (.eml) from the server
    account: Arc<Mutex<Option<Account>>>,
    /// originals already fetched: message id → .eml
    raw: Arc<Mutex<HashMap<u64, Arc<Vec<u8>>>>>,
    /// the key agents use for the mailbox (MCP); None while that is switched off
    key: Arc<Mutex<Option<String>>>,
    /// counts key changes: a request let in under an older key leaves no new link behind
    pub(crate) key_gen: Arc<std::sync::atomic::AtomicU64>,
}

/// The sharing server and the emails it serves.
pub struct Hub {
    env: Env,
    pub drafts: Receiver<Draft>,
    /// what agents ask the app for (searches, messages)
    pub asks: Receiver<crate::mcp::Ask>,
    /// where the server listens, once started
    pub addr: Option<SocketAddr>,
    /// this computer's name in the Tailscale network (MagicDNS), used in the links; looked up
    /// in the background, as Tailscale's tool takes a moment
    name: Arc<Mutex<Option<String>>>,
}

impl Hub {
    pub fn new(ctx: Option<eframe::egui::Context>) -> Self {
        let (drafts_tx, drafts) = channel();
        let (asks_tx, asks) = channel();
        let env = Env { shares: Default::default(), drafts: drafts_tx, asks: asks_tx, ctx, account: Default::default(), raw: Default::default(), key: Default::default(), key_gen: Default::default() };
        Hub { env, drafts, asks, addr: None, name: Default::default() }
    }

    /// The account used to fetch original messages for .eml links (only if it can connect).
    pub fn set_account(&self, acc: &Account) {
        if let Ok(mut a) = self.env.account.lock() {
            *a = acc.can_connect().then(|| acc.clone());
        }
    }

    /// Lets agents in (with this key) or keeps them out (None). A new key, or none, also ends the
    /// links agents were given for attachments.
    pub fn set_key(&self, key: Option<String>) {
        let key = key.filter(|k| !k.is_empty());
        if let Ok(mut k) = self.env.key.lock() {
            if *k != key {
                self.env.key_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if let Ok(mut s) = self.env.shares.lock() {
                    s.retain(|t, _| !t.starts_with(AGENT_LINK));
                }
            }
            *k = key;
        }
    }

    /// The key agents use, while their access is on.
    pub fn key(&self) -> Option<String> {
        self.env.key.lock().ok().and_then(|k| k.clone())
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
        let env = self.env.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let env = env.clone();
                std::thread::spawn(move || {
                    let _ = handle(conn, &env);
                });
            }
        });
        crate::diag::log(format!("copy for AI: sharing server listening on {addr}"));
        self.addr = Some(addr);
        let name = self.name.clone();
        std::thread::spawn(move || {
            let n = tailscale_name();
            crate::diag::log(format!("copy for AI: links use {}", n.as_deref().unwrap_or("the Tailscale address (no name found)")));
            if let Ok(mut x) = name.lock() {
                *x = n;
            }
        });
        Ok(addr)
    }

    /// Fetches the original .eml in the background right away, so the agent gets it at once.
    pub fn warm(&self, s: &Shared) {
        let (s, account, raw) = (s.clone(), self.env.account.clone(), self.env.raw.clone());
        std::thread::spawn(move || {
            let _ = eml_of(&s, &account, &raw);
        });
    }

    pub fn put(&self, token: &str, s: Shared) {
        if let Ok(mut m) = self.env.shares.lock() {
            m.insert(token.to_string(), s);
        }
    }

    /// Refreshes the shared copy of an email (e.g. after its full text arrived).
    pub fn update(&self, m: &Message) {
        if let Ok(mut map) = self.env.shares.lock() {
            for s in map.values_mut().filter(|s| s.id == m.id) {
                *s = Shared::of(m);
            }
        }
    }

    pub fn clear(&self) {
        if let Ok(mut m) = self.env.shares.lock() {
            m.clear();
        }
    }

    /// The server's address: by this computer's Tailscale name (e.g. laptop.tailnet.ts.net),
    /// or by its Tailscale address when the name isn't known.
    fn base(&self) -> Option<String> {
        let a = self.addr?;
        Some(match self.name.lock().ok().and_then(|n| n.clone()) {
            Some(n) => format!("http://{n}:{}", a.port()),
            None => format!("http://{a}"),
        })
    }

    /// The link for one shared email.
    pub fn url(&self, token: &str) -> Option<String> {
        Some(format!("{}/m/{token}", self.base()?))
    }

    /// Where agents reach the mailbox (MCP).
    pub fn mcp_url(&self) -> Option<String> {
        Some(format!("{}/mcp", self.base()?))
    }
}

/// A new random link name (128 bits, so it can't be guessed).
pub fn new_token() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A new key for agents (256 bits); None if the system gave no randomness.
pub fn new_key() -> Option<String> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).ok()?;
    Some(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// How links handed to agents start (they end when agents' key changes; see `set_key`).
pub const AGENT_LINK: &str = "ag";

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

fn handle(mut conn: TcpStream, env: &Env) -> std::io::Result<()> {
    conn.set_read_timeout(Some(std::time::Duration::from_secs(15)))?;
    conn.set_write_timeout(Some(std::time::Duration::from_secs(30)))?;
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
    let (mut origin, mut chunked) = (false, false);
    let mut auth = String::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                length = v.trim().parse().unwrap_or(0);
            } else if k.eq_ignore_ascii_case("host") {
                host = v.trim().to_string();
            } else if k.eq_ignore_ascii_case("origin") {
                origin = true;
            } else if k.eq_ignore_ascii_case("transfer-encoding") {
                chunked = true;
            } else if k.eq_ignore_ascii_case("authorization") {
                auth = v.trim().to_string();
            }
        }
    }
    let text = "text/plain; charset=utf-8";
    let host = if host.is_empty() { conn.local_addr().map(|a| a.to_string()).unwrap_or_default() } else { host };
    if path.trim_end_matches('/') == "/mcp" {
        // (the key and its generation read together, under the same lock as a change of key)
        let (key, key_gen) = env.key.lock().ok().map(|k| (k.clone(), env.key_gen.load(std::sync::atomic::Ordering::SeqCst))).unwrap_or_default();
        let Some(key) = key else {
            return respond(&mut conn, "404 Not Found", text, "", "Agent access is switched off in Poczta (Settings → AI).\n".as_bytes());
        };
        // web pages always say where they come from: none of them may use the mailbox
        if origin {
            return respond(&mut conn, "403 Forbidden", text, "", b"Not for web pages.\n");
        }
        let given = auth.strip_prefix("Bearer ").unwrap_or("").trim();
        if !same(given.as_bytes(), key.as_bytes()) {
            crate::diag::log("agents: a request with a wrong or missing key");
            return respond(&mut conn, "401 Unauthorized", text, "WWW-Authenticate: Bearer\r\n", b"A key is needed (Settings -> AI in Poczta).\n");
        }
        if method != "POST" {
            return respond(&mut conn, "405 Method Not Allowed", text, "Allow: POST\r\n", b"");
        }
        if chunked {
            return respond(&mut conn, "411 Length Required", text, "", b"");
        }
        let Some(body) = read_body(&mut conn, &buf[head_end..], length)? else {
            return respond(&mut conn, "413 Payload Too Large", text, "", b"");
        };
        return match crate::mcp::answer(env, &format!("http://{host}"), key_gen, &body) {
            Some(v) => respond(&mut conn, "200 OK", "application/json", "", v.to_string().as_bytes()),
            None => respond(&mut conn, "202 Accepted", text, "", b""),
        };
    }
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    if parts.first() != Some(&"m") || parts.len() < 2 {
        return respond(&mut conn, "404 Not Found", text, "", "Nie ma tu nic. Użyj linku skopiowanego z aplikacji pocztowej.\n".as_bytes());
    }
    let (token, format) = match parts[1].rsplit_once('.') {
        Some((t, f)) => (t.to_string(), f.to_string()),
        None => (parts[1].to_string(), String::new()),
    };
    let Some(shared) = env.shares.lock().ok().and_then(|m| m.get(&token).cloned()) else {
        return respond(&mut conn, "404 Not Found", text, "", "Ten link wygasł albo udostępnianie zostało wyłączone.\n".as_bytes());
    };
    let base = format!("http://{host}/m/{token}");
    match (method.as_str(), parts.get(2).copied(), parts.get(3).copied()) {
        ("GET", None, _) if format == "eml" => {
            let eml = eml_of(&shared, &env.account, &env.raw);
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
            let Some(body) = read_body(&mut conn, &buf[head_end..], length)? else {
                return respond(&mut conn, "413 Payload Too Large", text, "", "Szkic jest za długi (najwyżej 1 MB).\n".as_bytes());
            };
            let draft = String::from_utf8_lossy(&body).trim().to_string();
            if draft.is_empty() {
                return respond(&mut conn, "400 Bad Request", text, "", "Pusty szkic — wyślij treść odpowiedzi (Markdown).\n".as_bytes());
            }
            let _ = env.drafts.send(Draft::reply(shared.id, draft));
            if let Some(c) = &env.ctx {
                c.request_repaint();
            }
            crate::diag::log("copy for AI: a reply draft arrived");
            respond(&mut conn, "201 Created", text, "", "Szkic odpowiedzi zapisany w aplikacji (folder Szkice). Nic nie zostało wysłane.\n".as_bytes())
        }
        _ => respond(&mut conn, "404 Not Found", text, "", "Nieznany adres. Dostępne: GET …, GET ….json, GET …/html, GET …/a/N, POST …/draft\n".as_bytes()),
    }
}

/// The request's body (`length` bytes, some already read with the head); None when over 1 MB.
fn read_body(conn: &mut TcpStream, start: &[u8], length: usize) -> std::io::Result<Option<Vec<u8>>> {
    if length > 1024 * 1024 {
        return Ok(None);
    }
    let mut body = start.to_vec();
    let mut chunk = [0u8; 4096];
    while body.len() < length {
        let n = conn.read(&mut chunk)?;
        if n == 0 {
            // cut short: never acted on
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(length);
    Ok(Some(body))
}

/// Compares two keys in the same time whatever they hold (so timing doesn't give the key away).
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
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
