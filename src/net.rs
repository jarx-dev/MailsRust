//! Talking to real mail servers: receiving over IMAP, sending over SMTP.
//! Everything runs on a background thread and reports back through a channel.

use crate::model::{Account, Folder, Message};
use chrono::{Local, TimeZone};
use std::sync::mpsc::Sender;

pub enum NetResult {
    Received(InboxSync),
    Sent { id: u64 },
    Failed { what: String, error: String, id: Option<u64> },
    SignedIn(crate::oauth::LoginResult),
    SignInFailed(String),
    /// Queued changes were saved on the server.
    Synced,
    /// The full version of a message that was stored with its header only.
    Full { acct: u32, uid: u32, folder: Folder, message: Box<Message> },
    /// Copied people (DW) of an account's stored mail: (folder, uid, "a@x.pl, b@y.pl").
    Copies { acct: u32, found: Result<Vec<(Folder, u32, String)>, String> },
}

/// Runs a background job so that even an unexpected crash inside it comes back as an
/// error message instead of leaving the app waiting forever.
fn guarded<T>(job: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(job))
        .unwrap_or_else(|_| Err("nieoczekiwany błąd podczas synchronizacji".into()))
}

/// Watches the inbox on the server (IMAP IDLE): the server says at once when mail comes, and
/// `tx` gets a signal, so the app fetches it within a second or two instead of at the next
/// regular check. Runs on its own connection until `stop` is set; reconnects after a failure.
pub fn watch_inbox(account: Account, stop: std::sync::Arc<std::sync::atomic::AtomicBool>, tx: Sender<()>, ctx: eframe::egui::Context) {
    use std::sync::atomic::Ordering;
    std::thread::spawn(move || {
        let mut wait = 10u64;
        let mut beat = Heartbeat::new(true);
        while !stop.load(Ordering::Relaxed) {
            let mut seen = Seen::default();
            match guarded(|| idle_round(&account, &stop, &tx, &ctx, beat.noop, &mut seen)) {
                Ok(()) => wait = 10,
                Err(e) => {
                    // the connection was dropped (not by us): the renewal interval adapts
                    if let Some(last) = seen.last {
                        beat.dropped(seen.longest, last.elapsed());
                    }
                    crate::diag::log(format!("watching the inbox: {e} (trying again in {wait} s; renewal every {} s now)", beat.noop.as_secs()));
                    for _ in 0..wait {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_secs(1));
                    }
                    wait = (wait * 2).min(300);
                }
            }
        }
    });
}

/// How often a waiting (IDLE) connection is renewed so that routers and servers keep it: the
/// rule common in desktop mail clients. It
/// starts at 10 minutes with IDLE (5 without); each time the connection is dropped by the other
/// side, the lower bound rises to the longest silence that worked, the upper bound falls to the
/// silence that broke it (at most 29 minutes), and the interval becomes their middle (≥ 50 s).
struct Heartbeat {
    lower: std::time::Duration,
    upper: std::time::Duration,
    noop: std::time::Duration,
}

impl Heartbeat {
    fn new(with_idle: bool) -> Self {
        Heartbeat {
            lower: std::time::Duration::ZERO,
            upper: std::time::Duration::from_secs(29 * 60),
            noop: std::time::Duration::from_secs(if with_idle { 10 * 60 } else { 5 * 60 }),
        }
    }

    /// The connection was dropped: `longest` the longest silence it had lived through, `silent`
    /// the silence since the server last answered.
    fn dropped(&mut self, longest: std::time::Duration, silent: std::time::Duration) {
        if longest > self.lower {
            self.lower = longest;
        }
        if silent < self.upper {
            self.upper = silent;
            if self.lower > self.upper {
                self.lower = self.upper;
            }
            self.noop = ((self.upper + self.lower) / 2).max(std::time::Duration::from_secs(50));
        }
    }
}

/// What a waiting connection went through: when the server last answered, and the longest
/// silence that ended well.
#[derive(Default)]
struct Seen {
    last: Option<std::time::Instant>,
    longest: std::time::Duration,
}

/// One connection waiting for new mail; ends after 25 minutes (a fresh one is opened then:
/// Google's sign-in lasts an hour). Each wait lasts `renew` (see `Heartbeat`).
fn idle_round(acc: &Account, stop: &std::sync::atomic::AtomicBool, tx: &Sender<()>, ctx: &eframe::egui::Context, renew: std::time::Duration, seen: &mut Seen) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    let mut s = open_session(acc)?;
    // only looked at (EXAMINE): waiting changes nothing on the server
    s.examine("INBOX").map_err(|e| e.to_string())?;
    seen.last = Some(std::time::Instant::now());
    crate::diag::log("watching the inbox: waiting for new mail (IMAP IDLE)");
    let started = std::time::Instant::now();
    while !stop.load(Ordering::Relaxed) && started.elapsed() < std::time::Duration::from_secs(25 * 60) {
        // the wait is renewed every `renew` (keeps the connection alive through routers)
        let began = std::time::Instant::now();
        let outcome = s.idle().map_err(|e| e.to_string())?.wait_with_timeout(renew).map_err(|e| e.to_string())?;
        seen.longest = seen.longest.max(began.elapsed());
        seen.last = Some(std::time::Instant::now());
        if let imap::extensions::idle::WaitOutcome::MailboxChanged = outcome {
            crate::diag::log("watching the inbox: the server reports a change");
            if tx.send(()).is_err() {
                break;
            }
            ctx.request_repaint();
        }
    }
    let _ = s.logout();
    Ok(())
}

/// Google sign-in in the browser, on a background thread.
pub fn google_sign_in(client_id: String, client_secret: String, tx: Sender<NetResult>, ctx: eframe::egui::Context) {
    std::thread::spawn(move || {
        let res = match crate::oauth::login(&client_id, &client_secret) {
            Ok(r) => NetResult::SignedIn(r),
            Err(e) => NetResult::SignInFailed(e),
        };
        let _ = tx.send(res);
        ctx.request_repaint();
    });
}

/// Downloads a picture linked from an email (at most 8 MB).
pub fn fetch_image(url: String, tx: Sender<(String, Option<std::sync::Arc<[u8]>>)>, ctx: eframe::egui::Context) {
    std::thread::spawn(move || {
        let bytes = (|| -> Option<Vec<u8>> {
            let tls = native_tls::TlsConnector::new().ok()?;
            let agent = ureq::AgentBuilder::new()
                .tls_connector(std::sync::Arc::new(tls))
                .timeout(std::time::Duration::from_secs(20))
                .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) Poczta")
                .build();
            let resp = agent.get(&url).call().ok()?;
            let mut buf = Vec::new();
            use std::io::Read;
            resp.into_reader().take(8 * 1024 * 1024).read_to_end(&mut buf).ok()?;
            Some(buf)
        })();
        let _ = tx.send((url, bytes.map(std::sync::Arc::from)));
        ctx.request_repaint();
    });
}

fn oauth_token(acc: &Account) -> Result<String, String> {
    crate::oauth::access_token(&acc.oauth_client_id, &acc.oauth_client_secret, &acc.oauth_refresh_token)
}

pub struct Outgoing {
    pub id: u64,
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub body: String,
    /// A formatted version (with the signature), and pictures it shows: (content id, file).
    pub html: Option<String>,
    pub inline: Vec<(String, String)>,
    /// files attached to the message
    pub files: Vec<String>,
}

impl Outgoing {
    /// A copy (tests).
    pub fn clone_for_test(&self) -> Self {
        Self { id: self.id, to: self.to.clone(), cc: self.cc.clone(), subject: self.subject.clone(), body: self.body.clone(), html: self.html.clone(), inline: self.inline.clone(), files: self.files.clone() }
    }
}

/// The type of a file, from its name (for attachments).
fn mime_of(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "txt" | "log" => "text/plain; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "html" | "htm" => "text/html; charset=utf-8",
        "zip" => "application/zip",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "xml" => "application/xml",
        "json" => "application/json",
        "eml" => "message/rfc822",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

/// `known`: inbox messages the app already has (by server number), so recent ones that are
/// missing for any reason are fetched again.
/// `sent_state` / `sent_known`: the same for the folder of sent mail, synced right after.
#[allow(clippy::too_many_arguments)]
pub fn receive(
    account: Account,
    acct: u32,
    state: Option<SyncState>,
    known: std::collections::HashSet<u32>,
    others: Vec<(Folder, Option<SyncState>, std::collections::HashSet<u32>)>,
    tx: Sender<NetResult>,
    ctx: eframe::egui::Context,
) {
    std::thread::spawn(move || {
        // results go to the app batch by batch while the sync runs; the inbox first, then sent
        // mail, the bin and spam (also what was done on the phone or the web)
        // one connection for all the folders (opened again only after a folder failed)
        let mut conn: Option<Session> = None;
        let mut all = || -> Result<(), String> {
            fetch_inbox(&account, &mut conn, acct, "INBOX", Folder::Inbox, !others.is_empty(), state, &known, &tx, &ctx)?;
            let n = others.len();
            for (i, (f, st, kn)) in others.iter().enumerate() {
                let more = i + 1 < n;
                match special_mailbox(&account, *f) {
                    Some(name) => {
                        if let Err(e) = fetch_inbox(&account, &mut conn, acct, &name, *f, more, *st, kn, &tx, &ctx) {
                            // one folder failing doesn't stop the others (on a new connection);
                            // its name is looked up again next time (renamed on the server?)
                            conn = None;
                            forget_special_mailbox(&account, *f);
                            crate::diag::log(format!("sync of {:?} failed: {e}", f));
                            if !more {
                                return Err(e);
                            }
                        }
                    }
                    None => {
                        crate::diag::log(format!("no {:?} folder on the server", f));
                        let state = SyncState { complete: true, ..st.unwrap_or_default() };
                        let _ = tx.send(NetResult::Received(InboxSync {
                            acct,
                            folder: *f,
                            new: vec![],
                            flags: Default::default(),
                            labels: Default::default(),
                            window_from: None,
                            state,
                            full: false,
                            last: !more,
                        }));
                        ctx.request_repaint();
                    }
                }
            }
            if let Some(mut s) = conn.take() {
                let _ = s.logout();
            }
            Ok(())
        };
        if let Err(e) = guarded(&mut all) {
            let _ = tx.send(NetResult::Failed { what: "Receiving mail".into(), error: e, id: None });
            ctx.request_repaint();
        }
    });
}

/// The original message exactly as the server keeps it (.eml), read-only: it stays unread.
pub fn fetch_raw(account: &Account, uid: u32) -> Result<Vec<u8>, String> {
    guarded(|| {
        let mut session = open_session(account)?;
        session.examine("INBOX").map_err(|e| e.to_string())?;
        let fetches = session.uid_fetch(uid.to_string(), "(UID BODY.PEEK[])").map_err(|e| e.to_string())?;
        let raw = fetches.iter().next().and_then(|f| f.body()).map(|b| b.to_vec()).ok_or("nie ma już tej wiadomości na serwerze")?;
        drop(fetches);
        let _ = session.logout();
        Ok(raw)
    })
}

/// How a special folder is marked on the server, and its usual names.
fn special_names(f: Folder) -> (&'static str, &'static [&'static str]) {
    match f {
        Folder::Trash => ("\\Trash", &["[Gmail]/Kosz", "[Gmail]/Trash", "[Gmail]/Bin", "Trash", "Kosz", "Deleted Items", "Deleted"]),
        Folder::Junk => ("\\Junk", &["[Gmail]/Spam", "Spam", "Junk", "Junk E-mail"]),
        _ => ("\\Sent", &["[Gmail]/Sent Mail", "[Gmail]/Wys&AUI-ane", "Sent", "Sent Items", "Sent Messages", "Wys&AUI-ane"]),
    }
}

/// Folder names found per account and folder (also "none"); see `special_mailbox`.
static SPECIAL: std::sync::Mutex<std::collections::BTreeMap<String, Option<String>>> = std::sync::Mutex::new(std::collections::BTreeMap::new());

fn special_key(acc: &Account, f: Folder) -> String {
    format!("{}|{}|{}|{:?}", acc.username.trim(), acc.imap_host.trim(), acc.imap_port, f)
}

/// A folder name that no longer opens (renamed on the server): looked up again next time.
pub fn forget_special_mailbox(acc: &Account, f: Folder) {
    if let Ok(mut m) = SPECIAL.lock() {
        m.remove(&special_key(acc, f));
    }
}

/// The server's folder for sent mail, the bin or spam, as the server names it. Found once per
/// account and kept (it was a connection and a sign-in of its own on every sync).
pub fn special_mailbox(acc: &Account, f: Folder) -> Option<String> {
    let key = special_key(acc, f);
    if let Some(found) = SPECIAL.lock().ok().and_then(|m| m.get(&key).cloned()) {
        return found;
    }
    let token = if acc.use_oauth { oauth_token(acc).ok() } else { None };
    // (no connection: nothing kept, asked again next time)
    let mut c = crate::rawimap::RawImap::open(acc, token).ok()?;
    let (mark, names) = special_names(f);
    let name = c.special_folder(mark, names);
    c.logout();
    // (kept only when the server listed its folders)
    let name = name.ok()?;
    if let Ok(mut m) = SPECIAL.lock() {
        m.insert(key, name.clone());
    }
    name
}

/// Downloads the full text of one message that was first stored with its header only.
pub fn fetch_full(account: Account, acct: u32, uid: u32, folder: Folder, tx: Sender<NetResult>, ctx: eframe::egui::Context) {
    std::thread::spawn(move || {
        let res = guarded(|| {
            let mailbox = if folder == Folder::Inbox { "INBOX".to_string() } else { special_mailbox(&account, folder).ok_or("nie znaleziono folderu na serwerze")? };
            let mut session = open_session(&account)?;
            session.examine(&mailbox).map_err(|e| e.to_string())?;
            let fetches = session.uid_fetch(uid.to_string(), "(UID BODY.PEEK[] FLAGS)").map_err(|e| e.to_string())?;
            let f = fetches.iter().next().ok_or("nie ma już tej wiadomości na serwerze")?;
            let raw = f.body().ok_or("pusta odpowiedź serwera")?;
            let (seen, flagged) = flag_pair(f);
            let m = parse(raw, seen, flagged).ok_or("nie udało się odczytać wiadomości")?;
            let _ = session.logout();
            Ok(m)
        });
        let _ = tx.send(match res {
            Ok(m) => NetResult::Full { acct, uid, folder, message: Box::new(m) },
            Err(e) => NetResult::Failed { what: "Pobieranie wiadomości".into(), error: e, id: None },
        });
        ctx.request_repaint();
    });
}

pub fn send(account: Account, mail: Outgoing, tx: Sender<NetResult>, ctx: eframe::egui::Context) {
    std::thread::spawn(move || {
        let id = mail.id;
        let res = match guarded(|| smtp_send(&account, &mail)) {
            Ok(()) => NetResult::Sent { id },
            Err(e) => NetResult::Failed {
                what: "Sending mail".into(),
                error: e,
                id: Some(id),
            },
        };
        let _ = tx.send(res);
        ctx.request_repaint();
    });
}

type Session = imap::Session<native_tls::TlsStream<std::net::TcpStream>>;

/// Opens a network connection to the server quickly: IPv4 addresses first (a slow IPv6
/// route can otherwise cost many seconds), each try with a short time limit.
pub fn fast_tcp(host: &str, port: u16) -> Result<std::net::TcpStream, String> {
    use std::net::ToSocketAddrs;
    let t = std::time::Instant::now();
    let mut addrs: Vec<std::net::SocketAddr> = (host, port).to_socket_addrs().map_err(|e| e.to_string())?.collect();
    addrs.sort_by_key(|a| if a.is_ipv4() { 0 } else { 1 });
    crate::diag::took("    look up address", t, format!("{} addresses", addrs.len()));
    let t = std::time::Instant::now();
    let mut last_err = String::from("no address");
    for a in addrs {
        match std::net::TcpStream::connect_timeout(&a, std::time::Duration::from_secs(4)) {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(120)));
                crate::diag::took("    network connection", t, a.to_string());
                return Ok(s);
            }
            Err(e) => last_err = format!("{a}: {e}"),
        }
    }
    Err(last_err)
}

fn open_session(acc: &Account) -> Result<Session, String> {
    let host = acc.imap_host.trim();
    let tls = native_tls::TlsConnector::builder()
        .build()
        .map_err(|e| e.to_string())?;
    let mut client = if acc.imap_port == 993 {
        let tcp = fast_tcp(host, acc.imap_port)?;
        let t = std::time::Instant::now();
        let stream = tls.connect(host, tcp).map_err(|e| e.to_string())?;
        crate::diag::took("    encryption (TLS)", t, "");
        let mut c = imap::Client::new(stream);
        c.read_greeting().map_err(|e| e.to_string())?;
        c
    } else {
        imap::connect_starttls((host, acc.imap_port), host, &tls).map_err(|e| e.to_string())?
    };
    let _ = &mut client;
    let t = std::time::Instant::now();
    let session = if acc.use_oauth {
        let auth = crate::oauth::XOAuth2 {
            user: acc.username.trim().to_string(),
            token: oauth_token(acc)?,
        };
        client.authenticate("XOAUTH2", &auth).map_err(|e| {
            // (a kept token the server no longer takes: a fresh one next time)
            crate::oauth::forget_tokens();
            e.0.to_string()
        })
    } else {
        client.login(acc.username.trim(), &acc.password).map_err(|e| e.0.to_string())
    };
    crate::diag::took("    sign in", t, "");
    session
}

/// A change made in the app that also has to happen on the mail server.
#[derive(Clone, Debug)]
pub enum Op {
    /// Add (`true`) or remove a Gmail label.
    Label { uid: u32, label: String, add: bool },
    /// Move to the server's Trash.
    Trash { uid: u32 },
    /// Move to the server's Spam folder.
    Junk { uid: u32 },
    /// Take out of the inbox (Gmail keeps it under "All mail").
    Archive { uid: u32 },
    Seen { uid: u32, on: bool },
    Flagged { uid: u32, on: bool },
    /// The same change for a message in another folder of the server (sent mail, bin, spam).
    In(Folder, Box<Op>),
    /// Delete for good (from the bin or spam).
    Expunge { uid: u32 },
    /// Back into the inbox (from the bin or spam).
    ToInbox { uid: u32 },
}

/// Quotes a label for an IMAP command, with non-English letters in IMAP's encoding.
fn quote_label(label: &str) -> String {
    let enc = encode_mutf7(label);
    format!("\"{}\"", enc.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Carries out queued changes on the server in one connection. Returns how many failed.
pub fn run_ops(account: Account, ops: Vec<Op>, tx: Sender<NetResult>, ctx: eframe::egui::Context) {
    std::thread::spawn(move || {
        let res = guarded(|| -> Result<usize, String> {
            let token = if account.use_oauth { Some(oauth_token(&account)?) } else { None };
            let mut c = crate::rawimap::RawImap::open(&account, token)?;
            let mut trash: Option<Option<String>> = None;
            let mut junk: Option<Option<String>> = None;
            let mut failed = 0;
            let expunge = |c: &mut crate::rawimap::RawImap, uid: u32| {
                c.cmd(&format!("UID EXPUNGE {uid}")).or_else(|_| c.cmd("EXPUNGE")).is_ok()
            };
            // changes to inbox messages first, then those in the other folders, each in its own
            let mut inbox: Vec<Op> = Vec::new();
            let mut others: Vec<(Folder, Vec<Op>)> = Vec::new();
            for op in ops {
                match op {
                    Op::In(f, b) => match others.iter_mut().find(|x| x.0 == f) {
                        Some(g) => g.1.push(*b),
                        None => others.push((f, vec![*b])),
                    },
                    o => inbox.push(o),
                }
            }
            let mut groups: Vec<(String, Vec<Op>)> = Vec::new();
            if !inbox.is_empty() {
                groups.push(("INBOX".into(), inbox));
            }
            for (f, list) in others {
                let (mark, names) = special_names(f);
                match c.special_folder(mark, names).ok().flatten() {
                    Some(name) => groups.push((name, list)),
                    None => failed += list.len(),
                }
            }
            for (mailbox, ops) in groups {
            if c.cmd(&format!("SELECT {}", crate::rawimap::RawImap::quote(&mailbox))).is_err() {
                failed += ops.len();
                continue;
            }
            for op in ops {
                let ok = match op {
                    Op::In(..) => false,
                    Op::Expunge { uid } => c.cmd(&format!("UID STORE {uid} +FLAGS (\\Deleted)")).is_ok() && expunge(&mut c, uid),
                    Op::ToInbox { uid } => {
                        c.cmd(&format!("UID MOVE {uid} INBOX")).is_ok()
                            || (c.cmd(&format!("UID COPY {uid} INBOX")).is_ok()
                                && c.cmd(&format!("UID STORE {uid} +FLAGS (\\Deleted)")).is_ok()
                                && expunge(&mut c, uid))
                    }
                    Op::Label { uid, label, add } => c
                        .cmd(&format!("UID STORE {uid} {}X-GM-LABELS ({})", if add { "+" } else { "-" }, quote_label(&label)))
                        .is_ok(),
                    Op::Seen { uid, on } => c
                        .cmd(&format!("UID STORE {uid} {}FLAGS (\\Seen)", if on { "+" } else { "-" }))
                        .is_ok(),
                    Op::Flagged { uid, on } => c
                        .cmd(&format!("UID STORE {uid} {}FLAGS (\\Flagged)", if on { "+" } else { "-" }))
                        .is_ok(),
                    Op::Archive { uid } => c.cmd(&format!("UID STORE {uid} +FLAGS (\\Deleted)")).is_ok() && expunge(&mut c, uid),
                    Op::Trash { uid } | Op::Junk { uid } => {
                        let is_trash = matches!(op, Op::Trash { .. });
                        let slot = if is_trash { &mut trash } else { &mut junk };
                        if slot.is_none() {
                            *slot = Some(if is_trash {
                                c.special_folder("\\Trash", &["[Gmail]/Kosz", "[Gmail]/Trash", "Trash", "Kosz", "Deleted Items", "Deleted"]).ok().flatten()
                            } else {
                                c.special_folder("\\Junk", &["[Gmail]/Spam", "Spam", "Junk", "Junk E-mail"]).ok().flatten()
                            });
                        }
                        match slot.clone().flatten() {
                            Some(folder) => {
                                let q = crate::rawimap::RawImap::quote(&folder);
                                c.cmd(&format!("UID MOVE {uid} {q}")).is_ok()
                                    || (c.cmd(&format!("UID COPY {uid} {q}")).is_ok()
                                        && c.cmd(&format!("UID STORE {uid} +FLAGS (\\Deleted)")).is_ok()
                                        && expunge(&mut c, uid))
                            }
                            None => false,
                        }
                    }
                };
                if !ok {
                    failed += 1;
                }
            }
            }
            c.logout();
            Ok(failed)
        });
        let msg = match res {
            Ok(0) => NetResult::Synced,
            Ok(n) => NetResult::Failed { what: "Synchronizacja".into(), error: format!("{n} zmian nie zapisano na serwerze"), id: None },
            Err(e) => NetResult::Failed { what: "Synchronizacja".into(), error: e, id: None },
        };
        let _ = tx.send(msg);
        ctx.request_repaint();
    });
}

/// IMAP's "modified UTF-7" for non-English letters in folder and label names.
pub fn encode_mutf7(s: &str) -> String {
    use base64::Engine;
    let mut out = String::new();
    let mut pending: Vec<u16> = Vec::new();
    let flush = |pending: &mut Vec<u16>, out: &mut String| {
        if pending.is_empty() {
            return;
        }
        let bytes: Vec<u8> = pending.iter().flat_map(|u| u.to_be_bytes()).collect();
        let b64 = base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes).replace('/', ",");
        out.push('&');
        out.push_str(&b64);
        out.push('-');
        pending.clear();
    };
    for c in s.chars() {
        if (' '..='~').contains(&c) {
            flush(&mut pending, &mut out);
            if c == '&' {
                out.push_str("&-");
            } else {
                out.push(c);
            }
        } else {
            let mut buf = [0u16; 2];
            pending.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    flush(&mut pending, &mut out);
    out
}

/// Where the last sync of the inbox got to, so the next one only downloads what is new.
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SyncState {
    /// Changes when the server renumbers the inbox; then everything is fetched again.
    pub uidvalidity: u32,
    /// The highest message number already downloaded.
    pub last_uid: u32,
    /// The lowest message number already downloaded (older mail is fetched in batches).
    pub oldest_uid: u32,
    /// The whole inbox is on this computer.
    pub complete: bool,
}

/// What one step of a sync brought back. A sync sends several of these: first the new mail
/// and the current state of recent messages, then batches of older mail.
pub struct InboxSync {
    /// Which mail account (0: the main one).
    pub acct: u32,
    /// Which folder this is about (the inbox, or sent mail).
    pub folder: Folder,
    /// Messages not seen before.
    pub new: Vec<Message>,
    /// Current read / flagged state of the recent inbox messages (by server number).
    pub flags: std::collections::HashMap<u32, (bool, bool)>,
    /// Current Gmail labels of the recent inbox messages (empty if not Gmail).
    pub labels: std::collections::HashMap<u32, Vec<String>>,
    /// Lowest server number covered by `flags`; messages from there on that the server
    /// no longer lists were deleted or archived elsewhere.
    pub window_from: Option<u32>,
    pub state: SyncState,
    /// True when this was the first (full) download.
    pub full: bool,
    /// The last step of this sync.
    pub last: bool,
}

/// How many of the newest inbox messages are checked for read / flag / tag changes.
const WINDOW: u32 = 300;
/// How many messages the very first sync downloads before showing anything.
const FIRST: u32 = 100;
/// Messages bigger than this are first stored with their header only; the rest comes when
/// the message is opened (it is usually big because of attachments).
const BIG: u32 = 512 * 1024;
/// One sync keeps downloading older mail for at most this long, then continues next time.
const BUDGET: std::time::Duration = std::time::Duration::from_secs(20 * 60);

fn flag_pair(f: &imap::types::Fetch) -> (bool, bool) {
    let seen = f.flags().iter().any(|fl| matches!(fl, imap::types::Flag::Seen));
    let flagged = f.flags().iter().any(|fl| matches!(fl, imap::types::Flag::Flagged));
    (seen, flagged)
}

/// Downloads the given messages, in groups of 50: small ones in full, big ones header only.
fn download(session: &mut Session, uids: &[u32], into: &mut Vec<Message>) -> Result<(), String> {
    for chunk in uids.chunks(50) {
        let set = chunk.iter().map(|u| u.to_string()).collect::<Vec<_>>().join(",");
        // sizes first: a tiny request
        let t = std::time::Instant::now();
        let sizes = session.uid_fetch(&set, "(UID RFC822.SIZE)").map_err(|e| e.to_string())?;
        let mut small = Vec::new();
        let mut big = Vec::new();
        let mut small_bytes = 0usize;
        for f in sizes.iter() {
            if let Some(u) = f.uid {
                let size = f.size.unwrap_or(0);
                if size > BIG {
                    big.push(u);
                } else {
                    small.push(u);
                    small_bytes += size as usize;
                }
            }
        }
        drop(sizes);
        crate::diag::took("  sizes", t, format!("{} messages ({} small, {} big)", chunk.len(), small.len(), big.len()));
        if !small.is_empty() {
            let set = small.iter().map(|u| u.to_string()).collect::<Vec<_>>().join(",");
            let t = std::time::Instant::now();
            let fetches = session.uid_fetch(set, "(UID BODY.PEEK[] FLAGS)").map_err(|e| e.to_string())?;
            let secs = t.elapsed().as_secs_f64().max(0.001);
            crate::diag::took(
                "  download",
                t,
                format!("{} messages, {}, {:.0} KB/s", small.len(), crate::diag::kb(small_bytes), small_bytes as f64 / 1024.0 / secs),
            );
            let t = std::time::Instant::now();
            for f in fetches.iter() {
                let Some(raw) = f.body() else { continue };
                let (seen, flagged) = flag_pair(f);
                if let Some(mut m) = parse(raw, seen, flagged) {
                    m.uid = f.uid;
                    into.push(m);
                }
            }
            crate::diag::took("  read + store pictures/attachments", t, format!("{} messages", small.len()));
        }
        if !big.is_empty() {
            let set = big.iter().map(|u| u.to_string()).collect::<Vec<_>>().join(",");
            let t = std::time::Instant::now();
            let fetches = session.uid_fetch(set, "(UID BODY.PEEK[HEADER] FLAGS)").map_err(|e| e.to_string())?;
            crate::diag::took("  headers of big messages", t, format!("{}", big.len()));
            for f in fetches.iter() {
                let Some(raw) = f.header() else { continue };
                let (seen, flagged) = flag_pair(f);
                if let Some(mut m) = parse(raw, seen, flagged) {
                    m.uid = f.uid;
                    m.partial = true;
                    m.body = String::new();
                    into.push(m);
                }
            }
        }
    }
    Ok(())
}

/// Speed test for diagnostics: downloads the newest `n` inbox messages exactly as a sync
/// does, but read-only (the inbox is opened for reading, nothing is marked or changed and
/// nothing is added to the mailbox). Every step goes to the diagnostics log.
/// Read-only: date, sender and subject of one inbox message as the server has it (diagnostics).
pub fn peek_header(acc: &Account, uid: u32) -> Result<String, String> {
    let mut session = open_session(acc)?;
    session.examine("INBOX").map_err(|e| e.to_string())?;
    let f = session.uid_fetch(uid.to_string(), "(UID FLAGS BODY.PEEK[HEADER.FIELDS (DATE FROM SUBJECT MESSAGE-ID)])").map_err(|e| e.to_string())?;
    let mut out: String = f.iter().map(|x| format!("{:?}
{}", x.flags(), String::from_utf8_lossy(x.header().unwrap_or_default()))).collect();
    drop(f);
    let mut got = Vec::new();
    download(&mut session, &[uid], &mut got)?;
    out += &format!("
read by the app: {} message(s); {:?}", got.len(), got.first().map(|m| (m.read, m.date, m.partial, m.body.len())));
    let sz = session.uid_fetch(uid.to_string(), "(RFC822.SIZE)").map_err(|e| e.to_string())?;
    out += &format!("
size {:?}", sz.iter().next().and_then(|x| x.size));
    drop(sz);
    let _ = session.logout();
    Ok(out)
}

pub fn benchmark(acc: &Account, n: u32) -> Result<(), String> {
    let started = std::time::Instant::now();
    crate::diag::log(format!("── speed test: newest {n} messages, read-only"));
    let t = std::time::Instant::now();
    let mut session = open_session(acc)?;
    crate::diag::took("connect + sign in", t, &acc.imap_host);
    let t = std::time::Instant::now();
    let mbox = session.examine("INBOX").map_err(|e| e.to_string())?;
    crate::diag::took("open inbox (read-only)", t, format!("{} messages on the server", mbox.exists));
    let start = mbox.exists.saturating_sub(n - 1).max(1);
    let t = std::time::Instant::now();
    let summary = session.fetch(format!("{}:{}", start, mbox.exists), "(UID FLAGS)").map_err(|e| e.to_string())?;
    let mut uids: Vec<u32> = summary.iter().filter_map(|f| f.uid).collect();
    let unread_uids: Vec<u32> = summary.iter().filter(|f| !flag_pair(f).0).filter_map(|f| f.uid).collect();
    let server_unread = unread_uids.len();
    if !unread_uids.is_empty() {
        crate::diag::log(format!("unread on the server: {unread_uids:?}"));
    }
    drop(summary);
    crate::diag::took("summary", t, format!("{} messages, {server_unread} unread on the server", uids.len()));
    uids.sort_unstable_by(|a, b| b.cmp(a));
    let _ = session.logout();
    let mut msgs = Vec::new();
    let t = std::time::Instant::now();
    let parts: Vec<Vec<u32>> = uids.chunks(PART).map(|c| c.to_vec()).collect();
    parallel_download(acc, "INBOX", parts, WORKERS, true, |_, m| {
        msgs.extend(m);
        true
    })?;
    crate::diag::took(
        &format!("download all over {WORKERS} connections"),
        t,
        format!("{} messages, {} unread as read by the app", msgs.len(), msgs.iter().filter(|m| !m.read).count()),
    );
    let mut labels = Labels { acc, conn: None, broken: false };
    let t = std::time::Instant::now();
    labels.label(&mut msgs);
    crate::diag::took("tags", t, if labels.broken { "not available" } else { "" });
    if let Some(c) = labels.conn.take() {
        c.logout();
    }
    crate::diag::took("── speed test finished", started, format!("{} messages", msgs.len()));
    Ok(())
}

/// How many connections download older mail at the same time (Gmail allows up to 15).
const WORKERS: usize = 4;
/// Size of one piece of work for a download connection.
const PART: usize = 50;

/// Downloads the given batches over `workers` connections at once. `on_batch` gets each
/// finished batch (its index and messages) on the calling thread; returning false stops.
/// `strict`: any batch that fails makes the whole download fail (nothing may go missing).
fn parallel_download(
    acc: &Account,
    mailbox: &str,
    batches: Vec<Vec<u32>>,
    workers: usize,
    strict: bool,
    mut on_batch: impl FnMut(usize, Vec<Message>) -> bool,
) -> Result<(), String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    let total = batches.len();
    let queue: Arc<Mutex<std::collections::VecDeque<(usize, Vec<u32>)>>> =
        Arc::new(Mutex::new(batches.into_iter().enumerate().collect()));
    let stop = Arc::new(AtomicBool::new(false));
    let (rtx, rrx) = std::sync::mpsc::channel::<(usize, Result<Vec<Message>, String>)>();
    let n = workers.min(total).max(1);
    for w in 0..n {
        let (queue, stop, rtx, acc, mailbox) = (queue.clone(), stop.clone(), rtx.clone(), acc.clone(), mailbox.to_string());
        std::thread::spawn(move || {
            let t = std::time::Instant::now();
            let mut session = match open_session(&acc).and_then(|mut s| s.examine(&mailbox).map(|_| s).map_err(|e| e.to_string())) {
                Ok(s) => s,
                Err(e) => {
                    let _ = rtx.send((usize::MAX, Err(e)));
                    return;
                }
            };
            crate::diag::took(&format!("  connection {} ready", w + 1), t, "");
            while !stop.load(Ordering::Relaxed) {
                let Some((i, uids)) = queue.lock().ok().and_then(|mut q| q.pop_front()) else { break };
                let mut msgs = Vec::with_capacity(uids.len());
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| download(&mut session, &uids, &mut msgs)))
                    .unwrap_or_else(|_| Err("nieoczekiwany błąd".into()));
                let failed = r.is_err();
                let _ = rtx.send((i, r.map(|_| msgs)));
                if failed {
                    break;
                }
            }
            let _ = session.logout();
        });
    }
    drop(rtx);
    let mut done = 0;
    let mut first_error: Option<String> = None;
    while done < total {
        let Ok((i, r)) = rrx.recv() else { break };
        match r {
            Ok(msgs) => {
                done += 1;
                if !on_batch(i, msgs) {
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
            }
            Err(e) => {
                crate::diag::log(format!("  a download connection failed: {e}"));
                first_error.get_or_insert(e);
                // the other connections carry on with the remaining batches
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    match (done, first_error) {
        (0, Some(e)) if total > 0 => Err(e),
        (_, Some(e)) if strict => Err(e),
        (d, None) if strict && d < total => Err("pobieranie przerwane".into()),
        _ => Ok(()),
    }
}

/// Gmail labels over our own small connection, opened once per sync and reused.
struct Labels<'a> {
    acc: &'a Account,
    conn: Option<crate::rawimap::RawImap>,
    broken: bool,
}

impl<'a> Labels<'a> {
    fn get(&mut self, lo: u32, hi: u32) -> std::collections::HashMap<u32, Vec<String>> {
        if self.broken {
            return Default::default();
        }
        if self.conn.is_none() {
            let token = if self.acc.use_oauth { oauth_token(self.acc).ok() } else { None };
            match crate::rawimap::RawImap::open(self.acc, token).and_then(|mut c| c.cmd("EXAMINE INBOX").map(|_| c)) {
                Ok(c) => self.conn = Some(c),
                Err(_) => {
                    self.broken = true;
                    return Default::default();
                }
            }
        }
        let c = self.conn.as_mut().unwrap();
        match c.cmd(&format!("UID FETCH {lo}:{hi} (UID X-GM-LABELS)")) {
            Ok(lines) => parse_gm_labels(lines.join("\r\n").as_bytes()),
            Err(_) => {
                // not Gmail, or the connection dropped: carry on without tags
                self.broken = true;
                Default::default()
            }
        }
    }

    fn label(&mut self, msgs: &mut [Message]) -> std::collections::HashMap<u32, Vec<String>> {
        let lo = msgs.iter().filter_map(|m| m.uid).min();
        let hi = msgs.iter().filter_map(|m| m.uid).max();
        let (Some(lo), Some(hi)) = (lo, hi) else { return Default::default() };
        let map = self.get(lo, hi);
        for m in msgs.iter_mut() {
            if let Some(u) = m.uid {
                m.labels = map.get(&u).cloned().unwrap_or_default();
            }
        }
        map
    }
}

/// Syncs one folder of the server (the inbox, or sent mail): new messages and read / flag
/// changes first, then older mail until the whole folder is here. `more_after`: another folder
/// is synced right after this one, so this one's last step isn't the end of the sync.
#[allow(clippy::too_many_arguments)]
fn fetch_inbox(
    acc: &Account,
    conn: &mut Option<Session>,
    acct: u32,
    mailbox: &str,
    folder: Folder,
    more_after: bool,
    state: Option<SyncState>,
    known: &std::collections::HashSet<u32>,
    tx: &Sender<NetResult>,
    ctx: &eframe::egui::Context,
) -> Result<(), String> {
    let started = std::time::Instant::now();
    crate::diag::log(format!("── sync started: {}", if folder == Folder::Sent { "sent mail" } else { "inbox" }));
    let session = match conn {
        Some(s) => s,
        None => {
            let t = std::time::Instant::now();
            let s = open_session(acc)?;
            crate::diag::took("connect + sign in", t, &acc.imap_host);
            conn.insert(s)
        }
    };
    let t = std::time::Instant::now();
    let mbox = if folder == Folder::Inbox { session.select(mailbox) } else { session.examine(mailbox) }.map_err(|e| e.to_string())?;
    crate::diag::took("open folder", t, format!("{} messages on the server", mbox.exists));
    let uidvalidity = mbox.uid_validity.unwrap_or(0);
    let incremental = state.filter(|s| s.uidvalidity == uidvalidity && s.last_uid > 0 && uidvalidity != 0);
    // Gmail labels are read for Gmail's inbox only (other servers: no connection asked in vain)
    let mut labels = Labels { acc, conn: None, broken: folder != Folder::Inbox || !acc.imap_host.to_lowercase().contains("gmail") };
    let send = |mut s: InboxSync| {
        if more_after {
            s.last = false;
        }
        let _ = tx.send(NetResult::Received(s));
        ctx.request_repaint();
    };
    let mut first = InboxSync {
        acct,
        folder,
        new: Vec::new(),
        flags: Default::default(),
        labels: Default::default(),
        window_from: None,
        state: match incremental {
            Some(s) => s,
            None => SyncState { uidvalidity, ..Default::default() },
        },
        full: incremental.is_none(),
        last: false,
    };
    if mbox.exists == 0 {
        first.state.complete = true;
        first.last = true;
        send(first);
        return Ok(());
    }
    // 1. Small summary of the newest messages: their numbers and read / flag state.
    let start = mbox.exists.saturating_sub(WINDOW - 1).max(1);
    let t = std::time::Instant::now();
    let summary = session.fetch(format!("{}:{}", start, mbox.exists), "(UID FLAGS)").map_err(|e| e.to_string())?;
    crate::diag::took("summary of recent messages (read/flag state)", t, format!("{}", summary.len()));
    for f in summary.iter() {
        if let Some(u) = f.uid {
            first.flags.insert(u, flag_pair(f));
        }
    }
    drop(summary);
    first.window_from = first.flags.keys().min().copied();
    // 2. What is new (or the newest ones the very first time).
    let wanted: Vec<u32> = match incremental {
        Some(s) => {
            let w: Vec<u32> = first.flags.keys().copied().filter(|u| *u > s.last_uid || !known.contains(u)).collect();
            let missing = w.iter().filter(|u| **u <= s.last_uid).count();
            if missing > 0 {
                crate::diag::log(format!("{missing} recent messages were missing here: fetching them again"));
            }
            w
        }
        None => {
            let mut all: Vec<u32> = first.flags.keys().copied().collect();
            all.sort_unstable();
            all.into_iter().rev().take(FIRST as usize).collect()
        }
    };
    let t = std::time::Instant::now();
    if incremental.is_none() && wanted.len() > PART {
        // the very first time: several connections at once
        let parts: Vec<Vec<u32>> = wanted.chunks(PART / 2).map(|c| c.to_vec()).collect();
        parallel_download(acc, mailbox, parts, WORKERS, true, |_, msgs| {
            first.new.extend(msgs);
            true
        })?;
    } else {
        download(session, &wanted, &mut first.new)?;
    }
    crate::diag::took(if incremental.is_some() { "new mail" } else { "first download" }, t, format!("{} messages", wanted.len()));
    first.state.last_uid = first.flags.keys().copied().max().unwrap_or(0).max(first.state.last_uid);
    if first.state.oldest_uid == 0 {
        // not known yet (e.g. after an update): continue from the newest
        first.state.oldest_uid = wanted.iter().copied().min().unwrap_or(first.state.last_uid + 1);
    }
    let mut state = first.state;
    let done = state.complete || state.oldest_uid <= 1;
    if done {
        first.state.complete = true;
    }
    // New mail goes to the app at once; Gmail's labels (asked on a connection of their own,
    // about a second) follow as a step of their own, with the same read / flag state.
    let window = match (first.flags.keys().min().copied(), first.flags.keys().max().copied()) {
        (Some(lo), Some(hi)) if !labels.broken => Some((lo, hi)),
        _ => None,
    };
    first.last = done && window.is_none();
    let (flags, first_state) = (if window.is_some() { first.flags.clone() } else { Default::default() }, first.state);
    send(first);
    if let Some((lo, hi)) = window {
        let t = std::time::Instant::now();
        let map = labels.get(lo, hi);
        crate::diag::took("tags (Gmail labels)", t, format!("{} messages with tags", map.values().filter(|v| !v.is_empty()).count()));
        send(InboxSync { acct, folder, new: vec![], flags, labels: map, window_from: None, state: first_state, full: false, last: done });
    }
    if done {
        return Ok(());
    }

    // 3. Older mail, batch after batch on the same connection, until the whole inbox is here.
    let t = std::time::Instant::now();
    let mut older: Vec<u32> = session
        .uid_search(format!("UID 1:{}", state.oldest_uid - 1))
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|u| *u < state.oldest_uid)
        .collect();
    older.sort_unstable_by(|a, b| b.cmp(a)); // newest first
    crate::diag::took("list of older mail", t, format!("{} messages still to download", older.len()));
    // the main connection isn't needed while the parallel connections download (it would
    // time out meanwhile: the next folder opens a new one)
    let _ = session.logout();
    *conn = None;
    if older.is_empty() {
        state.complete = true;
        send(InboxSync { acct, folder, new: vec![], flags: Default::default(), labels: Default::default(), window_from: None, state, full: false, last: true });
    } else {
        // Batches of 50, newest first, downloaded by several connections at once. The saved
        // position only moves past batches that are complete with everything newer than them,
        // so an interrupted sync continues without gaps.
        let batches: Vec<Vec<u32>> = older.chunks(PART).map(|c| c.to_vec()).collect();
        let total = batches.len();
        let mut finished = vec![false; total];
        let mut prefix = 0usize;
        let mut got = 0usize;
        let t = std::time::Instant::now();
        parallel_download(acc, mailbox, batches.clone(), WORKERS, false, |i, mut msgs| {
            let t2 = std::time::Instant::now();
            labels.label(&mut msgs);
            crate::diag::took("  tags", t2, "");
            got += msgs.len();
            finished[i] = true;
            while prefix < total && finished[prefix] {
                state.oldest_uid = batches[prefix].iter().copied().min().unwrap_or(state.oldest_uid);
                prefix += 1;
            }
            state.complete = prefix == total;
            let out_of_time = started.elapsed() > BUDGET;
            let last = state.complete || out_of_time;
            crate::diag::log(format!(
                "older mail: {} of {} downloaded ({:.0} messages/s)",
                got,
                older.len(),
                got as f64 / t.elapsed().as_secs_f64().max(0.001)
            ));
            send(InboxSync { acct, folder, new: msgs, flags: Default::default(), labels: Default::default(), window_from: None, state, full: false, last });
            !last
        })?;
        if !state.complete && started.elapsed() <= BUDGET {
            // stopped early (e.g. a connection dropped): tell the app this sync is over
            send(InboxSync { acct, folder, new: vec![], flags: Default::default(), labels: Default::default(), window_from: None, state, full: false, last: true });
        }
    }
    if let Some(c) = labels.conn.take() {
        c.logout();
    }
    crate::diag::took("── sync finished", started, "");
    Ok(())
}
/// Reads Gmail's answer to `FETCH … (X-GM-LABELS)`: message number → its own labels.
/// Gmail's built-in labels (\Inbox, \Important, …) are left out.
pub fn parse_gm_labels(raw: &[u8]) -> std::collections::HashMap<u32, Vec<String>> {
    let text = String::from_utf8_lossy(raw);
    let mut out = std::collections::HashMap::new();
    for line in text.split("\r\n") {
        let Some(rest) = line.trim_start().strip_prefix("* ") else { continue };
        let Some((num, after)) = rest.split_once(' ') else { continue };
        let Ok(num) = num.parse::<u32>() else { continue };
        let Some(i) = after.find("X-GM-LABELS (") else { continue };
        let mut chars = after[i + 13..].chars().peekable();
        let mut labels = Vec::new();
        loop {
            match chars.peek() {
                None | Some(')') => break,
                Some(' ') => {
                    chars.next();
                }
                Some('"') => {
                    chars.next();
                    let mut s = String::new();
                    while let Some(c) = chars.next() {
                        match c {
                            '\\' => {
                                if let Some(n) = chars.next() {
                                    s.push(n);
                                }
                            }
                            '"' => break,
                            c => s.push(c),
                        }
                    }
                    labels.push(s);
                }
                Some(_) => {
                    let mut s = String::new();
                    while let Some(&c) = chars.peek() {
                        if c == ' ' || c == ')' {
                            break;
                        }
                        s.push(c);
                        chars.next();
                    }
                    labels.push(s);
                }
            }
        }
        let labels: Vec<String> = labels
            .into_iter()
            .filter(|l| !l.starts_with('\\') && !l.is_empty())
            .map(|l| decode_mutf7(&l))
            .collect();
        // Key by the message's server ID (UID) when the answer has it, otherwise by its position.
        let uid = after.match_indices("UID ").find_map(|(i, _)| {
            let prev = after[..i].chars().last();
            if prev.is_some_and(|c| c != ' ' && c != '(') {
                return None;
            }
            after[i + 4..].split(|c: char| !c.is_ascii_digit()).next().and_then(|d| d.parse::<u32>().ok())
        });
        out.insert(uid.unwrap_or(num), labels);
    }
    out
}

/// IMAP folder and label names write non-English letters in "modified UTF-7", e.g.
/// "Wymaga dzia&AUI-ania" → "Wymaga działania".
pub fn decode_mutf7(s: &str) -> String {
    use base64::Engine;
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let Some(end) = after.find('-') else {
            out.push_str(&rest[i..]);
            return out;
        };
        let chunk = &after[..end];
        if chunk.is_empty() {
            out.push('&');
        } else {
            let b64 = chunk.replace(',', "/");
            match base64::engine::general_purpose::STANDARD_NO_PAD.decode(b64.trim_end_matches('=')) {
                Ok(bytes) => {
                    let units: Vec<u16> = bytes.chunks_exact(2).map(|p| u16::from_be_bytes([p[0], p[1]])).collect();
                    out.push_str(&String::from_utf16_lossy(&units));
                }
                Err(_) => {
                    out.push('&');
                    out.push_str(chunk);
                    out.push('-');
                }
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// "a@x.pl, b@y.pl": the addresses of a To / Cc header.
fn addresses(list: Option<mail_parser::AddressList>) -> String {
    list.map(|a| a.mailboxes().filter_map(|x| x.address().map(|s| s.to_string())).collect::<Vec<_>>().join(", ")).unwrap_or_default()
}

/// Once per account, for mail stored before the app kept copied people (DW): the Cc line of
/// every message in its folders, read without changing anything on the server.
pub fn fill_copies(account: Account, acct: u32, tx: Sender<NetResult>, ctx: eframe::egui::Context) {
    std::thread::spawn(move || {
        let t = std::time::Instant::now();
        let found = guarded(|| {
            let mut session = open_session(&account)?;
            let mut found = Vec::new();
            for f in [Folder::Inbox, Folder::Sent, Folder::Trash, Folder::Junk] {
                // (the special folders are known after the first sync, which runs before this)
                let Some(name) = (if f == Folder::Inbox { Some("INBOX".into()) } else { special_mailbox(&account, f) }) else { continue };
                session.examine(&name).map_err(|e| e.to_string())?;
                let fetches = session.uid_fetch("1:*", "(UID BODY.PEEK[HEADER.FIELDS (CC)])").map_err(|e| e.to_string())?;
                for x in fetches.iter() {
                    let (Some(uid), Some(h)) = (x.uid, x.header()) else { continue };
                    let cc = mail_parser::MessageParser::default().parse(h).map(|m| addresses(m.cc())).unwrap_or_default();
                    if !cc.is_empty() {
                        found.push((f, uid, cc));
                    }
                }
            }
            let _ = session.logout();
            Ok(found)
        });
        crate::diag::took("copied people (DW) read", t, format!("{}: {}", account.email, found.as_ref().map(|f| format!("{} messages with DW", f.len())).unwrap_or_else(|e| e.clone())));
        let _ = tx.send(NetResult::Copies { acct, found });
        ctx.request_repaint();
    });
}

pub fn parse(raw: &[u8], seen: bool, flagged: bool) -> Option<Message> {
    let msg = mail_parser::MessageParser::default().parse(raw)?;
    let (from_name, from_addr) = msg
        .from()
        .and_then(|a| a.first())
        .map(|a| {
            let addr = a.address().unwrap_or("").to_string();
            let name = a.name().map(|n| n.to_string()).unwrap_or_else(|| addr.clone());
            (name, addr)
        })
        .unwrap_or_else(|| ("(unknown sender)".into(), String::new()));
    let (to, cc) = (addresses(msg.to()), addresses(msg.cc()));
    let date = msg
        .date()
        .and_then(|d| Local.timestamp_opt(d.to_timestamp(), 0).single())
        .map(|d| d.naive_local())
        .unwrap_or_else(|| Local::now().naive_local());
    let body = msg
        .body_text(0)
        .map(|b| b.to_string())
        .unwrap_or_default();
    // Pictures inside the email (referenced from the HTML as "cid:…").
    let mut inline = Vec::new();
    for part in msg.parts() {
        let Some(cid) = part.content_id() else { continue };
        let Some(ct) = part.content_type() else { continue };
        if !ct.ctype().eq_ignore_ascii_case("image") {
            continue;
        }
        let ext = ct.subtype().unwrap_or("img").to_lowercase();
        let path = crate::model::cache_put(&part.decoded(), &ext);
        inline.push((cid.trim_matches(['<', '>']).to_string(), path));
    }
    let html_path = msg
        .body_html(0)
        .filter(|h| h.to_lowercase().contains('<'))
        .map(|h| crate::model::cache_put(h.as_bytes(), "html"))
        .unwrap_or_default();
    // Attachments: the name for the list, and the file itself in the cache so it can be opened or saved.
    let mut attachments = Vec::new();
    let mut attachment_files = Vec::new();
    for p in msg.attachments() {
        // leave out the pictures that are shown inside the email
        if p.content_id().is_some() && p.attachment_name().is_none() {
            continue;
        }
        let name = p
            .attachment_name()
            .map(|n| n.to_string())
            .unwrap_or_else(|| if p.is_message() { "wiadomość.eml".into() } else { "załącznik".into() });
        let ext = std::path::Path::new(&name)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_else(|| "bin".into());
        let bytes = if p.is_message() { std::borrow::Cow::Borrowed(p.raw_body()) } else { p.decoded() };
        attachment_files.push(crate::model::cache_put(&bytes, &ext));
        attachments.push(name);
    }
    Some(Message {
        id: 0,
        folder: Folder::Inbox,
        account: 0,
        from_name,
        from_addr,
        to,
        cc,
        subject: msg.subject().unwrap_or("(no subject)").to_string(),
        body,
        date,
        read: seen,
        flagged,
        attachments,
        sample: false,
        html_path,
        inline,
        attachment_files,
        labels: vec![],
        uid: None,
        body_path: String::new(),
        partial: false,
        snippet: String::new(),
        on_disk: false,
    })
}

/// The email as it goes out: plain text, or plain text plus a formatted version with its pictures.
pub fn build_email(acc: &Account, m: &Outgoing) -> Result<lettre::Message, String> {
    use lettre::message::header::ContentType;
    use lettre::message::{Attachment, MultiPart, SinglePart};
    let from = format!("{} <{}>", acc.display_name, acc.email);
    let mut b = lettre::Message::builder()
        .from(from.parse().map_err(|e| format!("Your address: {e}"))?)
        .subject(m.subject.clone());
    let mut any = false;
    for r in m.to.split([',', ';']).map(str::trim).filter(|s| !s.is_empty()) {
        b = b.to(r.parse().map_err(|e| format!("\"{r}\": {e}"))?);
        any = true;
    }
    for r in m.cc.split([',', ';']).map(str::trim).filter(|s| !s.is_empty()) {
        b = b.cc(r.parse().map_err(|e| format!("\"{r}\": {e}"))?);
    }
    if !any {
        return Err("Add at least one recipient.".into());
    }
    // the message itself (plain, or formatted with its pictures)
    let content: MultiPart = match &m.html {
        None => MultiPart::mixed().singlepart(SinglePart::plain(m.body.clone())),
        Some(html) => {
            let mut related = MultiPart::related().singlepart(SinglePart::html(html.clone()));
            for (cid, file) in &m.inline {
                let Ok(bytes) = std::fs::read(file) else { continue };
                let kind = match file.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
                    "png" => "image/png",
                    "jpg" | "jpeg" => "image/jpeg",
                    "gif" => "image/gif",
                    "webp" => "image/webp",
                    _ => "application/octet-stream",
                };
                let ct = ContentType::parse(kind).map_err(|e| e.to_string())?;
                related = related.singlepart(Attachment::new_inline(cid.clone()).body(bytes, ct));
            }
            MultiPart::alternative().singlepart(SinglePart::plain(m.body.clone())).multipart(related)
        }
    };
    if m.files.is_empty() {
        if m.html.is_none() {
            return b.header(ContentType::TEXT_PLAIN).body(m.body.clone()).map_err(|e| e.to_string());
        }
        return b.multipart(content).map_err(|e| e.to_string());
    }
    // attached files after the message
    let mut mixed = if m.html.is_none() { MultiPart::mixed().singlepart(SinglePart::plain(m.body.clone())) } else { MultiPart::mixed().multipart(content) };
    for f in &m.files {
        let bytes = std::fs::read(f).map_err(|e| format!("Nie można odczytać załącznika {f}: {e}"))?;
        let name = std::path::Path::new(f).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "plik".into());
        let ct = ContentType::parse(mime_of(&name)).map_err(|e| e.to_string())?;
        mixed = mixed.singlepart(Attachment::new(name).body(bytes, ct));
    }
    b.multipart(mixed).map_err(|e| e.to_string())
}

fn smtp_send(acc: &Account, m: &Outgoing) -> Result<(), String> {
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{SmtpTransport, Transport};
    let email = build_email(acc, m)?;
    let host = acc.smtp_host.trim();
    let builder = if acc.smtp_port == 465 {
        SmtpTransport::relay(host)
    } else {
        SmtpTransport::starttls_relay(host)
    }
    .map_err(|e| e.to_string())?;
    let mailer = if acc.use_oauth {
        builder
            .port(acc.smtp_port)
            .credentials(Credentials::new(acc.username.trim().to_string(), oauth_token(acc)?))
            .authentication(vec![lettre::transport::smtp::authentication::Mechanism::Xoauth2])
            .build()
    } else {
        builder
            .port(acc.smtp_port)
            .credentials(Credentials::new(acc.username.trim().to_string(), acc.password.clone()))
            .build()
    };
    mailer.send(&email).map_err(|e| {
        if acc.use_oauth {
            crate::oauth::forget_tokens();
        }
        e.to_string()
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn copied_people_are_read() {
        let raw = b"From: A <a@x.pl>\r\nTo: B <b@x.pl>\r\nCc: C <c@x.pl>,\r\n =?UTF-8?Q?D=C5=BA?= <d@x.pl>\r\nSubject: s\r\n\r\nbody\r\n";
        let m = super::parse(raw, false, false).unwrap();
        assert_eq!((m.to.as_str(), m.cc.as_str()), ("b@x.pl", "c@x.pl, d@x.pl"));
    }
}
