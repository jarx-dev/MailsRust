//! Search by meaning: a small multilingual language model (Model2Vec "potion-multilingual",
//! MIT licence) turns a text into a list of 256 numbers that describes what it is about.
//! Messages with similar meaning get similar lists, even when they use different words.
//!
//! The model is downloaded once (about 500 MB) and, while it arrives, shrunk to a quarter
//! of its size; after that everything works offline.

use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::Duration;

const REPO: &str = "https://huggingface.co/minishlab/potion-multilingual-128M/resolve/main";
const MAGIC: &[u8; 8] = b"PCZTM2V1";

pub fn model_dir() -> PathBuf {
    let dir = crate::model::cache_dir().parent().map(|p| p.join("model")).unwrap_or_else(|| PathBuf::from("model"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn model_file() -> PathBuf {
    model_dir().join("model-i8.bin")
}

fn tokenizer_file() -> PathBuf {
    model_dir().join("tokenizer.json")
}

pub fn is_downloaded() -> bool {
    model_file().exists() && tokenizer_file().exists()
}

pub enum Progress {
    /// 0.0 … 1.0
    Downloading(f32),
    Done,
    Failed(String),
}

fn agent() -> Result<ureq::Agent, String> {
    let tls = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
    Ok(ureq::AgentBuilder::new()
        .tls_connector(std::sync::Arc::new(tls))
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .user_agent("Poczta (model download)")
        .build())
}

/// Downloads the tokenizer and the model, shrinking the model on the fly.
pub fn download(tx: Sender<Progress>, ctx: eframe::egui::Context) {
    std::thread::spawn(move || {
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| download_inner(&tx, &ctx)))
            .unwrap_or_else(|_| Err("nieoczekiwany błąd".into()));
        let _ = tx.send(match res {
            Ok(()) => Progress::Done,
            Err(e) => Progress::Failed(e),
        });
        ctx.request_repaint();
    });
}

fn download_inner(tx: &Sender<Progress>, ctx: &eframe::egui::Context) -> Result<(), String> {
    let agent = agent()?;
    // 1. the tokenizer (how texts are split into word pieces), ~18 MB
    let tok_tmp = model_dir().join("tokenizer.json.part");
    {
        let resp = agent.get(&format!("{REPO}/tokenizer.json")).call().map_err(|e| e.to_string())?;
        let mut r = resp.into_reader();
        let mut f = std::fs::File::create(&tok_tmp).map_err(|e| e.to_string())?;
        std::io::copy(&mut r, &mut f).map_err(|e| e.to_string())?;
    }
    // 2. the model: a table with one row of numbers per word piece, ~512 MB as floats
    let resp = agent.get(&format!("{REPO}/model.safetensors")).call().map_err(|e| e.to_string())?;
    let total: f64 = resp.header("Content-Length").and_then(|v| v.parse().ok()).unwrap_or(512_361_560.0);
    let mut r = BufReader::with_capacity(1 << 20, resp.into_reader());
    let mut len8 = [0u8; 8];
    r.read_exact(&mut len8).map_err(|e| e.to_string())?;
    let header_len = u64::from_le_bytes(len8) as usize;
    let mut header = vec![0u8; header_len];
    r.read_exact(&mut header).map_err(|e| e.to_string())?;
    let header: serde_json::Value = serde_json::from_slice(&header).map_err(|e| e.to_string())?;
    let emb = header.get("embeddings").ok_or("brak tabeli w modelu")?;
    let dtype = emb.get("dtype").and_then(|d| d.as_str()).unwrap_or("");
    let shape: Vec<usize> = emb
        .get("shape")
        .and_then(|s| s.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_u64().map(|v| v as usize)).collect())
        .unwrap_or_default();
    let start = emb
        .get("data_offsets")
        .and_then(|o| o.as_array())
        .and_then(|a| a.first())
        .and_then(|x| x.as_u64())
        .unwrap_or(0) as usize;
    if shape.len() != 2 || !(dtype == "F32" || dtype == "F16") {
        return Err(format!("nieznany format modelu ({dtype})"));
    }
    let (vocab, dim) = (shape[0], shape[1]);
    let bytes_per = if dtype == "F32" { 4 } else { 2 };
    // skip anything before the table
    std::io::copy(&mut (&mut r).take(start as u64), &mut std::io::sink()).map_err(|e| e.to_string())?;

    let out_tmp = model_dir().join("model-i8.bin.part");
    let mut w = BufWriter::new(std::fs::File::create(&out_tmp).map_err(|e| e.to_string())?);
    w.write_all(MAGIC).map_err(|e| e.to_string())?;
    w.write_all(&(vocab as u32).to_le_bytes()).map_err(|e| e.to_string())?;
    w.write_all(&(dim as u32).to_le_bytes()).map_err(|e| e.to_string())?;
    // scales first (one per row), then the rows; scales are only known after each row is
    // read, so they are kept in memory and written at the end of the file
    let mut scales: Vec<f32> = Vec::with_capacity(vocab);
    let mut raw = vec![0u8; dim * bytes_per];
    let mut row = vec![0f32; dim];
    let mut q = vec![0u8; dim];
    let mut done_bytes = (8 + header_len + start) as f64;
    let mut last_report = 0.0f32;
    for _ in 0..vocab {
        r.read_exact(&mut raw).map_err(|e| e.to_string())?;
        for (j, v) in row.iter_mut().enumerate() {
            *v = if bytes_per == 4 {
                f32::from_le_bytes([raw[4 * j], raw[4 * j + 1], raw[4 * j + 2], raw[4 * j + 3]])
            } else {
                f16_to_f32(u16::from_le_bytes([raw[2 * j], raw[2 * j + 1]]))
            };
        }
        let max = row.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-12);
        let scale = max / 127.0;
        for (j, v) in row.iter().enumerate() {
            q[j] = ((v / scale).round().clamp(-127.0, 127.0) as i8) as u8;
        }
        w.write_all(&q).map_err(|e| e.to_string())?;
        scales.push(scale);
        done_bytes += raw.len() as f64;
        let p = (done_bytes / total) as f32;
        if p - last_report >= 0.01 {
            last_report = p;
            let _ = tx.send(Progress::Downloading(p.min(0.99)));
            ctx.request_repaint();
        }
    }
    for s in &scales {
        w.write_all(&s.to_le_bytes()).map_err(|e| e.to_string())?;
    }
    w.flush().map_err(|e| e.to_string())?;
    drop(w);
    std::fs::rename(&tok_tmp, tokenizer_file()).map_err(|e| e.to_string())?;
    std::fs::rename(&out_tmp, model_file()).map_err(|e| e.to_string())?;
    Ok(())
}

fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) as u32) << 31;
    let exp = ((h >> 10) & 0x1f) as u32;
    let frac = (h & 0x3ff) as u32;
    let bits = if exp == 0 {
        if frac == 0 {
            sign
        } else {
            // subnormal
            let mut e = 127 - 15 + 1;
            let mut f = frac;
            while f & 0x400 == 0 {
                f <<= 1;
                e -= 1;
            }
            sign | (e << 23) | ((f & 0x3ff) << 13)
        }
    } else if exp == 31 {
        sign | 0x7f80_0000 | (frac << 13)
    } else {
        sign | ((exp + 127 - 15) << 23) | (frac << 13)
    };
    f32::from_bits(bits)
}

pub struct Model {
    tokenizer: crate::unigram::Unigram,
    pub dim: usize,
    vocab: usize,
    /// the model file, used straight from the disk (Windows keeps the parts in use in memory,
    /// shared with the file, instead of a private 130 MB copy); the numbers start at `rows_at`
    data: memmap2::Mmap,
    rows_at: usize,
    scales: Vec<f32>,
}

impl Model {
    pub fn load() -> Result<Model, String> {
        // the word pieces: a compact copy made once from the downloaded list
        let compact = model_dir().join("vocab.bin");
        let tokenizer = match crate::unigram::Unigram::load(&compact) {
            Ok(t) => t,
            Err(_) => {
                let t = crate::unigram::Unigram::from_json(&tokenizer_file())?;
                let _ = t.save(&compact);
                t
            }
        };
        let file = std::fs::File::open(model_file()).map_err(|e| e.to_string())?;
        // (the file is only read; the app never changes it while it is in use)
        let bytes = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| e.to_string())?;
        if bytes.len() < 16 || &bytes[..8] != MAGIC {
            return Err("uszkodzony plik modelu".into());
        }
        let vocab = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let dim = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let rows_end = 16 + vocab * dim;
        if bytes.len() < rows_end + vocab * 4 {
            return Err("niepełny plik modelu".into());
        }
        let scales: Vec<f32> = bytes[rows_end..rows_end + vocab * 4]
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        Ok(Model { tokenizer, dim, vocab, data: bytes, rows_at: 16, scales })
    }

    /// The meaning of a text as a list of `dim` numbers of length 1.
    pub fn embed(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0f32; self.dim];
        // the start of a long text is enough to know what it is about
        let text: String = text.chars().take(4000).collect();
        let ids = self.tokenizer.encode(&text);
        let mut n = 0usize;
        for &id in ids.iter().take(512) {
            let id = id as usize;
            if id >= self.vocab {
                continue;
            }
            let s = self.scales[id];
            let at = self.rows_at + id * self.dim;
            let row = &self.data[at..at + self.dim];
            for (a, b) in v.iter_mut().zip(row) {
                *a += (*b as i8) as f32 * s;
            }
            n += 1;
        }
        if n > 0 {
            let len = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
            for x in v.iter_mut() {
                *x /= len;
            }
        }
        v
    }
}

/// A meaning list stored compactly: each number as a small whole number.
pub fn pack(v: &[f32]) -> Vec<i8> {
    v.iter().map(|x| (x * 127.0).round().clamp(-127.0, 127.0) as i8).collect()
}

/// How alike two meanings are: about 1 = same, about 0 = unrelated.
pub fn similarity(q: &[f32], packed: &[i8]) -> f32 {
    q.iter().zip(packed).map(|(a, b)| a * (*b as f32)).sum::<f32>() / 127.0
}

/// Saved meanings of messages: message id → packed list.
pub fn load_vectors(dim: usize) -> std::collections::HashMap<u64, Vec<i8>> {
    let mut out = std::collections::HashMap::new();
    let Ok(bytes) = std::fs::read(model_dir().join("vectors.bin")) else { return out };
    for rec in bytes.chunks_exact(8 + dim) {
        let id = u64::from_le_bytes(rec[..8].try_into().unwrap());
        out.insert(id, rec[8..].iter().map(|b| *b as i8).collect());
    }
    out
}

pub fn save_vectors(v: &std::collections::HashMap<u64, Vec<i8>>) {
    let mut bytes = Vec::with_capacity(v.len() * 264);
    for (id, vec) in v {
        bytes.extend_from_slice(&id.to_le_bytes());
        bytes.extend(vec.iter().map(|x| *x as u8));
    }
    let tmp = model_dir().join("vectors.bin.part");
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, model_dir().join("vectors.bin"));
    }
}
