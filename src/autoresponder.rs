//! Gmail's automatic reply, run by the owner's n8n (it answers while this computer is off): its
//! settings are read and written through an n8n webhook (POST, token in `X-Poczta-Token`). The
//! webhook's address and token are kept in autoresponder.json next to the mailbox, the token
//! encrypted (only this Windows account can read it).

use std::time::Duration;

/// The settings as n8n keeps them (one row of its table).
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Rules {
    pub enabled: bool,
    /// the days it answers, as RRRR-MM-DD, both included
    pub date_start: String,
    pub date_end: String,
    /// working hours (Warsaw time): the reply goes before and after them; a start later than the
    /// end goes through midnight (15–13: reply only 13:00–15:00)
    pub hour_start: u8,
    pub hour_end: u8,
    /// only mail sent to this address is answered
    pub target_address: String,
    /// plain text; {od} {do} {start} {koniec} become the dates and hours
    pub text_before: String,
    pub text_after: String,
    /// addresses never answered, one per line
    pub ignored: String,
    /// who changed it last, and when (written by n8n)
    #[serde(skip_serializing)]
    pub updated_by: String,
}

impl Rules {
    /// What is wrong with them, in the person's words (n8n checks these and the texts' length, and refuses).
    pub fn problem(&self) -> Option<String> {
        let day = |d: &str| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok();
        match (day(&self.date_start), day(&self.date_end)) {
            (Some(a), Some(b)) if a <= b => {}
            _ => return Some(t!("ui.settings.autoresponder.bad_dates").into_owned()),
        }
        if self.hour_start > 24 || self.hour_end > 24 || self.hour_start == self.hour_end {
            return Some(t!("ui.settings.autoresponder.bad_hours").into_owned());
        }
        let a = self.target_address.trim();
        if a.contains(char::is_whitespace) || !matches!(a.split_once('@'), Some((who, domain)) if !who.is_empty() && domain.contains('.') && !domain.contains('@')) {
            return Some(t!("ui.settings.autoresponder.bad_address").into_owned());
        }
        None
    }

    /// The settings from JSON (n8n's row, or the AI's changes laid over `base`): a missing or null
    /// field keeps `base`'s value.
    pub fn over(base: &Rules, json: serde_json::Value) -> Result<Rules, String> {
        let serde_json::Value::Object(changes) = json else {
            return Err("not a JSON object".into());
        };
        let mut v = serde_json::to_value(base).map_err(|e| e.to_string())?;
        v["updated_by"] = base.updated_by.clone().into();
        for (k, x) in changes {
            if !x.is_null() {
                v[k] = x;
            }
        }
        serde_json::from_value(v).map_err(|e| e.to_string())
    }
}

/// RRRR-MM-DD as shown in the app (DD-MM-RRRR), and back; None for a day that doesn't exist.
pub fn shown_day(iso: &str) -> String {
    chrono::NaiveDate::parse_from_str(iso, "%Y-%m-%d").map(|d| d.format("%d-%m-%Y").to_string()).unwrap_or_else(|_| iso.to_string())
}
pub fn iso_day(shown: &str) -> Option<String> {
    chrono::NaiveDate::parse_from_str(shown.trim(), "%d-%m-%Y").ok().map(|d| d.format("%Y-%m-%d").to_string())
}

/// Where the webhook is and its token.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Link {
    pub url: String,
    #[serde(skip)]
    pub token: String,
    token_saved: String,
}

impl Link {
    /// An https address and a token that can go in a header (the token never in an error message).
    pub fn is_set(&self) -> bool {
        let t = self.token.trim();
        self.url.trim().starts_with("https://") && !t.is_empty() && t.bytes().all(|b| b.is_ascii_graphic())
    }
}

fn path() -> std::path::PathBuf {
    if let Ok(d) = std::env::var("POCZTA_DATA_DIR") {
        return std::path::PathBuf::from(d).join("autoresponder.json");
    }
    crate::model::cache_dir().parent().map(|p| p.join("autoresponder.json")).unwrap_or_else(|| "autoresponder.json".into())
}

pub fn load() -> Link {
    let mut l: Link = std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    l.token = crate::secret::unprotect(&l.token_saved).unwrap_or_default();
    l
}

pub fn save(l: &Link) -> Result<(), String> {
    let mut l = l.clone();
    l.token_saved = crate::secret::protect(l.token.trim());
    let not_saved = |e: String| t!("ui.settings.autoresponder.not_saved", error = e).into_owned();
    if l.token_saved.is_empty() {
        return Err(not_saved("DPAPI".into()));
    }
    let t = serde_json::to_string_pretty(&l).map_err(|e| not_saved(e.to_string()))?;
    std::fs::write(path(), t).map_err(|e| not_saved(e.to_string()))
}

fn call(l: &Link, body: serde_json::Value) -> Result<Rules, String> {
    if !l.is_set() {
        return Err(t!("ui.settings.autoresponder.no_link").into_owned());
    }
    let tls = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
    // no redirects: ureq would carry the token header to wherever one points
    let agent = ureq::AgentBuilder::new().tls_connector(std::sync::Arc::new(tls)).redirects(0).timeout_connect(Duration::from_secs(5)).timeout(Duration::from_secs(20)).build();
    let resp = agent.post(l.url.trim()).set("X-Poczta-Token", l.token.trim()).send_json(body).map_err(|e| match e {
        ureq::Error::Status(401, _) => t!("ui.settings.autoresponder.token_rejected").into_owned(),
        ureq::Error::Status(400, r) => {
            let said: String = r.into_json::<serde_json::Value>().ok().and_then(|v| v["error"].as_str().map(|s| s.chars().take(300).collect())).unwrap_or_else(|| "HTTP 400".into());
            t!("ui.settings.autoresponder.rejected", error = said).into_owned()
        }
        ureq::Error::Status(code, _) => t!("ui.settings.autoresponder.unavailable", error = format!("HTTP {code}")).into_owned(),
        ureq::Error::Transport(t) => t!("ui.settings.autoresponder.unavailable", error = t.to_string()).into_owned(),
    })?;
    let v: serde_json::Value = resp.into_json().map_err(|_| t!("ui.settings.autoresponder.bad_answer").into_owned())?;
    if v["ok"] != true {
        return Err(t!("ui.settings.autoresponder.bad_answer").into_owned());
    }
    Rules::over(&Rules::default(), v["settings"].clone()).map_err(|_| t!("ui.settings.autoresponder.bad_answer").into_owned())
}

/// The settings n8n has now.
pub fn fetch(l: &Link) -> Result<Rules, String> {
    call(l, serde_json::json!({ "action": "get" }))
}

/// Saves the settings in n8n; the answer is what it keeps.
pub fn store(l: &Link, r: &Rules) -> Result<Rules, String> {
    call(l, serde_json::json!({ "action": "set", "by": "Poczta", "settings": r }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_and_problems() {
        assert_eq!(shown_day("2026-11-19"), "19-11-2026");
        assert_eq!(iso_day(" 19-11-2026 ").as_deref(), Some("2026-11-19"));
        assert_eq!(iso_day("31-02-2026"), None);
        let ok = Rules { date_start: "2026-11-19".into(), date_end: "2026-11-19".into(), hour_start: 15, hour_end: 13, target_address: "a@b.pl".into(), ..Default::default() };
        assert_eq!(ok.problem(), None);
        assert!(Rules { date_end: "2026-11-18".into(), ..ok.clone() }.problem().is_some());
        assert!(Rules { hour_end: 15, ..ok.clone() }.problem().is_some());
        assert!(Rules { hour_start: 25, ..ok.clone() }.problem().is_some());
        assert!(Rules { target_address: "a b@c".into(), ..ok.clone() }.problem().is_some());
        assert!(Rules { target_address: "@".into(), ..ok.clone() }.problem().is_some());
        // n8n's answer carries more fields (id, key, dates of the row): they are ignored; a null
        // column keeps the default
        let r = Rules::over(&Rules::default(), serde_json::json!({ "id": 1, "key": "main", "enabled": true, "hour_start": 15, "text_before": null, "updated_by": "Poczta" })).unwrap();
        assert!(r.enabled && r.hour_start == 15 && r.text_before.is_empty() && r.updated_by == "Poczta");
        assert!(serde_json::to_value(&r).unwrap().get("updated_by").is_none());
        // the AI's partial answer changes only what it names
        let by_ai = Rules::over(&Rules { ignored: "x@y.pl".into(), updated_by: "Poczta".into(), ..ok.clone() }, serde_json::json!({ "hour_start": 16, "enabled": true })).unwrap();
        assert!(by_ai.hour_start == 16 && by_ai.enabled && by_ai.ignored == "x@y.pl" && by_ai.target_address == "a@b.pl" && by_ai.updated_by == "Poczta");
        assert!(Rules::over(&ok, serde_json::json!([1])).is_err());
    }
}
