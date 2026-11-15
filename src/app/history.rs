use super::*;

/// The mail exchanged with one person, as shown in the right bar: (person, mailbox fingerprint,
/// search, where to look) and what was found then.
pub(super) struct History {
    key: (String, u64, String, Scope),
    /// every message with the person (newest first), kept so typing a search does not go
    /// through the whole mailbox again
    all: Arc<Vec<u64>>,
    /// those matching the search and the scope
    ids: Arc<Vec<u64>>,
    /// how many there are everywhere, received and sent (before `Scope` narrows them)
    found: [usize; 3],
}

const ROW_H: f32 = 50.0;

/// Whether the message passed between you and `addr`: they wrote it, or you sent it to them
/// ("Do"). Mail where they were only one more recipient, or copied in, is not theirs.
fn between(m: &Message, addr: &str) -> bool {
    if addr.is_empty() {
        return false;
    }
    if m.folder != Folder::Sent {
        return m.from_addr.trim().eq_ignore_ascii_case(addr);
    }
    // (a quick look without copying the text first: most of what you sent went to others)
    m.to.as_bytes().windows(addr.len()).any(|w| w.eq_ignore_ascii_case(addr.as_bytes()))
        && m.to.split(',').any(|p| split_address(p).1.trim().eq_ignore_ascii_case(addr))
}

impl MailApp {
    /// Every address of the person's own accounts, in small letters.
    fn my_addrs(&self) -> Vec<String> {
        (0..=self.store.extra.len() as u32)
            .flat_map(|a| {
                let acc = self.account_ref(a);
                [acc.email.trim().to_lowercase(), acc.username.trim().to_lowercase()]
            })
            .filter(|a| a.contains('@'))
            .collect()
    }

    /// The person of the open message: its sender, or for your own mail the first person it went
    /// to. A message picked in the history itself keeps the person (going through what you sent
    /// them does not switch to another recipient).
    fn follow_person(&mut self) {
        if self.selected == self.hist_for {
            return;
        }
        self.hist_for = self.selected;
        // (a pick counts once: the same message chosen later in the list follows its own sender)
        let picked = self.hist_pick.take() == self.selected;
        let Some(m) = self.selected.and_then(|id| self.msg(id)) else { return };
        if picked && between(m, &self.hist_addr) {
            return;
        }
        let me = self.my_addrs();
        let people = std::iter::once((m.from_name.clone(), m.from_addr.clone()))
            .chain(m.to.split(',').filter(|p| !p.trim().is_empty()).map(split_address));
        let found = people.map(|(n, a)| (n, a.trim().to_string())).find(|(_, a)| a.contains('@') && !me.contains(&a.to_lowercase()));
        let (name, addr) = found.unwrap_or_default();
        if !addr.eq_ignore_ascii_case(&self.hist_addr) {
            self.hist_query.clear();
        }
        // (mail only between your own accounts has no one else: the history shows nobody)
        self.hist_name = if name.trim().is_empty() || name == addr { addr.clone() } else { name.trim().to_string() };
        self.hist_addr = addr;
    }

    /// The person's mail, made again only when the mailbox, the search or the scope changed.
    fn history(&self) -> Arc<History> {
        let key = (self.hist_addr.to_lowercase(), self.struct_sig, self.hist_query.trim().to_lowercase(), self.hist_scope);
        if let Some(h) = self.hist.borrow().as_ref().filter(|h| h.key == key) {
            return h.clone();
        }
        let _p = crate::prof::span("  history: rebuild");
        // the person's mail is looked for in the whole mailbox only when the person or the mailbox changed
        let same_person = self.hist.borrow().as_ref().filter(|h| h.key.0 == key.0 && h.key.1 == key.1).map(|h| h.all.clone());
        let every = same_person.unwrap_or_else(|| {
            let _p = crate::prof::span("  history: whole mailbox");
            let mut v: Vec<&Message> = self
                .store
                .messages
                .iter()
                // (drafts and mail still waiting to go are not yet part of the exchange)
                .filter(|m| !matches!(m.folder, Folder::Trash | Folder::Drafts | Folder::Outbox) && between(m, &key.0))
                .collect();
            v.sort_by_key(|m| std::cmp::Reverse(m.date));
            Arc::new(v.iter().map(|m| m.id).collect())
        });
        let q = &key.2;
        let all: Vec<&Message> = every
            .iter()
            .filter_map(|id| self.msg(*id))
            .filter(|m| q.is_empty() || m.subject.to_lowercase().contains(q) || m.preview().to_lowercase().contains(q))
            .collect();
        let found = [all.len(), all.iter().filter(|m| Scope::Received.holds(m)).count(), all.iter().filter(|m| Scope::Sent.holds(m)).count()];
        let ids = all.iter().filter(|m| self.hist_scope.holds(m)).map(|m| m.id).collect();
        let h = Arc::new(History { key, all: every, ids: Arc::new(ids), found });
        *self.hist.borrow_mut() = Some(h.clone());
        h
    }

    /// The right bar's "history" tab: every message to and from the person of the open message,
    /// received and sent told apart by colour and arrow, with a search through their subjects and
    /// the start of their text.
    pub(super) fn history_side(&mut self, ui: &mut Ui) {
        self.follow_person();
        Frame::new().inner_margin(Margin { left: 14, right: 10, top: 10, bottom: 6 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(t!("ui.history.title")).font(semibold(16.0)).color(TEXT()));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if fold_button(ui, true).on_hover_text(t!("ui.history.fold")).clicked() {
                        self.show_side = false;
                        self.save_now = true;
                    }
                });
            });
            if self.hist_addr.is_empty() {
                ui.add_space(10.0);
                ui.label(RichText::new(t!("ui.history.pick")).color(MUTED()));
                return;
            }
            ui.add_space(6.0);
            let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), Sense::hover());
            let (name, addr) = (self.hist_name.clone(), self.hist_addr.clone());
            self.avatar_or_logo(ui, Pos2::new(r.left() + 18.0, r.center().y), 17.0, &name, &addr);
            let p = ui.painter();
            line_f(p, Pos2::new(r.left() + 44.0, r.top() + 2.0), &name, semibold(14.0), TEXT(), r.width() - 48.0);
            line(p, Pos2::new(r.left() + 44.0, r.top() + 21.0), &addr, 12.0, MUTED(), r.width() - 48.0);
            ui.add_space(6.0);
            ui.add(egui::TextEdit::singleline(&mut self.hist_query).hint_text(t!("ui.history.search")).desired_width(f32::INFINITY));
            ui.add_space(6.0);
            let found = self.history().found;
            // (wrapped: long counts must not push the bar wider)
            ui.horizontal_wrapped(|ui| {
                for (i, (scope, name)) in [(Scope::All, t!("mail.list.scope_all")), (Scope::Received, t!("mail.list.scope_received")), (Scope::Sent, t!("mail.list.scope_sent"))].into_iter().enumerate() {
                    let on = self.hist_scope == scope;
                    let text = RichText::new(format!("{name} {}", found[i])).size(12.5).color(if on { ACCENT() } else { TEXT() });
                    let b = egui::Button::new(text)
                        .fill(if on { ACCENT_SOFT() } else { Color32::TRANSPARENT })
                        .stroke(Stroke::new(1.0, if on { ACCENT() } else { BORDER() }))
                        .corner_radius(CornerRadius::same(12));
                    if ui.add(b).clicked() {
                        self.hist_scope = scope;
                    }
                }
            });
        });
        if self.hist_addr.is_empty() {
            return;
        }
        let h = self.history();
        let ids = h.ids.clone();
        if ids.is_empty() {
            if !self.hist_query.trim().is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    ui.label(RichText::new(t!("mail.list.no_matches")).color(MUTED()));
                });
            }
            return;
        }
        let today = Local::now().date_naive();
        let needle = self.hist_query.trim().to_string();
        // (test picture: the first row's tooltip shown without a pointer)
        let tip_shot = self.shot.is_some() && std::env::var("POCZTA_START").as_deref() == Ok("history-tip");
        egui::ScrollArea::vertical().id_salt(("history", &self.hist_addr)).auto_shrink([false, false]).show_rows(ui, ROW_H, ids.len(), |ui, range| {
            for &mid in &ids[range] {
                let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
                let Some(m) = self.msg(mid) else { continue };
                let sent = m.folder == Folder::Sent;
                let col = if sent { SENT() } else { ACCENT() };
                let p = ui.painter();
                if self.selected == Some(mid) {
                    p.rect_filled(r.shrink2(Vec2::new(4.0, 1.0)), CornerRadius::same(4), SELECTED());
                } else if resp.hovered() {
                    p.rect_filled(r.shrink2(Vec2::new(4.0, 1.0)), CornerRadius::same(4), HOVER());
                }
                p.rect_filled(Rect::from_min_size(Pos2::new(r.left() + 6.0, r.top() + 7.0), Vec2::new(3.0, ROW_H - 14.0)), CornerRadius::same(2), col);
                // received: an arrow coming in; sent: the paper plane of "Wysłane"
                p.text(Pos2::new(r.left() + 22.0, r.top() + 15.0), Align2::CENTER_CENTER, if sent { ic::SEND } else { ic::DOWNLOAD }, FontId::proportional(13.0), col);
                let date = short_date(m.date, today);
                let dw = line(p, Pos2::new(r.right() - 10.0 - text_width(ui, &date, FontId::proportional(11.5)), r.top() + 7.0), &date, 11.5, MUTED(), 120.0);
                let subject = if m.subject.trim().is_empty() { t!("mail.reading.no_subject").into_owned() } else { m.subject.clone() };
                let font = if m.read { FontId::proportional(13.5) } else { semibold(13.5) };
                let p = ui.painter();
                line_mark(p, Pos2::new(r.left() + 34.0, r.top() + 6.0), &subject, font, TEXT(), r.width() - 50.0 - dw, &needle);
                line_mark(p, Pos2::new(r.left() + 34.0, r.top() + 26.0), &m.preview(), FontId::proportional(12.0), FAINT(), r.width() - 44.0, &needle);
                // the pointer resting on a row: the subject, when and which way, and more of the
                // text — below the row (or above it), never wider than it: the email view beside
                // the bar is a window of its own and would cover a tooltip reaching into it
                let mut tip = egui::Tooltip::for_enabled(&resp).width(r.width() - 24.0);
                tip.popup = tip.popup.align(egui::RectAlign::BOTTOM_START).align_alternatives(&[egui::RectAlign::TOP_START]);
                if tip_shot && mid == ids[0] {
                    tip.popup = tip.popup.open(true);
                }
                tip.show(|ui| {
                    ui.label(RichText::new(&subject).strong());
                    let way = if sent { t!("mail.list.scope_sent") } else { t!("mail.list.scope_received") };
                    ui.label(RichText::new(format!("{way} · {} {}", i18n::date(m.date.date()), m.date.format("%H:%M"))).color(col));
                    let text = if m.snippet.is_empty() { crate::model::snippet_of(&m.body) } else { m.snippet.clone() };
                    if !text.trim().is_empty() {
                        ui.add_space(4.0);
                        ui.label(RichText::new(text).color(TEXT2()));
                    }
                });
                if resp.clicked() || resp.double_clicked() {
                    // shown in the mail module (opening marks it read, so it must be seen), and
                    // the history stays with this person
                    self.module = Module::Mail;
                    if self.selected != Some(mid) {
                        self.hist_pick = Some(mid);
                    }
                    self.actions.push(if resp.double_clicked() { Action::OpenWindow(mid) } else { Action::Select(mid) });
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_mail_between_you_and_them() {
        let mut m = Message::blank();
        m.folder = Folder::Inbox;
        m.from_addr = "ANNA@x.pl".into();
        m.to = "\"Nowak, Jan\" <jan@x.pl>, ola@y.pl".into();
        m.cc = "Ewa <ewa@z.pl>".into();
        // received: only the sender counts, not others it went to or copied in
        assert!(between(&m, "anna@x.pl"));
        assert!(!between(&m, "ola@y.pl"));
        assert!(!between(&m, "ewa@z.pl"));
        assert!(!between(&m, ""));
        // sent: those you wrote to ("Do"), whole addresses only, not "DW"
        m.folder = Folder::Sent;
        assert!(between(&m, "OLA@y.pl"));
        assert!(between(&m, "jan@x.pl"));
        assert!(!between(&m, "a@y.pl"));
        assert!(!between(&m, "ewa@z.pl"));
        assert!(!between(&m, "anna@x.pl"));
    }
}
