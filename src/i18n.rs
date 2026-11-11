//! The interface language (English or Polish, picked in the ☰ menu): texts live in `locales/*.yml`
//! and are read with the `t!` macro; plural forms and the names of days and months are here.
use chrono::{Datelike, NaiveDate};

pub const LANGS: [(&str, &str); 2] = [("en", "English"), ("pl", "Polski")];

/// Switches every text drawn from now on (the next frame already uses it).
pub fn set(lang: &str) {
    rust_i18n::set_locale(if lang == "pl" { "pl" } else { "en" });
}

pub fn is_pl() -> bool {
    &*rust_i18n::locale() == "pl"
}

/// `key.one|few|many` in Polish (1 wiadomość, 2 wiadomości, 5 wiadomości), `key.one|other` in
/// English, with `%{n}` replaced by the number.
pub fn plural(key: &str, n: usize) -> String {
    let form = plural_form(n, is_pl());
    t!(format!("{key}.{form}"), n = n).into_owned()
}

fn plural_form(n: usize, pl: bool) -> &'static str {
    match (pl, n) {
        (_, 1) => "one",
        (false, _) => "other",
        (true, n) if (2..=4).contains(&(n % 10)) && !(12..=14).contains(&(n % 100)) => "few",
        (true, _) => "many",
    }
}

const WD_SHORT: [[&str; 7]; 2] = [
    ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
    ["pon.", "wt.", "śr.", "czw.", "pt.", "sob.", "niedz."],
];
const WD_MIN: [[&str; 7]; 2] = [["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"], ["Pn", "Wt", "Śr", "Cz", "Pt", "So", "Nd"]];
const WD_FULL: [[&str; 7]; 2] = [
    ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"],
    ["poniedziałek", "wtorek", "środa", "czwartek", "piątek", "sobota", "niedziela"],
];
const MONTH_NOM: [[&str; 12]; 2] = [
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"],
    [
        "styczeń", "luty", "marzec", "kwiecień", "maj", "czerwiec", "lipiec", "sierpień", "wrzesień", "październik", "listopad",
        "grudzień",
    ],
];
const MONTH_GEN: [[&str; 12]; 2] = [
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"],
    [
        "stycznia", "lutego", "marca", "kwietnia", "maja", "czerwca", "lipca", "sierpnia", "września", "października", "listopada",
        "grudnia",
    ],
];

fn l() -> usize {
    is_pl() as usize
}

/// Two-letter weekday names from Monday, for the calendar's header row.
pub fn wd_min() -> [&'static str; 7] {
    WD_MIN[l()]
}

/// "Mon" / "pon."
pub fn wd_short(d: NaiveDate) -> &'static str {
    WD_SHORT[l()][d.weekday().num_days_from_monday() as usize]
}

/// "Monday" / "poniedziałek"
pub fn wd_full(d: NaiveDate) -> &'static str {
    WD_FULL[l()][d.weekday().num_days_from_monday() as usize]
}

/// "November 2026" / "listopad 2026"
pub fn month_year(d: NaiveDate) -> String {
    format!("{} {}", MONTH_NOM[l()][d.month0() as usize], d.year())
}

/// "3 November" / "3 listopada"
pub fn day_month(d: NaiveDate) -> String {
    format!("{} {}", d.day(), MONTH_GEN[l()][d.month0() as usize])
}

/// "3 Nov" / "3 lis"
pub fn short_day_month(d: NaiveDate) -> String {
    let m = MONTH_GEN[l()][d.month0() as usize];
    format!("{} {}", d.day(), &m[..m.char_indices().nth(3).map(|x| x.0).unwrap_or(m.len())])
}

/// A full date: "8 Nov 2026" / "08-11-2026" (no longer than the Polish one, for narrow columns).
pub fn date(d: NaiveDate) -> String {
    d.format(if is_pl() { "%d-%m-%Y" } else { "%-d %b %Y" }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn plural_forms() {
        let pl: Vec<_> = [1, 2, 4, 5, 11, 12, 14, 21, 22, 25, 112, 122].iter().map(|&n| plural_form(n, true)).collect();
        assert_eq!(pl, ["one", "few", "few", "many", "many", "many", "many", "many", "few", "many", "many", "few"]);
        assert_eq!([0, 1, 2, 5].map(|n| plural_form(n, false)), ["other", "one", "other", "other"]);
    }

    /// Plural keys end in .one/.few/.many (Polish) or .one/.other (English); every other key and
    /// every plural group exists in both languages.
    fn keys(lang: &str) -> BTreeSet<String> {
        let all = crate::_RUST_I18N_BACKEND.messages_for_locale(lang).expect("locale loaded");
        all.into_iter()
            .map(|(k, _)| {
                let k = k.into_owned();
                match k.rsplit_once('.') {
                    Some((base, "one" | "few" | "many" | "other")) => format!("{base}.#"),
                    _ => k,
                }
            })
            .collect()
    }

    #[test]
    fn every_key_in_both_languages() {
        let (en, pl) = (keys("en"), keys("pl"));
        let only_en: Vec<_> = en.difference(&pl).collect();
        let only_pl: Vec<_> = pl.difference(&en).collect();
        assert!(only_en.is_empty() && only_pl.is_empty(), "only in en: {only_en:?}\nonly in pl: {only_pl:?}");
        let has = |lang: &str, k: &str| crate::_RUST_I18N_BACKEND.translate(lang, k).is_some();
        for base in en.iter().filter_map(|k| k.strip_suffix(".#")) {
            for f in ["one", "other"] {
                assert!(has("en", &format!("{base}.{f}")), "en {base}.{f}");
            }
            for f in ["one", "few", "many"] {
                assert!(has("pl", &format!("{base}.{f}")), "pl {base}.{f}");
            }
        }
    }

    /// Every `t!("…")` and `plural("…")` key written in the code has a text (a missing one would
    /// show the bare key on screen).
    #[test]
    fn every_used_key_exists() {
        let en = keys("en");
        let mut missing = vec![];
        let mut stack = vec![std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))];
        while let Some(p) = stack.pop() {
            if p.is_dir() {
                stack.extend(std::fs::read_dir(&p).unwrap().map(|e| e.unwrap().path()));
                continue;
            }
            let code = std::fs::read_to_string(&p).unwrap();
            for (call, plural) in [("t!(\"", false), ("plural(\"", true)] {
                for (at, _) in code.match_indices(call) {
                    // not `format!(` and the like
                    if code[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_') {
                        continue;
                    }
                    let rest = &code[at + call.len()..];
                    let key = &rest[..rest.find('"').unwrap()];
                    let want = if plural { format!("{key}.#") } else { key.to_string() };
                    // (the doc comment above)
                    if !en.contains(&want) && key != "…" {
                        missing.push(format!("{}: {key}", p.display()));
                    }
                }
            }
        }
        assert!(missing.is_empty(), "keys without a text: {missing:#?}");
    }

}
