//! Fast word search over all messages kept on this computer.
//!
//! Words are compared without Polish accents and with some room for word endings, so
//! "faktura" also finds "fakturę" and "faktury". The subject counts most, then the sender,
//! then the text. Results come best match first.

use std::collections::{BTreeMap, HashMap};

/// Lower case, Polish letters without accents, everything that isn't a letter or digit
/// becomes a space.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(|c| c.to_lowercase()) {
        let c = match c {
            'ą' => 'a',
            'ć' => 'c',
            'ę' => 'e',
            'ł' => 'l',
            'ń' => 'n',
            'ó' => 'o',
            'ś' => 's',
            'ź' | 'ż' => 'z',
            c if c.is_alphanumeric() => c,
            _ => ' ',
        };
        out.push(c);
    }
    out
}

/// Short everyday words that say nothing about what a message is about.
const STOP: &[&str] = &[
    "do", "na", "za", "od", "po", "ze", "to", "sie", "jest", "dla", "nie", "czy", "jak", "ale", "tak", "lub", "oraz", "pod",
    "nad", "bez", "przy", "przez", "ten", "ta", "te", "tym", "tego", "jej", "jego", "ich", "co", "ktory", "ktora", "ktore",
    "juz", "tez", "the", "and", "for", "of", "to", "in", "on", "is", "re", "fwd", "pl", "com", "www", "http", "https",
];

pub fn words(s: &str) -> Vec<String> {
    normalize(s)
        .split_whitespace()
        .filter(|w| {
            let n = w.chars().count();
            // codes, links and long jumbles of letters and digits (tracking numbers in
            // newsletters and the like) are never searched for and only fill the index
            let jumble = n > 12 && w.chars().any(|c| c.is_ascii_digit()) && w.chars().any(|c| c.is_alphabetic());
            (n >= 3 || (n >= 1 && w.chars().all(|c| c.is_ascii_digit()))) && n <= 32 && !jumble && !STOP.contains(w)
        })
        .map(String::from)
        .collect()
}

/// The part of a search word that has to match the start of a word in a message:
/// long words may end differently ("faktura" → "faktu…").
fn stem(w: &str) -> String {
    let n = w.chars().count();
    let keep = if n <= 4 { n } else { (n - 2).max(4) };
    w.chars().take(keep).collect()
}

struct Doc {
    id: u64,
    len: f32,
}

pub struct Index {
    /// word → (where its places start in `places`, how many messages have it)
    terms: BTreeMap<Box<str>, (u32, u32)>,
    /// for every word, the messages with it, packed: the step from the previous message
    /// number (1-5 bytes) and how often the word appears there (1 byte)
    places: Vec<u8>,
    docs: Vec<Doc>,
    avg_len: f32,
}

fn put_number(out: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn take_number(b: &[u8], at: &mut usize) -> u32 {
    let mut v = 0u32;
    let mut shift = 0;
    loop {
        let x = b[*at];
        *at += 1;
        v |= ((x & 0x7f) as u32) << shift;
        if x < 0x80 {
            return v;
        }
        shift += 7;
    }
}

/// What a message offers to the index.
pub struct Entry {
    pub id: u64,
    pub subject: String,
    pub from: String,
    pub body: String,
    /// the text is on disk: read it from there while indexing (one at a time)
    pub from_db: bool,
}

impl Index {
    /// Builds the index on all processor cores: each core takes a share of the messages,
    /// and the shares are put together at the end (in order, so results stay the same).
    pub fn build(entries: Vec<Entry>, reader: Option<crate::db::TextReader>) -> Index {
        let n = entries.len();
        let cores = std::thread::available_parallelism().map(|x| x.get()).unwrap_or(4).clamp(1, 8);
        let chunk = n.div_ceil(cores).max(1);
        type Part = (BTreeMap<String, Vec<(u32, f32)>>, Vec<Doc>, f32);
        let parts: Vec<Part> = std::thread::scope(|sc| {
            let handles: Vec<_> = entries
                .chunks(chunk)
                .enumerate()
                .map(|(ci, share)| {
                    let reader = reader.clone();
                    sc.spawn(move || {
                        // (low priority: the window comes first)
                        crate::diag::background_thread();
                        let mut terms: BTreeMap<String, Vec<(u32, f32)>> = BTreeMap::new();
                        let mut docs = Vec::with_capacity(share.len());
                        let mut total = 0.0;
                        let mut tf: HashMap<String, f32> = HashMap::new();
                        for (j, e) in share.iter().enumerate() {
                            let i = (ci * chunk + j) as u32;
                            tf.clear();
                            let mut len = 0.0;
                            // a text on disk is read just for this message, then let go
                            let disk = if e.from_db { reader.as_ref().and_then(|r| r.text(e.id)) } else { None };
                            let body = disk.as_deref().unwrap_or(e.body.as_str());
                            for (text, weight) in [(e.subject.as_str(), 3.0f32), (e.from.as_str(), 2.0), (body, 1.0)] {
                                // very long texts: the beginning says what the message is about
                                let mut end = text.len().min(40_000);
                                while !text.is_char_boundary(end) {
                                    end -= 1;
                                }
                                for w in words(&text[..end]) {
                                    *tf.entry(w).or_default() += weight;
                                    len += 1.0;
                                }
                            }
                            for (t, w) in tf.drain() {
                                terms.entry(t).or_default().push((i, w));
                            }
                            total += len;
                            docs.push(Doc { id: e.id, len });
                        }
                        (terms, docs, total)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap_or_default()).collect()
        });
        let mut terms: BTreeMap<String, Vec<(u32, f32)>> = BTreeMap::new();
        let mut docs = Vec::with_capacity(n);
        let mut total = 0.0;
        for (t, d, tot) in parts {
            if terms.is_empty() {
                terms = t;
            } else {
                for (k, v) in t {
                    terms.entry(k).or_default().extend(v);
                }
            }
            docs.extend(d);
            total += tot;
        }
        // packed: each word's list goes into one block of bytes (and its own list is let go)
        let mut places = Vec::with_capacity(terms.values().map(|v| v.len() * 3).sum());
        let mut packed: BTreeMap<Box<str>, (u32, u32)> = BTreeMap::new();
        for (k, mut v) in terms {
            v.sort_unstable_by_key(|x| x.0);
            let start = places.len() as u32;
            let mut prev = 0u32;
            for (d, w) in &v {
                put_number(&mut places, d - prev);
                places.push(w.round().clamp(1.0, 255.0) as u8);
                prev = *d;
            }
            packed.insert(k.into_boxed_str(), (start, v.len() as u32));
        }
        places.shrink_to_fit();
        let avg_len = if docs.is_empty() { 1.0 } else { (total / docs.len() as f32).max(1.0) };
        Index { terms: packed, places, docs, avg_len }
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    /// Messages matching the query, best first: (message id, score).
    pub fn search(&self, query: &str, limit: usize) -> Vec<(u64, f32)> {
        let q = words(query);
        if q.is_empty() || self.docs.is_empty() {
            return vec![];
        }
        let n = self.docs.len() as f32;
        let (k1, b) = (1.2f32, 0.6f32);
        let mut score: HashMap<u32, f32> = HashMap::new();
        let mut matched: HashMap<u32, u32> = HashMap::new();
        for (qi, word) in q.iter().enumerate() {
            let st = stem(word);
            let mut hit_here: HashMap<u32, f32> = HashMap::new();
            let from: Box<str> = st.clone().into_boxed_str();
            for (term, &(start, count)) in self.terms.range(from..) {
                if !term.starts_with(st.as_str()) {
                    break;
                }
                let exact = &**term == word.as_str();
                let df = count as f32;
                let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
                let boost = if exact { 1.0 } else if term.starts_with(word.as_str()) { 0.85 } else { 0.7 };
                let mut at = start as usize;
                let mut d = 0u32;
                for _ in 0..count {
                    d += take_number(&self.places, &mut at);
                    let w = self.places[at] as f32;
                    at += 1;
                    let len = self.docs[d as usize].len;
                    let s = idf * (w * (k1 + 1.0)) / (w + k1 * (1.0 - b + b * len / self.avg_len)) * boost;
                    let e = hit_here.entry(d).or_default();
                    *e = e.max(s);
                }
            }
            for (d, s) in hit_here {
                *score.entry(d).or_default() += s;
                let m = matched.entry(d).or_default();
                *m |= 1 << qi.min(31);
            }
        }
        // Precise first: messages with every search word; if none has them all, the ones
        // with the most.
        let want = q.len().min(32) as u32;
        let best = matched.values().map(|m| m.count_ones()).max().unwrap_or(0);
        let need = if best >= want { want } else { best };
        let mut out: Vec<(u64, f32)> = score
            .into_iter()
            .filter(|(d, _)| matched.get(d).map(|m| m.count_ones() >= need).unwrap_or(false))
            .map(|(d, s)| (self.docs[d as usize].id, s))
            .collect();
        out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        out.truncate(limit);
        out
    }
}
