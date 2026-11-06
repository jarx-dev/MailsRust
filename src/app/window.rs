use super::*;

impl MailApp {
    /// Speed test (POCZTA_BENCH): goes through messages one after another, searches, comes
    /// back, as a person would, and writes the measured times to the log every 3 seconds.
    pub(super) fn bench_step(&mut self, ctx: &egui::Context) {
        if std::env::var("POCZTA_BENCH").is_err() || self.loading.is_some() {
            return;
        }
        ctx.request_repaint();
        let n = self.drawn;
        if n % 6 == 0 && self.search.is_empty() {
            let ids = self.visible_ids();
            if !ids.is_empty() {
                let k = ((n / 6) as usize) % ids.len().min(60);
                self.actions.push(Action::Select(ids[k]));
            }
        }
        match n % 600 {
            300 => self.search = "faktura".into(),
            420 => self.search = "spotkanie w czwartek".into(),
            500 => self.search.clear(),
            _ => {}
        }

    }

    /// The number of unread messages in the inbox on the program's icons: the tray icon (with
    /// its hint) and the taskbar button.
    pub(super) fn update_badges(&mut self, frame: &mut eframe::Frame) {
        if self.loading.is_some() {
            return;
        }
        let unread = self.counts().inbox_unread_all;
        // the taskbar forgets the sign when its button is made again (e.g. after Explorer restarts)
        let again = self.clock() - self.badge_at > 30.0;
        if self.badge_shown == Some(unread) && !again {
            return;
        }
        let changed = self.badge_shown != Some(unread);
        if let (Ok(_), Ok(dir)) = (std::env::var("POCZTA_NOTIFY_TEST"), std::env::var("POCZTA_DATA_DIR")) {
            // test: the pictures put on the icons, to look at
            let _ = crate::notify::icon_with_count(32, unread).save(std::path::Path::new(&dir).join("badge-tray.png"));
            let _ = crate::notify::badge_only(32, unread).save(std::path::Path::new(&dir).join("badge-taskbar.png"));
            let _ = crate::notify::icon_with_count(32, 128).save(std::path::Path::new(&dir).join("badge-tray-128.png"));
        }
        self.badge_shown = Some(unread);
        self.badge_at = self.clock();
        if changed {
            if let Some(t) = &self._tray {
                let img = crate::notify::icon_with_count(32, unread);
                if let Ok(i) = tray_icon::Icon::from_rgba(img.into_raw(), 32, 32) {
                    let _ = t.set_icon(Some(i));
                }
                let tip = if unread == 0 { "Poczta".to_string() } else { format!("Poczta — {}", crate::notify::unread_text(unread)) };
                let _ = t.set_tooltip(Some(tip));
            }
        }
        #[cfg(windows)]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let hwnd = match frame.window_handle().map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
                _ => return,
            };
            self.main_hwnd = Some(hwnd);
            if self.taskbar.is_none() && !self.taskbar_tried {
                self.taskbar_tried = true;
                self.taskbar = crate::notify::Taskbar::new();
            }
            if let Some(tb) = &self.taskbar {
                if unread == 0 {
                    tb.overlay(hwnd, None, "");
                } else {
                    let png = crate::notify::png(&crate::notify::badge_only(32, unread));
                    tb.overlay(hwnd, Some(&png), &crate::notify::unread_text(unread));
                }
            }
        }
        #[cfg(not(windows))]
        let _ = frame;
    }

    /// While the mailbox is being read: a plain loading screen (the window can be moved by it)
    /// instead of an empty app that fills in a moment later. True when it was shown.
    pub(super) fn loading_screen(&mut self, ui: &mut Ui) -> bool {
        // the newest messages are in: shown, unless the whole mailbox is about to follow (a
        // moment's wait instead of a list that changes at once)
        if self.loading.is_none() || (self.preview.is_some() &&self.opened.elapsed() > std::time::Duration::from_millis(300)) {
            return false;
        }
        egui::CentralPanel::default().frame(Frame::new().fill(BG())).show(ui, |ui| {
            let r = ui.max_rect();
            if ui.interact(r, id("loading-drag"), Sense::drag()).drag_started_by(egui::PointerButton::Primary) {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            ui.scope_builder(egui::UiBuilder::new().max_rect(Rect::from_center_size(r.center(), Vec2::new(320.0, 140.0))), |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("Poczta").size(28.0).strong().color(TEXT()));
                    ui.add_space(14.0);
                    ui.add(egui::Spinner::new().size(22.0).color(ACCENT()));
                    ui.add_space(10.0);
                    ui.label(RichText::new("Wczytywanie poczty…").size(14.0).color(MUTED()));
                });
            });
        });
        true
    }

    /// A small floating bubble at the bottom of the message list: "syncing…" while mail is being
    /// sent or received, then the result for a few seconds (longer for problems).
    /// A short note at the top of the window, e.g. after copying something.
    pub(super) fn flash(&mut self, text: &str) {
        self.flash = Some((text.to_string(), self.clock()));
        self.ctx.request_repaint();
    }

    /// The note at the top centre of the window (above everything, the email view does not
    /// cover it there), for three seconds.
    pub(super) fn flash_ui(&mut self, ctx: &egui::Context) {
        let Some((text, at)) = self.flash.clone() else { return };
        const KEEP: f64 = 3.0;
        let age = self.clock() - at;
        if age > KEEP {
            self.flash = None;
            return;
        }
        let alpha = ((KEEP - age) / 0.4).clamp(0.0, 1.0) as f32;
        let s = ctx.content_rect();
        egui::Area::new(id("flash"))
            .order(egui::Order::Tooltip)
            .fixed_pos(Pos2::new(s.center().x, s.top() + 74.0))
            .pivot(Align2::CENTER_TOP)
            .interactable(false)
            .show(ctx, |ui| {
                ui.set_opacity(alpha);
                Frame::new()
                    .fill(Color32::from_rgb(0x1F, 0x8A, 0x4C))
                    .corner_radius(CornerRadius::same(20))
                    .inner_margin(Margin::symmetric(18, 10))
                    .shadow(egui::epaint::Shadow { offset: [0, 3], blur: 16, spread: 0, color: Color32::from_black_alpha(60) })
                    .show(ui, |ui| {
                        ui.set_max_width(640.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("📋").size(15.0).color(Color32::WHITE));
                            ui.add(egui::Label::new(RichText::new(text).size(14.0).color(Color32::WHITE)).wrap());
                        });
                    });
            });
        ctx.request_repaint_after(std::time::Duration::from_millis(60));
    }

    pub(super) fn toast(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        // Only a refresh you asked for shows "syncing"; automatic checks and saving
        // changes happen quietly in the background.
        let syncing = self.receiving && !self.quiet_sync;
        let problem = self.status.contains("nie powiodło") || self.status.contains("Najpierw") || self.status.contains("Wpisz");
        let keep = if problem { 8.0 } else { 4.0 };
        let age = now - self.status_at;
        if !syncing && (self.status.is_empty() || age > keep) {
            return;
        }
        let text = if syncing { "Synchronizuję dane…".to_string() } else { self.status.clone() };
        // fade out during the last half second
        let alpha = if syncing { 1.0 } else { ((keep - age) / 0.5).clamp(0.0, 1.0) as f32 };
        let pos = match (self.module, self.list_rect) {
            (Module::Mail, Some(r)) => Pos2::new(r.center().x, r.bottom() - 18.0),
            _ => {
                let s = ctx.content_rect();
                Pos2::new(s.left() + 140.0, s.bottom() - 70.0)
            }
        };
        let max_w = self.list_rect.map(|r| r.width() - 24.0).unwrap_or(320.0).max(180.0);
        egui::Area::new(id("sync-toast"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .pivot(Align2::CENTER_BOTTOM)
            .interactable(false)
            .show(ctx, |ui| {
                ui.set_opacity(alpha);
                Frame::new()
                    .fill(BG())
                    .stroke(Stroke::new(1.0, BORDER()))
                    .corner_radius(CornerRadius::same(18))
                    .inner_margin(Margin::symmetric(14, 8))
                    .shadow(egui::epaint::Shadow { offset: [0, 2], blur: 12, spread: 0, color: Color32::from_black_alpha(40) })
                    .show(ui, |ui| {
                        ui.set_max_width(max_w);
                        ui.horizontal(|ui| {
                            if syncing {
                                ui.add(egui::Spinner::new().size(14.0).color(ACCENT()));
                            } else if problem {
                                ui.label(RichText::new("⚠").color(FLAG));
                            } else {
                                ui.label(RichText::new("✓").color(Color32::from_rgb(0x2E, 0x9E, 0x5B)));
                            }
                            ui.add(egui::Label::new(RichText::new(text).size(13.0).color(TEXT())).wrap());
                        });
                    });
            });
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }

    /// A Google client file dropped onto the window.
    pub(super) fn take_dropped_files(&mut self, ctx: &egui::Context) {
        let files = ctx.input(|i| i.raw.dropped_files.clone());
        for f in files {
            let text = f.bytes().ok().and_then(|b| String::from_utf8(b).ok());
            match text.as_deref().and_then(crate::oauth::parse_client_json) {
                Some((id, secret)) => {
                    let a = &mut self.store.account;
                    a.oauth_client_id = id;
                    a.oauth_client_secret = secret;
                    a.use_oauth = true;
                    if a.imap_host.is_empty() {
                        a.use_gmail();
                    }
                    self.oauth_setup = false;
                    self.account_open = true;
                    self.set_status("Wczytano plik z Google — kliknij Zaloguj przez Google");
                }
                None => self.set_status("To nie jest plik klienta z Google Cloud (client_secret_….json)"),
            }
        }
    }

    /// Minimized (or in the tray) the app is in the background: it and its email view hand back
    /// the memory they can; back in front, it works normally again. The email view stays open
    /// (about 170 MB), so the email is there at once when the window comes back.
    pub(super) fn background_memory(&mut self, ctx: &egui::Context) {
        let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        if self.loading.is_some() {
            return;
        }
        if minimized != self.mail_view.in_background() {
            self.mail_view.set_background(minimized);
            if minimized {
                crate::diag::trim_memory();
                crate::diag::mem("in the background, memory handed back");
            } else {
                crate::diag::log("back from the background");
            }
        }
    }

    pub(super) fn keyboard(&mut self, ctx: &egui::Context) {
        // Delete pressed while reading inside the email
        for k in self.mail_view.keys() {
            if k == "clicked" {
                // the email was clicked: it has the keyboard, no field of the app is chosen
                self.email_clicked = self.clock();
                ctx.memory_mut(|m| {
                    if let Some(f) = m.focused() {
                        m.surrender_focus(f);
                    }
                });
                continue;
            }
            crate::diag::log(format!("key from the email view: {k}"));
            if k == "delete" && self.module == Module::Mail {
                if let Some(id) = self.selected {
                    self.actions.push(Action::Delete(id));
                }
            }
        }
        // A field of the app is chosen (search, …) but the keyboard is in the email view, which
        // nobody clicked: it took it by itself. After a moment (a real click reports itself
        // first) the keyboard goes back to the field.
        let field = ctx.memory(|m| m.focused().is_some());
        let inside = self.main_hwnd.is_some_and(|h| focus_state(h) == "inside it");
        if field && inside && self.clock() - self.email_clicked > 1.0 {
            let since = *self.kb_stray.get_or_insert(self.clock());
            if self.clock() - since > 0.4 {
                self.kb_stray = None;
                self.take_keyboard_back();
                crate::diag::log("keyboard: the email view had it without a click; back to the chosen field");
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        } else {
            self.kb_stray = None;
        }
        // a click on the app's own part (list, folders, buttons) takes the keyboard back from
        // the email view, so Delete and the arrows work on the list again
        if ctx.input(|i| i.pointer.any_pressed()) {
            self.take_keyboard_back();
        }
        if ctx.memory(|m| m.focused().is_some()) {
            return;
        }
        let (del, up, down, n, r) = ctx.input(|i| {
            (
                i.key_pressed(Key::Delete),
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::ArrowDown),
                i.modifiers.command && i.key_pressed(Key::N),
                i.modifiers.command && i.key_pressed(Key::R),
            )
        });
        if n {
            self.actions.push(Action::NewMessage);
        }
        if self.module != Module::Mail {
            return;
        }
        if let Some(id) = self.selected {
            if del {
                self.actions.push(Action::Delete(id));
            }
            if r {
                self.actions.push(Action::Reply(id, false));
            }
            if up || down {
                let ids = self.visible_ids();
                if let Some(p) = ids.iter().position(|x| *x == id) {
                    let np = if up { p.saturating_sub(1) } else { (p + 1).min(ids.len() - 1) };
                    self.actions.push(Action::Select(ids[np]));
                }
            }
        }
    }

    /// The keyboard back to the main window (from the email view inside it).
    pub(super) fn take_keyboard_back(&self) {
        if let Some(h) = self.main_hwnd {
            if keyboard_to(h) {
                crate::diag::log("keyboard: taken back from the email view");
            }
        }
    }

    pub(super) fn screenshot_step(&mut self, ctx: &egui::Context) {
        let Some(path) = self.shot.clone() else { return };
        self.frames += 1;
        ctx.request_repaint();
        let compose_shot = matches!(std::env::var("POCZTA_START").as_deref(), Ok("reply") | Ok("suggest")) && !self.composers.is_empty();
        if !compose_shot {
            shoot_step(ctx, &path, self.frames, &self.shot_image);
        }
        if self.frames > 900 + std::env::var("POCZTA_SHOT_FRAMES").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0) {
            eprintln!("Screenshot was not delivered in time");
            std::process::exit(1);
        }
    }
}

/// Picture-taking for `--screenshot`: asks for a picture of the current window after 120 frames,
/// saves it as PNG and closes the app.
pub(super) fn shoot_step(ctx: &egui::Context, path: &std::path::Path, frames: u32, state: &ShotState) {
    let wait = std::env::var("POCZTA_SHOT_FRAMES").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(120);
    if frames >= wait {
        let mut s = state.lock().unwrap();
        if !s.0 {
            s.0 = true;
            let slot = state.clone();
            let ctx2 = ctx.clone();
            ctx.request_screenshot(move |img| {
                slot.lock().unwrap().1 = Some(img);
                ctx2.request_repaint();
            });
        }
    }
    let img = state.lock().unwrap().1.take();
    if let Some(img) = img {
        let [w, h] = img.size;
        let mut bytes = Vec::with_capacity(w * h * 4);
        for p in &img.pixels {
            bytes.extend_from_slice(&p.to_srgba_unmultiplied());
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match image::save_buffer(path, &bytes, w as u32, h as u32, image::ColorType::Rgba8) {
            Ok(()) => println!("Saved screenshot to {}", path.display()),
            Err(e) => eprintln!("Could not save screenshot: {e}"),
        }
        std::process::exit(0);
    }
}

pub(super) type ShotState = Arc<Mutex<(bool, Option<Arc<egui::ColorImage>>)>>;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum WinButton {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// Minimize / maximize / close, drawn inside the app's own top bar.
pub(super) fn window_button(ui: &mut Ui, kind: WinButton) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(44.0, 34.0), Sense::click());
    let p = ui.painter();
    let hot = resp.hovered();
    if hot {
        let fill = if kind == WinButton::Close { Color32::from_rgb(0xE8, 0x11, 0x23) } else { SELECTED() };
        p.rect_filled(r, CornerRadius::same(4), fill);
    }
    let col = if hot && kind == WinButton::Close { Color32::WHITE } else { ICON() };
    let s = Stroke::new(1.2, col);
    let c = r.center();
    let h = 5.5;
    match kind {
        WinButton::Minimize => {
            p.hline(c.x - h..=c.x + h, c.y, s);
        }
        WinButton::Maximize => {
            p.rect_stroke(Rect::from_center_size(c, Vec2::splat(h * 2.0)), CornerRadius::same(1), s, egui::StrokeKind::Middle);
        }
        WinButton::Restore => {
            let back = Rect::from_center_size(c + Vec2::new(2.0, -2.0), Vec2::splat(h * 1.7));
            let front = Rect::from_center_size(c + Vec2::new(-1.5, 1.5), Vec2::splat(h * 1.7));
            p.hline(back.left()..=back.right(), back.top(), s);
            p.vline(back.right(), back.top()..=back.bottom(), s);
            p.rect_filled(front, CornerRadius::ZERO, if hot { SELECTED() } else { BG() });
            p.rect_stroke(front, CornerRadius::same(1), s, egui::StrokeKind::Middle);
        }
        WinButton::Close => {
            p.line_segment([c + Vec2::new(-h, -h), c + Vec2::new(h, h)], s);
            p.line_segment([c + Vec2::new(-h, h), c + Vec2::new(h, -h)], s);
        }
    }
    resp
}

/// Without the system frame the window needs its own resize edges and a thin outline.
pub(super) fn window_edges(ui: &mut Ui) {
    use egui::{CursorIcon, ResizeDirection as D};
    let ctx = ui.ctx().clone();
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    let screen = ctx.content_rect();
    if !maximized {
        ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, id("window-outline")))
            .rect_stroke(screen, CornerRadius::ZERO, Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
    } else {
        return;
    }
    let e = 5.0;
    let c = 12.0;
    let (l, r, t, b) = (screen.left(), screen.right(), screen.top(), screen.bottom());
    let zones = [
        (Rect::from_min_max(Pos2::new(l, t), Pos2::new(l + c, t + c)), D::NorthWest, CursorIcon::ResizeNorthWest),
        (Rect::from_min_max(Pos2::new(r - c, t), Pos2::new(r, t + c)), D::NorthEast, CursorIcon::ResizeNorthEast),
        (Rect::from_min_max(Pos2::new(l, b - c), Pos2::new(l + c, b)), D::SouthWest, CursorIcon::ResizeSouthWest),
        (Rect::from_min_max(Pos2::new(r - c, b - c), Pos2::new(r, b)), D::SouthEast, CursorIcon::ResizeSouthEast),
        (Rect::from_min_max(Pos2::new(l + c, t), Pos2::new(r - c, t + e)), D::North, CursorIcon::ResizeNorth),
        (Rect::from_min_max(Pos2::new(l + c, b - e), Pos2::new(r - c, b)), D::South, CursorIcon::ResizeSouth),
        (Rect::from_min_max(Pos2::new(l, t + c), Pos2::new(l + e, b - c)), D::West, CursorIcon::ResizeWest),
        (Rect::from_min_max(Pos2::new(r - e, t + c), Pos2::new(r, b - c)), D::East, CursorIcon::ResizeEast),
    ];
    for (i, (rect, dir, cursor)) in zones.into_iter().enumerate() {
        let resp = ui.interact(rect, id(("resize-edge", i)), Sense::drag()).on_hover_cursor(cursor);
        if resp.drag_started_by(egui::PointerButton::Primary) {
            ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
        }
    }
}

/// Where the keyboard is now, for the log ("this window", "inside it", "elsewhere", "none").
pub(super) fn focus_state(h: isize) -> &'static str {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetFocus;
        use windows_sys::Win32::UI::WindowsAndMessaging::IsChild;
        let now = GetFocus();
        return if now.is_null() {
            "none"
        } else if now as isize == h {
            "this window"
        } else if IsChild(h as _, now) != 0 {
            "inside it"
        } else {
            "elsewhere"
        };
    }
    #[allow(unreachable_code)]
    {
        let _ = h;
        "unknown"
    }
}

/// The keyboard to window `h` itself (it was clicked), wherever it is now.
pub(super) fn window_keyboard(h: isize) {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
        SetFocus(h as _);
    }
    let _ = h;
}

/// The keyboard to window `h`, when it is in a browser view inside it (a click on the app's own
/// part of that window). Never from another window. True when it moved.
pub(super) fn keyboard_to(h: isize) -> bool {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
        use windows_sys::Win32::UI::WindowsAndMessaging::IsChild;
        let now = GetFocus();
        if !now.is_null() && now as isize != h && IsChild(h as _, now) != 0 {
            SetFocus(h as _);
            return true;
        }
    }
    let _ = h;
    false
}

pub(super) enum TrayMsg {
    Show,
    Quit,
}

/// The icon next to the clock: a click brings the window back, its menu shows or quits.
pub(super) fn make_tray(ctx: &egui::Context) -> (Option<tray_icon::TrayIcon>, Receiver<TrayMsg>) {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    let (tx, rx) = channel();
    let show = MenuItem::new("Pokaż Pocztę", true, None);
    let quit = MenuItem::new("Zakończ", true, None);
    let menu = Menu::new();
    let _ = menu.append_items(&[&show, &PredefinedMenuItem::separator(), &quit]);
    let big = image::RgbaImage::from_raw(crate::icon::SIZE, crate::icon::SIZE, crate::icon::rgba());
    let icon = big
        .map(|b| image::imageops::resize(&b, 32, 32, image::imageops::FilterType::Lanczos3))
        .and_then(|small| tray_icon::Icon::from_rgba(small.into_raw(), 32, 32).ok());
    let mut builder = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("Poczta").with_menu_on_left_click(false);
    if let Some(i) = icon {
        builder = builder.with_icon(i);
    }
    let tray = match builder.build() {
        Ok(t) => Some(t),
        Err(e) => {
            crate::diag::log(format!("tray icon not available: {e}"));
            None
        }
    };
    let (quit_id, t2, c2) = (quit.id().clone(), tx.clone(), ctx.clone());
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let _ = t2.send(if e.id == quit_id { TrayMsg::Quit } else { TrayMsg::Show });
        c2.request_repaint();
    }));
    let c3 = ctx.clone();
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
            let _ = tx.send(TrayMsg::Show);
            c3.request_repaint();
        }
    }));
    (tray, rx)
}

/// The window is minimized or (while minimizing) tiny: its sizes are not worth remembering.
pub(super) fn minimized_or_tiny(ctx: &egui::Context) -> bool {
    let (min, size) = ctx.input(|i| (i.viewport().minimized.unwrap_or(false), i.content_rect().size()));
    min || size.x < 700.0 || size.y < 400.0
}
