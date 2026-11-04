use super::*;

/// A person of the address book being edited.
#[derive(Clone)]
pub(super) struct PersonEdit {
    pub(super) id: String,
    pub(super) name: String,
    /// one address per line
    pub(super) emails: String,
    pub(super) groups: Vec<String>,
    /// not in the book yet (e.g. someone from the mailbox)
    pub(super) new: bool,
}

/// The list of people found in the mailbox (not saved in the address book).
pub(super) const MAIL_PEOPLE: &str = "__mail";

impl MailApp {
    /// Everyone to suggest while writing: the address book's groups first, then people from
    /// the mailbox and from the address book (worked out again only when something changed).
    pub(super) fn mail_contacts(&mut self) -> Arc<Vec<crate::contacts::Contact>> {
        let sig = (self.store.messages.len(), self.store.next_id.wrapping_add(self.book.next_rev << 32).wrapping_add(self.book.people.len() as u64));
        if sig != self.contacts_sig {
            let t = std::time::Instant::now();
            let mut list: Vec<crate::contacts::Contact> = self
                .book
                .groups()
                .iter()
                .map(|g| crate::contacts::Contact::group(&g.name, self.book.group_addresses(&g.id)))
                .filter(|c| !c.members.is_empty())
                .collect();
            let mut mail = crate::contacts::collect(&self.store.messages, &self.store.account.email);
            let known: HashSet<String> = mail.iter().map(|c| c.addr.to_lowercase()).collect();
            for p in self.book.people() {
                for e in &p.emails {
                    if !known.contains(&e.to_lowercase()) {
                        mail.push(crate::contacts::Contact::person(&p.name, e));
                    }
                }
            }
            list.extend(mail);
            self.contacts = Arc::new(list);
            self.contacts_sig = sig;
            crate::diag::took("address suggestions ready", t, format!("{} people and groups", self.contacts.len()));
        }
        self.contacts.clone()
    }

    // ------------------------------------------------------------ address book

    /// The book in step with Google Contacts (through the Google script): soon after a change
    /// made here, and every 15 minutes.
    pub(super) fn book_tick(&mut self) {
        if let Some(res) = self.book_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.book_rx = None;
            match res {
                Ok(done) => {
                    let renamed = done.renamed.clone();
                    self.book.merge_after_sync(done.book, done.sent_rev, &done.renamed);
                    let rename = |id: &str| renamed.iter().find(|(a, _)| a == id).map(|(_, b)| b.clone());
                    if let Some(g) = self.book_group.clone().and_then(|g| rename(&g)) {
                        self.book_group = Some(g);
                    }
                    if let Some(e) = self.book_edit.as_mut() {
                        if let Some(n) = rename(&e.id) {
                            e.id = n;
                        }
                        e.groups = e.groups.iter().map(|g| rename(g).unwrap_or_else(|| g.clone())).collect();
                    }
                    self.book.save();
                    let n = self.book.people().count();
                    self.book_said = Some(Ok(format!("Kontakty Google: {n} {}, {}", crate::pl_people(n), self.book.synced)));
                    crate::diag::log(format!("contacts: in step with Google ({} sent, {n} people)", done.sent));
                }
                Err(e) => {
                    crate::diag::log(format!("contacts: {e}"));
                    self.book_said = Some(Err(e));
                }
            }
            self.touched = true;
        }
        if !self.google_contacts() {
            return;
        }
        if self.book_rx.is_some() || self.loading.is_some() || self.shot.is_some() || std::env::var("POCZTA_OFFLINE").is_ok() {
            return;
        }
        let now = self.clock();
        let failed = matches!(self.book_said, Some(Err(_)));
        let due = if self.book.dirty_count() > 0 && !failed { now - self.book_last > 2.0 } else { now - self.book_last > 15.0 * 60.0 };
        if !due {
            self.ctx.request_repaint_after(std::time::Duration::from_secs(if self.book.dirty_count() > 0 { 2 } else { 60 }));
            return;
        }
        self.book_last = now;
        let (tx, rx) = channel();
        let (acc, snapshot, ctx) = (self.store.account.clone(), self.book.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(crate::book::sync(&acc, &snapshot));
            ctx.request_repaint();
        });
        self.book_rx = Some(rx);
    }

    pub(super) fn book_changed(&mut self) {
        self.book.save();
        // a failed sync is tried again with the change
        if matches!(self.book_said, Some(Err(_))) {
            self.book_said = None;
        }
        self.touched = true;
    }

    pub(super) fn book_new_person(&mut self) {
        self.module = Module::Contacts;
        let groups = self.book_group.clone().filter(|g| g != MAIL_PEOPLE).into_iter().collect();
        self.book_edit = Some(PersonEdit { id: crate::book::new_id(), name: String::new(), emails: String::new(), groups, new: true });
    }

    /// A new message to everyone in a group.
    pub(super) fn write_to_group(&mut self, group: &str) {
        let to = self.book.group_addresses(group).join(", ");
        if to.is_empty() {
            self.set_status("W tej grupie nie ma jeszcze nikogo z adresem e-mail");
            return;
        }
        self.module = Module::Mail;
        self.open_composer(to, String::new(), String::new(), String::new(), None, None);
    }

    /// The left side in the contacts: everyone, people from the mailbox, the groups, and the
    /// state of the link with Google Contacts.
    pub(super) fn contacts_side(&mut self, ui: &mut Ui) {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(RichText::new("Kontakty").font(semibold(18.0)).color(TEXT()));
        });
        ui.add_space(6.0);
        let everyone = self.book.people().count();
        if folder_row(ui, "👤", MUTED(), "Wszystkie kontakty", everyone, self.book_group.is_none(), 28.0) {
            self.book_group = None;
        }
        let from_mail = self.mail_people().len();
        if folder_row(ui, "✉", MUTED(), "Z poczty", from_mail, self.book_group.as_deref() == Some(MAIL_PEOPLE), 28.0) {
            self.book_group = Some(MAIL_PEOPLE.into());
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(RichText::new("Grupy").small().color(MUTED()));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                if ui.add(egui::Button::new(RichText::new("+ Nowa grupa").size(12.5).color(ACCENT())).frame(false)).clicked() {
                    self.book_new_group = Some(String::new());
                }
            });
        });
        if let Some(name) = self.book_new_group.as_mut() {
            let (mut done, mut cancel) = (false, false);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                let r = ui.add(egui::TextEdit::singleline(name).hint_text("Nazwa grupy").desired_width(ui.available_width() - 70.0));
                r.request_focus();
                if (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))) || ui.button("OK").clicked() {
                    done = true;
                }
                if ui.input(|i| i.key_pressed(Key::Escape)) {
                    cancel = true;
                }
            });
            if cancel {
                self.book_new_group = None;
            }
            if done {
                let name = self.book_new_group.take().unwrap_or_default();
                if !name.trim().is_empty() {
                    let id = self.book.put_group(None, name.trim());
                    self.book_group = Some(id);
                    self.book_changed();
                }
            }
        }
        let groups: Vec<(String, String, usize)> = self.book.groups().iter().map(|g| (g.id.clone(), g.name.clone(), self.book.members(&g.id).len())).collect();
        for (id, name, n) in groups {
            if folder_row(ui, "👥", MUTED(), &name, n, self.book_group.as_deref() == Some(id.as_str()), 28.0) {
                self.book_group = Some(id);
            }
        }
        ui.add_space(14.0);
        Frame::new().inner_margin(Margin::symmetric(14, 0)).show(ui, |ui| {
            if !self.google_contacts() {
                ui.label(RichText::new("Kontakty są na tym komputerze. Aby mieć je wspólne z Kontaktami Google:").small().color(MUTED()));
                if ui.link(RichText::new("☰ → Kalendarz i kontakty Google").small()).clicked() {
                    self.gcal_open = true;
                }
            } else {
                if self.book_rx.is_some() {
                    ui.label(RichText::new("⟳ synchronizacja z Kontaktami Google…").small().color(MUTED()));
                } else {
                    match &self.book_said {
                        Some(Ok(m)) => {
                            ui.label(RichText::new(format!("✓ {m}")).small().color(Color32::from_rgb(0x2E, 0x9E, 0x5B)));
                        }
                        Some(Err(e)) => {
                            ui.label(RichText::new(e.as_str()).small().color(FLAG));
                        }
                        None => {}
                    }
                }
                let waiting = self.book.dirty_count();
                if waiting > 0 {
                    ui.label(RichText::new(format!("{waiting} zmian czeka na wysłanie do Google")).small().color(MUTED()));
                }
                if self.book_rx.is_none() && ui.link(RichText::new("Synchronizuj teraz").small()).clicked() {
                    self.book_last = -1e9;
                    self.book_said = None;
                }
            }
        });
    }

    /// People found in the mailbox who are not in the address book yet.
    pub(super) fn mail_people(&mut self) -> Vec<(String, String)> {
        let all = self.mail_contacts();
        all.iter()
            .filter(|c| c.members.is_empty() && !c.addr.is_empty() && self.book.person_by_email(&c.addr).is_none())
            .map(|c| (c.name.clone(), c.addr.clone()))
            .collect()
    }

    /// The middle of the contacts: the list (of a group, everyone, or from the mailbox) and the
    /// person chosen, with their groups.
    pub(super) fn contacts_main(&mut self, ui: &mut Ui) {
        let group = self.book_group.clone();
        let in_group = group.clone().filter(|g| g != MAIL_PEOPLE);
        let title = match group.as_deref() {
            None => "Wszystkie kontakty".to_string(),
            Some(MAIL_PEOPLE) => "Z poczty — osoby, które do Ciebie pisały albo do których pisałeś".to_string(),
            Some(g) => self.book.groups.iter().find(|x| x.id == g).map(|x| x.name.clone()).unwrap_or_default(),
        };
        ui.horizontal(|ui| {
            if let (Some(g), Some(name)) = (in_group.clone(), self.book_rename.as_mut()) {
                let r = ui.add(egui::TextEdit::singleline(name).desired_width(260.0).font(FontId::proportional(18.0)));
                if (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))) || ui.button("Zapisz").clicked() {
                    let n = self.book_rename.take().unwrap_or_default();
                    if !n.trim().is_empty() {
                        self.book.put_group(Some(&g), n.trim());
                        self.book_changed();
                    }
                }
            } else {
                ui.label(RichText::new(&title).font(semibold(18.0)).color(TEXT()));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(g) = in_group.clone() {
                    if ui.button("🗑 Usuń grupę").on_hover_text("Osoby zostają w kontaktach, znika tylko grupa").clicked() {
                        self.book.delete_group(&g);
                        self.book_group = None;
                        self.book_changed();
                    }
                    if ui.button("✏ Zmień nazwę").clicked() {
                        self.book_rename = Some(title.clone());
                    }
                    let n = self.book.group_addresses(&g).len();
                    if ui
                        .add(egui::Button::new(RichText::new(format!("✉  Napisz do grupy ({n})")).color(Color32::WHITE)).fill(ACCENT()))
                        .on_hover_text("Nowa wiadomość do wszystkich osób z tej grupy")
                        .clicked()
                    {
                        self.write_to_group(&g);
                    }
                }
                if ui.button("+ Nowy kontakt").clicked() {
                    self.book_new_person();
                }
            });
        });
        ui.add_space(6.0);
        // adding people to the group: type a name or an address (suggestions from the mailbox)
        if let Some(g) = in_group.clone() {
            let contacts = self.mail_contacts();
            ui.horizontal(|ui| {
                ui.label(RichText::new("Dodaj do grupy:").color(MUTED()));
                let w = (ui.available_width() - 90.0).min(520.0);
                crate::contacts::recipients_field(ui, &mut self.book_add, "Wpisz imię lub adres…", w, &contacts, ("book-add", 0));
                if ui.button("Dodaj").clicked() {
                    let text = std::mem::take(&mut self.book_add);
                    for part in text.split([',', ';']).map(str::trim).filter(|p| p.contains('@')) {
                        let (name, addr) = crate::model::split_address(part);
                        self.book.add_to_group(&g, &name, &addr);
                    }
                    self.book_changed();
                }
            });
            ui.add_space(6.0);
        }
        ui.add(egui::TextEdit::singleline(&mut self.book_search).hint_text("🔍 Szukaj w kontaktach").desired_width(320.0));
        ui.add_space(6.0);
        let q = self.book_search.trim().to_lowercase();
        // (id, name, first address, group names, from the mailbox)
        let rows: Vec<(String, String, String, String, bool)> = if group.as_deref() == Some(MAIL_PEOPLE) {
            self.mail_people().into_iter().map(|(n, a)| (format!("mail:{a}"), n, a, String::new(), true)).collect()
        } else {
            let people: Vec<&crate::book::Person> = match in_group.as_deref() {
                Some(g) => self.book.members(g),
                None => {
                    let mut v: Vec<&crate::book::Person> = self.book.people().collect();
                    v.sort_by_key(|p| p.name.to_lowercase());
                    v
                }
            };
            people
                .into_iter()
                .map(|p| {
                    let gnames: Vec<String> = p.groups.iter().filter_map(|g| self.book.groups.iter().find(|x| x.id == *g && !x.deleted).map(|x| x.name.clone())).collect();
                    (p.id.clone(), p.name.clone(), p.emails.first().cloned().unwrap_or_default(), gnames.join(", "), false)
                })
                .collect()
        };
        let rows: Vec<_> = rows.into_iter().filter(|r| q.is_empty() || r.1.to_lowercase().contains(&q) || r.2.to_lowercase().contains(&q)).collect();
        let avail = ui.available_rect_before_wrap();
        let list_w = (avail.width() * 0.5).clamp(300.0, 520.0);
        ui.horizontal_top(|ui| {
            // the list
            ui.allocate_ui_with_layout(Vec2::new(list_w, avail.height()), Layout::top_down(Align::Min), |ui| {
                if rows.is_empty() {
                    ui.add_space(20.0);
                    ui.label(RichText::new(if in_group.is_some() { "W tej grupie nikogo jeszcze nie ma — dodaj osoby polem powyżej." } else { "Brak kontaktów" }).color(MUTED()));
                }
                egui::ScrollArea::vertical().id_salt("book-list").auto_shrink([false, false]).show_rows(ui, 52.0, rows.len(), |ui, range| {
                    for (id, name, addr, gnames, from_mail) in &rows[range] {
                        let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 52.0), Sense::click());
                        let on = self.book_edit.as_ref().is_some_and(|e| e.id == *id || (*from_mail && e.emails.trim() == addr.as_str()));
                        if on {
                            ui.painter().rect_filled(r, CornerRadius::same(4), SELECTED());
                        } else if resp.hovered() {
                            ui.painter().rect_filled(r, CornerRadius::same(4), HOVER());
                        }
                        let shown = if name.trim().is_empty() { addr.clone() } else { name.clone() };
                        avatar(ui.painter(), Pos2::new(r.left() + 24.0, r.center().y), 16.0, &shown);
                        let p = ui.painter();
                        line_f(p, Pos2::new(r.left() + 50.0, r.top() + 6.0), &shown, semibold(14.0), TEXT(), r.width() - 60.0);
                        let sub = if gnames.is_empty() { addr.clone() } else { format!("{addr}  ·  👥 {gnames}") };
                        line(p, Pos2::new(r.left() + 50.0, r.top() + 27.0), &sub, 12.5, MUTED(), r.width() - 60.0);
                        if resp.clicked() {
                            self.book_edit = Some(match self.book.people.iter().find(|p| p.id == *id) {
                                Some(p) => PersonEdit { id: p.id.clone(), name: p.name.clone(), emails: p.emails.join("\n"), groups: p.groups.clone(), new: false },
                                None => PersonEdit { id: crate::book::new_id(), name: name.clone(), emails: addr.clone(), groups: vec![], new: true },
                            });
                        }
                        if in_group.is_some() && !from_mail {
                            resp.context_menu(|ui| {
                                if ui.button("Usuń z grupy").clicked() {
                                    if let Some(g) = in_group.clone() {
                                        self.book.remove_from_group(&g, id);
                                        self.book_changed();
                                    }
                                    ui.close();
                                }
                            });
                        }
                    }
                });
            });
            ui.separator();
            // the person
            ui.vertical(|ui| self.person_editor(ui));
        });
    }

    /// The chosen person: name, addresses, groups; saved to the book (and to Google).
    pub(super) fn person_editor(&mut self, ui: &mut Ui) {
        let groups: Vec<(String, String)> = self.book.groups().iter().map(|g| (g.id.clone(), g.name.clone())).collect();
        let Some(e) = self.book_edit.as_mut() else {
            ui.add_space(30.0);
            ui.label(RichText::new("Wybierz osobę z listy albo dodaj nowy kontakt.").color(MUTED()));
            return;
        };
        let (mut save, mut delete, mut write) = (false, false, false);
        ui.add_space(4.0);
        ui.label(RichText::new(if e.new { "Nowy kontakt" } else { "Kontakt" }).font(semibold(16.0)).color(TEXT()));
        ui.add_space(6.0);
        egui::Grid::new("person").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Imię i nazwisko");
            ui.add(egui::TextEdit::singleline(&mut e.name).desired_width(320.0));
            ui.end_row();
            ui.label("E-mail");
            ui.add(egui::TextEdit::multiline(&mut e.emails).hint_text("adres@firma.pl\n(kilka adresów: każdy w osobnej linii)").desired_rows(2).desired_width(320.0));
            ui.end_row();
            ui.label("Grupy");
            ui.vertical(|ui| {
                if groups.is_empty() {
                    ui.label(RichText::new("Brak grup — utwórz grupę po lewej („+ Nowa grupa”).").small().color(MUTED()));
                }
                for (id, name) in &groups {
                    let mut on = e.groups.contains(id);
                    if ui.checkbox(&mut on, name).changed() {
                        if on {
                            e.groups.push(id.clone());
                        } else {
                            e.groups.retain(|g| g != id);
                        }
                    }
                }
            });
            ui.end_row();
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new(RichText::new(if e.new { "Zapisz w kontaktach" } else { "Zapisz" }).color(Color32::WHITE)).fill(ACCENT())).clicked() {
                save = true;
            }
            if !e.emails.trim().is_empty() && ui.button("✉ Napisz").clicked() {
                write = true;
            }
            if !e.new && ui.button("🗑 Usuń kontakt").clicked() {
                delete = true;
            }
        });
        let e = e.clone();
        let first = e.emails.lines().map(str::trim).find(|l| l.contains('@')).unwrap_or("").to_string();
        if save {
            let emails: Vec<String> = e.emails.lines().map(str::trim).filter(|l| l.contains('@')).map(String::from).collect();
            if emails.is_empty() && e.name.trim().is_empty() {
                self.set_status("Wpisz imię i nazwisko albo adres e-mail");
            } else {
                self.book.put_person(crate::book::Person { id: e.id.clone(), name: e.name.trim().to_string(), emails, groups: e.groups.clone(), ..Default::default() });
                if let Some(x) = self.book_edit.as_mut() {
                    x.new = false;
                }
                self.book_changed();
                self.set_status("Kontakt zapisany");
            }
        }
        if delete {
            self.book.delete_person(&e.id);
            self.book_edit = None;
            self.book_changed();
            self.set_status("Kontakt usunięty");
        }
        if write {
            let to = if e.name.trim().is_empty() { first } else { format!("{} <{}>", e.name.trim(), first) };
            self.module = Module::Mail;
            self.open_composer(to, String::new(), String::new(), String::new(), None, None);
        }
    }
}
