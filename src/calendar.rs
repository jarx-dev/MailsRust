use crate::model::{Event, Store};
use crate::pl;
use crate::theme::*;
use chrono::{Datelike, Duration, Months, NaiveDate, NaiveTime, Timelike};
use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Id, Pos2, Rect, Sense, Stroke, Ui, Vec2};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CalView {
    Month,
    Week,
}

pub enum CalAction {
    Select(NaiveDate),
    New(NaiveDate, Option<NaiveTime>),
    Edit(u64),
}

pub fn week_start(d: NaiveDate) -> NaiveDate {
    d - Duration::days(d.weekday().num_days_from_monday() as i64)
}

pub fn month_grid_start(d: NaiveDate) -> NaiveDate {
    week_start(d.with_day(1).unwrap())
}

pub fn shift(d: NaiveDate, view: CalView, dir: i32) -> NaiveDate {
    match view {
        CalView::Month => {
            if dir > 0 {
                d.checked_add_months(Months::new(1)).unwrap_or(d)
            } else {
                d.checked_sub_months(Months::new(1)).unwrap_or(d)
            }
        }
        CalView::Week => d + Duration::days(7 * dir as i64),
    }
}

pub fn events_on(store: &Store, d: NaiveDate) -> Vec<&Event> {
    let mut v: Vec<&Event> = store.events.iter().filter(|e| e.date == d).collect();
    v.sort_by_key(|e| (!e.all_day, e.start));
    v
}

/// Whole-day events (holidays, leave, trips) in a row over the hour grid: `days` columns from
/// `start`, to the right of `gutter`.
pub fn all_day_row(ui: &mut Ui, store: &Store, start: NaiveDate, days: i64, gutter: f32) -> Option<CalAction> {
    let per_day: Vec<Vec<&Event>> = (0..days).map(|i| events_on(store, start + Duration::days(i)).into_iter().filter(|e| e.all_day).collect()).collect();
    let rows = per_day.iter().map(|v| v.len()).max().unwrap_or(0);
    if rows == 0 {
        return None;
    }
    let chip_h = 20.0;
    let w = ui.available_width();
    let (area, _) = ui.allocate_exact_size(Vec2::new(w, rows as f32 * (chip_h + 2.0) + 4.0), Sense::hover());
    let cw = (w - gutter) / days as f32;
    let mut action = None;
    for (i, evs) in per_day.iter().enumerate() {
        for (k, e) in evs.iter().enumerate() {
            let r = Rect::from_min_size(Pos2::new(area.left() + gutter + cw * i as f32 + 3.0, area.top() + 2.0 + k as f32 * (chip_h + 2.0)), Vec2::new(cw - 6.0, chip_h));
            let resp = ui.interact(r, id(("allday", e.id)), Sense::click());
            let c = chip_color(e);
            let p = ui.painter();
            p.rect_filled(r, CornerRadius::same(3), c.gamma_multiply(if resp.hovered() { 0.30 } else { 0.18 }));
            p.rect_filled(Rect::from_min_size(r.min, Vec2::new(3.0, chip_h)), CornerRadius::same(1), c);
            line(p, Pos2::new(r.left() + 8.0, r.top() + 2.0), &e.title, 12.0, TEXT(), r.width() - 10.0);
            if resp.on_hover_text(&e.title).clicked() {
                action = Some(CalAction::Edit(e.id));
            }
        }
    }
    action
}

fn chip_color(e: &Event) -> Color32 {
    EVENT_COLORS[e.color % EVENT_COLORS.len()]
}

fn id(salt: impl std::hash::Hash + std::fmt::Debug) -> Id {
    #[allow(deprecated)]
    Id::new(salt)
}

pub fn month_grid(ui: &mut Ui, store: &Store, sel: NaiveDate, today: NaiveDate) -> Option<CalAction> {
    let mut action = None;
    let full = ui.available_rect_before_wrap();
    let header_h = 26.0;
    let cw = full.width() / 7.0;
    let ch = (full.height() - header_h) / 6.0;
    let painter = ui.painter_at(full);
    for (i, n) in pl::WD_MIN.iter().enumerate() {
        painter.text(
            Pos2::new(full.left() + cw * i as f32 + 8.0, full.top() + header_h / 2.0),
            Align2::LEFT_CENTER,
            *n,
            FontId::proportional(12.0),
            MUTED(),
        );
    }
    let start = month_grid_start(sel);
    for row in 0..6 {
        for col in 0..7 {
            let day = start + Duration::days(row * 7 + col);
            let rect = Rect::from_min_size(
                Pos2::new(full.left() + cw * col as f32, full.top() + header_h + ch * row as f32),
                Vec2::new(cw, ch),
            );
            let resp = ui.interact(rect, id(("cell", day)), Sense::click());
            let in_month = day.month() == sel.month();
            let bg = if day == sel {
                ACCENT_SOFT()
            } else if resp.hovered() {
                HOVER()
            } else if !in_month {
                crate::theme::SURFACE()
            } else {
                crate::theme::BG()
            };
            painter.rect_filled(rect, CornerRadius::ZERO, bg);
            painter.rect_stroke(rect, CornerRadius::ZERO, Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
            let num_pos = Pos2::new(rect.left() + 16.0, rect.top() + 14.0);
            if day == today {
                painter.circle_filled(num_pos, 11.0, ACCENT());
            }
            painter.text(
                num_pos,
                Align2::CENTER_CENTER,
                day.day().to_string(),
                FontId::proportional(12.5),
                if day == today {
                    Color32::WHITE
                } else if in_month {
                    TEXT()
                } else {
                    crate::theme::MUTED()
                },
            );
            let evs = events_on(store, day);
            let chip_h = 19.0;
            let max_chips = (((ch - 30.0) / (chip_h + 2.0)).floor() as usize).max(1);
            let mut y = rect.top() + 28.0;
            for (k, e) in evs.iter().enumerate() {
                if k + 1 == max_chips && evs.len() > max_chips {
                    painter.text(
                        Pos2::new(rect.left() + 8.0, y + 2.0),
                        Align2::LEFT_TOP,
                        format!("+{} więcej", evs.len() - k),
                        FontId::proportional(11.5),
                        MUTED(),
                    );
                    break;
                }
                let chip = Rect::from_min_size(Pos2::new(rect.left() + 4.0, y), Vec2::new(cw - 8.0, chip_h));
                let cr = ui.interact(chip, id(("chip", e.id)), Sense::click());
                let c = chip_color(e);
                painter.rect_filled(chip, CornerRadius::same(3), c.gamma_multiply(if cr.hovered() { 0.30 } else { 0.16 }));
                painter.rect_filled(Rect::from_min_size(chip.min, Vec2::new(3.0, chip_h)), CornerRadius::same(1), c);
                line(
                    &painter,
                    Pos2::new(chip.left() + 7.0, chip.top() + 2.0),
                    &if e.all_day { e.title.clone() } else { format!("{} {}", e.start.format("%H:%M"), e.title) },
                    11.5,
                    TEXT(),
                    chip.width() - 10.0,
                );
                if cr.clicked() {
                    action = Some(CalAction::Edit(e.id));
                }
                y += chip_h + 2.0;
            }
            if action.is_none() {
                if resp.double_clicked() {
                    action = Some(CalAction::New(day, None));
                } else if resp.clicked() {
                    action = Some(CalAction::Select(day));
                }
            }
        }
    }
    ui.allocate_rect(full, Sense::hover());
    action
}

/// Hour grid for several days side by side (1 = day timeline, 7 = week).
fn hour_columns(
    ui: &mut Ui,
    salt: &str,
    store: &Store,
    start: NaiveDate,
    days: i64,
    today: NaiveDate,
    hour_h: f32,
    gutter: f32,
) -> Option<CalAction> {
    let mut action = None;
    egui::ScrollArea::vertical()
        .id_salt(("hours", salt))
        .auto_shrink([false, false])
        .vertical_scroll_offset(8.0 * hour_h)
        .show(ui, |ui| {
            let total = hour_h * 24.0;
            let (area, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), total), Sense::hover());
            let cw = (area.width() - gutter) / days as f32;
            let p = ui.painter();
            for h in 0..24u32 {
                let y = area.top() + hour_h * h as f32;
                p.hline(area.left() + gutter..=area.right(), y, Stroke::new(1.0, BORDER()));
                if hour_h > 50.0 {
                    p.hline(
                        area.left() + gutter..=area.right(),
                        y + hour_h / 2.0,
                        Stroke::new(1.0, crate::theme::SURFACE()),
                    );
                }
                let w = p
                    .text(Pos2::new(area.left() + 6.0, y + 3.0), Align2::LEFT_TOP, format!("{h:02}"), FontId::proportional(17.0), MUTED())
                    .width();
                p.text(Pos2::new(area.left() + 8.0 + w, y + 3.0), Align2::LEFT_TOP, "00", FontId::proportional(10.0), MUTED());
            }
            // the "now" line first: the events lie over it, so it never crosses their text
            if today >= start && today < start + Duration::days(days) {
                let now = chrono::Local::now().time();
                let y = area.top() + (now.hour() as f32 + now.minute() as f32 / 60.0) * hour_h;
                let x = area.left() + gutter + cw * (today - start).num_days() as f32;
                p.hline(x..=x + cw, y, Stroke::new(1.5, FLAG));
                p.circle_filled(Pos2::new(x, y), 3.5, FLAG);
            }
            for i in 0..days {
                let day = start + Duration::days(i);
                let x = area.left() + gutter + cw * i as f32;
                let col_rect = Rect::from_min_size(Pos2::new(x, area.top()), Vec2::new(cw, total));
                if days > 1 {
                    p.vline(x, area.y_range(), Stroke::new(1.0, BORDER()));
                }
                let resp = ui.interact(col_rect, id((salt, "col", day)), Sense::click());
                if resp.double_clicked() {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        let hour = ((pos.y - area.top()) / hour_h).floor().clamp(0.0, 23.0) as u32;
                        action = Some(CalAction::New(day, NaiveTime::from_hms_opt(hour, 0, 0)));
                    }
                } else if resp.clicked() && days > 1 {
                    action = Some(CalAction::Select(day));
                }
                for e in events_on(store, day).into_iter().filter(|e| !e.all_day) {
                    let mins = e.start.hour() as f32 * 60.0 + e.start.minute() as f32;
                    let y0 = area.top() + mins / 60.0 * hour_h;
                    let h = (e.minutes as f32 / 60.0 * hour_h).max(22.0);
                    let r = Rect::from_min_size(Pos2::new(x + 3.0, y0 + 1.0), Vec2::new(cw - 6.0, h - 2.0));
                    let er = ui.interact(r, id((salt, "ev", e.id)), Sense::click());
                    let c = chip_color(e);
                    p.rect_filled(r, CornerRadius::same(3), BG());
                    p.rect_filled(r, CornerRadius::same(3), c.gamma_multiply(if er.hovered() { 0.22 } else { 0.12 }));
                    p.rect_filled(Rect::from_min_size(r.min, Vec2::new(3.0, r.height())), CornerRadius::same(1), c);
                    let end = e.start + Duration::minutes(e.minutes);
                    line_f(p, Pos2::new(r.left() + 9.0, r.top() + 4.0), &e.title, semibold(12.5), TEXT(), r.width() - 12.0);
                    if h > 40.0 {
                        let sub = if e.location.is_empty() {
                            format!("🕓 {} – {}", e.start.format("%H:%M"), end.format("%H:%M"))
                        } else {
                            format!("🕓 {} – {}  ·  {}", e.start.format("%H:%M"), end.format("%H:%M"), e.location)
                        };
                        line(p, Pos2::new(r.left() + 9.0, r.top() + 22.0), &sub, 11.5, MUTED(), r.width() - 12.0);
                    }
                    if er.clicked() {
                        action = Some(CalAction::Edit(e.id));
                    }
                }
            }
        });
    action
}

pub fn week_grid(ui: &mut Ui, store: &Store, sel: NaiveDate, today: NaiveDate) -> Option<CalAction> {
    let mut action = None;
    let start = week_start(sel);
    let gutter = 52.0;
    let header_h = 40.0;
    let w = ui.available_width();
    let cw = (w - gutter) / 7.0;
    let (hdr, _) = ui.allocate_exact_size(Vec2::new(w, header_h), Sense::hover());
    let p = ui.painter();
    for i in 0..7 {
        let day = start + Duration::days(i);
        let x = hdr.left() + gutter + cw * i as f32;
        let r = Rect::from_min_size(Pos2::new(x, hdr.top()), Vec2::new(cw, header_h));
        if ui.interact(r, id(("whdr", day)), Sense::click()).clicked() {
            action = Some(CalAction::Select(day));
        }
        let col = if day == today { ACCENT() } else { TEXT() };
        p.text(Pos2::new(x + 8.0, hdr.top() + 4.0), Align2::LEFT_TOP, pl::wd_short(day), FontId::proportional(11.5), MUTED());
        p.text(Pos2::new(x + 8.0, hdr.top() + 18.0), Align2::LEFT_TOP, day.day().to_string(), FontId::proportional(16.0), col);
        if day == sel {
            p.hline(r.x_range().shrink(4.0), r.bottom() - 1.5, Stroke::new(3.0, ACCENT()));
        }
    }
    p.hline(hdr.x_range(), hdr.bottom(), Stroke::new(1.0, BORDER()));
    if let Some(a) = all_day_row(ui, store, start, 7, gutter) {
        action = Some(a);
    }
    if let Some(a) = hour_columns(ui, "week", store, start, 7, today, 46.0, gutter) {
        action = Some(a);
    }
    action
}

/// One day as a timeline, for the right-hand side bar.
pub fn day_timeline(ui: &mut Ui, store: &Store, day: NaiveDate, today: NaiveDate) -> Option<CalAction> {
    hour_columns(ui, "day", store, day, 1, today, 60.0, 56.0)
}

/// Small month calendar. Returns a clicked day.
pub fn mini_month(ui: &mut Ui, salt: &str, store: &Store, shown: NaiveDate, sel: NaiveDate, today: NaiveDate) -> Option<NaiveDate> {
    let mut out = None;
    let w = ui.available_width();
    let cell = (w / 7.0).floor().min(34.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(cell * 7.0, cell * 0.8 + cell * 6.0), Sense::hover());
    let p = ui.painter();
    for (i, n) in pl::WD_MIN.iter().enumerate() {
        p.text(
            Pos2::new(rect.left() + cell * (i as f32 + 0.5), rect.top() + cell * 0.4),
            Align2::CENTER_CENTER,
            *n,
            FontId::proportional(10.5),
            MUTED(),
        );
    }
    let start = month_grid_start(shown);
    for k in 0..42 {
        let day = start + Duration::days(k);
        let c = Pos2::new(
            rect.left() + cell * ((k % 7) as f32 + 0.5),
            rect.top() + cell * 0.8 + cell * ((k / 7) as f32 + 0.5),
        );
        let r = Rect::from_center_size(c, Vec2::splat(cell - 2.0));
        let resp = ui.interact(r, id(("mini", salt, day)), Sense::click());
        if day == today {
            p.circle_filled(c, cell * 0.42, ACCENT());
        } else if day == sel {
            p.circle_filled(c, cell * 0.42, ACCENT_SOFT());
        } else if resp.hovered() {
            p.circle_filled(c, cell * 0.42, HOVER());
        }
        let col = if day == today {
            Color32::WHITE
        } else if day.month() != shown.month() {
            crate::theme::MUTED()
        } else {
            TEXT()
        };
        p.text(c, Align2::CENTER_CENTER, day.day().to_string(), FontId::proportional(11.5), col);
        if day != today && store.events.iter().any(|e| e.date == day) {
            p.circle_filled(Pos2::new(c.x, c.y + cell * 0.32), 1.6, ACCENT());
        }
        if resp.clicked() {
            out = Some(day);
        }
    }
    out
}

pub struct EventEditor {
    pub id: Option<u64>,
    pub title: String,
    pub date: NaiveDate,
    pub start: String,
    pub minutes: i64,
    pub location: String,
    pub color: usize,
    pub error: Option<String>,
    pub picking: bool,
    pub shown: NaiveDate,
}

impl EventEditor {
    pub fn new(date: NaiveDate, time: Option<NaiveTime>) -> Self {
        Self {
            id: None,
            title: String::new(),
            date,
            start: time
                .unwrap_or_else(|| NaiveTime::from_hms_opt(9, 0, 0).unwrap())
                .format("%H:%M")
                .to_string(),
            minutes: 60,
            location: String::new(),
            color: 0,
            error: None,
            picking: false,
            shown: date,
        }
    }

    pub fn from_event(e: &Event) -> Self {
        Self {
            id: Some(e.id),
            title: e.title.clone(),
            date: e.date,
            start: e.start.format("%H:%M").to_string(),
            minutes: e.minutes,
            location: e.location.clone(),
            color: e.color,
            error: None,
            picking: false,
            shown: e.date,
        }
    }
}

pub enum EditorResult {
    Keep,
    Close,
    Save,
    Delete,
}

pub fn event_editor(ctx: &egui::Context, ed: &mut EventEditor, store: &Store) -> EditorResult {
    let mut res = EditorResult::Keep;
    let mut open = true;
    let title = if ed.id.is_some() { "Edytuj wydarzenie" } else { "Nowe wydarzenie" };
    crate::theme::dialog(ctx, "event_editor", title, 420.0, &mut open, |ui| {
            egui::Grid::new("evgrid").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                ui.label("Tytuł");
                ui.add(egui::TextEdit::singleline(&mut ed.title).hint_text("Co się dzieje?").desired_width(260.0));
                ui.end_row();
                ui.label("Data");
                ui.horizontal(|ui| {
                    if ui.small_button("‹").clicked() {
                        ed.date -= Duration::days(1);
                    }
                    if ui.button(format!("📅 {} {}", pl::wd_short(ed.date), pl::day_month(ed.date))).clicked() {
                        ed.picking = !ed.picking;
                        ed.shown = ed.date;
                    }
                    if ui.small_button("›").clicked() {
                        ed.date += Duration::days(1);
                    }
                });
                ui.end_row();
                if ed.picking {
                    ui.label("");
                    ui.vertical(|ui| {
                        ui.set_width(220.0);
                        ui.horizontal(|ui| {
                            if ui.small_button("‹").clicked() {
                                ed.shown = shift(ed.shown, CalView::Month, -1);
                            }
                            ui.label(pl::month_year(ed.shown));
                            if ui.small_button("›").clicked() {
                                ed.shown = shift(ed.shown, CalView::Month, 1);
                            }
                        });
                        let today = chrono::Local::now().date_naive();
                        if let Some(d) = mini_month(ui, "picker", store, ed.shown, ed.date, today) {
                            ed.date = d;
                            ed.picking = false;
                        }
                    });
                    ui.end_row();
                }
                ui.label("Początek");
                ui.add(egui::TextEdit::singleline(&mut ed.start).hint_text("09:00").desired_width(70.0));
                ui.end_row();
                ui.label("Czas trwania");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut ed.minutes).range(5..=1440).speed(5).suffix(" min"));
                    for (lbl, m) in [("30 min", 30), ("1 godz.", 60), ("2 godz.", 120)] {
                        if ui.small_button(lbl).clicked() {
                            ed.minutes = m;
                        }
                    }
                });
                ui.end_row();
                ui.label("Miejsce");
                ui.add(egui::TextEdit::singleline(&mut ed.location).hint_text("Opcjonalnie").desired_width(260.0));
                ui.end_row();
                ui.label("Kolor");
                ui.horizontal(|ui| {
                    for (i, c) in EVENT_COLORS.iter().enumerate() {
                        let (r, resp) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::click());
                        ui.painter().circle_filled(r.center(), 8.0, *c);
                        if ed.color == i {
                            ui.painter().circle_stroke(r.center(), 10.0, Stroke::new(2.0, TEXT()));
                        }
                        if resp.on_hover_text(EVENT_COLOR_NAMES[i]).clicked() {
                            ed.color = i;
                        }
                    }
                });
                ui.end_row();
            });
            if let Some(err) = &ed.error {
                ui.colored_label(FLAG, err);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(egui::RichText::new("Zapisz").color(Color32::WHITE)).fill(ACCENT())).clicked() {
                    res = EditorResult::Save;
                }
                if ui.button("Anuluj").clicked() {
                    res = EditorResult::Close;
                }
                if ed.id.is_some() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("🗑 Usuń").clicked() {
                            res = EditorResult::Delete;
                        }
                    });
                }
            });
        });
    if !open {
        res = EditorResult::Close;
    }
    res
}

pub fn parse_time(s: &str) -> Option<NaiveTime> {
    let s = s.trim();
    NaiveTime::parse_from_str(s, "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(s, "%H.%M"))
        .ok()
        .or_else(|| s.parse::<u32>().ok().and_then(|h| NaiveTime::from_hms_opt(h, 0, 0)))
}
