//! The app icon, drawn in code: a blue rounded square with a white envelope
//! and an orange badge with a tick (mail + things done on time).

pub const SIZE: u32 = 256;

type Rgb = [f32; 3];

fn rgb(hex: u32) -> Rgb {
    [
        ((hex >> 16) & 0xFF) as f32 / 255.0,
        ((hex >> 8) & 0xFF) as f32 / 255.0,
        (hex & 0xFF) as f32 / 255.0,
    ]
}

fn sd_round_rect(px: f32, py: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let qx = (px - cx).abs() - (hw - r);
    let qy = (py - cy).abs() - (hh - r);
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

fn sd_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (pax, pay, bax, bay) = (px - ax, py - ay, bx - ax, by - ay);
    let h = ((pax * bax + pay * bay) / (bax * bax + bay * bay)).clamp(0.0, 1.0);
    let (dx, dy) = (pax - bax * h, pay - bay * h);
    (dx * dx + dy * dy).sqrt()
}

fn sd_circle(px: f32, py: f32, cx: f32, cy: f32, r: f32) -> f32 {
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() - r
}

/// Coverage from a signed distance, with a one-pixel soft edge.
fn cov(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

/// RGBA pixels of the icon at `SIZE`×`SIZE`.
pub fn rgba() -> Vec<u8> {
    let s = SIZE as f32 / 256.0;
    let top = rgb(0x3D8EF7);
    let bottom = rgb(0x1B47B0);
    let flap = rgb(0x2F6FE0);
    let orange = rgb(0xF7931E);
    let white = [1.0, 1.0, 1.0];
    let mut out = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (px, py) = ((x as f32 + 0.5) / s, (y as f32 + 0.5) / s);
            let mut col = [0.0f32; 3];
            let mut a = 0.0f32;
            let mut over = |c: Rgb, k: f32| {
                let k = k.clamp(0.0, 1.0);
                for i in 0..3 {
                    col[i] = col[i] * (1.0 - k) + c[i] * k;
                }
                a = a + k * (1.0 - a);
            };
            // Background tile with a diagonal gradient.
            let bg = cov(sd_round_rect(px, py, 128.0, 128.0, 120.0, 120.0, 54.0) * s);
            if bg > 0.0 {
                let t = ((px + py) / 512.0).clamp(0.0, 1.0);
                let g = [
                    top[0] * (1.0 - t) + bottom[0] * t,
                    top[1] * (1.0 - t) + bottom[1] * t,
                    top[2] * (1.0 - t) + bottom[2] * t,
                ];
                over(g, bg);
                // soft light on the upper half
                let shine = (1.0 - py / 128.0).clamp(0.0, 1.0) * 0.10;
                over(white, bg * shine);
            }
            // Envelope shadow, body and flap.
            over([0.05, 0.12, 0.35], cov(sd_round_rect(px, py, 124.0, 146.0, 80.0, 56.0, 14.0) * s - 3.0) * 0.25);
            let body = cov(sd_round_rect(px, py, 120.0, 138.0, 80.0, 56.0, 14.0) * s);
            over(white, body);
            let d1 = sd_segment(px, py, 50.0, 96.0, 120.0, 146.0);
            let d2 = sd_segment(px, py, 120.0, 146.0, 190.0, 96.0);
            over(flap, cov((d1.min(d2) - 6.0) * s) * body);
            // Badge: white ring, orange disc, white tick.
            over(white, cov(sd_circle(px, py, 190.0, 70.0, 40.0) * s));
            over(orange, cov(sd_circle(px, py, 190.0, 70.0, 32.0) * s));
            let t1 = sd_segment(px, py, 174.0, 71.0, 186.0, 83.0);
            let t2 = sd_segment(px, py, 186.0, 83.0, 207.0, 58.0);
            over(white, cov((t1.min(t2) - 5.5) * s));

            out.push((col[0] * 255.0).round() as u8);
            out.push((col[1] * 255.0).round() as u8);
            out.push((col[2] * 255.0).round() as u8);
            out.push((a * 255.0).round() as u8);
        }
    }
    out
}

pub fn icon_data() -> std::sync::Arc<eframe::egui::IconData> {
    std::sync::Arc::new(eframe::egui::IconData {
        rgba: rgba(),
        width: SIZE,
        height: SIZE,
    })
}
