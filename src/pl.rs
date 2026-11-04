//! Polish names for days and months.
use chrono::{Datelike, NaiveDate};

pub const WD_SHORT: [&str; 7] = ["pon.", "wt.", "śr.", "czw.", "pt.", "sob.", "niedz."];
pub const WD_MIN: [&str; 7] = ["Pn", "Wt", "Śr", "Cz", "Pt", "So", "Nd"];
pub const WD_FULL: [&str; 7] = [
    "poniedziałek",
    "wtorek",
    "środa",
    "czwartek",
    "piątek",
    "sobota",
    "niedziela",
];
const MONTH_NOM: [&str; 12] = [
    "styczeń", "luty", "marzec", "kwiecień", "maj", "czerwiec", "lipiec", "sierpień", "wrzesień",
    "październik", "listopad", "grudzień",
];
const MONTH_GEN: [&str; 12] = [
    "stycznia", "lutego", "marca", "kwietnia", "maja", "czerwca", "lipca", "sierpnia", "września",
    "października", "listopada", "grudnia",
];

pub fn wd_short(d: NaiveDate) -> &'static str {
    WD_SHORT[d.weekday().num_days_from_monday() as usize]
}

pub fn wd_full(d: NaiveDate) -> &'static str {
    WD_FULL[d.weekday().num_days_from_monday() as usize]
}

/// "listopad 2026"
pub fn month_year(d: NaiveDate) -> String {
    format!("{} {}", MONTH_NOM[d.month0() as usize], d.year())
}

/// "3 listopada"
pub fn day_month(d: NaiveDate) -> String {
    format!("{} {}", d.day(), MONTH_GEN[d.month0() as usize])
}

/// "wtorek, 3 listopada 2026"
pub fn long_date(d: NaiveDate) -> String {
    format!("{}, {} {}", wd_full(d), day_month(d), d.year())
}

/// "3 lis"
pub fn short_day_month(d: NaiveDate) -> String {
    let m = MONTH_GEN[d.month0() as usize];
    format!("{} {}", d.day(), &m[..m.char_indices().nth(3).map(|x| x.0).unwrap_or(m.len())])
}
