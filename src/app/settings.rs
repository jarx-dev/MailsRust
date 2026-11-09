use super::*;

impl MailApp {
    /// "Copy link for AI": shares the email on this computer's Tailscale address and copies a
    /// short note with the link. Without Tailscale, the email's text is copied instead.
    pub(super) fn copy_for_ai(&mut self, id: u64) {
        let Some(m) = self.msg(id).cloned() else { return };
        let port = self.prefs.ai_port;
        let started = crate::share::tailscale_ip()
            .ok_or_else(|| "Tailscale nie jest włączony na tym komputerze — włącz go i spróbuj ponownie".to_string())
            .and_then(|ip| self.hub.start(ip, port));
        match started {
            Err(e) => {
                // no link without Tailscale: say why, rather than copying something else
                crate::diag::log(format!("copy for AI: {e}"));
                self.set_status(format!("Nie udało się utworzyć linku: {e}"));
                return;
            }
            Ok(addr) if addr.port() != port => {
                self.prefs.ai_port = addr.port();
                self.save_now = true;
            }
            Ok(_) => {}
        }
        let now = Local::now().timestamp();
        // the same email keeps its link while it is valid
        let token = match self.prefs.ai_shares.iter().find(|s| s.1 == id && now - s.2 < crate::share::DAYS * 86400) {
            Some(s) => s.0.clone(),
            None => {
                let t = crate::share::new_token();
                self.prefs.ai_shares.push((t.clone(), id, now));
                self.save_now = true;
                t
            }
        };
        self.hub.set_account(&self.store.account);
        let shared = crate::share::Shared::of(&m);
        if !m.partial {
            self.hub.warm(&shared);
        }
        self.hub.put(&token, shared);
        // the link is the whole email as a .eml file (with attachments and pictures)
        let Some(url) = self.hub.url(&token).map(|u| format!("{u}.eml")) else { return };
        // just the one link: the page behind it lists everything else (attachments, pictures,
        // formatted version, how to leave a reply draft)
        self.ctx.copy_text(url);
        self.flash("Link dla AI skopiowany do schowka — wklej go agentowi (działa w Twojej sieci Tailscale przez 7 dni)");
        // a big message with only its header here: get the rest, the shared copy follows
        if m.partial && matches!(m.folder, Folder::Inbox | Folder::Sent) {
            if let Some(uid) = m.uid {
                if self.store.account.can_connect() && self.fetching_full.insert((m.folder, uid)) {
                    net::fetch_full(self.account_for(m.account), m.account, uid, m.folder, self.tx.clone(), self.ctx.clone());
                }
            }
        }
    }

    /// Links shared before (and not expired) work again after the app restarts.
    pub(super) fn restore_ai_shares(&mut self) {
        let now = Local::now().timestamp();
        let ids: std::collections::HashSet<u64> = self.store.messages.iter().map(|m| m.id).collect();
        self.prefs.ai_shares.retain(|s| now - s.2 < crate::share::DAYS * 86400 && ids.contains(&s.1));
        if self.prefs.ai_shares.is_empty() || self.shot.is_some() {
            return;
        }
        let Some(ip) = crate::share::tailscale_ip() else { return };
        match self.hub.start(ip, self.prefs.ai_port) {
            Ok(addr) if addr.port() != self.prefs.ai_port => {
                // another program took the old port: the old links can't work any more
                self.prefs.ai_port = addr.port();
                self.prefs.ai_shares.clear();
                self.save_now = true;
                return;
            }
            Ok(_) => {}
            Err(_) => return,
        }
        self.hub.set_account(&self.store.account);
        for (token, id, _) in self.prefs.ai_shares.clone() {
            if let Some(m) = self.msg(id) {
                self.hub.put(&token, crate::share::Shared::of(m));
            }
        }
    }

    /// Fills in the Google client from a built-in setup or a file downloaded from Google Cloud.
    pub(super) fn find_google_client(&mut self) -> bool {
        if let Some((id, secret)) = crate::oauth::builtin_client().or_else(crate::oauth::find_downloaded_client) {
            self.store.account.oauth_client_id = id;
            self.store.account.oauth_client_secret = secret;
            self.set_status("Znaleziono dane aplikacji z Google");
            true
        } else {
            false
        }
    }

    /// What the sync did, step by step with timings, plus a read-only speed test.
    pub(super) fn diag_ui(&mut self, ctx: &egui::Context) {
        if !self.diag_open {
            return;
        }
        let mut open = true;
        let lines = crate::diag::lines();
        let testing = self.diag_testing.load(std::sync::atomic::Ordering::Relaxed);
        crate::theme::dialog(ctx, "diag", "Diagnostyka synchronizacji", 760.0, &mut open, |ui| {
                ui.horizontal(|ui| {
                    let can = self.store.account.can_connect() && !testing;
                    if ui
                        .add_enabled(can, egui::Button::new(RichText::new("  Test szybkości  ").color(Color32::WHITE)).fill(PRIMARY))
                        .on_hover_text("Pobiera 200 najnowszych wiadomości tylko do odczytu i mierzy czas każdego kroku")
                        .clicked()
                    {
                        let acc = self.store.account.clone();
                        let flag = self.diag_testing.clone();
                        let ctx = ctx.clone();
                        flag.store(true, std::sync::atomic::Ordering::Relaxed);
                        std::thread::spawn(move || {
                            let r = std::panic::catch_unwind(|| crate::net::benchmark(&acc, 200))
                                .unwrap_or_else(|_| Err("nieoczekiwany błąd".into()));
                            if let Err(e) = r {
                                crate::diag::log(format!("speed test failed: {e}"));
                            }
                            flag.store(false, std::sync::atomic::Ordering::Relaxed);
                            ctx.request_repaint();
                        });
                    }
                    if testing {
                        ui.spinner();
                        ui.label(RichText::new("trwa test…").color(MUTED()));
                    }
                    if ui.button("Kopiuj").clicked() {
                        self.flash = Some(("Dziennik skopiowany do schowka".into(), self.clock()));
                        ui.ctx().copy_text(lines.join("
"));
                    }
                    if ui.button("Otwórz plik dziennika").clicked() {
                        let _ = std::process::Command::new("notepad").arg(crate::diag::file()).spawn();
                    }
                });
                ui.label(
                    RichText::new("Każdy krok synchronizacji z czasem trwania. Zapisywane też w pliku dziennika.")
                        .small()
                        .color(MUTED()),
                );
                ui.separator();
                egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                    if lines.is_empty() {
                        ui.label(RichText::new("Jeszcze nic się nie działo — kliknij Odśwież albo Test szybkości.").color(MUTED()));
                    }
                    for l in &lines {
                        let strong = l.contains("──");
                        let mut t = RichText::new(l).monospace().size(11.5);
                        if strong {
                            t = t.strong().color(PRIMARY);
                        } else if l.contains("failed") {
                            t = t.color(Color32::from_rgb(190, 40, 40));
                        }
                        ui.label(t);
                    }
                });
            });
        if testing {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        if !open {
            self.diag_open = false;
        }
    }

    /// Google Calendar read from its secret iCal address, every 15 minutes and on "Odśwież".
    pub(super) fn gcal_tick(&mut self) {
        if !self.gcal_loaded {
            self.gcal_loaded = true;
            self.gcal_text = crate::secret::unprotect(&self.prefs.gcal_saved).unwrap_or_default();
            if let Ok(t) = std::env::var("POCZTA_GCAL_TEST") {
                self.gcal_text = t;
            }
        }
        if let Some(res) = self.gcal_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.gcal_rx = None;
            match res {
                Ok(evs) => {
                    let n = evs.len();
                    self.store.events.retain(|e| !e.google);
                    self.store.events.extend(evs);
                    let word = match n {
                        1 => "wydarzenie",
                        n if (2..=4).contains(&(n % 10)) && !(12..=14).contains(&(n % 100)) => "wydarzenia",
                        _ => "wydarzeń",
                    };
                    self.gcal_said = Some(Ok(format!("{n} {word} z Google, {}", Local::now().format("%H:%M"))));
                    crate::diag::log(format!("Google calendar: {n} events"));
                    self.save_now = true;
                    self.touched = true;
                }
                Err(e) => {
                    crate::diag::log(format!("Google calendar: {e}"));
                    self.gcal_said = Some(Err(e));
                }
            }
        }
        // answers about events written to Google: the copy made here gives way to Google's own
        let pushed: Vec<(Option<u64>, Result<(), String>)> = self.gcal_push.as_ref().map(|(_, rx)| rx.try_iter().collect()).unwrap_or_default();
        for (local, res) in pushed {
            match res {
                Ok(()) => {
                    if let Some(id) = local {
                        self.store.events.retain(|e| e.id != id);
                    }
                    self.gcal_last = -1e9;
                    self.set_status("Zapisano w Kalendarzu Google");
                }
                Err(e) => {
                    crate::diag::log(format!("Google calendar save: {e}"));
                    self.set_status(format!("Nie udało się zapisać w Kalendarzu Google: {e}"));
                }
            }
            self.touched = true;
        }
        if self.prefs.gcal_key.is_empty() {
            self.prefs.gcal_key = crate::share::new_token() + &crate::share::new_token();
            self.save_now = true;
        }
        let oauth = self.google_calendar();
        let urls: Vec<String> = crate::gcal::urls(&self.gcal_text).into_iter().filter(|u| !crate::gcal::is_script(u)).collect();
        if (!oauth && urls.is_empty()) || self.gcal_rx.is_some() || self.loading.is_some() || self.shot.is_some() {
            return;
        }
        let now = self.clock();
        if now - self.gcal_last < 15.0 * 60.0 {
            self.ctx.request_repaint_after(std::time::Duration::from_secs_f64((15.0 * 60.0 - (now - self.gcal_last)).clamp(5.0, 900.0)));
            return;
        }
        self.gcal_last = now;
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        let key = self.prefs.gcal_key.clone();
        let acc = self.store.account.clone();
        std::thread::spawn(move || {
            let _ = tx.send(if oauth { crate::google::calendar_events(&acc) } else { crate::gcal::sync(&urls, &key) });
            ctx.request_repaint();
        });
        self.gcal_rx = Some(rx);
    }

    /// The calendar comes from Google through the sign-in (allowed and switched on).
    pub(super) fn google_calendar(&self) -> bool {
        let a = &self.store.account;
        self.prefs.google_calendar && crate::google::ready(a) && crate::google::has_scope(a, crate::google::SCOPE_CALENDAR)
    }

    /// The contacts are shared with Google through the sign-in (allowed and switched on).
    pub(super) fn google_contacts(&self) -> bool {
        let a = &self.store.account;
        self.prefs.google_contacts && crate::google::ready(a) && crate::google::has_scope(a, crate::google::SCOPE_CONTACTS)
    }

    /// Writes an event to Google Calendar through the script (in the background). `local`: the
    /// copy made here, removed once Google has it.
    pub(super) fn gcal_write(&mut self, local: Option<u64>, gid: String, ev: Option<Event>) {
        if !self.google_calendar() {
            return;
        }
        let acc = self.store.account.clone();
        let tx = self.gcal_push.get_or_insert_with(channel).0.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let res = match &ev {
                Some(e) => crate::google::save_event(&acc, &gid, &e.title, &e.location, e.date.and_time(e.start), e.minutes).map(|_| ()),
                None => crate::google::delete_event(&acc, &gid),
            };
            let _ = tx.send((local, res));
            ctx.request_repaint();
        });
    }

    /// "Kalendarz i kontakty Google": through the Google sign-in (as for the mail).
    pub(super) fn gcal_settings_ui(&mut self, ui: &mut Ui) {
        if let Some(r) = self.google_check_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.google_check = Some(r);
            self.google_check_rx = None;
        }
        let a = self.store.account.clone();
        let ready = crate::google::ready(&a);
        let (cal_ok, people_ok) = (crate::google::has_scope(&a, crate::google::SCOPE_CALENDAR), crate::google::has_scope(&a, crate::google::SCOPE_CONTACTS));
        ui.label(
            RichText::new("Kalendarz i kontakty łączą się z Google tym samym logowaniem co poczta (konto Google, OAuth). Wydarzenia i kontakty z Google są tu na bieżąco, a zmiany zrobione w programie trafiają do Google.")
                .small()
                .color(MUTED()),
        );
        ui.add_space(4.0);
        let ok = Color32::from_rgb(0x2E, 0x9E, 0x5B);
        if !ready {
            ui.label(RichText::new("Poczta nie jest zalogowana kontem Google (OAuth). Zaloguj się w „Konto pocztowe” → Konto Google.").color(FLAG));
            if ui.button("Otwórz konto pocztowe").clicked() {
                self.account_open = true;
                self.gcal_open = false;
            }
            return;
        }
        ui.label(RichText::new(format!("Konto Google: {}", a.email)).strong());
        for (label, granted) in [("Poczta", true), ("Kalendarz", cal_ok), ("Kontakty", people_ok)] {
            if granted {
                ui.label(RichText::new(format!("✓  {label} — dostęp udzielony")).color(ok));
            } else {
                ui.label(RichText::new(format!("✗  {label} — brak zgody (zaloguj się ponownie poniżej)")).color(FLAG));
            }
        }
        if !(cal_ok && people_ok) {
            ui.add_space(6.0);
            ui.label(RichText::new("Jednorazowo, w Twoim projekcie Google Cloud (tym od logowania poczty):").small());
            ui.horizontal_wrapped(|ui| {
                if ui.small_button("1. Włącz Google Calendar API ↗").clicked() {
                    crate::oauth::open_url("https://console.cloud.google.com/apis/library/calendar-json.googleapis.com");
                }
                if ui.small_button("2. Włącz People API ↗").clicked() {
                    crate::oauth::open_url("https://console.cloud.google.com/apis/library/people.googleapis.com");
                }
            });
            ui.label(RichText::new("3. Potem zaloguj się ponownie — Google zapyta o dostęp do poczty, kalendarza i kontaktów; zaznacz wszystkie pola i kliknij Kontynuuj.").small());
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let label = if cal_ok && people_ok { "Zaloguj ponownie przez Google" } else { "  G   Zaloguj ponownie przez Google (poczta, kalendarz, kontakty)  " };
            let btn = egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(ACCENT());
            if ui.add_enabled(!self.signing_in, btn).clicked() {
                self.signing_in = true;
                self.sign_in_error = None;
                net::google_sign_in(a.oauth_client_id.clone(), a.oauth_client_secret.clone(), self.tx.clone(), self.ctx.clone());
                self.set_status("Otwieram stronę logowania Google w przeglądarce…");
            }
            if self.signing_in {
                ui.label(RichText::new("dokończ logowanie w przeglądarce…").small().color(MUTED()));
            }
        });
        if let Some(e) = &self.sign_in_error {
            ui.colored_label(FLAG, e);
        }
        ui.add_space(6.0);
        let mut changed = false;
        changed |= ui.checkbox(&mut self.prefs.google_calendar, "Synchronizuj kalendarz z Google").changed();
        changed |= ui.checkbox(&mut self.prefs.google_contacts, "Synchronizuj kontakty i grupy z Google").changed();
        if changed {
            self.save_now = true;
            self.gcal_last = -1e9;
            self.book_last = -1e9;
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            let busy = self.google_check_rx.is_some();
            let auto = std::env::var("POCZTA_START").as_deref() == Ok("google-check") && {
                use std::sync::atomic::{AtomicBool, Ordering};
                static ONCE: AtomicBool = AtomicBool::new(false);
                !ONCE.swap(true, Ordering::Relaxed)
            };
            if ui.add_enabled(!busy, egui::Button::new("Sprawdź połączenie")).on_hover_text("Sprawdza dostęp do kalendarza i kontaktów (tylko odczyt)").clicked() || auto {
                let (tx, rx) = channel();
                let (acc, ctx) = (a.clone(), ui.ctx().clone());
                std::thread::spawn(move || {
                    let _ = tx.send(crate::google::check(&acc));
                    ctx.request_repaint();
                });
                self.google_check_rx = Some(rx);
                self.google_check = None;
            }
            if ui.add_enabled(self.gcal_rx.is_none(), egui::Button::new("Synchronizuj teraz")).clicked() {
                self.gcal_last = -1e9;
                self.book_last = -1e9;
                self.gcal_said = None;
                self.book_said = None;
            }
            if busy || self.gcal_rx.is_some() || self.book_rx.is_some() {
                ui.label(RichText::new("pracuję…").small().color(MUTED()));
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
            }
        });
        if let Some((cal, people)) = &self.google_check {
            for (what, r) in [("Kalendarz", cal), ("Kontakty", people)] {
                match r {
                    Ok(m) => {
                        ui.label(RichText::new(format!("✓  {m}")).small().color(ok));
                    }
                    Err(e) => {
                        ui.label(RichText::new(format!("✗  {what}: {e}")).small().color(FLAG));
                    }
                }
            }
        }
        for said in [&self.gcal_said, &self.book_said] {
            match said {
                Some(Ok(m)) => {
                    ui.label(RichText::new(format!("✓  {m}")).small().color(ok));
                }
                Some(Err(e)) => {
                    ui.label(RichText::new(e.as_str()).small().color(FLAG));
                }
                None => {}
            }
        }
        ui.label(RichText::new("Wydarzenia powtarzające się, całodniowe i z cudzych kalendarzy są tylko do odczytu — zmieniasz je w Google.").small().color(MUTED()));
    }

    /// „Poprawianie tekstu (AI)”: where CLIProxy is, its key and the model that corrects.
    pub(super) fn ai_settings_ui(&mut self, ui: &mut Ui) {
        if let Some((said, models)) = self.ai_check.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.ai_said = Some(said);
            if !models.is_empty() {
                self.ai_models = models;
            }
            self.ai_check = None;
        }
        let s = self.ai_set.get_or_insert_with(crate::ai::load);
        let mut changed = false;
        ui.label(RichText::new("Poprawianie tekstu (AI)").strong());
        ui.label(
            RichText::new("Przycisk „✨ Popraw z AI” w oknie pisania poprawia tekst przez CLIProxy, w jednym z czterech stylów.")
                .small()
                .color(MUTED()),
        );
        ui.add_space(4.0);
        egui::Grid::new("accgrid-ai").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Adres CLIProxy");
            changed |= ui.add(egui::TextEdit::singleline(&mut s.url).hint_text("http://100.100.1.2:8317").desired_width(290.0)).changed();
            ui.end_row();
            ui.label("Klucz");
            changed |= ui.add(egui::TextEdit::singleline(&mut s.key).password(true).hint_text("sk-…").desired_width(290.0)).changed();
            ui.end_row();
            ui.label("Model");
            ui.horizontal(|ui| {
                changed |= ui.add(egui::TextEdit::singleline(&mut s.model).hint_text("claude-sonnet-5-5").desired_width(200.0)).changed();
                let mut list: Vec<String> = vec!["claude-sonnet-5-5".into(), "claude-opus-5-5".into(), "claude-haiku-5-5".into()];
                for m in &self.ai_models {
                    if !list.contains(m) {
                        list.push(m.clone());
                    }
                }
                egui::ComboBox::from_id_salt("ai-model").selected_text("").width(24.0).show_ui(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                        for m in list {
                            if ui.selectable_label(s.model == m, &m).clicked() {
                                s.model = m;
                                changed = true;
                            }
                        }
                    });
                });
            });
            ui.end_row();
            let effort_box = |ui: &mut Ui, salt: &str, value: &mut String| -> bool {
                let mut ch = false;
                let shown = crate::ai::EFFORTS.iter().find(|(k, _)| *k == value.as_str()).map(|(_, l)| *l).unwrap_or("bez myślenia (najszybciej)");
                egui::ComboBox::from_id_salt(salt).selected_text(shown).width(200.0).show_ui(ui, |ui| {
                    for (k, l) in crate::ai::EFFORTS {
                        if ui.selectable_label(value.as_str() == k, l).clicked() {
                            *value = k.to_string();
                            ch = true;
                        }
                    }
                });
                ch
            };
            ui.label("Myślenie");
            changed |= effort_box(ui, "ai-effort", &mut s.effort);
            ui.end_row();
            ui.label("Tłumaczenie");
            ui.horizontal(|ui| {
                changed |= ui.add(egui::TextEdit::singleline(&mut s.translate_model).hint_text("claude-haiku-5-5").desired_width(200.0)).changed();
                egui::ComboBox::from_id_salt("ai-tr-model").selected_text("").width(24.0).show_ui(ui, |ui| {
                    for m in ["claude-haiku-5-5", "claude-sonnet-5-5", "claude-opus-5-5"] {
                        if ui.selectable_label(s.translate_model == m, m).clicked() {
                            s.translate_model = m.to_string();
                            changed = true;
                        }
                    }
                });
            });
            ui.end_row();
            ui.label("Myślenie (tłum.)");
            changed |= effort_box(ui, "ai-tr-effort", &mut s.translate_effort);
            ui.end_row();
        });
        ui.label(
            RichText::new("Model i myślenie z góry służą do „Popraw z AI”. Do tłumaczenia wystarczy szybki Haiku bez myślenia; wyższe myślenie daje staranniejszy wynik, ale trwa dłużej.")
                .small()
                .color(MUTED()),
        );
        if changed {
            crate::ai::save(s);
            self.ai_said = None;
        }
        ui.horizontal(|ui| {
            let checking = self.ai_check.is_some();
            if ui.add_enabled(!checking, egui::Button::new("Sprawdź połączenie").small()).clicked() {
                let (tx, rx) = std::sync::mpsc::channel();
                let set = s.clone();
                let ctx = ui.ctx().clone();
                std::thread::spawn(move || {
                    let said = crate::ai::test(&set);
                    let models = crate::ai::models(&set).unwrap_or_default();
                    let _ = tx.send((said, models));
                    ctx.request_repaint();
                });
                self.ai_check = Some(rx);
                self.ai_said = None;
            }
            if checking {
                ui.label(RichText::new("sprawdzam…").small().color(MUTED()));
            }
            match &self.ai_said {
                Some(Ok(m)) => {
                    ui.label(RichText::new(format!("✓  {m}")).small().color(Color32::from_rgb(0x2E, 0x9E, 0x5B)));
                }
                Some(Err(e)) => {
                    ui.label(RichText::new(e.as_str()).small().color(FLAG));
                }
                None => {}
            }
        });
        if self.ai_check.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
        }
    }

    /// The AI settings and the Google Calendar each in a window of their own.
    pub(super) fn side_settings_ui(&mut self, ctx: &egui::Context) {
        if self.ai_open {
            let mut open = true;
            crate::theme::dialog(ctx, "ai", "Ustawienia AI", 600.0, &mut open, |ui| self.ai_settings_ui(ui));
            if !open {
                self.ai_open = false;
            }
        } else {
            self.ai_set = None;
        }
        if self.gcal_open {
            let mut open = true;
            crate::theme::dialog(ctx, "google", "Kalendarz i kontakty Google", 620.0, &mut open, |ui| self.gcal_settings_ui(ui));
            if !open {
                self.gcal_open = false;
            }
        }
    }

    /// A further mail account (mail only, signed in with a password — for Gmail an app password).
    pub(super) fn extra_account_ui(&mut self, ctx: &egui::Context) {
        let Some((which, mut a)) = self.extra_edit.clone() else { return };
        let mut open = true;
        let (mut save, mut remove, mut cancel) = (false, false, false);
        let title = if which.is_some() { "Konto pocztowe" } else { "Dodaj konto pocztowe" };
        crate::theme::dialog(ctx, "extra-account", title, 540.0, &mut open, |ui| {
            ui.label(RichText::new("Dodatkowe konto służy tylko do poczty. Jego nieprzeczytane wiadomości trafiają do wspólnej listy „Nieprzeczytane”, a odpowiedź wychodzi z konta, na które przyszła wiadomość.").small().color(MUTED()));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new("  G   To jest Gmail  ").color(Color32::WHITE)).fill(PRIMARY)).on_hover_text("Wpisuje ustawienia serwerów Gmaila").clicked() {
                    a.use_gmail();
                    a.use_oauth = false;
                }
                if ui.link(RichText::new("Utwórz hasło aplikacji Google ↗").small()).clicked() {
                    crate::oauth::open_url("https://myaccount.google.com/apppasswords");
                }
            });
            ui.add_space(6.0);
            egui::Grid::new("extra-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Imię i nazwisko");
                ui.add(egui::TextEdit::singleline(&mut a.display_name).desired_width(320.0));
                ui.end_row();
                ui.label("Adres e-mail");
                let r = ui.add(egui::TextEdit::singleline(&mut a.email).hint_text("ty@firma.pl").desired_width(320.0));
                if r.changed() && (a.username.is_empty() || a.imap_host == "imap.gmail.com") {
                    a.username = a.email.clone();
                }
                ui.end_row();
                ui.label("Hasło");
                ui.add(egui::TextEdit::singleline(&mut a.password).password(true).hint_text("Gmail: 16-znakowe hasło aplikacji").desired_width(320.0));
                ui.end_row();
            });
            egui::CollapsingHeader::new("Ustawienia serwerów").id_salt("extra-servers").default_open(a.imap_host.is_empty()).show(ui, |ui| {
                egui::Grid::new("extra-servers-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("Serwer IMAP");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut a.imap_host).hint_text("imap.example.com").desired_width(220.0));
                        ui.add(egui::DragValue::new(&mut a.imap_port).range(1..=65535));
                    });
                    ui.end_row();
                    ui.label("Serwer SMTP");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut a.smtp_host).hint_text("smtp.example.com").desired_width(220.0));
                        ui.add(egui::DragValue::new(&mut a.smtp_port).range(1..=65535));
                    });
                    ui.end_row();
                    ui.label("Login");
                    ui.add(egui::TextEdit::singleline(&mut a.username).hint_text("zwykle adres e-mail").desired_width(300.0));
                    ui.end_row();
                });
            });
            ui.label(RichText::new("Hasło jest zapisane w zaszyfrowanej postaci (tylko dla Twojego konta Windows).").small().color(MUTED()));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ready = !a.email.trim().is_empty() && !a.imap_host.trim().is_empty() && !a.password.is_empty();
                if ui.add_enabled(ready, egui::Button::new(RichText::new("Zapisz i sprawdź pocztę").color(Color32::WHITE)).fill(ACCENT())).clicked() {
                    save = true;
                }
                if ui.button("Anuluj").clicked() {
                    cancel = true;
                }
                if which.is_some() {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button(RichText::new("Usuń konto").color(FLAG)).on_hover_text("Usuwa konto i jego wiadomości z programu (na serwerze nic się nie zmienia)").clicked() {
                            remove = true;
                        }
                    });
                }
            });
        });
        if save {
            a.use_oauth = false;
            if a.username.trim().is_empty() {
                a.username = a.email.trim().to_string();
            }
            if a.display_name.trim().is_empty() {
                a.display_name = a.email.trim().to_string();
            }
            match which {
                Some(k) if k < self.store.extra.len() => self.store.extra[k].account = a,
                _ => {
                    self.store.extra.push(crate::model::ExtraAccount { account: a, ..Default::default() });
                    let acct = self.store.extra.len() as u32;
                    self.view = MailView::Folder(Folder::Inbox);
                    self.view_account = acct;
                }
            }
            self.extra_edit = None;
            self.save_now = true;
            self.touched = true;
            self.quiet_sync = false;
            self.send_receive();
            return;
        }
        if remove {
            if let Some(k) = which.filter(|k| *k < self.store.extra.len()) {
                let acct = k as u32 + 1;
                self.store.extra.remove(k);
                // its messages go; the accounts after it move one place up
                self.store.messages.retain(|m| m.account != acct);
                for m in self.store.messages.iter_mut().filter(|m| m.account > acct) {
                    m.account -= 1;
                }
                self.pending_ops.retain(|(x, _)| *x != acct);
                for (x, _) in self.pending_ops.iter_mut().filter(|(x, _)| *x > acct) {
                    *x -= 1;
                }
                self.view_account = 0;
                self.view = MailView::Folder(Folder::Inbox);
                self.selected = None;
                self.save_now = true;
                self.touched = true;
                self.set_status("Konto usunięte z programu");
            }
            self.extra_edit = None;
            return;
        }
        if cancel || !open {
            self.extra_edit = None;
        } else {
            self.extra_edit = Some((which, a));
        }
    }

    pub(super) fn account_ui(&mut self, ctx: &egui::Context) {
        if !self.account_open {
            return;
        }
        let mut open = true;
        let mut save_and_check = false;
        let mut start_sign_in = false;
        let mut refresh_signature = false;
        // the window fits on the screen; what does not fit scrolls
        crate::theme::dialog(ctx, "account", "Konto pocztowe", 560.0, &mut open, |ui| {
                let signing_in = self.signing_in;
                let sign_in_error = self.sign_in_error.clone();
                let show_setup = self.oauth_setup;
                let a = &mut self.store.account;
                // (an account already on Gmail needs no Gmail preset: only "Zaloguj przez Google" stays)
                if a.imap_host != "imap.gmail.com" {
                    ui.horizontal(|ui| {
                        if ui
                            .add(egui::Button::new(RichText::new("  G   Użyj Gmaila  ").color(Color32::WHITE)).fill(PRIMARY))
                            .on_hover_text("Wpisz ustawienia serwerów Gmaila i logowanie przez Google")
                            .clicked()
                        {
                            a.use_gmail();
                        }
                        ui.label(RichText::new("lub wpisz ustawienia swojego dostawcy").small().color(MUTED()));
                    });
                    ui.add_space(8.0);
                }
                egui::Grid::new("accgrid-top").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("Imię i nazwisko");
                    ui.add(egui::TextEdit::singleline(&mut a.display_name).desired_width(300.0));
                    ui.end_row();
                    ui.label("Adres e-mail");
                    let r = ui.add(egui::TextEdit::singleline(&mut a.email).hint_text("ty@gmail.com").desired_width(300.0));
                    if r.changed() && a.imap_host == "imap.gmail.com" {
                        a.username = a.email.clone();
                    }
                    ui.end_row();
                    ui.label("Logowanie");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut a.use_oauth, true, "Konto Google");
                        ui.selectable_value(&mut a.use_oauth, false, "Hasło");
                    });
                    ui.end_row();
                });
                ui.add_space(6.0);
                if a.use_oauth {
                    if !a.oauth_refresh_token.is_empty() {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("✓  Zalogowano przez Google jako {}", a.email))
                                    .color(Color32::from_rgb(0x2E, 0x9E, 0x5B)),
                            );
                            if ui.small_button("Wyloguj").clicked() {
                                a.oauth_refresh_token.clear();
                            }
                        });
                    } else {
                        ui.horizontal(|ui| {
                            let btn = egui::Button::new(RichText::new("G    Zaloguj przez Google").color(TEXT()))
                                .fill(BG())
                                .stroke(Stroke::new(1.0, BORDER()))
                                .min_size(Vec2::new(0.0, 30.0));
                            if ui.add_enabled(!signing_in, btn).clicked() {
                                start_sign_in = true;
                            }
                            if signing_in {
                                ui.spinner();
                                ui.label(RichText::new("Dokończ logowanie w przeglądarce…").color(MUTED()));
                            }
                        });
                    }
                    if let Some(e) = &sign_in_error {
                        ui.colored_label(FLAG, e);
                    }
                    if show_setup && a.oauth_client_id.trim().is_empty() {
                        ui.add_space(6.0);
                        Frame::new()
                            .fill(SURFACE())
                            .stroke(Stroke::new(1.0, BORDER()))
                            .corner_radius(CornerRadius::same(6))
                            .inner_margin(Margin::same(10))
                            .show(ui, |ui| {
                                ui.label(RichText::new("Jednorazowa konfiguracja (ok. 5 minut)").font(semibold(14.0)));
                                ui.label(
                                    RichText::new(
                                        "Google wymaga, by każda aplikacja miała własny identyfikator. Oryginalny program \
                                         dostał go od swojego producenta; tę aplikację trzeba raz zarejestrować na swoim koncie.",
                                    )
                                    .small()
                                    .color(MUTED()),
                                );
                                ui.add_space(4.0);
                                let steps: [(&str, &str); 4] = [
                                    ("1. Utwórz projekt w Google Cloud", "https://console.cloud.google.com/projectcreate"),
                                    ("2. Włącz Gmail API", "https://console.cloud.google.com/apis/library/gmail.googleapis.com"),
                                    ("3. Ekran zgody: Zewnętrzny, dodaj siebie jako testera", "https://console.cloud.google.com/auth/audience"),
                                    ("4. Utwórz klienta „Aplikacja komputerowa” i pobierz plik JSON", "https://console.cloud.google.com/auth/clients"),
                                ];
                                for (label, url) in steps {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new(label).small());
                                        if ui.small_button("Otwórz ↗").clicked() {
                                            crate::oauth::open_url(url);
                                        }
                                    });
                                }
                                ui.label(
                                    RichText::new("5. Pobrany plik zostanie znaleziony sam (folder Pobrane) — albo przeciągnij go na to okno.")
                                        .small(),
                                );
                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Bez konfiguracji:").small().color(MUTED()));
                                    if ui.link(RichText::new("zaloguj hasłem aplikacji").small()).clicked() {
                                        a.use_oauth = false;
                                        crate::oauth::open_url("https://myaccount.google.com/apppasswords");
                                    }
                                });
                            });
                    }
                    egui::CollapsingHeader::new("Zaawansowane")
                        .id_salt("oauth-advanced")
                        .show(ui, |ui| {
                            egui::Grid::new("accgrid-oauth").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                                ui.label(RichText::new("Client ID").small());
                                ui.add(
                                    egui::TextEdit::singleline(&mut a.oauth_client_id)
                                        .hint_text("….apps.googleusercontent.com")
                                        .desired_width(280.0),
                                );
                                ui.end_row();
                                ui.label(RichText::new("Client secret").small());
                                ui.add(
                                    egui::TextEdit::singleline(&mut a.oauth_client_secret)
                                        .password(true)
                                        .hint_text("GOCSPX-…")
                                        .desired_width(280.0),
                                );
                                ui.end_row();
                            });
                        });
                } else {
                    egui::Grid::new("accgrid-pass").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        ui.label("Hasło");
                        ui.add(
                            egui::TextEdit::singleline(&mut a.password)
                                .password(true)
                                .hint_text("Gmail: 16-znakowe hasło aplikacji")
                                .desired_width(300.0),
                        );
                        ui.end_row();
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Hasło jest zapisane w zaszyfrowanej postaci (tylko dla Twojego konta Windows).").small().color(MUTED()));
                        if a.imap_host == "imap.gmail.com" && ui.link(RichText::new("Utwórz hasło aplikacji ↗").small()).clicked() {
                            crate::oauth::open_url("https://myaccount.google.com/apppasswords");
                        }
                    });
                }
                ui.add_space(4.0);
                egui::CollapsingHeader::new("Ustawienia serwerów")
                    .id_salt("servers")
                    .default_open(!a.use_oauth && a.imap_host.is_empty())
                    .show(ui, |ui| {
                        egui::Grid::new("accgrid-servers").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                            ui.label("Serwer IMAP");
                            ui.horizontal(|ui| {
                                ui.add(egui::TextEdit::singleline(&mut a.imap_host).hint_text("imap.example.com").desired_width(210.0));
                                ui.add(egui::DragValue::new(&mut a.imap_port).range(1..=65535));
                            });
                            ui.end_row();
                            ui.label("Serwer SMTP");
                            ui.horizontal(|ui| {
                                ui.add(egui::TextEdit::singleline(&mut a.smtp_host).hint_text("smtp.example.com").desired_width(210.0));
                                ui.add(egui::DragValue::new(&mut a.smtp_port).range(1..=65535));
                            });
                            ui.end_row();
                            ui.label("Login");
                            ui.add(egui::TextEdit::singleline(&mut a.username).hint_text("zwykle adres e-mail").desired_width(290.0));
                            ui.end_row();
                        });
                    });
                ui.add_space(8.0);
                ui.separator();
                // Signature: taken from the person's own sent mail (Gmail adds it there)
                {
                    ui.label(RichText::new("Podpis").strong());
                    ui.checkbox(&mut self.store.account.signature_on, "Dodawaj podpis do nowych wiadomości i odpowiedzi");
                    if self.store.account.signature_text.trim().is_empty() {
                        ui.label(
                            RichText::new("Nie znaleziono jeszcze podpisu — pojawi się, gdy zsynchronizują się Twoje wiadomości wysłane.")
                                .small()
                                .color(MUTED()),
                        );
                    } else {
                        Frame::new().stroke(Stroke::new(1.0, BORDER())).corner_radius(CornerRadius::same(4)).inner_margin(Margin::same(8)).show(ui, |ui| {
                            egui::ScrollArea::vertical().id_salt("sig-settings").max_height(220.0).show(ui, |ui| self.signature_preview(ui));
                        });
                    }
                    let a = &mut self.store.account;
                    ui.horizontal(|ui| {
                        if ui.button("Pobierz ponownie z wysłanych").on_hover_text("Szuka podpisu w Twoich ostatnich wiadomościach wysłanych").clicked() {
                            a.signature_auto = true;
                            refresh_signature = true;
                        }
                        if !a.signature_text.is_empty() && ui.small_button("Usuń podpis").clicked() {
                            a.signature_text.clear();
                            a.signature_html.clear();
                            a.signature_images.clear();
                            a.signature_auto = false;
                        }
                    });
                    ui.label(RichText::new("Podpis jest brany z Twoich wiadomości wysłanych przez Gmaila, razem z wyglądem i zdjęciem.").small().color(MUTED()));
                }
                ui.add_space(8.0);
                ui.separator();
                // the footer as in Windows dialogs: on the right, the main action first
                ui.allocate_ui_with_layout(Vec2::new(ui.available_width(), 32.0), Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(egui::Button::new("Zamknij").min_size(Vec2::new(0.0, 30.0))).clicked() {
                        self.account_open = false;
                    }
                    if ui.add(egui::Button::new(RichText::new("Zapisz i sprawdź pocztę").color(Color32::WHITE)).fill(ACCENT()).min_size(Vec2::new(0.0, 30.0))).clicked() {
                        save_and_check = true;
                    }
                });
            });
        if refresh_signature {
            self.sig_seen = 0;
            self.save_now = true;
        }
        if !open {
            self.account_open = false;
            self.save_now = true;
        }
        // While the one-time setup is shown, keep looking for Google's downloaded file.
        let now = ctx.input(|i| i.time);
        if self.oauth_setup && self.store.account.oauth_client_id.trim().is_empty() && now - self.last_scan > 2.0 {
            self.last_scan = now;
            if self.find_google_client() {
                self.oauth_setup = false;
                start_sign_in = true;
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(2));
        }
        if start_sign_in {
            if self.store.account.oauth_client_id.trim().is_empty() && !self.find_google_client() {
                // Nothing set up yet: show the short one-time setup instead of an error.
                self.oauth_setup = true;
            } else {
                self.signing_in = true;
                self.sign_in_error = None;
                let a = &self.store.account;
                net::google_sign_in(a.oauth_client_id.clone(), a.oauth_client_secret.clone(), self.tx.clone(), self.ctx.clone());
                self.set_status("Otwieram stronę logowania Google w przeglądarce…");
            }
        }
        if save_and_check {
            self.account_open = false;
            self.save_now = true;
            self.send_receive();
        }
    }
}
