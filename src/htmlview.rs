//! Formatted emails drawn by the app itself (no browser window): the Blitz HTML/CSS engine
//! (Stylo for styles, Taffy for layout, Parley for text) lays the email out and Vello draws it
//! on the processor into a picture, which the reading pane shows like any other part of the
//! window. Menus and lists can lie over it, it never takes the keyboard, and it looks the same
//! every time.
//!
//! The work happens on a thread of its own. Pictures in the email (attached ones and ones from
//! the internet) come in while it is shown: the text first, the pictures a moment later.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::net::Resource;
use blitz_dom::{local_name, DocumentConfig};
use blitz_html::HtmlDocument;
use blitz_paint::paint_scene;
use blitz_traits::net::{BoxedHandler, NetProvider, Request, SharedCallback};
use blitz_traits::shell::{ColorScheme, Viewport};

/// The tallest picture made of one email (in pixels); longer emails are cut there.
const MAX_HEIGHT_PX: u32 = 30_000;
/// The pictures handed to the screen are cut in strips this tall (a texture has a size limit).
pub const STRIP_PX: u32 = 2048;

/// What to show: an email's HTML at a width.
pub struct Wanted {
    pub key: u64,
    pub html: String,
    /// width in screen points, and screen pixels per point
    pub width: f32,
    pub scale: f32,
}

enum Job {
    Show(Wanted),
    Link { key: u64, x: f32, y: f32, click: bool },
}

/// A strip of a drawn email: RGBA pixels `w`×`h`, `top` pixels from the email's top, of an email
/// `total` pixels tall; `done` when its pictures are all in (and this is its last strip).
pub struct Drawn {
    pub key: u64,
    pub w: u32,
    pub h: u32,
    pub top: u32,
    pub total: u32,
    pub rgba: Vec<u8>,
    pub scale: f32,
    pub done: bool,
    pub took_ms: u128,
}

pub enum Out {
    Drawn(Drawn),
    /// the link under a point (None: no link there)
    Link { key: u64, href: Option<String>, click: bool },
}

/// The drawing thread (one for the app).
pub struct Engine {
    jobs: Sender<Job>,
    pub out: Receiver<Out>,
}

impl Engine {
    pub fn start(ctx: eframe::egui::Context) -> Engine {
        let (jobs, job_rx) = channel::<Job>();
        let (out_tx, out) = channel::<Out>();
        std::thread::Builder::new()
            .name("email drawing".into())
            .spawn(move || worker(job_rx, out_tx, ctx))
            .expect("thread");
        Engine { jobs, out }
    }

    pub fn show(&self, r: Wanted) {
        let _ = self.jobs.send(Job::Show(r));
    }

    /// Asks which link is at (x, y) (points from the top left of the email).
    pub fn link_at(&self, key: u64, x: f32, y: f32, click: bool) {
        let _ = self.jobs.send(Job::Link { key, x, y, click });
    }
}

/// The key of an email drawn at a width.
pub fn key_of(id: u64, html_path: &str, width: f32, scale: f32) -> u64 {
    let mut h = DefaultHasher::new();
    (id, html_path, (width / 4.0).round() as i64, (scale * 100.0) as i64).hash(&mut h);
    h.finish()
}

/// Fetches what an email refers to: files of attached pictures at once, pictures from the
/// internet on threads of their own (kept on disk, so the next time they are there at once).
struct Fetcher {
    callback: SharedCallback<Resource>,
    pending: Arc<AtomicUsize>,
    wake: Sender<()>,
}

impl NetProvider<Resource> for Fetcher {
    fn fetch(&self, doc_id: usize, request: Request, handler: BoxedHandler<Resource>) {
        let url = request.url;
        match url.scheme() {
            "file" => {
                if let Some(bytes) = url.to_file_path().ok().and_then(|p| std::fs::read(p).ok()) {
                    handler.bytes(doc_id, bytes.into(), self.callback.clone());
                }
            }
            "data" => {
                if let Some(bytes) = data_url(url.as_str()) {
                    handler.bytes(doc_id, bytes.into(), self.callback.clone());
                }
            }
            "http" | "https" => {
                let (callback, pending, wake) = (self.callback.clone(), self.pending.clone(), self.wake.clone());
                pending.fetch_add(1, Ordering::SeqCst);
                let address = url.to_string();
                std::thread::spawn(move || {
                    if let Some(bytes) = remote(&address) {
                        handler.bytes(doc_id, bytes.into(), callback);
                    }
                    pending.fetch_sub(1, Ordering::SeqCst);
                    let _ = wake.send(());
                });
            }
            _ => {}
        }
    }
}

/// One connection setup for all pictures (secure connections through Windows, kept open
/// between pictures of the same server).
fn agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        let mut b = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(5)).timeout(Duration::from_secs(12)).max_idle_connections_per_host(4);
        if let Ok(tls) = native_tls::TlsConnector::new() {
            b = b.tls_connector(Arc::new(tls));
        }
        b.build()
    })
}

/// At most 8 pictures fetched at the same time.
struct Slot;
static SLOTS: std::sync::Mutex<usize> = std::sync::Mutex::new(0);
static FREED: std::sync::Condvar = std::sync::Condvar::new();
impl Slot {
    fn take() -> Slot {
        let mut n = SLOTS.lock().unwrap_or_else(|e| e.into_inner());
        while *n >= 8 {
            n = FREED.wait(n).unwrap_or_else(|e| e.into_inner());
        }
        *n += 1;
        Slot
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        let mut n = SLOTS.lock().unwrap_or_else(|e| e.into_inner());
        *n = n.saturating_sub(1);
        FREED.notify_one();
    }
}

/// A picture from the internet: from the disk when fetched before.
fn remote(address: &str) -> Option<Vec<u8>> {
    let mut h = DefaultHasher::new();
    address.hash(&mut h);
    let file = crate::model::cache_dir().join(format!("web-{:016x}.bin", h.finish()));
    if let Ok(b) = std::fs::read(&file) {
        return Some(b);
    }
    // a few at a time (an email with a hundred pictures doesn't open a hundred connections)
    let _slot = Slot::take();
    let resp = match agent().get(address).set("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Mail").call() {
        Ok(r) => r,
        Err(e) => {
            crate::diag::log(format!("email picture not fetched: {e}"));
            return None;
        }
    };
    let mut bytes = Vec::new();
    use std::io::Read;
    resp.into_reader().take(12 * 1024 * 1024).read_to_end(&mut bytes).ok()?;
    let _ = std::fs::write(&file, &bytes);
    Some(bytes)
}

/// The bytes of a "data:" address (base64 or plain).
fn data_url(s: &str) -> Option<Vec<u8>> {
    let rest = s.strip_prefix("data:")?;
    let (head, body) = rest.split_once(',')?;
    if head.ends_with(";base64") {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.decode(body.trim()).ok()
    } else {
        Some(body.as_bytes().to_vec())
    }
}

/// The app's own look for emails without one (font, margins, long words wrapped).
const BASE_CSS: &str = "<style>html{background:#fff}body{margin:12px 16px;font-family:'Segoe UI',Arial,sans-serif;font-size:14px;color:#222;overflow-wrap:anywhere;word-wrap:break-word}img{max-width:100%;height:auto}pre{white-space:pre-wrap}blockquote{margin-left:8px;padding-left:10px;border-left:3px solid #ccc}</style>";

fn with_base_css(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    if let Some(i) = lower.find("<head>") {
        let mut s = html.to_string();
        s.insert_str(i + 6, BASE_CSS);
        s
    } else if let Some(i) = lower.find("<html") {
        let end = lower[i..].find('>').map(|e| i + e + 1).unwrap_or(0);
        let mut s = html.to_string();
        s.insert_str(end, &format!("<head>{BASE_CSS}</head>"));
        s
    } else {
        format!("<!doctype html><html><head>{BASE_CSS}</head><body>{html}</body></html>")
    }
}

struct Current {
    key: u64,
    doc: HtmlDocument,
}

fn worker(jobs: Receiver<Job>, out: Sender<Out>, ctx: eframe::egui::Context) {
    let fonts = blitz_dom::FontContext::default();
    let mut current: Option<Current> = None;
    let mut next: Option<Job> = None;
    loop {
        let job = match next.take() {
            Some(j) => j,
            None => match jobs.recv() {
                Ok(j) => j,
                Err(_) => return,
            },
        };
        match job {
            Job::Link { key, x, y, click } => {
                let href = current.as_ref().filter(|c| c.key == key).and_then(|c| link_at(&c.doc, x, y));
                let _ = out.send(Out::Link { key, href, click });
                ctx.request_repaint();
            }
            Job::Show(r) => {
                // only the newest request matters (clicking through messages quickly)
                let mut r = r;
                while let Ok(j) = jobs.try_recv() {
                    match j {
                        Job::Show(newer) => r = newer,
                        other => {
                            next = Some(other);
                        }
                    }
                }
                let started = Instant::now();
                let (res_tx, res_rx) = channel::<Resource>();
                let (wake_tx, wake_rx) = channel::<()>();
                let pending = Arc::new(AtomicUsize::new(0));
                let callback: SharedCallback<Resource> = Arc::new(move |_doc: usize, res: Result<Resource, Option<String>>| {
                    if let Ok(res) = res {
                        let _ = res_tx.send(res);
                    }
                });
                let net = Arc::new(Fetcher { callback, pending: pending.clone(), wake: wake_tx });
                let html = with_base_css(&r.html);
                let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    HtmlDocument::from_html(&html, DocumentConfig { net_provider: Some(net as _), font_ctx: Some(fonts.clone()), ..Default::default() })
                }));
                let Ok(mut doc) = made else {
                    crate::diag::log("email drawing: this email could not be read");
                    continue;
                };
                let w_px = (r.width * r.scale).max(100.0) as u32;
                doc.as_mut().set_viewport(Viewport::new(w_px, (800.0 * r.scale) as u32, r.scale, ColorScheme::Light));
                while let Ok(res) = res_rx.try_recv() {
                    doc.as_mut().load_resource(res);
                }
                let done = pending.load(Ordering::SeqCst) == 0;
                let whole = draw(&mut doc, &r, done, started, &out, &ctx, &jobs, &mut next);
                current = Some(Current { key: r.key, doc });
                if !whole {
                    // another email was asked for meanwhile
                    continue;
                }
                // pictures from the internet: drawn again as they come (for up to 10 seconds),
                // unless another email is asked for meanwhile
                let mut changed = false;
                let mut last_draw = Instant::now();
                while pending.load(Ordering::SeqCst) > 0 && started.elapsed() < Duration::from_secs(10) {
                    match jobs.recv_timeout(Duration::from_millis(60)) {
                        Ok(j) => {
                            next = Some(j);
                            if matches!(next, Some(Job::Show(_))) {
                                break;
                            }
                            // a link question: answered right away
                            if let Some(Job::Link { key, x, y, click }) = next.take() {
                                let href = current.as_ref().filter(|c| c.key == key).and_then(|c| link_at(&c.doc, x, y));
                                let _ = out.send(Out::Link { key, href, click });
                                ctx.request_repaint();
                            }
                        }
                        Err(RecvTimeoutError::Disconnected) => return,
                        Err(RecvTimeoutError::Timeout) => {}
                    }
                    while wake_rx.try_recv().is_ok() {}
                    let c = current.as_mut().unwrap();
                    while let Ok(res) = res_rx.try_recv() {
                        c.doc.as_mut().load_resource(res);
                        changed = true;
                    }
                    // drawn again at most every 400 ms while pictures arrive
                    if changed && last_draw.elapsed() > Duration::from_millis(400) {
                        changed = false;
                        last_draw = Instant::now();
                        if !draw(&mut c.doc, &r, false, started, &out, &ctx, &jobs, &mut next) {
                            break;
                        }
                    }
                }
                if next.as_ref().is_some_and(|j| matches!(j, Job::Show(_))) {
                    continue;
                }
                if let Some(c) = current.as_mut() {
                    while let Ok(res) = res_rx.try_recv() {
                        c.doc.as_mut().load_resource(res);
                        changed = true;
                    }
                    if changed {
                        draw(&mut c.doc, &r, true, started, &out, &ctx, &jobs, &mut next);
                    }
                }
            }
        }
    }
}

/// Styles, layout and drawing of the email as it is now, in strips from the top (each sent as
/// soon as it is drawn, so the start of a long email shows at once). Stops (false) when another
/// email is asked for meanwhile; link questions are answered between strips.
#[allow(clippy::too_many_arguments)]
fn draw(doc: &mut HtmlDocument, r: &Wanted, done: bool, started: Instant, out: &Sender<Out>, ctx: &eframe::egui::Context, jobs: &Receiver<Job>, next: &mut Option<Job>) -> bool {
    let laid = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        doc.as_mut().resolve(0.0);
        doc.as_ref().root_element().final_layout.size.height
    }));
    let Ok(h_css) = laid else {
        crate::diag::log("email drawing: this email could not be laid out");
        return true;
    };
    let w = (r.width * r.scale).max(100.0) as u32;
    let total = ((h_css.max(40.0) * r.scale) as u32).min(MAX_HEIGHT_PX);
    let mut top = 0u32;
    while top < total {
        let h = (total - top).min(STRIP_PX);
        doc.as_mut().set_viewport_scroll(blitz_dom::Point { x: 0.0, y: top as f64 / r.scale as f64 });
        let drawn = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render_to_buffer::<VelloCpuImageRenderer, _>(|scene| paint_scene(scene, doc.as_ref(), r.scale as f64, w, h), w, h)));
        let Ok(rgba) = drawn else {
            crate::diag::log("email drawing: this email could not be drawn");
            break;
        };
        let last = top + h >= total;
        let _ = out.send(Out::Drawn(Drawn { key: r.key, w, h, top, total, rgba, scale: r.scale, done: done && last, took_ms: started.elapsed().as_millis() }));
        ctx.request_repaint();
        top += h;
        // meanwhile: another email (stop here), or which link is under the pointer
        while let Ok(j) = jobs.try_recv() {
            match j {
                Job::Show(_) => {
                    *next = Some(j);
                    doc.as_mut().set_viewport_scroll(blitz_dom::Point { x: 0.0, y: 0.0 });
                    return false;
                }
                Job::Link { key, x, y, click } => {
                    doc.as_mut().set_viewport_scroll(blitz_dom::Point { x: 0.0, y: 0.0 });
                    let href = if key == r.key { link_at(doc, x, y) } else { None };
                    let _ = out.send(Out::Link { key, href, click });
                }
            }
        }
    }
    // (links are found from the top of the email)
    doc.as_mut().set_viewport_scroll(blitz_dom::Point { x: 0.0, y: 0.0 });
    true
}

/// The address of the link at (x, y) (CSS pixels from the top left), if there is one.
fn link_at(doc: &HtmlDocument, x: f32, y: f32) -> Option<String> {
    let base = doc.as_ref();
    let hit = base.hit(x, y)?;
    let mut id = Some(hit.node_id);
    while let Some(i) = id {
        let node = base.get_node(i)?;
        if let Some(el) = node.element_data() {
            if el.name.local == local_name!("a") {
                if let Some(h) = el.attr(local_name!("href")) {
                    return Some(h.to_string());
                }
            }
        }
        id = node.parent;
    }
    None
}

/// Test: draws one email at once (pictures from the internet left out), and says how long the
/// steps took: (reading, styles + layout, drawing) in ms, and the height in pixels.
pub fn bench(html: &str, width: f32, scale: f32, fonts: &blitz_dom::FontContext) -> (u128, u128, u128, u32) {
    let t = Instant::now();
    let (res_tx, res_rx) = channel::<Resource>();
    let (wake, _w) = channel::<()>();
    let callback: SharedCallback<Resource> = Arc::new(move |_d: usize, r: Result<Resource, Option<String>>| {
        if let Ok(r) = r {
            let _ = res_tx.send(r);
        }
    });
    struct Local(SharedCallback<Resource>);
    impl NetProvider<Resource> for Local {
        fn fetch(&self, doc_id: usize, request: Request, handler: BoxedHandler<Resource>) {
            if request.url.scheme() == "file" {
                if let Some(b) = request.url.to_file_path().ok().and_then(|p| std::fs::read(p).ok()) {
                    handler.bytes(doc_id, b.into(), self.0.clone());
                }
            }
        }
    }
    let _ = wake;
    let html = with_base_css(html);
    let mut doc = HtmlDocument::from_html(&html, DocumentConfig { net_provider: Some(Arc::new(Local(callback)) as _), font_ctx: Some(fonts.clone()), ..Default::default() });
    let w_px = (width * scale) as u32;
    doc.as_mut().set_viewport(Viewport::new(w_px, (800.0 * scale) as u32, scale, ColorScheme::Light));
    while let Ok(r) = res_rx.try_recv() {
        doc.as_mut().load_resource(r);
    }
    let parsed = t.elapsed().as_millis();
    doc.as_mut().resolve(0.0);
    let laid = t.elapsed().as_millis();
    let h_css = doc.as_ref().root_element().final_layout.size.height;
    let h = ((h_css.max(40.0) * scale) as u32).min(MAX_HEIGHT_PX);
    let _ = render_to_buffer::<VelloCpuImageRenderer, _>(|scene| paint_scene(scene, doc.as_ref(), scale as f64, w_px, h), w_px, h);
    let drawn = t.elapsed().as_millis();
    (parsed, laid - parsed, drawn - laid, h)
}

/// The email's HTML ready to draw: pictures attached to it ("cid:") point at their files.
pub fn prepare(html_path: &str, inline: &[(String, String)]) -> Option<String> {
    let mut html = std::fs::read_to_string(html_path).ok()?;
    for (cid, path) in inline {
        if let Ok(u) = url::Url::from_file_path(path) {
            for prefix in ["cid:", "CID:", "Cid:"] {
                html = html.replace(&format!("{prefix}{cid}"), u.as_str());
            }
        }
    }
    Some(html)
}
