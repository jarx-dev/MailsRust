//! The writing field of the message window: a real editor (the Edge engine, as for reading
//! formatted mail) placed in the message window, where everything shows as it will be sent —
//! formatting, pictures, the signature and the quoted message.
//!
//! - Formatting buttons above the text; Ctrl+B / I / U work as usual.
//! - A pasted (Ctrl+V) or dropped picture goes into the text, scaled to the message width;
//!   clicking a picture shows −/+/fit/remove and a corner to drag for any size.
//! - Text in Markdown (e.g. from an AI agent) is turned into formatting when pasted.
//! - The page tells the app its content after every change; pictures are files on disk
//!   (file:// addresses) until sending, when they are put inside the email.

use eframe::egui;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

pub enum Msg {
    /// the content now: formatted and as plain text
    Html { html: String, text: String },
    /// a picture pasted or dropped (data: address)
    Img { data: String },
    /// text in Markdown pasted, to be turned into formatting
    Md { text: String },
    /// „Popraw z AI”: the written part (pictures as [[obrazek-N]]) to correct in a style
    Ai { style: String, html: String },
    /// the title of a pasted link's page (empty: none), for link `n`
    Title { n: u64, title: String },
    /// files dropped on the field (pictures go into the text, other files are attached)
    Dropped(Vec<String>),
    /// files being dragged over the field (true) or not any more
    Drag(bool),
    /// the person clicked into the writing field (it may have the keyboard now)
    Clicked,
    /// the writing field got the keyboard without being clicked (while it loaded, or Windows
    /// gave it back): the field chosen above gets it again
    Away,
}

/// Pictures that can go into the text itself.
pub fn is_picture(path: &str) -> bool {
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    ["png", "jpg", "jpeg", "gif", "webp", "bmp"].contains(&ext.as_str()) && !path.ends_with(|c: char| c == '/' || c == '\\')
}

/// The Windows window for choosing files to attach (several at once). Empty when cancelled.
#[cfg(windows)]
pub fn pick_files(owner: Option<isize>) -> Vec<String> {
    use windows_sys::Win32::UI::Controls::Dialogs::{GetOpenFileNameW, OFN_ALLOWMULTISELECT, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_PATHMUSTEXIST, OPENFILENAMEW};
    let mut buf = vec![0u16; 65536];
    let title: Vec<u16> = "Załącz pliki".encode_utf16().chain(Some(0)).collect();
    let filter: Vec<u16> = "Wszystkie pliki\0*.*\0\0".encode_utf16().collect();
    let mut ofn: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    ofn.hwndOwner = owner.map(|h| h as *mut std::ffi::c_void).unwrap_or(std::ptr::null_mut());
    ofn.lpstrFilter = filter.as_ptr();
    ofn.lpstrFile = buf.as_mut_ptr();
    ofn.nMaxFile = buf.len() as u32;
    ofn.lpstrTitle = title.as_ptr();
    ofn.Flags = OFN_ALLOWMULTISELECT | OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST;
    if unsafe { GetOpenFileNameW(&mut ofn) } == 0 {
        return vec![];
    }
    // one file: its full path; several: the folder, then the names, each ending with a zero
    let parts: Vec<String> = buf.split(|c| *c == 0).take_while(|p| !p.is_empty()).map(String::from_utf16_lossy).collect();
    match parts.as_slice() {
        [] => vec![],
        [one] => vec![one.clone()],
        [dir, names @ ..] => names.iter().map(|n| std::path::Path::new(dir).join(n).to_string_lossy().into_owned()).collect(),
    }
}

#[cfg(not(windows))]
pub fn pick_files(_: Option<isize>) -> Vec<String> {
    vec![]
}

pub struct ComposeView {
    view: wry::WebView,
    _context: wry::WebContext,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    last: Option<egui::Rect>,
    visible: bool,
    /// the app asks the page for news a few times a second: how many times it asked, how
    /// many answers came back, and whether an answer is on its way
    asked: u64,
    done: Arc<AtomicU64>,
    in_flight: Arc<AtomicBool>,
    last_ask: std::time::Instant,
    ctx: egui::Context,
}

/// An invisible tag in a message window's title, so its window can be found.
pub fn marker(key: u64) -> String {
    (0..10).map(|b| if (key >> b) & 1 == 1 { '\u{200C}' } else { '\u{200B}' }).collect()
}

/// The message window with this tag in its title (a window of this program).
#[cfg(windows)]
pub fn find_window(marker: &str) -> Option<isize> {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW, GetWindowThreadProcessId};
    struct Search {
        marker: Vec<u16>,
        found: isize,
        pid: u32,
    }
    unsafe extern "system" fn each(hwnd: HWND, l: LPARAM) -> i32 {
        let s = unsafe { &mut *(l as *mut Search) };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if pid != s.pid {
            return 1;
        }
        let mut buf = [0u16; 512];
        let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) } as usize;
        let title = &buf[..n];
        if title.len() >= s.marker.len() && title.ends_with(&s.marker) {
            s.found = hwnd as isize;
            return 0;
        }
        1
    }
    let mut s = Search { marker: marker.encode_utf16().collect(), found: 0, pid: unsafe { GetCurrentProcessId() } };
    unsafe { EnumWindows(Some(each), &mut s as *mut Search as LPARAM) };
    (s.found != 0).then_some(s.found)
}

#[cfg(not(windows))]
pub fn find_window(_marker: &str) -> Option<isize> {
    None
}

pub struct Parent(pub isize);

impl raw_window_handle::HasWindowHandle for Parent {
    fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        let hwnd = std::num::NonZeroIsize::new(self.0).ok_or(raw_window_handle::HandleError::Unavailable)?;
        let raw = raw_window_handle::RawWindowHandle::Win32(raw_window_handle::Win32WindowHandle::new(hwnd));
        Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(raw) })
    }
}

fn bounds(r: egui::Rect) -> wry::Rect {
    wry::Rect {
        position: wry::dpi::LogicalPosition::new(r.left() as f64, r.top() as f64).into(),
        size: wry::dpi::LogicalSize::new(r.width().max(1.0) as f64, r.height().max(1.0) as f64).into(),
    }
}

pub fn file_url(path: &str) -> String {
    let s = path.replace('\\', "/");
    let mut out = String::from("file:///");
    for c in s.chars() {
        match c {
            ' ' => out.push_str("%20"),
            '#' => out.push_str("%23"),
            '%' => out.push_str("%25"),
            '"' => out.push_str("%22"),
            _ => out.push(c),
        }
    }
    out
}

fn url_to_path(url: &str) -> String {
    let raw = url.trim_start_matches("file:///");
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).replace('/', "\\")
}

impl ComposeView {
    /// Puts the editor into the message window `hwnd`, showing `page`.
    pub fn new(hwnd: isize, page: &str, ctx: egui::Context) -> Result<Self, String> {
        match Self::new_in(hwnd, page, ctx.clone(), crate::webview::data_dir(false)) {
            Err(e) if crate::webview::busy(&e) => {
                crate::diag::log("writing field: browser folder in use by another copy of the app, using a second one");
                Self::new_in(hwnd, page, ctx, crate::webview::data_dir(true))
            }
            other => other,
        }
    }

    fn new_in(hwnd: isize, page: &str, ctx: egui::Context, dir: Option<std::path::PathBuf>) -> Result<Self, String> {
        let file = crate::model::cache_put(page.as_bytes(), "html");
        let mut context = wry::WebContext::new(dir);
        let (tx, rx) = channel();
        let start_url = file_url(&file);
        let (drop_tx, drop_ctx) = (tx.clone(), ctx.clone());
        let (nav_tx, nav_ctx) = (tx.clone(), ctx.clone());
        let start = start_url.clone();
        let view = wry::WebViewBuilder::new_with_web_context(&mut context)
            .with_url(start_url)
            // made shown but outside the window, and never hidden (WebView2 takes the keyboard
            // when a hidden view is shown again): moved in and out instead
            .with_visible(true)
            // the writing field takes the keyboard only when clicked (or asked to): never by itself
            // when it is made or has loaded, so typing in "Do" goes on undisturbed
            .with_focused(false)
            .with_bounds(bounds(crate::webview::away()))
            .with_background_color(if crate::theme::is_dark() { (0x1E, 0x1F, 0x22, 255) } else { (255, 255, 255, 255) })
            // the page never goes anywhere else; links open in the web browser
            .with_navigation_handler(move |url| {
                if url == start || url.starts_with("about:") {
                    return true;
                }
                // a click inside the page (see its script): only a note, the page stays
                if url.starts_with("pzfocus:") {
                    let _ = nav_tx.send(if url.contains("away") { Msg::Away } else { Msg::Clicked });
                    nav_ctx.request_repaint();
                    return false;
                }
                if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
                    let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", &url]).spawn();
                }
                false
            })
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            // files dragged in from outside come here (with their paths): pictures into the text,
            // anything else attached to the message
            .with_drag_drop_handler(move |e| {
                let msg = match e {
                    wry::DragDropEvent::Enter { .. } => Msg::Drag(true),
                    wry::DragDropEvent::Leave => Msg::Drag(false),
                    wry::DragDropEvent::Drop { paths, .. } => {
                        let _ = drop_tx.send(Msg::Drag(false));
                        Msg::Dropped(paths.iter().filter(|p| p.is_file()).map(|p| p.to_string_lossy().into_owned()).collect())
                    }
                    wry::DragDropEvent::Over { .. } => return true,
                    _ => return true,
                };
                let _ = drop_tx.send(msg);
                drop_ctx.request_repaint();
                true
            })
            .build_as_child(&Parent(hwnd))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            view,
            _context: context,
            tx,
            rx,
            last: None,
            visible: false,
            asked: 0,
            done: Arc::new(AtomicU64::new(0)),
            in_flight: Arc::new(AtomicBool::new(false)),
            last_ask: std::time::Instant::now(),
            ctx,
        })
    }

    /// Asks the page what happened since last time (typing, pasted pictures, Markdown), a few
    /// times a second. `now`: right away (e.g. before sending).
    pub fn poll(&mut self, now: bool) {
        if self.in_flight.load(Ordering::Relaxed) || (!now && self.last_ask.elapsed() < std::time::Duration::from_millis(150)) {
            return;
        }
        self.asked += 1;
        let seq = self.asked;
        self.last_ask = std::time::Instant::now();
        self.in_flight.store(true, Ordering::Relaxed);
        let (tx, done, flight, ctx) = (self.tx.clone(), self.done.clone(), self.in_flight.clone(), self.ctx.clone());
        let asked = self.view.evaluate_script_with_callback("window.pzTake ? window.pzTake() : '[]'", move |answer: String| {
            // the answer is the page's text, written as JSON
            let list = serde_json::from_str::<String>(&answer)
                .ok()
                .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(&s).ok())
                .unwrap_or_default();
            for v in list {
                let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
                let msg = match v["t"].as_str() {
                    Some("html") => Msg::Html { html: s("html"), text: s("text") },
                    Some("img") => Msg::Img { data: s("data") },
                    Some("md") => Msg::Md { text: s("text") },
                    Some("ai") => Msg::Ai { style: s("style"), html: s("html") },
                    // a pasted link: its page's title is fetched meanwhile
                    Some("link") => {
                        let (url, n, tx, ctx) = (s("url"), v["n"].as_u64().unwrap_or(0), tx.clone(), ctx.clone());
                        std::thread::spawn(move || {
                            let _ = tx.send(Msg::Title { n, title: crate::net::page_title(&url).unwrap_or_default() });
                            ctx.request_repaint();
                        });
                        continue;
                    }
                    _ => continue,
                };
                let _ = tx.send(msg);
            }
            done.store(seq, Ordering::Relaxed);
            flight.store(false, Ordering::Relaxed);
            ctx.request_repaint();
        });
        if asked.is_err() {
            self.in_flight.store(false, Ordering::Relaxed);
        }
    }

    /// The number the next question will have (to wait for its answer).
    pub fn next_question(&self) -> u64 {
        self.asked + 1
    }

    /// The last question answered.
    pub fn answered(&self) -> u64 {
        self.done.load(Ordering::Relaxed)
    }

    /// Shows the editor over `rect` of the message window (None: hidden, e.g. while a list of
    /// addresses is open above it).
    pub fn place(&mut self, rect: Option<egui::Rect>) {
        match rect {
            Some(r) => {
                if self.last != Some(r) {
                    let _ = self.view.set_bounds(bounds(r));
                    self.last = Some(r);
                }
                self.visible = true;
            }
            None => {
                // moved out of sight (not hidden)
                if self.visible {
                    let _ = self.view.set_bounds(bounds(crate::webview::away()));
                    self.last = None;
                    self.visible = false;
                }
            }
        }
    }

    /// The keyboard into the writing field, on purpose (not handed back to the fields above).
    pub fn focus(&self) {
        let _ = self.view.evaluate_script("window.pzWant&&window.pzWant()");
        let _ = self.view.focus();
        // and the text cursor into the text itself (not just the page)
        let _ = self.view.evaluate_script("window.pzWant&&window.pzWant();var e=document.getElementById('pz-ed');if(e&&document.activeElement!==e){e.focus();}");
    }

    pub fn messages(&self) -> Vec<Msg> {
        self.rx.try_iter().collect()
    }

    /// The corrected text back in the field (or why it couldn't be corrected).
    pub fn ai_done(&self, result: &Result<String, String>) {
        let js = match result {
            Ok(h) => format!("window.pzAiDone({})", serde_json::to_string(h).unwrap_or_default()),
            Err(e) => format!("window.pzAiFail({})", serde_json::to_string(e).unwrap_or_default()),
        };
        let _ = self.view.evaluate_script(&js);
    }

    /// Runs a piece of the page's script (tests).
    pub fn run_js(&self, js: &str) {
        let _ = self.view.evaluate_script(js);
    }

    /// A pasted link gets its page's title (or keeps the short address when `title` is empty).
    pub fn link_title(&self, n: u64, title: &str) {
        let _ = self.view.evaluate_script(&format!("window.pzTitle({n},{})", serde_json::to_string(title).unwrap_or_default()));
    }

    /// A picture of the writing field (PNG) into `out` (shown under a question asked over it).
    pub fn capture(&self, out: Arc<std::sync::Mutex<Option<Vec<u8>>>>) {
        let ctx = self.ctx.clone();
        crate::webview::capture_png(&self.view, move |png| {
            if let Ok(mut g) = out.lock() {
                *g = Some(png);
            }
            ctx.request_repaint();
        });
    }

    /// Puts formatted text in at the cursor.
    pub fn insert_html(&self, html: &str) {
        let js = format!("window.pzInsertHtml({})", serde_json::to_string(html).unwrap_or_default());
        let _ = self.view.evaluate_script(&js);
    }

    /// Puts a picture (a file on disk) in at the cursor, `width` wide.
    pub fn insert_image(&self, path: &str, width: u32) {
        let js = format!("window.pzInsertImg({}, {width})", serde_json::to_string(&file_url(path)).unwrap_or_default());
        let _ = self.view.evaluate_script(&js);
    }
}

/// A picture sent by the page (data: address) saved as a file: (file, width to show it at).
pub fn save_data_image(data: &str) -> Option<(String, u32)> {
    use base64::Engine;
    let (head, b64) = data.split_once(',')?;
    let ext = head.split('/').nth(1).and_then(|x| x.split(';').next()).unwrap_or("png");
    let ext = if ext == "jpeg" { "jpg" } else { ext };
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?;
    let (_, file, w) = crate::rich::store_picture(&bytes, ext)?;
    Some((file, w))
}

/// The written message ready to send: pictures from disk (and any data: pictures) put inside
/// the email under content ids. Returns the HTML and (content id, file) for each picture.
pub fn to_email(html: &str) -> (String, Vec<(String, String)>) {
    use base64::Engine;
    let mut out = String::with_capacity(html.len());
    let mut inline: Vec<(String, String)> = Vec::new();
    let mut seen: std::collections::HashMap<String, String> = Default::default();
    let mut rest = html;
    while let Some(i) = rest.find("src=\"") {
        out.push_str(&rest[..i + 5]);
        let after = &rest[i + 5..];
        let Some(end) = after.find('"') else {
            rest = after;
            break;
        };
        let url = &after[..end];
        let replaced = if url.starts_with("file:///") || url.starts_with("data:image/") {
            if let Some(cid) = seen.get(url) {
                Some(cid.clone())
            } else {
                let file = if url.starts_with("file:///") {
                    Some(url_to_path(url)).filter(|p| std::path::Path::new(p).exists())
                } else {
                    url.split_once(',').and_then(|(h, b)| {
                        let ext = h.split('/').nth(1).and_then(|x| x.split(';').next()).unwrap_or("png").to_string();
                        base64::engine::general_purpose::STANDARD.decode(b.trim()).ok().map(|bytes| crate::model::cache_put(&bytes, &ext))
                    })
                };
                file.map(|f| {
                    let cid = format!("img{}-{}@poczta", inline.len() + 1, crate::share::new_token().get(..10).unwrap_or("x"));
                    inline.push((cid.clone(), f));
                    seen.insert(url.to_string(), cid.clone());
                    cid
                })
            }
        } else {
            None
        };
        match replaced {
            Some(cid) => out.push_str(&format!("cid:{cid}")),
            None => out.push_str(url),
        }
        rest = &after[end..];
    }
    out.push_str(rest);
    let out = out.replace(" pz-sel", "").replace("class=\"pz-sel\"", "");
    (format!("<div style=\"font-family:'Segoe UI',Arial,sans-serif;font-size:14px;color:#222\">{out}</div>"), inline)
}

/// Pictures of another message shown from their files (cid: → file://), for quoting it.
pub fn show_cids(html: &str, inline: &[(String, String)]) -> String {
    let mut h = html.to_string();
    for (cid, file) in inline {
        let url = file_url(file);
        for p in ["cid:", "CID:", "Cid:"] {
            h = h.replace(&format!("{p}{cid}"), &url);
        }
    }
    h
}

/// Only the inside of an email's page (no scripts, no page head).
pub fn body_of(html: &str) -> String {
    let lower = html.to_lowercase();
    let mut inner = match (lower.find("<body"), lower.rfind("</body>")) {
        (Some(a), Some(b)) if a < b => {
            let start = lower[a..].find('>').map(|x| a + x + 1).unwrap_or(a);
            html[start..b].to_string()
        }
        _ => html.to_string(),
    };
    // the email's own look stays (its styles), its scripts don't
    let styles: String = {
        let mut s = String::new();
        let mut from = 0;
        while let Some(i) = lower[from..].find("<style") {
            let a = from + i;
            let Some(e) = lower[a..].find("</style>") else { break };
            s.push_str(&html[a..a + e + 8]);
            from = a + e + 8;
        }
        s
    };
    loop {
        let l = inner.to_lowercase();
        let Some(a) = l.find("<script") else { break };
        let b = l[a..].find("</script>").map(|x| a + x + 9).unwrap_or(inner.len());
        inner.replace_range(a..b, "");
    }
    format!("{styles}{inner}")
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The editor page: `content` is what the field starts with (text, signature, quoted message).
pub fn page(content: &str) -> String {
    format!(
        r##"<!doctype html><html lang="pl"><head><meta charset="utf-8"><meta name="color-scheme" content="light">
<style>
html,body{{margin:0;background:#fff;font-family:'Segoe UI',Arial,sans-serif;font-size:14px;color:#222;color-scheme:light}}
#pz-bar{{position:sticky;top:0;z-index:30;display:flex;flex-wrap:wrap;align-items:center;gap:1px;padding:4px 6px;background:#fff;border-bottom:1px solid #e6e8eb;user-select:none;font-family:'Segoe UI',Arial,sans-serif}}
#pz-bar button{{border:0;background:transparent;min-width:30px;height:28px;padding:0 6px;border-radius:5px;font-family:inherit;font-size:15px;color:#333;cursor:pointer}}
#pz-bar button:hover{{background:#eef1f5}}
#pz-bar .sep{{width:1px;height:18px;background:#e3e5e8;margin:0 6px}}
#pz-bar .hint{{margin-left:auto;color:#9aa0a6;font-size:11.5px;padding-right:4px}}
@media (max-width:820px){{#pz-bar .hint{{display:none}}}}
#pz-colors{{position:fixed;display:none;z-index:40;background:#fff;border:1px solid #d9dce0;border-radius:8px;box-shadow:0 4px 14px rgba(0,0,0,.14);padding:6px;gap:4px}}
#pz-colors span{{display:inline-block;width:20px;height:20px;border-radius:4px;cursor:pointer;border:1px solid rgba(0,0,0,.1)}}
#pz-ed{{padding:12px 16px 60px;outline:none;min-height:calc(100vh - 37px);box-sizing:border-box;word-wrap:break-word;line-height:1.45}}
#pz-ed img{{max-width:100%;height:auto}}
#pz-ed img.pz-sel{{outline:2px solid #1e6fd9;outline-offset:1px}}
#pz-ed h1,#pz-ed h2,#pz-ed h3{{margin:.5em 0 .3em;line-height:1.25}}
#pz-ed ul,#pz-ed ol{{margin:.3em 0;padding-left:1.6em}}
#pz-ed pre{{background:#f4f5f7;padding:8px 10px;border-radius:6px;white-space:pre-wrap}}
#pz-ed code{{background:#f0f1f3;padding:1px 4px;border-radius:4px;font-family:Consolas,monospace}}
#pz-ed table.md{{border-collapse:collapse}} #pz-ed table.md td,#pz-ed table.md th{{border:1px solid #d6d9dd;padding:4px 8px}}
#pz-ed blockquote{{margin:.4em 0;padding-left:10px;border-left:3px solid #d6d9dd;color:#555}}
.pz-quote{{margin-top:18px;padding-top:8px;border-top:1px solid #e6e8eb}}
.pz-quote-head{{color:#666;font-size:12.5px;margin-bottom:8px}}
#pz-img{{position:absolute;display:none;z-index:25;align-items:center;gap:2px;background:#fff;border:1px solid #d9dce0;border-radius:7px;box-shadow:0 2px 10px rgba(0,0,0,.14);padding:3px;font-family:'Segoe UI',Arial,sans-serif;user-select:none}}
#pz-img button{{border:0;background:transparent;height:26px;min-width:26px;padding:0 7px;border-radius:5px;font-size:13px;cursor:pointer;color:#333}}
#pz-img button:hover{{background:#eef1f5}}
#pz-w{{font-size:12px;color:#777;padding:0 6px;min-width:52px;text-align:center}}
#pz-ai{{color:#6a3fd1!important;font-size:13px!important;font-weight:600}}
#pz-ai.busy{{color:#888!important;cursor:wait}}
#pz-undo{{display:none;font-size:13px!important;color:#1e6fd9!important}}
#pz-aimenu{{position:fixed;display:none;flex-direction:column;z-index:40;width:270px;background:#fff;border:1px solid #d9dce0;border-radius:8px;box-shadow:0 4px 14px rgba(0,0,0,.14);padding:4px;font-family:'Segoe UI',Arial,sans-serif;user-select:none}}
#pz-aimenu .head{{font-size:11.5px;color:#888;padding:4px 8px 6px}}
#pz-aimenu .it{{display:flex;gap:8px;padding:6px 8px;border-radius:6px;cursor:pointer}}
#pz-aimenu .it:hover{{background:#eef1f5}}
#pz-aimenu .ck{{width:14px;color:#1e6fd9;font-size:13px}}
#pz-aimenu .l{{font-size:12.5px;font-weight:600;color:#222}}
#pz-aimenu .h{{font-size:11.5px;color:#777}}
#pz-drop{{position:fixed;display:none;inset:6px;z-index:60;border:2px dashed #1e6fd9;border-radius:10px;background:rgba(240,246,255,.94);color:#1e6fd9;font:600 15px 'Segoe UI',Arial,sans-serif;align-items:center;justify-content:center;text-align:center;pointer-events:none}}
#pz-drop small{{display:block;font-weight:400;font-size:12.5px;color:#4a6fa5;margin-top:4px}}
#pz-toast{{position:fixed;display:none;left:12px;right:12px;bottom:12px;z-index:50;background:#fdecea;color:#8a1c12;border:1px solid #f3c2bc;border-radius:7px;padding:8px 12px;font-size:12.5px;font-family:'Segoe UI',Arial,sans-serif}}
#pz-ed[contenteditable=false]{{opacity:.55;cursor:wait}}
#pz-handle{{position:absolute;display:none;z-index:26;width:12px;height:12px;background:#1e6fd9;border:2px solid #fff;border-radius:3px;cursor:nwse-resize;box-shadow:0 0 0 1px #1e6fd9}}
{dark}
::-webkit-scrollbar{{width:10px;height:10px}}::-webkit-scrollbar-thumb{{background:#cfd3d8;border-radius:5px;border:2px solid #fff}}::-webkit-scrollbar-thumb:hover{{background:#aeb4bb}}::-webkit-scrollbar-button{{display:none}}
</style></head><body>
<div id="pz-bar">
<button data-c="bold" title="Pogrubienie (Ctrl+B)"><b>B</b></button>
<button data-c="italic" title="Kursywa (Ctrl+I)"><i>I</i></button>
<button data-c="underline" title="Podkreślenie (Ctrl+U)"><u>U</u></button>
<button data-c="strikeThrough" title="Przekreślenie"><s>S</s></button>
<span class="sep"></span>
<button data-size="-1" title="Mniejszy tekst" style="font-size:12px">A−</button>
<button data-size="1" title="Większy tekst" style="font-size:17px">A+</button>
<button id="pz-col" title="Kolor tekstu" style="color:#c62828;font-weight:700;text-decoration:underline">A</button>
<span class="sep"></span>
<button data-block="h2" title="Nagłówek" style="font-weight:700">H</button>
<button data-c="insertUnorderedList" title="Lista">• ≡</button>
<button data-c="insertOrderedList" title="Lista numerowana">1.</button>
<button id="pz-link" title="Wstaw link">🔗</button>
<button data-c="removeFormat" title="Usuń formatowanie" style="font-size:13px">T̸</button>
<span class="sep"></span>
<button id="pz-ai" title="Popraw z AI: błędy i styl tekstu (podpis i cytat zostają bez zmian)">✨ Popraw z AI ▾</button>
<button id="pz-undo" title="Przywróć tekst sprzed poprawki">↶ Cofnij poprawkę</button>
<span class="hint">obraz: Ctrl+V lub przeciągnij · Markdown → format</span>
</div>
<div id="pz-colors"></div>
<div id="pz-aimenu"><div class="head">Popraw z AI — styl</div>{aimenu}</div>
<div id="pz-toast"></div>
<div id="pz-drop"><div>📎 Upuść pliki tutaj<small>obrazy trafią do treści, pozostałe pliki zostaną załączone</small></div></div>
<div id="pz-ed" contenteditable="true" spellcheck="true" lang="pl">{content}</div>
<div id="pz-img"><button data-w="-" title="Mniejszy">−</button><span id="pz-w"></span><button data-w="+" title="Większy">+</button><button data-w="fit" title="Dopasuj do szerokości wiadomości">Dopasuj</button><button data-w="orig" title="Oryginalny rozmiar">100%</button><button data-w="del" title="Usuń obraz">🗑</button></div>
<div id="pz-handle" title="Przeciągnij, aby zmienić rozmiar"></div>
<script>
var pzPtr=0;
document.addEventListener('pointerdown',function(){{pzPtr=Date.now();try{{location.href='pzfocus://in'}}catch(e){{}}}},true);
window.pzWant=function(){{pzPtr=Date.now();}};
window.addEventListener('focus',function(){{var at=Date.now();setTimeout(function(){{if(Math.abs(pzPtr-at)>800&&document.hasFocus()){{try{{location.href='pzfocus://away'}}catch(e){{}}}}}},250);}});
(function(){{
const ed=document.getElementById('pz-ed'),bar=document.getElementById('pz-bar'),box=document.getElementById('pz-img'),handle=document.getElementById('pz-handle'),cols=document.getElementById('pz-colors');
const MAXW={maxw};
try{{document.execCommand('styleWithCSS',false,true);document.execCommand('defaultParagraphSeparator',false,'div');}}catch(e){{}}
let Q=[];
function post(o){{if(o.t==='html')Q=Q.filter(x=>x.t!=='html');Q.push(o);}}
window.pzDrag=function(on){{document.getElementById('pz-drop').style.display=on?'flex':'none';}};
window.pzTake=function(){{const q=Q;Q=[];return JSON.stringify(q);}};
function sync(){{post({{t:'html',html:ed.innerHTML,text:ed.innerText}});}}
let saved=null;
function saveSel(){{const s=getSelection();if(s.rangeCount&&ed.contains(s.anchorNode))saved=s.getRangeAt(0).cloneRange();}}
function restore(){{ed.focus();if(saved){{const s=getSelection();s.removeAllRanges();s.addRange(saved);}}}}
document.addEventListener('selectionchange',saveSel);
ed.addEventListener('input',()=>{{sync();place();}});
function fitWidth(){{return Math.max(80,ed.clientWidth-40);}}
window.pzInsertHtml=function(h){{restore();document.execCommand('insertHTML',false,h);sync();}};
window.pzInsertImg=function(src,w){{restore();w=Math.min(w,MAXW,fitWidth());document.execCommand('insertHTML',false,'<img src="'+src+'" width="'+w+'" style="width:'+w+'px;max-width:100%;height:auto"><br>');sync();}};
bar.addEventListener('mousedown',e=>{{if(e.target.closest('button'))e.preventDefault();}});
bar.addEventListener('click',e=>{{
 const b=e.target.closest('button');if(!b||b.id==='pz-ai'||b.id==='pz-undo')return;restore();
 if(b.dataset.c){{document.execCommand(b.dataset.c,false,null);}}
 else if(b.dataset.size){{let cur=parseInt(document.queryCommandValue('fontSize'))||3;document.execCommand('fontSize',false,Math.max(1,Math.min(7,cur+parseInt(b.dataset.size))));}}
 else if(b.dataset.block){{const cur=(document.queryCommandValue('formatBlock')||'').toLowerCase();document.execCommand('formatBlock',false,cur===b.dataset.block?'div':b.dataset.block);}}
 else if(b.id==='pz-link'){{const u=prompt('Adres linku:','https://');if(u)document.execCommand('createLink',false,u);}}
 else if(b.id==='pz-col'){{const r=b.getBoundingClientRect();cols.style.left=r.left+'px';cols.style.top=(r.bottom+4)+'px';cols.style.display=cols.style.display==='flex'?'none':'flex';return;}}
 sync();
}});
['#222222','#c62828','#1e6fd9','#2e7d32','#e65100','#6a1b9a','#757575','#00897b','#ad1457'].forEach(c=>{{const s=document.createElement('span');s.style.background=c;s.title=c;s.addEventListener('mousedown',e=>{{e.preventDefault();restore();document.execCommand('foreColor',false,c);cols.style.display='none';sync();}});cols.appendChild(s);}});
document.addEventListener('mousedown',e=>{{if(!e.target.closest('#pz-colors')&&!e.target.closest('#pz-col'))cols.style.display='none';if(!e.target.closest('#pz-aimenu')&&!e.target.closest('#pz-ai'))aimenu.style.display='none';}});
// ---- „Popraw z AI”: only the written part goes (not the signature, not the quoted message);
// pictures travel as [[obrazek-N]] and come back to their places; the field waits meanwhile
const aibtn=document.getElementById('pz-ai'),undo=document.getElementById('pz-undo'),aimenu=document.getElementById('pz-aimenu'),toast=document.getElementById('pz-toast');
let aiBusy=false,aiBefore=null,aiImgs=[],toastT=0;
function isTail(n){{return n.nodeType===1&&(n.classList.contains('pz-sig')||n.classList.contains('pz-quote'));}}
function ownNodes(){{const out=[];for(const n of Array.from(ed.childNodes)){{if(isTail(n))break;out.push(n);}}return out;}}
function ownHtml(){{const d=document.createElement('div');ownNodes().forEach(n=>d.appendChild(n.cloneNode(true)));return d;}}
function clean(root){{root.querySelectorAll('script,style,iframe,object,embed,link,meta').forEach(x=>x.remove());root.querySelectorAll('*').forEach(el=>{{for(const a of Array.from(el.attributes)){{if(/^on/i.test(a.name)||(/^(href|src)$/i.test(a.name)&&/^\s*javascript:/i.test(a.value)))el.removeAttribute(a.name);}}}});}}
function setOwn(h){{ownNodes().forEach(n=>n.remove());const t=document.createElement('template');t.innerHTML=h;clean(t.content);const anchor=Array.from(ed.childNodes).find(isTail)||null;ed.insertBefore(t.content,anchor);
 if(anchor){{const p=anchor.previousSibling;const empty=p&&p.nodeType===1&&p.tagName==='DIV'&&!p.textContent.trim()&&!p.querySelector('img');if(!empty)ed.insertBefore(Object.assign(document.createElement('div'),{{innerHTML:'<br>'}}),anchor);}}}}
function say(m){{toast.textContent=m;toast.style.display='block';clearTimeout(toastT);toastT=setTimeout(()=>{{toast.style.display='none';}},8000);}}
function busy(on){{aiBusy=on;ed.contentEditable=on?'false':'true';aibtn.classList.toggle('busy',on);aibtn.textContent=on?'⏳ Poprawiam…':'✨ Popraw z AI ▾';}}
aibtn.addEventListener('click',()=>{{if(aiBusy)return;if(aimenu.style.display==='flex'){{aimenu.style.display='none';return;}}const r=aibtn.getBoundingClientRect();aimenu.style.left=Math.min(r.left,innerWidth-280)+'px';aimenu.style.top=(r.bottom+4)+'px';aimenu.style.display='flex';}});
aimenu.addEventListener('click',e=>{{const it=e.target.closest('.it');if(!it)return;aimenu.style.display='none';
 aimenu.querySelectorAll('.ck').forEach(c=>c.textContent='');it.querySelector('.ck').textContent='✓';
 const d=ownHtml();if(!d.innerText.trim()){{say('Najpierw napisz tekst do poprawienia.');return;}}
 aiBefore=d.innerHTML;aiImgs=[];d.querySelectorAll('img').forEach((im,i)=>{{im.classList.remove('pz-sel');aiImgs.push(im.outerHTML);im.replaceWith('[['+'obrazek-'+(i+1)+']]');}});
 toast.style.display='none';select(null);busy(true);Q.push({{t:'ai',style:it.dataset.s,html:d.innerHTML}});}});
const img=n=>aiImgs[Number(n)-1]||'';
window.pzAiDone=function(h){{busy(false);
 h=h.replace(/<img\b[^>]*>/gi,'').replace(/<p>\s*\[\[obrazek-(\d+)\]\]\s*<\/p>/g,(_,n)=>img(n)).replace(/\[\[obrazek-(\d+)\]\]/g,(_,n)=>img(n));
 setOwn(h);undo.style.display='inline-block';sync();ed.focus();}};
window.pzAiFail=function(m){{busy(false);aiBefore=null;say('Nie udało się poprawić tekstu: '+m);}};
undo.addEventListener('click',()=>{{if(aiBefore!==null){{setOwn(aiBefore);aiBefore=null;sync();}}undo.style.display='none';}});
ed.addEventListener('input',()=>{{if(!aiBusy){{undo.style.display='none';aiBefore=null;}}}});
function looksMd(s){{return /(^|\n)\s{{0,3}}(#{{1,6}}\s|[-*+]\s|\d+\.\s|>\s|```)|\*\*[^*\n]+\*\*|__[^_\n]+__|\[[^\]\n]+\]\([^)\n]+\)|\|\s*:?-{{3,}}/.test(s);}}
function readImg(f){{const r=new FileReader();r.onload=()=>{{post({{t:'img',data:r.result}});}};r.readAsDataURL(f);}}
let linkN=0;
function shortUrl(u){{try{{const x=new URL(u);let p=x.pathname.replace(/\/$/,'');try{{p=decodeURI(p);}}catch(e){{}}const s=x.host.replace(/^www\./,'')+p;return s.length>50?s.slice(0,49)+'…':s;}}catch(e){{return u;}}}}
// the page's title in place of the short address (unless the person changed the link's words)
window.pzTitle=function(n,t){{const a=ed.querySelector('a[data-pz-t="'+n+'"]');if(!a)return;a.removeAttribute('data-pz-t');
 if(t&&a.textContent===shortUrl(a.getAttribute('href')))a.textContent=t;sync();}};
ed.addEventListener('paste',e=>{{
 const cd=e.clipboardData;if(!cd)return;saveSel();
 const imgs=[...cd.items].filter(i=>i.kind==='file'&&i.type.startsWith('image/'));
 if(imgs.length){{e.preventDefault();imgs.forEach(i=>readImg(i.getAsFile()));return;}}
 if(cd.getData('text/html'))return;
 const t=cd.getData('text/plain');
 // a pasted address becomes a short link (the page's title once it comes, as in Edge); over
 // selected words, those words become the link
 const u=t&&t.trim();
 if(u&&/^https?:\/\/\S+$/i.test(u)){{e.preventDefault();
  if(!getSelection().isCollapsed){{document.execCommand('createLink',false,u);return;}}
  const n=++linkN,a=document.createElement('a');a.href=u;a.dataset.pzT=n;a.textContent=shortUrl(u);
  document.execCommand('insertHTML',false,a.outerHTML+'&nbsp;');post({{t:'link',url:u,n:n}});return;}}
 if(t&&looksMd(t)){{e.preventDefault();post({{t:'md',text:t}});return;}}
 if(t){{e.preventDefault();document.execCommand('insertText',false,t);}}
}});
document.addEventListener('dragover',e=>e.preventDefault());
document.addEventListener('drop',e=>{{
 e.preventDefault();const fs=[...((e.dataTransfer&&e.dataTransfer.files)||[])].filter(f=>f.type.startsWith('image/'));
 if(!fs.length)return;
 let r=document.caretRangeFromPoint?document.caretRangeFromPoint(e.clientX,e.clientY):null;
 if(r&&ed.contains(r.startContainer)){{saved=r;}}
 fs.forEach(readImg);
}});
let sel=null;
function select(img){{if(sel)sel.classList.remove('pz-sel');sel=img;if(img)img.classList.add('pz-sel');place();}}
ed.addEventListener('click',e=>{{select(e.target.tagName==='IMG'?e.target:null);}});
function place(){{
 if(!sel||!document.body.contains(sel)){{box.style.display='none';handle.style.display='none';sel=null;return;}}
 const r=sel.getBoundingClientRect(),x=scrollX,y=scrollY;
 box.style.display='flex';
 const top=r.top>80?r.top+y-40:r.bottom+y+6;
 box.style.left=Math.max(4,r.left+x)+'px';box.style.top=top+'px';
 handle.style.display='block';handle.style.left=(r.right+x-8)+'px';handle.style.top=(r.bottom+y-8)+'px';
 document.getElementById('pz-w').textContent=Math.round(r.width)+' px';
}}
window.addEventListener('scroll',place);window.addEventListener('resize',place);
function setW(w){{if(!sel)return;w=Math.max(40,Math.round(w));sel.setAttribute('width',w);sel.removeAttribute('height');sel.style.width=w+'px';sel.style.height='auto';place();sync();}}
box.addEventListener('mousedown',e=>e.preventDefault());
box.addEventListener('click',e=>{{
 const b=e.target.closest('button');if(!b||!sel)return;const w=sel.getBoundingClientRect().width;
 if(b.dataset.w==='-')setW(w*0.8);else if(b.dataset.w==='+')setW(w*1.25);
 else if(b.dataset.w==='fit')setW(Math.min(sel.naturalWidth||w,fitWidth()));
 else if(b.dataset.w==='orig')setW(sel.naturalWidth||w);
 else if(b.dataset.w==='del'){{sel.remove();select(null);sync();}}
}});
let drag=null;
handle.addEventListener('mousedown',e=>{{e.preventDefault();if(sel)drag={{x:e.clientX,w:sel.getBoundingClientRect().width}};}});
document.addEventListener('mousemove',e=>{{if(drag)setW(drag.w+(e.clientX-drag.x));}});
document.addEventListener('mouseup',()=>{{drag=null;}});
ed.addEventListener('keydown',e=>{{if(sel&&(e.key==='Delete'||e.key==='Backspace')){{e.preventDefault();sel.remove();select(null);sync();}}}});
window.addEventListener('load',()=>{{ed.querySelectorAll('.pz-sig,.pz-quote,blockquote').forEach(x=>x.setAttribute('spellcheck','false'));sync();const r=document.createRange();r.setStart(ed,0);r.collapse(true);saved=r;}});
}})();
</script></body></html>"##,
        content = content,
        maxw = crate::rich::DEFAULT_WIDTH,
        aimenu = {
            let last = crate::ai::load().style;
            crate::ai::STYLES
                .iter()
                .map(|(k, l, h, _)| {
                    let tick = if *k == last { "✓" } else { "" };
                    format!("<div class=\"it\" data-s=\"{k}\"><span class=\"ck\">{tick}</span><span><span class=\"l\" style=\"display:block\">{}</span><span class=\"h\" style=\"display:block\">{}</span></span></div>", esc(l), esc(h))
                })
                .collect::<String>()
        },
        // dark theme: the page's colours turned around, pictures and the colour swatches back
        dark = if crate::theme::is_dark() { "html{{background:#e1e0dd!important;filter:invert(1) hue-rotate(180deg)}}img,video,picture,canvas,svg image,[style*=background-image],[background]{{filter:invert(1) hue-rotate(180deg)}}#pz-colors span{{filter:invert(1) hue-rotate(180deg)}}" } else { "" }
    )
}

/// What the writing field starts with: the text, an empty line, the signature, the quoted message.
pub fn start_content(text_html: &str, signature: Option<String>, quote: Option<String>) -> String {
    let mut s = if text_html.trim().is_empty() { "<div><br></div><div><br></div>".to_string() } else { format!("{text_html}<div><br></div>") };
    if let Some(sig) = signature {
        // the signature and the quoted message are not checked for spelling (they may be in
        // another language, and they are not being written now)
        s.push_str(&format!("<div class=\"pz-sig\" spellcheck=\"false\">{sig}</div>"));
    }
    if let Some(q) = quote {
        s.push_str(&format!("<div class=\"pz-quote\" spellcheck=\"false\">{q}</div>"));
    }
    s
}

/// The quoted (or forwarded) message as it appears under the new text.
#[allow(clippy::too_many_arguments)]
pub fn quote_html(title: &str, from: &str, to: &str, date: &str, subject: &str, html: Option<String>, text: &str) -> String {
    let head = format!(
        "<div class=\"pz-quote-head\">------ {} ------<br>Od: {}<br>Do: {}<br>Data: {}<br>Temat: {}</div>",
        esc(title),
        esc(from),
        esc(to),
        esc(date),
        esc(subject)
    );
    let body = match html {
        Some(h) => h,
        None => crate::signature::text_to_html(text),
    };
    format!("{head}<blockquote style=\"margin:0;padding-left:10px;border-left:2px solid #ccc\">{body}</blockquote>")
}
