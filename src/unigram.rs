//! A compact word-piece splitter for the meaning model (the "Unigram" kind): the text is
//! cleaned up the same way the model expects, then cut into the pieces of its list that give
//! the best total score. All 500 000 pieces live in one block of memory (about 10 MB) with a
//! small lookup table, instead of half a million separate texts.

use std::path::Path;
use unicode_normalization::UnicodeNormalization;

/// Pieces longer than this many letters are not looked for (they almost never occur in mail).
const MAX_CHARS: usize = 32;
const MAGIC: &[u8; 8] = b"PZUNI001";

pub struct Unigram {
    /// all pieces one after another; piece i is buf[offs[i]..offs[i + 1]]
    buf: Vec<u8>,
    offs: Vec<u32>,
    scores: Vec<f32>,
    /// open lookup table: piece number + 1 (0 = empty slot)
    table: Vec<u32>,
    mask: usize,
    unk: u32,
    unk_score: f32,
    /// which beginnings of pieces exist (by fingerprint): longer pieces are not looked for once
    /// no piece starts like that
    prefixes: Vec<u64>,
}

const PREFIX_BITS: usize = 1 << 24;

fn hash(b: &[u8]) -> u64 {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let mut h = 0u64;
    let mut chunks = b.chunks_exact(8);
    for c in &mut chunks {
        let v = u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
        h = (h.rotate_left(5) ^ v).wrapping_mul(K);
    }
    for x in chunks.remainder() {
        h = (h.rotate_left(5) ^ *x as u64).wrapping_mul(K);
    }
    h ^ (h >> 29)
}

impl Unigram {
    fn build(buf: Vec<u8>, offs: Vec<u32>, scores: Vec<f32>, unk: u32) -> Unigram {
        let n = scores.len();
        let size = (n * 2).next_power_of_two().max(16);
        let mask = size - 1;
        let mut table = vec![0u32; size];
        for i in 0..n {
            let piece = &buf[offs[i] as usize..offs[i + 1] as usize];
            let mut slot = hash(piece) as usize & mask;
            while table[slot] != 0 {
                slot = (slot + 1) & mask;
            }
            table[slot] = i as u32 + 1;
        }
        let mut prefixes = vec![0u64; PREFIX_BITS / 64];
        for i in 0..n {
            let piece = &buf[offs[i] as usize..offs[i + 1] as usize];
            if let Ok(p) = std::str::from_utf8(piece) {
                for (k, c) in p.char_indices() {
                    let h = hash(&piece[..k + c.len_utf8()]) as usize & (PREFIX_BITS - 1);
                    prefixes[h / 64] |= 1 << (h % 64);
                }
            }
        }
        let min = scores.iter().copied().fold(f32::INFINITY, f32::min);
        Unigram { buf, offs, scores, table, mask, unk, unk_score: min - 10.0, prefixes }
    }

    /// From the model's tokenizer.json (done once; then the compact file is used).
    pub fn from_json(path: &Path) -> Result<Unigram, String> {
        #[derive(serde::Deserialize)]
        struct File {
            model: Model,
        }
        #[derive(serde::Deserialize)]
        struct Model {
            vocab: Vec<(String, f64)>,
            unk_id: Option<u32>,
        }
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let f: File = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        let mut offs = Vec::with_capacity(f.model.vocab.len() + 1);
        let mut scores = Vec::with_capacity(f.model.vocab.len());
        for (p, s) in &f.model.vocab {
            offs.push(buf.len() as u32);
            buf.extend_from_slice(p.as_bytes());
            scores.push(*s as f32);
        }
        offs.push(buf.len() as u32);
        Ok(Self::build(buf, offs, scores, f.model.unk_id.unwrap_or(0)))
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let n = self.scores.len();
        let mut out = Vec::with_capacity(self.buf.len() + n * 8 + 32);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(n as u32).to_le_bytes());
        out.extend_from_slice(&self.unk.to_le_bytes());
        for i in 0..n {
            let piece = &self.buf[self.offs[i] as usize..self.offs[i + 1] as usize];
            out.extend_from_slice(&self.scores[i].to_le_bytes());
            out.extend_from_slice(&(piece.len() as u16).to_le_bytes());
            out.extend_from_slice(piece);
        }
        let tmp = path.with_extension("part");
        std::fs::write(&tmp, out)?;
        std::fs::rename(tmp, path)
    }

    pub fn load(path: &Path) -> Result<Unigram, String> {
        let b = std::fs::read(path).map_err(|e| e.to_string())?;
        if b.len() < 16 || &b[..8] != MAGIC {
            return Err("zły plik słownika".into());
        }
        let n = u32::from_le_bytes(b[8..12].try_into().unwrap()) as usize;
        let unk = u32::from_le_bytes(b[12..16].try_into().unwrap());
        let mut at = 16;
        let mut buf = Vec::with_capacity(b.len());
        let mut offs = Vec::with_capacity(n + 1);
        let mut scores = Vec::with_capacity(n);
        for _ in 0..n {
            if at + 6 > b.len() {
                return Err("niepełny plik słownika".into());
            }
            let s = f32::from_le_bytes(b[at..at + 4].try_into().unwrap());
            let len = u16::from_le_bytes(b[at + 4..at + 6].try_into().unwrap()) as usize;
            at += 6;
            offs.push(buf.len() as u32);
            buf.extend_from_slice(b.get(at..at + len).ok_or("niepełny plik słownika")?);
            at += len;
            scores.push(s);
        }
        offs.push(buf.len() as u32);
        buf.shrink_to_fit();
        Ok(Self::build(buf, offs, scores, unk))
    }

    fn find_hashed(&self, piece: &[u8], h: u64) -> Option<u32> {
        let mut slot = h as usize & self.mask;
        loop {
            let v = self.table[slot];
            if v == 0 {
                return None;
            }
            let i = (v - 1) as usize;
            if &self.buf[self.offs[i] as usize..self.offs[i + 1] as usize] == piece {
                return Some(i as u32);
            }
            slot = (slot + 1) & self.mask;
        }
    }

    pub fn len(&self) -> usize {
        self.scores.len()
    }

    /// The piece numbers of a text, best split first to last.
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let s = normalize(text);
        if s.is_empty() {
            return vec![];
        }
        let bytes = s.as_bytes();
        let n = bytes.len();
        // best[i]: (score of the best split of the first i bytes, where the last piece starts, its number)
        let mut best: Vec<(f32, u32, u32)> = vec![(f32::NEG_INFINITY, 0, 0); n + 1];
        best[0].0 = 0.0;
        let bounds: Vec<usize> = s.char_indices().map(|(i, _)| i).chain(std::iter::once(n)).collect();
        for (bi, &i) in bounds.iter().enumerate() {
            if i == n || best[i].0 == f32::NEG_INFINITY {
                continue;
            }
            let base = best[i].0;
            let mut found_single = false;
            for (k, &j) in bounds.iter().enumerate().skip(bi + 1).take(MAX_CHARS) {
                let h = hash(&bytes[i..j]);
                let bit = h as usize & (PREFIX_BITS - 1);
                if self.prefixes[bit / 64] & (1 << (bit % 64)) == 0 {
                    break;
                }
                if let Some(id) = self.find_hashed(&bytes[i..j], h) {
                    let sc = base + self.scores[id as usize];
                    if sc > best[j].0 {
                        best[j] = (sc, i as u32, id);
                    }
                    if k == bi + 1 {
                        found_single = true;
                    }
                }
            }
            if !found_single {
                // a letter no piece covers: the "unknown" piece
                let j = bounds[bi + 1];
                let sc = base + self.unk_score;
                if sc > best[j].0 {
                    best[j] = (sc, i as u32, self.unk);
                }
            }
        }
        let mut ids = Vec::new();
        let mut at = n;
        while at > 0 {
            let (_, from, id) = best[at];
            // unknown letters next to each other count as one
            if !(id == self.unk && ids.last() == Some(&self.unk)) {
                ids.push(id);
            }
            at = from as usize;
        }
        ids.reverse();
        ids
    }
}

/// The clean-up the model expects: compatibility forms of letters (NFKC), spaces around
/// punctuation, single spaces, and "▁" for spaces, also at the start.
pub fn normalize(text: &str) -> String {
    let mut spaced = String::with_capacity(text.len() + 16);
    for c in text.nfkc() {
        if c.is_ascii_punctuation() {
            spaced.push(' ');
            spaced.push(c);
            spaced.push(' ');
        } else if c.is_whitespace() || c.is_control() {
            spaced.push(' ');
        } else if c == '\u{200B}' || c == '\u{FEFF}' {
            // zero-width marks disappear
        } else {
            spaced.push(c);
        }
    }
    let mut out = String::with_capacity(spaced.len() + 8);
    for w in spaced.split(' ').filter(|w| !w.is_empty()) {
        out.push('▁');
        out.push_str(w);
    }
    out
}
