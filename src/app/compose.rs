use super::*;

pub(super) struct Compose {
    pub(super) key: u64,
    pub(super) draft_id: Option<u64>,
    pub(super) to: String,
    pub(super) cc: String,
    pub(super) show_cc: bool,
    pub(super) subject: String,
    pub(super) body: String,
    pub(super) error: Option<String>,
    /// The message being answered or forwarded, shown under the editor and appended when sending.
    pub(super) original: Option<Original>,
    pub(super) show_original: bool,
    pub(super) focused: bool,
    /// add the signature when sending
    pub(super) signature: bool,
    /// pictures pasted or dropped into the text: (content id, file)
    pub(super) images: Vec<(String, String)>,
    /// showing the message as it will look instead of the editor
    pub(super) preview: bool,
    /// the formatted writing field (Edge engine): its starting page, the field once placed in
    /// the window, and what it holds now (formatted, plain)
    pub(super) page: Option<String>,
    pub(super) web: Option<crate::compose_view::ComposeView>,
    pub(super) web_failed: bool,
    pub(super) web_html: Option<String>,
    /// changed by the person since it opened
    pub(super) web_edited: bool,
    pub(super) web_tries: u32,
    /// send / save / close waiting for the field's last content: (what, question to wait for)
    pub(super) pending: Option<(&'static str, u64)>,
    /// „Popraw z AI” on its way: the corrected text comes back here
    pub(super) ai: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    /// files attached to the message
    pub(super) files: Vec<String>,
    /// the window for choosing files is open: the chosen files come back here
    pub(super) picking: Option<std::sync::mpsc::Receiver<Vec<String>>>,
    /// files are being dragged over the window
    pub(super) drag_over: bool,
    /// what the window started with (to tell whether anything was written)
    pub(super) start_html: Option<String>,
    pub(super) start_body: String,
    pub(super) start_subject: String,
    pub(super) start_files: usize,
    /// closing with something written: asking "zachować jako szkic?"
    pub(super) ask_close: bool,
    /// a picture of the writing field, taken when an address box is chosen: shown in the field's
    /// place while the list of suggestions lies over it (the text below never disappears)
    pub(super) under_shot: Arc<std::sync::Mutex<Option<Vec<u8>>>>,
    pub(super) under_tex: Option<egui::TextureHandle>,
    pub(super) under_asked: bool,
    /// the writing field was clicked (it keeps the keyboard until a field above is chosen)
    pub(super) editor_clicked: bool,
    /// since when the writing field has had the keyboard while a field above is chosen
    pub(super) kb_stray: Option<std::time::Instant>,
    /// (test log: the chosen field last frame)
    pub(super) focus_seen: Option<egui::Id>,
    /// the Windows window of this message window (found once)
    pub(super) hwnd: Option<isize>,
    /// the text was clicked while hidden under the list: it gets the keyboard once shown
    pub(super) focus_editor: bool,
    /// the keyboard was inside the writing field last time (to see it go in)
    pub(super) kb_inside: bool,
    /// where the writing field is (last drawn)
    pub(super) editor_rect: Option<Rect>,
    /// the mail account it is sent from (0: the main one)
    pub(super) account: u32,
    /// a picture of the writing field, shown under the question (so nothing disappears)
    pub(super) shot: Arc<std::sync::Mutex<Option<Vec<u8>>>>,
    pub(super) shot_tex: Option<egui::TextureHandle>,
}

#[derive(Clone)]
pub(super) struct Original {
    pub(super) forward: bool,
    pub(super) from_name: String,
    pub(super) from_addr: String,
    pub(super) to: String,
    pub(super) date: NaiveDateTime,
    pub(super) subject: String,
    pub(super) body: String,
    pub(super) attachments: Vec<String>,
    /// the formatted version and its pictures, for quoting it as it looked
    pub(super) html_path: String,
    pub(super) inline: Vec<(String, String)>,
    /// its attached files on disk (sent along when forwarding)
    pub(super) files: Vec<String>,
}

impl Original {
    pub(super) fn from(m: &Message, forward: bool) -> Self {
        Self {
            html_path: m.html_path.clone(),
            inline: m.inline.clone(),
            forward,
            from_name: m.from_name.clone(),
            from_addr: m.from_addr.clone(),
            to: m.to.clone(),
            date: m.date,
            subject: m.subject.clone(),
            body: m.body.clone(),
            attachments: m.attachments.clone(),
            files: m.attachment_files.clone(),
        }
    }

    pub(super) fn title(&self) -> &'static str {
        if self.forward { "Przekazana wiadomość" } else { "Wiadomość oryginalna" }
    }

    /// The quoted message as it looked (its formatted text under a short header), for the
    /// formatted part of the reply; None when it has no formatted version.
    pub(super) fn as_html(&self) -> Option<String> {
        if self.html_path.is_empty() {
            return None;
        }
        let page = std::fs::read_to_string(&self.html_path).ok()?;
        let lower = page.to_ascii_lowercase();
        let inner = match (lower.find("<body"), lower.rfind("</body>")) {
            (Some(b), Some(e)) if b < e => {
                let start = lower[b..].find('>').map(|i| b + i + 1).unwrap_or(b);
                &page[start..e]
            }
            _ => page.as_str(),
        };
        let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let head = format!(
            "<div style=\"font-family:Arial,sans-serif;font-size:13px;color:#555\">------ {} ------<br>Nadawca: \"{}\" &lt;{}&gt;<br>Do: {}<br>Data: {}<br>Temat: {}</div><br>",
            self.title(),
            esc(&self.from_name),
            esc(&self.from_addr),
            esc(&self.to),
            self.date.format("%d-%m-%Y %H:%M:%S"),
            esc(&self.subject)
        );
        let body = if self.forward {
            inner.to_string()
        } else {
            format!("<blockquote style=\"margin:0 0 0 6px;padding-left:10px;border-left:2px solid #ccc\">{inner}</blockquote>")
        };
        Some(format!("{head}{body}"))
    }

    /// Text added below the reply when it is sent or saved.
    pub(super) fn as_text(&self) -> String {
        let mut s = format!(
            "\n\n------ {} ------\nNadawca: \"{}\" <{}>\nDo: {}\nData: {}\nTemat: {}\n\n",
            self.title(),
            self.from_name,
            self.from_addr,
            self.to,
            self.date.format("%d-%m-%Y %H:%M:%S"),
            self.subject
        );
        for l in self.body.lines() {
            if !self.forward {
                s.push_str("> ");
            }
            s.push_str(l);
            s.push('\n');
        }
        s
    }
}

impl MailApp {
    /// Looks for the signature in sent mail (in the background) whenever new sent mail arrived.
    pub(super) fn check_signature(&mut self) {
        if let Some(rx) = &self.sig_rx {
            if let Ok(found) = rx.try_recv() {
                self.sig_rx = None;
                let a = &mut self.store.account;
                match found {
                    Some(f) if a.signature_auto => {
                        if a.signature_text != f.text || a.signature_html != f.html {
                            crate::diag::log(format!("signature found in sent mail ({} lines)", f.text.lines().count()));
                            a.signature_html = f.html;
                            a.signature_text = f.text;
                            a.signature_images = f.images;
                            self.save_now = true;
                        }
                    }
                    _ => {}
                }
            }
            return;
        }
        if self.loading.is_some() || !self.store.account.signature_auto || (self.shot.is_some() && std::env::var("POCZTA_SIG_TEST").is_err()) {
            return;
        }
        let n = self.counts().folder_count.get(&Folder::Sent).copied().unwrap_or(0);
        if n == 0 || n == self.sig_seen {
            return;
        }
        self.sig_seen = n;
        let mut recent: Vec<(NaiveDateTime, u64)> = self.store.messages.iter().filter(|m| m.folder == Folder::Sent).map(|m| (m.date, m.id)).collect();
        recent.sort_by(|a, b| b.0.cmp(&a.0));
        recent.truncate(40);
        for (_, id) in &recent {
            self.ensure_body(*id);
        }
        let sent: Vec<Message> = recent.iter().filter_map(|(_, id)| self.msg(*id).cloned()).collect();
        let (tx, rx) = channel();
        self.sig_rx = Some(rx);
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let found = std::panic::catch_unwind(|| crate::signature::detect(&sent)).ok().flatten();
            let _ = tx.send(found);
            ctx.request_repaint();
        });
    }

    /// The signature as it will be sent: its formatting and graphics, via the email renderer.
    pub(super) fn signature_preview(&mut self, ui: &mut Ui) {
        let a = &self.store.account;
        if a.signature_html.trim().is_empty() {
            ui.label(RichText::new(a.signature_text.clone()).size(12.5).color(TEXT()));
            return;
        }
        let html = a.signature_html.clone();
        let blocks = match &self.sig_blocks {
            Some((h, b)) if *h == html => b.clone(),
            _ => {
                let b = Arc::new(html::parse(&format!("<html><body>{html}</body></html>")));
                self.sig_blocks = Some((html, b.clone()));
                b
            }
        };
        let mut m = Message::blank();
        m.inline = self.store.account.signature_images.clone();
        self.render_html(ui, &m, &blocks);
    }

    pub(super) fn open_composer(&mut self, to: String, cc: String, subject: String, body: String, draft_id: Option<u64>, original: Option<Original>) {
        let key = self.next_key;
        self.next_key += 1;
        // the account it goes from (a reply: the account the message came to), said just before
        let account = std::mem::take(&mut self.compose_account).min(self.store.extra.len() as u32);
        // the formatted writing field starts with the text, the signature and the quoted message
        // (a draft written there before comes back exactly as it was left)
        // The app's own writing field (Rust, in the window itself) is the one used: a browser view
        // in the window took the keyboard from "Do" / "Temat" now and then. The browser field
        // stays only on request (POCZTA_WEB_EDITOR).
        let page = if self.mail_view.failed || std::env::var("POCZTA_PLAIN_EDITOR").is_ok() {
            None
        } else {
            let draft_html = draft_id
                .and_then(|id| self.msg(id))
                .filter(|m| m.folder == Folder::Drafts && !m.html_path.is_empty())
                .and_then(|m| std::fs::read_to_string(&m.html_path).ok());
            let draft_html = draft_html.or_else(|| self.compose_start.clone());
            let content = match draft_html {
                Some(h) => crate::compose_view::body_of(&h),
                None => {
                    let a = &self.store.account;
                    // (the signature found in sent mail is the main account's)
                    let sig = (account == 0 && a.signature_on && !a.signature_text.trim().is_empty()).then(|| {
                        let h = if a.signature_html.trim().is_empty() { crate::signature::text_to_html(&a.signature_text) } else { a.signature_html.clone() };
                        crate::compose_view::show_cids(&h, &a.signature_images)
                    });
                    let quote = original.as_ref().map(|o| {
                        let html = (!o.html_path.is_empty())
                            .then(|| std::fs::read_to_string(&o.html_path).ok())
                            .flatten()
                            .map(|h| crate::compose_view::show_cids(&crate::compose_view::body_of(&h), &o.inline));
                        let from = if o.from_name.is_empty() { o.from_addr.clone() } else { format!("{} <{}>", o.from_name, o.from_addr) };
                        crate::compose_view::quote_html(o.title(), &from, &o.to, &o.date.format("%d-%m-%Y %H:%M").to_string(), &o.subject, html, &o.body)
                    });
                    let text = if body.trim().is_empty() { String::new() } else { crate::signature::text_to_html(&body) };
                    crate::compose_view::start_content(&text, sig, quote)
                }
            };
            Some(crate::compose_view::page(&content))
        };
        // attached files: those of the draft, or (forwarding) those of the forwarded message
        let files: Vec<String> = match (draft_id.and_then(|id| self.msg(id)), &original) {
            (Some(d), _) if d.folder == Folder::Drafts => d.attachment_files.clone(),
            (_, Some(o)) if o.forward => o.files.clone(),
            _ => vec![],
        }
        .into_iter()
        .filter(|f| std::path::Path::new(f).is_file())
        .collect();
        self.composers.push(Compose {
            start_html: None,
            start_body: body.clone(),
            start_subject: subject.clone(),
            start_files: files.len(),
            ask_close: false,
            under_shot: Default::default(),
            under_tex: None,
            under_asked: false,
            editor_clicked: false,
            kb_stray: None,
            focus_seen: None,
            hwnd: None,
            focus_editor: false,
            kb_inside: false,
            editor_rect: None,
            account,
            shot: Default::default(),
            shot_tex: None,
            key,
            draft_id,
            show_cc: !cc.is_empty(),
            to,
            cc,
            subject,
            body,
            error: None,
            original,
            show_original: true,
            focused: false,
            signature: account == 0 && self.store.account.signature_on && !self.store.account.signature_text.trim().is_empty(),
            images: vec![],
            preview: false,
            page,
            web: None,
            web_failed: false,
            web_html: None,
            web_edited: false,
            web_tries: 0,
            pending: None,
            ai: None,
            files,
            picking: None,
            drag_over: false,
        });
    }

    pub(super) fn composers_ui(&mut self, ctx: &egui::Context) {
        let mut closed = vec![];
        if !self.composers.is_empty() {
            self.mail_contacts();
        }
        let contacts = self.contacts.clone();
        let can_send = self.can_connect();
        let sig_text = self.store.account.signature_text.clone();
        let acc_name = self.store.account.display_name.clone();
        let _acc_email = self.store.account.email.clone();
        // every account (for "Od:"), the main one first
        let accounts: Vec<String> = (0..=self.store.extra.len() as u32).map(|a| {
            let x = self.account_ref(a);
            format!("{} <{}>", x.display_name, x.email)
        }).collect();
        let main_pos = ctx.input(|inp| inp.viewport().outer_rect).map(|r| r.min);
        // Test option: with POCZTA_START=reply the picture is taken of the reply window.
        let start = std::env::var("POCZTA_START").unwrap_or_default();
        let shoot_compose = self.shot.is_some() && (start == "reply" || start == "suggest");
        let shot_path = self.shot.clone().unwrap_or_default();
        let shot_frames = self.frames;
        let shot_image = self.shot_image.clone();
        // the windows are taken out while drawn, so the signature can be shown with the email renderer
        let mut composers = std::mem::take(&mut self.composers);
        for (i, c) in composers.iter_mut().enumerate() {
            let mut open = true;
            let title = if c.subject.trim().is_empty() { "Nowa wiadomość".to_string() } else { c.subject.clone() };
            let mut outcome: Option<&'static str> = None;
            let from_acc = self.account_for(c.account);
            let from_line = format!("{} <{}>", from_acc.display_name, from_acc.email);
            // Each message gets its own frameless window that can be moved anywhere on screen.
            let mut builder = egui::ViewportBuilder::default()
                .with_title(format!("{title}{}", crate::compose_view::marker(c.key)))
                .with_inner_size([680.0, 660.0])
                .with_min_inner_size([420.0, 300.0])
                .with_decorations(false)
                .with_icon(self.icon.clone());
            if let Some(main) = main_pos {
                builder = builder.with_position(main + Vec2::new(380.0 + 30.0 * i as f32, 60.0 + 30.0 * i as f32));
            }
            let shoot_here = shoot_compose && i == 0;
            ctx.show_viewport_immediate(egui::ViewportId::from_hash_of(("compose", c.key)), builder, |root, _class| {
                if root.ctx().input(|inp| inp.viewport().close_requested()) {
                    open = false;
                }
                // a click on the window's own part (Do, DW, Temat, buttons) takes the keyboard back
                // from the writing field (Windows leaves it there otherwise: the field looks
                // chosen, but the letters would go into the text)
                if std::env::var("POCZTA_FOCUS_LOG").is_ok() {
                    let evs: Vec<String> = root.ctx().input(|inp| inp.events.iter().filter(|e| !matches!(e, egui::Event::PointerMoved(_))).map(|e| format!("{e:?}").chars().take(60).collect()).collect());
                    if !evs.is_empty() {
                        crate::diag::log(format!("focus test: events {evs:?}"));
                    }
                }
                if std::env::var("POCZTA_FOCUS_LOG").is_ok() && root.ctx().input(|inp| inp.pointer.any_pressed() || !inp.events.is_empty() && inp.events.iter().any(|e| matches!(e, egui::Event::Text(_)))) {
                    let h = crate::compose_view::find_window(&crate::compose_view::marker(c.key)).unwrap_or(0);
                    crate::diag::log(format!("focus test: press/text in message window; marked active: {}, keyboard: {}, field focused: {:?}", root.ctx().input(|inp| inp.focused), focus_state(h), root.ctx().memory(|m| m.focused())));
                }
                // Who has the keyboard: the writing field (a browser view) or the fields above.
                // When it goes into the writing field, it is the person's click when the pointer is
                // over the text (the fields above let go); otherwise the field took it by itself
                // (while being made or loading) and it comes back to the field chosen above.
                // Windows says where the keyboard is: when it is in this window, the window counts
                // as active (it gets no such word when the keyboard comes back to it from the
                // writing field, and a chosen field would then show no frame and no cursor)
                if c.hwnd.is_none() {
                    c.hwnd = crate::compose_view::find_window(&crate::compose_view::marker(c.key));
                }
                if c.hwnd.is_some_and(|h| focus_state(h) == "this window") && !root.ctx().input(|i| i.focused) {
                    root.ctx().input_mut(|i| i.focused = true);
                }
                if std::env::var("POCZTA_FOCUS_LOG").is_ok() {
                    let now_f = root.ctx().memory(|m| m.focused());
                    if now_f != c.focus_seen {
                        let evs: Vec<String> = root.ctx().input(|inp| inp.events.iter().map(|e| format!("{e:?}").chars().take(40).collect()).collect());
                        crate::diag::log(format!("focus test: chosen field {:?} -> {:?}; events {evs:?}; editor clicked {}", c.focus_seen, now_f, c.editor_clicked));
                        c.focus_seen = now_f;
                    }
                }
                // (the writing field's own page says when it was clicked and when it got the
                // keyboard without a click: see Msg::Clicked / Msg::Away)
                if c.web.is_some() {
                    if c.hwnd.is_none() {
                        c.hwnd = crate::compose_view::find_window(&crate::compose_view::marker(c.key));
                    }
                    if root.ctx().input(|inp| inp.pointer.any_pressed()) {
                        // a click on the window's own part: a field above, a button
                        c.editor_clicked = false;
                    }
                    // a field above is chosen, the writing field has the keyboard and was not clicked:
                    // after a moment (its click reports itself first) the keyboard goes back
                    let field = root.ctx().memory(|m| m.focused().is_some());
                    let inside = c.hwnd.is_some_and(|h| focus_state(h) == "inside it");
                    if field && inside && !c.editor_clicked {
                        let now = std::time::Instant::now();
                        let since = *c.kb_stray.get_or_insert(now);
                        if now.duration_since(since).as_millis() > 400 {
                            c.kb_stray = None;
                            if c.hwnd.is_some_and(keyboard_to) {
                                crate::diag::log("message window: the writing field had the keyboard without a click; back to the chosen field");
                            }
                        } else {
                            root.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                        }
                    } else {
                        c.kb_stray = None;
                    }
                }
                if c.web.is_some() && root.ctx().input(|inp| inp.pointer.any_pressed()) {
                    if let Some(h) = crate::compose_view::find_window(&crate::compose_view::marker(c.key)) {
                        let moved = keyboard_to(h);
                        // the window must also know it has the keyboard again, or the chosen field
                        // shows no frame and no cursor (though typing works)
                        if !root.ctx().input(|inp| inp.focused) {
                            let state = focus_state(h);
                            crate::diag::log(format!("message window: clicked while not marked active (keyboard: {state}, taken back: {moved})"));
                            window_keyboard(h);
                            root.ctx().send_viewport_cmd(egui::ViewportCommand::Focus);
                        }
                    }
                }
                egui::CentralPanel::default()
                    .frame(Frame::new().fill(BG()).inner_margin(Margin { left: 10, right: 4, top: 6, bottom: 10 }))
                    .show(root, |ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    // Line 1 doubles as the title bar: drag to move, double-click to maximize.
                    let bar = Rect::from_min_size(ui.max_rect().min - Vec2::new(10.0, 6.0), Vec2::new(ui.max_rect().width() + 14.0, 44.0));
                    let drag = ui.interact(bar, id(("compose-bar", c.key)), Sense::click_and_drag());
                    if drag.double_clicked() {
                        let maxed = ui.ctx().input(|inp| inp.viewport().maximized.unwrap_or(false));
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!maxed));
                    } else if drag.drag_started_by(egui::PointerButton::Primary) {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    // Line 1: send, from, save / discard, window buttons.
                    ui.horizontal(|ui| {
                        let send = ui.add(
                            egui::Button::new(RichText::new("➤  Wyślij").color(Color32::WHITE).size(14.0))
                                .fill(PRIMARY)
                                .min_size(Vec2::new(92.0, 30.0)),
                        );
                        let send = if can_send { send } else { send.on_hover_text("Offline: wiadomość poczeka w skrzynce nadawczej") };
                        if send.clicked() {
                            outcome = Some("send");
                        }
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
                        avatar(ui.painter(), r.center(), 11.0, if from_acc.display_name.trim().is_empty() { &acc_name } else { &from_acc.display_name });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            let maxed = ui.ctx().input(|inp| inp.viewport().maximized.unwrap_or(false));
                            if window_button(ui, WinButton::Close).on_hover_text("Zamknij").clicked() {
                                open = false;
                            }
                            let kind = if maxed { WinButton::Restore } else { WinButton::Maximize };
                            if window_button(ui, kind).clicked() {
                                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!maxed));
                            }
                            if window_button(ui, WinButton::Minimize).on_hover_text("Minimalizuj").clicked() {
                                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                            }
                            ui.add_space(10.0);
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if ui.add(egui::Button::new("🗑").frame(false)).on_hover_text("Odrzuć").clicked() {
                                outcome = Some("discard");
                            }
                            if ui.add(egui::Button::new("💾").frame(false)).on_hover_text("Zapisz szkic").clicked() {
                                outcome = Some("draft");
                            }
                            ui.add_space(8.0);
                            let clip = ui.add_enabled(c.picking.is_none(), egui::Button::new(RichText::new("📎 Załącz").size(13.5)).frame(false));
                            if clip.on_hover_text("Załącz pliki (możesz je też przeciągnąć na to okno)").clicked() {
                                let owner = crate::compose_view::find_window(&crate::compose_view::marker(c.key));
                                let (tx, rx) = std::sync::mpsc::channel();
                                let rctx = ui.ctx().clone();
                                std::thread::spawn(move || {
                                    let _ = tx.send(crate::compose_view::pick_files(owner));
                                    rctx.request_repaint();
                                });
                                c.picking = Some(rx);
                            }
                            // the sender, in the room left between the buttons (shortened, never under them)
                            ui.add_space(10.0);
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                let room = ui.available_width().max(40.0);
                                if accounts.len() > 1 {
                                    // several accounts: which one it is sent from
                                    let want = text_width(ui, &from_line, FontId::proportional(14.0)) + 34.0;
                                    egui::ComboBox::from_id_salt(("from", c.key))
                                        .width(want.min(room))
                                        .truncate()
                                        .selected_text(RichText::new(&from_line).color(MUTED()))
                                        .show_ui(ui, |ui| {
                                            for (k, label) in accounts.iter().enumerate() {
                                                ui.selectable_value(&mut c.account, k as u32, label);
                                            }
                                        })
                                        .response
                                        .on_hover_text(&from_line);
                                } else {
                                    ui.add(egui::Label::new(RichText::new(&from_line).color(MUTED())).truncate());
                                }
                            });
                        });
                    });
                    // Lines 2–3: recipients and subject, each on one line.
                    let label_w = 52.0;
                    let field = |ui: &mut Ui, label: &str, text: &mut String, hint: &str, extra: f32| {
                        ui.horizontal(|ui| {
                            ui.add_sized([label_w, 24.0], egui::Label::new(RichText::new(label).color(MUTED())));
                            ui.add(egui::TextEdit::singleline(text).hint_text(hint).desired_width(ui.available_width() - extra));
                        });
                    };
                    ui.horizontal(|ui| {
                        ui.add_sized([label_w, 24.0], egui::Label::new(RichText::new("Do:").color(MUTED())));
                        let extra = if c.show_cc {
                            0.0
                        } else {
                            text_width(ui, "Dodaj DW", FontId::proportional(13.5)) + ui.spacing().item_spacing.x + 4.0
                        };
                        let w = ui.available_width() - extra;
                        let r = crate::contacts::recipients_field(ui, &mut c.to, "Wpisz imię lub adres…", w, &contacts, ("to", c.key));
                        if start == "suggest" {
                            // test picture: typing in the recipients box
                            c.focused = true;
                            r.request_focus();
                        }
                        if !c.show_cc && ui.link("Dodaj DW").clicked() {
                            c.show_cc = true;
                        }
                    });
                    if c.show_cc {
                        ui.horizontal(|ui| {
                            ui.add_sized([label_w, 24.0], egui::Label::new(RichText::new("DW:").color(MUTED())));
                            let w = ui.available_width();
                            crate::contacts::recipients_field(ui, &mut c.cc, "", w, &contacts, ("cc", c.key));
                        });
                    }
                    field(ui, "Temat:", &mut c.subject, "", 0.0);
                    if let Some(chosen) = c.picking.as_ref().and_then(|rx| rx.try_recv().ok()) {
                        add_files(&mut c.files, chosen);
                        c.picking = None;
                    }
                    if c.picking.is_some() {
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
                    }
                    let web_mode = c.page.is_some() && !c.web_failed && !ui.ctx().embed_viewports();
                    let (hovering, dropped) = ui.input(|i| (!i.raw.hovered_files.is_empty(), i.raw.dropped_files.clone()));
                    // dropped on the top of the window: everything attached (on the simple editor,
                    // pictures go into the text there)
                    let dropped: Vec<String> = dropped
                        .iter()
                        .map(|f| f.path().to_string_lossy().into_owned())
                        .filter(|p| !p.is_empty() && std::path::Path::new(p).is_file())
                        .filter(|p| web_mode || !crate::compose_view::is_picture(p))
                        .collect();
                    add_files(&mut c.files, dropped);
                    if hovering || c.drag_over {
                        let r = ui.max_rect();
                        ui.painter().rect_stroke(r.shrink(2.0), CornerRadius::same(8), Stroke::new(2.0, ACCENT()), egui::StrokeKind::Inside);
                    }
                    if !c.files.is_empty() || hovering {
                        let mut remove: Option<usize> = None;
                        ui.horizontal_wrapped(|ui| {
                            ui.add_sized([label_w, 24.0], egui::Label::new(RichText::new("Pliki:").color(MUTED())));
                            for (k, f) in c.files.iter().enumerate() {
                                let name = std::path::Path::new(f).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                                let size = std::fs::metadata(f).map(|m| human_size(m.len())).unwrap_or_default();
                                Frame::new()
                                    .fill(SURFACE())
                                    .stroke(Stroke::new(1.0, BORDER()))
                                    .corner_radius(CornerRadius::same(5))
                                    .inner_margin(Margin { left: 8, right: 4, top: 2, bottom: 2 })
                                    .show(ui, |ui| {
                                        ui.spacing_mut().item_spacing.x = 4.0;
                                        let label = ui.add(egui::Label::new(RichText::new(format!("📎 {name}")).size(13.0).color(TEXT())).sense(Sense::click()));
                                        if label.on_hover_text(format!("{f}\nKliknij, aby otworzyć")).clicked() {
                                            let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", f]).spawn();
                                        }
                                        ui.label(RichText::new(size).size(11.5).color(MUTED()));
                                        if ui.add(egui::Button::new(RichText::new("✕").size(11.0).color(MUTED())).frame(false)).on_hover_text("Usuń załącznik").clicked() {
                                            remove = Some(k);
                                        }
                                    });
                            }
                            if hovering {
                                ui.label(RichText::new("Upuść, aby załączyć").size(13.0).color(ACCENT()));
                            }
                        });
                        if let Some(k) = remove {
                            c.files.remove(k);
                        }
                    }
                    ui.add_space(2.0);
                    let r = ui.available_rect_before_wrap();
                    ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, BORDER()));
                    ui.add_space(4.0);

                    let use_web = c.page.is_some() && !c.web_failed && !ui.ctx().embed_viewports();
                    if use_web {
                        // the formatted writing field fills the rest of the window (a little margin
                        // keeps the window's edges free for resizing)
                        let footer = if c.error.is_some() { 26.0 } else { 0.0 };
                        let avail = ui.available_rect_before_wrap();
                        let rect = Rect::from_min_max(avail.min + Vec2::new(0.0, 2.0), Pos2::new(avail.right() - 4.0, avail.bottom() - footer - 4.0));
                        ui.allocate_rect(rect, Sense::hover());
                        ui.painter().rect_stroke(rect.expand(1.0), CornerRadius::same(4), Stroke::new(1.0, BORDER()), egui::StrokeKind::Outside);
                        if c.web.is_none() {
                            c.web_tries += 1;
                            if let Some(hwnd) = crate::compose_view::find_window(&crate::compose_view::marker(c.key)) {
                                match crate::compose_view::ComposeView::new(hwnd, c.page.as_deref().unwrap_or(""), ui.ctx().clone()) {
                                    Ok(v) => {
                                        // a reply starts in the text, unless a field above is already being typed in
                                        if !c.to.trim().is_empty() && ui.ctx().memory(|m| m.focused().is_none()) {
                                            c.editor_clicked = true;
                                            v.focus();
                                        }
                                        c.web = Some(v);
                                    }
                                    Err(e) => {
                                        let busy = e.contains("800700AA") || e.contains("w użyciu") || e.to_lowercase().contains("in use");
                                        if busy && c.web_tries < 60 {
                                            // the browser part is busy starting another view: try again in a moment
                                            if c.web_tries == 1 {
                                                crate::diag::log(format!("writing field: browser busy, trying again ({e})"));
                                            }
                                            ui.ctx().request_repaint_after(std::time::Duration::from_millis(150));
                                        } else {
                                            crate::diag::log(format!("writing field: formatted editor not available ({e})"));
                                            c.web_failed = true;
                                        }
                                    }
                                }
                            } else if c.web_tries > 120 {
                                crate::diag::log("writing field: message window not found, using the simple editor");
                                c.web_failed = true;
                            } else {
                                ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
                            }
                        }
                        if let Some(v) = c.web.as_mut() {
                            v.poll(c.pending.is_some());
                            ui.ctx().request_repaint_after(std::time::Duration::from_millis(160));
                            for msg in v.messages() {
                                match msg {
                                    crate::compose_view::Msg::Html { html, text } => {
                                        if std::env::var("POCZTA_ATTACH_TEST").is_ok() && c.files.is_empty() {
                                            // test: two files attached, files being dragged over the field
                                            let dir = std::env::current_dir().unwrap_or_default();
                                            c.files = vec![dir.join("Cargo.toml").to_string_lossy().into_owned(), dir.join("assets").join("poczta.ico").to_string_lossy().into_owned()];
                                            v.run_js("window.pzDrag&&window.pzDrag(true)");
                                        }
                                        if let Ok(how) = std::env::var("POCZTA_AI_TEST") {
                                            use std::sync::atomic::{AtomicBool, Ordering};
                                            static TRIED: AtomicBool = AtomicBool::new(false);
                                            if !TRIED.swap(true, Ordering::Relaxed) {
                                                // a text with mistakes, as if typed, then the menu (or a correction)
                                                v.insert_html("Dzien dobry,<br>prosze zrestartowac komputer i sprawdzic czy drukarka działa. Dedykowany sterownik jest w załączniku.");
                                                v.run_js(if how == "run" {
                                                    "setTimeout(()=>document.querySelector('#pz-aimenu .it[data-s=correct]').click(),300)"
                                                } else {
                                                    "setTimeout(()=>document.getElementById('pz-ai').click(),300)"
                                                });
                                            }
                                            if how == "run" {
                                                crate::diag::log(format!("aitest: {}", text.lines().take(3).collect::<Vec<_>>().join(" | ")));
                                            }
                                        }
                                        if std::env::var("POCZTA_START").as_deref() == Ok("webreply") {
                                            use std::sync::atomic::{AtomicBool, Ordering};
                                            static FILLED: AtomicBool = AtomicBool::new(false);
                                            if !FILLED.swap(true, Ordering::Relaxed) {
                                                // as if typed / pasted: Markdown from an agent and a screenshot
                                                v.insert_html(&crate::rich::to_html("## Podsumowanie

Wpisy są **gotowe** i *sprawdzone*:
- SPF dla `naturbay.de`
- nowy klucz DKIM"));
                                                let png = std::fs::read("assets/poczta.png").unwrap_or_default();
                                                if let Some((_, file, w)) = crate::rich::store_picture(&png, "png") {
                                                    v.insert_image(&file, w);
                                                }
                                            }
                                            let (h, pics) = crate::compose_view::to_email(&html);
                                            crate::diag::log(format!(
                                                "webtest: {} pictures to send, heading {}, bold {}, signature {}, quote {}",
                                                pics.len(),
                                                h.contains("<h2"),
                                                h.contains("<strong>gotowe"),
                                                h.contains("Nowicki"),
                                                h.contains("Wiadomość oryginalna")
                                            ));
                                        }
                                        if std::env::var("POCZTA_FOCUS_LOG").is_ok() && text.contains("XYZQ") {
                                            crate::diag::log("focus test: typed text is in the writing field");
                                        }
                                        if c.web_html.is_some() {
                                            c.web_edited = true;
                                        }
                                        if c.start_html.is_none() {
                                            c.start_html = Some(html.clone());
                                        }
                                        c.web_html = Some(html);
                                        c.body = text;
                                    }
                                    crate::compose_view::Msg::Img { data } => {
                                        if let Some((file, w)) = crate::compose_view::save_data_image(&data) {
                                            v.insert_image(&file, w);
                                        }
                                    }
                                    crate::compose_view::Msg::Md { text } => v.insert_html(&crate::rich::to_html(&text)),
                                    crate::compose_view::Msg::Clicked => {
                                        // the text was clicked: it has the keyboard, the fields above let go
                                        c.editor_clicked = true;
                                        ui.ctx().memory_mut(|m| {
                                            if let Some(f) = m.focused() {
                                                m.surrender_focus(f);
                                            }
                                        });
                                    }
                                    crate::compose_view::Msg::Away => {
                                        // it took the keyboard by itself: back to the window (and the
                                        // field chosen above, which stays chosen)
                                        if !c.editor_clicked {
                                            if let Some(h) = c.hwnd {
                                                if keyboard_to(h) {
                                                    crate::diag::log("message window: keyboard given back by the writing field");
                                                }
                                            }
                                        }
                                    }
                                    crate::compose_view::Msg::Drag(on) => {
                                        c.drag_over = on;
                                        v.run_js(&format!("window.pzDrag&&window.pzDrag({on})"));
                                    }
                                    crate::compose_view::Msg::Dropped(paths) => {
                                        let (pics, other): (Vec<String>, Vec<String>) = paths.into_iter().partition(|p| crate::compose_view::is_picture(p));
                                        for p in pics {
                                            let ext = p.rsplit('.').next().unwrap_or("png").to_lowercase();
                                            if let Some((_, file, w)) = std::fs::read(&p).ok().and_then(|b| crate::rich::store_picture(&b, &ext)) {
                                                v.insert_image(&file, w);
                                            }
                                        }
                                        add_files(&mut c.files, other);
                                    }
                                    crate::compose_view::Msg::Ai { style, html } => {
                                        crate::ai::remember_style(&style);
                                        let (tx, rx) = std::sync::mpsc::channel();
                                        let ctx = ui.ctx().clone();
                                        std::thread::spawn(move || {
                                            let _ = tx.send(crate::ai::rewrite(&crate::ai::load(), &style, &html));
                                            ctx.request_repaint();
                                        });
                                        c.ai = Some(rx);
                                    }
                                }
                            }
                            if let Some(res) = c.ai.as_ref().and_then(|rx| rx.try_recv().ok()) {
                                if let Err(e) = &res {
                                    crate::diag::log(format!("Popraw z AI: {e}"));
                                }
                                v.ai_done(&res);
                                c.ai = None;
                            }
                            // the list of address suggestions opens over the field: the field steps aside
                            if c.ask_close && c.shot_tex.is_none() {
                                let png = c.shot.lock().ok().and_then(|mut g| g.take());
                                if let Some(img) = png.and_then(|p| image::load_from_memory(&p).ok()) {
                                    let img = img.to_rgba8();
                                    let size = [img.width() as usize, img.height() as usize];
                                    c.shot_tex = Some(ui.ctx().load_texture(format!("compose-shot-{}", c.key), egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()), egui::TextureOptions::LINEAR));
                                }
                            }
                            if let (true, Some(tex)) = (c.ask_close, &c.shot_tex) {
                                ui.painter().image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                            }
                            // typing in "Do" / "DW": a picture of the text is taken at once (the
                            // field is still shown), so the list of suggestions can lie over it
                            // (the list of suggestions is a window of its own over the writing field
                            // now: the field never steps aside, nor is it shown as a picture)
                            let typing = false;
                            let listing = false;
                            if typing && !c.under_asked && !listing {
                                c.under_asked = true;
                                c.under_tex = None;
                                v.capture(c.under_shot.clone());
                            } else if !typing && !listing {
                                c.under_asked = false;
                            }
                            if c.under_tex.is_none() {
                                let png = c.under_shot.lock().ok().and_then(|mut g| g.take());
                                if let Some(img) = png.and_then(|p| image::load_from_memory(&p).ok()) {
                                    let img = img.to_rgba8();
                                    let size = [img.width() as usize, img.height() as usize];
                                    c.under_tex = Some(ui.ctx().load_texture(format!("compose-under-{}", c.key), egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()), egui::TextureOptions::LINEAR));
                                }
                            }
                            if listing && !c.ask_close {
                                match &c.under_tex {
                                    Some(tex) => {
                                        ui.painter().image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                                        // a click on the text (its picture, while the list is open): the
                                        // list closes and the text gets the keyboard
                                        let pressed_here = ui.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| rect.contains(p)));
                                        if pressed_here {
                                            c.focus_editor = true;
                                            ui.ctx().memory_mut(|m| {
                                                if let Some(f) = m.focused() {
                                                    m.surrender_focus(f);
                                                }
                                            });
                                        }
                                    }
                                    None => {
                                        // (the picture is a moment away)
                                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(30));
                                    }
                                }
                            }
                            c.editor_rect = Some(rect);
                            if listing && !c.ask_close && c.under_tex.is_none() {
                                // no picture (the text was not in yet): the field stays, just below the list
                                let below = [("to", c.key), ("cc", c.key)].into_iter().filter_map(|s| crate::contacts::list_rect(ui.ctx(), s)).map(|r| r.bottom() + 4.0).fold(rect.top(), f32::max);
                                let rest = Rect::from_min_max(Pos2::new(rect.left(), below.min(rect.bottom() - 40.0)), rect.max);
                                v.place(Some(rest));
                            } else {
                                let covered = (c.ask_close && c.shot_tex.is_some()) || listing;
                                v.place(if covered { None } else { Some(rect) });
                                if c.focus_editor && !covered {
                                    c.focus_editor = false;
                                    c.editor_clicked = true;
                                    v.focus();
                                }
                            }
                        }
                    } else {
                        // Editor: takes all the room, or about half when the original is unfolded.
                        let footer = if c.error.is_some() { 24.0 } else { 0.0 };
                        let orig_head = if c.original.is_some() { 30.0 } else { 0.0 };
                        // the signature gets up to half the window (it scrolls if it is taller still)
                        let sig_box = (ui.available_height() * 0.45).clamp(120.0, 340.0);
                        let sig_h = if sig_text.trim().is_empty() { 0.0 } else if c.signature { 30.0 + sig_box } else { 28.0 };
                        let pics = crate::rich::images(&c.body);
                        let strip_h = if pics.is_empty() { 0.0 } else { 66.0 };
                        let free = (ui.available_height() - footer - orig_head - sig_h - 32.0 - strip_h).max(80.0);
                        let editor_h = if c.original.is_some() && c.show_original { (free * 0.42).max(90.0) } else { free };
                        let hint = if c.original.as_ref().map(|o| !o.forward).unwrap_or(false) { "Napisz odpowiedź…" } else { "Napisz wiadomość…" };
                        let edit_id = id(("cedit", c.key));
                        let n_chars = c.body.chars().count();
                        let sel = egui::text_edit::TextEditState::load(ui.ctx(), edit_id)
                            .and_then(|s| s.cursor.char_range())
                            .map(|r| (r.secondary.index.0.min(n_chars), r.primary.index.0.min(n_chars)))
                            .unwrap_or((n_chars, n_chars));
                        let mut new_sel: Option<(usize, usize)> = None;
                        let has_focus = ui.memory(|mem| mem.has_focus(edit_id));
                        // keyboard: Ctrl+B bold, Ctrl+I italics
                        if has_focus {
                            let (b, it) = ui.input_mut(|i| (i.consume_key(egui::Modifiers::COMMAND, egui::Key::B), i.consume_key(egui::Modifiers::COMMAND, egui::Key::I)));
                            if b {
                                new_sel = Some(crate::rich::wrap(&mut c.body, sel.0, sel.1, "**", "**"));
                            } else if it {
                                new_sel = Some(crate::rich::wrap(&mut c.body, sel.0, sel.1, "*", "*"));
                            }
                        }
                        // pictures pasted (Ctrl+V of a screenshot) or dropped in as files go into the text
                        let (pasted, dropped) = ui.input(|i| {
                            let p: Vec<std::sync::Arc<egui::ColorImage>> = i.events.iter().filter_map(|e| if let egui::Event::PasteImage(img) = e { Some(img.clone()) } else { None }).collect();
                            (p, i.raw.dropped_files.clone())
                        });
                        let mut added: Vec<(String, String, u32)> = pasted.iter().filter_map(|img| crate::rich::store_pasted(img)).collect();
                        for f in &dropped {
                            let path = f.path();
                            if path.as_os_str().is_empty() {
                                continue;
                            }
                            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                            if ["png", "jpg", "jpeg", "gif", "webp", "bmp"].contains(&ext.as_str()) {
                                if let Some(p) = std::fs::read(path).ok().and_then(|b| crate::rich::store_picture(&b, &ext)) {
                                    added.push(p);
                                }
                            }
                        }
                        if !added.is_empty() {
                            let at = sel.0.max(sel.1).min(n_chars);
                            let byte = c.body.char_indices().nth(at).map(|(b, _)| b).unwrap_or(c.body.len());
                            let mut ins = String::new();
                            for (cid, file, w) in added {
                                ins.push_str(&format!("\n{}\n", crate::rich::image_tag(&cid, w)));
                                c.images.push((cid, file));
                            }
                            c.body.insert_str(byte, &ins);
                            let end = at + ins.chars().count();
                            new_sel = Some((end, end));
                            c.preview = false;
                        }
                        // formatting: buttons in one line above the text
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 1.0;
                            let tool = |ui: &mut Ui, label: RichText, tip: &str| -> bool {
                                ui.add(egui::Button::new(label).frame(false).min_size(Vec2::new(28.0, 26.0))).on_hover_text(tip).clicked()
                            };
                            if tool(ui, RichText::new("B").font(semibold(15.0)).color(TEXT()), "Pogrubienie (Ctrl+B)") {
                                new_sel = Some(crate::rich::wrap(&mut c.body, sel.0, sel.1, "**", "**"));
                            }
                            if tool(ui, RichText::new("I").italics().size(15.0).color(TEXT()), "Kursywa (Ctrl+I)") {
                                new_sel = Some(crate::rich::wrap(&mut c.body, sel.0, sel.1, "*", "*"));
                            }
                            if tool(ui, RichText::new("U").underline().size(15.0).color(TEXT()), "Podkreślenie") {
                                new_sel = Some(crate::rich::wrap(&mut c.body, sel.0, sel.1, "<u>", "</u>"));
                            }
                            if tool(ui, RichText::new("S").strikethrough().size(15.0).color(TEXT()), "Przekreślenie") {
                                new_sel = Some(crate::rich::wrap(&mut c.body, sel.0, sel.1, "~~", "~~"));
                            }
                            ui.add_space(6.0);
                            if tool(ui, RichText::new("A−").size(12.0).color(TEXT()), "Mniejszy tekst") {
                                new_sel = Some(crate::rich::wrap(&mut c.body, sel.0, sel.1, "<span style=\"font-size:12px\">", "</span>"));
                            }
                            if tool(ui, RichText::new("A+").size(16.0).color(TEXT()), "Większy tekst") {
                                new_sel = Some(crate::rich::wrap(&mut c.body, sel.0, sel.1, "<span style=\"font-size:18px\">", "</span>"));
                            }
                            ui.menu_button(RichText::new("A").font(semibold(15.0)).color(Color32::from_rgb(0xC6, 0x28, 0x28)), |ui| {
                                for (name, css) in crate::rich::COLOURS {
                                    let col = u32::from_str_radix(&css[1..], 16).map(|n| Color32::from_rgb((n >> 16) as u8, (n >> 8) as u8, n as u8)).unwrap_or(TEXT());
                                    if ui.button(RichText::new(format!("■  {name}")).color(col)).clicked() {
                                        new_sel = Some(crate::rich::wrap(&mut c.body, sel.0, sel.1, &format!("<span style=\"color:{css}\">"), "</span>"));
                                        ui.close();
                                    }
                                }
                            })
                            .response
                            .on_hover_text("Kolor tekstu");
                            ui.add_space(6.0);
                            if tool(ui, RichText::new("H").font(semibold(15.0)).color(TEXT()), "Nagłówek") {
                                let e = crate::rich::prefix_lines(&mut c.body, sel.0, sel.1, "## ");
                                new_sel = Some((e, e));
                            }
                            if tool(ui, RichText::new("•").size(18.0).color(TEXT()), "Lista") {
                                let e = crate::rich::prefix_lines(&mut c.body, sel.0, sel.1, "- ");
                                new_sel = Some((e, e));
                            }
                            ui.add_space(8.0);
                            ui.label(RichText::new("Markdown · obraz: Ctrl+V lub przeciągnij").size(11.5).color(MUTED()));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let label = if c.preview { "✏  Edytuj" } else { "👁  Podgląd" };
                                if ui.add(egui::Button::new(RichText::new(label).size(12.5)).min_size(Vec2::new(0.0, 24.0))).clicked() {
                                    c.preview = !c.preview;
                                }
                            });
                        });
                        if c.preview {
                            // the message as it will be sent (formatting and pictures)
                            let html = crate::rich::to_html(&c.body);
                            let blocks = html::parse(&format!("<html><body>{html}</body></html>"));
                            let mut fake = Message::blank();
                            fake.inline = c.images.clone();
                            Frame::new().stroke(Stroke::new(1.0, BORDER())).corner_radius(CornerRadius::same(4)).inner_margin(Margin::same(10)).show(ui, |ui| {
                                egui::ScrollArea::vertical().id_salt(("cprev", c.key)).max_height(editor_h - 20.0).min_scrolled_height(editor_h - 20.0).show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    self.render_html(ui, &fake, &blocks);
                                });
                            });
                        } else {
                            let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
                                let job = crate::rich::highlight(buf.as_str(), wrap, TEXT());
                                ui.fonts_mut(|f| f.layout_job(job))
                            };
                            let editor = egui::ScrollArea::vertical()
                                .id_salt(("cbody", c.key))
                                .max_height(editor_h)
                                .min_scrolled_height(editor_h)
                                .show(ui, |ui| {
                                    ui.add_sized(
                                        [ui.available_width(), editor_h],
                                        egui::TextEdit::multiline(&mut c.body).id(edit_id).hint_text(hint).margin(Margin::same(6)).layouter(&mut layouter),
                                    )
                                })
                                .inner;
                            if !c.focused {
                                editor.request_focus();
                                c.focused = true;
                            }
                        }
                        if let Some((a, b)) = new_sel {
                            // back into the text with the same part chosen
                            ui.memory_mut(|mem| mem.request_focus(edit_id));
                            let mut st = egui::text_edit::TextEditState::load(ui.ctx(), edit_id).unwrap_or_default();
                            st.cursor.set_char_range(Some(egui::text_selection::CCursorRange::two(egui::text::CCursor::new(a), egui::text::CCursor::new(b))));
                            st.store(ui.ctx(), edit_id);
                        }
                        // pictures in the text: small previews with size buttons
                        if !pics.is_empty() {
                            let mut change: Option<(std::ops::Range<usize>, Option<u32>)> = None;
                            let mut fake = Message::blank();
                            fake.inline = c.images.clone();
                            egui::ScrollArea::horizontal().id_salt(("cpics", c.key)).max_height(64.0).show(ui, |ui| {
                                ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                                    for (range, cid, w) in &pics {
                                        Frame::new().stroke(Stroke::new(1.0, BORDER())).corner_radius(CornerRadius::same(4)).inner_margin(Margin::same(4)).show(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                ui.spacing_mut().item_spacing.x = 2.0;
                                                if let (key, ImgState::Ready(bytes)) = self.image_bytes(&fake, &format!("cid:{cid}")) {
                                                    ui.add(egui::Image::from_bytes(format!("bytes://cpic/{key}"), egui::load::Bytes::Shared(bytes)).fit_to_exact_size(Vec2::new(64.0, 44.0)));
                                                }
                                                ui.vertical(|ui| {
                                                    ui.label(RichText::new(format!("{w} px")).size(11.5).color(MUTED()));
                                                    ui.horizontal(|ui| {
                                                        if ui.small_button("−").on_hover_text("Mniejszy").clicked() {
                                                            change = Some((range.clone(), Some(((*w as f32) * 0.8) as u32).map(|x| x.max(60))));
                                                        }
                                                        if ui.small_button("+").on_hover_text("Większy").clicked() {
                                                            change = Some((range.clone(), Some((((*w as f32) * 1.25) as u32).min(1400))));
                                                        }
                                                        if ui.small_button("✕").on_hover_text("Usuń obraz").clicked() {
                                                            change = Some((range.clone(), None));
                                                        }
                                                    });
                                                });
                                            });
                                        });
                                        let _ = cid;
                                    }
                                });
                            });
                            if let Some((range, w)) = change {
                                let tag_cid = pics.iter().find(|p| p.0 == range).map(|p| p.1.clone()).unwrap_or_default();
                                match w {
                                    Some(w) => c.body.replace_range(range, &crate::rich::image_tag(&tag_cid, w)),
                                    None => c.body.replace_range(range, ""),
                                }
                            }
                        }
                        // the signature (from Gmail's sent mail), added when sending; can be left out
                        if !sig_text.trim().is_empty() {
                            ui.add_space(2.0);
                            ui.checkbox(&mut c.signature, RichText::new("Dodaj podpis").size(12.5).color(MUTED()));
                            if c.signature {
                                // as it will be sent: with its look and graphics
                                egui::ScrollArea::vertical().id_salt(("csig", c.key)).max_height(sig_box).show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.add_space(24.0);
                                        ui.vertical(|ui| self.signature_preview(ui));
                                    });
                                });
                            }
                        }

                        // The original message, compact and foldable.
                        if let Some(o) = &c.original {
                            ui.add_space(4.0);
                            let when = format!("{} {}", pl::short_day_month(o.date.date()), o.date.format("%H:%M"));
                            let head = format!(
                                "{}  {}  ·  {}  ·  {}",
                                if c.show_original { "⌄" } else { "›" },
                                o.title(),
                                o.from_name,
                                when
                            );
                            let (hr, hresp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::click());
                            if hresp.hovered() {
                                ui.painter().rect_filled(hr, CornerRadius::same(3), HOVER());
                            }
                            line(ui.painter(), Pos2::new(hr.left() + 4.0, hr.top() + 3.0), &head, 13.0, MUTED(), hr.width() - 8.0);
                            if hresp.on_hover_text(if c.show_original { "Zwiń" } else { "Pokaż" }).clicked() {
                                c.show_original = !c.show_original;
                            }
                            if c.show_original {
                                let box_h = (ui.available_height() - footer).max(60.0);
                                let fr = Frame::new()
                                    .fill(SURFACE())
                                    .corner_radius(CornerRadius::same(3))
                                    .inner_margin(Margin { left: 14, right: 8, top: 8, bottom: 8 })
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width() - 2.0);
                                        egui::ScrollArea::vertical()
                                            .id_salt(("orig", c.key))
                                            .max_height(box_h - 16.0)
                                            .show(ui, |ui| {
                                                ui.spacing_mut().item_spacing.y = 2.0;
                                                let to = if o.to.is_empty() { "—".to_string() } else { o.to.clone() };
                                                ui.label(
                                                    RichText::new(format!("Od: {} <{}>   Do: {}", o.from_name, o.from_addr, to))
                                                        .small()
                                                        .color(MUTED()),
                                                );
                                                let mut meta = format!(
                                                    "Data: {}   Temat: {}",
                                                    o.date.format("%d-%m-%Y %H:%M"),
                                                    o.subject
                                                );
                                                if !o.attachments.is_empty() {
                                                    meta.push_str(&format!("   📎 {}", o.attachments.join(", ")));
                                                }
                                                ui.label(RichText::new(meta).small().color(MUTED()));
                                                ui.add_space(6.0);
                                                ui.add(egui::Label::new(RichText::new(&o.body).size(13.0).color(ICON())).wrap());
                                            });
                                    });
                                let rr = fr.response.rect;
                                ui.painter().vline(rr.left() + 1.5, rr.y_range(), Stroke::new(3.0, BORDER()));
                            }
                        }
                    }
                    if let Some(e) = &c.error {
                        ui.colored_label(FLAG, e);
                    }
                });
                if c.ask_close {
                    let existing = c.draft_id.is_some();
                    let mut choice: Option<&'static str> = None;
                    let frame = Frame::new()
                        .fill(BG())
                        .stroke(Stroke::new(1.0, BORDER()))
                        .corner_radius(CornerRadius::same(12))
                        .inner_margin(Margin::same(20))
                        .shadow(egui::epaint::Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(70) });
                    let asked = egui::Modal::new(id(("ask-close", c.key))).frame(frame).show(root.ctx(), |ui| {
                        ui.set_width(400.0);
                        ui.label(RichText::new(if existing { "Zapisać zmiany w szkicu?" } else { "Zachować wiadomość jako szkic?" }).font(semibold(17.0)).color(TEXT()));
                        ui.add_space(4.0);
                        ui.label(RichText::new("Ta wiadomość nie została wysłana.").color(MUTED()));
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            if ui.add(egui::Button::new(RichText::new(if existing { "Zapisz zmiany" } else { "Zapisz szkic" }).color(Color32::WHITE)).fill(ACCENT()).min_size(Vec2::new(0.0, 30.0))).clicked() {
                                choice = Some("draft");
                            }
                            if ui.add(egui::Button::new(if existing { "Odrzuć zmiany" } else { "Odrzuć" }).min_size(Vec2::new(0.0, 30.0))).clicked() {
                                choice = Some(if existing { "close" } else { "discard" });
                            }
                            if ui.add(egui::Button::new("Wróć do pisania").min_size(Vec2::new(0.0, 30.0))).clicked() {
                                choice = Some("back");
                            }
                        });
                    });
                    if asked.should_close() && choice.is_none() {
                        choice = Some("back");
                    }
                    match choice {
                        Some("back") => {
                            c.ask_close = false;
                            c.shot_tex = None;
                        }
                        Some(o) => {
                            c.ask_close = false;
                            outcome = Some(o);
                        }
                        None => {}
                    }
                }
                if !root.ctx().embed_viewports() {
                    window_edges(root);
                }
                if shoot_here {
                    shoot_step(root.ctx(), &shot_path, shot_frames, &shot_image);
                }
            });
            // closing: first the field's latest content, then: nothing written → gone at once,
            // something written → the question "zachować jako szkic?"
            if std::env::var("POCZTA_CLOSE_TEST").is_ok() && c.start_html.is_some() && self.opened.elapsed().as_secs() > 9 {
                // test: the window closed with its ✕ (once)
                use std::sync::atomic::{AtomicBool, Ordering};
                static CLOSED: AtomicBool = AtomicBool::new(false);
                if !CLOSED.swap(true, Ordering::Relaxed) {
                    open = false;
                }
            }
            if !open && outcome.is_none() && !c.ask_close {
                outcome = Some("closing");
            }
            // the formatted field: first get what it holds right now, then send / save
            if let Some(v) = c.web.as_ref() {
                if let Some(o) = outcome.take() {
                    if c.pending.is_none() {
                        c.pending = Some((o, v.next_question()));
                    }
                }
                if let Some((o, seq)) = c.pending {
                    if v.answered() >= seq {
                        c.pending = None;
                        outcome = Some(o);
                    } else {
                        ctx.request_repaint();
                    }
                }
            }
            if outcome == Some("closing") {
                let text_same = match (&c.web_html, &c.start_html) {
                    (Some(now), Some(start)) => now == start,
                    (None, _) => c.body == c.start_body,
                    (Some(_), None) => true,
                };
                let untouched = text_same && c.subject == c.start_subject && c.files.len() == c.start_files;
                outcome = if untouched {
                    // an existing draft stays as it was; a new message is simply dropped
                    Some(if c.draft_id.is_some() { "close" } else { "discard" })
                } else {
                    c.ask_close = true;
                    c.shot_tex = None;
                    if let Some(v) = c.web.as_ref() {
                        v.capture(c.shot.clone());
                    }
                    ctx.request_repaint();
                    None
                };
            }
            if outcome == Some("send") && c.to.trim().is_empty() {
                c.error = Some("Dodaj co najmniej jednego odbiorcę.".into());
                outcome = None;
            }
            if outcome == Some("send") {
                // the files are looked at on disk only when sending, not on every redraw
                let total: u64 = c.files.iter().filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum();
                if total > 25 * 1024 * 1024 {
                    c.error = Some(format!("Załączniki są za duże ({}) — Gmail przyjmuje razem do 25 MB.", human_size(total)));
                    outcome = None;
                }
            }
            if let Some(o) = outcome {
                closed.push((i, o));
            }
        }
        // (windows opened meanwhile, e.g. by an AI draft, come after the ones drawn)
        composers.append(&mut self.composers);
        self.composers = composers;
        for (i, o) in closed.into_iter().rev() {
            let mut c = self.composers.remove(i);
            self.touched = true;
            crate::diag::log(format!("message window closed: {o}"));
            if o == "close" {
                continue;
            }
            // the formatted writing field: what it holds is the whole message (text, pictures,
            // signature and quote); pictures go inside the email when it is sent
            let from_web = c.web_html.take();
            drop(c.web.take());
            if from_web.is_some() {
                c.original = None;
            }
            let orig = c.original.take();
            if let Some(orig) = &orig {
                c.body.push_str(&orig.as_text());
            }
            // sent as formatted mail (Markdown → HTML), with the signature between the new text and
            // the quoted message, and the pictures in the text sent along
            let pictures: Vec<(String, String)> = c.images.iter().filter(|(cid, _)| c.body.contains(cid.as_str())).cloned().collect();
            let mut html = None;
            let mut inline = pictures.clone();
            let mut html_file = String::new();
            if let Some(web) = &from_web {
                if o == "send" {
                    let (h, pics) = crate::compose_view::to_email(web);
                    html_file = crate::model::cache_put(h.as_bytes(), "html");
                    html = Some(h);
                    inline = pics;
                } else {
                    // a draft keeps the field exactly as it was (pictures as files on disk)
                    html_file = crate::model::cache_put(web.as_bytes(), "html");
                    inline = vec![];
                }
            } else if o == "send" {
                let use_sig = c.signature && !self.store.account.signature_text.trim().is_empty();
                let quote = orig.as_ref().and_then(|o| o.as_html());
                let (plain, h) = build_bodies(&c.body, use_sig.then_some(&self.store.account), quote.as_deref());
                if use_sig {
                    inline.extend(self.store.account.signature_images.iter().cloned());
                }
                if quote.is_some() {
                    // the quoted message's own pictures go along with it
                    if let Some(o) = &orig {
                        inline.extend(o.inline.iter().cloned());
                    }
                }
                html_file = crate::model::cache_put(h.as_bytes(), "html");
                c.body = plain;
                html = Some(h);
            }
            if o == "discard" {
                if let Some(id) = c.draft_id {
                    self.store.messages.retain(|m| m.id != id);
                }
                continue;
            }
            let folder = if o == "send" { Folder::Outbox } else { Folder::Drafts };
            let id = match c.draft_id {
                Some(id) => id,
                None => {
                    let id = self.store.new_id();
                    self.store.messages.push(Message {
                        id,
                        folder,
                        account: 0,
                        from_name: String::new(),
                        from_addr: String::new(),
                        to: String::new(),
                        cc: String::new(),
                        subject: String::new(),
                        body: String::new(),
                        date: Local::now().naive_local(),
                        read: true,
                        flagged: false,
                        attachments: vec![],
                        sample: false,
                        html_path: String::new(),
                        inline: vec![],
                        attachment_files: vec![],
                        labels: vec![],
                        uid: None,
                        body_path: String::new(),
                        partial: false,
                        snippet: String::new(),
                        on_disk: false,
                    });
                    id
                }
            };
            let acc = self.account_for(c.account);
            if let Some(m) = self.msg_mut(id) {
                m.account = c.account;
                m.folder = folder;
                m.from_name = acc.display_name.clone();
                m.from_addr = acc.email.clone();
                m.to = c.to.clone();
                m.cc = c.cc.clone();
                m.subject = if c.subject.trim().is_empty() { "(bez tematu)".into() } else { c.subject.clone() };
                m.body = c.body.clone();
                m.body_path.clear();
                m.date = Local::now().naive_local();
                m.sample = false;
                // drafts keep their pictures; sent mail shows as sent (formatted)
                m.inline = inline.clone();
                m.html_path = html_file.clone();
                m.attachments = c.files.iter().map(|f| std::path::Path::new(f).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).collect();
                m.attachment_files = c.files.clone();
            }
            if o == "send" {
                if can_send {
                    self.busy += 1;
                    net::send(acc, Outgoing { id, to: c.to, cc: c.cc, subject: c.subject, body: c.body, html, inline, files: c.files }, self.tx.clone(), self.ctx.clone());
                    self.set_status("Wysyłanie…");
                } else {
                    self.set_status("Zapisano w skrzynce nadawczej — zostanie wysłana po zalogowaniu");
                }
            } else {
                self.set_status("Szkic zapisany");
            }
        }
    }
}

/// The message as it is sent: plain text, and formatted (the Markdown written turned into HTML),
/// with the signature (if any) put in above the quoted earlier message.
pub(super) fn build_bodies(body: &str, sig: Option<&crate::model::Account>, quote_html: Option<&str>) -> (String, String) {
    let cut = body.find("\n\n------ ").unwrap_or(body.len());
    let (mine, quoted) = body.split_at(cut);
    let mut plain = crate::rich::plain(mine.trim_end());
    let mut html = format!("<div style=\"font-family:Arial,sans-serif;font-size:14px\">{}", crate::rich::to_html(mine.trim_end()));
    if let Some(acc) = sig {
        plain.push_str(&format!("\n\n-- \n{}", acc.signature_text.trim()));
        let sig_html = if acc.signature_html.trim().is_empty() { crate::signature::text_to_html(&acc.signature_text) } else { acc.signature_html.clone() };
        html.push_str(&format!("<br>{sig_html}"));
    }
    plain.push_str(quoted);
    if let Some(q) = quote_html {
        // the message answered or forwarded, as it looked
        html += &format!("<div style=\"margin-top:14px\">{q}</div>");
    } else if !quoted.trim().is_empty() {
        html += &format!(
            "<div style=\"margin-top:14px;padding-left:10px;border-left:2px solid #ccc;color:#555\">{}</div>",
            crate::signature::text_to_html(quoted.trim())
        );
    }
    html += "</div>";
    (plain, html)
}

/// Adds files to the attachments of a message (each once).
pub(super) fn add_files(list: &mut Vec<String>, new: Vec<String>) {
    for f in new {
        if !list.iter().any(|x| x.eq_ignore_ascii_case(&f)) {
            list.push(f);
        }
    }
}
