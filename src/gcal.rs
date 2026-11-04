//! Google Calendar without a Google Cloud project: the calendar's "secret address in iCal
//! format" (an .ics file Google keeps up to date) read every few minutes. Events come one way,
//! from Google to the app, and are shown read-only.

use crate::model::Event;
use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc, Weekday};
use std::collections::{HashMap, HashSet};

/// Ids of Google events start here (the app's own events count from 1).
const ID_BASE: u64 = 1 << 60;

fn agent() -> Result<ureq::Agent, String> {
    let tls = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
    Ok(ureq::AgentBuilder::new()
        .tls_connector(std::sync::Arc::new(tls))
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(60))
        .build())
}

/// The calendar file behind an address (a web address, or a file on disk for tests).
fn download(url: &str) -> Result<String, String> {
    let url = url.trim();
    let url = url.strip_prefix("webcal://").map(|r| format!("https://{r}")).unwrap_or_else(|| url.to_string());
    if !url.starts_with("http://") && !url.starts_with("https://") {
        let path = url.strip_prefix("file:///").unwrap_or(&url);
        return std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"));
    }
    let resp = agent()?.get(&url).call().map_err(|e| match e {
        ureq::Error::Status(404, _) | ureq::Error::Status(403, _) | ureq::Error::Status(401, _) => {
            "Google nie udostępnia kalendarza pod tym adresem (adres zmieniony albo wyłączony)".to_string()
        }
        ureq::Error::Status(code, _) => format!("Google odpowiedział {code}"),
        other => format!("brak połączenia: {other}"),
    })?;
    let text = resp.into_string().map_err(|e| e.to_string())?;
    if !text.contains("BEGIN:VCALENDAR") {
        return Err("pod tym adresem nie ma kalendarza (to musi być „Tajny adres w formacie iCal”)".into());
    }
    Ok(text)
}

/// One line of the calendar file: name, parameters, value.
struct Prop {
    name: String,
    params: HashMap<String, String>,
    value: String,
}

fn parse_line(line: &str) -> Option<Prop> {
    // the value starts at the first ':' outside quotes
    let mut quoted = false;
    let mut split = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ':' if !quoted => {
                split = Some(i);
                break;
            }
            _ => {}
        }
    }
    let i = split?;
    let (head, value) = (&line[..i], &line[i + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?.trim().to_ascii_uppercase();
    let params = parts
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.trim().to_ascii_uppercase(), v.trim().trim_matches('"').to_string()))
        .collect();
    Some(Prop { name, params, value: value.to_string() })
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A moment of the calendar file: a whole day, or a time on this computer's clock.
#[derive(Clone, Copy, Debug, PartialEq)]
enum When {
    Day(NaiveDate),
    At(NaiveDateTime),
}

impl When {
    fn date(self) -> NaiveDate {
        match self {
            When::Day(d) => d,
            When::At(t) => t.date(),
        }
    }
}

fn parse_when(p: &Prop) -> Option<When> {
    parse_value(&p.value, p.params.get("VALUE").map(|v| v == "DATE").unwrap_or(false))
}

fn parse_value(v: &str, date_only: bool) -> Option<When> {
    let v = v.trim();
    if date_only || v.len() == 8 {
        return NaiveDate::parse_from_str(&v[..8.min(v.len())], "%Y%m%d").ok().map(When::Day);
    }
    let t = NaiveDateTime::parse_from_str(v.trim_end_matches('Z'), "%Y%m%dT%H%M%S").ok()?;
    if v.ends_with('Z') {
        // Universal time → this computer's time
        Some(When::At(Utc.from_utc_datetime(&t).with_timezone(&Local).naive_local()))
    } else {
        // a named time zone (nearly always the calendar's own) or "floating": as written
        Some(When::At(t))
    }
}

/// "PT1H30M", "P1D", "P2W" → minutes.
fn parse_duration(s: &str) -> Option<i64> {
    let s = s.trim().trim_start_matches(['+', '-']);
    let s = s.strip_prefix('P')?;
    let (mut total, mut num) = (0i64, String::new());
    for c in s.chars() {
        match c {
            'T' => {}
            '0'..='9' => num.push(c),
            _ => {
                let n: i64 = num.parse().unwrap_or(0);
                num.clear();
                total += n * match c {
                    'W' => 7 * 24 * 60,
                    'D' => 24 * 60,
                    'H' => 60,
                    'M' => 1,
                    _ => 0,
                };
            }
        }
    }
    Some(total)
}

/// A repeat rule (RRULE), the parts Google uses.
#[derive(Default, Debug)]
struct Rule {
    freq: String,
    interval: i64,
    count: Option<usize>,
    until: Option<NaiveDate>,
    by_day: Vec<(i32, Weekday)>,
    by_month_day: Vec<i32>,
}

fn weekday(s: &str) -> Option<Weekday> {
    Some(match s {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

fn parse_rule(s: &str) -> Rule {
    let mut r = Rule { interval: 1, ..Default::default() };
    for part in s.split(';') {
        let Some((k, v)) = part.split_once('=') else { continue };
        match k.to_ascii_uppercase().as_str() {
            "FREQ" => r.freq = v.to_ascii_uppercase(),
            "INTERVAL" => r.interval = v.parse().unwrap_or(1).max(1),
            "COUNT" => r.count = v.parse().ok(),
            "UNTIL" => r.until = parse_value(v, false).or_else(|| parse_value(v, true)).map(When::date),
            "BYDAY" => {
                r.by_day = v
                    .split(',')
                    .filter_map(|d| {
                        let d = d.trim();
                        let (n, wd) = d.split_at(d.len().saturating_sub(2));
                        Some((n.parse().unwrap_or(0), weekday(wd)?))
                    })
                    .collect()
            }
            "BYMONTHDAY" => r.by_month_day = v.split(',').filter_map(|d| d.trim().parse().ok()).collect(),
            _ => {}
        }
    }
    r
}

/// The `n`-th `wd` of a month (negative: from the end).
fn nth_weekday(year: i32, month: u32, wd: Weekday, n: i32) -> Option<NaiveDate> {
    if n > 0 {
        NaiveDate::from_weekday_of_month_opt(year, month, wd, n as u8)
    } else {
        let next = if month == 12 { NaiveDate::from_ymd_opt(year + 1, 1, 1)? } else { NaiveDate::from_ymd_opt(year, month + 1, 1)? };
        let mut d = next - Duration::days(1);
        while d.weekday() != wd {
            d -= Duration::days(1);
        }
        Some(d - Duration::weeks((-n - 1) as i64))
    }
}

fn add_months(d: NaiveDate, m: i64) -> (i32, u32) {
    let total = d.year() as i64 * 12 + d.month0() as i64 + m;
    ((total / 12) as i32, (total % 12) as u32 + 1)
}

/// The days a repeating event happens on, from `start` up to `to` (all of them counted for COUNT).
fn expand(start: NaiveDate, r: &Rule, to: NaiveDate) -> Vec<NaiveDate> {
    let mut out = Vec::new();
    let mut counted = 0usize;
    let end = r.until.map(|u| u.min(to)).unwrap_or(to);
    let done = |counted: usize| r.count.is_some_and(|c| counted >= c);
    let mut k = 0i64;
    while k < 20_000 {
        let mut days: Vec<NaiveDate> = match r.freq.as_str() {
            "DAILY" => vec![start + Duration::days(k * r.interval)],
            "WEEKLY" => {
                let week = start - Duration::days(start.weekday().num_days_from_monday() as i64) + Duration::weeks(k * r.interval);
                if r.by_day.is_empty() {
                    vec![week + Duration::days(start.weekday().num_days_from_monday() as i64)]
                } else {
                    r.by_day.iter().map(|(_, wd)| week + Duration::days(wd.num_days_from_monday() as i64)).collect()
                }
            }
            "MONTHLY" => {
                let (y, m) = add_months(start, k * r.interval);
                if !r.by_day.is_empty() {
                    r.by_day.iter().filter_map(|(n, wd)| if *n == 0 { None } else { nth_weekday(y, m, *wd, *n) }).collect()
                } else if !r.by_month_day.is_empty() {
                    r.by_month_day.iter().filter_map(|d| if *d > 0 { NaiveDate::from_ymd_opt(y, m, *d as u32) } else { None }).collect()
                } else {
                    NaiveDate::from_ymd_opt(y, m, start.day()).into_iter().collect()
                }
            }
            "YEARLY" => NaiveDate::from_ymd_opt(start.year() + (k * r.interval) as i32, start.month(), start.day()).into_iter().collect(),
            _ => vec![start],
        };
        days.sort();
        let first = days.first().copied();
        for d in days {
            if d < start {
                continue;
            }
            if d > end || done(counted) {
                return out;
            }
            counted += 1;
            out.push(d);
        }
        if r.freq.is_empty() || first.is_some_and(|f| f > end) || !matches!(r.freq.as_str(), "DAILY" | "WEEKLY" | "MONTHLY" | "YEARLY") {
            break;
        }
        k += 1;
    }
    out
}

fn stable_id(uid: &str, day: NaiveDate, n: usize) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (uid, day, n).hash(&mut h);
    ID_BASE | (h.finish() >> 4)
}

/// The events of one calendar file between `from` and `to`, in colour `color`.
pub fn parse(ics: &str, color: usize, from: NaiveDate, to: NaiveDate) -> Vec<Event> {
    // lines that continue on the next line (starting with a space) joined back
    let text = ics.replace("\r\n", "\n").replace("\n ", "").replace("\n\t", "");
    // every event's lines
    let mut blocks: Vec<Vec<Prop>> = Vec::new();
    let mut cur: Option<Vec<Prop>> = None;
    for line in text.lines() {
        let l = line.trim_end();
        if l.eq_ignore_ascii_case("BEGIN:VEVENT") {
            cur = Some(Vec::new());
        } else if l.eq_ignore_ascii_case("END:VEVENT") {
            if let Some(b) = cur.take() {
                blocks.push(b);
            }
        } else if let Some(b) = cur.as_mut() {
            if let Some(p) = parse_line(l) {
                b.push(p);
            }
        }
    }
    // changed single days of repeating events: (uid, day) → handled by their own block
    let mut moved: HashSet<(String, NaiveDate)> = HashSet::new();
    for b in &blocks {
        let uid = b.iter().find(|p| p.name == "UID").map(|p| p.value.clone()).unwrap_or_default();
        if let Some(rid) = b.iter().find(|p| p.name == "RECURRENCE-ID").and_then(parse_when) {
            moved.insert((uid, rid.date()));
        }
    }
    let mut out = Vec::new();
    for b in &blocks {
        let get = |n: &str| b.iter().find(|p| p.name == n);
        if get("STATUS").is_some_and(|p| p.value.eq_ignore_ascii_case("CANCELLED")) {
            continue;
        }
        let Some(start) = get("DTSTART").and_then(parse_when) else { continue };
        let uid = get("UID").map(|p| p.value.clone()).unwrap_or_default();
        let title = get("SUMMARY").map(|p| unescape(&p.value)).filter(|t| !t.trim().is_empty()).unwrap_or_else(|| "(bez tytułu)".into());
        let location = get("LOCATION").map(|p| unescape(&p.value)).unwrap_or_default();
        let end = get("DTEND").and_then(parse_when);
        let dur = get("DURATION").and_then(|p| parse_duration(&p.value));
        // length: minutes for timed events, days for whole-day ones
        let (all_day, minutes, days) = match (start, end) {
            (When::Day(s), Some(When::Day(e))) => (true, 24 * 60, (e - s).num_days().clamp(1, 31)),
            (When::Day(_), _) => (true, 24 * 60, dur.map(|m| (m / 1440).clamp(1, 31)).unwrap_or(1)),
            (When::At(s), Some(When::At(e))) => (false, (e - s).num_minutes().max(0), 1),
            (When::At(_), _) => (false, dur.unwrap_or(60), 1),
        };
        let time = match start {
            When::At(t) => t.time(),
            When::Day(_) => NaiveTime::MIN,
        };
        let first_day = start.date();
        let excluded: HashSet<NaiveDate> = b
            .iter()
            .filter(|p| p.name == "EXDATE")
            .flat_map(|p| {
                let date_only = p.params.get("VALUE").map(|v| v == "DATE").unwrap_or(false);
                p.value.split(',').filter_map(move |v| parse_value(v, date_only)).map(When::date).collect::<Vec<_>>()
            })
            .collect();
        let is_override = get("RECURRENCE-ID").is_some();
        let starts: Vec<NaiveDate> = match get("RRULE") {
            Some(r) if !is_override => expand(first_day, &parse_rule(&r.value), to),
            _ => vec![first_day],
        };
        for d in starts {
            if excluded.contains(&d) || (!is_override && moved.contains(&(uid.clone(), d))) {
                continue;
            }
            for k in 0..days {
                let day = d + Duration::days(k);
                if day < from || day > to {
                    continue;
                }
                // a timed event running past midnight is shown until the end of its first day
                let len = if all_day { 24 * 60 } else { minutes.min(24 * 60 - (time.hour_minutes())).max(15) };
                out.push(Event {
                    id: stable_id(&uid, day, k as usize),
                    title: title.clone(),
                    date: day,
                    start: time,
                    minutes: len,
                    location: location.clone(),
                    color,
                    google: true,
                    all_day,
                    gid: String::new(),
                });
            }
        }
    }
    out
}

trait HourMinutes {
    fn hour_minutes(&self) -> i64;
}

impl HourMinutes for NaiveTime {
    fn hour_minutes(&self) -> i64 {
        use chrono::Timelike;
        self.hour() as i64 * 60 + self.minute() as i64
    }
}

/// Colours given to the calendars one after another.
const COLORS: [usize; 6] = [1, 3, 4, 2, 5, 0];

/// All calendars (one address per line): their events from four months back to a year ahead.
pub fn sync(urls: &[String], key: &str) -> Result<Vec<Event>, String> {
    let today = Local::now().date_naive();
    let (from, to) = (today - Duration::days(120), today + Duration::days(400));
    let mut all = Vec::new();
    let mut errors = Vec::new();
    for (i, u) in urls.iter().enumerate() {
        let got = if is_script(u) {
            let range = [
                ("action", "list".to_string()),
                ("from", ms_of(from.and_time(NaiveTime::MIN)).to_string()),
                ("to", ms_of(to.and_time(NaiveTime::MIN)).to_string()),
            ];
            ask_script(u, key, &range).map(|v| parse_script(&v, i))
        } else {
            download(u).map(|text| parse(&text, COLORS[i % COLORS.len()], from, to))
        };
        match got {
            Ok(evs) => all.extend(evs),
            Err(e) => errors.push(if urls.len() > 1 { format!("kalendarz {}: {e}", i + 1) } else { e }),
        }
    }
    if !errors.is_empty() && all.is_empty() {
        return Err(errors.join("; "));
    }
    if !errors.is_empty() {
        crate::diag::log(format!("Google calendar: {}", errors.join("; ")));
    }
    Ok(all)
}

/// The addresses written in the settings (one per line).
pub fn urls(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
}

// ------------------------------------------------------------ Google Apps Script (two ways, no Cloud project)

/// A Google Apps Script web app (script.google.com … /exec) instead of a calendar file.
pub fn is_script(url: &str) -> bool {
    url.contains("script.google.com/macros/") || url.contains("script.googleusercontent.com/")
}

/// The script the person pastes on script.google.com (with this computer's secret key in it).
/// It lists the events of the calendars shown in Google Calendar, and saves and deletes the
/// events made in this app (in the main calendar).
pub fn script_text(key: &str) -> String {
    format!(
        r#"// Poczta — synchronizacja Kalendarza i Kontaktów Google (wklej całość, zapisz i wdróż jako aplikację internetową).
// Kontakty potrzebują usługi „People API”: w edytorze po lewej „Usługi” (+) → People API → Dodaj.
const KLUCZ = '{key}';

function doGet(e) {{
  const p = e.parameter;
  if (p.key !== KLUCZ) return out({{ error: 'zły klucz' }});
  try {{
    if (p.action === 'save') return out(save(p));
    if (p.action === 'delete') return out(remove(p));
    if (p.action === 'contacts') return out(contacts());
    if (p.action === 'saveGroup') return out(saveGroup(p));
    if (p.action === 'deleteGroup') return out(deleteGroup(p));
    if (p.action === 'savePerson') return out(savePerson(JSON.parse(p.data)));
    if (p.action === 'deletePerson') return out(deletePerson(p));
    return out(list(p));
  }} catch (err) {{
    return out({{ error: String(err) }});
  }}
}}

function list(p) {{
  const from = new Date(Number(p.from)), to = new Date(Number(p.to));
  const main = CalendarApp.getDefaultCalendar().getId();
  const cals = CalendarApp.getAllCalendars().filter(function (c) {{ return !c.isHidden() && c.isSelected(); }});
  const events = [];
  cals.forEach(function (c, i) {{
    c.getEvents(from, to).forEach(function (ev) {{
      try {{ if (ev.getMyStatus() === CalendarApp.GuestStatus.NO) return; }} catch (x) {{}}
      events.push({{
        id: ev.getId(), cal: i, title: ev.getTitle(), location: ev.getLocation(),
        allDay: ev.isAllDayEvent(), start: ev.getStartTime().getTime(), end: ev.getEndTime().getTime(),
        editable: c.getId() === main && !ev.isRecurringEvent()
      }});
    }});
  }});
  return {{ events: events, calendars: cals.map(function (c) {{ return c.getName(); }}) }};
}}

function save(p) {{
  const cal = CalendarApp.getDefaultCalendar();
  const s = new Date(Number(p.start)), en = new Date(Number(p.end));
  let ev = p.id ? cal.getEventById(p.id) : null;
  if (ev) {{
    ev.setTitle(p.title);
    ev.setLocation(p.location || '');
    ev.setTime(s, en);
  }} else {{
    ev = cal.createEvent(p.title, s, en, {{ location: p.location || '' }});
  }}
  return {{ id: ev.getId() }};
}}

function remove(p) {{
  const ev = CalendarApp.getDefaultCalendar().getEventById(p.id);
  if (ev) ev.deleteEvent();
  return {{ ok: true }};
}}

// ---- Kontakty Google (People API) ----

function contacts() {{
  const groups = [];
  let pt;
  do {{
    const r = People.ContactGroups.list({{ pageSize: 1000, pageToken: pt, groupFields: 'name,groupType' }});
    (r.contactGroups || []).forEach(function (g) {{
      if (g.groupType === 'USER_CONTACT_GROUP') groups.push({{ id: g.resourceName, name: g.name }});
    }});
    pt = r.nextPageToken;
  }} while (pt);
  const people = [];
  pt = undefined;
  do {{
    const r = People.People.Connections.list('people/me', {{ pageSize: 1000, pageToken: pt, personFields: 'names,emailAddresses,memberships' }});
    (r.connections || []).forEach(function (c) {{
      people.push({{
        id: c.resourceName,
        name: (c.names && c.names[0] && c.names[0].displayName) || '',
        emails: (c.emailAddresses || []).map(function (e) {{ return e.value; }}),
        groups: (c.memberships || []).map(function (m) {{ return m.contactGroupMembership && m.contactGroupMembership.contactGroupResourceName; }})
          .filter(function (g) {{ return g && g.indexOf('contactGroups/') === 0 && g !== 'contactGroups/myContacts' && g !== 'contactGroups/starred'; }})
      }});
    }});
    pt = r.nextPageToken;
  }} while (pt);
  return {{ people: people, groups: groups }};
}}

function saveGroup(p) {{
  if (p.id) {{
    const g = People.ContactGroups.get(p.id);
    People.ContactGroups.update({{ contactGroup: {{ name: p.name, etag: g.etag }} }}, p.id);
    return {{ id: p.id }};
  }}
  const g = People.ContactGroups.create({{ contactGroup: {{ name: p.name }} }});
  return {{ id: g.resourceName }};
}}

function deleteGroup(p) {{
  People.ContactGroups.remove(p.id, {{ deleteContacts: false }});
  return {{ ok: true }};
}}

function savePerson(d) {{
  const memberships = (d.groups || []).map(function (g) {{ return {{ contactGroupMembership: {{ contactGroupResourceName: g }} }}; }});
  memberships.push({{ contactGroupMembership: {{ contactGroupResourceName: 'contactGroups/myContacts' }} }});
  const body = {{ names: [{{ unstructuredName: d.name }}], emailAddresses: (d.emails || []).map(function (e) {{ return {{ value: e }}; }}), memberships: memberships }};
  if (d.id) {{
    const cur = People.People.get(d.id, {{ personFields: 'metadata' }});
    body.etag = cur.etag;
    const r = People.People.updateContact(body, d.id, {{ updatePersonFields: 'names,emailAddresses,memberships' }});
    return {{ id: r.resourceName }};
  }}
  const r = People.People.createContact(body);
  return {{ id: r.resourceName }};
}}

function deletePerson(p) {{
  People.People.deleteContact(p.id);
  return {{ ok: true }};
}}

// Uruchom tę funkcję raz w edytorze (▶ Uruchom), jeśli Google nie zapytał o zgodę na kalendarz i kontakty.
function autoryzuj() {{
  CalendarApp.getDefaultCalendar().getName();
  People.ContactGroups.list({{ pageSize: 1 }});
}}

function out(o) {{
  return ContentService.createTextOutput(JSON.stringify(o)).setMimeType(ContentService.MimeType.JSON);
}}
"#
    )
}

fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The Polish form for a number: 1 kontakt, 2 kontakty, 5 kontaktów.
fn pl(n: usize, one: &'static str, few: &'static str, many: &'static str) -> &'static str {
    match n {
        1 => one,
        n if (2..=4).contains(&(n % 10)) && !(12..=14).contains(&(n % 100)) => few,
        _ => many,
    }
}

/// Is everything allowed? The calendar and the contacts asked once each (only reading):
/// for each, what it holds or what is missing (with what to do).
pub fn check(url: &str, key: &str) -> (Result<String, String>, Result<String, String>) {
    let now = Local::now().naive_local();
    let cal = ask_script(
        url,
        key,
        &[("action", "list".into()), ("from", ms_of(now - Duration::days(7)).to_string()), ("to", ms_of(now + Duration::days(30)).to_string())],
    )
    .map(|v| {
        let n = v["events"].as_array().map(|a| a.len()).unwrap_or(0);
        let c = v["calendars"].as_array().map(|a| a.len()).unwrap_or(0);
        format!("kalendarz działa ({c} {}, {n} {} w najbliższych tygodniach)", pl(c, "kalendarz", "kalendarze", "kalendarzy"), pl(n, "wydarzenie", "wydarzenia", "wydarzeń"))
    });
    let people = ask_script(url, key, &[("action", "contacts".into())]).and_then(|v| match v.get("people") {
        Some(p) => {
            let (n, g) = (p.as_array().map(|a| a.len()).unwrap_or(0), v["groups"].as_array().map(|a| a.len()).unwrap_or(0));
            Ok(format!("kontakty działają ({n} {}, {g} {})", pl(n, "kontakt", "kontakty", "kontaktów"), pl(g, "grupa", "grupy", "grup")))
        }
        None => Err("skrypt jest w starszej wersji, bez kontaktów — skopiuj go z programu jeszcze raz, wklej w miejsce starego i wdróż nową wersję".into()),
    });
    (cal, people)
}

/// One question to the script: its answer (JSON).
fn ask_script(url: &str, key: &str, params: &[(&str, String)]) -> Result<serde_json::Value, String> {
    let mut q = format!("{}?key={}", url.trim(), enc(key));
    for (k, v) in params {
        q.push_str(&format!("&{k}={}", enc(v)));
    }
    let resp = agent()?.get(&q).call().map_err(|e| match e {
        ureq::Error::Status(404, _) => "nie ma skryptu pod tym adresem (sprawdź adres /exec)".to_string(),
        ureq::Error::Status(401 | 403, _) => "Google nie wpuszcza do skryptu — przy wdrożeniu wybierz „Kto ma dostęp: Każdy”".to_string(),
        ureq::Error::Status(code, _) => format!("Google odpowiedział {code}"),
        other => format!("brak połączenia: {other}"),
    })?;
    let text = resp.into_string().map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
        if text.contains("accounts.google.com") || text.to_lowercase().contains("<html") {
            "Google prosi o zalogowanie — przy wdrożeniu skryptu wybierz „Kto ma dostęp: Każdy”".to_string()
        } else {
            "skrypt odpowiedział czymś innym niż oczekiwano".to_string()
        }
    })?;
    if let Some(e) = v["error"].as_str() {
        let low = e.to_lowercase();
        if low.contains("authorization is required") || low.contains("you do not have permission") || low.contains("insufficient") {
            return Err("skrypt nie ma zgody Google — w edytorze skryptu wybierz funkcję „autoryzuj”, kliknij ▶ Uruchom i zezwól, potem wdróż nową wersję".into());
        }
        if e.contains("People is not defined") {
            return Err("w skrypcie brakuje usługi People API — w edytorze skryptu: Usługi (+) → People API → Dodaj, potem Wdróż → Zarządzaj wdrożeniami → nowa wersja".into());
        }
        return Err(if e.contains("klucz") { "skrypt ma inny klucz — skopiuj skrypt z programu jeszcze raz i wdróż go ponownie".into() } else { format!("skrypt: {e}") });
    }
    Ok(v)
}

fn local_ms(ms: i64) -> Option<NaiveDateTime> {
    Local.timestamp_millis_opt(ms).single().map(|t| t.naive_local())
}

fn ms_of(t: NaiveDateTime) -> i64 {
    Local.from_local_datetime(&t).earliest().map(|t| t.timestamp_millis()).unwrap_or(0)
}

/// The script's answer to "list" turned into events.
pub fn parse_script(v: &serde_json::Value, color_base: usize) -> Vec<Event> {
    let mut out = Vec::new();
    for e in v["events"].as_array().cloned().unwrap_or_default() {
        let (Some(s), Some(en)) = (e["start"].as_i64().and_then(local_ms), e["end"].as_i64().and_then(local_ms)) else { continue };
        let gid = e["id"].as_str().unwrap_or_default().to_string();
        let title = e["title"].as_str().filter(|t| !t.trim().is_empty()).unwrap_or("(bez tytułu)").to_string();
        let location = e["location"].as_str().unwrap_or_default().to_string();
        let all_day = e["allDay"].as_bool().unwrap_or(false);
        let editable = e["editable"].as_bool().unwrap_or(false);
        let color = COLORS[(color_base + e["cal"].as_u64().unwrap_or(0) as usize) % COLORS.len()];
        let days = if all_day { (en.date() - s.date()).num_days().clamp(1, 31) } else { 1 };
        for k in 0..days {
            let day = s.date() + Duration::days(k);
            let minutes = if all_day { 24 * 60 } else { (en - s).num_minutes().min(24 * 60 - s.time().hour_minutes()).max(15) };
            out.push(Event {
                id: stable_id(&gid, day, k as usize),
                title: title.clone(),
                date: day,
                start: if all_day { NaiveTime::MIN } else { s.time() },
                minutes,
                location: location.clone(),
                color,
                google: true,
                all_day,
                gid: if editable && !all_day { gid.clone() } else { String::new() },
            });
        }
    }
    out
}

/// The first script address among the calendars (where new events are saved).
pub fn script_url(text: &str) -> Option<String> {
    urls(text).into_iter().find(|u| is_script(u))
}

/// A calendar file with what Google writes: repeats, an exception, a moved day, whole days.
pub const SAMPLE: &str = "BEGIN:VCALENDAR\r
VERSION:2.0\r
PRODID:-//Google Inc//Google Calendar 70.9054//EN\r
X-WR-CALNAME:Praca\r
X-WR-TIMEZONE:Europe/Warsaw\r
BEGIN:VEVENT\r
DTSTART;TZID=Europe/Warsaw:20261005T090000\r
DTEND;TZID=Europe/Warsaw:20261005T100000\r
RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR\r
EXDATE;TZID=Europe/Warsaw:20261104T090000\r
UID:daily-standup@google.com\r
SUMMARY:Spotkanie IT\r
LOCATION:Spotkanie online\r
END:VEVENT\r
BEGIN:VEVENT\r
DTSTART;TZID=Europe/Warsaw:20261106T113000\r
DTEND;TZID=Europe/Warsaw:20261106T120000\r
RECURRENCE-ID;TZID=Europe/Warsaw:20261106T090000\r
UID:daily-standup@google.com\r
SUMMARY:Spotkanie IT (przeniesione)\r
END:VEVENT\r
BEGIN:VEVENT\r
DTSTART:20261103T140000Z\r
DTEND:20261103T153000Z\r
UID:review@google.com\r
SUMMARY:Przegląd wdrożenia\\, etap 2\r
LOCATION:Sala A\r
END:VEVENT\r
BEGIN:VEVENT\r
DTSTART;VALUE=DATE:20261111\r
DTEND;VALUE=DATE:20261112\r
UID:holiday@google.com\r
SUMMARY:Narodowe Święto Niepodległości\r
END:VEVENT\r
BEGIN:VEVENT\r
DTSTART;TZID=Europe/Warsaw:20260105T080000\r
DTEND;TZID=Europe/Warsaw:20260105T083000\r
RRULE:FREQ=MONTHLY;BYDAY=-1FR;COUNT=24\r
UID:monthly@google.com\r
SUMMARY:Raport miesięczny\r
END:VEVENT\r
BEGIN:VEVENT\r
DTSTART;TZID=Europe/Warsaw:20261103T120000\r
DTEND;TZID=Europe/Warsaw:20261103T130000\r
UID:cancelled@google.com\r
STATUS:CANCELLED\r
SUMMARY:Odwołane\r
END:VEVENT\r
END:VCALENDAR\r
";
