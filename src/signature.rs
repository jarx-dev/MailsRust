//! The person's email signature, taken from their own sent mail (Gmail adds it to every
//! message, but doesn't hand it out to mail apps signed in with an app password).
//!
//! 1. Gmail (and many other programs) mark the signature in the message's formatting, e.g.
//!    `<div class="gmail_signature">`: the one found most often in recent sent mail wins.
//! 2. Otherwise: the ending that recent sent messages share (above any quoted reply), found in
//!    the text and then looked up in the formatted version to keep its look and pictures.

use crate::model::{Folder, Message};
use scraper::{ElementRef, Html, Selector};
use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
pub struct Found {
    pub html: String,
    pub text: String,
    /// pictures inside the signature: (content id, cached file)
    pub images: Vec<(String, String)>,
}

/// How many recent sent messages are looked at.
const RECENT: usize = 30;

/// Inside a quoted earlier message (someone else's signature lives there).
fn quoted(el: &ElementRef) -> bool {
    el.ancestors().filter_map(ElementRef::wrap).any(|a| {
        let v = a.value();
        v.name() == "blockquote"
            || v.attr("class").is_some_and(|c| c.contains("gmail_quote") || c.contains("quote"))
            || v.attr("id").is_some_and(|i| {
                let i = i.to_lowercase();
                i.contains("reply") || i.contains("quote") || i.contains("divrplyfwdmsg") || i.contains("appendonsend")
            })
    })
}

fn text_of(el: &ElementRef) -> String {
    let mut lines: Vec<String> = Vec::new();
    for t in el.text() {
        for l in t.split('\n') {
            let l = l.split_whitespace().collect::<Vec<_>>().join(" ");
            if !l.is_empty() {
                lines.push(l);
            }
        }
    }
    lines.join("\n")
}

fn images_of(html: &str, m: &Message) -> Vec<(String, String)> {
    let lower = html.to_lowercase();
    m.inline
        .iter()
        .filter(|(cid, _)| lower.contains(&format!("cid:{}", cid.to_lowercase())))
        .cloned()
        .collect()
}

/// The text the person wrote themselves: everything above the quoted earlier message.
fn own_lines(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    for l in body.lines() {
        let t = l.trim();
        let lower = t.to_lowercase();
        let quote_start = t.starts_with('>')
            || t.starts_with("------")
            || lower.starts_with("-----original")
            || lower.starts_with("from: ")
            || lower.starts_with("od: ")
            || lower.starts_with("wysłane z ")
            || (lower.starts_with("w dniu ") && lower.ends_with(':'))
            || (lower.starts_with("dnia ") && lower.ends_with(':'))
            || (lower.starts_with("on ") && lower.ends_with("wrote:"));
        if quote_start {
            break;
        }
        if !t.is_empty() {
            out.push(t.split_whitespace().collect::<Vec<_>>().join(" "));
        }
    }
    out
}

/// Finds the signature in the person's recent sent mail, if there is one.
pub fn detect(msgs: &[Message]) -> Option<Found> {
    let mut sent: Vec<&Message> = msgs.iter().filter(|m| m.folder == Folder::Sent && !m.sample).collect();
    sent.sort_by(|a, b| b.date.cmp(&a.date));
    sent.truncate(RECENT);
    if sent.is_empty() {
        return None;
    }

    // 1. signatures marked as such
    let marked = Selector::parse(
        ".gmail_signature, [data-smartmail=gmail_signature], #signature, .signature, #Signature, [id^=signature_], .moz-signature, #x_signature",
    )
    .ok()?;
    let mut votes: HashMap<String, (usize, Found)> = HashMap::new();
    for m in &sent {
        let Ok(html) = std::fs::read_to_string(&m.html_path) else { continue };
        if m.html_path.is_empty() {
            continue;
        }
        let doc = Html::parse_document(&html);
        let Some(el) = doc.select(&marked).find(|e| !quoted(e)) else { continue };
        let text = text_of(&el);
        if text.len() < 6 || text.len() > 2000 {
            continue;
        }
        let html = el.html();
        let images = images_of(&html, m);
        let e = votes.entry(text.clone()).or_insert((0, Found { html, text, images }));
        e.0 += 1;
    }
    if let Some((_, (n, f))) = votes.into_iter().max_by_key(|(_, (n, _))| *n) {
        if n >= 2 || sent.len() < 3 {
            return Some(f);
        }
    }

    // 2. the ending recent messages share
    let own: Vec<Vec<String>> = sent.iter().take(12).map(|m| own_lines(&m.body)).filter(|l| l.len() >= 2).collect();
    let newest = own.first()?;
    let need = (own.len() / 2).max(3).min(own.len());
    let mut best: Option<Vec<String>> = None;
    for k in (2..=newest.len().min(25)).rev() {
        let tail = &newest[newest.len() - k..];
        let n = own.iter().filter(|o| o.len() >= k && &o[o.len() - k..] == tail).count();
        if n >= need && n >= 2 {
            best = Some(tail.to_vec());
            break;
        }
    }
    let lines = best?;
    let text = lines.join("\n");
    // the same part in the formatted version (with its look and pictures), if there is one
    let first = lines.first().cloned().unwrap_or_default();
    let last = lines.last().cloned().unwrap_or_default();
    let any = Selector::parse("div, table, td, p, span").ok()?;
    for m in sent.iter().filter(|m| !m.html_path.is_empty()).take(5) {
        let Ok(html) = std::fs::read_to_string(&m.html_path) else { continue };
        let doc = Html::parse_document(&html);
        // the smallest part of the page holding the whole signature
        let best_el = doc
            .select(&any)
            .filter(|e| !quoted(e))
            .filter(|e| {
                let t = text_of(e);
                t.contains(&first) && t.contains(&last) && t.len() <= text.len() * 2 + 200
            })
            .min_by_key(|e| e.html().len());
        if let Some(el) = best_el {
            let html = el.html();
            let images = images_of(&html, m);
            return Some(Found { html, text, images });
        }
    }
    Some(Found { html: text_to_html(&text), text, images: vec![] })
}

/// Plain text as simple HTML (line breaks kept).
pub fn text_to_html(t: &str) -> String {
    let esc = t.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    esc.lines().collect::<Vec<_>>().join("<br>\n")
}
