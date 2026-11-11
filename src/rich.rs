//! Formatted text when writing: the message is written in Markdown (as AI agents produce it),
//! shown styled while typing, and sent as HTML. Pictures sit in the text as
//! `<img src="cid:…" width="…">` and are sent along inside the email.

use eframe::egui::{self, text::LayoutJob, Color32, FontFamily, FontId, Stroke, TextFormat};

/// How wide a pasted picture is shown in the message by default (it can be resized).
pub const DEFAULT_WIDTH: u32 = 600;

/// Colours offered for text: (name in the interface language, CSS colour).
pub fn colours() -> [(String, &'static str); 7] {
    [
        (t!("compose.rich.black").into_owned(), "#222222"),
        (t!("compose.rich.red").into_owned(), "#c62828"),
        (t!("compose.rich.blue").into_owned(), "#1e6fd9"),
        (t!("compose.rich.green").into_owned(), "#2e7d32"),
        (t!("compose.rich.orange").into_owned(), "#e65100"),
        (t!("compose.rich.purple").into_owned(), "#6a1b9a"),
        (t!("compose.rich.grey").into_owned(), "#757575"),
    ]
}

/// The written text as HTML: Markdown (with line breaks kept as typed) plus the small HTML
/// pieces the toolbar adds (colours, sizes, underline, pictures).
pub fn to_html(md: &str) -> String {
    use pulldown_cmark::{html, Event, Options, Parser};
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    // in an email, Enter means a new line
    let parser = Parser::new_ext(md, opts).map(|e| if matches!(e, Event::SoftBreak) { Event::HardBreak } else { e });
    let mut out = String::new();
    html::push_html(&mut out, parser);
    // pictures never wider than the message
    out.replace("<img ", "<img style=\"max-width:100%;height:auto\" ")
        .replace("<table>", "<table class=\"md\" style=\"border-collapse:collapse\">")
        .replace("<td>", "<td style=\"border:1px solid #d6d9dd;padding:4px 8px\">")
        .replace("<th>", "<th style=\"border:1px solid #d6d9dd;padding:4px 8px;background:#f4f5f7\">")
}

/// The text without formatting marks, for the plain-text version of the email.
pub fn plain(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let mut rest = md;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        match after.find('>') {
            Some(j) if j < 300 && looks_like_tag(&after[..=j]) => {
                if after[..=j].to_lowercase().starts_with("<img") {
                    out.push_str(&t!("compose.rich.image_placeholder"));
                }
                rest = &after[j + 1..];
            }
            _ => {
                out.push('<');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out.replace("**", "").replace("__", "")
}

fn looks_like_tag(t: &str) -> bool {
    let l = t.to_lowercase();
    ["<span", "</span", "<u>", "</u>", "<b>", "</b>", "<i>", "</i>", "<strong", "</strong", "<em>", "</em>", "<img", "<br", "<font", "</font"]
        .iter()
        .any(|p| l.starts_with(p))
}

/// The pictures in the text: (byte range of the tag, content id, width).
pub fn images(md: &str) -> Vec<(std::ops::Range<usize>, String, u32)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = md[from..].find("<img ") {
        let start = from + i;
        let Some(len) = md[start..].find('>') else { break };
        let end = start + len + 1;
        let tag = &md[start..end];
        let attr = |name: &str| -> Option<String> {
            let k = format!("{name}=\"");
            let a = tag.find(&k)? + k.len();
            let b = tag[a..].find('"')? + a;
            Some(tag[a..b].to_string())
        };
        if let Some(src) = attr("src") {
            if let Some(cid) = src.strip_prefix("cid:") {
                let w = attr("width").and_then(|w| w.parse().ok()).unwrap_or(DEFAULT_WIDTH);
                out.push((start..end, cid.to_string(), w));
            }
        }
        from = end;
    }
    out
}

pub fn image_tag(cid: &str, width: u32) -> String {
    format!("<img src=\"cid:{cid}\" width=\"{width}\">")
}

fn char_to_byte(s: &str, c: usize) -> usize {
    s.char_indices().nth(c).map(|(b, _)| b).unwrap_or(s.len())
}

/// Puts `before` and `after` around the chosen text (char positions); returns the new
/// selection (char positions) around the same text.
pub fn wrap(text: &mut String, from: usize, to: usize, before: &str, after: &str) -> (usize, usize) {
    let (a, b) = (from.min(to), from.max(to));
    let (ba, bb) = (char_to_byte(text, a), char_to_byte(text, b));
    // already wrapped: take the marks off again
    if text[..ba].ends_with(before) && text[bb..].starts_with(after) && !before.is_empty() {
        text.replace_range(bb..bb + after.len(), "");
        text.replace_range(ba - before.len()..ba, "");
        let n = before.chars().count();
        return (a - n, b - n);
    }
    text.insert_str(bb, after);
    text.insert_str(ba, before);
    let n = before.chars().count();
    (a + n, b + n)
}

/// Puts `prefix` at the start of every line touched by the selection (e.g. "- " for a list).
pub fn prefix_lines(text: &mut String, from: usize, to: usize, prefix: &str) -> usize {
    let (a, b) = (from.min(to), from.max(to));
    let (ba, bb) = (char_to_byte(text, a), char_to_byte(text, b));
    let line_start = text[..ba].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let mut added = 0;
    let mut pos = line_start;
    loop {
        if !text[pos..].starts_with(prefix) {
            text.insert_str(pos, prefix);
            added += 1;
        }
        let limit = bb + added * prefix.len();
        match text[pos..].find('\n') {
            Some(i) if pos + i < limit => pos = pos + i + 1,
            _ => break,
        }
    }
    b + added * prefix.chars().count()
}

fn colour(v: &str) -> Option<Color32> {
    let v = v.trim().to_lowercase();
    if let Some(h) = v.strip_prefix('#') {
        let h = if h.len() == 3 { h.chars().flat_map(|c| [c, c]).collect::<String>() } else { h.to_string() };
        if h.len() == 6 {
            let n = u32::from_str_radix(&h, 16).ok()?;
            return Some(Color32::from_rgb((n >> 16) as u8, (n >> 8) as u8, n as u8));
        }
    }
    match v.as_str() {
        "red" => Some(Color32::from_rgb(0xC6, 0x28, 0x28)),
        "blue" => Some(Color32::from_rgb(0x1E, 0x6F, 0xD9)),
        "green" => Some(Color32::from_rgb(0x2E, 0x7D, 0x32)),
        "orange" => Some(Color32::from_rgb(0xE6, 0x51, 0x00)),
        "gray" | "grey" => Some(Color32::GRAY),
        _ => None,
    }
}

fn css(style: &str, key: &str) -> Option<String> {
    style.split(';').find_map(|d| {
        let (k, v) = d.split_once(':')?;
        (k.trim().eq_ignore_ascii_case(key)).then(|| v.trim().to_string())
    })
}

#[derive(Clone)]
struct St {
    bold: bool,
    italic: bool,
    under: bool,
    code: bool,
    strike: bool,
    colours: Vec<Color32>,
    sizes: Vec<f32>,
}

/// The text as shown while writing: Markdown and the toolbar's marks styled (bold, italics,
/// headings, colours, sizes), the marks themselves faint.
pub fn highlight(text: &str, wrap_width: f32, text_colour: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width;
    let faint = Color32::from_rgb(0xB4, 0xB8, 0xBE);
    let semibold = FontFamily::Name("semibold".into());
    let mark = |job: &mut LayoutJob, t: &str, size: f32| {
        job.append(t, 0.0, TextFormat { font_id: FontId::proportional(size.min(13.0)), color: faint, ..Default::default() });
    };
    let mut st = St { bold: false, italic: false, under: false, code: false, strike: false, colours: vec![], sizes: vec![] };
    for line in text.split_inclusive('\n') {
        let mut rest = line;
        let mut base = 14.5f32;
        let mut heading = false;
        let hashes = rest.chars().take_while(|c| *c == '#').count();
        if (1..=3).contains(&hashes) && rest[hashes..].starts_with(' ') {
            base = [22.0, 19.0, 16.5][hashes - 1];
            heading = true;
            mark(&mut job, &rest[..hashes + 1], 14.0);
            rest = &rest[hashes + 1..];
        } else if rest.starts_with("- ") || rest.starts_with("* ") {
            job.append(&rest[..2], 0.0, TextFormat { font_id: FontId::new(14.5, semibold.clone()), color: crate::theme::ACCENT(), ..Default::default() });
            rest = &rest[2..];
        } else if rest.starts_with("> ") {
            mark(&mut job, &rest[..2], 14.5);
            rest = &rest[2..];
        }
        let fmt = |st: &St| {
            let size = st.sizes.last().copied().unwrap_or(base);
            let family = if st.bold || heading { semibold.clone() } else { FontFamily::Proportional };
            TextFormat {
                font_id: if st.code { FontId::monospace(size - 1.0) } else { FontId::new(size, family) },
                color: st.colours.last().copied().unwrap_or(text_colour),
                italics: st.italic,
                underline: if st.under { Stroke::new(1.0, st.colours.last().copied().unwrap_or(text_colour)) } else { Stroke::NONE },
                strikethrough: if st.strike { Stroke::new(1.0, text_colour) } else { Stroke::NONE },
                background: if st.code { Color32::from_rgb(0xF0, 0xF1, 0xF3) } else { Color32::TRANSPARENT },
                ..Default::default()
            }
        };
        let mut buf = String::new();
        let mut i = 0;
        while i < rest.len() {
            let s = &rest[i..];
            let flush = |job: &mut LayoutJob, buf: &mut String, st: &St| {
                if !buf.is_empty() {
                    job.append(buf, 0.0, fmt(st));
                    buf.clear();
                }
            };
            if st.code && !s.starts_with('`') {
                let c = s.chars().next().unwrap();
                buf.push(c);
                i += c.len_utf8();
                continue;
            }
            if s.starts_with('`') {
                flush(&mut job, &mut buf, &st);
                mark(&mut job, "`", base);
                st.code = !st.code;
                i += 1;
                continue;
            }
            if s.starts_with("**") || s.starts_with("__") {
                flush(&mut job, &mut buf, &st);
                mark(&mut job, &s[..2], base);
                st.bold = !st.bold;
                i += 2;
                continue;
            }
            if s.starts_with("~~") {
                flush(&mut job, &mut buf, &st);
                mark(&mut job, "~~", base);
                st.strike = !st.strike;
                i += 2;
                continue;
            }
            if s.starts_with('*') && (st.italic || !s[1..].starts_with([' ', '\n'])) {
                flush(&mut job, &mut buf, &st);
                mark(&mut job, "*", base);
                st.italic = !st.italic;
                i += 1;
                continue;
            }
            if s.starts_with('<') {
                if let Some(j) = s.find('>').filter(|j| *j < 300) {
                    let tag = &s[..=j];
                    let l = tag.to_lowercase();
                    let known = looks_like_tag(tag);
                    if known {
                        flush(&mut job, &mut buf, &st);
                        if l.starts_with("<img") {
                            // a picture: its own text, tiny on a blue band (the strip under the
                            // text shows the picture itself). The text drawn must stay exactly the
                            // text written, or the cursor would land in the wrong place.
                            job.append(
                                tag,
                                0.0,
                                TextFormat {
                                    font_id: FontId::proportional(7.0),
                                    color: crate::theme::ACCENT(),
                                    background: Color32::from_rgb(0xE3, 0xEE, 0xFB),
                                    ..Default::default()
                                },
                            );
                        } else {
                            // colour / size / underline marks: tiny and faint, so the text reads well
                            job.append(tag, 0.0, TextFormat { font_id: FontId::proportional(6.5), color: faint, ..Default::default() });
                            if l.starts_with("<span") || l.starts_with("<font") {
                                let style = tag.split_once("style=\"").map(|x| x.1.split('"').next().unwrap_or("")).unwrap_or("");
                                let c = css(style, "color").and_then(|v| colour(&v)).or_else(|| {
                                    tag.split_once("color=\"").and_then(|x| colour(x.1.split('"').next().unwrap_or("")))
                                });
                                let size = css(style, "font-size").and_then(|v| v.trim_end_matches("px").trim().parse::<f32>().ok());
                                st.colours.push(c.unwrap_or(st.colours.last().copied().unwrap_or(text_colour)));
                                st.sizes.push(size.map(|x| x.clamp(9.0, 32.0)).unwrap_or(st.sizes.last().copied().unwrap_or(base)));
                            } else if l.starts_with("</span") || l.starts_with("</font") {
                                st.colours.pop();
                                st.sizes.pop();
                            } else if l == "<u>" {
                                st.under = true;
                            } else if l == "</u>" {
                                st.under = false;
                            } else if l.starts_with("<b") || l.starts_with("<strong") {
                                st.bold = true;
                            } else if l.starts_with("</b") || l.starts_with("</strong") {
                                st.bold = false;
                            } else if l == "<i>" || l.starts_with("<em") {
                                st.italic = true;
                            } else if l == "</i>" || l.starts_with("</em") {
                                st.italic = false;
                            }
                        }
                        i += j + 1;
                        continue;
                    }
                }
            }
            let c = s.chars().next().unwrap();
            buf.push(c);
            i += c.len_utf8();
        }
        if !buf.is_empty() {
            job.append(&buf, 0.0, fmt(&st));
        }
        // bold / italics don't run past the end of a line
        st.bold = false;
        st.italic = false;
        st.strike = false;
        st.code = false;
    }
    job
}

/// A picture pasted or dropped into the message: saved, with a new content id; its width in
/// the message is scaled down to `DEFAULT_WIDTH` if it is wider.
pub fn store_picture(png_or_file: &[u8], ext: &str) -> Option<(String, String, u32)> {
    let (w, _) = image::ImageReader::new(std::io::Cursor::new(png_or_file)).with_guessed_format().ok()?.into_dimensions().ok()?;
    let file = crate::model::cache_put(png_or_file, ext);
    let cid = format!("obraz-{}@poczta", crate::share::new_token().get(..12).unwrap_or("x").to_string());
    Some((cid, file, w.min(DEFAULT_WIDTH).max(16)))
}

/// A pasted screenshot (raw pixels) as a PNG picture.
pub fn store_pasted(img: &egui::ColorImage) -> Option<(String, String, u32)> {
    let [w, h] = img.size;
    let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
    let buf = image::RgbaImage::from_raw(w as u32, h as u32, rgba)?;
    let mut png = Vec::new();
    buf.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).ok()?;
    store_picture(&png, "png")
}
