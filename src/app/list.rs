use super::*;

/// The message list as shown: rows (date headings and messages) with their positions,
/// worked out once and reused until the mailbox, the folder or the search changes.
pub(super) struct VisList {
    pub(super) key: (MailView, String, String, (usize, usize), u64, NaiveDate, Scope),
    /// while searching: how many results there are everywhere, among received and among sent mail
    pub(super) found: [usize; 3],
    pub(super) ids: Vec<u64>,
    pub(super) unread: usize,
    /// (heading or message, top position)
    pub(super) rows: Vec<(Row, f32)>,
    pub(super) height: f32,
}

#[derive(Clone, Copy)]
pub(super) enum Row {
    /// a date heading, as an index into `group_title` (the words follow the language live)
    Group(u8),
    Msg(u64),
}

pub(super) const ROW_H: f32 = 66.0;

pub(super) const GROUP_H: f32 = 32.0;

/// Numbers for the left bar (per folder, unread, flagged, tags), counted once per change.
#[derive(Default)]
pub(super) struct Counts {
    pub(super) unread_all: usize,
    pub(super) flagged: usize,
    pub(super) folder_count: HashMap<Folder, usize>,
    pub(super) folder_unread: HashMap<Folder, usize>,
    /// the further accounts' folders: (account, folder) → messages / unread
    pub(super) acct_count: HashMap<(u32, Folder), usize>,
    pub(super) acct_unread: HashMap<(u32, Folder), usize>,
    /// unread in the inboxes of every account
    pub(super) inbox_unread_all: usize,
    pub(super) tags: std::collections::BTreeMap<String, usize>,
    pub(super) group_total: HashMap<String, usize>,
}

/// Where a search looks: everywhere, in received mail, or in sent mail.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Scope {
    All,
    Received,
    Sent,
}

impl Scope {
    pub(super) fn holds(self, m: &Message) -> bool {
        match self {
            Scope::All => m.folder != Folder::Trash,
            Scope::Received => !matches!(m.folder, Folder::Sent | Folder::Drafts | Folder::Outbox | Folder::Trash),
            Scope::Sent => m.folder == Folder::Sent,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum MailView {
    Folder(Folder),
    Unread,
    Flagged,
    /// Received in the last 7 days and not answered yet.
    NoReply,
    /// Messages with the tag in `MailApp::label_filter`.
    Label,
}

impl MailView {
    /// The view's name in the interface language. Read once per language and kept, so callers
    /// keep getting a `&'static str` (as `Folder::name`).
    pub(super) fn title(self) -> &'static str {
        static NAMES: std::sync::OnceLock<[[&'static str; 4]; 2]> = std::sync::OnceLock::new();
        let i = match self {
            MailView::Folder(f) => return f.name(),
            MailView::Unread => 0,
            MailView::Flagged => 1,
            MailView::NoReply => 2,
            MailView::Label => 3,
        };
        let names = NAMES.get_or_init(|| {
            let keep = |s: std::borrow::Cow<str>| -> &'static str { Box::leak(s.into_owned().into_boxed_str()) };
            ["en", "pl"].map(|l| {
                [
                    keep(t!("mail.list.title_unread", locale = l)),
                    keep(t!("mail.list.title_flagged", locale = l)),
                    keep(t!("mail.list.title_no_reply", locale = l)),
                    keep(t!("mail.list.title_tag", locale = l)),
                ]
            })
        });
        names[crate::i18n::is_pl() as usize][i]
    }
}

pub(super) const LOGO_URL: &str = "https://www.google.com/s2/favicons?domain=";

impl MailApp {
    /// Notices changes to the mailbox (cheaply) and refreshes the lookup table when needed.
    pub(super) fn refresh_caches(&mut self) {
        // Looking through every message takes a few milliseconds with tens of thousands of them,
        // so it is done only when something could have changed them (a click or key, news from
        // the network, a message window closing…), and every 2 seconds just in case.
        let now = self.ctx.input(|i| i.time);
        self.db_dirty |= self.touched;
        if !self.touched && now - self.last_check < 2.0 && self.index.len() == self.store.messages.len() {
            return;
        }
        self.touched = false;
        self.last_check = now;
        let ssig = self.store.messages.iter().fold(self.store.messages.len() as u64, |h, m| {
            let x = m.id
                ^ ((m.folder as u64) << 40)
                ^ (m.subject.len() as u64).rotate_left(23)
                ^ ((m.snippet.len() as u64) << 1 | m.partial as u64).rotate_left(31)
                ^ (m.date.and_utc().timestamp() as u64).rotate_left(17);
            h.wrapping_mul(0x100000001B3).wrapping_add(x)
        });
        if ssig != self.struct_sig {
            self.struct_sig = ssig;
            self.start_topics();
        }
        let sig = self.store.messages.iter().fold(self.store.messages.len() as u64, |h, m| {
            let x = m.id
                ^ ((m.snippet.len() as u64) << 1 | m.partial as u64).rotate_left(29)
                ^ ((m.folder as u64) << 40)
                ^ ((m.read as u64) << 48)
                ^ ((m.flagged as u64) << 49)
                ^ ((m.labels.len() as u64) << 50)
                ^ (m.date.and_utc().timestamp() as u64).rotate_left(17)
                ^ (m.labels.iter().map(|l| l.len() as u64).sum::<u64>() << 56);
            h.wrapping_mul(0x100000001B3).wrapping_add(x)
        });
        if sig != self.msg_sig || self.index.len() != self.store.messages.len() {
            self.db_dirty = true;
            self.msg_sig = sig;
            self.index = self.store.messages.iter().enumerate().map(|(i, m)| (m.id, i)).collect();
        }
    }

    pub(super) fn counts(&mut self) -> Arc<Counts> {
        // only the newest messages are in yet: no numbers rather than wrong ones
        if self.preview.is_some() {
            return Arc::default();
        }
        // (tags are counted for the account in focus: a change of account counts again)
        let key = self.msg_sig ^ (self.view_account as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        if let Some((sig, c)) = &self.counts {
            if *sig == key {
                return c.clone();
            }
        }
        let _p = crate::prof::span("  counts: rebuild");
        let mut c = Counts::default();
        // counted by the tag names as they are (no copies); copied once per tag at the end
        let mut tags: HashMap<&str, usize> = HashMap::new();
        let mut group_total: HashMap<&str, usize> = HashMap::new();
        let mut folder_count = [0usize; 16];
        let mut folder_unread = [0usize; 16];
        for m in &self.store.messages {
            let fi = m.folder as usize & 15;
            if m.account == 0 {
                folder_count[fi] += 1;
            } else {
                *c.acct_count.entry((m.account, m.folder)).or_default() += 1;
            }
            if !m.read {
                if m.account == 0 {
                    folder_unread[fi] += 1;
                } else {
                    *c.acct_unread.entry((m.account, m.folder)).or_default() += 1;
                }
                if m.folder == Folder::Inbox {
                    c.inbox_unread_all += 1;
                }
                // "Nieprzeczytane": every account together
                if !matches!(m.folder, Folder::Trash | Folder::Junk) {
                    c.unread_all += 1;
                }
            }
            if m.folder == Folder::Trash {
                continue;
            }
            // ("Oznaczone": the main account only)
            if m.flagged && m.account == 0 {
                c.flagged += 1;
            }
            // tags: of the account in focus only
            if m.account != self.view_account {
                continue;
            }
            match m.labels.len() {
                0 => {}
                1 => {
                    let l = m.labels[0].as_str();
                    *tags.entry(l).or_default() += 1;
                    *group_total.entry(l.split_once('/').map(|x| x.0).unwrap_or(l)).or_default() += 1;
                }
                _ => {
                    let mut groups: Vec<&str> = Vec::new();
                    for l in &m.labels {
                        *tags.entry(l.as_str()).or_default() += 1;
                        let g = l.split_once('/').map(|x| x.0).unwrap_or(l);
                        if !groups.contains(&g) {
                            groups.push(g);
                        }
                    }
                    for g in groups {
                        *group_total.entry(g).or_default() += 1;
                    }
                }
            }
        }
        for f in Folder::ALL {
            let i = f as usize & 15;
            c.folder_count.insert(f, folder_count[i]);
            c.folder_unread.insert(f, folder_unread[i]);
        }
        c.tags = tags.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        c.group_total = group_total.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let c = Arc::new(c);
        self.counts = Some((key, c.clone()));
        c
    }

    pub(super) fn thread_of(&self, m: &Message) -> Arc<Vec<u64>> {
        if let Some((id, sig, t)) = self.thread.borrow().as_ref() {
            if *id == m.id && *sig == self.msg_sig {
                return t.clone();
            }
        }
        // the conversation index (still being made: nothing shown yet, it appears in a moment)
        let Some((_, topics)) = self.topics.as_ref() else { return Arc::new(vec![]) };
        let topics = topics.clone();
        let _p = crate::prof::span("  conversation: find");
        let topic = thread_subject(&m.subject);
        let me = self.store.account.email.trim().to_lowercase();
        let people = |x: &Message| -> Vec<String> {
            std::iter::once(x.from_addr.clone())
                .chain(x.to.split(',').chain(x.cc.split(',')).map(|p| split_address(p).1))
                .map(|a| a.trim().to_lowercase())
                .filter(|a| a.contains('@') && *a != me)
                .collect()
        };
        let mine = people(m);
        // a forward of yours is part of the conversation even when it went to someone new
        let my_forward = |x: &Message| x.folder == Folder::Sent && is_forward_subject(&x.subject);
        let open_is_forward = my_forward(m);
        let mut found: Vec<&Message> = match topics.get(&topic) {
            Some(ids) if topic.len() >= 3 => ids
                .iter()
                .filter(|id| **id != m.id)
                .filter_map(|id| self.msg(*id))
                .filter(|x| !matches!(x.folder, Folder::Trash | Folder::Junk | Folder::Drafts | Folder::Outbox))
                .filter(|x| mine.is_empty() || open_is_forward || my_forward(x) || people(x).iter().any(|a| mine.contains(a)))
                .collect(),
            _ => vec![],
        };
        found.sort_by_key(|x| x.date);
        let t = Arc::new(found.iter().map(|x| x.id).collect::<Vec<u64>>());
        *self.thread.borrow_mut() = Some((m.id, self.msg_sig, t.clone()));
        t
    }

    /// One message of a conversation as a card: folded to one line, or unfolded with its text.
    /// Returns true when its header was clicked (to fold or unfold it).
    pub(super) fn thread_card(&mut self, ui: &mut Ui, x: &Message, unfolded: bool) -> bool {
        let when = format!("{} {}", i18n::date(x.date.date()), x.date.format("%H:%M"));
        let who = if x.from_name.is_empty() { x.from_addr.clone() } else { x.from_name.clone() };
        let card = SURFACE();
        let hot = SELECTED();
        if !unfolded {
            let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 54.0), Sense::click());
            let r = r.shrink2(Vec2::new(0.0, 3.0));
            ui.painter().rect_filled(r, CornerRadius::same(6), if resp.hovered() { hot } else { card });
            self.avatar_or_logo(ui, Pos2::new(r.left() + 26.0, r.center().y), 16.0, &x.from_name, &x.from_addr);
            let p = ui.painter();
            let tw = text_width(ui, &when, FontId::proportional(12.5));
            p.text(Pos2::new(r.right() - 12.0, r.top() + 7.0), Align2::RIGHT_TOP, &when, FontId::proportional(12.5), MUTED());
            line_f(p, Pos2::new(r.left() + 52.0, r.top() + 5.0), &who, semibold(14.0), ACCENT(), r.width() - 80.0 - tw);
            line(p, Pos2::new(r.left() + 52.0, r.top() + 26.0), &x.preview(), 13.0, TEXT2(), r.width() - 66.0);
            // a double click opens the message in a window of its own (both clicks also fold
            // and unfold it, so it stays as it was); a formatted one opens at the first click
            let formatted = !x.html_path.is_empty();
            if resp.double_clicked() && !formatted {
                self.actions.push(Action::OpenWindow(x.id));
            }
            let tip = if formatted { t!("mail.list.tip_open") } else { t!("mail.list.tip_unfold") };
            return resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip).clicked();
        }
        let mut clicked = false;
        Frame::new()
            .fill(card)
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin { left: 0, right: 12, top: 0, bottom: 10 })
            .outer_margin(Margin::symmetric(0, 3))
            .show(ui, |ui| {
                let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 50.0), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(r, CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, hot);
                }
                self.avatar_or_logo(ui, Pos2::new(r.left() + 26.0, r.center().y), 16.0, &x.from_name, &x.from_addr);
                let p = ui.painter();
                let tw = text_width(ui, &when, FontId::proportional(12.5));
                p.text(Pos2::new(r.right(), r.top() + 8.0), Align2::RIGHT_TOP, &when, FontId::proportional(12.5), MUTED());
                line_f(p, Pos2::new(r.left() + 52.0, r.top() + 6.0), &who, semibold(14.0), ACCENT(), r.width() - 70.0 - tw);
                let to_name = if x.to.is_empty() { "—".to_string() } else { split_address(x.to.split(',').next().unwrap_or("")).0 };
                line(p, Pos2::new(r.left() + 52.0, r.top() + 27.0), &t!("mail.list.to_name", name = to_name), 12.5, MUTED(), r.width() - 270.0);
                if resp.double_clicked() {
                    self.actions.push(Action::OpenWindow(x.id));
                }
                clicked = resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked();
                // what to do with this message, under its time (as on the open message)
                let tools = [
                    (ic::OPEN, t!("mail.list.open")),
                    (ic::FORWARD, t!("mail.list.forward")),
                    (ic::REPLY_ALL, t!("mail.list.reply_all")),
                    (ic::REPLY, t!("mail.list.reply")),
                    (ic::LINK, t!("mail.list.copy_for_ai")),
                    (ic::MORE, t!("mail.list.more")),
                ];
                for (k, (glyph, tip)) in tools.iter().enumerate() {
                    // (the reply buttons and Otwórz on the right, the AI link and the menu before them)
                    let slot = if k >= 4 { k + 1 } else { k };
                    let b = Rect::from_min_size(Pos2::new(r.right() - 28.0 - slot as f32 * 30.0, r.top() + 22.0), Vec2::splat(28.0));
                    let br = ui.interact(b, id(("card-tool", x.id, k)), Sense::click());
                    if br.hovered() {
                        ui.painter().rect_filled(b, CornerRadius::same(5), HOVER());
                    }
                    paint_icon(ui.painter(), b.center(), glyph, 15.0, MUTED());
                    hint_above(ui, &br, tip);
                    if k == 5 {
                        let (id, read, flagged) = (x.id, x.read, x.flagged);
                        let br = br.on_hover_cursor(egui::CursorIcon::PointingHand);
                        egui::Popup::menu(&br).show(|ui| self.message_menu(ui, id, read, flagged));
                        continue;
                    }
                    if br.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        self.actions.push(match k {
                            0 => Action::Select(x.id),
                            1 => Action::Forward(x.id),
                            2 => Action::Reply(x.id, true),
                            3 => Action::Reply(x.id, false),
                            _ => Action::CopyForAi(x.id),
                        });
                    }
                }
                ui.horizontal(|ui| {
                    // the text uses the card's whole width (the same margin as on the right)
                    ui.add_space(12.0);
                    ui.vertical(|ui| {
                        let blocks = self.card_blocks(x);
                        let body = x.body.trim();
                        if let Some(b) = blocks.filter(|b| !b.is_empty()) {
                            // the formatted version, as the sender wrote it (wider than the card:
                            // it scrolls sideways inside it instead of pushing the card out)
                            Frame::new().fill(BG()).corner_radius(CornerRadius::same(4)).inner_margin(Margin::same(12)).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                egui::ScrollArea::horizontal().id_salt(("card-html", x.id)).show(ui, |ui| self.render_html(ui, x, &b));
                            });
                        } else if body.is_empty() && x.partial {
                            ui.label(RichText::new(t!("mail.list.body_not_downloaded")).color(MUTED()));
                        } else {
                            // very long texts are cut here; "Otwórz" shows the whole message
                            let cut: String = body.chars().take(8000).collect();
                            let more = cut.len() < body.len();
                            ui.add(egui::Label::new(RichText::new(cut).size(13.5).color(TEXT())).selectable(true).wrap());
                            if more {
                                ui.label(RichText::new(t!("mail.list.body_cut")).size(12.5).color(MUTED()));
                            }
                        }
                        if !x.attachments.is_empty() {
                            ui.label(RichText::new(format!("📎 {}", x.attachments.join(", "))).size(12.5).color(MUTED()));
                        }
                    });
                });
            });
        clicked
    }

    /// Earlier messages of the conversation as cards above the open message: one line each,
    /// unfolded in place with a click (and folded again the same way). Newer ones as a link.
    /// Returns whether the open message belongs to a conversation.
    pub(super) fn thread_cards(&mut self, ui: &mut Ui, m: &Message) -> bool {
        self.thread_later.clear();
        let others = self.thread_of(m);
        if others.is_empty() {
            return false;
        }
        let msgs: Vec<Message> = others.iter().filter_map(|id| self.msg(*id).cloned()).collect();
        let (earlier, later): (Vec<&Message>, Vec<&Message>) = msgs.iter().partition(|x| (x.date, x.id) < (m.date, m.id));
        let open = self.thread_open == Some(m.id);
        let shown: Vec<&Message> = if open || earlier.len() <= 2 { earlier.clone() } else { earlier[earlier.len() - 2..].to_vec() };
        let hidden = earlier.len() - shown.len();
        if hidden > 0 {
            let label = i18n::plural("mail.list.older_show", hidden);
            let w = text_width(ui, &label, FontId::proportional(13.0)) + 28.0;
            let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::hover());
            let r = Rect::from_center_size(row.center(), Vec2::new(w, 26.0));
            let resp = ui.interact(r, id(("thread-more", m.id)), Sense::click());
            ui.painter().rect_filled(r, CornerRadius::same(13), if resp.hovered() { HOVER() } else { BG() });
            ui.painter().rect_stroke(r, CornerRadius::same(13), Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
            ui.painter().text(r.center(), Align2::CENTER_CENTER, &label, FontId::proportional(13.0), TEXT());
            if resp.clicked() {
                self.thread_open = Some(m.id);
            }
        }
        // the cards scroll in their own space; with the open message folded they may use it all
        // (the newer ones are known now: their room below is kept free)
        self.thread_later = later.iter().map(|x| x.id).collect();
        let max_h = if self.main_folded { (ui.available_height() - 70.0 - self.later_height(m.id)).max(60.0) } else { (ui.available_height() * 0.55).max(160.0) };
        egui::ScrollArea::vertical().id_salt(("thread", m.id)).max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
            for x in &shown {
                let unfolded = self.thread_unfolded.contains(&x.id);
                if unfolded && x.on_disk {
                    self.ensure_body(x.id);
                }
                if self.thread_card(ui, x, unfolded) {
                    if unfolded {
                        self.thread_unfolded.remove(&x.id);
                    } else if !x.html_path.is_empty() {
                        // a formatted one opens in the email view, so it looks exactly as the open one
                        self.actions.push(Action::Select(x.id));
                    } else {
                        self.thread_unfolded.insert(x.id);
                        if !x.read {
                            self.actions.push(Action::SetRead(x.id, true));
                        }
                    }
                }
            }
        });
        ui.add_space(6.0);
        true
    }

    /// The conversation's messages newer than the open one, below it, folded to one line each
    /// (oldest first, all of them, scrolling in the room of `LATER_SHOWN`); a click opens one, a
    /// double click opens it in a window of its own.
    pub(super) fn later_cards(&mut self, ui: &mut Ui, open: u64) {
        let ids: Vec<u64> = self.thread_later.clone();
        if ids.is_empty() || self.thread_for != Some(open) {
            return;
        }
        ui.add_space(6.0);
        // (under the email view only its kept room; under a plain text whatever is left too)
        let room = (ids.len().min(LATER_SHOWN) as f32 * 54.0).max(ui.available_height() - 28.0);
        egui::ScrollArea::vertical().id_salt(("later", open)).max_height(room).auto_shrink([false, true]).show(ui, |ui| {
            for id in &ids {
                let Some(x) = self.msg(*id).cloned() else { continue };
                if self.thread_card(ui, &x, false) {
                    self.actions.push(Action::Select(x.id));
                }
            }
        });
        if ids.len() > LATER_SHOWN {
            let label = i18n::plural("mail.list.newer_goto", ids.len());
            let resp = ui.add(egui::Label::new(RichText::new(label).size(12.5).color(ACCENT())).sense(Sense::click()));
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                if let Some(last) = ids.last() {
                    self.actions.push(Action::Select(*last));
                }
            }
        }
    }

    /// Room the newer messages take below the open one.
    pub(super) fn later_height(&self, open: u64) -> f32 {
        if self.thread_for != Some(open) || self.thread_later.is_empty() {
            return 0.0;
        }
        let n = self.thread_later.len().min(LATER_SHOWN) as f32;
        6.0 + n * 54.0 + if self.thread_later.len() > LATER_SHOWN { 22.0 } else { 0.0 }
    }

    /// The current list (made again only when something it depends on changed).
    pub(super) fn vis_list(&self) -> Arc<VisList> {
        let today = Local::now().date_naive();
        let hits = self.finder.hits().map(|h| (h.as_ptr() as usize, h.len())).unwrap_or((0, 0));
        // folders and search results don't change with read / flagged / tags (only these views do),
        // so opening a message (which marks it read) doesn't sort the whole folder again
        let about_flags = matches!(self.view, MailView::Unread | MailView::Flagged | MailView::Label | MailView::NoReply) && self.search.trim().is_empty();
        let sig = if about_flags { self.msg_sig } else { self.struct_sig };
        // (the list without replies also changes when the replies have been found again)
        let sig = if self.view == MailView::NoReply { sig ^ self.answered.0.rotate_left(17) ^ (self.prefs.no_reply_needed.len() as u64).rotate_left(40) } else { sig };
        // ("Nieprzeczytane" keeps the open message: the list depends on which one that is)
        let kept = if self.view == MailView::Unread { self.selected.unwrap_or(0) } else { 0 };
        let key = (self.view, format!("{}#{}#{}", self.label_filter, self.view_account, kept), self.search.trim().to_lowercase(), hits, sig, today, self.scope);
        if let Some(v) = self.vis.borrow().iter().find(|v| v.key == key) {
            return v.clone();
        }
        let _p = crate::prof::span("  list: rebuild rows");
        let mut list = self.visible();
        let mut found = [0usize; 3];
        if !key.2.is_empty() {
            // results everywhere, then only those where the person chose to look
            found = [
                list.len(),
                list.iter().filter(|m| Scope::Received.holds(m)).count(),
                list.iter().filter(|m| Scope::Sent.holds(m)).count(),
            ];
            list.retain(|m| self.scope.holds(m));
        }
        let unread = list.iter().filter(|m| !m.read).count();
        let searching = !key.2.is_empty();
        let mut rows = Vec::with_capacity(list.len() + 8);
        let mut y = 0.0;
        let mut last_group = u8::MAX;
        for m in &list {
            let g = group_of(m.date.date(), today);
            // (search results: no date headings, those found by meaning come after the rest)
            if g != last_group && !searching {
                last_group = g;
                rows.push((Row::Group(g), y));
                y += GROUP_H;
            }
            rows.push((Row::Msg(m.id), y));
            y += ROW_H;
        }
        let v = Arc::new(VisList { key, found, ids: list.iter().map(|m| m.id).collect(), unread, rows, height: y });
        {
            let mut cache = self.vis.borrow_mut();
            cache.insert(0, v.clone());
            cache.truncate(4);
        }
        v
    }

    pub(super) fn visible(&self) -> Vec<&Message> {
        let q = self.search.trim().to_lowercase();
        // Searching: results from the search index, newest first; those found only by their
        // meaning (not their words) after the others.
        if !q.is_empty() {
            if let Some(hits) = self.finder.hits() {
                // (only in the account in focus)
                let mut v: Vec<&Message> = hits.iter().filter_map(|id| self.msg(*id)).filter(|m| m.folder != Folder::Trash && m.account == self.view_account).collect();
                v.sort_by_key(|m| (self.finder.by_meaning.contains(&m.id), std::cmp::Reverse(m.date)));
                return v;
            }
            // the index is ready and the answer is a few milliseconds away: no stopgap scan
            if self.finder.has_index() {
                return vec![];
            }
        }
        let mut v: Vec<&Message> = self
            .store
            .messages
            .iter()
            .filter(|m| {
                if !q.is_empty() {
                    // Search looks through every folder of the account in focus, except the bin.
                    return m.folder != Folder::Trash && m.account == self.view_account;
                }
                match self.view {
                    MailView::Folder(f) => m.folder == f && m.account == self.view_account,
                    // (the message open now stays, though read, until another one is chosen)
                    MailView::Unread => (!m.read || self.selected == Some(m.id)) && !matches!(m.folder, Folder::Trash | Folder::Junk),
                    MailView::Flagged => m.flagged && m.folder != Folder::Trash && m.account == 0,
                    MailView::NoReply => self.waits_for_reply(m),
                    MailView::Label => {
                        m.folder != Folder::Trash
                            && m.account == self.view_account
                            && m.labels.iter().any(|l| {
                                *l == self.label_filter || (self.label_filter.ends_with('/') && l.starts_with(&self.label_filter))
                            })
                    }
                }
            })
            .filter(|m| {
                // (only while the search index is still being built: subjects and people, not
                // the whole texts, so typing never waits)
                q.is_empty()
                    || m.subject.to_lowercase().contains(&q)
                    || m.from_name.to_lowercase().contains(&q)
                    || m.from_addr.to_lowercase().contains(&q)
                    || m.to.to_lowercase().contains(&q)
            })
            .collect();
        v.sort_by(|a, b| b.date.cmp(&a.date));
        v
    }

    /// Received in the last 7 days, not answered, from a person (not an automatic sender).
    pub(super) fn waits_for_reply(&self, m: &Message) -> bool {
        // ("Bez odpowiedzi": the main account only; only "Nieprzeczytane" joins every account)
        if m.account != 0 || m.folder != Folder::Inbox || self.answered.1.contains_key(&m.id) || self.prefs.no_reply_needed.contains(&m.id) {
            return false;
        }
        let week_ago = Local::now().naive_local() - Duration::days(7);
        if m.date < week_ago {
            return false;
        }
        let from = m.from_addr.to_lowercase();
        let acc = self.account_ref(m.account);
        let me = acc.email.trim().to_lowercase();
        if from.is_empty() || (!me.is_empty() && from == me) {
            return false;
        }
        // only what was written to you (you in "Do"), not copies or mail to a list or to others
        let to = m.to.to_lowercase();
        let login = acc.username.trim().to_lowercase();
        let to_me = (!me.is_empty() && to.contains(&me)) || (login.contains('@') && to.contains(&login));
        if !to_me {
            return false;
        }
        const AUTOMATIC: [&str; 12] =
            ["noreply", "no-reply", "no_reply", "donotreply", "do-not-reply", "notification", "notifications@", "mailer-daemon", "newsletter", "postmaster", "bounce", "alert"];
        !AUTOMATIC.iter().any(|a| from.contains(a))
    }

    pub(super) fn visible_ids(&self) -> Vec<u64> {
        self.vis_list().ids.clone()
    }

    /// The logo of the sender's company website, if it has one (personal mailboxes don't).
    /// Logos are fetched once from Google's public site-icon service and kept on disk.
    pub(super) fn logo_for(&mut self, email: &str) -> Option<(String, Arc<[u8]>)> {
        const PERSONAL: &[&str] = &[
            "gmail.com", "googlemail.com", "wp.pl", "o2.pl", "onet.pl", "op.pl", "onet.eu", "interia.pl", "interia.eu",
            "poczta.fm", "tlen.pl", "gazeta.pl", "outlook.com", "hotmail.com", "live.com", "msn.com", "yahoo.com",
            "icloud.com", "me.com", "proton.me", "protonmail.com", "aol.com", "gmx.com", "gmx.net", "mail.com",
            "yandex.com", "example.com", "example.org", "example.net", "example.biz",
        ];
        let domain = email.rsplit('@').next()?.trim().trim_end_matches('>').to_lowercase();
        if !email.contains('@') || !domain.contains('.') || domain.contains(' ') {
            return None;
        }
        // "mail.firma.com.pl" → "firma.com.pl", "news.firma.pl" → "firma.pl"
        let parts: Vec<&str> = domain.split('.').collect();
        let keep = if parts.len() >= 3 && matches!(parts[parts.len() - 2], "com" | "co" | "org" | "net" | "gov" | "edu") { 3 } else { 2 };
        let domain = parts[parts.len().saturating_sub(keep)..].join(".");
        if PERSONAL.contains(&domain.as_str()) {
            return None;
        }
        let key = format!("logo:{domain}");
        if let Some(state) = self.images.get(&key) {
            return match state {
                ImgState::Ready(b) => Some((domain, b.clone())),
                _ => None,
            };
        }
        let file = crate::model::cache_dir().join(format!("logo-{domain}.img"));
        if let Ok(bytes) = std::fs::read(&file) {
            let state = if bytes.is_empty() { ImgState::Failed } else { ImgState::Ready(Arc::from(bytes)) };
            self.images.insert(key, state.clone());
            return match state {
                ImgState::Ready(b) => Some((domain, b)),
                _ => None,
            };
        }
        self.images.insert(key, ImgState::Loading);
        net::fetch_image(format!("{LOGO_URL}{domain}&sz=128"), self.img_tx.clone(), self.ctx.clone());
        None
    }

    /// The round picture for a person: their company logo, or coloured initials.
    pub(super) fn avatar_or_logo(&mut self, ui: &Ui, center: Pos2, r: f32, name: &str, email: &str) {
        match self.logo_for(email) {
            Some((domain, bytes)) => {
                let p = ui.painter();
                p.circle_filled(center, r, LOGO_BG());
                p.circle_stroke(center, r, Stroke::new(1.0, BORDER()));
                let rect = Rect::from_center_size(center, Vec2::splat(r * 1.3));
                egui::Image::from_bytes(format!("bytes://logo/{domain}"), egui::load::Bytes::Shared(bytes))
                    .corner_radius(CornerRadius::same((r * 0.25) as u8))
                    .paint_at(ui, rect);
            }
            None => avatar(ui.painter(), center, r, name),
        }
    }

    pub(super) fn message_list(&mut self, ui: &mut Ui) {
        self.list_rect = Some(ui.max_rect());
        let today = Local::now().date_naive();
        let vis = self.vis_list();
        let title = if self.loading.is_some() {
            t!("mail.list.loading_mailbox").into_owned()
        } else if self.search.trim().is_empty() {
            let name = if self.view == MailView::Label && self.label_filter == FAVOURITES {
                // the Gmail label keeps its Polish name; the heading follows the language
                t!("ui.sidebar.favourites").into_owned()
            } else if self.view == MailView::Label { pretty_label(self.label_filter.trim_end_matches('/')) } else { self.view.title().to_string() };
            let unread = match self.view {
                MailView::Folder(f) => self.counts().folder_unread.get(&f).copied().unwrap_or(0),
                _ => vis.unread,
            };
            format!("{} ({} / {})", name, unread, vis.ids.len())
        } else {
            t!("mail.list.search_results", n = vis.ids.len()).into_owned()
        };
        let (hr, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 46.0), Sense::hover());
        line_f(ui.painter(), Pos2::new(hr.left() + 14.0, hr.top() + 12.0), &title, semibold(17.0), TEXT(), hr.width() - 28.0);
        if !self.search.trim().is_empty() {
            // where to look: everywhere, received or sent mail, each with its number of results
            let (sr, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::hover());
            let mut x = sr.left() + 12.0;
            for (i, (scope, name)) in [(Scope::All, t!("mail.list.scope_all")), (Scope::Received, t!("mail.list.scope_received")), (Scope::Sent, t!("mail.list.scope_sent"))].into_iter().enumerate() {
                let label = format!("{name} {}", vis.found[i]);
                let w = text_width(ui, &label, FontId::proportional(12.5)) + 18.0;
                let r = Rect::from_min_size(Pos2::new(x, sr.top() + 2.0), Vec2::new(w, 24.0));
                let resp = ui.interact(r, id(("search-scope", i)), Sense::click());
                let on = self.scope == scope;
                let p = ui.painter();
                p.rect_filled(r, CornerRadius::same(12), if on { ACCENT_SOFT() } else if resp.hovered() { HOVER() } else { Color32::TRANSPARENT });
                p.rect_stroke(r, CornerRadius::same(12), Stroke::new(1.0, if on { ACCENT() } else { BORDER() }), egui::StrokeKind::Inside);
                p.text(r.center(), Align2::CENTER_CENTER, &label, FontId::proportional(12.5), if on { ACCENT() } else { TEXT() });
                if resp.clicked() && !on {
                    self.scope = scope;
                    let ids = self.visible_ids();
                    if !self.selected.map(|s| ids.contains(&s)).unwrap_or(false) {
                        self.selected = ids.first().copied();
                    }
                }
                x += w + 6.0;
            }
            let vis2 = self.vis_list();
            if !Arc::ptr_eq(&vis, &vis2) {
                ui.ctx().request_repaint();
            }
        }

        // Only the rows on screen are drawn, so a mailbox of any size scrolls smoothly.
        egui::ScrollArea::vertical().id_salt("list").auto_shrink([false, false]).show_viewport(ui, |ui, view| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if vis.rows.is_empty() {
                if self.loading.is_none() {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new(if self.search.is_empty() { t!("mail.list.empty_folder") } else { t!("mail.list.no_matches") })
                                .color(MUTED()),
                        );
                    });
                }
                return;
            }
            let origin = ui.max_rect().min;
            let w = ui.available_width();
            ui.set_min_height(vis.height + 8.0);
            let first = vis.rows.partition_point(|(_, y)| *y + ROW_H < view.min.y);
            for (row, y) in &vis.rows[first..] {
                if *y > view.max.y {
                    break;
                }
                match *row {
                    Row::Group(g) => {
                        let gr = Rect::from_min_size(origin + Vec2::new(0.0, *y), Vec2::new(w, GROUP_H));
                        let p = ui.painter();
                        chevron(p, Pos2::new(gr.left() + 16.0, gr.center().y), true, MUTED());
                        p.text(Pos2::new(gr.left() + 30.0, gr.center().y), Align2::LEFT_CENTER, group_title(g), FontId::proportional(14.5), MUTED());
                        p.hline(gr.left() + 10.0..=gr.right() - 10.0, gr.bottom() - 1.0, Stroke::new(1.0, BORDER()));
                    }
                    Row::Msg(id) => {
                        let Some(m) = self.msg(id).cloned() else { continue };
                        let r = Rect::from_min_size(origin + Vec2::new(0.0, *y), Vec2::new(w, ROW_H));
                        ui.scope_builder(egui::UiBuilder::new().max_rect(r), |ui| self.message_row(ui, &m, today));
                    }
                }
            }
        });
    }

    pub(super) fn message_row(&mut self, ui: &mut Ui, m: &Message, today: NaiveDate) {
        let w = ui.available_width();
        let (outer, resp) = ui.allocate_exact_size(Vec2::new(w, ROW_H), Sense::click());
        let r = outer.shrink2(Vec2::new(4.0, 1.0));
        let on = self.selected == Some(m.id);
        let p = ui.painter();
        if on {
            // the open message in the accent's light tint (unread mail has its own dot)
            p.rect_filled(r, CornerRadius::same(3), ACCENT_SOFT());
        } else if resp.hovered() {
            p.rect_filled(r, CornerRadius::same(3), HOVER());
        }
        let outgoing = matches!(m.folder, Folder::Sent | Folder::Drafts | Folder::Outbox);
        let (who, avatar_name) = if outgoing {
            if m.to.is_empty() {
                let none = t!("mail.list.no_recipient").into_owned();
                (none.clone(), none)
            } else {
                let name = split_address(m.to.split(',').next().unwrap_or("")).0;
                (t!("mail.list.to_label", name = name).into_owned(), name)
            }
        } else {
            (m.from_name.clone(), m.from_name.clone())
        };
        let email = if outgoing { split_address(m.to.split(',').next().unwrap_or("")).1 } else { m.from_addr.clone() };
        self.avatar_or_logo(ui, Pos2::new(r.left() + 28.0, r.top() + 24.0), 17.0, &avatar_name, &email);
        let p = ui.painter();
        let x = r.left() + 56.0;
        // a search result found by meaning rather than by its words gets a small "≈"
        let by_meaning = !self.search.trim().is_empty() && self.finder.by_meaning.contains(&m.id);
        let date = if by_meaning { t!("mail.list.by_meaning", date = short_date(m.date, today)).into_owned() } else { short_date(m.date, today) };
        let df = FontId::proportional(12.0);
        let dw = text_width(ui, &date, df.clone());
        p.text(Pos2::new(r.right() - 10.0, r.top() + 7.0), Align2::RIGHT_TOP, &date, df, if m.read { MUTED() } else { TEXT() });
        let name_font = if m.read { FontId::proportional(14.0) } else { semibold(14.0) };
        line_f(p, Pos2::new(x, r.top() + 5.0), &who, name_font, TEXT(), r.right() - x - dw - 20.0);
        let mut icons_w = 0.0;
        if m.flagged {
            p.text(Pos2::new(r.right() - 10.0, r.top() + 25.0), Align2::RIGHT_TOP, "⚑", FontId::proportional(13.0), FLAG);
            icons_w += 16.0;
        }
        if !m.attachments.is_empty() {
            p.text(Pos2::new(r.right() - 10.0 - icons_w, r.top() + 25.0), Align2::RIGHT_TOP, "📎", FontId::proportional(13.0), MUTED());
            icons_w += 18.0;
        }
        if let Some(f) = self.forwarded.get(&m.id).and_then(|f| self.msg(*f)) {
            let ar = Rect::from_min_size(Pos2::new(r.right() - 26.0 - icons_w, r.top() + 24.0), Vec2::new(18.0, 18.0));
            ui.painter().text(ar.center(), Align2::CENTER_CENTER, ic::FORWARD, FontId::proportional(13.0), Color32::from_rgb(0x8E, 0x5B, 0xD0));
            let tip = t!("mail.list.forwarded_tip", date = i18n::short_day_month(f.date.date()), time = f.date.format("%H:%M"), to = short_people(&f.to)).into_owned();
            ui.interact(ar, id(("forwarded", m.id)), Sense::hover()).on_hover_text(tip);
            icons_w += 18.0;
        }
        if let Some(when) = self.answered.1.get(&m.id) {
            let ar = Rect::from_min_size(Pos2::new(r.right() - 26.0 - icons_w, r.top() + 24.0), Vec2::new(18.0, 18.0));
            ui.painter().text(ar.center(), Align2::CENTER_CENTER, ic::REPLY, FontId::proportional(13.0), Color32::from_rgb(0x8E, 0x5B, 0xD0));
            let when = t!("mail.list.replied_tip", date = i18n::short_day_month(when.date()), time = when.format("%H:%M")).into_owned();
            ui.interact(ar, id(("answered", m.id)), Sense::hover()).on_hover_text(when);
            icons_w += 18.0;
        }
        let p = ui.painter();
        let subj_col = if m.read { TEXT2() } else { TEXT() };
        let subj_font = if m.read { FontId::proportional(13.5) } else { semibold(13.5) };
        // the search, marked in the subject and the start of the text
        let needle = self.search.as_str();
        line_mark(p, Pos2::new(x, r.top() + 24.0), &m.subject, subj_font, subj_col, r.right() - x - 14.0 - icons_w, needle);
        if !m.read {
            // a small blue dot left of the picture marks messages not opened yet
            p.circle_filled(Pos2::new(r.left() + 3.5, r.top() + 24.0), 3.0, ACCENT());
        }
        // Third line: the first tag (and how many more), then the start of the text.
        let mut tx = x;
        if let Some(l) = m.labels.first() {
            let room = r.right() - tx - 60.0;
            if room >= 40.0 {
                tx += tag_chip(p, Pos2::new(tx, r.top() + 43.0), &display_label(l), 11.5, room.min(130.0)) + 4.0;
                if m.labels.len() > 1 {
                    let more = format!("+{}", m.labels.len() - 1);
                    tx += p.text(Pos2::new(tx, r.top() + 43.0), Align2::LEFT_TOP, more, FontId::proportional(12.0), MUTED()).width() + 6.0;
                }
            }
        }
        line_mark(p, Pos2::new(tx + if tx > x { 2.0 } else { 0.0 }, r.top() + 43.0), &m.preview(), FontId::proportional(13.0), FAINT(), r.right() - tx - 10.0, needle);

        if resp.clicked() {
            self.actions.push(Action::Select(m.id));
        }
        if resp.double_clicked() {
            // a draft opens for writing, any other message in a window of its own
            self.actions.push(if m.folder == Folder::Drafts { Action::EditDraft(m.id) } else { Action::OpenWindow(m.id) });
        }
        let (id, read, flagged) = (m.id, m.read, m.flagged);
        resp.context_menu(|ui| self.message_menu(ui, id, read, flagged));
    }

    pub(super) fn message_menu(&mut self, ui: &mut Ui, id: u64, read: bool, flagged: bool) {
        if ui.button(t!("mail.list.menu_reply")).clicked() {
            self.actions.push(Action::Reply(id, false));
            ui.close();
        }
        if ui.button(t!("mail.list.menu_reply_all")).clicked() {
            self.actions.push(Action::Reply(id, true));
            ui.close();
        }
        if ui.button(t!("mail.list.menu_forward")).clicked() {
            self.actions.push(Action::Forward(id));
            ui.close();
        }
        let mine = self.msg(id).is_some_and(|m| {
            {
                let me = self.account_ref(m.account).email.trim();
                matches!(m.folder, Folder::Sent | Folder::Outbox) || (!me.is_empty() && m.from_addr.eq_ignore_ascii_case(me))
            }
        });
        if mine
            && ui
                .button(t!("mail.list.menu_send_again"))
                .on_hover_text(t!("mail.list.menu_send_again_tip"))
                .clicked()
        {
            self.actions.push(Action::SendAgain(id));
            ui.close();
        }
        ui.separator();
        if ui
            .button(t!("mail.list.menu_copy_link_ai"))
            .on_hover_text(t!("mail.list.menu_copy_link_ai_tip"))
            .clicked()
        {
            self.actions.push(Action::CopyForAi(id));
            ui.close();
        }
        if ui.button(t!("mail.list.menu_copy_text_ai")).on_hover_text(t!("mail.list.menu_copy_text_ai_tip")).clicked() {
            self.actions.push(Action::CopyTextForAi(id));
            ui.close();
        }
        let tr_label = if self.show_translated.contains(&id) { t!("mail.list.menu_show_original") } else { t!("mail.list.menu_translate") };
        if ui.button(tr_label).on_hover_text(t!("mail.list.menu_translate_tip")).clicked() {
            self.actions.push(Action::Translate(id));
            ui.close();
        }
        ui.separator();
        if ui.button(if read { t!("mail.list.menu_mark_unread") } else { t!("mail.list.menu_mark_read") }).clicked() {
            self.actions.push(Action::SetRead(id, !read));
            ui.close();
        }
        if ui.button(if flagged { t!("mail.list.menu_unflag") } else { t!("mail.list.menu_flag") }).clicked() {
            self.actions.push(Action::ToggleFlag(id));
            ui.close();
        }
        let marked = self.prefs.no_reply_needed.contains(&id);
        let waiting = self.msg(id).is_some_and(|m| self.waits_for_reply(m));
        if marked {
            if ui.button(t!("mail.list.menu_needs_reply")).on_hover_text(t!("mail.list.menu_needs_reply_tip")).clicked() {
                self.actions.push(Action::NoReplyNeeded(id, false));
                ui.close();
            }
        } else if waiting
            && ui
                .button(t!("mail.list.menu_no_reply"))
                .on_hover_text(t!("mail.list.menu_no_reply_tip"))
                .clicked()
        {
            self.actions.push(Action::NoReplyNeeded(id, true));
            ui.close();
        }
        let fav = self.msg(id).is_some_and(|m| m.labels.iter().any(|l| l == FAVOURITES));
        if ui
            .button(if fav { t!("mail.list.menu_unfav") } else { t!("mail.list.menu_fav") })
            .on_hover_text(t!("mail.list.menu_fav_tip"))
            .clicked()
        {
            self.actions.push(if fav { Action::RemoveTag(id, FAVOURITES.into()) } else { Action::AddTag(id, FAVOURITES.into()) });
            ui.close();
        }
        ui.menu_button(t!("mail.list.menu_tags"), |ui| self.tag_menu(ui, id));
        // a Gmail rule for the sender: only the main account signs in to Google
        let sender = self
            .msg(id)
            .filter(|m| !mine && m.account == 0 && crate::google::ready(&self.store.account))
            .map(|m| m.from_addr.trim().to_lowercase())
            .filter(|a| crate::google::blockable(a));
        if let Some(addr) = sender {
            ui.menu_button(format!("{}  {}", ic::BLOCKED, t!("mail.rules.menu_block")), |ui| {
                ui.set_max_width(340.0);
                ui.label(RichText::new(t!("mail.rules.menu_block_explain", addr = addr)).small().color(MUTED()));
                if ui.button(t!("mail.rules.menu_block_confirm", addr = addr)).clicked() {
                    self.actions.push(Action::Move(id, Folder::Junk));
                    self.actions.push(Action::BlockSender(addr.clone()));
                    ui.close();
                }
            });
        }
        ui.menu_button(t!("mail.list.menu_move_to"), |ui| {
            for f in Folder::ALL {
                if ui.button(format!("{}  {}", f.icon(), f.name())).clicked() {
                    self.actions.push(Action::Move(id, f));
                    ui.close();
                }
            }
        });
        ui.separator();
        if ui.button(t!("mail.list.menu_delete")).clicked() {
            self.actions.push(Action::Delete(id));
            ui.close();
        }
    }
}

/// Which date heading a day falls under (an index for `group_title`).
pub(super) fn group_of(d: NaiveDate, today: NaiveDate) -> u8 {
    let days = (today - d).num_days();
    if days <= 0 {
        0
    } else if days == 1 {
        1
    } else if days < 7 && d.weekday().num_days_from_monday() < today.weekday().num_days_from_monday() {
        2
    } else if days < 14 {
        3
    } else if d.month() == today.month() && d.year() == today.year() {
        4
    } else {
        5
    }
}

pub(super) fn group_title(g: u8) -> String {
    match g {
        0 => t!("mail.list.group_today"),
        1 => t!("mail.list.group_yesterday"),
        2 => t!("mail.list.group_earlier_week"),
        3 => t!("mail.list.group_last_week"),
        4 => t!("mail.list.group_earlier_month"),
        _ => t!("mail.list.group_older"),
    }
    .into_owned()
}

pub(super) fn short_date(dt: NaiveDateTime, today: NaiveDate) -> String {
    let d = dt.date();
    let days = (today - d).num_days();
    if days <= 0 {
        dt.format("%H:%M").to_string()
    } else if days < 7 {
        format!("{} {}", i18n::wd_short(d), dt.format("%H:%M"))
    } else {
        i18n::date(d)
    }
}

/// A subject without reply / forward marks ("Re:", "RE[10]:", "Fwd:", "Odp:", "PD:"…), for
/// finding the messages of one conversation.
/// A subject of a forwarded message ("Fwd:", "Fw:", "PD:", "WG:", "TR:").
pub(super) fn is_forward_subject(s: &str) -> bool {
    let t = s.trim_start().to_lowercase();
    let Some(colon) = t.find(':').filter(|i| *i <= 6) else { return false };
    let head: String = t[..colon].chars().filter(|c| !"[]() ".contains(*c)).collect();
    ["fwd", "fw", "pd", "wg", "tr", "rv"].contains(&head.as_str())
}

/// "Anna Kowalska, Piotr Nowak +2": who a message went to, shortly.
pub(super) fn short_people(list: &str) -> String {
    let people = people_in(list);
    let names: Vec<String> = people.iter().take(2).map(|p| p.0.clone()).collect();
    let mut out = names.join(", ");
    if people.len() > 2 {
        out.push_str(&format!(" +{}", people.len() - 2));
    }
    out
}

pub(super) fn thread_subject(s: &str) -> String {
    let mut t = s.trim().to_lowercase();
    loop {
        let Some(colon) = t.find(':').filter(|i| *i <= 10) else { break };
        let head: String = t[..colon].chars().filter(|c| !c.is_ascii_digit() && !"[]() ".contains(*c)).collect();
        if ["re", "fw", "fwd", "odp", "pd", "aw", "wg", "tr", "rv", "sv", "vs"].contains(&head.as_str()) {
            t = t[colon + 1..].trim_start().to_string();
        } else {
            break;
        }
    }
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// How many newer messages of a conversation are shown below the open one (folded).
pub(super) const LATER_SHOWN: usize = 3;
