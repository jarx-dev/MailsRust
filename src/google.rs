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
                let api = if what == "kalendarz" { "Google Calendar API" } else { "People API" };
                return format!("w Twoim projekcie Google Cloud nie jest włączone {api} — włącz je (przycisk w tym oknie), odczekaj minutę i spróbuj ponownie");
            }
            if code == 403 && (body.contains("insufficient") || body.contains("ACCESS_TOKEN_SCOPE_INSUFFICIENT") || body.contains("PERMISSION_DENIED")) {
                return "brak zgody na dostęp — kliknij „Zaloguj ponownie przez Google” i zaznacz dostęp do kalendarza i kontaktów".into();
            }
            if code == 401 {
                crate::oauth::forget_tokens();
                return "Google nie przyjął logowania — zaloguj się ponownie przez Google".into();
            }
            let msg = serde_json::from_str::<Value>(&body).ok().and_then(|v| v["error"]["message"].as_str().map(String::from)).unwrap_or(body);
            format!("Google ({what}) odpowiedział {code}: {}", msg.chars().take(200).collect::<String>())
        }
        other => format!("brak połączenia z Google: {other}"),
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
                let title = e["summary"].as_str().filter(|t| !t.trim().is_empty()).unwrap_or("(bez tytułu)").to_string();
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
    v["id"].as_str().map(String::from).ok_or_else(|| "Google nie potwierdził zapisu wydarzenia".into())
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
        return v["resourceName"].as_str().map(String::from).ok_or_else(|| "Google nie potwierdził utworzenia grupy".into());
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
        return v["resourceName"].as_str().map(String::from).ok_or_else(|| "Google nie potwierdził zapisu kontaktu".into());
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
        format!("kalendarz działa ({n} {})", pl(n, "kalendarz", "kalendarze", "kalendarzy"))
    });
    let people = contacts(&token).map(|(g, p)| format!("kontakty działają ({} {}, {} {})", p.len(), pl(p.len(), "kontakt", "kontakty", "kontaktów"), g.len(), pl(g.len(), "grupa", "grupy", "grup")));
    (cal, people)
}

fn pl(n: usize, one: &'static str, few: &'static str, many: &'static str) -> &'static str {
    match n {
        1 => one,
        n if (2..=4).contains(&(n % 10)) && !(12..=14).contains(&(n % 100)) => few,
        _ => many,
    }
}
