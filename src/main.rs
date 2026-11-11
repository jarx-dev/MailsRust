#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[macro_use]
extern crate rust_i18n;
// the interface texts in English and Polish (src/i18n.rs)
i18n!("locales", fallback = "en");

mod ai;
mod app;
mod autostart;
mod book;
mod cli;
mod calendar;
mod chat;
mod compose_view;
mod contacts;
mod db;
mod diag;
mod finder;
mod gcal;
mod google;
mod html;
mod i18n;
mod htmlview;
mod icon;
mod model;
mod net;
mod notify;
mod oauth;
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

    // a test, often on a machine someone is using, is unseen unless POCZTA_VISIBLE=1; a timed run only
    // with its own data folder, or an unseen copy on the real mailbox would swallow every later start
    let timed = std::env::var("POCZTA_CLOSE_AFTER_MS").ok().and_then(|v| v.parse::<u128>().ok()).is_some();
    let test_run = shot.is_some() || (timed && std::env::var_os("POCZTA_DATA_DIR").is_some());
    let unseen = test_run && std::env::var("POCZTA_VISIBLE").as_deref() != Ok("1");
    // (set before the command-line checks: one of them signs in through the browser)
    UNSEEN.store(unseen, std::sync::atomic::Ordering::Relaxed);
    if cli::run(&args) {
        return Ok(());
    }
    #[cfg(windows)]
    if unseen {
        keep_windows_unseen();
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
    // a picture never restores the saved window: a window maximized after its first frames was
    // pictured at its old size (the drawable did not follow), so it opens at its final size
    let shot_settings = shot.as_ref().map(|_| std::env::temp_dir().join("poczta-screenshot-app.ron"));
    if let Some(p) = &shot_settings {
        let _ = std::fs::remove_file(p);
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1376.0, 720.0])
            .with_min_inner_size([960.0, 560.0])
            .with_title("Poczta")
            .with_decorations(false)
            .with_active(!unseen)
            .with_taskbar(!unseen)
            .with_icon(icon),
        persist_window: shot.is_none(),
        // Tests can use a separate data folder so the person's own settings stay untouched.
        persistence_path: shot_settings.or_else(|| std::env::var("POCZTA_DATA_DIR").ok().map(|d| PathBuf::from(d).join("app.ron"))),
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

static UNSEEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A test run that nobody may see: no window on screen, no tray icon, no sound or note.
pub fn unseen() -> bool {
    UNSEEN.load(std::sync::atomic::Ordering::Relaxed)
}

/// Every top-level window of this (the UI) thread is DWM-cloaked before it shows, so it is drawn as
/// usual but never seen, and is never activated. Hooks, because eframe makes the extra windows itself.
#[cfg(windows)]
fn keep_windows_unseen() {
    use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CLOAK};
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetWindowLongW, SetWindowsHookExW, CWPSTRUCT, GWL_STYLE, HCBT_ACTIVATE, WH_CALLWNDPROC, WH_CBT, WM_CREATE, WM_SHOWWINDOW, WM_WINDOWPOSCHANGED, WM_WINDOWPOSCHANGING, WS_CHILD,
    };
    unsafe extern "system" fn on_message(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
        if code >= 0 {
            let m = unsafe { &*(l as *const CWPSTRUCT) };
            // again before every show, move or resize: egui-winit takes the cloak off itself after restoring a saved maximized window
            let showing = matches!(m.message, WM_CREATE | WM_SHOWWINDOW | WM_WINDOWPOSCHANGING | WM_WINDOWPOSCHANGED);
            if showing && unsafe { GetWindowLongW(m.hwnd, GWL_STYLE) } as u32 & WS_CHILD == 0 {
                let on: i32 = 1;
                unsafe { DwmSetWindowAttribute(m.hwnd, DWMWA_CLOAK as _, &on as *const i32 as *const _, 4) };
            }
        }
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, w, l) }
    }
    unsafe extern "system" fn on_cbt(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
        // nonzero refuses the activation: the person's own window keeps the front and the keyboard
        if code == HCBT_ACTIVATE as i32 {
            return 1;
        }
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, w, l) }
    }
    unsafe {
        let me = GetCurrentThreadId();
        let calls = SetWindowsHookExW(WH_CALLWNDPROC, Some(on_message), std::ptr::null_mut(), me);
        let cbt = SetWindowsHookExW(WH_CBT, Some(on_cbt), std::ptr::null_mut(), me);
        // without a hook the test windows would show and take the front: the run ends instead
        if calls.is_null() || cbt.is_null() {
            diag::log("ERROR startup: hiding the test windows failed (SetWindowsHookExW), test run not started");
            std::process::exit(3);
        }
    }
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

