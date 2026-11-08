#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai;
mod app;
mod autostart;
mod book;
mod cli;
mod calendar;
mod compose_view;
mod contacts;
mod db;
mod diag;
mod finder;
mod gcal;
mod google;
mod html;
mod htmlview;
mod icon;
mod model;
mod net;
mod notify;
mod oauth;
mod pl;
mod popup;
mod prof;
mod rawimap;
mod search;
mod secret;
mod semantic;
mod rich;
mod share;
mod signature;
mod theme;
mod translate;
mod unigram;
mod update;
mod webview;

use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let launched = std::time::Instant::now();
    // an unexpected error is written to the log (where and what), not lost with the window
    std::panic::set_hook(Box::new(|info| {
        let place = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let what = info.payload().downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| info.payload().downcast_ref::<String>().cloned()).unwrap_or_default();
        let thread = std::thread::current().name().unwrap_or("?").to_string();
        diag::log(format!("ERROR (panic) in thread {thread} at {place}: {what}
{}", std::backtrace::Backtrace::force_capture()));
    }));
    let args: Vec<String> = std::env::args().collect();
    // the previous build watching over a freshly installed one (see update.rs)
    if update::run_guard(&args) {
        return Ok(());
    }
    update::finish_pending_update(&args);
    let shot = args
        .iter()
        .position(|a| a == "--screenshot")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);

    if cli::run(&args) {
        return Ok(());
    }

    // one copy of the program: started again, it brings the running one to the front and ends
    // (a test copy with its own data folder counts as a program of its own). A new build that
    // finds a copy started meanwhile (during the swap) still reports itself ready, or the update
    // guard would take the good build back.
    #[cfg(windows)]
    if shot.is_none() && app::another_copy_shown() {
        update::startup_complete(true);
        return Ok(());
    }

    // a window size saved while the window was minimized (tiny) is forgotten: the window then
    // opens at its normal size instead of as a small strip in the corner
    {
        let file = std::env::var("POCZTA_DATA_DIR")
            .ok()
            .map(|d| PathBuf::from(d).join("app.ron"))
            .or_else(|| eframe::storage_dir("Mailroom").map(|d| d.join("app.ron")));
        if let Some(file) = file {
            if let Ok(text) = std::fs::read_to_string(&file) {
                if let Some(fixed) = forget_tiny_window(&text) {
                    let _ = std::fs::write(&file, fixed);
                    diag::log("startup: the saved window size was too small, using the normal size");
                }
            }
        }
    }
    let t = std::time::Instant::now();
    let icon = icon::icon_data();
    diag::took("startup: window icon", t, "");
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1376.0, 720.0])
            .with_min_inner_size([960.0, 560.0])
            .with_title("Poczta")
            .with_decorations(false)
            .with_icon(icon),
        persist_window: shot.is_none(),
        // Tests can use a separate data folder so the person's own settings stay untouched.
        persistence_path: std::env::var("POCZTA_DATA_DIR").ok().map(|d| PathBuf::from(d).join("app.ron")),
        ..Default::default()
    };
    diag::took("startup: program ready to open its window", launched, "");
    // the mailbox is read while the window is being made
    let preload = (shot.is_none() || std::env::var("POCZTA_SHOT_REAL").is_ok()).then(app::start_loading);
    let r = eframe::run_native(
        "Mailroom",
        options,
        Box::new(move |cc| {
            diag::took("startup: window and graphics ready", launched, "");
            Ok(Box::new(app::MailApp::new(cc, shot, preload)))
        }),
    );
    diag::log("closing: app finished");
    r
}

/// The settings without their "window" entry when the saved window is too small to use.
fn forget_tiny_window(text: &str) -> Option<String> {
    let line_start = text.find("\"window\":")?;
    let line_end = text[line_start..].find('\n').map(|e| line_start + e + 1).unwrap_or(text.len());
    let line = &text[line_start..line_end];
    let size_at = line.find("inner_size_points:(x:")? + "inner_size_points:(x:".len();
    let rest = &line[size_at..];
    let x: f32 = rest.split(',').next()?.trim().parse().ok()?;
    let y: f32 = rest.split("y:").nth(1)?.split(')').next()?.trim().parse().ok()?;
    if x >= 700.0 && y >= 400.0 {
        return None;
    }
    Some(format!("{}{}", &text[..line_start], &text[line_end..]))
}

/// "osoba / osoby / osób" for a number of people.
pub fn pl_people(n: usize) -> &'static str {
    match n {
        1 => "osoba",
        n if (2..=4).contains(&(n % 10)) && !(12..=14).contains(&(n % 100)) => "osoby",
        _ => "osób",
    }
}
