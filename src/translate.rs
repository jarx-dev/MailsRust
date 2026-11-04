//! A received email translated into Polish by AI, looking as it did: only the pieces of text
//! between the tags go to the model; every tag, style, picture and link stays as it was.

/// The pieces of text of an HTML page worth translating: byte ranges (without the spaces
/// around them). Scripts, styles and the page head are left out.
pub fn pieces(html: &str) -> Vec<(usize, usize)> {
    let lower = html.to_lowercase();
    let b = html.as_bytes();
    // the text starts at <body> when there is one
    let mut i = lower.find("<body").and_then(|p| lower[p..].find('>').map(|e| p + e + 1)).unwrap_or(0);
    let mut out = Vec::new();
    let mut text_start = i;
    while i < b.len() {
        if b[i] == b'<' {
            push_piece(html, text_start, i, &mut out);
            if lower[i..].starts_with("<!--") {
                i = lower[i..].find("-->").map(|e| i + e + 3).unwrap_or(b.len());
            } else {
                let tag_start = i;
                let end = tag_end(b, i);
                let name: String = lower[i + 1..end.min(b.len())].trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
                i = end;
                // (only an opening tag: its closing tag is where the skipping ends)
                let closing = lower[tag_start + 1..].starts_with('/');
                if !closing && !lower[..i].ends_with("/>") && (name == "script" || name == "style" || name == "title") {
                    let close = format!("</{name}");
                    i = lower[i..].find(&close).map(|e| i + e).unwrap_or(b.len());
                    // the style or script itself is never text to translate
                    text_start = i;
                    continue;
                }
            }
            text_start = i;
        } else {
            i += 1;
        }
    }
    push_piece(html, text_start, b.len().min(i), &mut out);
    out
}

/// Where a tag that starts at `i` ends (after its '>', quotes respected).
fn tag_end(b: &[u8], mut i: usize) -> usize {
    let mut quote: Option<u8> = None;
    while i < b.len() {
        match (quote, b[i]) {
            (None, b'"') | (None, b'\'') => quote = Some(b[i]),
            (Some(q), c) if c == q => quote = None,
            (None, b'>') => return i + 1,
            _ => {}
        }
        i += 1;
    }
    b.len()
}

fn push_piece(html: &str, from: usize, to: usize, out: &mut Vec<(usize, usize)>) {
    if from >= to || !html.is_char_boundary(from) || !html.is_char_boundary(to) {
        return;
    }
    let s = &html[from..to];
    let core = s.trim_matches(|c: char| c.is_whitespace());
    // something to read: at least two letters (not just numbers, dates or &nbsp;)
    let plain = core.replace("&nbsp;", " ").replace("&amp;", "&");
    if plain.chars().filter(|c| c.is_alphabetic()).count() < 2 {
        return;
    }
    let lead = s.len() - s.trim_start_matches(|c: char| c.is_whitespace()).len();
    out.push((from + lead, from + lead + core.len()));
}

/// The page with its pieces replaced by their translations (same order).
pub fn put_back(html: &str, pieces: &[(usize, usize)], done: &[String]) -> String {
    let mut out = String::with_capacity(html.len() + html.len() / 5);
    let mut last = 0;
    for ((a, b), t) in pieces.iter().zip(done) {
        out.push_str(&html[last..*a]);
        // the model writes plain text: a stray '<' must not open a tag
        out.push_str(&t.replace('<', "&lt;").replace('>', "&gt;"));
        last = *b;
    }
    out.push_str(&html[last..]);
    out
}

/// A plain-text email as a page (so the translation shows like a formatted one).
pub fn text_page(text: &str) -> String {
    let esc = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let paras: String = esc.split("\n\n").map(|p| format!("<p>{}</p>", p.replace('\n', "<br>"))).collect();
    format!("<html><head><meta charset=\"utf-8\"></head><body style=\"font-family:'Segoe UI',Arial,sans-serif;font-size:14px;color:#222\">{paras}</body></html>")
}

/// The whole email translated: its page (formatted) or its text, into a page in the cache.
/// Pieces go to the model in parts of a few thousand characters, several parts at a time.
pub fn translate_email(settings: &crate::ai::Settings, html: Option<String>, text: &str) -> Result<String, String> {
    let page = html.unwrap_or_else(|| text_page(text));
    let found = pieces(&page);
    if found.is_empty() {
        return Err("w tej wiadomości nie ma tekstu do przetłumaczenia".into());
    }
    let texts: Vec<String> = found.iter().map(|(a, b)| page[*a..*b].to_string()).collect();
    // parts of up to ~4000 characters (and 120 pieces)
    let mut parts: Vec<std::ops::Range<usize>> = Vec::new();
    let (mut start, mut size) = (0usize, 0usize);
    for (k, t) in texts.iter().enumerate() {
        if k > start && (size + t.len() > 2500 || k - start >= 80) {
            parts.push(start..k);
            start = k;
            size = 0;
        }
        size += t.len();
    }
    parts.push(start..texts.len());
    let t0 = std::time::Instant::now();
    let results: Vec<Result<Vec<String>, String>> = std::thread::scope(|sc| {
        let mut out = Vec::new();
        // up to eight parts at a time
        for chunk in parts.chunks(8) {
            let handles: Vec<_> = chunk.iter().map(|r| sc.spawn(|| crate::ai::translate_texts(settings, &texts[r.clone()]))).collect();
            for h in handles {
                out.push(h.join().unwrap_or_else(|_| Err("nieoczekiwany błąd".into())));
            }
        }
        out
    });
    let mut done = Vec::with_capacity(texts.len());
    for r in results {
        done.extend(r?);
    }
    crate::diag::log(format!("translated an email: {} pieces in {} parts, {:.1} s", texts.len(), parts.len(), t0.elapsed().as_secs_f32()));
    let out = put_back(&page, &found, &done);
    Ok(crate::model::cache_put(out.as_bytes(), "html"))
}
