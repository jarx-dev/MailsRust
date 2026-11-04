//! Speed measuring (only when POCZTA_PROFILE is set): how long each part of a screen redraw
//! takes, summed up every few seconds into the diagnostics log.

use std::sync::{Mutex, OnceLock};
use std::time::Instant;

fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("POCZTA_PROFILE").is_ok())
}

/// name → (total ms, times, longest ms)
static ACC: Mutex<Vec<(&'static str, f64, u32, f64)>> = Mutex::new(Vec::new());

pub struct Span(&'static str, Instant);

impl Drop for Span {
    fn drop(&mut self) {
        let ms = self.1.elapsed().as_secs_f64() * 1000.0;
        if let Ok(mut a) = ACC.lock() {
            match a.iter_mut().find(|x| x.0 == self.0) {
                Some(x) => {
                    x.1 += ms;
                    x.2 += 1;
                    x.3 = x.3.max(ms);
                }
                None => a.push((self.0, ms, 1, ms)),
            }
        }
    }
}

/// Measures until the returned value is dropped.
pub fn span(name: &'static str) -> Option<Span> {
    on().then(|| Span(name, Instant::now()))
}

/// Writes the sums to the log and starts over.
pub fn report() {
    if !on() {
        return;
    }
    let Ok(mut a) = ACC.lock() else { return };
    let mut v = std::mem::take(&mut *a);
    v.sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap_or(std::cmp::Ordering::Equal));
    for (name, total, n, max) in v {
        crate::diag::log(format!("profile {name}: {:.2} ms per call, longest {:.1} ms, {n} calls", total / n as f64, max));
    }
}
