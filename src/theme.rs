use eframe::egui::{
    self, text::LayoutJob, text::TextWrapping, Align2, Color32, CornerRadius, FontFamily, FontId,
    Painter, Pos2, Sense, Stroke, TextFormat, Ui, Vec2,
};

/// Main button colour, taken from the app icon's blue.

/// The dark theme is on (switched in the menu or with the moon button; remembered).
static DARK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_dark(on: bool) {
    DARK.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn is_dark() -> bool {
    DARK.load(std::sync::atomic::Ordering::Relaxed)
}

/// A colour of the light theme, or of the dark one.
const fn pick(light: u32, dark: u32, is_dark: bool) -> Color32 {
    let c = if is_dark { dark } else { light };
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

macro_rules! themed {
    ($($(#[$m:meta])* $name:ident = $light:expr, $dark:expr;)*) => {
        $(
            $(#[$m])*
            #[allow(non_snake_case)]
            #[inline]
            pub fn $name() -> Color32 {
                pick($light, $dark, is_dark())
            }
        )*
    };
}

themed! {
    /// links, the selected tab, numbers of unread mail
    ACCENT = 0x1E6FD9, 0x6AA6F5;
    ACCENT_SOFT = 0xE3EEFB, 0x23354F;
    SELECTED = 0xEAEAEA, 0x353840;
    HOVER = 0xF4F4F4, 0x2B2E33;
    BORDER = 0xE4E4E4, 0x3A3D44;
    TEXT = 0x222222, 0xE4E6EA;
    MUTED = 0x767676, 0x9AA0A8;
    ICON = 0x444444, 0xC4C8CE;
    /// the background of the window and its panels
    BG = 0xFFFFFF, 0x1E1F22;
    /// cards and boxes a little apart from the background
    SURFACE = 0xF4F5F7, 0x27292E;
    /// second-line text (subjects of read mail and the like)
    TEXT2 = 0x555555, 0xB9BDC4;
    /// faint text (the start of a message in the list)
    FAINT = 0x8A8A8A, 0x8C929A;
    /// the "important" label
    IMPORTANT = 0xFFE38A, 0x5C4812;
    /// behind company logos (most are made for a white page)
    LOGO_BG = 0xFFFFFF, 0xC9CDD3;
}

pub const PRIMARY: Color32 = Color32::from_rgb(0x2D, 0x74, 0xE6);
pub const PRIMARY_HOVER: Color32 = Color32::from_rgb(0x1F, 0x5B, 0xC4);
pub const FLAG: Color32 = Color32::from_rgb(0xE0, 0x3E, 0x3E);

pub const EVENT_COLORS: [Color32; 4] = [
    Color32::from_rgb(0x1E, 0x6F, 0xD9),
    Color32::from_rgb(0x8E, 0x4E, 0xC6),
    Color32::from_rgb(0x2E, 0x9E, 0x5B),
    Color32::from_rgb(0xE0, 0x8A, 0x1E),
];
pub const EVENT_COLOR_NAMES: [&str; 4] = ["Niebieski", "Fioletowy", "Zielony", "Pomarańczowy"];

/// Icons of Segoe Fluent Icons (private code points, so they never replace a letter).
pub mod ic {
    pub const MENU: &str = "\u{E700}";
    pub const REFRESH: &str = "\u{E72C}";
    pub const MAIL: &str = "\u{E715}";
    pub const MAIL_OPEN: &str = "\u{E8C3}";
    pub const FLAG: &str = "\u{E7C1}";
    pub const ARCHIVE: &str = "\u{E7B8}";
    pub const DELETE: &str = "\u{E74D}";
    pub const MOON: &str = "\u{E708}";
    pub const SUN: &str = "\u{E706}";
    pub const CALENDAR: &str = "\u{E787}";
    pub const PERSON: &str = "\u{E77B}";
    pub const REPLY: &str = "\u{E97A}";
    pub const INBOX: &str = "\u{E896}";
    pub const OUTBOX: &str = "\u{E898}";
    pub const SEND: &str = "\u{E724}";
    pub const EDIT: &str = "\u{E70F}";
    pub const BLOCKED: &str = "\u{E733}";
    pub const STAR: &str = "\u{E734}";
    pub const SETTINGS: &str = "\u{E713}";
    pub const LIST: &str = "\u{E8FD}";
    pub const FORWARD: &str = "\u{EE35}";
    /// (no such icon in the set: `paint_icon` draws two reply arrows)
    pub const REPLY_ALL: &str = "\u{E97A}\u{E97A}";
    pub const LINK: &str = "\u{E71B}";
    pub const MORE: &str = "\u{E712}";
    pub const TRANSLATE: &str = "\u{F2B7}";
    pub const OPEN: &str = "\u{E8A7}";
    pub const SEARCH: &str = "\u{E721}";
    pub const CLEAR: &str = "\u{E711}";
    pub const CHECK: &str = "\u{E8FB}";
}

/// An icon of `ic` centred at `at`; "reply all" is the reply arrow with a second arrowhead.
pub fn paint_icon(p: &Painter, at: Pos2, glyph: &str, size: f32, col: Color32) {
    if glyph != ic::REPLY_ALL {
        p.text(at, Align2::CENTER_CENTER, glyph, FontId::proportional(size), col);
        return;
    }
    let shift = size * 0.15;
    let front = p.text(at + Vec2::new(shift, 0.0), Align2::CENTER_CENTER, ic::REPLY, FontId::proportional(size), col);
    // the same arrow further left, cut off just before the front arrow's head: only its head shows
    let clip = egui::Rect::from_min_max(Pos2::new(front.left() - size, front.top()), Pos2::new(front.left() + size * 0.08, front.bottom()));
    p.with_clip_rect(clip.intersect(p.clip_rect()))
        .text(at + Vec2::new(shift - size * 0.3, 0.0), Align2::CENTER_CENTER, ic::REPLY, FontId::proportional(size), col);
}

/// Use the standard Windows UI fonts, with the Windows symbol fonts as a fallback for icons.
fn load_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let dir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let mut have = vec![];
    for (name, file) in [
        ("ui", "segoeui.ttf"),
        ("semibold", "seguisb.ttf"),
        ("symbols", "seguisym.ttf"),
        ("emoji", "seguiemj.ttf"),
        // one outline icon set (`ic`): Windows 11, else the same code points of Windows 10
        ("icons", "SegoeIcons.ttf"),
        ("icons", "segmdl2.ttf"),
    ] {
        if have.iter().any(|h| h == name) {
            continue;
        }
        // used straight from the font files (Windows shares them; no private copy of 17 MB)
        let mapped = std::fs::File::open(format!("{dir}\\Fonts\\{file}")).ok().and_then(|f| unsafe { memmap2::Mmap::map(&f) }.ok());
        if let Some(map) = mapped {
            let map: &'static memmap2::Mmap = Box::leak(Box::new(map));
            let bytes: &'static [u8] = map;
            fonts.font_data.insert(name.to_string(), std::sync::Arc::new(egui::FontData::from_static(bytes)));
            have.push(name.to_string());
        }
    }
    let defaults = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let fallbacks: Vec<String> = ["icons", "symbols", "emoji"]
        .iter()
        .map(|s| s.to_string())
        .filter(|s| have.contains(s))
        .collect();
    let family = |main: &str| -> Vec<String> {
        let mut v = vec![];
        if have.iter().any(|h| h == main) {
            v.push(main.to_string());
        } else if have.iter().any(|h| h == "ui") {
            v.push("ui".to_string());
        }
        v.extend(fallbacks.iter().cloned());
        v.extend(defaults.iter().cloned());
        v
    };
    fonts.families.insert(FontFamily::Proportional, family("ui"));
    fonts
        .families
        .insert(FontFamily::Name("semibold".into()), family("semibold"));
    if let Some(mono) = fonts.families.get_mut(&FontFamily::Monospace) {
        for f in fallbacks.iter().rev() {
            mono.insert(1.min(mono.len()), f.clone());
        }
    }
    ctx.set_fonts(fonts);
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

pub fn apply(ctx: &egui::Context) {
    load_fonts(ctx);
    apply_colors(ctx);
}

/// The toolkit's own colours (text boxes, menus, scroll bars…) for the light or dark theme.
pub fn apply_colors(ctx: &egui::Context) {
    let dark = is_dark();
    let mut visuals = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    visuals.panel_fill = BG();
    visuals.window_fill = if dark { SURFACE() } else { BG() };
    visuals.extreme_bg_color = if dark { Color32::from_rgb(0x17, 0x18, 0x1A) } else { BG() };
    visuals.faint_bg_color = HOVER();
    visuals.selection.bg_fill = if dark { Color32::from_rgb(0x2B, 0x4A, 0x75) } else { ACCENT_SOFT() };
    visuals.selection.stroke = Stroke::new(1.0, ACCENT());
    visuals.hyperlink_color = ACCENT();
    visuals.window_corner_radius = CornerRadius::same(8);
    visuals.window_stroke = Stroke::new(1.0, BORDER());
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT());
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER());
    visuals.widgets.noninteractive.bg_fill = BG();
    for w in [
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(4);
    }
    visuals.widgets.inactive.weak_bg_fill = if dark { SURFACE() } else { BG() };
    visuals.widgets.inactive.bg_fill = if dark { SURFACE() } else { visuals.widgets.inactive.bg_fill };
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER());
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT());
    visuals.widgets.hovered.weak_bg_fill = HOVER();
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, if dark { Color32::from_rgb(0x55, 0x59, 0x60) } else { Color32::from_rgb(0xCC, 0xCC, 0xCC) });
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT());
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, TEXT());
    let theme = if dark { egui::Theme::Dark } else { egui::Theme::Light };
    ctx.set_theme(theme);
    ctx.set_visuals_of(theme, visuals);
    ctx.style_mut_of(theme, |s| {
        s.spacing.item_spacing = Vec2::new(8.0, 6.0);
        s.spacing.button_padding = Vec2::new(10.0, 4.0);
        s.spacing.interact_size.y = 26.0;
        use egui::TextStyle::*;
        s.text_styles.insert(Body, FontId::proportional(13.5));
        s.text_styles.insert(Button, FontId::proportional(13.5));
        s.text_styles.insert(Small, FontId::proportional(11.5));
        s.text_styles.insert(Heading, semibold(18.0));
    });
}

/// Emoji and pictograms can't be drawn in colour here, so they are left out of tag names.
pub fn strip_emoji(s: &str) -> String {
    s.chars()
        .filter(|c| {
            let u = *c as u32;
            !(u >= 0x1F000
                || (0x2190..=0x2BFF).contains(&u)
                || (0x2300..=0x23FF).contains(&u)
                || u == 0x200D
                // ℹ ™ © ® and other symbols used as decoration in tag names
                || u == 0x2139
                || u == 0x24C2
                || (0xFE00..=0xFE0F).contains(&u)
                || (0xE000..=0xF8FF).contains(&u))
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// How a tag is shown: without emoji, and for nested tags ("n8n/Support") only the last part.
pub fn display_label(name: &str) -> String {
    let last = name.rsplit('/').next().unwrap_or(name);
    let clean = strip_emoji(last);
    if clean.is_empty() { strip_emoji(name) } else { clean }
}

/// Soft background and readable text colour for a tag, always the same for the same name.
pub fn tag_colors(name: &str) -> (Color32, Color32) {
    const P: [(Color32, Color32); 8] = [
        (Color32::from_rgb(0xFD, 0xE8, 0xEF), Color32::from_rgb(0xB0, 0x2A, 0x5B)),
        (Color32::from_rgb(0xE3, 0xEE, 0xFB), Color32::from_rgb(0x1E, 0x5F, 0xB8)),
        (Color32::from_rgb(0xE4, 0xF4, 0xEA), Color32::from_rgb(0x1F, 0x7A, 0x45)),
        (Color32::from_rgb(0xFD, 0xF0, 0xDC), Color32::from_rgb(0x9A, 0x5B, 0x0B)),
        (Color32::from_rgb(0xEF, 0xE7, 0xFB), Color32::from_rgb(0x6B, 0x3F, 0xB5)),
        (Color32::from_rgb(0xDF, 0xF4, 0xF6), Color32::from_rgb(0x12, 0x73, 0x80)),
        (Color32::from_rgb(0xF1, 0xF1, 0xF1), Color32::from_rgb(0x55, 0x55, 0x55)),
        (Color32::from_rgb(0xFF, 0xF6, 0xCC), Color32::from_rgb(0x85, 0x6A, 0x00)),
    ];
    let h = name.bytes().fold(5381u32, |a, b| a.wrapping_mul(33) ^ b as u32);
    let (bg, fg) = P[(h as usize) % P.len()];
    if is_dark() {
        // dark theme: a dark tinted label with its colour in the text, nothing bright
        let mix = |a: Color32, b: Color32, t: f32| {
            let m = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t) as u8;
            Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
        };
        return (mix(BG(), fg, 0.28), mix(fg, Color32::WHITE, 0.55));
    }
    (bg, fg)
}

/// A small coloured tag ("chip") drawn at `pos` (top-left). Returns its width.
pub fn tag_chip(painter: &Painter, pos: Pos2, name: &str, size: f32, max_w: f32) -> f32 {
    // the colour comes from the tag name alone, without the " ✕" some chips carry
    let (bg, fg) = tag_colors(name.trim_end_matches('✕').trim_end());
    let mut job = LayoutJob::single_section(
        name.to_string(),
        TextFormat { font_id: FontId::proportional(size), color: fg, ..Default::default() },
    );
    job.wrap = TextWrapping::truncate_at_width((max_w - 12.0).max(10.0));
    let galley = painter.layout_job(job);
    let w = galley.size().x + 12.0;
    let h = galley.size().y + 2.0;
    painter.rect_filled(egui::Rect::from_min_size(pos, Vec2::new(w, h)), CornerRadius::same(4), bg);
    painter.galley(Pos2::new(pos.x + 6.0, pos.y + 1.0), galley, fg);
    w
}

pub fn avatar_color(name: &str) -> Color32 {
    const P: [Color32; 8] = [
        Color32::from_rgb(0x4C, 0x8B, 0xF5),
        Color32::from_rgb(0xE0, 0x6C, 0x75),
        Color32::from_rgb(0x2E, 0xA0, 0x6F),
        Color32::from_rgb(0xD9, 0x8B, 0x2B),
        Color32::from_rgb(0x8E, 0x6B, 0xD8),
        Color32::from_rgb(0x1E, 0x9F, 0xB4),
        Color32::from_rgb(0xC2, 0x5B, 0xA8),
        Color32::from_rgb(0x6B, 0x7F, 0x99),
    ];
    let h = name
        .bytes()
        .fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    P[(h as usize) % P.len()]
}

pub fn avatar(painter: &Painter, center: Pos2, r: f32, name: &str) {
    painter.circle_filled(center, r, avatar_color(name));
    painter.text(
        center,
        Align2::CENTER_CENTER,
        crate::model::initials(name),
        semibold(r * 0.75),
        Color32::WHITE,
    );
}

/// Single line of text, cut with "…" when wider than `max_w`. Returns the width used.
pub fn line_f(painter: &Painter, pos: Pos2, text: &str, font: FontId, color: Color32, max_w: f32) -> f32 {
    let mut job = LayoutJob::single_section(
        text.to_string(),
        TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job.wrap = TextWrapping::truncate_at_width(max_w.max(1.0));
    let galley = painter.layout_job(job);
    let w = galley.size().x;
    painter.galley(pos, galley, color);
    w
}

pub fn line(painter: &Painter, pos: Pos2, text: &str, size: f32, color: Color32, max_w: f32) -> f32 {
    line_f(painter, pos, text, FontId::proportional(size), color, max_w)
}

pub fn text_width(ui: &Ui, text: &str, font: FontId) -> f32 {
    ui.painter().layout_no_wrap(text.to_string(), font, TEXT()).size().x
}

/// Flat toolbar button: icon and label on one line.
pub fn tool_button(ui: &mut Ui, icon: &str, label: &str, enabled: bool) -> bool {
    let icon_w = text_width(ui, icon, FontId::proportional(16.0));
    let label_w = if label.is_empty() { 0.0 } else { text_width(ui, label, FontId::proportional(13.5)) + 6.0 };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(icon_w + label_w + 14.0, 30.0), Sense::click());
    let p = ui.painter();
    if enabled && resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(4), HOVER());
    }
    let col = if enabled { TEXT() } else { Color32::from_rgb(0xB8, 0xB8, 0xB8) };
    p.text(
        Pos2::new(rect.left() + 7.0, rect.center().y),
        Align2::LEFT_CENTER,
        icon,
        FontId::proportional(16.0),
        if enabled { ICON() } else { col },
    );
    if !label.is_empty() {
        p.text(
            Pos2::new(rect.left() + 7.0 + icon_w + 6.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(13.5),
            col,
        );
    }
    let resp = if label.is_empty() { resp } else { resp };
    enabled && resp.clicked()
}

/// The orange primary button ("+ Nowy").
pub fn primary_button(ui: &mut Ui, label: &str) -> bool {
    let w = text_width(ui, label, FontId::proportional(14.0)) + 44.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 32.0), Sense::click());
    let fill = if resp.hovered() { PRIMARY_HOVER } else { PRIMARY };
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(4), fill);
    p.text(Pos2::new(rect.left() + 18.0, rect.center().y), Align2::CENTER_CENTER, "+", FontId::proportional(20.0), Color32::WHITE);
    p.text(Pos2::new(rect.left() + 32.0, rect.center().y), Align2::LEFT_CENTER, label, FontId::proportional(14.0), Color32::WHITE);
    resp.clicked()
}

pub fn tool_sep(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(9.0, 24.0), Sense::hover());
    ui.painter().vline(rect.center().x, rect.y_range(), Stroke::new(1.0, BORDER()));
}


/// A settings window in the app's own look: a white (or dark) card with the app's border and
/// a soft shadow over a dimmed app, the title on top with a close button, its size following
/// what is inside and never bigger than the screen (what does not fit scrolls). Escape or a
/// click beside it closes it. Returns what `add` returned while it is open.
pub fn dialog<R>(ctx: &egui::Context, id: &str, title: &str, width: f32, open: &mut bool, add: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    if !*open {
        return None;
    }
    let screen = ctx.content_rect();
    let width = width.min(screen.width() - 72.0).max(280.0);
    let max_h = (screen.height() - 120.0).max(220.0);
    let frame = egui::Frame::new()
        .fill(BG())
        .stroke(Stroke::new(1.0, BORDER()))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(egui::Margin { left: 22, right: 18, top: 14, bottom: 18 })
        .shadow(egui::epaint::Shadow { offset: [0, 8], blur: 30, spread: 0, color: Color32::from_black_alpha(if is_dark() { 140 } else { 70 }) });
    #[allow(deprecated)]
    let modal_id = egui::Id::new(("dialog", id));
    let shown = egui::Modal::new(modal_id)
        .frame(frame)
        .backdrop_color(Color32::from_black_alpha(if is_dark() { 120 } else { 70 }))
        .show(ctx, |ui| {
            ui.set_width(width);
            let mut close = false;
            // title and the close button
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(title).font(semibold(18.0)).color(TEXT()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (r, resp) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
                    if resp.hovered() {
                        ui.painter().rect_filled(r, CornerRadius::same(6), HOVER());
                    }
                    let c = r.center();
                    let s = 5.5;
                    let col = if resp.hovered() { TEXT() } else { MUTED() };
                    ui.painter().line_segment([c + Vec2::new(-s, -s), c + Vec2::new(s, s)], Stroke::new(1.6, col));
                    ui.painter().line_segment([c + Vec2::new(-s, s), c + Vec2::new(s, -s)], Stroke::new(1.6, col));
                    if resp.on_hover_text("Zamknij (Esc)").clicked() {
                        close = true;
                    }
                });
            });
            ui.add_space(8.0);
            let line = ui.available_rect_before_wrap();
            ui.painter().hline(line.left() - 22.0..=line.right() + 18.0, line.top(), Stroke::new(1.0, BORDER()));
            ui.add_space(12.0);
            // the content: as tall as it needs, at most the screen; then it scrolls
            let out = egui::ScrollArea::vertical()
                .id_salt(("dialog-body", id))
                .max_height(max_h - 70.0)
                // with much inside, use the screen's height (not just the room left below the
                // middle of the screen, where the window starts)
                .min_scrolled_height(max_h - 70.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.set_width(width - 10.0);
                    ui.spacing_mut().item_spacing.y = 8.0;
                    add(ui)
                })
                .inner;
            (out, close)
        });
    let outside = shown.should_close();
    let (out, close) = shown.inner;
    if close || outside {
        *open = false;
    }
    Some(out)
}
