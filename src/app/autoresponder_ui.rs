use super::*;
use crate::autoresponder::{iso_day, shown_day, Link, Rules};

type Answer = Result<(Rules, String), String>;

/// The autoresponder window (☰ → Autoresponder): its state lives while the window is open.
pub(super) struct AutoUi {
    link: Link,
    /// the settings being edited; None until n8n answered
    rules: Option<Rules>,
    /// the days as typed (DD-MM-RRRR)
    from: String,
    to: String,
    /// what the person asks the AI for
    sentence: String,
    /// work in the background: what it is, and its answer (the settings and what to say)
    busy: Option<(String, Receiver<Answer>)>,
    said: Option<Result<String, String>>,
}

impl AutoUi {
    pub(super) fn new(ctx: &egui::Context) -> Self {
        let mut a = Self { link: crate::autoresponder::load(), rules: None, from: String::new(), to: String::new(), sentence: String::new(), busy: None, said: None };
        if std::env::var("POCZTA_START").as_deref() == Ok("autoresponder") && crate::unseen() {
            // a test picture: a filled form without n8n
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
        } else if a.link.is_set() {
            let link = a.link.clone();
            a.run(ctx, t!("ui.settings.autoresponder.loading").into_owned(), move || crate::autoresponder::fetch(&link).map(|r| (r, t!("ui.settings.autoresponder.loaded").into_owned())));
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

    /// The form as settings to send: its days back in RRRR-MM-DD, checked.
    fn edited(&self) -> Result<Rules, String> {
        let mut r = self.rules.clone().unwrap_or_default();
        r.date_start = iso_day(&self.from).ok_or_else(|| t!("ui.settings.autoresponder.bad_dates").into_owned())?;
        r.date_end = iso_day(&self.to).ok_or_else(|| t!("ui.settings.autoresponder.bad_dates").into_owned())?;
        match r.problem() {
            Some(p) => Err(p),
            None => Ok(r),
        }
    }
}

impl MailApp {
    pub(super) fn autoresponder_ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let a = self.auto_ui.get_or_insert_with(|| AutoUi::new(&ctx));
        if let Some(answer) = a.busy.as_ref().and_then(|(_, rx)| rx.try_recv().ok()) {
            a.busy = None;
            match answer {
                Ok((r, m)) => {
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
                ui.add(egui::TextEdit::singleline(&mut a.link.url).hint_text("https://…/webhook/poczta-autoresponder").desired_width(380.0));
                ui.end_row();
                ui.label(t!("ui.settings.autoresponder.token"));
                ui.add(egui::TextEdit::singleline(&mut a.link.token).password(true).desired_width(380.0));
                ui.end_row();
            });
            if ui.add_enabled(!busy && a.link.is_set(), egui::Button::new(t!("ui.settings.autoresponder.connect")).small()).clicked() {
                crate::autoresponder::save(&a.link);
                let link = a.link.clone();
                a.run(&ctx, t!("ui.settings.autoresponder.loading").into_owned(), move || crate::autoresponder::fetch(&link).map(|r| (r, t!("ui.settings.autoresponder.loaded").into_owned())));
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
                        ui.add(egui::TextEdit::singleline(&mut a.from).hint_text("DD-MM-RRRR").desired_width(96.0));
                        ui.label("–");
                        ui.add(egui::TextEdit::singleline(&mut a.to).hint_text("DD-MM-RRRR").desired_width(96.0));
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
        // (asked for after the form, which holds `a.rules` borrowed while drawn)
        if ask {
            match a.edited() {
                Ok(current) => {
                    let (sentence, now) = (a.sentence.trim().to_string(), chrono::Local::now().format("%Y-%m-%d (%A) %H:%M").to_string());
                    let keep_by = current.updated_by.clone();
                    a.run(&ctx, t!("ui.settings.autoresponder.asking").into_owned(), move || {
                        let current_json = serde_json::to_string_pretty(&current).unwrap_or_default();
                        let answer = crate::ai::autoresponder(&crate::ai::load(), &now, &current_json, &sentence)?;
                        let mut r: Rules = serde_json::from_str(&answer).map_err(|e| t!("ui.settings.autoresponder.bad_ai", error = e.to_string()).into_owned())?;
                        if let Some(p) = r.problem() {
                            return Err(t!("ui.settings.autoresponder.bad_ai", error = p).into_owned());
                        }
                        r.updated_by = keep_by;
                        Ok((r, t!("ui.settings.autoresponder.ai_done").into_owned()))
                    });
                }
                Err(e) => a.said = Some(Err(e)),
            }
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
                    let link = a.link.clone();
                    a.run(&ctx, t!("ui.settings.autoresponder.loading").into_owned(), move || crate::autoresponder::fetch(&link).map(|r| (r, t!("ui.settings.autoresponder.loaded").into_owned())));
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
