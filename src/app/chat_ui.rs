use super::*;
use crate::chat::{Cmd, Msg, Pace, Row, Update};

/// The chat on the window's side: what the worker last sent, and what is being written.
#[derive(Default)]
pub(super) struct ChatState {
    link: Option<(Sender<Cmd>, Receiver<Update>)>,
    /// the sign-in the worker was given
    key: u64,
    /// when the worker stopped by itself (it is started again a minute later)
    stopped_at: Option<f64>,
    pace: Option<Pace>,
    pub(super) rows: Vec<Row>,
    pub(super) open: Option<String>,
    msgs: Arc<Vec<Msg>>,
    /// written here and not confirmed by Google yet, oldest first
    outgoing: Vec<Outgoing>,
    /// what is being written, per conversation (a text never moves to another conversation)
    drafts: HashMap<String, String>,
    /// the last round failed (in Polish)
    error: Option<String>,
    /// drawn heights of the messages (by name) at this width: those off screen are only skipped
    heights: HashMap<String, f32>,
    heights_w: i32,
    /// the time of the last good round ("12:30")
    synced: Option<String>,
    busy: bool,
    pub(super) unread: usize,
    /// the read mark already sent: (conversation, its newest message)
    marked: Option<(String, i64)>,
    hd_rx: Option<Receiver<Result<(String, String), String>>>,
    /// when the account kind may be asked again after a failure (the app's clock)
    hd_next: f64,
    /// made-up conversations for screenshots
    pub(super) demo: bool,
    me: String,
    focus_draft: bool,
    /// the open conversation was drawn scrolled to its newest message
    at_bottom: bool,
}

/// A message written here that Google has not confirmed: on its way, or failed. A failed one keeps
/// its request id, so sending it again can't post it twice even if Google got it after all.
#[derive(Clone)]
struct Outgoing {
    id: String,
    space: String,
    msg: Msg,
    failed: Option<String>,
}

fn sign_in_key(a: &crate::model::Account) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (&a.oauth_refresh_token, &a.oauth_scopes, &a.google_sub).hash(&mut h);
    h.finish()
}

fn day_label(d: NaiveDate, today: NaiveDate) -> String {
    match (today - d).num_days() {
        0 => t!("misc.chat_ui.today").into_owned(),
        1 => t!("misc.chat_ui.yesterday").into_owned(),
        _ => format!("{} {}", i18n::wd_full(d), i18n::date(d)),
    }
}

impl MailApp {
    /// The chat tab is shown: a Google Workspace account signed in with Google.
    pub(super) fn chat_visible(&self) -> bool {
        let a = &self.store.account;
        self.chat.demo || (self.prefs.google_chat && crate::google::ready(a) && a.google_hd.as_deref().is_some_and(|d| !d.is_empty()))
    }

    /// The chat may be read (the sign-in allows it).
    pub(super) fn chat_ready(&self) -> bool {
        self.chat.demo || (self.chat_visible() && crate::chat::allowed(&self.store.account))
    }

    pub(super) fn chat_demo(&mut self) {
        let (rows, msgs) = crate::chat::sample();
        self.chat.demo = true;
        self.chat.unread = rows.iter().map(|r| r.unread).sum();
        self.chat.open = rows.first().map(|r| r.name.clone());
        self.chat.rows = rows;
        self.chat.msgs = Arc::new(msgs);
        self.chat.me = "users/me".into();
        self.chat.synced = Some(Local::now().format("%H:%M").to_string());
    }

    fn chat_cmd(&mut self, cmd: Cmd) {
        if let Some((tx, _)) = &self.chat.link {
            let _ = tx.send(cmd);
        }
    }

    /// Every frame: what the worker sent, the worker itself, how often it asks Google.
    pub(super) fn chat_tick(&mut self) {
        let me = if self.store.account.google_sub.is_empty() { String::new() } else { format!("users/{}", self.store.account.google_sub) };
        if !self.chat.demo && me != self.chat.me {
            // another Google account (before anything the old worker sent is read): nothing of the
            // old one stays on screen or goes out under the new one, and a new worker starts
            let c = &mut self.chat;
            (c.rows, c.open, c.msgs, c.marked, c.unread, c.link, c.me) = (vec![], None, Arc::default(), None, 0, None, me);
            c.drafts.clear();
            c.outgoing.clear();
            c.heights.clear();
        }
        let mut updates = vec![];
        let mut stopped = false;
        if let Some((_, rx)) = &self.chat.link {
            loop {
                match rx.try_recv() {
                    Ok(u) => updates.push(u),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        stopped = true;
                        break;
                    }
                }
            }
        }
        let mut fresh = vec![];
        if self.module == Module::Chat && !self.chat_visible() {
            self.module = Module::Mail;
        }
        for u in updates {
            match u {
                Update::Rows(r) => {
                    self.chat.unread = r.iter().map(|r| r.unread).sum();
                    self.chat.rows = r;
                }
                Update::Messages(space, m) => {
                    if self.chat.open.as_deref() == Some(space.as_str()) {
                        self.chat.msgs = m;
                        // a message may have been edited or replaced: heights are measured again
                        self.chat.heights.clear();
                    }
                }
                Update::Sent { id, result } => {
                    if let Some(i) = self.chat.outgoing.iter().position(|o| o.id == id) {
                        match result {
                            Ok(_) => {
                                self.chat.outgoing.remove(i);
                            }
                            Err(e) => self.chat.outgoing[i].failed = Some(e),
                        }
                    }
                }
                Update::Fresh(n) => fresh.extend(n),
                Update::Done(r) => {
                    self.chat.busy = false;
                    match r {
                        Ok(()) => {
                            self.chat.error = None;
                            self.chat.synced = Some(Local::now().format("%H:%M").to_string());
                        }
                        Err(e) => self.chat.error = Some(e),
                    }
                }
                Update::Busy => self.chat.busy = true,
            }
        }
        if !fresh.is_empty() {
            self.chat_notify(fresh);
        }
        if stopped {
            self.chat.link = None;
            self.chat.busy = false;
            self.chat.stopped_at = Some(self.clock());
            self.chat.error = Some(t!("misc.chat_ui.stopped").into_owned());
            self.chat_all_unsent();
        }

        let usable = self.loading.is_none() && self.shot.is_none() && !self.read_only && !self.quitting && std::env::var("POCZTA_OFFLINE").is_err();
        if !usable {
            return;
        }
        // Workspace or a personal account, for a sign-in made before the app kept that
        if let Some(r) = self.chat.hd_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.chat.hd_rx = None;
            match r {
                Ok((hd, sub)) => {
                    let a = &mut self.store.account;
                    // an answer for this account only (another one may have signed in meanwhile)
                    if a.google_hd.is_none() && (a.google_sub.is_empty() || a.google_sub == sub) {
                        crate::diag::log(format!("chat: Google account is {}", if hd.is_empty() { "personal" } else { "Google Workspace" }));
                        a.google_hd = Some(hd);
                        a.google_sub = sub;
                        self.save_now = true;
                    }
                }
                Err(e) => {
                    crate::diag::log(format!("chat: account kind not known: {e}"));
                    // asked again in 5 minutes (offline at start, Google busy)
                    self.chat.hd_next = self.clock() + 300.0;
                }
            }
        }
        let a = &self.store.account;
        if crate::google::ready(a) && a.google_hd.is_none() && self.chat.hd_rx.is_none() && self.clock() >= self.chat.hd_next {
            let (tx, rx) = channel();
            let (acc, ctx) = (a.clone(), self.ctx.clone());
            std::thread::spawn(move || {
                let _ = tx.send(crate::google::userinfo(&acc));
                ctx.request_repaint();
            });
            self.chat.hd_rx = Some(rx);
        }

        if !self.chat_ready() {
            if self.chat.link.take().is_some() {
                crate::diag::log("chat: switched off");
                self.chat_all_unsent();
            }
            self.chat.unread = 0;
            return;
        }
        let key = sign_in_key(&self.store.account);
        if self.chat.link.is_none() {
            if self.chat.stopped_at.is_some_and(|t| self.clock() - t < 60.0) {
                return;
            }
            self.chat.link = Some(crate::chat::start(self.store.account.clone(), self.ctx.clone()));
            self.chat.key = key;
            self.chat.pace = None;
            if let Some(o) = self.chat.open.clone() {
                self.chat_cmd(Cmd::Open(o));
            }
        } else if key != self.chat.key {
            // the same account signed in again: a new token for the worker
            self.chat.key = key;
            self.chat_cmd(Cmd::Account(Box::new(self.store.account.clone())));
        }
        let (focused, minimized) = self.ctx.input(|i| (i.viewport().focused.unwrap_or(false), i.viewport().minimized.unwrap_or(false)));
        let pace = if self.hidden || minimized {
            Pace::Background
        } else if focused && self.module == Module::Chat {
            Pace::Watching
        } else {
            Pace::Open
        };
        if self.chat.pace != Some(pace) {
            self.chat.pace = Some(pace);
            self.chat_cmd(Cmd::Pace(pace));
        }
        // the conversation on screen counts as read up to its newest message shown here (and in
        // Google Chat); scrolled up to older ones, what came below is not seen yet
        if pace == Pace::Watching && self.chat.at_bottom {
            let last = self.chat.msgs.last().map(|m| m.at);
            if let (Some(row), Some(last)) = (self.chat.open.as_ref().and_then(|o| self.chat.rows.iter().find(|r| &r.name == o)), last) {
                let mark = (row.name.clone(), last);
                if row.unread > 0 && self.chat.marked.as_ref() != Some(&mark) {
                    self.chat.marked = Some(mark.clone());
                    self.chat_cmd(Cmd::MarkRead(mark.0, mark.1));
                }
            }
        }
    }

    /// New messages from others while the window is not in front: a sound and a note in the corner.
    fn chat_notify(&mut self, notes: Vec<crate::chat::Note>) {
        crate::diag::log(format!("chat: {} new messages", notes.len()));
        if !self.prefs.notify || crate::unseen() {
            return;
        }
        let (focused, minimized) = self.ctx.input(|i| (i.viewport().focused.unwrap_or(false), i.viewport().minimized.unwrap_or(false)));
        if focused && !minimized && !self.hidden {
            return;
        }
        let Some(n) = notes.last() else { return };
        crate::notify::sound();
        self.ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
        let head = i18n::plural("misc.chat_ui.new_messages", notes.len());
        let who = if n.who.is_empty() { n.title.clone() } else { n.who.clone() };
        let from = if n.title.is_empty() || n.title == who { who.clone() } else { format!("{who} · {}", n.title) };
        let subject = if n.text.is_empty() { t!("misc.chat_ui.file_subject").into_owned() } else { n.text.lines().next().unwrap_or("").chars().take(140).collect() };
        let [r, g, b, _] = crate::theme::avatar_color(&who).to_array();
        crate::popup::show(
            crate::popup::Note { head, from, subject, dark: self.prefs.dark, logo: None, initials: crate::model::initials(&who), color: (r as u32) << 16 | (g as u32) << 8 | b as u32 },
            8,
            self.popup_click.0.clone(),
            self.ctx.clone(),
        );
        self.popup = Some(NewMail { ids: vec![], chat: Some(n.space.clone()) });
    }

    /// Text written in the chat and not confirmed by Google: an update must not restart over it
    /// (only while the chat is shown, or a hidden one would hold updates back for good).
    pub(super) fn chat_holds_text(&self) -> bool {
        self.chat_visible() && (!self.chat.outgoing.is_empty() || self.chat.drafts.values().any(|d| !d.trim().is_empty()))
    }

    /// The worker went away with texts still on their way: they stay on screen as failed.
    fn chat_all_unsent(&mut self) {
        for o in self.chat.outgoing.iter_mut().filter(|o| o.failed.is_none()) {
            o.failed = Some(t!("misc.chat_ui.maybe_unsent").into_owned());
        }
    }

    /// A failed message: sent again under its own request id, or taken back into the box to edit.
    fn chat_retry(&mut self, id: &str, edit: bool) {
        let Some(i) = self.chat.outgoing.iter().position(|o| o.id == id) else { return };
        if edit {
            let o = self.chat.outgoing.remove(i);
            let d = self.chat.drafts.entry(o.space).or_default();
            *d = if d.trim().is_empty() { o.msg.text } else { format!("{}\n{d}", o.msg.text) };
            self.chat.focus_draft = true;
            return;
        }
        let o = &self.chat.outgoing[i];
        let cmd = Cmd::Send { space: o.space.clone(), text: o.msg.text.clone(), id: o.id.clone() };
        let sent = self.chat.link.as_ref().is_some_and(|(tx, _)| tx.send(cmd).is_ok());
        self.chat.outgoing[i].failed = if sent { None } else { Some(t!("misc.chat_ui.not_connected").into_owned()) };
    }

    pub(super) fn chat_open(&mut self, name: String) {
        if self.chat.open.as_deref() == Some(name.as_str()) {
            return;
        }
        self.chat.open = Some(name.clone());
        self.chat.msgs = Arc::new(vec![]);
        self.chat.heights.clear();
        self.chat.focus_draft = true;
        self.chat.at_bottom = false;
        if self.chat.demo {
            self.chat.msgs = Arc::new(if name == "spaces/a" { crate::chat::sample().1 } else { vec![] });
        }
        self.chat_cmd(Cmd::Open(name));
    }

    pub(super) fn chat_refresh(&mut self) {
        self.chat.stopped_at = None;
        self.chat_cmd(Cmd::Refresh);
    }

    fn chat_sign_in(&mut self) {
        self.sign_in_more(crate::chat::SCOPES, t!("misc.chat_ui.opening_login"));
    }

    fn chat_send(&mut self, space: &str) {
        let text = self.chat.drafts.get(space).map(|d| d.trim().to_string()).unwrap_or_default();
        if text.is_empty() {
            return;
        }
        self.chat.drafts.remove(space);
        let id = crate::chat::new_id();
        let m = Msg { name: format!("pending-{id}"), sender: self.chat.me.clone(), who: self.store.account.display_name.clone(), at: crate::chat::now(), text: text.clone(), ..Default::default() };
        if self.chat.demo {
            let mut all = (*self.chat.msgs).clone();
            all.push(m);
            self.chat.msgs = Arc::new(all);
            return;
        }
        let sent = self.chat.link.as_ref().is_some_and(|(tx, _)| tx.send(Cmd::Send { space: space.to_string(), text, id: id.clone() }).is_ok());
        let failed = (!sent).then(|| t!("misc.chat_ui.not_connected").into_owned());
        self.chat.outgoing.push(Outgoing { id, space: space.to_string(), msg: m, failed });
    }

    // ------------------------------------------------------------ drawing

    /// The conversations, in the left panel.
    pub(super) fn chat_side(&mut self, ui: &mut Ui) {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(RichText::new(t!("misc.chat_ui.title")).font(semibold(18.0)).color(TEXT()));
        });
        ui.add_space(6.0);
        if !self.chat_ready() {
            Frame::new().inner_margin(Margin::symmetric(12, 0)).show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(t!("misc.chat_ui.waiting")).small().color(MUTED())).wrap());
            });
            return;
        }
        let today = Local::now().date_naive();
        let mut clicked = None;
        let mut read = None;
        if self.chat.rows.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                let t = if self.chat.synced.is_none() { t!("misc.chat_ui.loading_rows") } else { t!("misc.chat_ui.no_rows") };
                ui.label(RichText::new(t).color(MUTED()));
            });
        }
        for row in &self.chat.rows {
            let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 54.0), Sense::click());
            if ui.is_rect_visible(r) {
                let on = self.chat.open.as_deref() == Some(row.name.as_str());
                let p = ui.painter();
                if on {
                    p.rect_filled(r, CornerRadius::same(4), ACCENT_SOFT());
                } else if resp.hovered() {
                    p.rect_filled(r, CornerRadius::same(4), HOVER());
                }
                let c = Pos2::new(r.left() + 22.0, r.center().y);
                if row.kind == "SPACE" {
                    // a named space (a room), not a person
                    p.circle_filled(c, 15.0, SURFACE());
                    paint_icon(p, c, ic::PEOPLE, 15.0, MUTED());
                } else {
                    avatar(p, c, 15.0, &row.title);
                }
                let time = short_date(crate::chat::local(row.active), today);
                let tr = p.text(Pos2::new(r.right() - 8.0, r.top() + 9.0), Align2::RIGHT_TOP, &time, FontId::proportional(11.5), if row.unread > 0 { ACCENT() } else { MUTED() });
                let x = r.left() + 44.0;
                let unread = row.unread > 0;
                let font = if unread { semibold(13.5) } else { FontId::proportional(13.5) };
                line_f(p, Pos2::new(x, r.top() + 7.0), &row.title, font, TEXT(), tr.left() - x - 6.0);
                line(p, Pos2::new(x, r.top() + 29.0), &row.last, 12.5, if unread { TEXT2() } else { MUTED() }, r.right() - x - if unread { 34.0 } else { 10.0 });
                if unread {
                    count_badge(p, Pos2::new(r.right() - 22.0, r.top() + 37.0), row.unread);
                }
            }
            let resp = if row.external { resp.on_hover_text(t!("misc.chat_ui.external_hover")) } else { resp };
            if resp.clicked() {
                clicked = Some(row.name.clone());
            }
            resp.context_menu(|ui| {
                if ui.button(t!("misc.chat_ui.mark_read")).clicked() {
                    read = Some(row.name.clone());
                    ui.close();
                }
                if !row.uri.is_empty() && ui.button(t!("misc.chat_ui.open_in_chat")).clicked() {
                    crate::oauth::open_url(&row.uri);
                    ui.close();
                }
            });
        }
        if let Some(name) = clicked {
            self.chat_open(name);
        }
        if let Some(name) = read {
            self.chat_cmd(Cmd::MarkRead(name, i64::MAX));
        }
        ui.add_space(10.0);
        Frame::new().inner_margin(Margin::symmetric(14, 0)).show(ui, |ui| {
            if let Some(e) = &self.chat.error {
                ui.add(egui::Label::new(RichText::new(e).small().color(FLAG)).wrap());
            } else if self.chat.busy {
                ui.label(RichText::new(t!("misc.chat_ui.syncing")).small().color(MUTED()));
            } else if let Some(t) = &self.chat.synced {
                ui.label(RichText::new(t!("misc.chat_ui.updated", time = t)).small().color(MUTED()));
            }
            if !self.chat.demo && ui.link(RichText::new(t!("misc.chat_ui.refresh")).small()).clicked() {
                self.chat_refresh();
            }
        });
    }

    /// The open conversation, or the steps that switch the chat on.
    pub(super) fn chat_main(&mut self, ui: &mut Ui) {
        if !self.chat_ready() {
            Frame::new().inner_margin(Margin::symmetric(24, 18)).show(ui, |ui| self.chat_setup(ui));
            return;
        }
        let Some(open) = self.chat.open.clone() else {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| ui.label(RichText::new(t!("misc.chat_ui.pick")).color(MUTED())));
            return;
        };
        let row = self.chat.rows.iter().find(|r| r.name == open).cloned().unwrap_or_default();
        egui::Panel::top("chat-head").frame(Frame::new().fill(BG()).inner_margin(Margin { left: 16, right: 12, top: 10, bottom: 8 })).show(ui, |ui| {
            // the button first, the title in what is left (a long title is cut, not pushed over it);
            // inside a row, so the centred layout keeps the row's height
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if !row.uri.is_empty() && ui.small_button(t!("misc.chat_ui.open_in_chat")).clicked() {
                        crate::oauth::open_url(&row.uri);
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(&row.title).font(semibold(17.0)).color(TEXT())).truncate());
                        if row.external {
                            ui.add(egui::Label::new(RichText::new(t!("misc.chat_ui.external_tag")).small().color(MUTED())).truncate());
                        }
                    });
                });
            });
            let r = ui.min_rect();
            ui.painter().hline(r.x_range(), r.bottom() + 8.0, Stroke::new(1.0, BORDER()));
        });
        egui::Panel::bottom("chat-compose").frame(Frame::new().fill(BG()).inner_margin(Margin { left: 16, right: 12, top: 8, bottom: 12 })).show(ui, |ui| self.chat_compose(ui, &open, &row.uri));
        let out = egui::ScrollArea::vertical().id_salt(("chat", &open)).stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| self.chat_messages(ui, &open, &row.uri));
        self.chat.at_bottom = out.state.clamped_offset().y + out.inner_rect.height() >= out.content_size.y - 8.0;
    }

    fn chat_messages(&mut self, ui: &mut Ui, open: &str, uri: &str) {
        let _p = crate::prof::span("chat messages");
        let today = Local::now().date_naive();
        let me = self.chat.me.clone();
        let msgs = self.chat.msgs.clone();
        let out: Vec<Outgoing> = self.chat.outgoing.iter().filter(|o| o.space == open).cloned().collect();
        if msgs.is_empty() && out.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| ui.label(RichText::new(if self.chat.busy { t!("misc.chat_ui.loading_msgs") } else { t!("misc.chat_ui.no_msgs") }).color(MUTED())));
            return;
        }
        let maxw = (ui.available_width() * 0.72).clamp(160.0, 640.0);
        // heights measured at another width no longer hold
        let wk = ui.available_width().round() as i32;
        if self.chat.heights_w != wk {
            self.chat.heights_w = wk;
            self.chat.heights.clear();
        }
        let clip = ui.clip_rect();
        let mut prev: Option<&Msg> = None;
        let mut retry: Option<(String, bool)> = None;
        ui.add_space(6.0);
        for (m, o) in msgs.iter().map(|m| (m, None)).chain(out.iter().map(|o| (&o.msg, Some(o)))) {
            let top = ui.cursor().top();
            // a message off screen whose height is known only takes its room
            if let Some(h) = self.chat.heights.get(&m.name).copied() {
                if top + h < clip.top() || top > clip.bottom() {
                    ui.add_space(h);
                    prev = Some(m);
                    continue;
                }
            }
            let when = crate::chat::local(m.at);
            let new_day = prev.is_none_or(|p| crate::chat::local(p.at).date() != when.date());
            if new_day {
                ui.add_space(10.0);
                ui.vertical_centered(|ui| ui.label(RichText::new(day_label(when.date(), today)).small().color(MUTED())));
            }
            let mine = !me.is_empty() && m.sender == me;
            let new_group = new_day || prev.is_none_or(|p| p.sender != m.sender || m.at - p.at > 10 * 60 * 1_000_000);
            if new_group {
                ui.add_space(8.0);
                let time = when.format("%H:%M").to_string();
                if mine {
                    // Align::Min: a centred cross axis would take the whole height of the view
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        ui.add_space(16.0);
                        ui.label(RichText::new(time).small().color(MUTED()));
                    });
                } else {
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        let (r, _) = ui.allocate_exact_size(Vec2::new(28.0, 20.0), Sense::hover());
                        let name = if m.who.is_empty() { "?" } else { m.who.as_str() };
                        avatar(ui.painter(), Pos2::new(r.center().x, r.top() + 14.0), 13.0, name);
                        ui.add_space(6.0);
                        ui.label(RichText::new(name).font(semibold(13.0)).color(TEXT()));
                        ui.label(RichText::new(time).small().color(MUTED()));
                    });
                }
            } else {
                ui.add_space(2.0);
            }
            // the bubble is as wide as its longest line (a right-to-left row would stretch it)
            let body = egui::TextStyle::Body.resolve(ui.style());
            let mut need = m.text.lines().map(|l| text_width(ui, l, body.clone())).fold(0.0, f32::max);
            for (name, _) in &m.files {
                need = need.max(text_width(ui, &format!("{} {name}", ic::ATTACH), body.clone()));
            }
            if o.is_some() || m.edited {
                need = need.max(if o.is_some_and(|o| o.failed.is_some()) { 220.0 } else { 60.0 });
            }
            let w = need.ceil().clamp(8.0, maxw);
            let layout = if mine { Layout::right_to_left(Align::Min) } else { Layout::left_to_right(Align::Min) };
            ui.with_layout(layout, |ui| {
                ui.add_space(if mine { 16.0 } else { 54.0 });
                Frame::new()
                    .fill(if mine { ACCENT_SOFT() } else { SURFACE() })
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.set_width(w);
                        ui.vertical(|ui| {
                            if !m.text.is_empty() {
                                ui.add(egui::Label::new(RichText::new(&m.text).color(if o.is_some() { MUTED() } else { TEXT() })).wrap().selectable(true));
                            }
                            for (name, link) in &m.files {
                                if ui.link(format!("{} {name}", ic::ATTACH)).on_hover_text(t!("misc.chat_ui.open_browser")).clicked() {
                                    crate::oauth::open_url(if link.is_empty() { uri } else { link });
                                }
                            }
                            if m.edited && o.is_none() {
                                ui.label(RichText::new(t!("misc.chat_ui.edited")).size(11.0).color(MUTED()));
                            }
                            match o.map(|o| (o, o.failed.as_deref())) {
                                Some((_, None)) => {
                                    ui.label(RichText::new(t!("misc.chat_ui.sending")).size(11.0).color(MUTED()));
                                }
                                Some((o, Some(e))) => {
                                    ui.add(egui::Label::new(RichText::new(t!("misc.chat_ui.not_sent", error = e)).size(11.0).color(FLAG)).wrap());
                                    ui.horizontal_wrapped(|ui| {
                                        if ui.link(RichText::new(t!("misc.chat_ui.resend")).size(11.0)).clicked() {
                                            retry = Some((o.id.clone(), false));
                                        }
                                        if ui.link(RichText::new(t!("misc.chat_ui.edit")).size(11.0)).clicked() {
                                            retry = Some((o.id.clone(), true));
                                        }
                                        if crate::chat::needs_app_setup(e) && ui.link(RichText::new(t!("misc.chat_ui.configure_app")).size(11.0)).clicked() {
                                            crate::oauth::open_url(crate::chat::CONFIGURE_URL);
                                        }
                                    });
                                }
                                None => {}
                            }
                        });
                    });
            });
            // a message not confirmed yet changes its look: measured every frame
            if o.is_none() {
                self.chat.heights.insert(m.name.clone(), ui.cursor().top() - top);
            }
            prev = Some(m);
        }
        ui.add_space(10.0);
        if let Some((id, edit)) = retry {
            self.chat_retry(&id, edit);
        }
    }

    fn chat_compose(&mut self, ui: &mut Ui, space: &str, uri: &str) {
        if !self.chat.demo && !crate::chat::can_send(&self.store.account) {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(t!("misc.chat_ui.needs_consent")).color(MUTED()));
                if ui.add_enabled(!self.signing_in, egui::Link::new(t!("misc.chat_ui.allow"))).clicked() {
                    self.chat_sign_in();
                }
                if !uri.is_empty() && ui.link(t!("misc.chat_ui.reply_in_chat")).clicked() {
                    crate::oauth::open_url(uri);
                }
            });
            return;
        }
        // per conversation: undo history and scroll position stay with their own text
        let edit_id = id(("chat-draft", space));
        let mut send = false;
        if ui.memory(|m| m.has_focus(edit_id)) {
            // Enter sends, Shift+Enter starts a new line
            ui.input_mut(|i| {
                if !i.modifiers.shift && !i.modifiers.ctrl && !i.modifiers.alt && i.consume_key(egui::Modifiers::NONE, Key::Enter) {
                    send = true;
                }
            });
        }
        ui.horizontal(|ui| {
            let w = ui.available_width() - 48.0;
            let draft = self.chat.drafts.entry(space.to_string()).or_default();
            egui::ScrollArea::vertical().id_salt(("chat-draft", space)).max_height(120.0).show(ui, |ui| {
                // a blinking cursor would redraw the whole conversation twice a second
                ui.visuals_mut().text_cursor.blink = false;
                ui.add(egui::TextEdit::multiline(draft).id(edit_id).desired_rows(2).desired_width(w).margin(Margin::same(6)).hint_text(t!("misc.chat_ui.hint")));
            });
            let empty = draft.trim().is_empty();
            let btn = egui::Button::new(RichText::new(ic::SEND).size(17.0).color(Color32::WHITE)).fill(ACCENT()).min_size(Vec2::new(38.0, 38.0));
            if ui.add_enabled(!empty, btn).on_hover_text(t!("misc.chat_ui.send_hover")).clicked() {
                send = true;
            }
        });
        if std::mem::take(&mut self.chat.focus_draft) || send {
            ui.memory_mut(|m| m.request_focus(edit_id));
        }
        if send {
            self.chat_send(space);
        }
    }

    /// The one-time steps for a Google Workspace account.
    fn chat_setup(&mut self, ui: &mut Ui) {
        let a = self.store.account.clone();
        ui.set_max_width(620.0);
        ui.label(RichText::new(t!("misc.chat_ui.setup_title")).font(semibold(20.0)).color(TEXT()));
        ui.add_space(6.0);
        ui.add(egui::Label::new(RichText::new(t!("misc.chat_ui.setup_intro", email = a.email, domain = a.google_hd.clone().unwrap_or_default())).color(TEXT2())).wrap());
        ui.add_space(12.0);
        ui.label(RichText::new(t!("misc.chat_ui.setup_once")).strong());
        ui.add_space(6.0);
        if ui.button(t!("misc.chat_ui.step1")).clicked() {
            crate::oauth::open_url(crate::chat::ENABLE_API_URL);
        }
        ui.add_space(6.0);
        if ui.button(t!("misc.chat_ui.step2")).clicked() {
            crate::oauth::open_url(crate::chat::CONFIGURE_URL);
        }
        ui.add(
            egui::Label::new(
                RichText::new(t!("misc.chat_ui.step2_note"))
                    .small()
                    .color(MUTED()),
            )
            .wrap(),
        );
        ui.add_space(10.0);
        let btn = egui::Button::new(RichText::new(t!("misc.chat_ui.step3")).color(Color32::WHITE)).fill(ACCENT());
        if ui.add_enabled(!self.signing_in, btn).clicked() {
            self.chat_sign_in();
        }
        if self.signing_in {
            ui.label(RichText::new(t!("misc.chat_ui.finish_login")).small().color(MUTED()));
        }
        ui.add(egui::Label::new(RichText::new(t!("misc.chat_ui.pick_account", email = a.email)).small().color(MUTED())).wrap());
        if let Some(e) = &self.sign_in_error {
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(e).color(FLAG)).wrap());
        }
        ui.add_space(14.0);
        ui.label(RichText::new(t!("misc.chat_ui.hide_hint")).small().color(MUTED()));
    }
}
