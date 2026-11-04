//! The local mail database: one file (%LOCALAPPDATA%\Poczta\mail.redb) with three tables —
//! message details, message texts and everything else (account, calendar, sync position).
//! redb is a small crash-safe database written in Rust. Saving only writes what changed,
//! so it stays quick with tens of thousands of messages offline.

use crate::model::{Message, Store};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

const MESSAGES: TableDefinition<u64, &[u8]> = TableDefinition::new("messages");
const TEXTS: TableDefinition<u64, &str> = TableDefinition::new("texts");
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

/// Reads message texts from the database, from any thread (the texts stay on disk until
/// a message is opened, and the search reads them from here in the background).
#[derive(Clone)]
pub struct TextReader(std::sync::Arc<Database>);

impl TextReader {
    pub fn text(&self, id: u64) -> Option<String> {
        let r = self.0.begin_read().ok()?;
        let t = r.open_table(TEXTS).ok()?;
        let v = t.get(id).ok()??;
        Some(v.value().to_string())
    }
}

pub struct Db {
    db: std::sync::Arc<Database>,
    /// What is already in the file, to write only changes: message id → fingerprint.
    saved_rows: HashMap<u64, u64>,
    saved_texts: HashMap<u64, u64>,
    saved_meta: u64,
}

/// A quick fingerprint maker (8 bytes at a time): fast enough to look through tens of thousands
/// of messages in a few milliseconds. Not for security, only to notice changes.
#[derive(Default)]
struct Quick(u64);

impl Hasher for Quick {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        const K: u64 = 0x517c_c1b7_2722_0a95;
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            let v = u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
            self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(K);
        }
        for b in chunks.remainder() {
            self.0 = (self.0.rotate_left(5) ^ *b as u64).wrapping_mul(K);
        }
    }
}

fn fingerprint(bytes: &[u8]) -> u64 {
    let mut h = Quick::default();
    bytes.hash(&mut h);
    h.finish()
}

/// A quick fingerprint of a message's details (everything except its text), taken without
/// converting the message to JSON, so checking 30 000 messages for changes takes a few
/// milliseconds. Keep it in step with the fields of `Message`.
fn row_key(m: &Message) -> u64 {
    let mut h = Quick::default();
    (m.id, m.folder, &m.from_name, &m.from_addr, &m.to, &m.cc, &m.subject, m.date, m.read, m.flagged).hash(&mut h);
    (&m.attachments, m.sample, &m.html_path, &m.inline, &m.attachment_files, &m.labels, m.uid, &m.body_path, m.partial).hash(&mut h);
    m.snippet.hash(&mut h);
    h.finish()
}

/// A fingerprint of a message text (only texts in memory are looked at, a handful at a time).
fn text_key(body: &str) -> u64 {
    let mut h = Quick::default();
    h.write(body.as_bytes());
    h.write_usize(body.len());
    h.finish()
}

pub fn path() -> std::path::PathBuf {
    // tests use their own folder so they never touch the real mailbox
    if let Ok(d) = std::env::var("POCZTA_DATA_DIR") {
        let _ = std::fs::create_dir_all(&d);
        return std::path::PathBuf::from(d).join("mail.redb");
    }
    let dir = crate::model::cache_dir().parent().map(|p| p.to_path_buf()).unwrap_or_default();
    dir.join("mail.redb")
}

impl Db {
    pub fn open() -> Result<Db, String> {
        let t = std::time::Instant::now();
        // a small cache: the mailbox is read once into the app, and Windows keeps the file cached
        // anyway (the default cache would hold a second copy of the whole mailbox in memory)
        let db = Database::builder().set_cache_size(8 * 1024 * 1024).create(path()).map_err(|e| e.to_string())?;
        crate::diag::took("  database: file opened", t, "");
        // make sure the tables exist; a write (and its wait for the disk) only the very first time
        let ready = db
            .begin_read()
            .ok()
            .is_some_and(|r| r.open_table(MESSAGES).is_ok() && r.open_table(TEXTS).is_ok() && r.open_table(META).is_ok());
        if !ready {
            let w = db.begin_write().map_err(|e| e.to_string())?;
            {
                w.open_table(MESSAGES).map_err(|e| e.to_string())?;
                w.open_table(TEXTS).map_err(|e| e.to_string())?;
                w.open_table(META).map_err(|e| e.to_string())?;
            }
            w.commit().map_err(|e| e.to_string())?;
        }
        Ok(Db { db: std::sync::Arc::new(db), saved_rows: HashMap::new(), saved_texts: HashMap::new(), saved_meta: 0 })
    }

    /// The mailbox as saved last time (None if the database is still empty).
    pub fn load(&mut self) -> Option<Store> {
        let r = self.db.begin_read().ok()?;
        let meta = r.open_table(META).ok()?;
        let store_bytes = meta.get("store").ok()??.value().to_vec();
        let mut store: Store = serde_json::from_slice(&store_bytes).ok()?;
        self.saved_meta = fingerprint(&store_bytes);
        let rows = r.open_table(MESSAGES).ok()?;
        // The messages are read in slices (by number), one slice per processor core; each core
        // reads its slice's details and texts and pairs them up, so nothing waits for anything.
        let t0 = std::time::Instant::now();
        let first = rows.first().ok().flatten().map(|(k, _)| k.value());
        let last = rows.last().ok().flatten().map(|(k, _)| k.value());
        drop(rows);
        let mut messages = Vec::new();
        if let (Some(first), Some(last)) = (first, last) {
            let cores = std::thread::available_parallelism().map(|x| x.get()).unwrap_or(4).clamp(1, 8) as u64;
            let span = (last - first) / cores + 1;
            let db = &self.db;
            type Slice = Vec<(u64, Message, u64, u64)>;
            let slices: Vec<Slice> = std::thread::scope(|sc| {
                let handles: Vec<_> = (0..cores)
                    .map(|c| {
                        let lo = first + c * span;
                        let hi = lo.saturating_add(span);
                        sc.spawn(move || -> Slice {
                            let Ok(r) = db.begin_read() else { return vec![] };
                            let (Ok(rows), Ok(texts)) = (r.open_table(MESSAGES), r.open_table(TEXTS)) else { return vec![] };
                            let mut out = Vec::new();
                            if let Ok(it) = rows.range(lo..hi) {
                                for item in it.flatten() {
                                    let id = item.0.value();
                                    let Ok(mut m) = serde_json::from_slice::<Message>(item.1.value()) else { continue };
                                    // the text stays in the database; only messages saved before
                                    // excerpts existed have theirs made now (once)
                                    let mut fresh = false;
                                    if m.snippet.is_empty() {
                                        if let Ok(Some(t)) = texts.get(id) {
                                            m.snippet = crate::model::snippet_of(t.value());
                                            // (a text with nothing to show gets a single space, so
                                            // it isn't looked at again on every start)
                                            if m.snippet.is_empty() {
                                                m.snippet = " ".into();
                                            }
                                            fresh = true;
                                        }
                                    }
                                    m.on_disk = true;
                                    let rk = if fresh { 0 } else { row_key(&m) };
                                    out.push((id, m, rk, 0));
                                }
                            }
                            out
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap_or_default()).collect()
            });
            crate::diag::took("  database: messages read on all cores", t0, "");
            crate::diag::mem("messages read (in slices)");
            let n: usize = slices.iter().map(|x| x.len()).sum();
            messages.reserve(n);
            self.saved_rows.reserve(n);
            self.saved_texts.reserve(n);
            let mut fresh: Vec<usize> = Vec::new();
            for slice in slices {
                for (id, m, rk, _) in slice {
                    if rk == 0 {
                        fresh.push(messages.len());
                    }
                    self.saved_rows.insert(id, rk);
                    messages.push(m);
                }
            }
            // excerpts made just now: written back once, here in the background
            if !fresh.is_empty() {
                let t = std::time::Instant::now();
                if let Ok(w) = self.db.begin_write() {
                    if let Ok(mut rows) = w.open_table(MESSAGES) {
                        for &i in &fresh {
                            let m: &Message = &messages[i];
                            if let Ok(row) = serde_json::to_vec(m) {
                                if rows.insert(m.id, row.as_slice()).is_ok() {
                                    self.saved_rows.insert(m.id, row_key(m));
                                }
                            }
                        }
                    }
                    let _ = w.commit();
                }
                crate::diag::took("  database: excerpts made for the message list", t, format!("{}", fresh.len()));
            }
        }
        store.messages = messages;
        Some(store)
    }

    /// Writes what changed since the last save, in one transaction.
    /// `messages_may_have_changed`: false when the app knows nothing touched the messages since
    /// the last save (then only the account / calendar part is checked).
    /// Lets a message's text go from memory if it is safely in the database.
    pub fn release(&self, m: &mut Message) {
        if !m.on_disk && !m.body.is_empty() && self.saved_texts.get(&m.id) == Some(&text_key(&m.body)) {
            m.body = String::new();
            m.on_disk = true;
        }
    }

    /// A reader of message texts for other threads.
    pub fn reader(&self) -> TextReader {
        TextReader(self.db.clone())
    }

    /// The text of one message (read when it is opened).
    pub fn text(&mut self, id: u64) -> Option<String> {
        let t = TextReader(self.db.clone()).text(id)?;
        self.saved_texts.insert(id, text_key(&t));
        Some(t)
    }

    /// `keep`: messages whose text stays in memory after saving (the one being read).
    pub fn save(&mut self, store: &mut Store, messages_may_have_changed: bool, keep: Option<u64>) -> Result<(), String> {
        // First find what changed, without touching the file: if nothing did (the usual case
        // for the regular save), there is no writing and no waiting for the disk at all.
        let mut rows_changed: Vec<(usize, u64)> = Vec::new();
        let mut texts_changed: Vec<(usize, u64)> = Vec::new();
        let mut new_ids = 0usize;
        let look = if messages_may_have_changed { store.messages.len() } else { 0 };
        for (i, m) in store.messages.iter_mut().enumerate().take(look) {
            // texts in memory (new mail, or opened): written if changed, with a new excerpt
            let mut tf_new = None;
            if !m.on_disk {
                let tf = text_key(&m.body);
                if self.saved_texts.get(&m.id) != Some(&tf) {
                    let mut snip = crate::model::snippet_of(&m.body);
                    if snip.is_empty() {
                        snip = " ".into();
                    }
                    if m.snippet != snip {
                        m.snippet = snip;
                    }
                    tf_new = Some(tf);
                }
            }
            if let Some(tf) = tf_new {
                texts_changed.push((i, tf));
            }
            let f = row_key(m);
            match self.saved_rows.get(&m.id) {
                Some(old) if *old == f => {}
                old => {
                    if old.is_none() {
                        new_ids += 1;
                    }
                    rows_changed.push((i, f));
                }
            }
        }
        // messages gone since the last save (only looked for when the numbers don't add up)
        let gone: Vec<u64> = if messages_may_have_changed && self.saved_rows.len() + new_ids != store.messages.len() {
            let present: std::collections::HashSet<u64> = store.messages.iter().map(|m| m.id).collect();
            self.saved_rows.keys().filter(|id| !present.contains(id)).copied().collect()
        } else {
            vec![]
        };
        // everything else: account, calendar, sync position (without the messages)
        let msgs = std::mem::take(&mut store.messages);
        let bytes = serde_json::to_vec(&*store).map_err(|e| e.to_string());
        store.messages = msgs;
        let bytes = bytes?;
        let meta_f = fingerprint(&bytes);
        if rows_changed.is_empty() && texts_changed.is_empty() && gone.is_empty() && meta_f == self.saved_meta {
            for m in store.messages.iter_mut().take(look) {
                if !m.on_disk && !m.body.is_empty() && Some(m.id) != keep && self.saved_texts.get(&m.id) == Some(&text_key(&m.body)) {
                    m.body = String::new();
                    m.on_disk = true;
                }
            }
            return Ok(());
        }
        let w = self.db.begin_write().map_err(|e| e.to_string())?;
        {
            let mut rows = w.open_table(MESSAGES).map_err(|e| e.to_string())?;
            let mut texts = w.open_table(TEXTS).map_err(|e| e.to_string())?;
            let mut meta = w.open_table(META).map_err(|e| e.to_string())?;
            for (i, f) in &rows_changed {
                let m = &mut store.messages[*i];
                // the text goes to its own table, the rest of the message to another
                let body = std::mem::take(&mut m.body);
                let row = serde_json::to_vec(&*m).map_err(|e| e.to_string());
                m.body = body;
                let row = row?;
                rows.insert(m.id, row.as_slice()).map_err(|e| e.to_string())?;
                self.saved_rows.insert(m.id, *f);
            }
            for (i, tf) in &texts_changed {
                let m = &store.messages[*i];
                texts.insert(m.id, m.body.as_str()).map_err(|e| e.to_string())?;
                self.saved_texts.insert(m.id, *tf);
            }
            for id in &gone {
                rows.remove(*id).map_err(|e| e.to_string())?;
                texts.remove(*id).map_err(|e| e.to_string())?;
                self.saved_rows.remove(id);
                self.saved_texts.remove(id);
            }
            if !rows_changed.is_empty() || !texts_changed.is_empty() || !gone.is_empty() {
                crate::diag::log(format!(
                    "  database: {} message details, {} texts written, {} removed",
                    rows_changed.len(),
                    texts_changed.len(),
                    gone.len()
                ));
            }
            if meta_f != self.saved_meta {
                meta.insert("store", bytes.as_slice()).map_err(|e| e.to_string())?;
                self.saved_meta = meta_f;
            }
        }
        w.commit().map_err(|e| e.to_string())?;
        // texts now safely in the database go back to disk (only the open one stays)
        for m in store.messages.iter_mut() {
            if !m.on_disk && !m.body.is_empty() && Some(m.id) != keep && self.saved_texts.get(&m.id) == Some(&text_key(&m.body)) {
                m.body = String::new();
                m.on_disk = true;
            }
        }
        Ok(())
    }
}
