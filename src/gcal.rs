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
            t!("cal.gcal.not_shared").into_owned()
        }
        ureq::Error::Status(code, _) => t!("cal.gcal.http_status", code = code).into_owned(),
        other => t!("cal.gcal.no_connection", err = other).into_owned(),
    })?;
    let text = resp.into_string().map_err(|e| e.to_string())?;
    if !text.contains("BEGIN:VCALENDAR") {
        return Err(t!("cal.gcal.not_calendar").into_owned());
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

/// The id of a meeting put into the calendar from an invitation (one per day it is on): told
/// apart from every other event by its top bits, and found again by the meeting's own id.
pub fn invite_id(uid: &str, day: NaiveDate) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (uid, day).hash(&mut h);
    (1 << 59) | (h.finish() >> 5)
}

pub fn is_invite(id: u64) -> bool {
    id >> 59 == 1
}

/// The days of an invitation's meeting (repeats too), as read-only events for the calendar,
/// four months back to a year ahead of `today`.
pub fn invite_events(ics: &str, today: NaiveDate) -> Vec<Event> {
    let Some(inv) = invitation(ics) else { return Vec::new() };
    let mut evs = parse(ics, 2, today - Duration::days(120), today + Duration::days(400));
    for e in &mut evs {
        e.id = invite_id(&inv.uid, e.date);
    }
    evs
}

/// The events of a calendar file, each as its lines (lines continued on the next one joined back).
fn blocks(ics: &str) -> Vec<Vec<Prop>> {
    let text = unfold(ics);
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
    blocks
}

/// A calendar file with its long lines joined back (a line continued starts with a space or tab).
fn unfold(ics: &str) -> String {
    ics.replace("\r\n", "\n").replace("\n ", "").replace("\n\t", "")
}

/// The e-mail address of a calendar person (`mailto:a@b`), lower-case; "" when there is none.
fn mailto(value: &str) -> String {
    let v = value.trim();
    let v = match v.get(..7) {
        Some(p) if p.eq_ignore_ascii_case("mailto:") => &v[7..],
        _ => v,
    };
    if v.contains('@') { v.trim().to_lowercase() } else { String::new() }
}

/// The answer to an invitation for its organiser (iTIP `REPLY`, RFC 5546): the meeting named as
/// the invitation names it (its time zones too), and one guest, `me`, with `answer` (`accepted`,
/// `tentative` or `declined`). None when the file has no meeting or no organiser to answer.
pub fn reply(ics: &str, me: &str, answer: &str, now: chrono::DateTime<Utc>) -> Option<String> {
    let partstat = match answer {
        "accepted" => "ACCEPTED",
        "tentative" => "TENTATIVE",
        _ => "DECLINED",
    };
    let text = unfold(ics);
    let (mut zones, mut kept, mut zone, mut event, mut done) = (String::new(), String::new(), false, false, false);
    for line in text.lines().map(str::trim_end) {
        let upper = line.to_ascii_uppercase();
        if upper == "BEGIN:VTIMEZONE" {
            zone = true;
        }
        if zone {
            zones.push_str(line);
            zones.push_str("\r\n");
            zone = upper != "END:VTIMEZONE";
        } else if upper == "BEGIN:VEVENT" && !done {
            event = true;
        } else if upper == "END:VEVENT" && event {
            (event, done) = (false, true);
        } else if event {
            let name = upper.split([';', ':']).next().unwrap_or("");
            if ["UID", "RECURRENCE-ID", "SEQUENCE", "DTSTART", "DTEND", "DURATION", "SUMMARY", "ORGANIZER"].contains(&name) {
                kept.push_str(line);
                kept.push_str("\r\n");
            }
        }
    }
    let organizer = kept.lines().find(|l| l.to_ascii_uppercase().starts_with("ORGANIZER")).and_then(parse_line).map(|p| mailto(&p.value))?;
    if organizer.is_empty() || !kept.to_ascii_uppercase().contains("UID:") {
        return None;
    }
    let me = me.trim();
    let lines = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Poczta//Poczta//EN\r\nMETHOD:REPLY\r\n{zones}BEGIN:VEVENT\r\n{kept}DTSTAMP:{}\r\nATTENDEE;PARTSTAT={partstat}:mailto:{me}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        now.format("%Y%m%dT%H%M%SZ")
    );
    // lines longer than 75 bytes go on in the next line, after a space (RFC 5545)
    let mut out = String::with_capacity(lines.len() + 64);
    for line in lines.split_terminator("\r\n") {
        let mut n = 0;
        for c in line.chars() {
            if n + c.len_utf8() > 75 {
                out.push_str("\r\n ");
                n = 1;
            }
            out.push(c);
            n += c.len_utf8();
        }
        out.push_str("\r\n");
    }
    Some(out)
}

/// A meeting sent by email (the calendar file inside an invitation).
#[derive(Clone, Debug, PartialEq)]
pub struct Invitation {
    /// its id, the same in every calendar it is in
    pub uid: String,
    pub title: String,
    pub location: String,
    pub start: NaiveDateTime,
    pub minutes: i64,
    pub all_day: bool,
    /// the time zone the time is written in, when it is not this computer's own for sure
    pub zone: String,
    /// the organiser called it off (the email says so; nothing to answer)
    pub cancelled: bool,
    /// an invitation asking for an answer (not somebody's answer or a plain calendar file)
    pub asks: bool,
    /// who to answer (an e-mail address, lower-case; "" when it does not say)
    pub organizer: String,
}

/// The meeting of an invitation's calendar file (its first event), if it is one.
pub fn invitation(ics: &str) -> Option<Invitation> {
    let method = ics.lines().find_map(|l| l.trim().to_ascii_uppercase().strip_prefix("METHOD:").map(|m| m.trim().to_string())).unwrap_or_default();
    let b = blocks(ics).into_iter().next()?;
    let get = |n: &str| b.iter().find(|p| p.name == n);
    let start_p = get("DTSTART")?;
    let start = parse_when(start_p)?;
    let (all_day, at) = match start {
        When::Day(d) => (true, d.and_time(NaiveTime::MIN)),
        When::At(t) => (false, t),
    };
    let minutes = match (start, get("DTEND").and_then(parse_when)) {
        (When::At(s), Some(When::At(e))) => (e - s).num_minutes().max(0),
        (When::Day(s), Some(When::Day(e))) => (e - s).num_days().max(1) * 1440,
        _ => get("DURATION").and_then(|p| parse_duration(&p.value)).unwrap_or(if all_day { 1440 } else { 60 }),
    };
    Some(Invitation {
        uid: get("UID").map(|p| p.value.trim().to_string()).filter(|u| !u.is_empty())?,
        title: get("SUMMARY").map(|p| unescape(&p.value)).filter(|t| !t.trim().is_empty()).unwrap_or_else(|| t!("cal.gcal.untitled").into_owned()),
        location: get("LOCATION").map(|p| unescape(&p.value)).unwrap_or_default(),
        start: at,
        minutes,
        all_day,
        // (a time ending in Z is turned into this computer's; one with a named zone is as written)
        zone: start_p.params.get("TZID").cloned().unwrap_or_default(),
        cancelled: method == "CANCEL" || get("STATUS").is_some_and(|p| p.value.eq_ignore_ascii_case("CANCELLED")),
        asks: method == "REQUEST",
        organizer: get("ORGANIZER").map(|p| mailto(&p.value)).unwrap_or_default(),
    })
}

/// The events of the calendar at the same time as an invitation's meeting (not whole days, not
/// the meeting itself: its copy from the invitation, or Google's with the same title and start).
pub fn clashes<'a>(events: &'a [Event], inv: &Invitation) -> Vec<&'a Event> {
    if inv.all_day {
        return Vec::new();
    }
    let end = inv.start + Duration::minutes(inv.minutes.max(1));
    events
        .iter()
        .filter(|e| !e.all_day && e.id != invite_id(&inv.uid, e.date))
        .filter(|e| {
            let s = e.date.and_time(e.start);
            s < end && s + Duration::minutes(e.minutes.max(1)) > inv.start && !(s == inv.start && e.title == inv.title)
        })
        .collect()
}

/// The events of one calendar file between `from` and `to`, in colour `color`.
pub fn parse(ics: &str, color: usize, from: NaiveDate, to: NaiveDate) -> Vec<Event> {
    // every event's lines
    let blocks = blocks(ics);
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
        let title = get("SUMMARY").map(|p| unescape(&p.value)).filter(|t| !t.trim().is_empty()).unwrap_or_else(|| t!("cal.gcal.untitled").into_owned());
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
            Err(e) => errors.push(if urls.len() > 1 { t!("cal.gcal.calendar_n", n = i + 1, err = e).into_owned() } else { e }),
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
        r#"{header}
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

{contacts_title}

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

{authorize_hint}
function autoryzuj() {{
  CalendarApp.getDefaultCalendar().getName();
  People.ContactGroups.list({{ pageSize: 1 }});
}}

function out(o) {{
  return ContentService.createTextOutput(JSON.stringify(o)).setMimeType(ContentService.MimeType.JSON);
}}
"#,
        header = t!("cal.gcal.script_header"),
        contacts_title = t!("cal.gcal.script_contacts"),
        authorize_hint = t!("cal.gcal.script_authorize"),
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
        t!("cal.gcal.calendar_ok", calendars = crate::i18n::plural("cal.gcal.calendars", c), events = crate::i18n::plural("cal.gcal.events", n)).into_owned()
    });
    let people = ask_script(url, key, &[("action", "contacts".into())]).and_then(|v| match v.get("people") {
        Some(p) => {
            let (n, g) = (p.as_array().map(|a| a.len()).unwrap_or(0), v["groups"].as_array().map(|a| a.len()).unwrap_or(0));
            Ok(t!("cal.gcal.contacts_ok", people = crate::i18n::plural("cal.gcal.people", n), groups = crate::i18n::plural("cal.gcal.groups", g)).into_owned())
        }
        None => Err(t!("cal.gcal.old_script").into_owned()),
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
        ureq::Error::Status(404, _) => t!("cal.gcal.script_404").into_owned(),
        ureq::Error::Status(401 | 403, _) => t!("cal.gcal.script_denied").into_owned(),
        ureq::Error::Status(code, _) => t!("cal.gcal.http_status", code = code).into_owned(),
        other => t!("cal.gcal.no_connection", err = other).into_owned(),
    })?;
    let text = resp.into_string().map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
        if text.contains("accounts.google.com") || text.to_lowercase().contains("<html") {
            t!("cal.gcal.script_login").into_owned()
        } else {
            t!("cal.gcal.script_other").into_owned()
        }
    })?;
    if let Some(e) = v["error"].as_str() {
        let low = e.to_lowercase();
        if low.contains("authorization is required") || low.contains("you do not have permission") || low.contains("insufficient") {
            return Err(t!("cal.gcal.script_no_consent").into_owned());
        }
        if e.contains("People is not defined") {
            return Err(t!("cal.gcal.script_no_people").into_owned());
        }
        return Err(if e.contains("klucz") { t!("cal.gcal.script_other_key").into_owned() } else { t!("cal.gcal.script_error", err = e).into_owned() });
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
        let title = e["title"].as_str().filter(|t| !t.trim().is_empty()).map(String::from).unwrap_or_else(|| t!("cal.gcal.untitled").into_owned());
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

/// The calendar file of an invitation to a meeting, as mail programs send it.
pub const SAMPLE_INVITE: &str = "BEGIN:VCALENDAR\r
METHOD:REQUEST\r
BEGIN:VEVENT\r
UID:invite-1@example.com\r
DTSTART:20261120T090000Z\r
DTEND:20261120T103000Z\r
SUMMARY:Przegląd wdrożenia\\, etap 2\r
LOCATION:Sala B\r
ORGANIZER;CN=Anna Nowak:mailto:anna@example.com\r
ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:me@example.com\r
END:VEVENT\r
END:VCALENDAR\r
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invitation_read_from_its_calendar_file() {
        let i = invitation(SAMPLE_INVITE).unwrap();
        assert_eq!((i.uid.as_str(), i.title.as_str(), i.location.as_str()), ("invite-1@example.com", "Przegląd wdrożenia, etap 2", "Sala B"));
        assert!(i.asks && !i.cancelled && !i.all_day && i.zone.is_empty());
        assert_eq!(i.minutes, 90);
        // a time in universal time is shown on this computer's clock
        assert_eq!(i.start, Utc.with_ymd_and_hms(2026, 11, 20, 9, 0, 0).unwrap().with_timezone(&Local).naive_local());
        // called off; a named time zone is kept to be shown; a whole day
        let off = invitation(&SAMPLE_INVITE.replace("METHOD:REQUEST", "METHOD:CANCEL").replace("DTSTART:20261120T090000Z", "DTSTART;TZID=Europe/London:20261120T090000").replace("DTEND:20261120T103000Z", "DURATION:PT45M")).unwrap();
        assert!(off.cancelled && !off.asks);
        assert_eq!((off.zone.as_str(), off.minutes, off.start.format("%H:%M").to_string().as_str()), ("Europe/London", 45, "09:00"));
        let day = invitation(&SAMPLE_INVITE.replace("DTSTART:20261120T090000Z", "DTSTART;VALUE=DATE:20261120").replace("DTEND:20261120T103000Z", "DTEND;VALUE=DATE:20261122")).unwrap();
        assert!(day.all_day && day.minutes == 2 * 1440);
        // somebody's answer, a plain calendar file and a file without a meeting are not invitations to answer
        assert!(!invitation(&SAMPLE_INVITE.replace("METHOD:REQUEST", "METHOD:REPLY")).unwrap().asks);
        assert!(!invitation(SAMPLE).unwrap().asks);
        assert!(invitation("BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nEND:VCALENDAR").is_none());
        // in the calendar: every day of a repeating meeting, each found again by the meeting's id
        let today = NaiveDate::from_ymd_opt(2026, 11, 18).unwrap();
        let weekly = invite_events(&SAMPLE_INVITE.replace("LOCATION:Sala B", "RRULE:FREQ=WEEKLY;COUNT=3"), today);
        assert_eq!(weekly.len(), 3);
        assert!(weekly.iter().all(|e| is_invite(e.id) && e.id == invite_id("invite-1@example.com", e.date) && e.google && e.gid.is_empty()));
        assert!(!is_invite(stable_id("invite-1@example.com", today, 0)) && !is_invite(7));
        assert!(invite_events(&SAMPLE_INVITE.replace("UID:", "STATUS:CANCELLED\r\nUID:"), today).is_empty());
    }

    #[test]
    fn what_an_invitation_clashes_with() {
        let inv = invitation(&SAMPLE_INVITE.replace("DTSTART:20261120T090000Z", "DTSTART;TZID=X:20261120T090000").replace("DTEND:20261120T103000Z", "DURATION:PT90M")).unwrap();
        let day = inv.start.date();
        let ev = |title: &str, h: u32, m: u32, minutes: i64| Event {
            id: 7,
            title: title.into(),
            date: day,
            start: NaiveTime::from_hms_opt(h, m, 0).unwrap(),
            minutes,
            location: String::new(),
            color: 0,
            google: false,
            all_day: false,
            gid: String::new(),
        };
        let mut own = ev(&inv.title, 9, 0, 90);
        own.id = invite_id(&inv.uid, day);
        let mut whole = ev("Urlop", 0, 0, 1440);
        whole.all_day = true;
        let evs = [ev("Stand-up", 10, 15, 15), ev("Lunch", 10, 30, 30), ev("Early", 8, 0, 60), own, ev(&inv.title, 9, 0, 90), whole];
        let titles: Vec<&str> = clashes(&evs, &inv).iter().map(|e| e.title.as_str()).collect();
        // touching at the end or the start is no clash; the meeting itself and whole days are not counted
        assert_eq!(titles, ["Stand-up"]);
        assert!(clashes(&evs, &Invitation { all_day: true, ..inv }).is_empty());
    }

    #[test]
    fn the_answer_to_an_invitation_for_its_organiser() {
        assert_eq!(invitation(SAMPLE_INVITE).unwrap().organizer, "anna@example.com");
        let now = Utc.with_ymd_and_hms(2026, 11, 18, 7, 0, 0).unwrap();
        let zoned = SAMPLE_INVITE
            .replace("METHOD:REQUEST\r\n", "METHOD:REQUEST\r\nBEGIN:VTIMEZONE\r\nTZID:Europe/Warsaw\r\nBEGIN:STANDARD\r\nTZOFFSETTO:+0100\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n")
            .replace("DTSTART:20261120T090000Z", "DTSTART;TZID=Europe/Warsaw:20261120T100000\r\nSEQUENCE:2")
            .replace("SUMMARY:Przegląd wdrożenia\\, etap 2", &format!("SUMMARY:{}\r\n {}", "Bardzo długi tytuł spotkania o przeglądzie wdrożenia", "i jego drugim etapie"));
        let r = reply(&zoned, "Me@Example.com", "tentative", now).unwrap();
        for part in ["METHOD:REPLY\r\n", "UID:invite-1@example.com\r\n", "SEQUENCE:2\r\n", "DTSTART;TZID=Europe/Warsaw:20261120T100000\r\n", "DTSTAMP:20261118T070000Z\r\n", "ATTENDEE;PARTSTAT=TENTATIVE:mailto:Me@Example.com\r\n", "ORGANIZER;CN=Anna Nowak:mailto:anna@example.com\r\n", "BEGIN:VTIMEZONE\r\nTZID:Europe/Warsaw\r\n"] {
            assert!(r.contains(part), "{part} in {r}");
        }
        // only the guest answering; the location is not repeated; long lines are folded
        assert!(!r.contains("me@example.com") && !r.contains("LOCATION") && r.lines().all(|l| l.len() <= 75));
        assert_eq!(invitation(&r).unwrap().title, "Bardzo długi tytuł spotkania o przeglądzie wdrożeniai jego drugim etapie");
        assert!(reply(&zoned, "me@example.com", "declined", now).unwrap().contains("PARTSTAT=DECLINED"));
        // nobody to answer
        assert!(reply(&SAMPLE_INVITE.replace("ORGANIZER;CN=Anna Nowak:mailto:anna@example.com\r\n", ""), "me@example.com", "accepted", now).is_none());
    }
}
