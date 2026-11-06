//! Formatted emails are shown with the real Edge browser engine (WebView2), placed as a
//! child window exactly over the reading pane, so they look like they do in Gmail.
//! Links open in the normal web browser. If WebView2 is missing, the app falls back to
//! its own simple reader.

use crate::model::Message;
use eframe::egui;

/// A picture (PNG) of what a browser view shows; `done` gets it (on this thread, later).
pub fn capture_png(view: &wry::WebView, done: impl FnOnce(Vec<u8>) + 'static) -> bool {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Com::STREAM_SEEK_SET;
        use wry::WebViewExtWindows;
        let Some(stream) = windows::Win32::UI::Shell::SHCreateMemStream(None) else { return false };
        let keep = stream.clone();
        let handler = webview2_com::CapturePreviewCompletedHandler::create(Box::new(move |res| {
            if res.is_ok() {
                let _ = keep.Seek(0, STREAM_SEEK_SET, None);
                let mut bytes = Vec::new();
                let mut chunk = vec![0u8; 256 * 1024];
                loop {
                    let mut read = 0u32;
                    let _ = keep.Read(chunk.as_mut_ptr() as *mut _, chunk.len() as u32, Some(&mut read));
                    if read == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&chunk[..read as usize]);
                }
                done(bytes);
            }
            Ok(())
        }));
        return view.webview().CapturePreview(webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG, &stream, &handler).is_ok();
    }
    #[cfg(not(windows))]
    false
}

/// A picture of the email view (PNG), taken shortly before a menu opens over it.
pub type Shot = std::sync::Arc<std::sync::Mutex<Option<(u64, Vec<u8>)>>>;

pub struct MailView {
    /// the latest picture of the email shown (message id, PNG), and a picture on its way
    pub shot: Shot,
    shot_busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    shot_at: Option<std::time::Instant>,
    /// keys pressed inside the email (Delete) come here, and wake the app
    keys: (std::sync::mpsc::Sender<String>, std::sync::mpsc::Receiver<String>),
    pub wake: Option<egui::Context>,
    view: Option<wry::WebView>,
    _context: Option<wry::WebContext>,
    shown: Option<u64>,
    visible: bool,
    pub failed: bool,
    last_rect: Option<egui::Rect>,
    /// pages asked for, and pages finished loading: until the newest one is in, the view stays
    /// hidden (so the email clicked before never shows in place of the one clicked now)
    navs: u64,
    loaded: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// when the newest page was asked for (for the log: how long the email took to show)
    asked: std::sync::Arc<std::sync::Mutex<Option<std::time::Instant>>>,
    late_noted: bool,
    nav_at: Option<std::time::Instant>,
    /// since when the app is in the background (Edge keeps as little in memory as it can)
    background_since: Option<std::time::Instant>,
    /// the view was made anew (at the start): it stays outside the
    /// window until its first email is in, as its first frames are black
    fresh: bool,
    /// an email wanted with no view yet: since when (the view is made a moment later)
    wanted_at: Option<std::time::Instant>,
}

impl Default for MailView {
    fn default() -> Self {
        Self { shot: Default::default(), shot_busy: Default::default(), shot_at: None, keys: std::sync::mpsc::channel(), wake: None, view: None, _context: None, shown: None, visible: false, failed: false, last_rect: None, navs: 0, loaded: Default::default(), nav_at: None, asked: Default::default(), late_noted: false, background_since: None, fresh: false, wanted_at: None }
    }
}

/// Where a browser view goes when it should not be seen: outside the window. It is never hidden
/// (WebView2 takes the keyboard when a hidden view is shown again, see WebView2Feedback #862), it
/// is only moved away and back.
pub fn away() -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(-32000.0, -32000.0), egui::vec2(64.0, 64.0))
}

/// The folder the browser part keeps its data in: the same for every view of the app (one set
/// of settings, so the views share it). When it is in use by another copy of the app, a second
/// folder (so a view still gets made).
pub fn data_dir(second: bool) -> Option<std::path::PathBuf> {
    // (a test copy has a browser of its own, never sharing the running app's)
    let base = match std::env::var("POCZTA_DATA_DIR") {
        Ok(d) => std::path::PathBuf::from(d),
        Err(_) => crate::model::cache_dir().parent().map(|p| p.to_path_buf())?,
    };
    Some(if second { base.join(format!("webview-{}", std::process::id())) } else { base.join("webview") })
}

/// True for the error of a browser folder in use by another copy of the app.
pub fn busy(e: &str) -> bool {
    e.contains("800700AA") || e.contains("w użyciu") || e.to_lowercase().contains("in use")
}

fn bounds(r: egui::Rect) -> wry::Rect {
    wry::Rect {
        position: wry::dpi::LogicalPosition::new(r.left() as f64, r.top() as f64).into(),
        size: wry::dpi::LogicalSize::new(r.width().max(1.0) as f64, r.height().max(1.0) as f64).into(),
    }
}

fn open_outside(url: &str) {
    let _ = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
}

fn file_url(path: &std::path::Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file:///");
    for c in s.chars() {
        match c {
            ' ' => out.push_str("%20"),
            '#' => out.push_str("%23"),
            '%' => out.push_str("%25"),
            _ => out.push(c),
        }
    }
    out
}

/// The email's HTML with its embedded pictures pointed at the cached files, links opening
/// outside, and a plain default font for text the email doesn't style itself.
/// A formatted email shown in its own window (`hwnd`), filling `rect` of it.
pub fn standalone(hwnd: isize, m: &Message, rect: egui::Rect) -> Result<(wry::WebView, wry::WebContext), String> {
    let html = prepare(m).ok_or("brak wersji sformatowanej")?;
    let path = crate::model::cache_put(html.as_bytes(), "html");
    let dir = data_dir(false);
    let mut context = wry::WebContext::new(dir);
    let start = file_url(std::path::Path::new(&path));
    let first = start.clone();
    let view = wry::WebViewBuilder::new_with_web_context(&mut context)
        .with_url(start)
        .with_bounds(bounds(rect))
        .with_background_color(if crate::theme::is_dark() { (0x1E, 0x1F, 0x22, 255) } else { (255, 255, 255, 255) })
        .with_navigation_handler(move |url| {
            if url == first {
                return true;
            }
            if url.starts_with("poczta-key://") {
                return false;
            }
            if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
                open_outside(&url);
                false
            } else {
                true
            }
        })
        .with_new_window_req_handler(|url, _| {
            open_outside(&url);
            wry::NewWindowResponse::Deny
        })
        .build_as_child(&crate::compose_view::Parent(hwnd))
        .map_err(|e| e.to_string())?;
    Ok((view, context))
}

/// A file (an attachment) shown in its own window: PDFs with the Edge viewer, pictures fitted
/// to the window, texts as they are.
pub fn file_view(hwnd: isize, path: &str, rect: egui::Rect) -> Result<(wry::WebView, wry::WebContext), String> {
    let p = std::path::Path::new(path);
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let url = if ["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg"].contains(&ext.as_str()) {
        // a picture: centred and fitted, on a neutral background
        let page = format!(
            "<!doctype html><html><head><meta charset=utf-8><style>html,body{{margin:0;height:100%;background:#f3f4f6}}body{{display:flex;align-items:center;justify-content:center}}img{{max-width:100%;max-height:100%;object-fit:contain}}</style></head><body><img src=\"{}\"></body></html>",
            file_url(p)
        );
        file_url(std::path::Path::new(&crate::model::cache_put(page.as_bytes(), "html")))
    } else {
        file_url(p)
    };
    let dir = data_dir(false);
    let mut context = wry::WebContext::new(dir);
    let view = wry::WebViewBuilder::new_with_web_context(&mut context)
        .with_url(url)
        .with_bounds(bounds(rect))
        .with_navigation_handler(|url| {
            if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
                open_outside(&url);
                false
            } else {
                true
            }
        })
        .with_new_window_req_handler(|url, _| {
            open_outside(&url);
            wry::NewWindowResponse::Deny
        })
        .build_as_child(&crate::compose_view::Parent(hwnd))
        .map_err(|e| e.to_string())?;
    Ok((view, context))
}

/// Moves a view to `rect` of its window.
pub fn place(view: &wry::WebView, rect: egui::Rect) {
    let _ = view.set_bounds(bounds(rect));
}

/// Styles and scripts from the internet would hold the whole email back until they arrive (a
/// slow or unreachable server: a blank view for many seconds). Stylesheets are loaded after the
/// text is shown, `@import`s become such stylesheets, and scripts from outside are left out.
fn no_waiting_for_internet(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len() + 256);
    let mut late: Vec<String> = Vec::new();
    let mut i = 0;
    while i < html.len() {
        let rest = &lower[i..];
        let Some(rel) = rest.find('<') else {
            out.push_str(&html[i..]);
            break;
        };
        let at = i + rel;
        out.push_str(&html[i..at]);
        let tag_end = lower[at..].find('>').map(|e| at + e + 1).unwrap_or(html.len());
        let tag = &lower[at..tag_end];
        let remote = |t: &str| t.contains("http://") || t.contains("https://") || t.contains("=\"//") || t.contains("='//") || t.contains("=//");
        if tag.starts_with("<link") && tag.contains("stylesheet") && remote(tag) {
            // the same stylesheet, applied once it is in (not before the email shows)
            let orig = &html[at..tag_end];
            let body = orig.trim_end_matches('>').trim_end_matches('/');
            out.push_str(&format!("{body} media=\"print\" onload=\"this.media='all'\">"));
            i = tag_end;
            continue;
        }
        if tag.starts_with("<script") && tag.contains("src=") && remote(tag) {
            // a script from outside: left out, with its closing tag
            let close = lower[tag_end..].find("</script>").map(|e| tag_end + e + 9).unwrap_or(tag_end);
            i = close;
            continue;
        }
        if tag.starts_with("<style") {
            let close = lower[tag_end..].find("</style>").map(|e| tag_end + e).unwrap_or(html.len());
            let css = &html[tag_end..close];
            out.push_str(&html[at..tag_end]);
            if css.to_ascii_lowercase().contains("@import") {
                // "@import url(…);" from outside: taken out, loaded later as a stylesheet
                let mut kept = String::with_capacity(css.len());
                for part in css.split_inclusive(';') {
                    let t = part.trim_start();
                    let tl = t.to_ascii_lowercase();
                    if tl.starts_with("@import") && (tl.contains("http") || tl.contains("//")) {
                        let url: String = t[7..]
                            .trim()
                            .trim_start_matches("url(")
                            .trim_end_matches(';')
                            .trim()
                            .trim_end_matches(')')
                            .trim_matches(['"', '\'', ' '])
                            .to_string();
                        if !url.is_empty() && !url.contains(['<', '>', '"']) {
                            late.push(url);
                        }
                    } else {
                        kept.push_str(part);
                    }
                }
                out.push_str(&kept);
            } else {
                out.push_str(css);
            }
            i = close;
            continue;
        }
        out.push_str(&html[at..tag_end]);
        i = tag_end;
    }
    if !late.is_empty() {
        let links: String = late.iter().map(|u| format!("<link rel=\"stylesheet\" href=\"{u}\" media=\"print\" onload=\"this.media='all'\">")).collect();
        let lo = out.to_ascii_lowercase();
        match lo.find("</head>") {
            Some(h) => out.insert_str(h, &links),
            None => out.push_str(&links),
        }
    }
    out
}

fn prepare(m: &Message) -> Option<String> {
    let mut html = std::fs::read_to_string(&m.html_path).ok()?;
    html = no_waiting_for_internet(&html);
    for (cid, path) in &m.inline {
        let url = file_url(std::path::Path::new(path));
        for prefix in ["cid:", "CID:", "Cid:"] {
            html = html.replace(&format!("{prefix}{cid}"), &url);
        }
    }
    let head = "<meta charset=\"utf-8\"><base target=\"_blank\"><meta name=\"color-scheme\" content=\"light\">\
        <style>html{background:#fff;color-scheme:light}\
        ::-webkit-scrollbar{width:10px;height:10px}::-webkit-scrollbar-track{background:transparent}\
        ::-webkit-scrollbar-thumb{background:#cfd3d8;border-radius:5px;border:2px solid #fff}::-webkit-scrollbar-thumb:hover{background:#aeb4bb}\
        ::-webkit-scrollbar-button{display:none;height:0;width:0}body{margin:12px 16px;font-family:'Segoe UI',Arial,sans-serif;font-size:14px;color:#222;word-wrap:break-word}\
        img{max-width:100%;height:auto}</style>\
        <script>document.addEventListener('keydown',function(e){var t=e.target;\
        if(t&&(t.isContentEditable||/^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName)))return;\
        if(e.key==='Delete'&&!e.ctrlKey&&!e.altKey&&!e.shiftKey){e.preventDefault();location.href='poczta-key://delete';}},true);\
        document.addEventListener('pointerdown',function(){try{location.href='poczta-key://clicked'}catch(e){}},true);</script>";
    // dark theme: the email's colours turned around (light page → dark, dark text → light),
    // pictures turned back so they look as they should
    let dark_css = format!("<style>{}</style>", "html{background:#e1e0dd!important;filter:invert(1) hue-rotate(180deg)}img,video,picture,canvas,svg image,[style*=background-image],[background]{filter:invert(1) hue-rotate(180deg)}::-webkit-scrollbar-track{background:#1e1f22}::-webkit-scrollbar-thumb{background:#4a4e55!important;border-color:#1e1f22!important}");
    let head = if crate::theme::is_dark() { format!("{head}{dark_css}") } else { head.to_string() };
    let head = head.as_str();
    let lower = html.to_lowercase();
    if let Some(i) = lower.find("<head>") {
        html.insert_str(i + 6, head);
    } else if let Some(i) = lower.find("<html") {
        let end = lower[i..].find('>').map(|e| i + e + 1).unwrap_or(0);
        html.insert_str(end, &format!("<head>{head}</head>"));
    } else {
        html = format!("<!doctype html><html><head>{head}</head><body>{html}</body></html>");
    }
    Some(html)
}

impl MailView {
    /// The app went to the background (minimized, in the tray) or came back. In the background
    /// Edge lets go of what it can (its own low-memory mode); back in front, normal again.
    pub fn set_background(&mut self, on: bool) {
        self.background_since = on.then(std::time::Instant::now);
        #[cfg(windows)]
        if let Some(v) = &self.view {
            use webview2_com::Microsoft::Web::WebView2::Win32::{ICoreWebView2_19, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL};
            use windows::core::Interface;
            use wry::WebViewExtWindows;
            let level = if on { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW } else { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL };
            // (an Edge older than this setting: nothing done)
            if let Ok(w) = v.webview().cast::<ICoreWebView2_19>() {
                let _ = unsafe { w.SetMemoryUsageTargetLevel(level) };
            }
        }
    }

    pub fn in_background(&self) -> bool {
        self.background_since.is_some()
    }

    /// Shows `want` (message and where to draw it) or hides the view when `want` is None.
    /// Starts the browser view ahead of time (hidden), so the first formatted email shows at once.
    pub fn prepare(&mut self, frame: &eframe::Frame) {
        if self.view.is_none() && !self.failed {
            self.build(frame, egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(10.0)));
        }
    }

    fn build(&mut self, frame: &eframe::Frame, rect: egui::Rect) {
        let started = std::time::Instant::now();
        let _ = rect;
        let mut built = self.build_in(frame, data_dir(false));
        if built.as_ref().is_err_and(|e| busy(e)) {
            crate::diag::log("startup: browser folder in use by another copy of the app, using a second one");
            built = self.build_in(frame, data_dir(true));
        }
        match built {
            Ok((v, context)) => {
                crate::diag::took("startup: browser view for formatted mail", started, "");
                self.view = Some(v);
                self._context = Some(context);
                self.fresh = true;
            }
            Err(e) => {
                crate::diag::log(format!("email view: browser not available ({e})"));
                self.failed = true;
            }
        }
    }

    fn build_in(&mut self, frame: &eframe::Frame, dir: Option<std::path::PathBuf>) -> Result<(wry::WebView, wry::WebContext), String> {
        let mut context = wry::WebContext::new(dir);
        let (keys, wake) = (self.keys.0.clone(), self.wake.clone());
        let (loaded, wake2, asked) = (self.loaded.clone(), self.wake.clone(), self.asked.clone());
        let built = wry::WebViewBuilder::new_with_web_context(&mut context)
            .with_on_page_load_handler(move |e, _| {
                let since = asked.lock().ok().and_then(|a| *a).map(|t| t.elapsed().as_millis()).unwrap_or(0);
                crate::diag::log(format!("email view: page {} after {since} ms", if matches!(e, wry::PageLoadEvent::Finished) { "loaded" } else { "started" }));
                if matches!(e, wry::PageLoadEvent::Finished) {
                    loaded.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if let Some(c) = &wake2 {
                        c.request_repaint();
                    }
                }
            })
            // made shown but outside the window (never hidden: see `away`)
            .with_bounds(bounds(away()))
            .with_visible(true)
            // the keyboard stays with the message list until the email itself is clicked
            .with_focused(false)
            .with_background_color(if crate::theme::is_dark() { (0x1E, 0x1F, 0x22, 255) } else { (255, 255, 255, 255) })
            .with_navigation_handler(move |url| {
                // a key pressed inside the email (see the page's little script)
                if let Some(k) = url.strip_prefix("poczta-key://") {
                    let _ = keys.send(k.trim_end_matches('/').to_string());
                    if let Some(c) = &wake {
                        c.request_repaint();
                    }
                    return false;
                }
                if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
                    open_outside(&url);
                    false
                } else {
                    true
                }
            })
            .with_new_window_req_handler(|url, _| {
                open_outside(&url);
                wry::NewWindowResponse::Deny
            })
            .build_as_child(frame)
            .map_err(|e| e.to_string())?;
        Ok((built, context))
    }

    pub fn sync(&mut self, frame: &eframe::Frame, want: Option<(&Message, egui::Rect)>) {
        let Some((m, rect)) = want else {
            // not wanted on the screen: moved away, not hidden
            if self.visible {
                if let Some(v) = &self.view {
                    let _ = v.set_bounds(bounds(away()));
                }
                self.last_rect = None;
                self.visible = false;
            }
            return;
        };
        if self.view.is_none() && !self.failed {
            // making the view stops the app for ~0.4 s; the window is drawn first, so it never
            // shows an undrawn (black) part meanwhile
            let wait = std::time::Duration::from_millis(60);
            let since = *self.wanted_at.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() < wait {
                if let Some(c) = &self.wake {
                    c.request_repaint_after(wait.saturating_sub(since.elapsed()));
                }
                return;
            }
            self.wanted_at = None;
            self.build(frame, rect);
        }
        let Some(view) = &self.view else { return };
        if self.shown != Some(m.id) {
            let t = std::time::Instant::now();
            match prepare(m) {
                Some(html) => {
                    let kb = html.len() / 1024;
                    // (a test copy writes into its own folder, never into the real app's page)
                    let path = match std::env::var("POCZTA_DATA_DIR") {
                        Ok(d) => std::path::PathBuf::from(d).join("view.html"),
                        Err(_) => crate::model::cache_dir().join("view.html"),
                    };
                    match std::fs::write(&path, html) {
                        Ok(()) => {
                            if let Ok(mut a) = self.asked.lock() {
                                *a = Some(std::time::Instant::now());
                            }
                            // the email before goes at once (never shown in place of this one)
                            let _ = view.evaluate_script("try{document.documentElement.style.visibility='hidden'}catch(e){}");
                            let r = view.load_url(&file_url(&path));
                            crate::diag::log(format!("email view: page asked ({kb} KB, made in {} ms){}", t.elapsed().as_millis(), r.err().map(|e| format!(" — {e}")).unwrap_or_default()));
                            self.navs += 1;
                            self.nav_at = Some(std::time::Instant::now());
                            self.late_noted = false;
                        }
                        Err(e) => crate::diag::log(format!("email view: the page could not be written: {e}")),
                    }
                }
                None => crate::diag::log(format!("email view: no page for the message ({})", if m.html_path.is_empty() { "no formatted text" } else { "its file can't be read" })),
            }
            self.shown = Some(m.id);
        }
        // the new email not in yet: nothing rather than the one before (at most a moment)
        // (for the log: an email that takes long to come in)
        let behind = self.loaded.load(std::sync::atomic::Ordering::SeqCst) < self.navs;
        // a view made anew waits outside the window for its first email (at most 2 s), so no
        // black frame shows; the app's own background is seen there meanwhile
        let wait = std::time::Duration::from_secs(2);
        let waiting = self.fresh && behind && self.nav_at.is_some_and(|t| t.elapsed() < wait);
        if waiting {
            if let (Some(c), Some(t)) = (&self.wake, self.nav_at) {
                c.request_repaint_after(wait.saturating_sub(t.elapsed()));
            }
        } else {
            self.fresh = false;
        }
        let target = if waiting { away() } else { rect };
        if self.last_rect != Some(target) {
            let _ = view.set_bounds(bounds(target));
            self.last_rect = Some(target);
        }
        if behind && !self.late_noted && self.nav_at.is_some_and(|t| t.elapsed().as_secs() >= 3) {
            self.late_noted = true;
            crate::diag::log("email view: the email is still loading after 3 s");
        }
        self.visible = true;
    }

    /// Runs a piece of the page's script (tests).
    pub fn run_js(&self, js: &str) {
        if let Some(v) = &self.view {
            let _ = v.evaluate_script(js);
        }
    }

    /// Takes a picture of the email shown (in the background; it lands in `shot`). At most
    /// about once a second.
    pub fn capture(&mut self) {
        use std::sync::atomic::Ordering;
        if !self.visible || self.shot_busy.load(Ordering::Relaxed) || self.shot_at.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(900)) {
            return;
        }
        let (Some(v), Some(id)) = (&self.view, self.shown) else { return };
        self.shot_at = Some(std::time::Instant::now());
        #[cfg(windows)]
        unsafe {
            use wry::WebViewExtWindows;
            use windows::Win32::System::Com::STREAM_SEEK_SET;
            let Some(stream) = windows::Win32::UI::Shell::SHCreateMemStream(None) else { return };
            let (out, busy, wake, keep) = (self.shot.clone(), self.shot_busy.clone(), self.wake.clone(), stream.clone());
            busy.store(true, Ordering::Relaxed);
            let handler = webview2_com::CapturePreviewCompletedHandler::create(Box::new(move |res| {
                busy.store(false, Ordering::Relaxed);
                if res.is_ok() {
                    let _ = keep.Seek(0, STREAM_SEEK_SET, None);
                    let mut bytes = Vec::new();
                    let mut chunk = vec![0u8; 256 * 1024];
                    loop {
                        let mut read = 0u32;
                        let _ = keep.Read(chunk.as_mut_ptr() as *mut _, chunk.len() as u32, Some(&mut read));
                        if read == 0 {
                            break;
                        }
                        bytes.extend_from_slice(&chunk[..read as usize]);
                    }
                    if let Ok(mut g) = out.lock() {
                        *g = Some((id, bytes));
                    }
                    if let Some(c) = &wake {
                        c.request_repaint();
                    }
                }
                Ok(())
            }));
            if v.webview().CapturePreview(webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG, &stream, &handler).is_err() {
                self.shot_busy.store(false, Ordering::Relaxed);
            }
        }
    }

    /// The email shown now (its id), when it is shown.
    pub fn shown(&self) -> Option<u64> {
        self.shown.filter(|_| self.visible)
    }

    /// Keys pressed inside the email since last time.
    pub fn keys(&self) -> Vec<String> {
        self.keys.1.try_iter().collect()
    }

    /// Forget what is shown, e.g. after the message got its formatted version.
    pub fn reload(&mut self) {
        self.shown = None;
    }

    /// After switching the theme: the page background and the email shown again.
    pub fn retheme(&mut self) {
        self.shown = None;
        if let Some(v) = &self.view {
            let _ = v.set_background_color(if crate::theme::is_dark() { (0x1E, 0x1F, 0x22, 255) } else { (255, 255, 255, 255) });
        }
    }
}
