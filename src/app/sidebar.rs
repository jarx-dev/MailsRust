use super::*;

/// The Gmail label that holds favourite messages.
pub(super) const FAVOURITES: &str = "Ulubione";

/// Small square button with a double chevron: "»" folds the calendar away, "«" brings it back.
pub(super) fn fold_button(ui: &mut Ui, fold: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(r, CornerRadius::same(4), HOVER());
    }
    let col = if resp.hovered() { ACCENT() } else { MUTED() };
    let s = Stroke::new(1.5, col);
    let c = r.center();
    let dir = if fold { 1.0 } else { -1.0 };
    for dx in [-3.0f32, 2.0] {
        let tip = c + Vec2::new(dx + 2.0 * dir, 0.0);
        let back = c + Vec2::new(dx - 2.0 * dir, 0.0);
        p.line_segment([Pos2::new(back.x, c.y - 4.5), tip], s);
        p.line_segment([tip, Pos2::new(back.x, c.y + 4.5)], s);
    }
    resp
}

/// "n8n › Wymaga działania": a tag's full path without emoji.
pub(super) fn pretty_label(name: &str) -> String {
    let parts: Vec<String> = name.split('/').map(strip_emoji).filter(|s| !s.is_empty()).collect();
    if parts.is_empty() { name.to_string() } else { parts.join(" › ") }
}

/// A small drawn arrow for folding: pointing right (folded) or down (open).
pub(super) fn chevron(p: &egui::Painter, c: Pos2, open: bool, col: Color32) {
    let s = 3.6;
    let stroke = Stroke::new(1.5, col);
    let pts = if open {
        vec![c + Vec2::new(-s, -s * 0.5), c + Vec2::new(0.0, s * 0.5), c + Vec2::new(s, -s * 0.5)]
    } else {
        vec![c + Vec2::new(-s * 0.5, -s), c + Vec2::new(s * 0.5, 0.0), c + Vec2::new(-s * 0.5, s)]
    };
    p.add(egui::Shape::line(pts, stroke));
}

/// Order of names as people expect: capital letters and Polish letters do not move a name.
pub(super) fn sort_key(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'ą' => "a~".to_string(),
            'ć' => "c~".to_string(),
            'ę' => "e~".to_string(),
            'ł' => "l~".to_string(),
            'ń' => "n~".to_string(),
            'ó' => "o~".to_string(),
            'ś' => "s~".to_string(),
            'ź' => "z~".to_string(),
            'ż' => "z~~".to_string(),
            c => c.to_string(),
        })
        .collect()
}

pub(super) fn draw_folder_icon(p: &egui::Painter, c: Pos2, col: Color32) {
    let s = Stroke::new(1.3, col);
    let pts = vec![
        c + Vec2::new(-6.5, -4.5),
        c + Vec2::new(-2.5, -4.5),
        c + Vec2::new(-1.0, -3.0),
        c + Vec2::new(6.5, -3.0),
        c + Vec2::new(6.5, 5.0),
        c + Vec2::new(-6.5, 5.0),
    ];
    p.add(egui::Shape::closed_line(pts, s));
}

/// A small outlined tag (label) symbol, drawn with lines so it looks clean at any size.
pub(super) fn draw_tag_icon(p: &egui::Painter, c: Pos2, col: Color32) {
    let s = Stroke::new(1.3, col);
    let pts = vec![
        c + Vec2::new(-6.0, -5.0),
        c + Vec2::new(1.5, -5.0),
        c + Vec2::new(6.5, 0.0),
        c + Vec2::new(1.5, 5.0),
        c + Vec2::new(-6.0, 5.0),
    ];
    p.add(egui::Shape::closed_line(pts, s));
    p.circle_filled(c + Vec2::new(1.5, 0.0), 1.4, col);
}

impl MailApp {
    /// Every tag used on any message, sorted.
    pub(super) fn all_tags(&self) -> Vec<String> {
        match &self.counts {
            Some((_, c)) => c.tags.keys().cloned().collect(),
            None => {
                let set: std::collections::BTreeSet<String> = self.store.messages.iter().flat_map(|m| m.labels.iter().cloned()).collect();
                set.into_iter().collect()
            }
        }
    }

    /// One line in the sidebar's tag list. `key` is the full tag name (or "group/" for a
    /// whole group); `level` 1 is indented under its group.
    pub(super) fn tag_row(&mut self, ui: &mut Ui, key: String, shown: String, count: usize, level: usize, dot: bool) {
        let on = self.view == MailView::Label && self.label_filter == key && self.search.is_empty();
        let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::click());
        let p = ui.painter();
        if on {
            p.rect_filled(r, CornerRadius::same(4), SELECTED());
        } else if resp.hovered() {
            p.rect_filled(r, CornerRadius::same(4), HOVER());
        }
        let x = r.left() + 44.0 + 16.0 * level as f32;
        if dot {
            let (_, fg) = tag_colors(&shown);
            p.circle_filled(Pos2::new(x, r.center().y), 4.5, fg);
        } else {
            draw_folder_icon(p, Pos2::new(x, r.center().y), MUTED());
        }
        p.text(Pos2::new(r.right() - 12.0, r.center().y), Align2::RIGHT_CENTER, count.to_string(), FontId::proportional(12.0), MUTED());
        // the name stops before the number on the right
        let count_w = text_width(ui, &count.to_string(), FontId::proportional(12.0));
        line(p, Pos2::new(x + 14.0, r.center().y - 10.0), &shown, 14.0, TEXT(), r.right() - x - 36.0 - count_w);
        if resp.clicked() {
            self.label_filter = key.clone();
            self.view = MailView::Label;
            self.search.clear();
            self.selected = self.visible_ids().first().copied();
        }
        if !key.ends_with('/') {
            resp.context_menu(|ui| {
                if ui.button(format!("Odepnij „{shown}” od wszystkich wiadomości")).clicked() {
                    let ids: Vec<u64> = self.store.messages.iter().filter(|m| m.labels.contains(&key)).map(|m| m.id).collect();
                    for id in ids {
                        self.actions.push(Action::RemoveTag(id, key.clone()));
                    }
                    ui.close();
                }
            });
        }
    }

    /// Menu for choosing a message's tags: tick existing ones, or type a new one.
    pub(super) fn tag_menu(&mut self, ui: &mut Ui, id: u64) {
        let current = self.msg(id).map(|m| m.labels.clone()).unwrap_or_default();
        for t in self.all_tags() {
            let mut on = current.contains(&t);
            let (bg, fg) = tag_colors(&display_label(&t));
            let resp = ui.checkbox(&mut on, RichText::new(pretty_label(&t)).color(fg).background_color(bg));
            if resp.changed() {
                self.actions.push(if on { Action::AddTag(id, t.clone()) } else { Action::RemoveTag(id, t.clone()) });
            }
        }
        ui.separator();
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut self.new_tag).hint_text("Nowy tag…").desired_width(140.0));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if (ui.button("Dodaj").clicked() || enter) && !self.new_tag.trim().is_empty() {
                self.actions.push(Action::AddTag(id, self.new_tag.trim().to_string()));
                self.new_tag.clear();
                ui.close();
            }
        });
    }

    pub(super) fn top_bar(&mut self, ui: &mut Ui) {
        // The bar's empty space works as the window's title bar: drag to move, double-click to maximize.
        // It is registered first, so every button drawn on top of it still gets its own clicks.
        let bar = ui.max_rect().expand2(Vec2::new(8.0, 6.0));
        let drag = ui.interact(bar, id("title-bar"), Sense::click_and_drag());
        if drag.double_clicked() {
            let maximized = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
        } else if drag.drag_started_by(egui::PointerButton::Primary) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.menu_button(RichText::new("☰").size(18.0), |ui| {
                if ui.button("✉  Nowa wiadomość").clicked() {
                    self.actions.push(Action::NewMessage);
                    ui.close();
                }
                if ui.button("📅  Nowe wydarzenie").clicked() {
                    self.editor = Some(EventEditor::new(self.cal_day, None));
                    ui.close();
                }
                ui.separator();
                if ui.button("⟳  Wyślij / odbierz").clicked() {
                    self.actions.push(Action::SendReceive);
                    ui.close();
                }
                if ui.button("👤  Konta…").clicked() {
                    self.account_open = true;
                    ui.close();
                }
                ui.checkbox(&mut self.show_side, "Panel kalendarza po prawej");
                if !self.prefs.ai_shares.is_empty()
                    && ui
                        .button(format!("🔗  Wyłącz linki dla AI ({})", self.prefs.ai_shares.len()))
                        .on_hover_text("Wszystkie udostępnione e-maile przestają być dostępne dla agentów")
                        .clicked()
                {
                    self.actions.push(Action::StopAiSharing);
                    ui.close();
                }
                let mut dark = self.prefs.dark;
                if ui.checkbox(&mut dark, "Ciemny motyw").changed() {
                    self.set_dark(dark);
                }
                if ui.button("➕  Dodaj konto pocztowe…").on_hover_text("Kolejna skrzynka (tylko poczta, logowanie hasłem)").clicked() {
                    self.extra_edit = Some((None, crate::model::Account { imap_port: 993, smtp_port: 465, ..Default::default() }));
                    ui.close();
                }
                if ui.button("✨  Ustawienia AI…").on_hover_text("Poprawianie tekstu i tłumaczenie: CLIProxy, modele, poziom myślenia").clicked() {
                    self.ai_open = true;
                    ui.close();
                }
                if ui.button("📅  Kalendarz i kontakty Google…").on_hover_text("Synchronizacja z Kalendarzem i Kontaktami Google").clicked() {
                    self.gcal_open = true;
                    ui.close();
                }
                if ui
                    .checkbox(&mut self.prefs.notify, "Powiadamiaj o nowych wiadomościach")
                    .on_hover_text("Dźwięk, miganie ikony na pasku zadań i okienko w rogu ekranu")
                    .changed()
                {
                    self.save_now = true;
                }
                let mut autostart = crate::autostart::is_on();
                if ui
                    .checkbox(&mut autostart, "Uruchamiaj razem z Windows")
                    .on_hover_text("Po zalogowaniu do Windows Poczta startuje zminimalizowana i odbiera pocztę w tle")
                    .changed()
                {
                    if let Err(e) = crate::autostart::set(autostart) {
                        self.flash(&format!("Autostart: {e}"));
                    }
                }
                if ui.button("⏱  Diagnostyka synchronizacji").clicked() {
                    self.diag_open = true;
                    ui.close();
                }
                ui.separator();
                if ui.button("Zakończ").on_hover_text("Kończy działanie programu (zamknięcie okna tylko je minimalizuje)").clicked() {
                    self.quit();
                }
                let (version, build) = crate::update::version();
                ui.label(RichText::new(version).size(11.5).color(MUTED()))
                    .on_hover_text(format!("Wersja z datą i godziną zbudowania (rr.mmdd.ggmm): po aktualizacji jest wyższa. Build {build}"));
            });
            ui.add_space(6.0);
            let new_label = if self.module == Module::Calendar { "Nowe" } else { "Nowy" };
            if primary_button(ui, new_label) {
                match self.module {
                    Module::Mail => self.actions.push(Action::NewMessage),
                    Module::Calendar => self.editor = Some(EventEditor::new(self.cal_day, None)),
                    Module::Contacts => self.book_new_person(),
                }
            }
            ui.add_space(8.0);
            let compact = ui.available_width() < 1060.0;
            let l = |s: &'static str| if compact { "" } else { s };
            match self.module {
                Module::Mail => {
                    let sel = self.selected.filter(|id| self.msg(*id).is_some());
                    let has = sel.is_some();
                    if tool_button(ui, "⟳", l("Odśwież"), true) {
                        self.actions.push(Action::SendReceive);
                    }
                    if tool_button(ui, "↶", l("Odpowiedz"), has) {
                        self.actions.push(Action::Reply(sel.unwrap(), false));
                    }
                    if tool_button(ui, "⮪", l("Odpowiedz wszystkim"), has) {
                        self.actions.push(Action::Reply(sel.unwrap(), true));
                    }
                    if tool_button(ui, "➦", l("Prześlij dalej"), has) {
                        self.actions.push(Action::Forward(sel.unwrap()));
                    }
                    let sel_read = sel.and_then(|id| self.msg(id)).map(|m| m.read).unwrap_or(true);
                    if tool_button(ui, if sel_read { "✉" } else { "✔" }, l(if sel_read { "Nieprzeczytana" } else { "Przeczytana" }), has) {
                        self.actions.push(Action::SetRead(sel.unwrap(), !sel_read));
                    }
                    if tool_button(ui, "⚐", l("Oznacz"), has) {
                        self.actions.push(Action::ToggleFlag(sel.unwrap()));
                    }
                    if tool_button(ui, "🗃", l("Archiwum"), has) {
                        self.actions.push(Action::Move(sel.unwrap(), Folder::Archive));
                    }
                    if tool_button(ui, "🗑", l("Usuń"), has) {
                        self.actions.push(Action::Delete(sel.unwrap()));
                    }
                }
                Module::Contacts => {}
                Module::Calendar => {
                    if tool_button(ui, "◎", "Dziś", true) {
                        self.cal_day = Local::now().date_naive();
                        self.mini_shown = self.cal_day;
                    }
                    if tool_button(ui, "‹", "", true) {
                        self.cal_day = calendar::shift(self.cal_day, self.cal_view, -1);
                        self.mini_shown = self.cal_day;
                    }
                    if tool_button(ui, "›", "", true) {
                        self.cal_day = calendar::shift(self.cal_day, self.cal_view, 1);
                        self.mini_shown = self.cal_day;
                    }
                    tool_sep(ui);
                    if tool_button(ui, "▦", "Miesiąc", true) {
                        self.cal_view = CalView::Month;
                    }
                    if tool_button(ui, "▥", "Tydzień", true) {
                        self.cal_view = CalView::Week;
                    }
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let maximized = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
                if window_button(ui, WinButton::Close).on_hover_text("Zamknij (program działa dalej w zasobniku)").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
                let max_kind = if maximized { WinButton::Restore } else { WinButton::Maximize };
                if window_button(ui, max_kind)
                    .on_hover_text(if maximized { "Przywróć" } else { "Maksymalizuj" })
                    .clicked()
                {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                }
                if window_button(ui, WinButton::Minimize).on_hover_text("Minimalizuj").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
                ui.add_space(6.0);
                // the theme switch: a moon in the light theme, a sun in the dark one
                let (icon, tip) = if self.prefs.dark { ("☀", "Jasny motyw") } else { ("🌙", "Ciemny motyw") };
                let (tr, tresp) = ui.allocate_exact_size(Vec2::new(32.0, 30.0), Sense::click());
                if tresp.hovered() {
                    ui.painter().rect_filled(tr, CornerRadius::same(4), HOVER());
                }
                ui.painter().text(tr.center(), Align2::CENTER_CENTER, icon, FontId::proportional(16.0), ICON());
                if tresp.on_hover_text(tip).clicked() {
                    let on = !self.prefs.dark;
                    self.set_dark(on);
                }
                ui.add_space(6.0);
                ui.spacing_mut().item_spacing.x = 6.0;
                if self.module == Module::Mail {
                    let w = ui.available_width().clamp(160.0, 360.0);
                    let was_empty = self.search.trim().is_empty();
                    let hint = if !self.store.extra.is_empty() {
                        let a = self.account_ref(self.view_account);
                        format!("🔍  Szukaj w: {}", if a.display_name.trim().is_empty() { a.email.trim() } else { a.display_name.trim() })
                    } else if self.finder.meaning_ready() {
                        "🔍  Szukaj — słowa lub znaczenie".to_string()
                    } else {
                        "🔍  Szukaj".to_string()
                    };
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text(RichText::new(hint).italics())
                            .desired_width(w)
                            .margin(Margin::symmetric(10, 5)),
                    );
                    if r.changed() {
                        if was_empty {
                            // a new search starts where the person is: in sent mail, received, or everywhere
                            self.scope = match self.view {
                                MailView::Folder(Folder::Sent) => Scope::Sent,
                                MailView::Folder(Folder::Inbox) => Scope::Received,
                                _ => Scope::All,
                            };
                        }
                        let ids = self.visible_ids();
                        if !self.selected.map(|s| ids.contains(&s)).unwrap_or(false) {
                            self.selected = ids.first().copied();
                        }
                    }
                }
            });
        });
    }

    // ------------------------------------------------------------ left side bar

    pub(super) fn nav_bar(&mut self, ui: &mut Ui) {
        egui::Panel::bottom("modules")
            .frame(Frame::new().fill(BG()))
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.painter().hline(ui.max_rect().x_range(), ui.max_rect().top(), Stroke::new(1.0, BORDER()));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let unread = self.counts().inbox_unread_all;
                    for (m, icon, tip) in [(Module::Mail, "✉", "Poczta"), (Module::Calendar, "📅", "Kalendarz"), (Module::Contacts, "👤", "Kontakty")] {
                        let tip = if m == Module::Mail && unread > 0 { format!("{tip} — {}", crate::notify::unread_text(unread)) } else { tip.to_string() };
                        let resp = icon_tab(ui, icon, self.module == m, 64.0);
                        if m == Module::Mail && unread > 0 {
                            count_badge(ui.painter(), resp.rect.center() + Vec2::new(9.0, -9.0), unread);
                        }
                        if resp.on_hover_text(tip).clicked() {
                            self.module = m;
                        }
                    }
                });
            });
        // What is being prepared in the background: the offline copy and search by meaning.
        let mut notes: Vec<String> = Vec::new();
        if self.store.has_real_mail && self.preview.is_none() && self.store.sync.map(|s| !s.complete).unwrap_or(false) {
            let n = self.counts().folder_count.get(&Folder::Inbox).copied().unwrap_or(0);
            notes.push(format!("Pobieranie całej skrzynki… ({n} wiadomości offline)"));
        }
        if let Some(s) = self.finder.status() {
            notes.push(s);
        }
        if !notes.is_empty() {
            let failed = matches!(self.finder.meaning, crate::finder::MeaningState::Failed(_));
            egui::Panel::bottom("background-work")
                .frame(Frame::new().fill(BG()).inner_margin(Margin { left: 10, right: 6, top: 6, bottom: 6 }))
                .show_separator_line(false)
                .show(ui, |ui| {
                    // a still sign instead of a spinning one: a spinner redraws the whole window
                    // 60 times a second for as long as it shows; the note updates once a second
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(1000));
                    for n in &notes {
                        ui.horizontal(|ui| {
                            if !failed {
                                ui.label(RichText::new("⟳").size(12.0).color(MUTED()));
                            }
                            ui.add(egui::Label::new(RichText::new(n).size(11.5).color(MUTED())).wrap());
                        });
                    }
                    if failed && ui.link(RichText::new("Spróbuj ponownie").size(11.5)).clicked() {
                        self.finder.retry();
                    }
                });
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.module {
            Module::Mail => self.folder_list(ui),
            Module::Calendar => self.calendar_side(ui),
            Module::Contacts => self.contacts_side(ui),
        });
    }

    pub(super) fn folder_list(&mut self, ui: &mut Ui) {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(RichText::new("Poczta").font(semibold(18.0)).color(TEXT()));
        });
        ui.add_space(6.0);
        let counts = self.counts();
        let (unread_all, flagged) = (counts.unread_all, counts.flagged);
        for (v, icon, n) in [(MailView::Unread, "◷", unread_all), (MailView::Flagged, "⚐", flagged)] {
            let row = folder_row_r(ui, icon, MUTED(), v.title(), n, self.view == v && self.search.is_empty(), 28.0);
            if v == MailView::Unread && mark_all_menu(&row, n) {
                self.actions.push(Action::MarkAllRead(None, None));
            }
            if row.clicked() {
                self.view = v;
                if v == MailView::Flagged {
                    self.view_account = 0;
                }
                self.search.clear();
                // (a read message open in another folder doesn't come along into "Nieprzeczytane")
                self.selected = None;
                self.selected = self.visible_ids().first().copied();
            }
        }
        // waiting for your reply (last 7 days): counted again only when something changed
        let state = self.msg_sig ^ self.answered.0.rotate_left(17) ^ Local::now().date_naive().num_days_from_ce() as u64 ^ (self.prefs.no_reply_needed.len() as u64).rotate_left(40);
        if self.noreply_count.0 != state {
            let n = self.store.messages.iter().filter(|m| self.waits_for_reply(m)).count();
            self.noreply_count = (state, n);
        }
        let waiting = self.noreply_count.1;
        if folder_row(ui, "↩", MUTED(), MailView::NoReply.title(), waiting, self.view == MailView::NoReply && self.search.is_empty(), 28.0) {
            self.view = MailView::NoReply;
            self.view_account = 0;
            self.search.clear();
            self.selected = self.visible_ids().first().copied();
        }
        let favs = counts.tags.get(FAVOURITES).copied().unwrap_or(0);
        let on = self.view == MailView::Label && self.label_filter == FAVOURITES && self.search.is_empty();
        if folder_row(ui, "☆", Color32::from_rgb(0xE0, 0xA1, 0x00), FAVOURITES, favs, on, 28.0) {
            self.label_filter = FAVOURITES.to_string();
            self.view = MailView::Label;
            self.search.clear();
            self.selected = self.visible_ids().first().copied();
        }
        // Tags (Gmail labels), with how many messages carry each (favourites have their own row).
        let mut tags = counts.tags.clone();
        tags.remove(FAVOURITES);
        if !tags.is_empty() {
            let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(r, CornerRadius::same(4), HOVER());
            }
            let p = ui.painter();
            chevron(p, Pos2::new(r.left() + 14.0, r.center().y), self.tags_open, MUTED());
            draw_tag_icon(p, Pos2::new(r.left() + 33.0, r.center().y), MUTED());
            let tr = p.text(Pos2::new(r.left() + 44.0, r.center().y), Align2::LEFT_CENTER, "Tagi", FontId::proportional(14.0), TEXT());
            if !self.store.extra.is_empty() {
                // several accounts: whose tags these are
                let a = self.account_ref(self.view_account);
                let who = if a.display_name.trim().is_empty() { a.email.trim() } else { a.display_name.trim() };
                let room = r.right() - tr.right() - 16.0;
                line(p, Pos2::new(tr.right() + 6.0, r.center().y - 8.0), &format!("· {}", who), 12.5, MUTED(), room);
            }
            if resp.clicked() {
                self.tags_open = !self.tags_open;
                self.save_now = true;
            }
            if self.tags_open {
                // Nested tags ("n8n/Support") are grouped under their parent ("n8n").
                let mut top: std::collections::BTreeMap<String, (Option<usize>, Vec<(String, usize)>)> = Default::default();
                for (name, n) in tags {
                    match name.split_once('/') {
                        Some((parent, _)) => top.entry(parent.to_string()).or_default().1.push((name, n)),
                        None => top.entry(name.clone()).or_default().0 = Some(n),
                    }
                }
                // groups (folders of tags) first, then single tags; each alphabetically, without
                // minding capital letters or Polish letters
                let mut top: Vec<(String, (Option<usize>, Vec<(String, usize)>))> = top.into_iter().collect();
                top.sort_by_cached_key(|(name, (_, children))| (children.is_empty(), sort_key(&display_label(name))));
                for (_, (_, children)) in top.iter_mut() {
                    children.sort_by_cached_key(|(name, _)| sort_key(&display_label(name)));
                }
                for (parent, (own, children)) in top {
                    if children.is_empty() {
                        self.tag_row(ui, parent.clone(), display_label(&parent), own.unwrap_or(0), 0, true);
                        continue;
                    }
                    // the group's own row shows every message with any tag inside it
                    let total = counts.group_total.get(&parent).copied().unwrap_or(0);
                    let folded = self.prefs.folded_groups.contains(&parent);
                    let row_top = ui.cursor().top();
                    self.tag_row(ui, format!("{parent}/"), display_label(&parent), total, 0, false);
                    // a small arrow in front of the group folds or unfolds its tags
                    let arrow = Rect::from_center_size(Pos2::new(ui.max_rect().left() + 28.0, row_top + 14.0), Vec2::splat(20.0));
                    let ar = ui.interact(arrow, id(("fold-group", parent.as_str())), Sense::click());
                    if ar.hovered() {
                        ui.painter().rect_filled(arrow, CornerRadius::same(4), HOVER());
                    }
                    chevron(ui.painter(), arrow.center(), !folded, if ar.hovered() { ACCENT() } else { MUTED() });
                    if ar.on_hover_text(if folded { "Rozwiń" } else { "Zwiń" }).clicked() {
                        if folded {
                            self.prefs.folded_groups.retain(|g| *g != parent);
                        } else {
                            self.prefs.folded_groups.push(parent.clone());
                        }
                        self.save_now = true;
                    }
                    if folded {
                        continue;
                    }
                    for (name, n) in children {
                        let shown = display_label(&name);
                        self.tag_row(ui, name, shown, n, 1, true);
                    }
                }
            }
        }
        ui.add_space(8.0);
        // Account group
        let acc_name = self.store.account.display_name.clone();
        let online = self.can_connect();
        let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 32.0), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(r, CornerRadius::same(4), HOVER());
        }
        let p = ui.painter();
        chevron(p, Pos2::new(r.left() + 14.0, r.center().y), self.account_expanded, MUTED());
        avatar(p, Pos2::new(r.left() + 36.0, r.center().y), 11.0, &acc_name);
        let w = line(p, Pos2::new(r.left() + 54.0, r.center().y - 9.0), &acc_name, 13.5, TEXT(), r.width() - 80.0);
        p.circle_filled(
            Pos2::new(r.left() + 62.0 + w, r.center().y),
            3.5,
            if online { Color32::from_rgb(0x2E, 0x9E, 0x5B) } else { BORDER() },
        );
        if resp.on_hover_text(if online { "Połączono" } else { "Offline — kliknij ⚙, aby się zalogować" }).clicked() {
            self.account_expanded = !self.account_expanded;
            self.save_now = true;
        }
        let gear = Rect::from_center_size(Pos2::new(r.right() - 16.0, r.center().y), Vec2::splat(24.0));
        let gr = ui.interact(gear, id("acc-gear"), Sense::click());
        ui.painter().text(gear.center(), Align2::CENTER_CENTER, "⚙", FontId::proportional(14.0), if gr.hovered() { ACCENT() } else { MUTED() });
        if gr.on_hover_text("Ustawienia konta").clicked() {
            self.account_open = true;
        }
        if self.account_expanded {
            for f in Folder::ALL {
                let n = if f == Folder::Drafts || f == Folder::Outbox { &counts.folder_count } else { &counts.folder_unread }.get(&f).copied().unwrap_or(0);
                let col = match f {
                    Folder::Inbox => ACCENT(),
                    Folder::Junk => Color32::from_rgb(0xD9, 0x6B, 0x2B),
                    _ => MUTED(),
                };
                let row = folder_row_r(ui, f.icon(), col, f.name(), n, self.view == MailView::Folder(f) && self.view_account == 0 && self.search.is_empty(), 44.0);
                let unread = counts.folder_unread.get(&f).copied().unwrap_or(0);
                if mark_all_menu(&row, unread) {
                    self.actions.push(Action::MarkAllRead(Some(0), Some(f)));
                }
                if row.clicked() {
                    self.view = MailView::Folder(f);
                    self.view_account = 0;
                    self.search.clear();
                    self.selected = self.visible_ids().first().copied();
                    if let Some(id) = self.selected {
                        // shown, but it stays unread until the person opens it
                        self.actions.push(Action::Show(id));
                    }
                }
            }
        }
        // further accounts: their own folders (their unread mail is also in "Nieprzeczytane")
        for k in 0..self.store.extra.len() {
            let acct = k as u32 + 1;
            let x = self.store.extra[k].account.clone();
            ui.add_space(4.0);
            let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 32.0), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(r, CornerRadius::same(4), HOVER());
            }
            let open = !self.extra_folded.contains(&k);
            let shown = if x.display_name.trim().is_empty() { x.email.clone() } else { x.display_name.clone() };
            let p = ui.painter();
            chevron(p, Pos2::new(r.left() + 14.0, r.center().y), open, MUTED());
            avatar(p, Pos2::new(r.left() + 36.0, r.center().y), 11.0, &shown);
            let w = line(p, Pos2::new(r.left() + 54.0, r.center().y - 9.0), &shown, 13.5, TEXT(), r.width() - 80.0);
            let online = x.can_connect();
            p.circle_filled(Pos2::new(r.left() + 62.0 + w, r.center().y), 3.5, if online { Color32::from_rgb(0x2E, 0x9E, 0x5B) } else { BORDER() });
            if resp.on_hover_text(&x.email).clicked() {
                if open {
                    self.extra_folded.insert(k);
                } else {
                    self.extra_folded.remove(&k);
                }
            }
            let gear = Rect::from_center_size(Pos2::new(r.right() - 16.0, r.center().y), Vec2::splat(24.0));
            let gr = ui.interact(gear, id(("extra-gear", k)), Sense::click());
            ui.painter().text(gear.center(), Align2::CENTER_CENTER, "⚙", FontId::proportional(14.0), if gr.hovered() { ACCENT() } else { MUTED() });
            if gr.on_hover_text("Ustawienia tego konta").clicked() {
                self.extra_edit = Some((Some(k), x.clone()));
            }
            if open {
                for f in [Folder::Inbox, Folder::Drafts, Folder::Outbox, Folder::Sent, Folder::Junk, Folder::Trash] {
                    let n = if f == Folder::Drafts || f == Folder::Outbox { &counts.acct_count } else { &counts.acct_unread }.get(&(acct, f)).copied().unwrap_or(0);
                    let col = match f {
                        Folder::Inbox => ACCENT(),
                        Folder::Junk => Color32::from_rgb(0xD9, 0x6B, 0x2B),
                        _ => MUTED(),
                    };
                    let on = self.view == MailView::Folder(f) && self.view_account == acct && self.search.is_empty();
                    let row = folder_row_r(ui, f.icon(), col, f.name(), n, on, 44.0);
                    let unread = counts.acct_unread.get(&(acct, f)).copied().unwrap_or(0);
                    if mark_all_menu(&row, unread) {
                        self.actions.push(Action::MarkAllRead(Some(acct), Some(f)));
                    }
                    if row.clicked() {
                        self.view = MailView::Folder(f);
                        self.view_account = acct;
                        self.search.clear();
                        self.selected = self.visible_ids().first().copied();
                        if let Some(id) = self.selected {
                            self.actions.push(Action::Show(id));
                        }
                    }
                }
            }
        }
    }

    pub(super) fn calendar_side(&mut self, ui: &mut Ui) {
        let today = Local::now().date_naive();
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(RichText::new("Kalendarz").font(semibold(18.0)).color(TEXT()));
        });
        ui.add_space(6.0);
        Frame::new().inner_margin(Margin::symmetric(12, 0)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(pl::month_year(self.mini_shown)).color(TEXT()));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(egui::Button::new("›").frame(false)).clicked() {
                        self.mini_shown = calendar::shift(self.mini_shown, CalView::Month, 1);
                    }
                    if ui.add(egui::Button::new("‹").frame(false)).clicked() {
                        self.mini_shown = calendar::shift(self.mini_shown, CalView::Month, -1);
                    }
                });
            });
            if let Some(d) = calendar::mini_month(ui, "left", &self.store, self.mini_shown, self.cal_day, today) {
                self.cal_day = d;
            }
            ui.add_space(12.0);
            for (i, n) in ["Osobiste", "Praca", "Zdrowie", "Spotkania"].iter().enumerate() {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                    ui.painter().rect_stroke(r.shrink(1.0), CornerRadius::same(3), Stroke::new(1.5, EVENT_COLORS[i]), egui::StrokeKind::Inside);
                    ui.label(*n);
                });
            }
        });
    }

    // ------------------------------------------------------------ right side bar

    pub(super) fn side_bar(&mut self, ui: &mut Ui) {
        let today = Local::now().date_naive();
        egui::Panel::bottom("side-tabs")
            .frame(Frame::new().fill(BG()))
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.painter().hline(ui.max_rect().x_range(), ui.max_rect().top(), Stroke::new(1.0, BORDER()));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let w = ui.available_width() / 2.0;
                    if icon_tab(ui, "📅", self.side_mode == SideMode::Day, w).on_hover_text("Dzień").clicked() {
                        self.side_mode = SideMode::Day;
                    }
                    if icon_tab(ui, "☰", self.side_mode == SideMode::Agenda, w).on_hover_text("Nadchodzące").clicked() {
                        self.side_mode = SideMode::Agenda;
                    }
                });
            });
        Frame::new().inner_margin(Margin { left: 14, right: 10, top: 10, bottom: 0 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Kalendarz").font(semibold(18.0)).color(TEXT()));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if fold_button(ui, true).on_hover_text("Zwiń kalendarz na bok").clicked() {
                        self.show_side = false;
                        self.save_now = true;
                    }
                });
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                if ui.add(egui::Button::new(RichText::new("‹").size(18.0)).frame(false)).clicked() {
                    self.side_day -= Duration::days(1);
                }
                if ui.add(egui::Button::new(RichText::new("›").size(18.0)).frame(false)).clicked() {
                    self.side_day += Duration::days(1);
                }
                ui.add_space(6.0);
                let label = if self.side_mode == SideMode::Day {
                    format!("{}  ⌄", pl::day_month(self.side_day))
                } else {
                    format!("od {}  ⌄", pl::day_month(self.side_day))
                };
                if ui.add(egui::Button::new(RichText::new(label).font(semibold(13.5))).frame(false)).clicked() {
                    self.side_picker = !self.side_picker;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(egui::Button::new(RichText::new("+").size(20.0)).frame(false))
                        .on_hover_text("Nowe wydarzenie")
                        .clicked()
                    {
                        self.editor = Some(EventEditor::new(self.side_day, None));
                    }
                });
            });
            if self.side_picker {
                if let Some(d) = calendar::mini_month(ui, "right", &self.store, self.side_day, self.side_day, today) {
                    self.side_day = d;
                    self.side_picker = false;
                }
            }
        });
        match self.side_mode {
            SideMode::Day => {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.label(RichText::new("Cały dzień").small().color(MUTED()));
                });
                if let Some(a) = calendar::all_day_row(ui, &self.store, self.side_day, 1, 12.0) {
                    self.cal_action(a);
                }
                let r = ui.available_rect_before_wrap();
                ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, BORDER()));
                if let Some(a) = calendar::day_timeline(ui, &self.store, self.side_day, today) {
                    self.cal_action(a);
                }
            }
            SideMode::Agenda => {
                egui::ScrollArea::vertical().id_salt("agenda").auto_shrink([false, false]).show(ui, |ui| {
                    Frame::new().inner_margin(Margin::symmetric(12, 4)).show(ui, |ui| {
                        let mut shown = 0;
                        for k in 0..30 {
                            let d = self.side_day + Duration::days(k);
                            let evs: Vec<Event> = calendar::events_on(&self.store, d).into_iter().cloned().collect();
                            if evs.is_empty() {
                                continue;
                            }
                            let label = if d == today {
                                "Dzisiaj".to_string()
                            } else if d == today + Duration::days(1) {
                                "Jutro".to_string()
                            } else {
                                format!("{}, {}", pl::wd_full(d), pl::day_month(d))
                            };
                            ui.add_space(6.0);
                            ui.label(RichText::new(label).small().color(MUTED()));
                            for e in evs {
                                let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::click());
                                let p = ui.painter();
                                if resp.hovered() {
                                    p.rect_filled(r, CornerRadius::same(4), HOVER());
                                }
                                let c = EVENT_COLORS[e.color % EVENT_COLORS.len()];
                                p.circle_filled(Pos2::new(r.left() + 6.0, r.center().y), 3.5, c);
                                let t = if e.all_day { format!("cały dzień  {}", e.title) } else { format!("{}  {}", e.start.format("%H:%M"), e.title) };
                                line(p, Pos2::new(r.left() + 16.0, r.center().y - 9.0), &t, 13.0, TEXT(), r.width() - 20.0);
                                if resp.clicked() {
                                    self.cal_action(CalAction::Edit(e.id));
                                }
                                shown += 1;
                            }
                        }
                        if shown == 0 {
                            ui.add_space(10.0);
                            ui.label(RichText::new("Brak zaplanowanych wydarzeń").color(MUTED()));
                        }
                    });
                });
            }
        }
    }

    pub(super) fn cal_action(&mut self, a: CalAction) {
        match a {
            CalAction::Select(d) => {
                self.cal_day = d;
                self.mini_shown = d;
            }
            CalAction::New(d, t) => {
                self.cal_day = d;
                self.editor = Some(EventEditor::new(d, t));
            }
            CalAction::Edit(id) => {
                if let Some(e) = self.store.events.iter().find(|e| e.id == id) {
                    if e.google && e.gid.is_empty() {
                        let when = if e.all_day { "cały dzień".to_string() } else { format!("{} – {}", e.start.format("%H:%M"), (e.start + Duration::minutes(e.minutes)).format("%H:%M")) };
                        let place = if e.location.is_empty() { String::new() } else { format!(" · {}", e.location) };
                        self.set_status(format!("{} ({when}{place}) — z Kalendarza Google (powtarzające się, całodniowe albo z innego kalendarza zmienisz w Google)", e.title));
                    } else {
                        self.editor = Some(EventEditor::from_event(e));
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------ mail panes

    pub(super) fn editor_ui(&mut self, ctx: &egui::Context) {
        let google_cal = self.google_calendar();
        let Some(ed) = self.editor.as_mut() else { return };
        match calendar::event_editor(ctx, ed, &self.store) {
            EditorResult::Keep => {}
            EditorResult::Close => self.editor = None,
            EditorResult::Delete => {
                if let Some(id) = ed.id {
                    let gid = self.store.events.iter().find(|e| e.id == id).map(|e| e.gid.clone()).unwrap_or_default();
                    self.store.events.retain(|e| e.id != id);
                    if !gid.is_empty() {
                        self.gcal_write(None, gid, None);
                    }
                }
                self.editor = None;
                self.set_status("Wydarzenie usunięte");
            }
            EditorResult::Save => {
                let Some(start) = calendar::parse_time(&ed.start) else {
                    ed.error = Some("Godzina powinna wyglądać tak: 14:30".into());
                    return;
                };
                if ed.title.trim().is_empty() {
                    ed.error = Some("Nadaj wydarzeniu tytuł".into());
                    return;
                }
                let ev = Event {
                    id: 0,
                    title: ed.title.trim().to_string(),
                    date: ed.date,
                    start,
                    minutes: ed.minutes,
                    location: ed.location.clone(),
                    color: ed.color,
                    google: false,
                    all_day: false,
                    gid: String::new(),
                };
                let date = ed.date;
                let to_google = google_cal;
                match ed.id {
                    Some(id) => {
                        let mut write: Option<(String, Event)> = None;
                        if let Some(e) = self.store.events.iter_mut().find(|e| e.id == id) {
                            let (google, gid) = (e.google, e.gid.clone());
                            *e = Event { id, google, gid: gid.clone(), ..ev };
                            if !gid.is_empty() {
                                write = Some((gid, e.clone()));
                            }
                        }
                        if let Some((gid, e)) = write {
                            self.gcal_write(None, gid, Some(e));
                        }
                    }
                    None => {
                        let id = self.store.new_id();
                        let e = Event { id, ..ev };
                        self.store.events.push(e.clone());
                        // with the calendar script, new events go to Google (the copy here is
                        // replaced by Google's own after the next reading)
                        if to_google {
                            self.gcal_write(Some(id), String::new(), Some(e));
                        }
                    }
                }
                self.cal_day = date;
                self.side_day = date;
                self.editor = None;
                self.set_status("Wydarzenie zapisane");
            }
        }
    }
}

/// One folder line: icon, name, count on the right.
pub(super) fn folder_row(ui: &mut Ui, icon: &str, icon_col: Color32, name: &str, count: usize, on: bool, indent: f32) -> bool {
    folder_row_r(ui, icon, icon_col, name, count, on, indent).clicked()
}

/// A row of the left bar with a right-click menu: "Oznacz wszystkie jako przeczytane" (when
/// something there is unread); true when that was chosen.
pub(super) fn mark_all_menu(resp: &egui::Response, unread: usize) -> bool {
    let mut chosen = false;
    resp.context_menu(|ui| {
        if ui.add_enabled(unread > 0, egui::Button::new("✓  Oznacz wszystkie jako przeczytane")).clicked() {
            chosen = true;
            ui.close();
        }
    });
    chosen
}

pub(super) fn folder_row_r(ui: &mut Ui, icon: &str, icon_col: Color32, name: &str, count: usize, on: bool, indent: f32) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::click());
    let p = ui.painter();
    if on {
        p.rect_filled(r, CornerRadius::same(4), SELECTED());
    } else if resp.hovered() {
        p.rect_filled(r, CornerRadius::same(4), HOVER());
    }
    p.text(Pos2::new(r.left() + indent, r.center().y), Align2::CENTER_CENTER, icon, FontId::proportional(15.0), icon_col);
    let count_txt = if count > 0 { count.to_string() } else { String::new() };
    p.text(Pos2::new(r.right() - 12.0, r.center().y), Align2::RIGHT_CENTER, &count_txt, semibold(12.5), ACCENT());
    line(p, Pos2::new(r.left() + indent + 16.0, r.center().y - 10.0), name, 14.0, TEXT(), r.width() - indent - 56.0);
    resp
}

/// Icon-only tab with a blue underline when active (bottom module bars).
pub(super) fn icon_tab(ui: &mut Ui, icon: &str, on: bool, w: f32) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(w, 46.0), Sense::click());
    let p = ui.painter();
    if resp.hovered() && !on {
        p.rect_filled(r, CornerRadius::ZERO, HOVER());
    }
    p.text(r.center(), Align2::CENTER_CENTER, icon, FontId::proportional(20.0), if on { ACCENT() } else { MUTED() });
    if on {
        p.hline(r.x_range(), r.bottom() - 1.5, Stroke::new(3.0, ACCENT()));
    }
    resp
}

/// A small red number (unread messages) at `at`.
pub(super) fn count_badge(p: &egui::Painter, at: Pos2, n: usize) {
    let text = crate::notify::label(n);
    let font = FontId::proportional(10.0);
    let galley = p.layout_no_wrap(text, font, Color32::WHITE);
    let w = (galley.size().x + 8.0).max(16.0);
    let r = Rect::from_center_size(at + Vec2::new(w / 2.0 - 8.0, 0.0), Vec2::new(w, 16.0));
    p.rect_filled(r.expand(1.5), CornerRadius::same(10), BG());
    p.rect_filled(r, CornerRadius::same(8), Color32::from_rgb(0xE5, 0x39, 0x35));
    p.galley(r.center() - galley.size() / 2.0, galley, Color32::WHITE);
}
