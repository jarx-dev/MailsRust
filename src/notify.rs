//! Telling the person about mail: the number of unread messages on the program's icons (the
//! taskbar button and the tray) and a signal when new mail comes: a sound, a blinking taskbar
//! button and a small note in the corner of the screen.

use image::{Rgba, RgbaImage};

/// Digits (and "+") drawn on a 3×5 grid, row by row, three bits per row.
fn glyph(c: char) -> [u8; 5] {
    match c {
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b001, 0b001, 0b001],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        '+' => [0b000, 0b010, 0b111, 0b010, 0b000],
        _ => [0; 5],
    }
}

/// How the number is written on a badge: up to 99, then "99+".
pub fn label(n: usize) -> String {
    if n > 99 { "99+".into() } else { n.to_string() }
}

const RED: [u8; 3] = [0xE5, 0x39, 0x35];

fn blend(img: &mut RgbaImage, x: i32, y: i32, c: [u8; 3], k: f32) {
    if x < 0 || y < 0 || x >= img.width() as i32 || y >= img.height() as i32 || k <= 0.0 {
        return;
    }
    let p = img.get_pixel_mut(x as u32, y as u32);
    let k = k.min(1.0);
    let a0 = p[3] as f32 / 255.0;
    let a = k + a0 * (1.0 - k);
    let mix = |src: u8, dst: u8| {
        if a <= 0.0 { 0 } else { ((src as f32 * k + dst as f32 * a0 * (1.0 - k)) / a).round() as u8 }
    };
    *p = Rgba([mix(c[0], p[0]), mix(c[1], p[1]), mix(c[2], p[2]), (a * 255.0).round() as u8]);
}

/// A red rounded badge with `text` in white, `h` pixels tall, its right bottom corner at the
/// picture's right bottom corner (a white rim keeps it apart from the icon under it).
fn badge(img: &mut RgbaImage, text: &str, h: u32, rim: bool) {
    let (w_img, h_img) = (img.width() as f32, img.height() as f32);
    let cells = text.chars().count() as u32 * 4 - 1;
    // the size of one cell of the digits: as big as fits in the height and the picture's width
    let k = ((h as f32 - 4.0) / 6.0).floor().min(((w_img - 6.0) / cells as f32).floor()).max(1.0);
    let tw = cells as f32 * k;
    let th = 5.0 * k;
    let bh = h as f32;
    let bw = (tw + bh * 0.55).max(bh);
    let (x1, y1) = (w_img, h_img);
    let (x0, y0) = (x1 - bw, y1 - bh);
    let r = bh / 2.0;
    let rect = |px: f32, py: f32, grow: f32| {
        // signed distance to the rounded badge
        let cx = px.clamp(x0 + r, x1 - r);
        let cy = (y0 + y1) / 2.0;
        ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() - (r + grow)
    };
    for y in 0..img.height() as i32 {
        for x in 0..img.width() as i32 {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            if rim {
                blend(img, x, y, [255, 255, 255], (0.5 - rect(px, py, 1.5)).clamp(0.0, 1.0));
            }
            blend(img, x, y, RED, (0.5 - rect(px, py, 0.0)).clamp(0.0, 1.0));
        }
    }
    let left = (x0 + (bw - tw) / 2.0).round();
    let top = (y0 + (bh - th) / 2.0).round();
    for (i, c) in text.chars().enumerate() {
        let g = glyph(c);
        for (row, bits) in g.iter().enumerate() {
            for col in 0..3 {
                if bits & (0b100 >> col) != 0 {
                    let gx = left + (i as f32 * 4.0 + col as f32) * k;
                    let gy = top + row as f32 * k;
                    for dy in 0..k as i32 {
                        for dx in 0..k as i32 {
                            blend(img, gx as i32 + dx, gy as i32 + dy, [255, 255, 255], 1.0);
                        }
                    }
                }
            }
        }
    }
}

/// The program's icon at `size` pixels, with the number of unread messages on it (none: as is).
pub fn icon_with_count(size: u32, unread: usize) -> RgbaImage {
    let big = RgbaImage::from_raw(crate::icon::SIZE, crate::icon::SIZE, crate::icon::rgba()).unwrap_or_else(|| RgbaImage::new(crate::icon::SIZE, crate::icon::SIZE));
    let mut img = image::imageops::resize(&big, size, size, image::imageops::FilterType::Lanczos3);
    if unread > 0 {
        badge(&mut img, &label(unread), (size as f32 * 0.6).round() as u32, true);
    }
    img
}

/// Only the badge, filling a `size`×`size` picture (the taskbar's small sign on the button).
pub fn badge_only(size: u32, unread: usize) -> RgbaImage {
    let mut img = RgbaImage::new(size, size);
    badge(&mut img, &label(unread), size, false);
    img
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use windows_sys::core::GUID;
    use windows_sys::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIconFromResourceEx, DestroyIcon, LR_DEFAULTCOLOR};

    const CLSID_TASKBAR_LIST: GUID = GUID::from_u128(0x56FDF344_FD6D_11d0_958A_006097C9A090);
    const IID_TASKBAR_LIST3: GUID = GUID::from_u128(0xEA1AFB91_9E28_4B86_90E9_9E9F8A5EEFAF);

    type HrInit = unsafe extern "system" fn(*mut c_void) -> i32;
    type SetOverlayIcon = unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, *const u16) -> i32;

    /// The taskbar's list of buttons (made once, on the window's own thread).
    pub struct Taskbar(*mut c_void);

    impl Taskbar {
        pub fn new() -> Option<Self> {
            unsafe {
                let _ = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
                let mut p: *mut c_void = std::ptr::null_mut();
                let hr = CoCreateInstance(&CLSID_TASKBAR_LIST, std::ptr::null_mut(), CLSCTX_INPROC_SERVER, &IID_TASKBAR_LIST3, &mut p);
                if hr < 0 || p.is_null() {
                    return None;
                }
                let vt = *(p as *const *const usize);
                let init: HrInit = std::mem::transmute(*vt.add(3));
                if init(p) < 0 {
                    return None;
                }
                Some(Self(p))
            }
        }

        /// A small picture (PNG) on the window's taskbar button, or none.
        pub fn overlay(&self, hwnd: isize, png: Option<&[u8]>, text: &str) {
            unsafe {
                let icon = match png {
                    Some(b) => CreateIconFromResourceEx(b.as_ptr(), b.len() as u32, 1, 0x0003_0000, 0, 0, LR_DEFAULTCOLOR),
                    None => std::ptr::null_mut(),
                };
                let vt = *(self.0 as *const *const usize);
                let set: SetOverlayIcon = std::mem::transmute(*vt.add(18));
                let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
                let _ = set(self.0, hwnd as *mut c_void, icon, wide.as_ptr());
                if !icon.is_null() {
                    DestroyIcon(icon);
                }
            }
        }
    }

    /// Windows' own "new mail" sound.
    pub fn sound() {
        use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ALIAS, SND_ASYNC, SND_NODEFAULT};
        let name: Vec<u16> = "Notification.Mail".encode_utf16().chain(Some(0)).collect();
        unsafe {
            if PlaySoundW(name.as_ptr(), std::ptr::null_mut(), SND_ALIAS | SND_ASYNC | SND_NODEFAULT) == 0 {
                let def: Vec<u16> = "Notification.Default".encode_utf16().chain(Some(0)).collect();
                PlaySoundW(def.as_ptr(), std::ptr::null_mut(), SND_ALIAS | SND_ASYNC);
            }
        }
    }
}

#[cfg(windows)]
pub use imp::{sound, Taskbar};

#[cfg(not(windows))]
pub struct Taskbar;
#[cfg(not(windows))]
impl Taskbar {
    pub fn new() -> Option<Self> {
        None
    }
    pub fn overlay(&self, _: isize, _: Option<&[u8]>, _: &str) {}
}
#[cfg(not(windows))]
pub fn sound() {}

/// A picture as PNG bytes.
pub fn png(img: &RgbaImage) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    let _ = img.write_to(&mut out, image::ImageFormat::Png);
    out.into_inner()
}

/// How "N unread" reads in Polish.
pub fn unread_text(n: usize) -> String {
    match n {
        0 => "brak nieprzeczytanych".into(),
        1 => "1 nieprzeczytana".into(),
        n if (2..=4).contains(&(n % 10)) && !(12..=14).contains(&(n % 100)) => format!("{n} nieprzeczytane"),
        n => format!("{n} nieprzeczytanych"),
    }
}
