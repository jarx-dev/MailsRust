//! Google Calendar and Google Contacts through the person's own Google sign-in (OAuth, the same
//! as for Gmail): the Calendar API v3 and the People API. Reads everything, writes the events
//! and the contacts and groups made or changed in the app.

use crate::model::{Account, Event};
use chrono::{Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use serde_json::{json, Value};

pub const SCOPE_CALENDAR: &str = "https://www.googleapis.com/auth/calendar";
pub const SCOPE_CONTACTS: &str = "https://www.googleapis.com/auth/contacts";

const CAL: &str = "https://www.googleapis.com/calendar/v3";
const PEOPLE: &str = "https://people.googleapis.com/v1";

/// The account can reach Google this way (signed in with Google, not with a password).
pub fn ready(acc: &Account) -> bool {
    acc.use_oauth && !acc.oauth_refresh_token.is_empty() && !acc.oauth_client_id.trim().is_empty()
}

/// The sign-in included the calendar / the contacts (a sign-in from before they were asked
/// for doesn't: then the person signs in again once).
pub fn has_scope(acc: &Account, scope: &str) -> bool {
    acc.oauth_scopes.split_whitespace().any(|s| s == scope)
}

pub fn token(acc: &Account) -> Result<String, String> {
    crate::oauth::access_token(&acc.oauth_client_id, &acc.oauth_client_secret, &acc.oauth_refresh_token)
}

/// For a sign-in made before the app kept it: the Google Workspace domain ("" for a personal
/// account) and the account's id.
pub fn userinfo(acc: &Account) -> Result<(String, String), String> {
    let v = get(&token(acc)?, "https://openidconnect.googleapis.com/v1/userinfo", "konto")?;
    Ok((v["hd"].as_str().unwrap_or("").to_string(), v["sub"].as_str().unwrap_or("").to_string()))
}

pub(crate) fn agent() -> Result<ureq::Agent, String> {
    let tls = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
    Ok(ureq::AgentBuilder::new()
        .tls_connector(std::sync::Arc::new(tls))
        .timeout_connect(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(60))
        .build())
}

/// What went wrong, in words the person can act on.
fn explain(e: ureq::Error, what: &str) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            if body.contains("SERVICE_DISABLED") || body.contains("has not been used in project") || body.contains("is disabled") {
                let api = match what {
                    "kalendarz" => "Google Calendar API",
                    // from the message menu there's no button to turn it on: say where
                    "reguły" => return t!("mail.rules.api_disabled").into_owned(),
                    _ => "People API",
                };
                return t!("cal.google.api_disabled", api = api).into_owned();
            }
            if code == 403 && (body.contains("insufficient") || body.contains("ACCESS_TOKEN_SCOPE_INSUFFICIENT") || body.contains("PERMISSION_DENIED")) {
                return if what == "reguły" { t!("mail.rules.no_consent") } else { t!("cal.google.no_consent") }.into_owned();
            }
            if code == 401 {
                crate::oauth::forget_tokens();
                return t!("cal.google.login_refused").into_owned();
            }
            let msg = serde_json::from_str::<Value>(&body).ok().and_then(|v| v["error"]["message"].as_str().map(String::from)).unwrap_or(body);
            let what = match what {
                "kalendarz" => t!("cal.google.what_calendar"),
                "kontakty" => t!("cal.google.what_contacts"),
                "reguły" => t!("mail.rules.what"),
                _ => t!("cal.google.what_account"),
            };
            t!("cal.google.http_status", what = what, code = code, msg = msg.chars().take(200).collect::<String>()).into_owned()
        }
        other => t!("cal.google.no_connection", err = other).into_owned(),
    }
}

fn get(token: &str, url: &str, what: &str) -> Result<Value, String> {
    agent()?.get(url).set("Authorization", &format!("Bearer {token}")).call().map_err(|e| explain(e, what))?.into_json().map_err(|e| e.to_string())
}

fn send(token: &str, method: &str, url: &str, body: Option<Value>, what: &str) -> Result<Value, String> {
    let req = agent()?.request(method, url).set("Authorization", &format!("Bearer {token}"));
    let resp = match body {
        Some(b) => req.send_json(b),
        None => req.call(),
    }
    .map_err(|e| explain(e, what))?;
    let text = resp.into_string().unwrap_or_default();
    Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
}

pub(crate) fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ------------------------------------------------------------ calendar

fn rfc3339(t: NaiveDateTime) -> String {
    Local.from_local_datetime(&t).earliest().map(|t| t.to_rfc3339()).unwrap_or_default()
}

fn when(v: &Value) -> Option<(NaiveDateTime, bool)> {
    if let Some(d) = v["date"].as_str() {
        return NaiveDate::parse_from_str(d, "%Y-%m-%d").ok().map(|d| (d.and_time(NaiveTime::MIN), true));
    }
    let t = chrono::DateTime::parse_from_rfc3339(v["dateTime"].as_str()?).ok()?;
    Some((t.with_timezone(&Local).naive_local(), false))
}

const COLORS: [usize; 6] = [1, 3, 4, 2, 5, 0];

fn stable_id(cal: &str, ev: &str, day: NaiveDate) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (cal, ev, day).hash(&mut h);
    (1 << 60) | (h.finish() >> 4)
}

/// The events of the calendars shown in Google Calendar, four months back to a year ahead.
/// Events of the main calendar (not repeating, not whole-day) can be changed from the app.
pub fn calendar_events(acc: &Account) -> Result<Vec<Event>, String> {
    let token = token(acc)?;
    let list = get(&token, &format!("{CAL}/users/me/calendarList?minAccessRole=reader"), "kalendarz")?;
    let today = Local::now().date_naive();
    let (from, to) = (rfc3339((today - Duration::days(120)).and_time(NaiveTime::MIN)), rfc3339((today + Duration::days(400)).and_time(NaiveTime::MIN)));
    let mut out = Vec::new();
    let cals: Vec<Value> = list["items"].as_array().cloned().unwrap_or_default().into_iter().filter(|c| c["selected"].as_bool().unwrap_or(c["primary"].as_bool().unwrap_or(false))).collect();
    for (i, c) in cals.iter().enumerate() {
        let cal_id = c["id"].as_str().unwrap_or_default();
        let primary = c["primary"].as_bool().unwrap_or(false);
        let mut page: Option<String> = None;
        loop {
            let mut url = format!("{CAL}/calendars/{}/events?singleEvents=true&orderBy=startTime&maxResults=2500&timeMin={}&timeMax={}", enc(cal_id), enc(&from), enc(&to));
            if let Some(p) = &page {
                url.push_str(&format!("&pageToken={}", enc(p)));
            }
            let v = get(&token, &url, "kalendarz")?;
            for e in v["items"].as_array().cloned().unwrap_or_default() {
                if e["status"].as_str() == Some("cancelled") {
                    continue;
                }
                // declined invitations are not shown
                let declined = e["attendees"].as_array().is_some_and(|a| a.iter().any(|x| x["self"].as_bool() == Some(true) && x["responseStatus"].as_str() == Some("declined")));
                if declined {
                    continue;
                }
                let (Some((s, all_day)), Some((en, _))) = (when(&e["start"]), when(&e["end"])) else { continue };
                let gid = e["id"].as_str().unwrap_or_default().to_string();
                let title = e["summary"].as_str().filter(|t| !t.trim().is_empty()).map(String::from).unwrap_or_else(|| t!("cal.gcal.untitled").into_owned());
                let location = e["location"].as_str().unwrap_or_default().to_string();
                let editable = primary && e["recurringEventId"].is_null() && !all_day;
                let days = if all_day { (en.date() - s.date()).num_days().clamp(1, 31) } else { 1 };
                for k in 0..days {
                    let day = s.date() + Duration::days(k);
                    let minutes = if all_day { 24 * 60 } else { (en - s).num_minutes().min(24 * 60 - (s.time() - NaiveTime::MIN).num_minutes()).max(15) };
                    out.push(Event {
                        id: stable_id(cal_id, &gid, day),
                        title: title.clone(),
                        date: day,
                        start: if all_day { NaiveTime::MIN } else { s.time() },
                        minutes,
                        location: location.clone(),
                        color: COLORS[i % COLORS.len()],
                        google: true,
                        all_day,
                        gid: if editable { gid.clone() } else { String::new() },
                    });
                }
            }
            page = v["nextPageToken"].as_str().map(String::from);
            if page.is_none() {
                break;
            }
        }
    }
    Ok(out)
}

/// An event made or changed in the app, written to the main Google calendar: its id there.
pub fn save_event(acc: &Account, gid: &str, title: &str, location: &str, start: NaiveDateTime, minutes: i64) -> Result<String, String> {
    let token = token(acc)?;
    let body = json!({
        "summary": title,
        "location": location,
        "start": {"dateTime": rfc3339(start)},
        "end": {"dateTime": rfc3339(start + Duration::minutes(minutes.max(5)))},
    });
    let v = if gid.is_empty() {
        send(&token, "POST", &format!("{CAL}/calendars/primary/events"), Some(body), "kalendarz")?
    } else {
        send(&token, "PATCH", &format!("{CAL}/calendars/primary/events/{}", enc(gid)), Some(body), "kalendarz")?
    };
    v["id"].as_str().map(String::from).ok_or_else(|| t!("cal.google.event_unconfirmed").into_owned())
}

pub fn delete_event(acc: &Account, gid: &str) -> Result<(), String> {
    let token = token(acc)?;
    send(&token, "DELETE", &format!("{CAL}/calendars/primary/events/{}", enc(gid)), None, "kalendarz").map(|_| ())
}

// ------------------------------------------------------------ contacts

/// The contact groups made by the person, and everyone in "Kontakty" with their groups.
pub fn contacts(token: &str) -> Result<(Vec<(String, String)>, Vec<(String, String, Vec<String>, Vec<String>)>), String> {
    let mut groups = Vec::new();
    let mut page: Option<String> = None;
    loop {
        let mut url = format!("{PEOPLE}/contactGroups?pageSize=1000&groupFields=name,groupType");
        if let Some(p) = &page {
            url.push_str(&format!("&pageToken={}", enc(p)));
        }
        let v = get(token, &url, "kontakty")?;
        for g in v["contactGroups"].as_array().cloned().unwrap_or_default() {
            if g["groupType"].as_str() == Some("USER_CONTACT_GROUP") {
                groups.push((g["resourceName"].as_str().unwrap_or_default().to_string(), g["name"].as_str().unwrap_or_default().to_string()));
            }
        }
        page = v["nextPageToken"].as_str().map(String::from);
        if page.is_none() {
            break;
        }
    }
    let mut people = Vec::new();
    page = None;
    loop {
        let mut url = format!("{PEOPLE}/people/me/connections?pageSize=1000&personFields=names,emailAddresses,memberships");
        if let Some(p) = &page {
            url.push_str(&format!("&pageToken={}", enc(p)));
        }
        let v = get(token, &url, "kontakty")?;
        for c in v["connections"].as_array().cloned().unwrap_or_default() {
            let id = c["resourceName"].as_str().unwrap_or_default().to_string();
            let name = c["names"][0]["displayName"].as_str().unwrap_or_default().to_string();
            let emails: Vec<String> = c["emailAddresses"].as_array().map(|a| a.iter().filter_map(|e| e["value"].as_str().map(String::from)).collect()).unwrap_or_default();
            let gs: Vec<String> = c["memberships"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|m| m["contactGroupMembership"]["contactGroupResourceName"].as_str().map(String::from))
                        .filter(|g| g.starts_with("contactGroups/") && g != "contactGroups/myContacts" && g != "contactGroups/starred")
                        .collect()
                })
                .unwrap_or_default();
            people.push((id, name, emails, gs));
        }
        page = v["nextPageToken"].as_str().map(String::from);
        if page.is_none() {
            break;
        }
    }
    Ok((groups, people))
}

/// A group made (empty `id`) or renamed: its id.
pub fn save_group(token: &str, id: &str, name: &str) -> Result<String, String> {
    if id.is_empty() {
        let v = send(token, "POST", &format!("{PEOPLE}/contactGroups"), Some(json!({"contactGroup": {"name": name}})), "kontakty")?;
        return v["resourceName"].as_str().map(String::from).ok_or_else(|| t!("cal.google.group_unconfirmed").into_owned());
    }
    let cur = get(token, &format!("{PEOPLE}/{id}"), "kontakty")?;
    send(token, "PUT", &format!("{PEOPLE}/{id}"), Some(json!({"contactGroup": {"name": name, "etag": cur["etag"]}})), "kontakty")?;
    Ok(id.to_string())
}

pub fn delete_group(token: &str, id: &str) -> Result<(), String> {
    send(token, "DELETE", &format!("{PEOPLE}/{id}?deleteContacts=false"), None, "kontakty").map(|_| ())
}

/// A person made (empty `id`) or changed, with their groups: their id.
pub fn save_person(token: &str, id: &str, name: &str, emails: &[String], groups: &[String]) -> Result<String, String> {
    let mut memberships: Vec<Value> = groups.iter().map(|g| json!({"contactGroupMembership": {"contactGroupResourceName": g}})).collect();
    memberships.push(json!({"contactGroupMembership": {"contactGroupResourceName": "contactGroups/myContacts"}}));
    let mut body = json!({
        "names": [{"unstructuredName": name}],
        "emailAddresses": emails.iter().map(|e| json!({"value": e})).collect::<Vec<_>>(),
        "memberships": memberships,
    });
    if id.is_empty() {
        let v = send(token, "POST", &format!("{PEOPLE}/people:createContact"), Some(body), "kontakty")?;
        return v["resourceName"].as_str().map(String::from).ok_or_else(|| t!("cal.google.contact_unconfirmed").into_owned());
    }
    let cur = get(token, &format!("{PEOPLE}/{id}?personFields=metadata"), "kontakty")?;
    body["etag"] = cur["etag"].clone();
    send(token, "PATCH", &format!("{PEOPLE}/{id}:updateContact?updatePersonFields=names,emailAddresses,memberships"), Some(body), "kontakty")?;
    Ok(id.to_string())
}

pub fn delete_person(token: &str, id: &str) -> Result<(), String> {
    send(token, "DELETE", &format!("{PEOPLE}/{id}:deleteContact"), None, "kontakty").map(|_| ())
}

/// Is everything allowed? The calendar and the contacts asked once each (only reading).
pub fn check(acc: &Account) -> (Result<String, String>, Result<String, String>) {
    let token = match token(acc) {
        Ok(t) => t,
        Err(e) => return (Err(e.clone()), Err(e)),
    };
    let cal = get(&token, &format!("{CAL}/users/me/calendarList?minAccessRole=reader"), "kalendarz").map(|v| {
        let n = v["items"].as_array().map(|a| a.len()).unwrap_or(0);
        t!("cal.google.calendar_ok", calendars = crate::i18n::plural("cal.gcal.calendars", n)).into_owned()
    });
    let people = contacts(&token).map(|(g, p)| t!("cal.gcal.contacts_ok", people = crate::i18n::plural("cal.gcal.people", p.len()), groups = crate::i18n::plural("cal.gcal.groups", g.len())).into_owned());
    (cal, people)
}

// ------------------------------------------------------------ Gmail rules (filters)

/// Reading and changing Gmail's filters, asked for only when the person opens the rules.
pub const SCOPE_RULES: &str = "https://www.googleapis.com/auth/gmail.settings.basic";
const GMAIL: &str = "https://gmail.googleapis.com/gmail/v1/users/me";

/// One Gmail filter, in words.
pub struct Rule {
    pub id: String,
    /// which mail: "from: a@b.pl, subject: …"
    pub when: String,
    /// what happens to it: "to the bin, mark as read"
    pub then: String,
    /// the filter as Gmail gave it, for changing it
    pub raw: Value,
}

/// The account's filters and its own labels (id, name), which a rule can add.
#[derive(Default)]
pub struct RuleBook {
    pub rules: Vec<Rule>,
    pub labels: Vec<(String, String)>,
    /// the labels couldn't be read (the rules show label ids then)
    pub labels_error: Option<String>,
}

/// Every filter of the account, also the ones made on Gmail's page.
pub fn rules(acc: &Account) -> Result<RuleBook, String> {
    let token = token(acc)?;
    let filters = get(&token, &format!("{GMAIL}/settings/filters"), "reguły")?;
    // filters name the person's own labels by id ("Label_12"), the list gives their names (without it, the ids)
    let (labels, labels_error) = match get(&token, &format!("{GMAIL}/labels"), "reguły") {
        Ok(v) => (v, None),
        Err(e) => (Value::Null, Some(e)),
    };
    let mut own: Vec<(String, String)> = labels["labels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|l| l["type"] == "user")
        .filter_map(|l| Some((l["id"].as_str()?.to_string(), l["name"].as_str()?.to_string())))
        .collect();
    own.sort_by_key(|l| l.1.to_lowercase());
    let label = |id: &str| own.iter().find(|l| l.0 == id).map(|l| l.1.clone()).unwrap_or_else(|| id.to_string());
    let rules = filters["filter"].as_array().into_iter().flatten().map(|f| describe(f, &label)).collect();
    Ok(RuleBook { rules, labels: own, labels_error })
}

pub(crate) fn describe(f: &Value, label: &dyn Fn(&str) -> String) -> Rule {
    let c = &f["criteria"];
    let mut when = vec![];
    for (key, word) in [
        ("from", t!("mail.rules.from")),
        ("to", t!("mail.rules.to")),
        ("subject", t!("mail.rules.subject")),
        ("query", t!("mail.rules.query")),
        ("negatedQuery", t!("mail.rules.negated")),
    ] {
        if let Some(v) = c[key].as_str().filter(|v| !v.is_empty()) {
            when.push(format!("{word} {v}"));
        }
    }
    if c["hasAttachment"].as_bool() == Some(true) {
        when.push(t!("mail.rules.attachment").into_owned());
    }
    if let Some(size) = c["size"].as_i64() {
        let kb = (size + 1023) / 1024;
        when.push(if c["sizeComparison"] == "smaller" { t!("mail.rules.smaller", kb = kb) } else { t!("mail.rules.larger", kb = kb) }.into_owned());
    }
    let a = &f["action"];
    let mut then = vec![];
    let ids = |key: &str| a[key].as_array().map(|l| l.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>()).unwrap_or_default();
    for id in ids("addLabelIds") {
        then.push(match id.as_str() {
            "TRASH" => t!("mail.rules.to_bin").into_owned(),
            "STARRED" => t!("mail.rules.star").into_owned(),
            "IMPORTANT" => t!("mail.rules.important").into_owned(),
            "SPAM" => t!("mail.rules.to_spam").into_owned(),
            c if c.starts_with("CATEGORY_") => t!("mail.rules.category", name = c["CATEGORY_".len()..].to_lowercase()).into_owned(),
            other => t!("mail.rules.label", name = label(other)).into_owned(),
        });
    }
    for id in ids("removeLabelIds") {
        then.push(match id.as_str() {
            "INBOX" => t!("mail.rules.archive").into_owned(),
            "UNREAD" => t!("mail.rules.mark_read").into_owned(),
            "SPAM" => t!("mail.rules.never_spam").into_owned(),
            "IMPORTANT" => t!("mail.rules.not_important").into_owned(),
            other => t!("mail.rules.unlabel", name = label(other)).into_owned(),
        });
    }
    if let Some(to) = a["forward"].as_str() {
        then.push(t!("mail.rules.forward", to = to).into_owned());
    }
    Rule { id: f["id"].as_str().unwrap_or("").to_string(), when: when.join(", "), then: then.join(", "), raw: f.clone() }
}

/// A rule on Gmail's side: mail from `addr` goes straight to the bin, as Gmail's own "Delete it"
/// (a filter can't send mail to spam, Google doesn't allow it).
pub fn block_sender(acc: &Account, addr: &str) -> Result<(), String> {
    if !blockable(addr) {
        return Err(t!("mail.rules.bad_address", addr = addr).into_owned());
    }
    let body = json!({ "criteria": { "from": addr }, "action": { "addLabelIds": ["TRASH"] } });
    match send(&token(acc)?, "POST", &format!("{GMAIL}/settings/filters"), Some(body), "reguły") {
        // blocked twice: the rule is already there
        Err(e) if e.contains("Filter already exists") => Ok(()),
        r => r.map(|_| ()),
    }
}

/// A plain address only: Gmail reads the filter's "from" as a search, where a leading "-"
/// means "not from" and would send every other message to the bin.
pub fn blockable(addr: &str) -> bool {
    let Some((user, domain)) = addr.split_once('@') else { return false };
    let plain = |s: &str| s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._%+-".contains(&b));
    user.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && plain(user)
        && domain.contains('.')
        && domain.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
        && plain(domain)
}

pub fn delete_rule(acc: &Account, id: &str) -> Result<(), String> {
    send(&token(acc)?, "DELETE", &format!("{GMAIL}/settings/filters/{}", enc(id)), None, "reguły").map(|_| ())
}

/// A rule as the editor shows it. Whatever the editor doesn't show (a category, extra labels
/// the rule removes) stays as it was in `base`.
#[derive(Clone, Default)]
pub struct RuleForm {
    /// the rule being changed (None: a new one)
    pub id: Option<String>,
    pub base: Value,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub words: String,
    pub not_words: String,
    pub attachment: bool,
    /// 0: any size
    pub size_kb: u32,
    pub smaller: bool,
    pub archive: bool,
    pub read: bool,
    pub star: bool,
    /// the person's own label to add ("" none)
    pub label: String,
    /// 0: as Gmail decides, 1: always important, 2: never
    pub important: u8,
    pub never_spam: bool,
    pub trash: bool,
    pub forward: String,
    /// also change the mail already in the mailbox
    pub apply: bool,
}

const SYSTEM: [&str; 6] = ["TRASH", "STARRED", "IMPORTANT", "INBOX", "UNREAD", "SPAM"];

fn own_label(id: &str) -> bool {
    !SYSTEM.contains(&id) && !id.starts_with("CATEGORY_")
}

impl RuleForm {
    pub fn from_filter(f: &Value) -> Self {
        let c = &f["criteria"];
        let s = |k: &str| c[k].as_str().unwrap_or("").to_string();
        let a = &f["action"];
        let has = |k: &str, id: &str| a[k].as_array().is_some_and(|l| l.iter().any(|x| x == id));
        let important = if has("addLabelIds", "IMPORTANT") { 1 } else if has("removeLabelIds", "IMPORTANT") { 2 } else { 0 };
        let label = a["addLabelIds"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).find(|x| own_label(x)).unwrap_or("").to_string();
        RuleForm {
            id: f["id"].as_str().map(String::from),
            base: f.clone(),
            from: s("from"),
            to: s("to"),
            subject: s("subject"),
            words: s("query"),
            not_words: s("negatedQuery"),
            attachment: c["hasAttachment"].as_bool() == Some(true),
            size_kb: c["size"].as_u64().map(|b| b.div_ceil(1024) as u32).unwrap_or(0),
            smaller: c["sizeComparison"] == "smaller",
            archive: has("removeLabelIds", "INBOX"),
            read: has("removeLabelIds", "UNREAD"),
            star: has("addLabelIds", "STARRED"),
            label,
            important,
            never_spam: has("removeLabelIds", "SPAM"),
            trash: has("addLabelIds", "TRASH"),
            forward: a["forward"].as_str().unwrap_or("").to_string(),
            apply: false,
        }
    }

    /// The filter to send to Gmail: `base` with what the editor shows put in.
    pub fn to_filter(&self) -> Value {
        let mut c = self.base["criteria"].as_object().cloned().unwrap_or_default();
        for (k, v) in [("from", &self.from), ("to", &self.to), ("subject", &self.subject), ("query", &self.words), ("negatedQuery", &self.not_words)] {
            if v.trim().is_empty() {
                c.remove(k);
            } else {
                c.insert(k.into(), v.trim().into());
            }
        }
        if self.attachment {
            c.insert("hasAttachment".into(), true.into());
        } else {
            c.remove("hasAttachment");
        }
        if self.size_kb > 0 {
            // the editor shows KB: a size made on Gmail's page in bytes stays exact if not changed
            let old = self.base["criteria"]["size"].as_u64().filter(|b| b.div_ceil(1024) == u64::from(self.size_kb));
            c.insert("size".into(), old.unwrap_or(u64::from(self.size_kb) * 1024).into());
            c.insert("sizeComparison".into(), if self.smaller { "smaller" } else { "larger" }.into());
        } else {
            c.remove("size");
            c.remove("sizeComparison");
        }
        let a = &self.base["action"];
        // what the editor doesn't show is kept: categories and labels the rule takes off
        let keep = |k: &str, managed: &dyn Fn(&str) -> bool| -> Vec<Value> {
            a[k].as_array().into_iter().flatten().filter(|x| x.as_str().is_some_and(|x| !managed(x))).cloned().collect()
        };
        let mut add = keep("addLabelIds", &|x| SYSTEM.contains(&x) || own_label(x));
        let mut remove = keep("removeLabelIds", &|x| SYSTEM.contains(&x));
        for (on, id) in [(self.trash, "TRASH"), (self.star, "STARRED"), (self.important == 1, "IMPORTANT")] {
            if on {
                add.push(id.into());
            }
        }
        if !self.label.is_empty() {
            add.push(self.label.clone().into());
        }
        for (on, id) in [(self.archive, "INBOX"), (self.read, "UNREAD"), (self.never_spam, "SPAM"), (self.important == 2, "IMPORTANT")] {
            if on {
                remove.push(id.into());
            }
        }
        let mut action = serde_json::Map::new();
        if !add.is_empty() {
            action.insert("addLabelIds".into(), add.into());
        }
        if !remove.is_empty() {
            action.insert("removeLabelIds".into(), remove.into());
        }
        if !self.forward.trim().is_empty() {
            action.insert("forward".into(), self.forward.trim().into());
        }
        json!({ "criteria": c, "action": action })
    }

    /// Why the rule can't be saved yet (Gmail needs some condition and something to do).
    pub fn problem(&self) -> Option<String> {
        let f = self.to_filter();
        // judged by the search it makes: a condition the editor doesn't show must not pass for one
        if search_query(&f["criteria"]).is_empty() {
            return Some(t!("mail.rules.need_condition").into_owned());
        }
        if f["action"].as_object().is_none_or(|a| a.is_empty()) {
            return Some(t!("mail.rules.need_action").into_owned());
        }
        None
    }

    /// Nothing changed in an existing rule.
    pub fn unchanged(&self) -> bool {
        self.id.is_some() && same_rule(&self.to_filter(), &self.base)
    }
}

/// Two filters do the same: labels in any order, addresses and the subject in any case (the
/// words searched for keep their case: "OR" is an operator there).
fn same_rule(x: &Value, y: &Value) -> bool {
    let norm = |f: &Value| {
        let low = |v: &Value| match v {
            Value::String(s) => Value::String(s.trim().to_lowercase()),
            other => other.clone(),
        };
        let c: serde_json::Map<String, Value> = f["criteria"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, v)| (k.clone(), if ["from", "to", "subject"].contains(&k.as_str()) { low(v) } else { v.clone() }))
            .collect();
        let mut a = f["action"].as_object().cloned().unwrap_or_default();
        for k in ["addLabelIds", "removeLabelIds"] {
            if let Some(Value::Array(l)) = a.get_mut(k) {
                l.sort_by(|x, y| x.as_str().cmp(&y.as_str()));
            }
        }
        if let Some(fw) = a.get_mut("forward") {
            *fw = low(fw);
        }
        json!({ "criteria": c, "action": a })
    };
    norm(x) == norm(y)
}

/// Gmail's search for the mail a filter matches (as Gmail's own "also apply to matching
/// conversations"; spam and the bin are left out, as there). Where unsure it matches less.
pub(crate) fn search_query(c: &Value) -> String {
    let text = |k: &str| c[k].as_str().map(str::trim).filter(|v| !v.is_empty());
    let mut q = vec![];
    if let Some(v) = text("from") {
        q.push(format!("from:({v})"));
    }
    // a filter's "to" also means the copies (Cc, Bcc)
    if let Some(v) = text("to") {
        q.push(format!("{{to:({v}) cc:({v}) bcc:({v})}}"));
    }
    // a filter's subject is a phrase
    if let Some(v) = text("subject") {
        q.push(format!("subject:\"{}\"", v.replace('"', "")));
    }
    if let Some(v) = text("query") {
        q.push(format!("({v})"));
    }
    // "doesn't have": not matching that search (as the filter's own negatedQuery)
    if let Some(v) = text("negatedQuery") {
        q.push(format!("-({v})"));
    }
    if c["hasAttachment"].as_bool() == Some(true) {
        q.push("has:attachment".into());
    }
    if let Some(size) = c["size"].as_u64() {
        q.push(format!("{}:{size}", if c["sizeComparison"] == "smaller" { "smaller" } else { "larger" }));
    }
    // only a condition makes a search: alone this would match the whole mailbox
    if !q.is_empty() && c["excludeChats"].as_bool() == Some(true) {
        q.push("-in:chats".into());
    }
    q.join(" ")
}

/// Saves the rule (a changed one: the new one first, then the old one goes, so a failure keeps
/// it) and, if asked, does the same to the mail already there. Err: the rule wasn't saved;
/// otherwise how many messages changed, and why changing them failed (the rule is saved then).
pub fn save_rule(acc: &Account, form: &RuleForm) -> Result<(usize, Option<String>), String> {
    let token = token(acc)?;
    let filter = form.to_filter();
    let url = format!("{GMAIL}/settings/filters");
    if !form.unchanged() {
        let made = match send(&token, "POST", &url, Some(filter.clone()), "reguły") {
            Err(e) if e.contains("Filter already exists") => None,
            r => Some(r?),
        };
        if let Some(old) = &form.id {
            let old_left = || get(&token, &url, "reguły").map(|all| all["filter"].as_array().is_some_and(|l| l.iter().any(|f| f["id"] == old.as_str())));
            // "already exists": the old rule goes only if another one does the same (a retry after
            // a failed delete); if the one that exists is the old rule itself, it stays
            let delete = made.is_some() || {
                let all = get(&token, &url, "reguły")?;
                let list = all["filter"].as_array().cloned().unwrap_or_default();
                list.iter().any(|f| f["id"] == old.as_str()) && list.iter().any(|f| f["id"] != old.as_str() && same_rule(f, &filter))
            };
            if delete {
                if let Err(e) = send(&token, "DELETE", &format!("{url}/{}", enc(old)), None, "reguły") {
                    match old_left() {
                        // deleted after all, only the answer was lost
                        Ok(false) => {}
                        // the old rule stays: the new one is taken back, so the rules are as before
                        Ok(true) => {
                            if let Some(new) = made.as_ref().and_then(|m| m["id"].as_str()) {
                                if let Err(e2) = send(&token, "DELETE", &format!("{url}/{}", enc(new)), None, "reguły") {
                                    return Err(t!("mail.rules.both_left", e = e2).into_owned());
                                }
                            }
                            return Err(e);
                        }
                        // not known: nothing more is deleted
                        Err(_) => return Err(t!("mail.rules.unsure", e = e).into_owned()),
                    }
                }
            }
        }
    }
    if !form.apply {
        return Ok((0, None));
    }
    Ok(match apply_existing(&token, &filter) {
        Ok(n) => (n, None),
        Err(e) => (0, Some(e)),
    })
}

/// The filter's action done to the matching mail already in the mailbox.
fn apply_existing(token: &str, filter: &Value) -> Result<usize, String> {
    let query = search_query(&filter["criteria"]);
    let a = &filter["action"];
    let (add, remove) = (a["addLabelIds"].clone(), a["removeLabelIds"].clone());
    if query.is_empty() || (add.is_null() && remove.is_null()) {
        return Ok(0);
    }
    let q = enc(&query);
    let (mut ids, mut page) = (Vec::<String>::new(), String::new());
    // shortcut: at most 100 pages (50 000 messages), a larger mailbox would need a progress bar
    for _ in 0..100 {
        let v = get(token, &format!("{GMAIL}/messages?maxResults=500&q={q}{page}"), "reguły")?;
        ids.extend(v["messages"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str().map(String::from)));
        match v["nextPageToken"].as_str() {
            Some(t) => page = format!("&pageToken={}", enc(t)),
            None => break,
        }
    }
    for chunk in ids.chunks(1000) {
        let mut body = json!({ "ids": chunk });
        if !add.is_null() {
            body["addLabelIds"] = add.clone();
        }
        if !remove.is_null() {
            body["removeLabelIds"] = remove.clone();
        }
        send(token, "POST", &format!("{GMAIL}/messages/batchModify"), Some(body), "reguły")?;
    }
    Ok(ids.len())
}

#[cfg(test)]
mod tests {
    #[test]
    fn gmail_rules_read_in_words() {
        let f = serde_json::json!({
            "id": "ANe1",
            "criteria": { "from": "spam@x.pl", "size": 2048, "sizeComparison": "larger" },
            "action": { "addLabelIds": ["TRASH", "Label_7", "CATEGORY_PROMOTIONS"], "removeLabelIds": ["INBOX", "UNREAD"] }
        });
        let r = super::describe(&f, &|id| if id == "Label_7" { "Faktury".into() } else { id.into() });
        assert_eq!(r.id, "ANe1");
        assert_eq!(r.when, "from: spam@x.pl, larger than 2 KB");
        assert_eq!(r.then, "to the Trash, label “Faktury”, category: promotions, skip the Inbox (archive), mark as read");
        let empty = super::describe(&serde_json::json!({ "id": "x" }), &|id| id.into());
        assert_eq!((empty.when.as_str(), empty.then.as_str()), ("", ""));
        for ok in ["spam@x.pl", "a.b+c@mail.example.com", "9x@a-b.pl"] {
            assert!(super::blockable(ok), "{ok}");
        }
        for bad in ["-a@x.pl", "a b@x.pl", "{a b}@x.pl", "a@x", "a@-x.pl", "a@b@x.pl", "ą@x.pl", "", "x.pl", "a@x.pl OR b"] {
            assert!(!super::blockable(bad), "{bad}");
        }
    }

    #[test]
    fn gmail_rule_editor_keeps_what_it_does_not_show() {
        let f = serde_json::json!({
            "id": "F1",
            "criteria": { "from": "news@x.pl", "size": 3000, "sizeComparison": "smaller", "excludeChats": true },
            "action": { "addLabelIds": ["Label_7", "CATEGORY_UPDATES", "STARRED"], "removeLabelIds": ["INBOX", "Label_9"] }
        });
        let mut r = super::RuleForm::from_filter(&f);
        assert_eq!((r.from.as_str(), r.size_kb, r.smaller, r.label.as_str(), r.star, r.archive, r.read), ("news@x.pl", 3, true, "Label_7", true, true, false));
        assert!(r.unchanged());
        r.label = "Label_8".into();
        r.star = false;
        r.read = true;
        r.size_kb = 0;
        r.subject = " faktura ".into();
        let out = r.to_filter();
        assert_eq!(out["criteria"], serde_json::json!({ "from": "news@x.pl", "subject": "faktura", "excludeChats": true }));
        assert_eq!(out["action"], serde_json::json!({ "addLabelIds": ["CATEGORY_UPDATES", "Label_8"], "removeLabelIds": ["Label_9", "INBOX", "UNREAD"] }));
        assert!(!r.unchanged() && r.problem().is_none());
        assert_eq!(super::search_query(&out["criteria"]), "from:(news@x.pl) subject:\"faktura\" -in:chats");
        let all = serde_json::json!({ "to": "a@x.pl", "subject": "a \"b\" c", "query": "x OR y", "negatedQuery": "p q" });
        assert_eq!(super::search_query(&all), "{to:(a@x.pl) cc:(a@x.pl) bcc:(a@x.pl)} subject:\"a b c\" (x OR y) -(p q)");
        // a condition the editor doesn't show is not a search: never the whole mailbox
        assert_eq!(super::search_query(&serde_json::json!({ "excludeChats": true })), "");
        let hidden = super::RuleForm { base: serde_json::json!({ "criteria": { "excludeChats": true } }), read: true, ..Default::default() };
        assert!(hidden.problem().is_some());
        // the same rule in other letters and label order
        let mut same = r.clone();
        same.base = r.to_filter();
        same.id = Some("F1".into());
        same.from = "NEWS@x.pl".into();
        assert!(same.unchanged());
        same.words = "a OR b".into();
        same.base = same.to_filter();
        same.words = "a or b".into();
        assert!(!same.unchanged());
        let empty = super::RuleForm { read: true, ..Default::default() };
        assert!(empty.problem().is_some());
        let nothing = super::RuleForm { from: "a@x.pl".into(), ..Default::default() };
        assert!(nothing.problem().is_some());
    }
}
