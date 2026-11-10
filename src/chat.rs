//! Google Chat through the person's own Google sign-in (Google Workspace accounts only).
//!
//! One worker thread owns the conversations: it keeps them in `chat.redb` next to the mailbox,
//! asks Google what changed (the list of conversations first, then only the conversations with
//! something new, several at once) and sends the window copies of what changed over a channel.
//! The window never waits on it.

use crate::model::Account;
use chrono::{DateTime, Local, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const SCOPE_SPACES: &str = "https://www.googleapis.com/auth/chat.spaces.readonly";
pub const SCOPE_READ: &str = "https://www.googleapis.com/auth/chat.messages.readonly";
pub const SCOPE_SEND: &str = "https://www.googleapis.com/auth/chat.messages.create";
pub const SCOPE_MEMBERS: &str = "https://www.googleapis.com/auth/chat.memberships.readonly";
pub const SCOPE_READSTATE: &str = "https://www.googleapis.com/auth/chat.users.readstate";
/// Everything the chat asks for, added to the usual sign-in (mail, calendar, contacts).
pub const SCOPES: &str = "https://www.googleapis.com/auth/chat.spaces.readonly https://www.googleapis.com/auth/chat.messages.readonly https://www.googleapis.com/auth/chat.messages.create https://www.googleapis.com/auth/chat.memberships.readonly https://www.googleapis.com/auth/chat.users.readstate";

pub const ENABLE_API_URL: &str = "https://console.cloud.google.com/apis/library/chat.googleapis.com";
pub const CONFIGURE_URL: &str = "https://console.cloud.google.com/apis/api/chat.googleapis.com/hangouts-chat";
pub const WEB_URL: &str = "https://chat.google.com/";

const API: &str = "https://chat.googleapis.com/v1";
/// messages kept per conversation (older ones stay in Google Chat)
const KEEP: usize = 400;
/// messages fetched for a conversation never read here before
const FIRST: usize = 50;
/// conversations filled at the first sync; older ones are filled when opened
const BACKFILL: usize = 60;
/// conversations fetched at once (each request waits ~0.5–1 s on Google)
const PARALLEL: usize = 6;

/// The sign-in allows reading the chats.
pub fn allowed(acc: &Account) -> bool {
    crate::google::has_scope(acc, SCOPE_SPACES) && crate::google::has_scope(acc, SCOPE_READ)
}

pub fn can_send(acc: &Account) -> bool {
    crate::google::has_scope(acc, SCOPE_SEND)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Msg {
    pub name: String,
    /// "users/123…"
    pub sender: String,
    pub who: String,
    /// when it was written, in microseconds since 1970 (UTC)
    pub at: i64,
    #[serde(default)]
    pub edited: bool,
    pub text: String,
    /// attached files: (name, address to open them in the browser)
    #[serde(default)]
    pub files: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Space {
    name: String,
    kind: String,
    title: String,
    /// the title came from Google or the members (not guessed from the messages)
    #[serde(default)]
    named: bool,
    external: bool,
    uri: String,
    /// the newest message in it, as Google last said
    active: i64,
    /// `active` when its messages were last fetched (a deleted newest message must not make
    /// every round fetch again)
    fetched_to: i64,
    /// read up to here (0: never read, or not known)
    read: i64,
    /// its history was fetched at least once
    filled: bool,
    msgs: Vec<Msg>,
}

/// One conversation in the list.
#[derive(Clone, Debug, Default)]
pub struct Row {
    pub name: String,
    pub title: String,
    pub kind: String,
    pub external: bool,
    pub active: i64,
    pub last: String,
    pub unread: usize,
    pub uri: String,
}

/// How often the worker asks Google: the chat on screen, the app open, or the app in the background.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pace {
    Watching,
    Open,
    Background,
}

pub enum Cmd {
    Open(String),
    Send { space: String, text: String, id: String },
    /// read up to this message time (only what the window has shown)
    MarkRead(String, i64),
    Refresh,
    Pace(Pace),
    Account(Box<Account>),
}

pub struct Note {
    pub space: String,
    pub title: String,
    pub who: String,
    pub text: String,
}

pub enum Update {
    Rows(Vec<Row>),
    Messages(String, Arc<Vec<Msg>>),
    Sent { id: String, result: Result<(), String> },
    /// new messages from others since the last round (never from the first one)
    Fresh(Vec<Note>),
    /// a round ended: Ok, or what went wrong (in Polish)
    Done(Result<(), String>),
    Busy,
}

// ------------------------------------------------------------------ chat.redb

const SPACES_T: TableDefinition<&str, &[u8]> = TableDefinition::new("spaces");
const META_T: TableDefinition<&str, &str> = TableDefinition::new("meta");

/// Opened by the worker, closed by the app before it ends (an unclosed redb file is checked in
/// full on the next start). Locked only around a transaction, never during a request.
static DB: Mutex<Option<Database>> = Mutex::new(None);
/// The worker that may write; an older one still finishing a request stops instead.
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn db() -> std::sync::MutexGuard<'static, Option<Database>> {
    DB.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn path() -> std::path::PathBuf {
    crate::db::path().with_file_name("chat.redb")
}

/// Opens chat.redb for the worker of generation `gen` (checked under the lock: a replaced or
/// ended worker neither reopens it nor rewrites it).
fn open_db(gen: u64) -> Result<(), String> {
    let mut g = db();
    if GENERATION.load(Ordering::SeqCst) != gen {
        return Err("czat zatrzymany".into());
    }
    if g.is_some() {
        return Ok(());
    }
    let d = Database::builder().set_cache_size(2 << 20).create(path()).map_err(|e| e.to_string())?;
    let w = d.begin_write().map_err(|e| e.to_string())?;
    {
        w.open_table(SPACES_T).map_err(|e| e.to_string())?;
        w.open_table(META_T).map_err(|e| e.to_string())?;
    }
    w.commit().map_err(|e| e.to_string())?;
    *g = Some(d);
    Ok(())
}

/// Closes chat.redb (the app is ending); later writes are skipped.
pub fn close() -> bool {
    GENERATION.fetch_add(1, Ordering::SeqCst);
    db().take().is_some()
}

/// The stored conversations of this Google account; another account's are wiped first.
fn load(gen: u64, owner: &str) -> Result<HashMap<String, Space>, String> {
    let g = db();
    if GENERATION.load(Ordering::SeqCst) != gen {
        return Err("czat zatrzymany".into());
    }
    let d = g.as_ref().ok_or("baza czatu zamknięta")?;
    let stored_owner = d.begin_read().ok().and_then(|r| r.open_table(META_T).ok()?.get("owner").ok()?.map(|v| v.value().to_string()));
    if stored_owner.as_deref() != Some(owner) {
        let w = d.begin_write().map_err(|e| e.to_string())?;
        {
            let mut t = w.open_table(SPACES_T).map_err(|e| e.to_string())?;
            t.retain(|_, _| false).map_err(|e| e.to_string())?;
            w.open_table(META_T).map_err(|e| e.to_string())?.insert("owner", owner).map_err(|e| e.to_string())?;
        }
        w.commit().map_err(|e| e.to_string())?;
        return Ok(HashMap::new());
    }
    let r = d.begin_read().map_err(|e| e.to_string())?;
    let t = r.open_table(SPACES_T).map_err(|e| e.to_string())?;
    let mut out = HashMap::new();
    for item in t.iter().map_err(|e| e.to_string())? {
        let (_, v) = item.map_err(|e| e.to_string())?;
        if let Ok(s) = serde_json::from_slice::<Space>(v.value()) {
            out.insert(s.name.clone(), s);
        }
    }
    Ok(out)
}

fn store(gen: u64, changed: &[&Space], gone: &[String]) -> Result<(), String> {
    if changed.is_empty() && gone.is_empty() {
        // a round with nothing new costs no disk write
        return Ok(());
    }
    let g = db();
    let Some(d) = g.as_ref() else { return Ok(()) };
    if GENERATION.load(Ordering::SeqCst) != gen {
        return Ok(());
    }
    let w = d.begin_write().map_err(|e| e.to_string())?;
    {
        let mut t = w.open_table(SPACES_T).map_err(|e| e.to_string())?;
        for s in changed {
            let bytes = serde_json::to_vec(s).map_err(|e| e.to_string())?;
            t.insert(s.name.as_str(), bytes.as_slice()).map_err(|e| e.to_string())?;
        }
        for name in gone {
            t.remove(name.as_str()).map_err(|e| e.to_string())?;
        }
    }
    w.commit().map_err(|e| e.to_string())
}

// ------------------------------------------------------------------ times

fn micros(s: &str) -> i64 {
    DateTime::parse_from_rfc3339(s).map(|d| d.timestamp_micros()).unwrap_or(0)
}

fn rfc3339(us: i64) -> String {
    Utc.timestamp_micros(us).single().map(|d| d.to_rfc3339_opts(SecondsFormat::Micros, false)).unwrap_or_default()
}

pub fn local(us: i64) -> NaiveDateTime {
    Local.timestamp_micros(us).single().map(|d| d.naive_local()).unwrap_or_default()
}

pub fn now() -> i64 {
    Utc::now().timestamp_micros()
}

/// A request id that makes a retried send arrive once.
pub fn new_id() -> String {
    let mut b = [0u8; 16];
    if getrandom::fill(&mut b).is_err() {
        b[..8].copy_from_slice(&now().to_le_bytes());
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ------------------------------------------------------------------ Google's answers

fn parse_msg(v: &Value) -> Option<Msg> {
    let name = v["name"].as_str()?.to_string();
    if v["deleteTime"].as_str().is_some_and(|t| !t.is_empty()) {
        return None;
    }
    let mut text = v["text"].as_str().unwrap_or("").to_string();
    if text.is_empty() && v["cardsV2"].as_array().is_some_and(|a| !a.is_empty()) {
        text = "[karta aplikacji — otwórz w Google Chat]".into();
    }
    let files = v["attachment"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|f| (f["contentName"].as_str().unwrap_or("załącznik").to_string(), f["downloadUri"].as_str().unwrap_or("").to_string()))
                .collect()
        })
        .unwrap_or_default();
    Some(Msg {
        name,
        sender: v["sender"]["name"].as_str().unwrap_or("").to_string(),
        who: v["sender"]["displayName"].as_str().unwrap_or("").to_string(),
        at: micros(v["createTime"].as_str().unwrap_or("")),
        edited: v["lastUpdateTime"].as_str().is_some_and(|t| !t.is_empty()),
        text,
        files,
    })
}

fn parse_page(v: &Value) -> Vec<Msg> {
    v["messages"].as_array().map(|a| a.iter().filter_map(parse_msg).collect()).unwrap_or_default()
}

/// What went wrong, in words the person can act on.
fn explain(code: u16, body: &str) -> String {
    let message = serde_json::from_str::<Value>(body).ok().and_then(|v| v["error"]["message"].as_str().map(String::from)).unwrap_or_else(|| body.chars().take(200).collect());
    if code == 401 {
        crate::oauth::forget_tokens();
        return "Google nie przyjął logowania — zaloguj się ponownie przez Google".into();
    }
    if body.contains("Google Chat app not found") {
        return "wysyłanie wymaga skonfigurowania aplikacji Chat w Twoim projekcie Google Cloud (nazwa, ikona, opis) — zobacz instrukcję w module Czat".into();
    }
    if body.contains("SERVICE_DISABLED") || body.contains("has not been used in project") {
        return "w Twoim projekcie Google Cloud nie jest włączone Google Chat API — włącz je, odczekaj minutę i spróbuj ponownie".into();
    }
    if message.contains("only available to Google Workspace") {
        return "czat Google działa tylko dla kont Google Workspace".into();
    }
    if code == 403 && (body.contains("insufficient") || body.contains("ACCESS_TOKEN_SCOPE_INSUFFICIENT")) {
        return "brak zgody na czat — zaloguj się ponownie przez Google i zaznacz wszystkie pola".into();
    }
    if code == 429 {
        return "Google prosi o wolniejsze odpytywanie — spróbuję za chwilę".into();
    }
    format!("Google Chat odpowiedział {code}: {}", message.chars().take(200).collect::<String>())
}

const NO_CONNECTION: &str = "brak połączenia z Google";

/// One request. An empty or odd body is an error, never an empty answer (an empty list of
/// conversations would wipe the stored ones).
fn call(http: &ureq::Agent, acc: &Account, method: &str, url: &str, body: Option<Value>) -> Result<Value, String> {
    let token = crate::google::token(acc)?;
    let req = http.request(method, url).set("Authorization", &format!("Bearer {token}"));
    let resp = match body {
        Some(b) => req.send_json(b),
        None => req.call(),
    };
    match resp {
        Ok(r) => {
            let text = r.into_string().map_err(|e| format!("{NO_CONNECTION}: {e}"))?;
            match serde_json::from_str::<Value>(&text) {
                Ok(v) if v.is_object() => Ok(v),
                _ => Err(format!("niezrozumiała odpowiedź Google ({} bajtów)", text.len())),
            }
        }
        Err(ureq::Error::Status(code, r)) => Err(explain(code, &r.into_string().unwrap_or_default())),
        Err(e) => Err(format!("{NO_CONNECTION}: {e}")),
    }
}

/// What one conversation needs fetched.
struct Job {
    name: String,
    /// its newest stored message (0: only the last page is wanted)
    after: i64,
    /// look up the people in it, for its name
    members: bool,
}

/// What came back for one conversation.
struct Got {
    name: String,
    msgs: Vec<Msg>,
    /// the messages replace the stored ones (too many new ones to fetch them all: no gap is left)
    replace: bool,
    read: Option<i64>,
    people: Option<Vec<String>>,
}

fn download(http: &ureq::Agent, acc: &Account, me: &str, job: &Job) -> Result<Got, String> {
    let name = &job.name;
    let last_page = |n: usize| -> Result<Vec<Msg>, String> {
        let v = call(http, acc, "GET", &format!("{API}/{name}/messages?pageSize={n}&orderBy={}", crate::google::enc("createTime desc")), None)?;
        let mut page = parse_page(&v);
        page.reverse();
        Ok(page)
    };
    let (msgs, replace) = if job.after == 0 {
        (last_page(FIRST)?, false)
    } else {
        let filter = crate::google::enc(&format!("createTime > \"{}\"", rfc3339(job.after)));
        let mut got = vec![];
        let mut page = String::new();
        let mut complete = false;
        for _ in 0..3 {
            let more = if page.is_empty() { String::new() } else { format!("&pageToken={}", crate::google::enc(&page)) };
            let v = call(http, acc, "GET", &format!("{API}/{name}/messages?pageSize=200&orderBy={}&filter={filter}{more}", crate::google::enc("createTime asc")), None)?;
            got.extend(parse_page(&v));
            match v["nextPageToken"].as_str() {
                Some(p) if !p.is_empty() => page = p.to_string(),
                _ => {
                    complete = true;
                    break;
                }
            }
        }
        if complete {
            (got, false)
        } else {
            (last_page(FIRST)?, true)
        }
    };
    let read = if crate::google::has_scope(acc, SCOPE_READSTATE) {
        call(http, acc, "GET", &format!("{API}/users/me/{name}/spaceReadState"), None).ok().map(|v| micros(v["lastReadTime"].as_str().unwrap_or("")))
    } else {
        None
    };
    // the names are a nicety: a failed lookup keeps the messages already fetched
    let people = if job.members && crate::google::has_scope(acc, SCOPE_MEMBERS) {
        call(http, acc, "GET", &format!("{API}/{name}/members?pageSize=100&filter={}", crate::google::enc("member.type = \"HUMAN\"")), None).ok().map(|v| {
            let mut people: Vec<String> = vec![];
            for m in v["memberships"].as_array().cloned().unwrap_or_default() {
                let who = m["member"]["displayName"].as_str().unwrap_or("");
                if m["member"]["name"].as_str() != Some(me) && !who.is_empty() && !people.iter().any(|p| p == who) {
                    people.push(who.to_string());
                }
            }
            people
        })
    } else {
        None
    };
    Ok(Got { name: name.clone(), msgs, replace, read, people })
}

/// Several conversations at once; each answer comes back with its own result.
fn download_all(http: &ureq::Agent, acc: &Account, me: &str, jobs: &[Job]) -> Vec<Result<Got, String>> {
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<(usize, Result<Got, String>)>> = Mutex::new(vec![]);
    std::thread::scope(|sc| {
        for _ in 0..jobs.len().min(PARALLEL) {
            sc.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(job) = jobs.get(i) else { break };
                let r = download(http, acc, me, job).map_err(|e| format!("{}: {e}", job.name));
                out.lock().unwrap_or_else(|e| e.into_inner()).push((i, r));
            });
        }
    });
    let mut out = out.into_inner().unwrap_or_else(|e| e.into_inner());
    out.sort_by_key(|x| x.0);
    out.into_iter().map(|x| x.1).collect()
}

// ------------------------------------------------------------------ the worker

/// Starts the worker for this account. Dropping the sender stops it.
pub fn start(acc: Account, ctx: eframe::egui::Context) -> (Sender<Cmd>, Receiver<Update>) {
    let (cmd_tx, cmd_rx) = channel();
    let (up_tx, up_rx) = channel();
    let gen = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let _ = std::thread::Builder::new().name("chat".into()).spawn(move || {
        crate::diag::background_thread();
        let mut w = Worker { gen, me: me(&acc), acc, spaces: HashMap::new(), open: None, pace: Pace::Open, out: up_tx, ctx, http: None, synced_once: false, failures: 0, last_round: None, round_at: 0, read_retry: HashMap::new(), round_soon: false };
        w.run(cmd_rx);
    });
    (cmd_tx, up_rx)
}

fn me(acc: &Account) -> String {
    if acc.google_sub.is_empty() {
        String::new()
    } else {
        format!("users/{}", acc.google_sub)
    }
}

struct Worker {
    gen: u64,
    acc: Account,
    me: String,
    spaces: HashMap<String, Space>,
    open: Option<String>,
    pace: Pace,
    out: Sender<Update>,
    ctx: eframe::egui::Context,
    /// connections kept between requests (a new TLS handshake costs 0.3–1 s)
    http: Option<ureq::Agent>,
    synced_once: bool,
    failures: u32,
    last_round: Option<Instant>,
    /// when the last good round started (µs): a conversation new since then notifies
    round_at: i64,
    /// conversations whose read mark Google didn't get yet
    read_retry: HashMap<String, i64>,
    /// a round is wanted at once (a sent message could not be fetched back)
    round_soon: bool,
}

impl Worker {
    fn send(&self, u: Update) {
        let _ = self.out.send(u);
        self.ctx.request_repaint();
    }

    fn alive(&self) -> bool {
        GENERATION.load(Ordering::SeqCst) == self.gen
    }

    fn run(&mut self, rx: Receiver<Cmd>) {
        let t = Instant::now();
        let loaded = open_db(self.gen).and_then(|_| load(self.gen, &self.me));
        match loaded {
            Ok(s) => {
                crate::diag::took("chat: stored conversations read", t, format!("{} conversations", s.len()));
                self.spaces = s;
            }
            Err(e) => crate::diag::log(format!("chat: database error: {e}")),
        }
        self.send(Update::Rows(self.rows()));
        let mut next = Instant::now();
        loop {
            if !self.alive() {
                break;
            }
            match rx.recv_timeout(next.saturating_duration_since(Instant::now())) {
                Ok(cmd) => {
                    if self.handle(cmd) {
                        next = Instant::now();
                    }
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if Instant::now() < next {
                continue;
            }
            let res = self.round();
            self.last_round = Some(Instant::now());
            let wait = match &res {
                Ok(()) => {
                    self.failures = 0;
                    match self.pace {
                        Pace::Watching => 15,
                        Pace::Open => 60,
                        Pace::Background => 180,
                    }
                }
                Err(e) => {
                    crate::diag::log(format!("chat: {e}"));
                    self.failures += 1;
                    (30u64 << self.failures.min(4)).min(600)
                }
            };
            self.send(Update::Done(res));
            next = Instant::now() + Duration::from_secs(wait);
        }
        crate::diag::log("chat: worker stopped");
    }

    /// Returns true when a round is due at once.
    fn handle(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Open(name) => {
                self.open = Some(name.clone());
                if let Some(s) = self.spaces.get(&name) {
                    self.send(Update::Messages(name.clone(), Arc::new(s.msgs.clone())));
                }
                // what is new in it, and its latest messages again (edits, deletions)
                if let Err(e) = self.refresh_open() {
                    crate::diag::log(format!("chat: {e}"));
                }
                false
            }
            Cmd::Send { space, text, id } => {
                let result = self.post(&space, &text, &id);
                if let Err(e) = &result {
                    crate::diag::log(format!("chat: sending failed: {e}"));
                }
                self.send(Update::Sent { id, result });
                std::mem::take(&mut self.round_soon)
            }
            Cmd::MarkRead(space, upto) => {
                if let Err(e) = self.mark_read(&space, upto) {
                    crate::diag::log(format!("chat: marking as read failed: {e}"));
                }
                false
            }
            Cmd::Refresh => true,
            Cmd::Pace(p) => {
                let was = self.pace;
                self.pace = p;
                // the chat came on screen: news at once if the last round is older than its pace
                p == Pace::Watching && was != Pace::Watching && self.last_round.is_none_or(|t| t.elapsed() > Duration::from_secs(15))
            }
            // a new token for the same Google account (another account gets a new worker)
            Cmd::Account(a) => {
                self.acc = *a;
                true
            }
        }
    }

    fn agent(&mut self) -> Result<ureq::Agent, String> {
        if let Some(a) = &self.http {
            return Ok(a.clone());
        }
        let a = crate::google::agent()?;
        self.http = Some(a.clone());
        Ok(a)
    }

    /// A dropped connection is made again on the next request.
    fn check<T>(&mut self, r: Result<T, String>) -> Result<T, String> {
        if r.as_ref().is_err_and(|e| e.contains(NO_CONNECTION)) {
            self.http = None;
        }
        r
    }

    fn job(&self, name: &str) -> Option<Job> {
        let s = self.spaces.get(name)?;
        Some(Job { name: name.to_string(), after: if s.filled { s.msgs.last().map(|m| m.at).unwrap_or(0) } else { 0 }, members: !s.named })
    }

    /// One round: the list of conversations, then the new messages of those that changed.
    fn round(&mut self) -> Result<(), String> {
        let t = Instant::now();
        let started = now();
        self.send(Update::Busy);
        let http = self.agent()?;
        let mut listed: Vec<Value> = vec![];
        let mut page = String::new();
        loop {
            let more = if page.is_empty() { String::new() } else { format!("&pageToken={}", crate::google::enc(&page)) };
            let v = call(&http, &self.acc, "GET", &format!("{API}/spaces?pageSize=1000{more}"), None);
            let v = self.check(v)?;
            listed.extend(v["spaces"].as_array().cloned().unwrap_or_default());
            match v["nextPageToken"].as_str() {
                Some(p) if !p.is_empty() => page = p.to_string(),
                _ => break,
            }
        }
        let mut dirty: HashSet<String> = HashSet::new();
        let mut seen: HashSet<String> = HashSet::new();
        for v in &listed {
            let Some(name) = v["name"].as_str() else { continue };
            seen.insert(name.to_string());
            let s = self.spaces.entry(name.to_string()).or_insert_with(|| Space { name: name.to_string(), ..Default::default() });
            let before = (s.kind.clone(), s.title.clone(), s.external, s.uri.clone(), s.active);
            s.kind = v["spaceType"].as_str().unwrap_or("").to_string();
            s.external = v["externalUserAllowed"].as_bool().unwrap_or(false);
            s.uri = v["spaceUri"].as_str().unwrap_or("").to_string();
            s.active = micros(v["lastActiveTime"].as_str().unwrap_or(""));
            if let Some(t) = v["displayName"].as_str().filter(|t| !t.is_empty()) {
                s.title = t.to_string();
                s.named = true;
            }
            if before != (s.kind.clone(), s.title.clone(), s.external, s.uri.clone(), s.active) {
                dirty.insert(name.to_string());
            }
        }
        // conversations the person left (or that were deleted)
        let gone: Vec<String> = self.spaces.keys().filter(|k| !seen.contains(*k)).cloned().collect();
        for g in &gone {
            self.spaces.remove(g);
        }

        let mut by_active: Vec<(i64, String)> = self.spaces.values().map(|s| (s.active, s.name.clone())).collect();
        by_active.sort_by(|a, b| b.cmp(a));
        let due: Vec<String> = by_active
            .iter()
            .enumerate()
            .filter(|(i, (_, n))| {
                let s = &self.spaces[n];
                if s.filled {
                    s.active > s.fetched_to
                } else {
                    *i < BACKFILL
                }
            })
            .map(|(_, (_, n))| n.clone())
            .collect();
        let jobs: Vec<Job> = due.iter().filter_map(|n| self.job(n)).collect();
        let results = download_all(&http, &self.acc, &self.me, &jobs);
        let mut notes = vec![];
        let mut failed: Vec<String> = vec![];
        let mut fetched: HashSet<String> = HashSet::new();
        for r in results {
            match self.check(r) {
                Ok(got) => {
                    dirty.insert(got.name.clone());
                    fetched.insert(got.name.clone());
                    notes.extend(self.apply(got));
                }
                // the others are kept, and that one is tried again next round
                Err(e) => {
                    crate::diag::log(format!("chat: {e}"));
                    failed.push(e);
                }
            }
        }
        if self.pace == Pace::Watching && self.open.as_ref().is_some_and(|o| !due.contains(o)) {
            if let Err(e) = self.refresh_open() {
                crate::diag::log(format!("chat: {e}"));
            }
        }
        self.retry_read_marks(&http);
        let changed: Vec<&Space> = dirty.iter().filter_map(|n| self.spaces.get(n)).collect();
        if let Err(e) = store(self.gen, &changed, &gone) {
            crate::diag::log(format!("chat: database error: {e}"));
        }
        if !dirty.is_empty() || !gone.is_empty() || !self.synced_once {
            self.send(Update::Rows(self.rows()));
        }
        // only a fetched one: what is on screen may hold a sent message not fetched yet
        if let Some(o) = self.open.clone().filter(|o| fetched.contains(o)) {
            if let Some(s) = self.spaces.get(&o) {
                self.send(Update::Messages(o, Arc::new(s.msgs.clone())));
            }
        }
        if !notes.is_empty() {
            self.send(Update::Fresh(notes));
        }
        if !self.synced_once {
            let mine = self.spaces.values().flat_map(|s| &s.msgs).filter(|m| m.sender == self.me).count();
            let all = self.spaces.values().map(|s| s.msgs.len()).sum::<usize>();
            crate::diag::took("chat: first round", t, format!("{} conversations, {} fetched, {all} messages, {mine} of them yours", self.spaces.len(), jobs.len()));
        }
        self.synced_once = true;
        // only when nothing came through is the round a failure (and the pace backs off): one
        // conversation that keeps failing must not slow down all the others
        if !jobs.is_empty() && failed.len() == jobs.len() {
            return Err(failed.swap_remove(0));
        }
        self.round_at = started;
        Ok(())
    }

    /// Puts what came for one conversation into it. Returns the new messages from others, for a
    /// notification (none at the first round: that is history, not news).
    fn apply(&mut self, got: Got) -> Vec<Note> {
        let me = self.me.clone();
        let notify = self.synced_once;
        let since = self.round_at;
        // a read mark Google didn't get yet is not undone by Google's older one
        let held = self.read_retry.get(&got.name).copied().unwrap_or(0);
        let Some(s) = self.spaces.get_mut(&got.name) else { return vec![] };
        let newest = s.msgs.last().map(|m| m.at).unwrap_or(0);
        let first_fill = !s.filled;
        let known: HashSet<String> = s.msgs.iter().map(|m| m.name.clone()).collect();
        let fresh: Vec<Msg> = got.msgs.iter().filter(|m| !known.contains(&m.name)).cloned().collect();
        // news: newer than what was here, or (a conversation seen for the first time) newer than
        // the last good round (none known yet: nothing counts as news)
        let bar = if !first_fill {
            newest
        } else if since == 0 {
            i64::MAX
        } else {
            since
        };
        // read here, on another device, or marked here and not told to Google yet
        let read = got.read.unwrap_or(s.read).max(held);
        let notes = if notify {
            fresh
                .iter()
                .filter(|m| m.sender != me && m.at > bar && m.at > read)
                .map(|m| Note { space: s.name.clone(), title: s.title.clone(), who: m.who.clone(), text: m.text.clone() })
                .collect()
        } else {
            vec![]
        };
        if got.replace {
            s.msgs = got.msgs;
        } else {
            s.msgs.extend(fresh);
            s.msgs.sort_by_key(|m| m.at);
        }
        if s.msgs.len() > KEEP {
            s.msgs.drain(..s.msgs.len() - KEEP);
        }
        s.filled = true;
        s.fetched_to = s.active;
        match got.read {
            Some(r) => s.read = r.max(held),
            // without Google's read mark, the history found at the first fill counts as read
            None if first_fill && s.read == 0 => s.read = s.msgs.last().map(|m| m.at).unwrap_or(0),
            None => {}
        }
        if !s.named {
            let mut people = got.people.clone().unwrap_or_default();
            s.named = !people.is_empty();
            if people.is_empty() {
                // from the messages the others wrote
                for m in s.msgs.iter().rev() {
                    if m.sender != me && !m.who.is_empty() && !people.contains(&m.who) {
                        people.push(m.who.clone());
                    }
                }
            }
            s.title = match people.len() {
                0 => "Rozmowa".into(),
                1..=3 => people.join(", "),
                n => format!("{} i {} innych", people[..2].join(", "), n - 2),
            };
        }
        notes
    }

    /// The open conversation: what is new in it, then its latest page again, which picks up
    /// edits and deletions.
    fn refresh_open(&mut self) -> Result<(), String> {
        let Some(name) = self.open.clone() else { return Ok(()) };
        let Some(s) = self.spaces.get(&name) else { return Ok(()) };
        let behind = !s.filled || s.active > s.fetched_to;
        let http = self.agent()?;
        if behind {
            let job = self.job(&name).ok_or("rozmowa zniknęła")?;
            let got = download(&http, &self.acc, &self.me, &job);
            let got = self.check(got)?;
            let notes = self.apply(got);
            if !notes.is_empty() {
                self.send(Update::Fresh(notes));
            }
        } else {
            let v = call(&http, &self.acc, "GET", &format!("{API}/{name}/messages?pageSize=30&orderBy={}", crate::google::enc("createTime desc")), None);
            let v = self.check(v)?;
            let mut page = parse_page(&v);
            page.reverse();
            let Some(s) = self.spaces.get_mut(&name) else { return Ok(()) };
            let from = page.first().map(|m| m.at).unwrap_or(i64::MIN);
            // only a page that reaches back into what is stored replaces its end (no gap)
            if s.msgs.last().is_some_and(|m| m.at < from) {
                return Ok(());
            }
            let key = |v: &[Msg]| v.iter().map(|m| (m.name.clone(), m.text.clone(), m.edited)).collect::<Vec<_>>();
            let old_tail: Vec<Msg> = s.msgs.iter().filter(|m| m.at >= from).cloned().collect();
            if key(&old_tail) == key(&page) {
                return Ok(());
            }
            s.msgs.retain(|m| m.at < from);
            s.msgs.extend(page);
        }
        let changed: Vec<&Space> = self.spaces.get(&name).into_iter().collect();
        if let Err(e) = store(self.gen, &changed, &[]) {
            crate::diag::log(format!("chat: database error: {e}"));
        }
        if let Some(s) = self.spaces.get(&name) {
            self.send(Update::Messages(name.clone(), Arc::new(s.msgs.clone())));
        }
        self.send(Update::Rows(self.rows()));
        Ok(())
    }

    fn post(&mut self, space: &str, text: &str, id: &str) -> Result<(), String> {
        // what was new before this message, fetched right after it: an answer written by the
        // other person meanwhile must not end up behind the fetch position
        let job = self.job(space);
        let http = self.agent()?;
        let v = call(&http, &self.acc, "POST", &format!("{API}/{space}/messages?requestId={id}"), Some(json!({ "text": text })));
        let v = self.check(v)?;
        // Google has it from here on: nothing below may report it as not sent
        let Some(m) = parse_msg(&v) else {
            crate::diag::log("chat: sent, but Google's answer named no message");
            return Ok(());
        };
        if self.me.is_empty() {
            self.me = m.sender.clone();
        }
        let got = match &job {
            Some(job) => download(&http, &self.acc, &self.me, job).ok().filter(|g| g.msgs.iter().any(|x| x.name == m.name)),
            None => None,
        };
        // shown, though not stored, until a fetch brings it: the bubble written here is gone
        let mut shown = None;
        let notes = match got {
            Some(got) => {
                let notes = self.apply(got);
                if let Some(s) = self.spaces.get_mut(space) {
                    s.active = s.active.max(s.msgs.last().map(|m| m.at).unwrap_or(0));
                    s.fetched_to = s.active;
                }
                notes
            }
            None => {
                // not fetched: it shows up with the others at a round started at once (storing it
                // here would move the fetch position past an answer written meanwhile)
                if let Some(s) = self.spaces.get_mut(space) {
                    s.fetched_to = 0;
                }
                self.round_soon = true;
                shown = Some(m);
                vec![]
            }
        };
        let changed: Vec<&Space> = self.spaces.get(space).into_iter().collect();
        if let Err(e) = store(self.gen, &changed, &[]) {
            crate::diag::log(format!("chat: database error: {e}"));
        }
        if let Some(s) = self.spaces.get(space) {
            let mut msgs = s.msgs.clone();
            msgs.extend(shown);
            self.send(Update::Messages(space.to_string(), Arc::new(msgs)));
        }
        self.send(Update::Rows(self.rows()));
        if !notes.is_empty() {
            self.send(Update::Fresh(notes));
        }
        Ok(())
    }

    /// Read up to `upto` (what the window has shown), never further: a message that came after
    /// stays unread here and in Google Chat.
    fn mark_read(&mut self, space: &str, upto: i64) -> Result<(), String> {
        let Some(s) = self.spaces.get_mut(space) else { return Ok(()) };
        let newest = s.msgs.iter().map(|m| m.at).filter(|&at| at <= upto).max().unwrap_or(0);
        if newest == 0 || s.read >= newest {
            return Ok(());
        }
        s.read = newest;
        let changed: Vec<&Space> = self.spaces.get(space).into_iter().collect();
        if let Err(e) = store(self.gen, &changed, &[]) {
            crate::diag::log(format!("chat: database error: {e}"));
        }
        self.send(Update::Rows(self.rows()));
        if crate::google::has_scope(&self.acc, SCOPE_READSTATE) {
            let http = self.agent()?;
            let r = call(&http, &self.acc, "PATCH", &format!("{API}/users/me/{space}/spaceReadState?updateMask=lastReadTime"), Some(json!({ "lastReadTime": rfc3339(newest) })));
            if let Err(e) = self.check(r) {
                // Google is told at the next round
                self.read_retry.insert(space.to_string(), newest);
                return Err(e);
            }
        }
        Ok(())
    }

    /// Read marks Google didn't get yet.
    fn retry_read_marks(&mut self, http: &ureq::Agent) {
        for (space, held) in std::mem::take(&mut self.read_retry) {
            // never older than what is known by now: an older mark would make newer messages unread
            let read = self.spaces.get(&space).map_or(held, |s| s.read.max(held));
            let r = call(http, &self.acc, "PATCH", &format!("{API}/users/me/{space}/spaceReadState?updateMask=lastReadTime"), Some(json!({ "lastReadTime": rfc3339(read) })));
            if self.check(r).is_err() {
                self.read_retry.insert(space, read);
            }
        }
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows: Vec<Row> = self
            .spaces
            .values()
            .map(|s| {
                let last = s
                    .msgs
                    .last()
                    .map(|m| {
                        let text = if m.text.is_empty() { m.files.first().map(|f| format!("{} {}", crate::theme::ic::ATTACH, f.0)).unwrap_or_default() } else { m.text.lines().next().unwrap_or("").to_string() };
                        let text: String = text.chars().take(120).collect();
                        if m.sender == self.me && !self.me.is_empty() {
                            format!("Ty: {text}")
                        } else if s.kind != "DIRECT_MESSAGE" && !m.who.is_empty() {
                            format!("{}: {text}", m.who.split_whitespace().next().unwrap_or(""))
                        } else {
                            text
                        }
                    })
                    .unwrap_or_default();
                let unread = if s.filled { s.msgs.iter().filter(|m| m.at > s.read && m.sender != self.me).count() } else { 0 };
                Row { name: s.name.clone(), title: if s.title.is_empty() { "Rozmowa".into() } else { s.title.clone() }, kind: s.kind.clone(), external: s.external, active: s.active, last, unread, uri: s.uri.clone() }
            })
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.active));
        rows
    }
}

/// Made-up conversations for screenshots (`--screenshot` with `POCZTA_START=chat`).
pub fn sample() -> (Vec<Row>, Vec<Msg>) {
    let t = now();
    let min = 60_000_000i64;
    let me = "users/me";
    let m = |who: &str, sender: &str, ago: i64, text: &str| Msg { name: format!("m{ago}"), sender: sender.into(), who: who.into(), at: t - ago * min, edited: false, text: text.into(), files: vec![] };
    let mut msgs = vec![
        m("Anna Kowalska", "users/anna", 1500, "Dzień dobry! Czy mogę prosić o wycenę modułu raportów?"),
        m("Aleks Nowicki", me, 1490, "Dzień dobry, jasne — wyślę ją jutro rano."),
        m("Anna Kowalska", "users/anna", 70, "Dziękuję za wycenę. Mamy jeszcze pytanie o termin wdrożenia."),
        m("Anna Kowalska", "users/anna", 69, "Czy start w grudniu jest realny?"),
        m("Aleks Nowicki", me, 40, "Tak, grudzień jest realny, jeśli akceptacja przyjdzie do piątku."),
        m("Anna Kowalska", "users/anna", 3, "Świetnie, potwierdzę do piątku."),
    ];
    msgs[2].files = vec![("wycena-raporty.pdf".into(), String::new())];
    let row = |name: &str, title: &str, kind: &str, external: bool, ago: i64, last: &str, unread: usize| Row { name: name.into(), title: title.into(), kind: kind.into(), external, active: t - ago * min, last: last.into(), unread, uri: String::new() };
    let rows = vec![
        row("spaces/a", "Anna Kowalska", "DIRECT_MESSAGE", true, 3, "Świetnie, potwierdzę do piątku.", 1),
        row("spaces/b", "Piotr Nowak", "DIRECT_MESSAGE", false, 95, "Ty: Wrzuciłem poprawkę na serwer testowy", 0),
        row("spaces/c", "Zespół wdrożeń", "SPACE", false, 260, "Marta: Jutro przegląd o 10:00", 3),
        row("spaces/d", "Ewa Lewandowska, Tomasz Zieliński", "GROUP_CHAT", true, 2900, "Tomasz: Dzięki, wszystko działa", 0),
        row("spaces/e", "Kamil Wójcik", "DIRECT_MESSAGE", true, 9000, &format!("{} umowa-ramowa.docx", crate::theme::ic::ATTACH), 0),
    ];
    (rows, msgs)
}
