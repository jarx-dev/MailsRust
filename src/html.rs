//! A small reader for HTML emails: turns the email's HTML into simple blocks
//! (paragraphs with bold / italic / colour / links, headings, pictures, lines,
//! tables and coloured boxes) that the reading pane can draw.
//! Hidden parts of the email stay hidden.

use scraper::{ElementRef, Html, Node};

pub type Rgb = [u8; 3];

#[derive(Clone, Debug)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub link: Option<String>,
    pub size: f32,
    pub color: Option<Rgb>,
    /// a small picture inside the line of text (e.g. an icon before a phone number): its
    /// address and width, if given
    pub img: Option<(String, Option<f32>)>,
}

#[derive(Clone, Debug)]
pub struct Cell {
    pub blocks: Vec<Block>,
    pub colspan: usize,
    pub bg: Option<Rgb>,
    pub border: Option<Rgb>,
    pub center: bool,
}

#[derive(Clone, Debug)]
pub enum Block {
    Para { runs: Vec<Run>, center: bool },
    Image { src: String, width: Option<f32>, height: Option<f32>, link: Option<String>, center: bool },
    Rule,
    /// A real table: rows of cells side by side. `weights` gives each column's share of the width.
    Table { rows: Vec<Vec<Cell>>, weights: Vec<f32>, border: Option<Rgb>, bg: Option<Rgb>, gap: f32 },
    /// A coloured or outlined box (e.g. a "card" in a newsletter).
    Boxed { blocks: Vec<Block>, bg: Option<Rgb>, border: Option<Rgb>, center: bool },
}

#[derive(Clone)]
struct Style {
    bold: bool,
    italic: bool,
    link: Option<String>,
    size: f32,
    center: bool,
    pre: bool,
    color: Option<Rgb>,
}

const BODY_SIZE: f32 = 14.5;

struct Walker {
    blocks: Vec<Block>,
    runs: Vec<Run>,
    center: bool,
}

impl Walker {
    fn new() -> Self {
        Self { blocks: vec![], runs: vec![], center: false }
    }

    fn flush(&mut self) {
        let runs = std::mem::take(&mut self.runs);
        let mut runs: Vec<Run> = runs.into_iter().filter(|r| !r.text.is_empty()).collect();
        if let Some(first) = runs.first_mut() {
            first.text = first.text.trim_start().to_string();
        }
        if let Some(last) = runs.last_mut() {
            last.text = last.text.trim_end().to_string();
        }
        runs.retain(|r| !r.text.is_empty());
        if runs.iter().any(|r| !r.text.trim().is_empty()) {
            for r in runs.iter_mut() {
                while r.text.contains("\n\n\n") {
                    r.text = r.text.replace("\n\n\n", "\n\n");
                }
                r.text = r.text.replace(" \n", "\n").replace("\n ", "\n");
            }
            self.blocks.push(Block::Para { runs, center: self.center });
        }
    }

    fn same(r: &Run, st: &Style) -> bool {
        r.img.is_none() && r.bold == st.bold && r.italic == st.italic && r.link == st.link && r.size == st.size && r.color == st.color
    }

    fn push_text(&mut self, text: &str, st: &Style) {
        if self.runs.is_empty() {
            self.center = st.center;
        }
        let text = if st.pre {
            text.to_string()
        } else {
            let mut out = String::with_capacity(text.len());
            let mut space = self.runs.last().map(|r| r.text.ends_with([' ', '\n'])).unwrap_or(true);
            for c in text.chars() {
                if c.is_whitespace() {
                    if !space {
                        out.push(' ');
                        space = true;
                    }
                } else {
                    out.push(c);
                    space = false;
                }
            }
            out
        };
        if text.is_empty() {
            return;
        }
        if let Some(last) = self.runs.last_mut() {
            if Self::same(last, st) {
                last.text.push_str(&text);
                return;
            }
        }
        self.runs.push(Run { text, bold: st.bold, italic: st.italic, link: st.link.clone(), size: st.size, color: st.color, img: None });
    }

    fn push_raw(&mut self, text: &str, st: &Style) {
        if self.runs.is_empty() {
            self.center = st.center;
        }
        if let Some(last) = self.runs.last_mut() {
            if Self::same(last, st) {
                while last.text.ends_with(' ') {
                    last.text.pop();
                }
                last.text.push_str(text);
                return;
            }
        }
        self.runs.push(Run { text: text.to_string(), bold: st.bold, italic: st.italic, link: st.link.clone(), size: st.size, color: st.color, img: None });
    }

    /// A small picture that stays in the line of text.
    fn push_icon(&mut self, src: String, width: Option<f32>, st: &Style) {
        if self.runs.is_empty() {
            self.center = st.center;
        }
        self.runs.push(Run {
            text: "\u{FFFC}".into(),
            bold: false,
            italic: false,
            link: st.link.clone(),
            size: st.size,
            color: None,
            img: Some((src, width)),
        });
    }
}

fn css(style: &str, key: &str) -> Option<String> {
    for decl in style.split(';') {
        if let Some((k, v)) = decl.split_once(':') {
            if k.trim().eq_ignore_ascii_case(key) {
                return Some(v.trim().trim_end_matches("!important").trim().to_lowercase());
            }
        }
    }
    None
}

fn is_hidden(el: &ElementRef) -> bool {
    let v = el.value();
    if v.attr("hidden").is_some() {
        return true;
    }
    let style = v.attr("style").unwrap_or("").to_lowercase().replace(' ', "");
    style.contains("display:none")
        || style.contains("visibility:hidden")
        || style.contains("max-height:0")
        || style.contains("font-size:0")
        || style.contains("opacity:0;")
        || style.ends_with("opacity:0")
}

fn num(s: &str) -> Option<f32> {
    let s = s.trim();
    if s.ends_with('%') {
        return None;
    }
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    digits.parse::<f32>().ok()
}

fn px(style: &str, key: &str) -> Option<f32> {
    css(style, key).filter(|v| v.ends_with("px")).and_then(|v| num(v.trim_end_matches("px")))
}

pub fn parse_color(s: &str) -> Option<Rgb> {
    let s = s.trim().to_lowercase();
    if let Some(hex) = s.strip_prefix('#') {
        let hex: String = hex.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        let v = |i: usize, n: usize| u8::from_str_radix(&hex[i..i + n], 16).ok();
        return match hex.len() {
            3 => Some([v(0, 1)? * 17, v(1, 1)? * 17, v(2, 1)? * 17]),
            6 | 8 => Some([v(0, 2)?, v(2, 2)?, v(4, 2)?]),
            _ => None,
        };
    }
    if let Some(inner) = s.strip_prefix("rgb(").or_else(|| s.strip_prefix("rgba(")) {
        let parts: Vec<u8> = inner
            .trim_end_matches(')')
            .split(',')
            .take(3)
            .filter_map(|p| p.trim().parse::<f32>().ok().map(|x| x.clamp(0.0, 255.0) as u8))
            .collect();
        return (parts.len() == 3).then(|| [parts[0], parts[1], parts[2]]);
    }
    Some(match s.as_str() {
        "white" => [255, 255, 255],
        "black" => [0, 0, 0],
        "red" => [220, 38, 38],
        "green" => [22, 128, 61],
        "blue" => [37, 99, 235],
        "gray" | "grey" => [128, 128, 128],
        "orange" => [234, 128, 32],
        "yellow" => [250, 204, 21],
        "navy" => [0, 0, 128],
        "silver" => [192, 192, 192],
        "lightgray" | "lightgrey" => [211, 211, 211],
        "whitesmoke" => [245, 245, 245],
        _ => return None,
    })
}

/// Background colour of an element (style or old-style bgcolor attribute).
fn background(el: &ElementRef) -> Option<Rgb> {
    let v = el.value();
    let style = v.attr("style").unwrap_or("");
    css(style, "background-color")
        .and_then(|c| parse_color(&c))
        .or_else(|| {
            css(style, "background").and_then(|b| b.split_whitespace().find_map(parse_color))
        })
        .or_else(|| v.attr("bgcolor").and_then(parse_color))
}

/// Border colour of an element, if it draws a visible border.
fn border(el: &ElementRef) -> Option<Rgb> {
    let v = el.value();
    let style = v.attr("style").unwrap_or("");
    for key in ["border", "border-top", "border-bottom", "border-left"] {
        if let Some(b) = css(style, key) {
            if b.contains("none") || b.starts_with('0') {
                continue;
            }
            return Some(b.split_whitespace().find_map(parse_color).unwrap_or([210, 210, 210]));
        }
    }
    if v.attr("border").and_then(num).map(|b| b > 0.0).unwrap_or(false) {
        return Some([210, 210, 210]);
    }
    None
}

fn element_style(el: &ElementRef, st: &Style) -> Style {
    let e = el.value();
    let name = e.name();
    let mut s = st.clone();
    let style = e.attr("style").unwrap_or("").to_lowercase();
    let align = e.attr("align").map(|a| a.to_lowercase());
    match css(&style, "text-align").as_deref().or(align.as_deref()) {
        Some("center") => s.center = true,
        Some("left") | Some("right") | Some("justify") => s.center = false,
        _ => {}
    }
    if name == "center" {
        s.center = true;
    }
    if let Some(w) = css(&style, "font-weight") {
        s.bold = w == "bold" || w == "bolder" || w.parse::<u32>().map(|n| n >= 600).unwrap_or(false);
    }
    if css(&style, "font-style").as_deref() == Some("italic") {
        s.italic = true;
    }
    if let Some(c) = css(&style, "color").and_then(|c| parse_color(&c)) {
        s.color = Some(c);
    } else if name == "font" {
        if let Some(c) = e.attr("color").and_then(parse_color) {
            s.color = Some(c);
        }
    }
    match name {
        "b" | "strong" | "th" => s.bold = true,
        "i" | "em" => s.italic = true,
        "a" => {
            if let Some(h) = e.attr("href") {
                let h = h.trim();
                if h.starts_with("http") || h.starts_with("mailto:") {
                    s.link = Some(h.to_string());
                }
            }
        }
        "h1" => {
            s.size = 22.0;
            s.bold = true;
        }
        "h2" => {
            s.size = 19.0;
            s.bold = true;
        }
        "h3" => {
            s.size = 17.0;
            s.bold = true;
        }
        "h4" | "h5" | "h6" => s.bold = true,
        "small" => s.size = (s.size * 0.85).max(11.0),
        "pre" => s.pre = true,
        _ => {}
    }
    if let Some(p) = px(&style, "font-size") {
        if p >= 9.0 && !name.starts_with('h') {
            s.size = (p * 0.95).clamp(11.0, 34.0);
        }
    }
    s
}

/// The rows of a table: (row element, its cells).
fn table_rows<'a>(table: ElementRef<'a>) -> Vec<Vec<ElementRef<'a>>> {
    let mut rows = vec![];
    let mut add_row = |tr: ElementRef<'a>| {
        let cells: Vec<ElementRef> = tr
            .children()
            .filter_map(ElementRef::wrap)
            .filter(|c| matches!(c.value().name(), "td" | "th") && !is_hidden(c))
            .collect();
        if !cells.is_empty() {
            rows.push(cells);
        }
    };
    for child in table.children().filter_map(ElementRef::wrap) {
        match child.value().name() {
            "tr" => add_row(child),
            "thead" | "tbody" | "tfoot" => {
                for tr in child.children().filter_map(ElementRef::wrap) {
                    if tr.value().name() == "tr" {
                        add_row(tr);
                    }
                }
            }
            _ => {}
        }
    }
    rows
}

fn text_len(el: &ElementRef) -> usize {
    el.text().map(|t| t.split_whitespace().map(|w| w.chars().count() + 1).sum::<usize>()).sum()
}

fn walk_children(w: &mut Walker, node: ego_tree::NodeRef<Node>, st: &Style) {
    for child in node.children() {
        walk(w, child, st);
    }
}

/// Lays out one element's content into its own list of blocks.
fn sub_blocks(node: ego_tree::NodeRef<Node>, st: &Style) -> (Vec<Block>, bool) {
    let mut sub = Walker::new();
    walk_children(&mut sub, node, st);
    sub.flush();
    let center = sub.blocks.iter().all(|b| match b {
        Block::Para { center, .. } | Block::Image { center, .. } => *center,
        _ => false,
    });
    (sub.blocks, center && st.center)
}

fn walk(w: &mut Walker, node: ego_tree::NodeRef<Node>, st: &Style) {
    match node.value() {
        Node::Text(t) => w.push_text(t, st),
        Node::Element(e) => {
            let Some(el) = ElementRef::wrap(node) else { return };
            let name = e.name();
            if matches!(name, "head" | "style" | "script" | "title" | "meta" | "link" | "noscript" | "template" | "svg" | "object") {
                return;
            }
            if is_hidden(&el) {
                return;
            }
            let s = element_style(&el, st);
            let style = e.attr("style").unwrap_or("").to_lowercase();
            match name {
                "br" => {
                    w.push_raw("\n", &s);
                    return;
                }
                "hr" => {
                    w.flush();
                    w.blocks.push(Block::Rule);
                    return;
                }
                "img" => {
                    let src = e.attr("src").unwrap_or("").trim().to_string();
                    let width = e.attr("width").and_then(num).or_else(|| px(&style, "width"));
                    let height = e.attr("height").and_then(num).or_else(|| px(&style, "height"));
                    if src.is_empty() || width.map(|x| x <= 3.0).unwrap_or(false) || height.map(|x| x <= 3.0).unwrap_or(false) {
                        return;
                    }
                    // icons meant to sit in the text (aligned to it, or small) stay in the line
                    let small = width.map(|x| x <= 48.0).unwrap_or(false) || height.map(|x| x <= 48.0).unwrap_or(false);
                    if style.contains("vertical-align") || small {
                        w.push_icon(src, width, &s);
                        return;
                    }
                    w.flush();
                    w.blocks.push(Block::Image { src, width, height, link: s.link.clone(), center: s.center });
                    return;
                }
                "table" => {
                    let rows = table_rows(el);
                    let max_cells = rows.iter().map(|r| r.len()).max().unwrap_or(0);
                    if max_cells >= 2 {
                        w.flush();
                        w.blocks.push(build_table(el, &rows, &s));
                        return;
                    }
                    // A layout table with one cell per row: just its content, top to bottom,
                    // in a box if it has a colour.
                    let bg = background(&el);
                    let bd = border(&el);
                    if bg.is_some() || bd.is_some() {
                        w.flush();
                        let (blocks, center) = sub_blocks(node, &s);
                        if !blocks.is_empty() {
                            w.blocks.push(Block::Boxed { blocks, bg, border: bd, center });
                        }
                        return;
                    }
                }
                "td" | "th" | "div" | "p" | "section" | "blockquote" => {
                    // Coloured or outlined blocks become boxes.
                    let bg = background(&el).filter(|c| *c != [255, 255, 255] || border(&el).is_some());
                    let bd = border(&el);
                    if (bg.is_some() || bd.is_some()) && text_len(&el) > 0 || name == "blockquote" {
                        w.flush();
                        let (blocks, center) = sub_blocks(node, &s);
                        if !blocks.is_empty() {
                            let bd = if name == "blockquote" && bd.is_none() { Some([200, 200, 200]) } else { bd };
                            w.blocks.push(Block::Boxed { blocks, bg, border: bd, center });
                        }
                        return;
                    }
                }
                _ => {}
            }
            let block = matches!(
                name,
                "p" | "div" | "table" | "tbody" | "thead" | "tfoot" | "tr" | "td" | "th" | "li" | "ul" | "ol" | "h1" | "h2" | "h3"
                    | "h4" | "h5" | "h6" | "blockquote" | "section" | "article" | "header" | "footer" | "center" | "pre" | "body"
                    | "html" | "main" | "nav" | "dl" | "dt" | "dd" | "figure" | "address"
            );
            if block {
                w.flush();
            }
            if name == "li" {
                w.push_raw("•  ", &s);
            }
            walk_children(w, node, &s);
            if block {
                w.flush();
            }
        }
        _ => walk_children(w, node, st),
    }
}

fn build_table(table: ElementRef, rows: &[Vec<ElementRef>], st: &Style) -> Block {
    let cols = rows
        .iter()
        .map(|r| r.iter().map(|c| c.value().attr("colspan").and_then(num).unwrap_or(1.0).max(1.0) as usize).sum::<usize>())
        .max()
        .unwrap_or(1)
        .max(1);
    // Column shares: an explicit width wins, otherwise the longest text in the column.
    let mut weights = vec![0.0f32; cols];
    let mut explicit = vec![None::<f32>; cols];
    for row in rows {
        let mut col = 0;
        for c in row {
            let span = c.value().attr("colspan").and_then(num).unwrap_or(1.0).max(1.0) as usize;
            if span == 1 && col < cols {
                let style = c.value().attr("style").unwrap_or("").to_lowercase();
                let wattr = c.value().attr("width").map(|w| w.trim().to_string());
                let set = match wattr.as_deref() {
                    Some(w) if w.ends_with('%') => num(w.trim_end_matches('%')).map(|p| p * 8.0),
                    Some(w) => num(w),
                    None => px(&style, "width"),
                };
                if let Some(x) = set {
                    explicit[col] = Some(explicit[col].unwrap_or(0.0).max(x));
                }
                let len = text_len(c).clamp(4, 36) as f32 * 7.5;
                // a column holding a picture (e.g. the photo in a signature) gets at least its width
                let pic = c
                    .descendants()
                    .filter_map(ElementRef::wrap)
                    .filter(|e| e.value().name() == "img")
                    .filter_map(|e| {
                        e.value().attr("width").and_then(num).or_else(|| px(&e.value().attr("style").unwrap_or("").to_lowercase(), "width"))
                    })
                    .fold(0.0f32, f32::max);
                let len = if pic > 0.0 { len.max(pic + 20.0) } else { len };
                weights[col] = weights[col].max(len);
            }
            col += span;
        }
    }
    for i in 0..cols {
        if let Some(x) = explicit[i] {
            weights[i] = x;
        }
        if weights[i] <= 0.0 {
            weights[i] = 60.0;
        }
    }
    let ts = element_style(&table, st);
    let out_rows = rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|c| {
                    let cs = element_style(c, &ts);
                    let (blocks, center) = sub_blocks(**c, &cs);
                    Cell {
                        blocks,
                        colspan: c.value().attr("colspan").and_then(num).unwrap_or(1.0).max(1.0) as usize,
                        bg: background(c).or_else(|| c.parent().and_then(ElementRef::wrap).and_then(|tr| background(&tr))),
                        border: border(c),
                        center: center || cs.center,
                    }
                })
                .collect()
        })
        .collect();
    let gap = table.value().attr("cellspacing").and_then(num).unwrap_or(0.0).min(16.0);
    // Cells with their own outline (cards) are drawn one by one; otherwise the table gets lines.
    let cells_outlined = rows.iter().flatten().any(|c| border(c).is_some()) && gap > 0.0;
    let bd = border(&table).or_else(|| if cells_outlined { None } else { rows.iter().flatten().find_map(border) });
    Block::Table { rows: out_rows, weights, border: bd, bg: background(&table), gap }
}

pub fn parse(html: &str) -> Vec<Block> {
    let doc = Html::parse_document(html);
    let mut w = Walker::new();
    let st = Style { bold: false, italic: false, link: None, size: BODY_SIZE, center: false, pre: false, color: None };
    walk(&mut w, doc.tree.root(), &st);
    w.flush();
    w.blocks
}
