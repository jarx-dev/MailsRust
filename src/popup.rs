//! The note about new mail in the corner of the screen: a small window of Windows' own, on a
//! thread of its own. It does not depend on the app's main window (which may be minimized,
//! when the app's drawing does not run at all).

/// What the note shows.
pub struct Note {
    pub head: String,
    pub from: String,
    pub subject: String,
    pub dark: bool,
}

#[cfg(windows)]
mod imp {
    use super::Note;
    use std::sync::mpsc::Sender;
    use std::sync::Mutex;
    use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    /// The note shown now: its text, where clicks go, and its window.
    struct Shown {
        note: Note,
        clicked: Sender<()>,
        wake: eframe::egui::Context,
        hwnd: isize,
        scale: f32,
    }

    static SHOWN: Mutex<Option<Shown>> = Mutex::new(None);

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    fn rgb(c: u32) -> COLORREF {
        // 0xRRGGBB → Windows' 0x00BBGGRR
        ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF)
    }

    const W: f32 = 360.0;
    const H: f32 = 86.0;
    const CLOSE_TIMER: usize = 1;

    unsafe fn text(hdc: HDC, s: &str, size: f32, bold: bool, color: u32, r: RECT, scale: f32) {
        let face = wide("Segoe UI");
        let font = CreateFontW(-(size * scale).round() as i32, 0, 0, 0, if bold { 600 } else { 400 }, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr());
        let old = SelectObject(hdc, font);
        SetTextColor(hdc, rgb(color));
        let w: Vec<u16> = s.encode_utf16().collect();
        let mut r = r;
        DrawTextW(hdc, w.as_ptr(), w.len() as i32, &mut r, DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX | DT_VCENTER);
        SelectObject(hdc, old);
        DeleteObject(font);
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        match msg {
            WM_PAINT => {
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                let hdc = BeginPaint(hwnd, &mut ps);
                if let Ok(g) = SHOWN.lock() {
                    if let Some(s) = g.as_ref() {
                        let k = s.scale;
                        let px = |v: f32| (v * k).round() as i32;
                        let (bg, border, title, muted) = if s.note.dark { (0x26282B, 0x3A3D42, 0xE8EAED, 0x9AA0A6) } else { (0xFFFFFF, 0xD9DCE0, 0x222222, 0x6B7178) };
                        let mut rc: RECT = std::mem::zeroed();
                        GetClientRect(hwnd, &mut rc);
                        let b = CreateSolidBrush(rgb(bg));
                        FillRect(hdc, &rc, b);
                        DeleteObject(b);
                        let pen = CreatePen(PS_SOLID, 1, rgb(border));
                        let op = SelectObject(hdc, pen);
                        let ob = SelectObject(hdc, GetStockObject(NULL_BRUSH));
                        Rectangle(hdc, 0, 0, rc.right, rc.bottom);
                        SelectObject(hdc, op);
                        DeleteObject(pen);
                        // the round sign with an envelope
                        let accent = CreateSolidBrush(rgb(0x1E6FD9));
                        SelectObject(hdc, accent);
                        let np = SelectObject(hdc, GetStockObject(NULL_PEN));
                        Ellipse(hdc, px(14.0), px(23.0), px(54.0), px(63.0));
                        SelectObject(hdc, np);
                        SelectObject(hdc, ob);
                        DeleteObject(accent);
                        SetBkMode(hdc, TRANSPARENT as i32);
                        let sym = RECT { left: px(14.0), top: px(23.0), right: px(54.0), bottom: px(63.0) };
                        let face = wide("Segoe UI Symbol");
                        let font = CreateFontW(-px(19.0), 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr());
                        let of = SelectObject(hdc, font);
                        SetTextColor(hdc, rgb(0xFFFFFF));
                        let env: Vec<u16> = "✉".encode_utf16().collect();
                        let mut sr = sym;
                        DrawTextW(hdc, env.as_ptr(), env.len() as i32, &mut sr, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
                        SelectObject(hdc, of);
                        DeleteObject(font);
                        let left = px(66.0);
                        let right = rc.right - px(30.0);
                        text(hdc, &s.note.head, 11.5, false, 0x1E6FD9, RECT { left, top: px(10.0), right, bottom: px(28.0) }, k);
                        text(hdc, &s.note.from, 14.0, true, title, RECT { left, top: px(29.0), right, bottom: px(50.0) }, k);
                        text(hdc, &s.note.subject, 13.0, false, muted, RECT { left, top: px(52.0), right, bottom: px(74.0) }, k);
                        // the close mark
                        text(hdc, "✕", 12.0, false, muted, RECT { left: rc.right - px(26.0), top: px(6.0), right: rc.right - px(6.0), bottom: px(26.0) }, k);
                    }
                }
                EndPaint(hwnd, &ps);
                0
            }
            WM_LBUTTONUP => {
                let x = (lp & 0xFFFF) as i16 as i32;
                let y = ((lp >> 16) & 0xFFFF) as i16 as i32;
                let mut rc: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rc);
                let on_close = if let Ok(g) = SHOWN.lock() {
                    let k = g.as_ref().map(|s| s.scale).unwrap_or(1.0);
                    let close = x > rc.right - (30.0 * k) as i32 && y < (30.0 * k) as i32;
                    if !close {
                        if let Some(s) = g.as_ref() {
                            let _ = s.clicked.send(());
                            s.wake.request_repaint();
                        }
                    }
                    close
                } else {
                    true
                };
                let _ = on_close;
                DestroyWindow(hwnd);
                0
            }
            WM_SETCURSOR => {
                SetCursor(LoadCursorW(std::ptr::null_mut(), IDC_HAND));
                1
            }
            WM_TIMER if wp == CLOSE_TIMER => {
                DestroyWindow(hwnd);
                0
            }
            WM_DESTROY => {
                if let Ok(mut g) = SHOWN.lock() {
                    if g.as_ref().is_some_and(|s| s.hwnd == hwnd as isize) {
                        *g = None;
                    }
                }
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }

    /// Shows the note for `seconds`; a click on it sends a signal to `clicked`. A note still on
    /// screen is replaced.
    pub fn show(note: Note, seconds: u32, clicked: Sender<()>, wake: eframe::egui::Context) {
        // the note before goes away
        if let Ok(g) = SHOWN.lock() {
            if let Some(s) = g.as_ref() {
                unsafe { PostMessageW(s.hwnd as HWND, WM_CLOSE, 0, 0) };
            }
        }
        std::thread::spawn(move || unsafe {
            let inst = GetModuleHandleW(std::ptr::null());
            let class = wide("PocztaNewMailNote");
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW | CS_DROPSHADOW,
                lpfnWndProc: Some(wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: inst,
                hIcon: std::ptr::null_mut(),
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_HAND),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class.as_ptr(),
                hIconSm: std::ptr::null_mut(),
            };
            RegisterClassExW(&wc); // a second time it fails harmlessly: the class is already there
            let scale = GetDpiForSystem().max(96) as f32 / 96.0;
            let (w, h) = ((W * scale).round() as i32, (H * scale).round() as i32);
            let mut work: RECT = std::mem::zeroed();
            SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work as *mut RECT as *mut std::ffi::c_void, 0);
            let margin = (14.0 * scale) as i32;
            let (x, y) = (work.right - w - margin, work.bottom - h - margin);
            let title = wide("Nowa poczta");
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                x,
                y,
                w,
                h,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                inst,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                crate::diag::log("new mail note: the window could not be made");
                return;
            }
            if let Ok(mut g) = SHOWN.lock() {
                *g = Some(Shown { note, clicked, wake, hwnd: hwnd as isize, scale });
            }
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            UpdateWindow(hwnd);
            SetTimer(hwnd, CLOSE_TIMER, seconds * 1000, None);
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        });
    }
}

#[cfg(windows)]
pub use imp::show;

#[cfg(not(windows))]
pub fn show(_: Note, _: u32, _: std::sync::mpsc::Sender<()>, _: eframe::egui::Context) {}
