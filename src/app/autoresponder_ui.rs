use super::*;
use crate::autoresponder::{iso_day, shown_day, Link, Rules};

type Answer = Result<(Rules, String), String>;

/// A day as typed in the form (DD-MM-RRRR), if it is one.
fn typed_day(s: &str) -> Option<chrono::NaiveDate> {
    use chrono::Datelike;
    chrono::NaiveDate::parse_from_str(s.trim(), "%d-%m-%Y").ok().filter(|d| d.year() >= 2000)
}

/// The button next to a day, and the calendar it opens under itself (over the form, which stays
/// where it is). Returns the day clicked.
fn pick_day(ui: &mut Ui, store: &crate::model::Store, picking: &mut Option<(bool, chrono::NaiveDate)>, end: bool, typed: &str) -> Option<chrono::NaiveDate> {
    let open = matches!(picking, Some((e, _)) if *e == end);
    let resp = ui.add(egui::Button::new(crate::theme::ic::CALENDAR).small().selected(open)).on_hover_text(t!("ui.settings.autoresponder.pick_day"));
    if resp.clicked() {
        *picking = (!open).then(|| (end, typed_day(typed).unwrap_or_else(|| chrono::Local::now().date_naive())));
    }
    let (_, shown) = picking.as_mut().filter(|(e, _)| *e == end)?;
    let (mut still, mut picked) = (true, None);
    let sel = typed_day(typed).unwrap_or(*shown);
    egui::Popup::from_response(&resp).open_bool(&mut still).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        picked = crate::calendar::day_picker(ui, if end { "auto-to" } else { "auto-from" }, store, shown, sel);
    });
    if !still {
        *picking = None;
    }
    picked
}

/// The autoresponder window (☰ → Autoresponder): its state lives while the window is open.
pub(super) struct AutoUi {
    /// the webhook the form was read from, and where Save writes
    link: Link,
    /// the address and token as typed; they become `link` once "Connect" has read from them
    draft: Link,
    /// "Connect" running: the link it tries
    connecting: Option<Link>,
    /// the settings being edited; None until n8n answered
    rules: Option<Rules>,
    /// the days as typed (DD-MM-RRRR)
    from: String,
    to: String,
    /// the calendar open under the days: for the end day (else the start), and the month it shows
    picking: Option<(bool, chrono::NaiveDate)>,
    /// what the person asks the AI for
    sentence: String,
    /// work in the background: what it is, and its answer (the settings and what to say)
    busy: Option<(String, Receiver<Answer>)>,
    said: Option<Result<String, String>>,
}

impl AutoUi {
    /// `demo`: a test run or picture, which never reads the saved webhook.
    pub(super) fn new(ctx: &egui::Context, demo: bool) -> Self {
        let link = if demo { Link::default() } else { crate::autoresponder::load() };
        let mut a = Self { draft: link.clone(), link, connecting: None, rules: None, from: String::new(), to: String::new(), picking: None, sentence: String::new(), busy: None, said: None };
        if demo {
            // a filled form without n8n
            a.show(Rules {
                enabled: true,
                date_start: "2026-11-19".into(),
                date_end: "2026-11-21".into(),
                hour_start: 15,
                hour_end: 21,
                target_address: "jan@example.com".into(),
                text_before: "Dziękuję za wiadomość. Odpowiem po godzinie {start}:00.\n\nW dniach od {od} do {do} pracuję w godzinach {start}:00–{koniec}:00.".into(),
                text_after: "Dziękuję za wiadomość. Zajmę się tą sprawą następnego dnia roboczego po {start}:00.".into(),
                ignored: "biuro@example.com\nfaktury@example.com".into(),
                updated_by: "Poczta 2026-11-18T12:41:11Z".into(),
            });
            if std::env::var("POCZTA_START").as_deref() == Ok("autoresponder-days") {
                a.picking = typed_day(&a.from).map(|d| (false, d));
            }
        } else if a.link.is_set() {
            a.load(ctx);
        }
        a
    }

    fn show(&mut self, r: Rules) {
        self.from = shown_day(&r.date_start);
        self.to = shown_day(&r.date_end);
        self.rules = Some(r);
    }

    fn run(&mut self, ctx: &egui::Context, what: String, job: impl FnOnce() -> Answer + Send + 'static) {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(job());
            ctx.request_repaint();
        });
        self.busy = Some((what, rx));
        self.said = None;
    }

    fn load(&mut self, ctx: &egui::Context) {
        let link = self.link.clone();
        self.run(ctx, t!("ui.settings.autoresponder.loading").into_owned(), move || crate::autoresponder::fetch(&link).map(|r| (r, t!("ui.settings.autoresponder.loaded").into_owned())));
    }

    /// The form as settings: its days back in RRRR-MM-DD (empty when not a day).
    fn form(&self) -> Rules {
        let mut r = self.rules.clone().unwrap_or_default();
        r.date_start = iso_day(&self.from).unwrap_or_default();
        r.date_end = iso_day(&self.to).unwrap_or_default();
        r.target_address = r.target_address.trim().to_string();
        r
    }

    /// The form as settings to send, checked.
    fn edited(&self) -> Result<Rules, String> {
        let r = self.form();
        r.problem().map_or(Ok(r), Err)
    }
}

impl MailApp {
    pub(super) fn autoresponder_ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let demo = self.shot.is_some() || crate::unseen();
        let store = &self.store;
        let a = self.auto_ui.get_or_insert_with(|| AutoUi::new(&ctx, demo));
        let got = match a.busy.as_ref().map(|(_, rx)| rx.try_recv()) {
            Some(Ok(answer)) => Some(answer),
            // (the job's thread died without an answer)
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => Some(Err(t!("core.net.unexpected").into_owned())),
            _ => None,
        };
        if let Some(answer) = got {
            a.busy = None;
            let tried = a.connecting.take();
            match answer {
                Ok((r, m)) => {
                    if let Some(l) = tried {
                        a.link = l;
                    }
                    a.show(r);
                    a.said = Some(Ok(m));
                }
                Err(e) => a.said = Some(Err(e)),
            }
        }
        let busy = a.busy.is_some();
        ui.label(RichText::new(t!("ui.settings.autoresponder.intro")).small().color(MUTED()));
        ui.add_space(4.0);
        egui::CollapsingHeader::new(t!("ui.settings.autoresponder.link")).id_salt("auto-link").default_open(!a.link.is_set()).show(ui, |ui| {
            egui::Grid::new("auto-link-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label(t!("ui.settings.autoresponder.url"));
                ui.add(egui::TextEdit::singleline(&mut a.draft.url).hint_text("https://…/webhook/poczta-autoresponder").desired_width(380.0));
                ui.end_row();
                ui.label(t!("ui.settings.autoresponder.token"));
                ui.add(egui::TextEdit::singleline(&mut a.draft.token).password(true).desired_width(380.0));
                ui.end_row();
            });
            if ui.add_enabled(!busy && a.draft.is_set(), egui::Button::new(t!("ui.settings.autoresponder.connect")).small()).clicked() {
                // the new link replaces the old one (and its form) only once it has answered and
                // is saved: a mistyped token keeps the working connection
                let draft = a.draft.clone();
                a.connecting = Some(draft.clone());
                a.run(&ctx, t!("ui.settings.autoresponder.loading").into_owned(), move || {
                    let r = crate::autoresponder::fetch(&draft)?;
                    crate::autoresponder::save(&draft)?;
                    Ok((r, t!("ui.settings.autoresponder.loaded").into_owned()))
                });
            }
        });
        let mut ask = false;
        if let Some(r) = a.rules.as_mut() {
            ui.add_space(6.0);
            ui.add_enabled_ui(!busy, |ui| {
                ui.horizontal(|ui| {
                    ui.label(t!("ui.settings.autoresponder.ai"));
                    ui.add(egui::TextEdit::singleline(&mut a.sentence).hint_text(t!("ui.settings.autoresponder.ai_placeholder")).desired_width(330.0));
                    ask = ui.add_enabled(!a.sentence.trim().is_empty(), egui::Button::new(t!("ui.settings.autoresponder.ai_button"))).clicked();
                });
                ui.add_space(6.0);
                ui.checkbox(&mut r.enabled, RichText::new(t!("ui.settings.autoresponder.enabled")).strong());
                ui.add_space(4.0);
                egui::Grid::new("auto-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label(t!("ui.settings.autoresponder.dates"));
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut a.from).desired_width(96.0));
                        if let Some(d) = pick_day(ui, store, &mut a.picking, false, &a.from) {
                            // the start picked: the end next, at least that day
                            a.from = d.format("%d-%m-%Y").to_string();
                            if typed_day(&a.to).is_none_or(|t| t < d) {
                                a.to = a.from.clone();
                            }
                            a.picking = Some((true, d));
                        }
                        ui.label("–");
                        ui.add(egui::TextEdit::singleline(&mut a.to).desired_width(96.0));
                        if let Some(d) = pick_day(ui, store, &mut a.picking, true, &a.to) {
                            a.to = d.format("%d-%m-%Y").to_string();
                            a.picking = None;
                        }
                        ui.label(RichText::new(t!("ui.settings.autoresponder.dates_hint")).small().color(MUTED()));
                    });
                    ui.end_row();
                    ui.label(t!("ui.settings.autoresponder.hours"));
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut r.hour_start).range(0..=24).suffix(":00"));
                        ui.label("–");
                        ui.add(egui::DragValue::new(&mut r.hour_end).range(0..=24).suffix(":00"));
                    });
                    ui.end_row();
                    ui.label("");
                    ui.add(egui::Label::new(RichText::new(t!("ui.settings.autoresponder.hours_hint")).small().color(MUTED())).wrap());
                    ui.end_row();
                    ui.label(t!("ui.settings.autoresponder.address"));
                    ui.add(egui::TextEdit::singleline(&mut r.target_address).desired_width(300.0)).on_hover_text(t!("ui.settings.autoresponder.address_hint"));
                    ui.end_row();
                    ui.label(t!("ui.settings.autoresponder.before"));
                    ui.add(egui::TextEdit::multiline(&mut r.text_before).desired_rows(2).desired_width(420.0));
                    ui.end_row();
                    ui.label(t!("ui.settings.autoresponder.after"));
                    ui.add(egui::TextEdit::multiline(&mut r.text_after).desired_rows(2).desired_width(420.0));
                    ui.end_row();
                    ui.label("");
                    ui.add(egui::Label::new(RichText::new(t!("ui.settings.autoresponder.texts_hint")).small().color(MUTED())).wrap());
                    ui.end_row();
                    ui.label(t!("ui.settings.autoresponder.ignored"));
                    ui.add(egui::TextEdit::multiline(&mut r.ignored).desired_rows(2).desired_width(420.0)).on_hover_text(t!("ui.settings.autoresponder.ignored_hint"));
                    ui.end_row();
                });
            });
            if let Some(who) = Some(r.updated_by.trim()).filter(|w| !w.is_empty()) {
                ui.label(RichText::new(t!("ui.settings.autoresponder.last", who = who)).small().color(MUTED()));
            }
        }
        // (asked for after the form, which holds `a.rules` borrowed while drawn); the form need not
        // be valid yet, the answer is checked
        if ask {
            let current = a.form();
            let (sentence, now) = (a.sentence.trim().to_string(), chrono::Local::now().format("%Y-%m-%d (%A) %H:%M").to_string());
            a.run(&ctx, t!("ui.settings.autoresponder.asking").into_owned(), move || {
                let bad = |e: String| t!("ui.settings.autoresponder.bad_ai", error = e).into_owned();
                let current_json = serde_json::to_string_pretty(&current).unwrap_or_default();
                let answer = crate::ai::autoresponder(&crate::ai::load(), &now, &current_json, &sentence).map_err(bad)?;
                let v = serde_json::from_str(&answer).map_err(|e| bad(e.to_string()))?;
                let mut r = Rules::over(&current, v).map_err(bad)?;
                // (who saved last is n8n's to say)
                r.updated_by = current.updated_by.clone();
                match r.problem() {
                    Some(p) => Err(bad(p)),
                    None => Ok((r, t!("ui.settings.autoresponder.ai_done").into_owned())),
                }
            });
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if a.rules.is_some() {
                if ui.add_enabled(!busy, egui::Button::new(RichText::new(t!("ui.settings.autoresponder.save")).color(Color32::WHITE)).fill(PRIMARY)).clicked() {
                    match a.edited() {
                        Ok(r) => {
                            let link = a.link.clone();
                            a.run(&ctx, t!("ui.settings.autoresponder.saving").into_owned(), move || crate::autoresponder::store(&link, &r).map(|r| (r, t!("ui.settings.autoresponder.saved").into_owned())));
                        }
                        Err(e) => a.said = Some(Err(e)),
                    }
                }
                if ui.add_enabled(!busy && a.link.is_set(), egui::Button::new(t!("ui.settings.autoresponder.reload"))).clicked() {
                    a.load(&ctx);
                }
            }
            if let Some((what, _)) = &a.busy {
                ui.spinner();
                ui.label(RichText::new(what.as_str()).small().color(MUTED()));
            }
            match &a.said {
                Some(Ok(m)) => {
                    ui.label(RichText::new(format!("✓  {m}")).small().color(Color32::from_rgb(0x2E, 0x9E, 0x5B)));
                }
                Some(Err(e)) => {
                    ui.add(egui::Label::new(RichText::new(e.as_str()).small().color(FLAG)).wrap());
                }
                None => {}
            }
        });
    }
}
