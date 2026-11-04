//! Keeps search ready in the background: the word index, the meaning model and the
//! meaning of every message. Search results mix both: exact words first, then messages
//! that mean the same thing in other words.

use crate::model::{Folder, Message};
use crate::search;
use crate::semantic::{self, Model, Progress};
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

#[derive(Clone, PartialEq)]
pub enum MeaningState {
    Off,
    Downloading(f32),
    Loading,
    Ready,
    Failed(String),
}

pub struct Finder {
    enabled: bool,
    index: Option<Arc<search::Index>>,
    index_sig: u64,
    building: bool,
    index_tx: Sender<(u64, Arc<search::Index>)>,
    index_rx: Receiver<(u64, Arc<search::Index>)>,
    pub meaning: MeaningState,
    model: Option<Arc<Model>>,
    dl_tx: Sender<Progress>,
    dl_rx: Receiver<Progress>,
    load_tx: Sender<Result<Arc<Model>, String>>,
    load_rx: Receiver<Result<Arc<Model>, String>>,
    /// the meanings, shared with the searching thread (copied only if it is changed while a
    /// search is still reading it, which is rare)
    vectors: Arc<HashMap<u64, Vec<i8>>>,
    vectors_dirty: bool,
    /// the meanings being written to disk on another thread (the window doesn't wait for it)
    saving: Option<std::thread::JoinHandle<()>>,
    embedding: bool,
    vec_tx: Sender<Vec<(u64, Vec<i8>)>>,
    vec_rx: Receiver<Vec<(u64, Vec<i8>)>>,
    key: (String, u64, usize),
    hits: Option<Vec<u64>>,
    /// how many messages still wait for their meaning
    pub to_analyse: usize,
    /// results found only because they mean something similar (not by the words)
    pub by_meaning: std::collections::HashSet<u64>,
    last_build: std::time::Instant,
    /// (mailbox fingerprint, analysed count) when the messages still to analyse were last counted
    todo_key: (u64, usize),
    /// reads message texts from the database (they stay on disk)
    reader: Option<crate::db::TextReader>,
    /// searches run on another thread; their answers come back here
    found_tx: Sender<Found>,
    found_rx: Receiver<Found>,
    /// the meanings as one list (made again only when new ones were added) and the messages in
    /// the bin (made again only when the mailbox changed), handed to the searching thread

    trash: Option<(u64, Arc<std::collections::HashSet<u64>>)>,
}

/// An answer from the searching thread: (search, results best first, found by meaning only).
type Found = ((String, u64, usize), Vec<u64>, std::collections::HashSet<u64>);

impl Finder {
    pub fn new(enabled: bool) -> Self {
        let (index_tx, index_rx) = channel();
        let (dl_tx, dl_rx) = channel();
        let (load_tx, load_rx) = channel();
        let (vec_tx, vec_rx) = channel();
        let (found_tx, found_rx) = channel();
        Self {
            enabled,
            index: None,
            index_sig: 0,
            building: false,
            index_tx,
            index_rx,
            meaning: MeaningState::Off,
            model: None,
            dl_tx,
            dl_rx,
            load_tx,
            load_rx,
            vectors: Arc::new(HashMap::new()),
            vectors_dirty: false,
            saving: None,
            embedding: false,
            vec_tx,
            vec_rx,
            key: (String::new(), 0, 0),
            hits: None,
            to_analyse: 0,
            by_meaning: Default::default(),
            last_build: std::time::Instant::now(),
            todo_key: (0, usize::MAX),
            reader: None,
            found_tx,
            found_rx,

            trash: None,
        }
    }

    /// Starts the word index from texts already copied elsewhere (while the mailbox was being
    /// read), so the app doesn't have to copy them again. `sig`: the mailbox they came from.
    pub fn prebuild(&mut self, entries: Vec<search::Entry>, sig: u64, ctx: &eframe::egui::Context) {
        if !self.enabled || self.building {
            return;
        }
        self.building = true;
        self.last_build = std::time::Instant::now();
        let (tx, ctx, reader) = (self.index_tx.clone(), ctx.clone(), self.reader.clone());
        std::thread::spawn(move || {
            let t = std::time::Instant::now();
            let n = entries.len();
            let idx = Arc::new(search::Index::build(entries, reader));
            crate::diag::took("search index built", t, format!("{n} messages"));
            crate::diag::give_back_memory();
            crate::diag::mem("with the word index");
            let _ = tx.send((sig, idx));
            ctx.request_repaint();
        });
    }

    pub fn set_reader(&mut self, r: crate::db::TextReader) {
        self.reader = Some(r);
    }

    /// The word index is ready (searches then answer within milliseconds).
    pub fn has_index(&self) -> bool {
        self.index.is_some()
    }

    /// Results for the current search, best first (None until the index is ready).
    pub fn hits(&self) -> Option<&Vec<u64>> {
        self.hits.as_ref()
    }

    /// Try the meaning model again after a failed download.
    pub fn retry(&mut self) {
        if matches!(self.meaning, MeaningState::Failed(_)) {
            self.meaning = MeaningState::Off;
        }
    }

    pub fn save(&mut self, msgs: &[Message]) {
        if !self.vectors_dirty {
            return;
        }
        let ids: std::collections::HashSet<u64> = msgs.iter().map(|m| m.id).collect();
        // (only when some belong to messages that are gone: no copy otherwise)
        if self.vectors.keys().any(|id| !ids.contains(id)) {
            Arc::make_mut(&mut self.vectors).retain(|id, _| ids.contains(id));
        }
        // one write at a time (they share the same file); the previous one ended long ago
        self.wait_saved();
        let v = self.vectors.clone();
        self.saving = Some(std::thread::spawn(move || semantic::save_vectors(&v)));
        self.vectors_dirty = false;
    }

    /// Waits for the meanings being written (before the app ends).
    pub fn wait_saved(&mut self) {
        if let Some(h) = self.saving.take() {
            let _ = h.join();
        }
    }

    /// Called every frame: keeps everything up to date and answers the search.
    /// `sig`: the app's fingerprint of the mailbox (changes whenever messages change).
    pub fn tick(&mut self, msgs: &[Message], sig: u64, query: &str, allow_download: bool, ctx: &eframe::egui::Context) {
        // answers from the background
        while let Ok((key, hits, by_meaning)) = self.found_rx.try_recv() {
            // only the answer to what is being searched now (older ones are dropped)
            if key == self.key {
                if std::env::var("POCZTA_PROFILE").is_ok() {
                    crate::diag::log(format!("search {:?}: {} results ({} by meaning)", key.0, hits.len(), by_meaning.len()));
                }
                self.hits = Some(hits);
                self.by_meaning = by_meaning;
                crate::diag::give_back_memory();
            }
        }
        while let Ok((sig, idx)) = self.index_rx.try_recv() {
            self.index = Some(idx);
            self.index_sig = sig;
            self.building = false;
        }
        while let Ok(p) = self.dl_rx.try_recv() {
            match p {
                Progress::Downloading(f) => self.meaning = MeaningState::Downloading(f),
                Progress::Done => self.start_loading(ctx),
                Progress::Failed(e) => self.meaning = MeaningState::Failed(e),
            }
        }
        while let Ok(r) = self.load_rx.try_recv() {
            match r {
                Ok(m) => {
                    self.vectors = Arc::new(semantic::load_vectors(m.dim));
                    self.model = Some(m);
                    self.meaning = MeaningState::Ready;
                }
                Err(e) => self.meaning = MeaningState::Failed(e),
            }
        }
        while let Ok(batch) = self.vec_rx.try_recv() {
            for (id, v) in batch {
                Arc::make_mut(&mut self.vectors).insert(id, v);
            }
            self.embedding = false;
            self.vectors_dirty = true;
        }
        if !self.enabled {
            return;
        }

        // word index: rebuilt in the background whenever the messages changed
        // while lots of mail is arriving, rebuild at most once a minute unless someone searches
        let due = self.index.is_none() || !query.trim().is_empty() || self.last_build.elapsed().as_secs() >= 300;
        if !self.building && self.index_sig != sig && due || self.index.is_none() && !self.building {
            self.building = true;
            self.last_build = std::time::Instant::now();
            let t = std::time::Instant::now();
            let entries = entries(msgs);
            crate::diag::took("search index: texts copied", t, "");
            let tx = self.index_tx.clone();
            let ctx = ctx.clone();
            let reader = self.reader.clone();
            std::thread::spawn(move || {
                crate::diag::background_thread();
                let t = std::time::Instant::now();
                let n = entries.len();
                let idx = Arc::new(search::Index::build(entries, reader));
                crate::diag::took("search index built", t, format!("{n} messages"));
                let _ = tx.send((sig, idx));
                ctx.request_repaint();
            });
        }

        // meaning model: load it, or download it once
        if self.model.is_none() && self.meaning == MeaningState::Off {
            if semantic::is_downloaded() {
                self.start_loading(ctx);
            } else if allow_download {
                self.meaning = MeaningState::Downloading(0.0);
                semantic::download(self.dl_tx.clone(), ctx.clone());
            }
        }

        // the meaning of messages not analysed yet, a batch at a time in the background
        if let (Some(model), false) = (&self.model, self.embedding) {
            // looked through only when the mailbox or the analysed messages changed
            let key = (sig, self.vectors.len());
            if self.todo_key == key && self.to_analyse == 0 {
                // nothing new to analyse
            } else {
            self.todo_key = key;
            let todo: Vec<&Message> = msgs.iter().filter(|m| !self.vectors.contains_key(&m.id)).collect();
            self.to_analyse = todo.len();
            if !todo.is_empty() {
                self.embedding = true;
                // a batch per free processor core (one core stays free for the window)
                // half the processor at most, at low priority: the window and the email view come first
                let cores = (std::thread::available_parallelism().map(|x| x.get()).unwrap_or(2) / 2).clamp(1, 4);
                let batch: Vec<(u64, String, bool)> = todo
                    .iter()
                    .take(400 * cores)
                    .map(|m| {
                        // the start of a long text is enough to know what it is about
                        let mut end = m.body.len().min(16_000);
                        while !m.body.is_char_boundary(end) {
                            end -= 1;
                        }
                        (m.id, format!("{}\n{}\n{}", m.subject, m.from_name, &m.body[..end]), m.on_disk)
                    })
                    .collect();
                let reader = self.reader.clone();
                let model = model.clone();
                let tx = self.vec_tx.clone();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let t0 = std::time::Instant::now();
                    let chunk = batch.len().div_ceil(cores).max(1);
                    let out: Vec<(u64, Vec<i8>)> = std::thread::scope(|sc| {
                        let parts: Vec<_> = batch
                            .chunks(chunk)
                            .map(|c| {
                                let model = &model;
                                let reader = reader.clone();
                                sc.spawn(move || {
                                    crate::diag::background_thread();
                                    c.iter()
                                        .map(|(id, t, disk)| {
                                            // a text on disk is read here, for this message only
                                            let text = match (disk, reader.as_ref()) {
                                                (true, Some(r)) => {
                                                    let body = r.text(*id).unwrap_or_default();
                                                    let mut end = body.len().min(16_000);
                                                    while !body.is_char_boundary(end) {
                                                        end -= 1;
                                                    }
                                                    format!("{t}{}", &body[..end])
                                                }
                                                _ => t.clone(),
                                            };
                                            (*id, semantic::pack(&model.embed(&text)))
                                        })
                                        .collect::<Vec<_>>()
                                })
                            })
                            .collect();
                        parts.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
                    });
                    crate::diag::took("meaning of messages", t0, format!("{}", out.len()));
                    let _ = tx.send(out);
                    ctx.request_repaint();
                });
            }
            }
        }

        // the search itself
        let q = query.trim();
        let key = (q.to_string(), self.index_sig, self.vectors.len());
        if key == self.key {
            return;
        }
        self.key = key.clone();
        if q.is_empty() {
            self.hits = None;
            self.by_meaning.clear();
            return;
        }
        let Some(idx) = self.index.clone() else {
            self.hits = None;
            return;
        };
        // what the searching thread needs, made again only when it changed
        if self.trash.as_ref().map(|t| t.0) != Some(sig) {
            self.trash = Some((sig, Arc::new(msgs.iter().filter(|m| m.folder == Folder::Trash).map(|m| m.id).collect())));
        }
        let trash = self.trash.as_ref().map(|t| t.1.clone()).unwrap_or_default();
        let model = self.model.clone();
        // the searching thread reads the same meanings (no copy)
        let vectors = model.is_some().then(|| self.vectors.clone());
        let (tx, ctx, q) = (self.found_tx.clone(), ctx.clone(), q.to_string());
        // the search runs on another thread, so typing never waits for it; the results shown so
        // far stay until the new ones come
        std::thread::spawn(move || {
            let mut score: HashMap<u64, f32> = HashMap::new();
            let lex = idx.search(&q, 400);
            let top_lex = lex.first().map(|x| x.1).unwrap_or(1.0).max(1e-6);
            for (id, s) in &lex {
                score.insert(*id, 0.7 * s / top_lex);
            }
            let mut by_meaning = std::collections::HashSet::new();
            if let (Some(model), Some(vectors)) = (model, vectors) {
                let qv = model.embed(&q);
                let mut sims: Vec<(u64, f32)> = vectors.iter().map(|(id, v)| (*id, semantic::similarity(&qv, v))).collect();
                sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                let top = sims.first().map(|x| x.1).unwrap_or(0.0);
                // only messages that really mean something close to the search
                let threshold = (top * 0.65).max(0.28);
                for (id, s) in sims.into_iter().take(200).filter(|x| x.1 >= threshold) {
                    if !score.contains_key(&id) {
                        by_meaning.insert(id);
                    }
                    *score.entry(id).or_default() += 0.45 * s / top.max(1e-6);
                }
            }
            let mut ranked: Vec<(u64, f32)> = score.into_iter().filter(|(id, _)| !trash.contains(id)).collect();
            ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let _ = tx.send((key, ranked.into_iter().map(|x| x.0).take(300).collect(), by_meaning));
            ctx.request_repaint();
        });
    }

    fn start_loading(&mut self, ctx: &eframe::egui::Context) {
        self.meaning = MeaningState::Loading;
        let tx = self.load_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(Model::load).unwrap_or_else(|_| Err("nieoczekiwany błąd".into()));
            crate::diag::mem("with the meaning model");
            let _ = tx.send(r.map(Arc::new));
            ctx.request_repaint();
        });
    }

    /// One short line about what is being prepared, if anything.
    pub fn status(&self) -> Option<String> {
        match &self.meaning {
            MeaningState::Downloading(p) => Some(format!("Wyszukiwanie po znaczeniu: pobieranie modelu {}%", (p * 100.0) as i32)),
            MeaningState::Loading => Some("Wyszukiwanie po znaczeniu: przygotowanie…".into()),
            MeaningState::Ready if self.to_analyse > 0 => {
                Some(format!("Wyszukiwanie po znaczeniu: analiza wiadomości (zostało {})", self.to_analyse))
            }
            MeaningState::Failed(e) => Some(format!("Wyszukiwanie po znaczeniu niedostępne: {e}")),
            _ => None,
        }
    }

    pub fn meaning_ready(&self) -> bool {
        self.model.is_some()
    }
}

/// What each message offers to the word index (copied on several cores at once).
pub fn entries(msgs: &[Message]) -> Vec<search::Entry> {
    let one = |m: &Message| search::Entry {
        id: m.id,
        subject: m.subject.clone(),
        // sender, recipients and tags count as much as the sender's name
        from: format!(
            "{} {} {} {}",
            m.from_name,
            m.from_addr,
            m.to,
            m.labels.iter().map(|l| crate::theme::strip_emoji(&l.replace('/', " "))).collect::<Vec<_>>().join(" ")
        ),
        body: if m.on_disk { String::new() } else { m.body.clone() },
        from_db: m.on_disk,
    };
    let cores = std::thread::available_parallelism().map(|x| x.get()).unwrap_or(4).clamp(1, 8);
    let chunk = msgs.len().div_ceil(cores).max(1);
    std::thread::scope(|sc| {
        let parts: Vec<_> = msgs.chunks(chunk).map(|c| sc.spawn(move || c.iter().map(one).collect::<Vec<_>>())).collect();
        parts.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    })
}
