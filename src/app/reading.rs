use super::*;

/// A picture used inside an email.
#[derive(Clone)]
pub(super) enum ImgState {
    Loading,
    Ready(Arc<[u8]>),
    Failed,
}

/// A message opened in a window of its own.
pub(super) struct ReaderWin {
    pub(super) key: u64,
    /// the message as it was when opened (with its text)
    pub(super) msg: Message,
    /// the formatted version, shown by the Edge engine in this window
    pub(super) web: Option<(wry::WebView, wry::WebContext)>,
    pub(super) failed: bool,
    pub(super) tries: u32,
    pub(super) last: Option<Rect>,
}

/// An attachment shown in a window of its own (PDF, a picture, a text).
pub(super) struct PreviewWin {
    pub(super) key: u64,
    pub(super) msg: u64,
    pub(super) index: usize,
    pub(super) name: String,
    /// the copy with its real name
    pub(super) path: String,
    pub(super) web: Option<(wry::WebView, wry::WebContext)>,
    pub(super) failed: bool,
    pub(super) tries: u32,
    pub(super) last: Option<Rect>,
}

/// Attachments the app can show itself (the Edge engine shows PDFs, pictures and texts).
pub(super) fn previewable(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    ["pdf", "png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "txt", "log", "csv", "json", "xml", "htm", "html", "mp4", "webm", "mp3", "wav"].contains(&ext.as_str())
}

/// The tag in an attachment window's title.
pub(super) fn preview_marker(key: u64) -> String {
    format!("{}\u{2061}", crate::compose_view::marker(key))
}

/// The tag in a reading window's title, so its window can be found (different from the
/// message windows' tags).
pub(super) fn reader_marker(key: u64) -> String {
    format!("{}\u{2060}", crate::compose_view::marker(key))
}

impl MailApp {
    /// Pictures of emails read earlier are let go once they add up to a lot (they are read again
    /// from the disk cache if needed); company logos stay.
    pub(super) fn trim_pictures(&mut self) {
        let total: usize = self.images.values().map(|s| if let ImgState::Ready(b) = s { b.len() } else { 0 }).sum();
        if total < 24 * 1024 * 1024 {
            return;
        }
        self.images.retain(|k, _| k.starts_with("logo:"));
        self.html_cache = None;
        self.card_html.clear();
        // the decoded pictures the screen kept as well
        self.ctx.forget_all_images();
        crate::diag::log(format!("memory: pictures of earlier emails let go ({} MB)", total / 1_048_576));
    }

    /// Makes sure a message's text is in memory (texts stay on disk until they are needed).
    pub(super) fn ensure_body(&mut self, id: u64) {
        if !self.msg(id).is_some_and(|m| m.on_disk) {
            return;
        }
        let text = self.db.as_mut().and_then(|d| d.text(id));
        if let Some(m) = self.msg_mut(id) {
            m.body = text.unwrap_or_default();
            m.on_disk = false;
        }
    }

    /// A message shown in the reading pane for a moment counts as read (as if it had been
    /// clicked): after 1 second.
    pub(super) fn read_after_a_while(&mut self, ctx: &egui::Context) {
        const AFTER: f64 = 1.0;
        let now = self.clock();
        let shown = self.selected.filter(|_| self.module == Module::Mail && self.shot.is_none() && !ctx.input(|i| i.viewport().minimized.unwrap_or(false)));
        let Some(id) = shown else {
            // not on screen: counting starts again when it is
            self.read_timer = None;
            return;
        };
        match self.read_timer {
            Some((t, _, _)) if t == id => {}
            _ => self.read_timer = Some((id, now, false)),
        }
        let Some((_, since, done)) = self.read_timer else { return };
        if done {
            return;
        }
        if !self.msg(id).is_some_and(|m| !m.read) {
            self.read_timer = Some((id, since, true));
            return;
        }
        if now - since >= AFTER {
            self.read_timer = Some((id, since, true));
            self.actions.push(Action::SetRead(id, true));
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(AFTER - (now - since) + 0.05));
        }
    }

    /// The formatted version of a message, read from the cache once and kept while it is shown.
    pub(super) fn html_blocks(&mut self, m: &Message) -> Option<Arc<Vec<html::Block>>> {
        if m.html_path.is_empty() {
            return None;
        }
        if let Some((id, b)) = &self.html_cache {
            if *id == m.id {
                return Some(b.clone());
            }
        }
        let text = std::fs::read_to_string(&m.html_path).ok()?;
        let blocks = Arc::new(html::parse(&text));
        self.html_cache = Some((m.id, blocks.clone()));
        Some(blocks)
    }

    /// The formatted version of a message outside the reading pane (a conversation card, a
    /// reading window), read and parsed once per message, not on every redraw.
    pub(super) fn card_blocks(&mut self, m: &Message) -> Option<Arc<Vec<html::Block>>> {
        if m.html_path.is_empty() {
            return None;
        }
        if let Some(b) = self.card_html.get(&m.id) {
            return Some(b.clone());
        }
        let b = Arc::new(html::parse(&std::fs::read_to_string(&m.html_path).ok()?));
        self.card_html.insert(m.id, b.clone());
        Some(b)
    }

    /// Bytes of a picture used in an email; starts a download for pictures from the web.
    pub(super) fn image_bytes(&mut self, m: &Message, src: &str) -> (String, ImgState) {
        use std::hash::{Hash, Hasher};
        let lower = src.to_lowercase();
        if let Some(cid) = lower.strip_prefix("cid:") {
            let file = m
                .inline
                .iter()
                .find(|(c, _)| c.eq_ignore_ascii_case(cid) || c.eq_ignore_ascii_case(&src[4..]))
                .map(|(_, f)| f.clone());
            let Some(file) = file else { return (src.to_string(), ImgState::Failed) };
            let key = format!("file:{file}");
            let state = self
                .images
                .entry(key.clone())
                .or_insert_with(|| match std::fs::read(&file) {
                    Ok(b) => ImgState::Ready(Arc::from(b)),
                    Err(_) => ImgState::Failed,
                })
                .clone();
            return (key, state);
        }
        if lower.starts_with("data:image/") {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            src.hash(&mut h);
            let key = format!("data:{:016x}", h.finish());
            let state = self
                .images
                .entry(key.clone())
                .or_insert_with(|| {
                    use base64::Engine;
                    src.split_once(',')
                        .and_then(|(_, d)| base64::engine::general_purpose::STANDARD.decode(d.trim()).ok())
                        .map(|b| ImgState::Ready(Arc::from(b)))
                        .unwrap_or(ImgState::Failed)
                })
                .clone();
            return (key, state);
        }
        if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("//") {
            let url = if lower.starts_with("//") { format!("https:{src}") } else { src.to_string() };
            if !self.images.contains_key(&url) {
                self.images.insert(url.clone(), ImgState::Loading);
                net::fetch_image(url.clone(), self.img_tx.clone(), self.ctx.clone());
            }
            let state = self.images[&url].clone();
            return (url, state);
        }
        (src.to_string(), ImgState::Failed)
    }

    pub(super) fn render_html(&mut self, ui: &mut Ui, m: &Message, blocks: &[html::Block]) {
        let full_w = ui.available_width();
        let mut skip = 0usize;
        for (bi, b) in blocks.iter().enumerate() {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            // pictures right after one another (e.g. social icons) go side by side
            let run = blocks[bi..].iter().take_while(|x| matches!(x, html::Block::Image { .. })).count();
            if run > 1 {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                    for x in &blocks[bi..bi + run] {
                        if let html::Block::Image { src, width, height, link, .. } = x {
                            self.picture(ui, m, src, *width, *height, link.as_ref(), full_w, true);
                        }
                    }
                });
                ui.add_space(6.0);
                skip = run - 1;
                continue;
            }
            match b {
                html::Block::Para { runs, center } => {
                    // small pictures inside the text (icons), looked up before drawing
                    let icons: Vec<Option<(String, ImgState)>> = runs.iter().map(|r| r.img.as_ref().map(|(src, _)| self.image_bytes(m, src))).collect();
                    let draw = |ui: &mut Ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::new(0.0, 2.0);
                            for (ri, r) in runs.iter().enumerate() {
                                if let Some((src_w, icon)) = r.img.as_ref().map(|(_, w)| *w).zip(Some(icons[ri].clone())) {
                                    if let Some((key, ImgState::Ready(bytes))) = icon {
                                        let uri = format!("bytes://icon/{:x}", {
                                            use std::hash::{Hash, Hasher};
                                            let mut h = std::collections::hash_map::DefaultHasher::new();
                                            key.hash(&mut h);
                                            h.finish()
                                        });
                                        let img = egui::Image::from_bytes(uri, egui::load::Bytes::Shared(bytes));
                                        let img = match src_w {
                                            Some(w) => img.fit_to_exact_size(Vec2::new(w, w * 4.0)),
                                            None => img.fit_to_original_size(1.0).max_size(Vec2::new(64.0, 40.0)),
                                        };
                                        let sense = if r.link.is_some() { Sense::click() } else { Sense::hover() };
                                        let resp = ui.add(img.sense(sense));
                                        ui.add_space(4.0);
                                        if let Some(url) = &r.link {
                                            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                                            }
                                        }
                                    }
                                    continue;
                                }
                                let mut t = RichText::new(&r.text).size(r.size);
                                if r.bold {
                                    t = t.font(semibold(r.size));
                                }
                                if r.italic {
                                    t = t.italics();
                                }
                                match &r.link {
                                    Some(url) => {
                                        ui.add(egui::Hyperlink::from_label_and_url(t.color(r.color.map(rgb).unwrap_or(ACCENT())), url));
                                    }
                                    None => {
                                        ui.add(egui::Label::new(t.color(r.color.map(rgb).unwrap_or(TEXT()))).selectable(true));
                                    }
                                }
                            }
                        });
                    };
                    let links: Vec<&String> = runs.iter().filter_map(|r| r.link.as_ref()).collect();
                    let has_icons = runs.iter().any(|r| r.img.is_some());
                    if has_icons {
                        // pictures in the text: drawn one by one (centred if the text is)
                        if *center {
                            ui.vertical_centered(draw);
                        } else {
                            draw(ui);
                        }
                    } else if *center && runs.len() == 1 && links.len() == 1 {
                        // a centered link on its own, e.g. a button in a newsletter
                        let r = &runs[0];
                        let mut t = RichText::new(r.text.trim()).size(r.size).color(ACCENT());
                        if r.bold {
                            t = t.font(semibold(r.size));
                        }
                        ui.vertical_centered(|ui| {
                            ui.add(egui::Hyperlink::from_label_and_url(t, links[0].as_str()));
                        });
                    } else if *center {
                        // centered text: one block of text laid out in the middle
                        let mut job = egui::text::LayoutJob::default();
                        for r in runs.iter() {
                            job.append(
                                &r.text,
                                0.0,
                                egui::TextFormat {
                                    font_id: if r.bold { semibold(r.size) } else { FontId::proportional(r.size) },
                                    color: r.color.map(rgb).unwrap_or(if r.link.is_some() { ACCENT() } else { TEXT() }),
                                    italics: r.italic,
                                    ..Default::default()
                                },
                            );
                        }
                        job.halign = Align::Center;
                        job.wrap.max_width = ui.available_width();
                        let galley = ui.painter().layout_job(job);
                        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), galley.size().y), Sense::click());
                        ui.painter().galley(Pos2::new(rect.center().x, rect.top()), galley, TEXT());
                        if let Some(url) = links.first() {
                            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(url.as_str()).clicked() {
                                ui.ctx().open_url(egui::OpenUrl::new_tab(url.as_str()));
                            }
                        }
                    } else {
                        draw(ui);
                    }
                    ui.add_space(6.0);
                }
                html::Block::Rule => {
                    ui.separator();
                }
                html::Block::Table { rows, weights, border, bg, gap } => {
                    let gap = *gap;
                    let avail = ui.available_width();
                    let total: f32 = weights.iter().sum::<f32>().max(1.0);
                    let widths: Vec<f32> = weights.iter().map(|w| w / total * avail).collect();
                    let line = border.map(rgb);
                    let mut frame = Frame::new();
                    if let Some(c) = bg {
                        frame = frame.fill(rgb(*c));
                    }
                    if let Some(c) = line {
                        frame = frame.stroke(Stroke::new(1.0, c));
                    }
                    frame.show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for (ri, row) in rows.iter().enumerate() {
                            let row_resp = ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                                ui.spacing_mut().item_spacing = Vec2::ZERO;
                                let mut col = 0;
                                for cell in row {
                                    let end = (col + cell.colspan).min(widths.len());
                                    let w = widths[col.min(widths.len())..end].iter().sum::<f32>().max(24.0);
                                    col += cell.colspan;
                                    let align = if cell.center { Align::Center } else { Align::Min };
                                    ui.allocate_ui_with_layout(Vec2::new(w, 0.0), Layout::top_down(align), |ui| {
                                        ui.set_width(w);
                                        let half = (gap / 2.0) as i8;
                                        let mut f = Frame::new()
                                            .inner_margin(Margin::symmetric(8, 6))
                                            .outer_margin(Margin::same(half))
                                            .corner_radius(CornerRadius::same(if gap > 0.0 { 4 } else { 0 }));
                                        if let Some(c) = cell.bg {
                                            f = f.fill(rgb(c));
                                        }
                                        if let Some(c) = cell.border.filter(|_| gap > 0.0) {
                                            f = f.stroke(Stroke::new(1.0, rgb(c)));
                                        }
                                        f.show(ui, |ui| {
                                            ui.set_width((w - 16.0 - gap).max(8.0));
                                            ui.spacing_mut().item_spacing.y = 4.0;
                                            self.render_html(ui, m, &cell.blocks);
                                        });
                                    });
                                }
                            });
                            if let Some(c) = line {
                                if ri + 1 < rows.len() {
                                    let r = row_resp.response.rect;
                                    ui.painter().hline(r.x_range(), r.bottom(), Stroke::new(1.0, c));
                                }
                            }
                        }
                    });
                    ui.add_space(8.0);
                }
                html::Block::Boxed { blocks: inner, bg, border, .. } => {
                    let mut f = Frame::new().inner_margin(Margin::symmetric(14, 10)).corner_radius(CornerRadius::same(6));
                    if let Some(c) = bg {
                        f = f.fill(rgb(*c));
                    }
                    if let Some(c) = border {
                        f = f.stroke(Stroke::new(1.0, rgb(*c)));
                    }
                    f.show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        self.render_html(ui, m, inner);
                    });
                    ui.add_space(6.0);
                }
                html::Block::Image { src, width, height, link, center } => {
                    if *center {
                        ui.vertical_centered(|ui| self.picture(ui, m, src, *width, *height, link.as_ref(), full_w, false));
                    } else {
                        self.picture(ui, m, src, *width, *height, link.as_ref(), full_w, false);
                    }
                    ui.add_space(6.0);
                }
            }
        }
    }

    /// One picture of an email: at the width the email asks for, else at its own size (never
    /// stretched); a placeholder while it downloads; opens its link when clicked.
    #[allow(clippy::too_many_arguments)]
    /// `in_row`: side by side with other pictures (no extra room below, the row has its height).
    pub(super) fn picture(&mut self, ui: &mut Ui, m: &Message, src: &str, width: Option<f32>, height: Option<f32>, link: Option<&String>, full_w: f32, in_row: bool) {
        let (key, state) = self.image_bytes(m, src);
        let max_w = width.unwrap_or(full_w).min(full_w);
        let resp = match &state {
            ImgState::Ready(bytes) => {
                let uri = format!("bytes://mail/{:x}", {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    key.hash(&mut h);
                    h.finish()
                });
                let img = egui::Image::from_bytes(uri, egui::load::Bytes::Shared(bytes.clone()));
                let img = match width {
                    Some(_) => img.fit_to_exact_size(Vec2::new(max_w, 4000.0)),
                    None => img.fit_to_original_size(1.0).max_width(max_w),
                }
                .sense(if link.is_some() { Sense::click() } else { Sense::hover() });
                // room to grow downwards: inside table cells the space starts with no
                // height, and a picture fitted to it would shrink to nothing
                if in_row {
                    Some(ui.add(img))
                } else {
                    Some(ui.allocate_ui(Vec2::new(max_w, 4000.0), |ui| ui.add(img)).inner)
                }
            }
            ImgState::Loading => {
                let h = height.map(|h| h * (max_w / width.unwrap_or(max_w)).min(1.0)).unwrap_or(60.0).clamp(20.0, 400.0);
                let (r, _) = ui.allocate_exact_size(Vec2::new(max_w.min(320.0), h), Sense::hover());
                ui.painter().rect_filled(r, CornerRadius::same(4), SURFACE());
                ui.painter().text(r.center(), Align2::CENTER_CENTER, "🖼", FontId::proportional(18.0), MUTED());
                None
            }
            ImgState::Failed => None,
        };
        if let (Some(resp), Some(url)) = (resp, link) {
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(url).clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
            }
        }
    }

    pub(super) fn reading_pane(&mut self, ui: &mut Ui) {
        if let Some(id) = self.selected {
            self.ensure_body(id);
        }
        let p_clone = crate::prof::span("  reading: copy message");
        let m_opt = self.selected.and_then(|id| self.msg(id)).cloned().map(|mut m| {
            if let Some(page) = self.translations.get(&m.id).filter(|_| self.show_translated.contains(&m.id)) {
                m.html_path = page.clone();
            }
            m
        });
        drop(p_clone);
        let Some(m) = m_opt else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Wybierz wiadomość, aby ją przeczytać").color(MUTED()));
            });
            return;
        };
        let today = Local::now().date_naive();
        if self.thread_for != Some(m.id) {
            // another message opened: its conversation starts folded, the message itself open
            self.thread_for = Some(m.id);
            self.thread_unfolded.clear();
            self.main_folded = false;
        }
        Frame::new().inner_margin(Margin { left: 20, right: 20, top: 12, bottom: 10 }).show(ui, |ui| {
            // the pane's own width: whatever is above the text, the text keeps this place
            let pane_x = ui.max_rect().x_range();
            // Subject line with chips on the right.
            let (hr, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 32.0), Sense::hover());
            let mut right = hr.right();
            if m.flagged {
                let txt = "Ważne  ✕";
                let w = text_width(ui, txt, FontId::proportional(13.0)) + 18.0;
                let chip = Rect::from_min_size(Pos2::new(right - w, hr.top() + 3.0), Vec2::new(w, 26.0));
                let cr = ui.interact(chip, id(("chip", m.id)), Sense::click());
                ui.painter().rect_filled(chip, CornerRadius::same(2), IMPORTANT());
                ui.painter().text(chip.center(), Align2::CENTER_CENTER, txt, FontId::proportional(13.0), if crate::theme::is_dark() { Color32::from_rgb(0xFF, 0xDF, 0x80) } else { Color32::from_rgb(0x22, 0x22, 0x22) });
                if cr.on_hover_text("Usuń oznaczenie").clicked() {
                    self.actions.push(Action::ToggleFlag(m.id));
                }
                right -= w + 8.0;
            }
            // "+ Tag": choose this message's tags.
            {
                let w = text_width(ui, "+ Tag", FontId::proportional(13.0)) + 14.0;
                let r = Rect::from_min_size(Pos2::new(right - w, hr.top() + 5.0), Vec2::new(w, 22.0));
                let resp = ui.interact(r, id(("add-tag", m.id)), Sense::click());
                let p = ui.painter();
                p.rect_stroke(
                    r,
                    CornerRadius::same(4),
                    Stroke::new(1.0, if resp.hovered() { ACCENT() } else { BORDER() }),
                    egui::StrokeKind::Inside,
                );
                p.text(r.center(), Align2::CENTER_CENTER, "+ Tag", FontId::proportional(13.0), if resp.hovered() { ACCENT() } else { MUTED() });
                let resp = resp.on_hover_text("Dodaj lub usuń tagi tej wiadomości");
                let mid = m.id;
                egui::Popup::menu(&resp).show(|ui| self.tag_menu(ui, mid));
                right -= w + 6.0;
            }
            // The message's tags, right to left, as long as the subject keeps some room.
            // Clicking a tag shows all messages with it; its ✕ takes it off this message.
            for l in m.labels.iter().rev() {
                let shown = format!("{}   ✕", display_label(l));
                let w = text_width(ui, &shown, FontId::proportional(13.0)).min(180.0) + 12.0;
                if right - w < hr.left() + 200.0 {
                    break;
                }
                let pos = Pos2::new(right - w, hr.top() + 6.0);
                let used = tag_chip(ui.painter(), pos, &shown, 13.0, 192.0);
                let x_rect = Rect::from_min_size(Pos2::new(pos.x + used - 20.0, pos.y), Vec2::new(20.0, 20.0));
                let name_rect = Rect::from_min_size(pos, Vec2::new(used - 20.0, 20.0));
                if ui.interact(x_rect, id(("tag-x", m.id, l.as_str())), Sense::click()).on_hover_text("Odepnij ten tag od wiadomości").clicked() {
                    self.actions.push(Action::RemoveTag(m.id, l.clone()));
                }
                if ui.interact(name_rect, id(("tag", m.id, l.as_str())), Sense::click()).on_hover_text("Pokaż wiadomości z tym tagiem").clicked() {
                    self.label_filter = l.clone();
                    self.view = MailView::Label;
                    self.search.clear();
                }
                right -= used + 6.0;
            }
            line_f(ui.painter(), Pos2::new(hr.left(), hr.top() + 4.0), &m.subject, semibold(18.0), TEXT(), right - hr.left() - 8.0);
            ui.add_space(10.0);
            // earlier messages of this conversation, folded to one line each
            let p_thread = crate::prof::span("  reading: conversation");
            let in_thread = self.thread_cards(ui, &m);
            drop(p_thread);
            if in_thread && self.main_folded {
                // the open message folded to one line as well; a click brings it back
                if self.thread_card(ui, &m, false) {
                    self.main_folded = false;
                }
                return;
            }
            if in_thread {
                let r = ui.available_rect_before_wrap();
                ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, BORDER()));
                ui.add_space(4.0);
            }

            // Sender line (in a conversation, a click on it folds the message to one line).
            let answered_at = self.answered.1.get(&m.id).copied();
            let forward_note = self
                .forwarded
                .get(&m.id)
                .and_then(|f| self.msg(*f))
                .map(|f| format!("➦  Przekazano {} {} do: {}", pl::short_day_month(f.date.date()), f.date.format("%H:%M"), short_people(&f.to)));
            let to_people = people_in(&m.to);
            let cc_people = people_in(&m.cc);
            let has_notes = answered_at.is_some() || forward_note.is_some();
            let head_h = 54.0 + if cc_people.is_empty() { 0.0 } else { 22.0 } + if has_notes { 22.0 } else { 0.0 };
            let (sr, sender_resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), head_h), Sense::click());
            // a double click on the sender line opens the message in a window of its own
            if sender_resp.double_clicked() {
                self.actions.push(Action::OpenWindow(m.id));
                self.main_folded = false;
            }
            if in_thread {
                // folding hint drawn on the line itself (a pop-up hint would hide under the email)
                if sender_resp.hovered() {
                    ui.painter().rect_filled(sr.expand2(Vec2::new(6.0, 2.0)), CornerRadius::same(6), SURFACE());
                    ui.painter().text(
                        Pos2::new(sr.center().x, sr.bottom() - 2.0),
                        Align2::CENTER_BOTTOM,
                        "⌃ Zwiń",
                        FontId::proportional(11.5),
                        MUTED(),
                    );
                }
                if sender_resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    self.main_folded = true;
                }
            }
            self.avatar_or_logo(ui, Pos2::new(sr.left() + 24.0, sr.top() + 26.0), 24.0, &m.from_name, &m.from_addr);
            let when = if m.date.date() == today {
                m.date.format("%H:%M:%S").to_string()
            } else {
                format!("{} {}", pl::short_day_month(m.date.date()), m.date.format("%H:%M"))
            };
            let tw = text_width(ui, &when, FontId::proportional(15.0));
            ui.painter().text(Pos2::new(sr.right(), sr.top() + 4.0), Align2::RIGHT_TOP, &when, FontId::proportional(15.0), TEXT());
            let text_w = sr.width() - 70.0 - tw - 12.0;
            let full_w = sr.width() - 70.0;
            line_f(ui.painter(), Pos2::new(sr.left() + 60.0, sr.top() + 4.0), &m.from_name, semibold(15.0), ACCENT(), text_w);
            let rcpt_key = egui::Id::new(("rcpt-open", m.id));
            let rcpt_open = ui.data(|d| d.get_temp::<bool>(rcpt_key)).unwrap_or(false);
            // (the names are fitted to the room left by the buttons, below)
            let _ = full_w;
            // What to do with it, in a row of its own: answering first, as buttons with words;
            // the smaller tools as icons on the right.
            let marked = self.prefs.no_reply_needed.contains(&m.id);
            let offer = marked || self.waits_for_reply(&m);
            let tr_tip = if self.show_translated.contains(&m.id) { "Pokaż oryginał" } else { "Przetłumacz na polski (AI)" };
            let nr_tip = if marked { "Oznaczono: nie wymaga odpowiedzi — kliknij, aby cofnąć" } else { "Nie wymaga odpowiedzi (ukryj na liście „Bez odpowiedzi”)" };
            let icon_rect = Rect::from_min_max(Pos2::new(sr.right() - 380.0, sr.top() + 26.0), Pos2::new(sr.right() + 6.0, sr.top() + 56.0));
            let buttons = ui.scope_builder(egui::UiBuilder::new().max_rect(icon_rect).layout(Layout::right_to_left(Align::Center)), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    let mut icon = |ui: &mut Ui, k: usize, glyph: &str, tip: &str| -> egui::Response {
                        let (rr, resp) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
                        if resp.hovered() {
                            ui.painter().rect_filled(rr, CornerRadius::same(5), HOVER());
                        }
                        if k == 5 {
                            // a reply arrow, crossed out: no reply needed (coloured when marked)
                            let col = if marked { Color32::from_rgb(0x8E, 0x5B, 0xD0) } else { MUTED() };
                            ui.painter().text(rr.center(), Align2::CENTER_CENTER, "↶", FontId::proportional(16.0), col);
                            ui.painter().line_segment([rr.center() + Vec2::new(-7.0, 7.0), rr.center() + Vec2::new(7.0, -7.0)], Stroke::new(1.6, col));
                        } else {
                            // (the reply arrow is drawn thinner by the font: a size up, to match the others)
                            let size = if k == 6 { 21.0 } else { 16.0 };
                            let at = if k == 6 { rr.center() - Vec2::new(0.0, 3.0) } else { rr.center() };
                            ui.painter().text(at, Align2::CENTER_CENTER, glyph, FontId::proportional(size), MUTED());
                        }
                        // the hint goes above the button: below it the formatted email would cover it
                        hint_above(ui, &resp, tip);
                        resp
                    };
                    let more = icon(ui, 0, "⋯", "Więcej");
                    let (mid, read, flagged) = (m.id, m.read, m.flagged);
                    if std::env::var("POCZTA_START").as_deref() == Ok("menu") && self.clock() > 4.0 {
                        // test picture: the menu open over the email
                        use std::sync::atomic::{AtomicBool, Ordering};
                        static OPENED: AtomicBool = AtomicBool::new(false);
                        if !OPENED.swap(true, Ordering::Relaxed) {
                            egui::Popup::open_id(ui.ctx(), egui::Popup::default_response_id(&more));
                        }
                    }
                    egui::Popup::menu(&more).show(|ui| self.message_menu(ui, mid, read, flagged));
                    if icon(ui, 3, "🔗", "Kopiuj link dla AI").clicked() {
                        self.actions.push(Action::CopyForAi(m.id));
                    }
                    if icon(ui, 4, "🌐", tr_tip).clicked() {
                        self.actions.push(Action::Translate(m.id));
                    }
                    if offer && icon(ui, 5, "", nr_tip).clicked() {
                        self.actions.push(Action::NoReplyNeeded(m.id, !marked));
                    }
                    if icon(ui, 2, "➦", "Prześlij dalej").clicked() {
                        self.actions.push(Action::Forward(m.id));
                    }
                    if icon(ui, 1, "⮪", "Odpowiedz wszystkim").clicked() {
                        self.actions.push(Action::Reply(m.id, true));
                    }
                    // replying: a plain icon like the others, the first of the row
                    let reply = icon(ui, 6, "↶", "Odpowiedz (Ctrl+R)");
                    if reply.clicked() {
                        self.actions.push(Action::Reply(m.id, false));
                    }
                    reply.rect.left()
                });
            // the address line (and when you answered) up to the buttons, never under them
            let buttons_left = buttons.inner.min(buttons.response.rect.left());
            let room = (buttons_left - (sr.left() + 60.0) - 12.0).max(40.0);
            let to_line = fit_names(ui, "do ", &to_people, room);
            let cc_line = fit_names(ui, "DW: ", &cc_people, room);
            line(ui.painter(), Pos2::new(sr.left() + 60.0, sr.top() + 28.0), &to_line, 13.5, ACCENT(), room);
            let mut next_y = sr.top() + 50.0;
            if !cc_people.is_empty() {
                line(ui.painter(), Pos2::new(sr.left() + 60.0, next_y), &cc_line, 13.5, ACCENT(), room);
                next_y += 22.0;
            }
            // more people than shown: a click on the lines shows everyone (and folds them again)
            if to_line.contains("  +") || cc_line.contains("  +") || rcpt_open {
                let area = Rect::from_min_max(Pos2::new(sr.left() + 60.0, sr.top() + 26.0), Pos2::new(sr.left() + 60.0 + room, next_y));
                let r = ui.interact(area, id(("rcpt-lines", m.id)), Sense::click());
                hint_above(ui, &r, if rcpt_open { "Zwiń listę adresatów" } else { "Pokaż wszystkich adresatów" });
                if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    ui.data_mut(|d| d.insert_temp(rcpt_key, !rcpt_open));
                }
            }
            // a line of its own under the address: when you answered, when you forwarded it and to whom
            let mut notes: Vec<String> = vec![];
            if let Some(when) = answered_at {
                notes.push(format!("↶  Odpowiedziano {} {}", pl::short_day_month(when.date()), when.format("%H:%M")));
            }
            if let Some(n) = forward_note {
                notes.push(n);
            }
            if !notes.is_empty() {
                line(ui.painter(), Pos2::new(sr.left() + 60.0, next_y), &notes.join("   ·   "), 12.5, Color32::from_rgb(0x8E, 0x5B, 0xD0), room);
            }
            // everyone it was sent to and copied to: only when unfolded (the lines above show the first few)
            if rcpt_open {
                recipients_row(ui, m.id, &m.to, &m.cc, 60.0, true);
            }
            if m.folder == Folder::Drafts {
                ui.add_space(4.0);
                if ui.add(egui::Button::new(RichText::new("✏  Edytuj szkic").color(Color32::WHITE)).fill(ACCENT())).clicked() {
                    self.actions.push(Action::EditDraft(m.id));
                }
            }
            if !m.attachments.is_empty() {
                ui.add_space(6.0);
                // Attachments as chips in rows of the pane's width: a long name is shortened, a chip
                // that doesn't fit goes to the next row (nothing pushes the text aside).
                let width = pane_x.max - pane_x.min;
                let font = FontId::proportional(14.0);
                let mut chips: Vec<(usize, String, String, f32)> = Vec::new();
                for (i, a) in m.attachments.iter().enumerate() {
                    let file = m.attachment_files.get(i).filter(|f| std::path::Path::new(f).exists());
                    let size = file.and_then(|f| std::fs::metadata(f).ok()).map(|md| human_size(md.len())).unwrap_or_default();
                    let room = ((width - 12.0) / 2.0 - 52.0).clamp(80.0, 300.0);
                    let label = fit_name(ui, a, &size, &font, room);
                    let w = text_width(ui, &label, font.clone()) + 52.0;
                    chips.push((i, label, a.clone(), w));
                }
                let mut rows: Vec<Vec<(usize, String, String, f32)>> = vec![vec![]];
                let mut x = 0.0;
                for c in chips {
                    if x > 0.0 && x + c.3 > width {
                        rows.push(vec![]);
                        x = 0.0;
                    }
                    x += c.3 + 6.0;
                    rows.last_mut().unwrap().push(c);
                }
                let files_key = egui::Id::new(("files-open", m.id));
                let files_open = ui.data(|d| d.get_temp::<bool>(files_key)).unwrap_or(false);
                let total_rows = rows.len();
                let shown_rows = if files_open { total_rows } else { total_rows.min(2) };
                let hidden: usize = rows.iter().skip(shown_rows).map(|r| r.len()).sum();
                let mut flip = false;
                for (ri, row) in rows.into_iter().take(shown_rows).enumerate() {
                    let last = ri + 1 == shown_rows;
                    ui.horizontal(|ui| {
                        ui.set_max_width(width);
                        ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                        for (i, label, name, _) in row {
                            Frame::new()
                                .stroke(Stroke::new(1.0, BORDER()))
                                .corner_radius(CornerRadius::same(4))
                                .inner_margin(Margin { left: 2, right: 2, top: 0, bottom: 0 })
                                .show(ui, |ui| {
                                    ui.spacing_mut().item_spacing.x = 2.0;
                                    let can_preview = previewable(&name);
                                    let open = ui.add(egui::Button::new(RichText::new(&label).color(TEXT())).frame(false).wrap_mode(egui::TextWrapMode::Extend));
                                    let tip = if can_preview { format!("{name} — podgląd") } else { format!("{name} — tego pliku nie da się podejrzeć, kliknij, aby zapisać w Pobranych") };
                                    hint_above(ui, &open, &tip);
                                    let (sep, _) = ui.allocate_exact_size(Vec2::new(9.0, 18.0), Sense::hover());
                                    ui.painter().vline(sep.center().x, sep.y_range(), Stroke::new(1.0, BORDER()));
                                    let save = ui.add(egui::Button::new(RichText::new("⬇").size(15.0).color(ACCENT())).frame(false));
                                    hint_above(ui, &save, "Zapisz w folderze Pobrane");
                                    if open.clicked() {
                                        self.actions.push(if can_preview { Action::PreviewAttachment(m.id, i) } else { Action::SaveAttachment(m.id, i) });
                                    }
                                    if save.clicked() {
                                        self.actions.push(Action::SaveAttachment(m.id, i));
                                    }
                                });
                        }
                        // the rest of the files: behind "+N więcej" (and back with "Zwiń")
                        if last && (hidden > 0 || (files_open && total_rows > 2)) {
                            let more = if hidden > 0 { format!("+{hidden} więcej") } else { "Zwiń".to_string() };
                            let r = ui.add(egui::Label::new(RichText::new(more).size(13.0).color(ACCENT()).underline()).sense(Sense::click()));
                            if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                flip = true;
                            }
                        }
                    });
                }
                if flip {
                    ui.data_mut(|d| d.insert_temp(files_key, !files_open));
                }
            }
            ui.add_space(12.0);
            // A big message stored with its header only: the rest is on its way.
            if m.partial && m.body.is_empty() && m.html_path.is_empty() {
                ui.add_space(20.0);
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(14.0).color(ACCENT()));
                    ui.label(RichText::new("Pobieranie pełnej treści wiadomości…").color(MUTED()));
                });
                return;
            }
            // Translation into Polish: on its way, done (with a way back to the original), or failed.
            let busy = self.translating.contains_key(&m.id);
            let shown_tr = self.show_translated.contains(&m.id) && self.translations.contains_key(&m.id);
            let failed = self.translate_error.as_ref().filter(|(id, _)| *id == m.id).map(|(_, e)| e.clone());
            if busy || shown_tr || failed.is_some() {
                ui.horizontal(|ui| {
                    if busy {
                        ui.label(RichText::new("🌐  Tłumaczę wiadomość na polski…").size(13.0).color(ACCENT()));
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
                    } else if shown_tr {
                        ui.label(RichText::new("🌐  Przetłumaczono na polski (AI)").size(13.0).color(Color32::from_rgb(0x8E, 0x5B, 0xD0)));
                        if ui.link(RichText::new("Pokaż oryginał").size(13.0)).clicked() {
                            self.actions.push(Action::Translate(m.id));
                        }
                    } else if let Some(e) = &failed {
                        ui.label(RichText::new(format!("Nie udało się przetłumaczyć: {e}")).size(13.0).color(FLAG));
                    }
                });
                ui.add_space(6.0);
            }
            // Formatted emails: drawn by the app itself (Blitz), in this framed area.
            // (drawn by the app's own engine only on request: POCZTA_BLITZ; Edge lays emails out
            // exactly like a browser, as the original app's Chromium does)
            if !m.html_path.is_empty() && std::env::var("POCZTA_BLITZ").is_ok() {
                let mut rect = ui.available_rect_before_wrap();
                rect.min.x = pane_x.min;
                rect.max.x = pane_x.max;
                ui.painter().rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
                ui.allocate_rect(rect, Sense::hover());
                self.email_view(ui, &m, rect.shrink(2.0));
                return;
            }
            // (the browser window, kept as a fallback: POCZTA_BROWSER_VIEW)
            if !m.html_path.is_empty() && (self.shot.is_none() || std::env::var("POCZTA_WEBTEST").is_ok()) && !self.mail_view.failed && std::env::var("POCZTA_BLITZ").is_err() {
                let mut rect = ui.available_rect_before_wrap();
                // always the pane's width, from its left edge
                rect.min.x = pane_x.min;
                rect.max.x = pane_x.max;
                // the newer messages of the conversation below it (never under the email view)
                let below = self.later_height(m.id);
                rect.max.y = (rect.max.y - below).max(rect.min.y + 120.0);
                ui.painter().rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
                ui.allocate_rect(rect, Sense::hover());
                self.web_target = Some((m.id, rect.shrink(2.0)));
                // a menu is open: the email is shown as a picture (the menu lies over it)
                if self.menu_open {
                    if let Some((sid, tex)) = &self.mail_shot {
                        if *sid == m.id {
                            ui.painter().image(tex.id(), rect.shrink(2.0), Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                        }
                    }
                }
                self.later_cards(ui, m.id);
                return;
            }
            let blocks = self.html_blocks(&m);
            egui::ScrollArea::vertical().id_salt(("body", m.id)).auto_shrink([false, false]).show(ui, |ui| {
                Frame::new()
                    .stroke(Stroke::new(1.0, BORDER()))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(26, 22))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        match &blocks {
                            Some(b) if !b.is_empty() => self.render_html(ui, &m, b),
                            _ => {
                                ui.add(egui::Label::new(RichText::new(&m.body).size(14.5).color(TEXT())).selectable(true).wrap());
                            }
                        }
                    });
                // the newer messages of the conversation, folded, after the text
                self.later_cards(ui, m.id);
                ui.add_space(10.0);
            });
        });
    }

    /// A link of an email: web addresses in the web browser, e-mail addresses in a new message.
    pub(super) fn open_link(&mut self, href: &str) {
        let h = href.trim();
        if let Some(addr) = h.strip_prefix("mailto:") {
            let to = addr.split('?').next().unwrap_or("").to_string();
            self.compose_account = self.view_account;
            self.open_composer(to, String::new(), String::new(), String::new(), None, None);
        } else if h.starts_with("http://") || h.starts_with("https://") {
            crate::oauth::open_url(h);
        }
    }

    /// The formatted email in `rect`: drawn on the drawing thread at this width, shown as a
    /// picture that scrolls; links open on a click.
    pub(super) fn email_view(&mut self, ui: &mut Ui, m: &Message, rect: Rect) {
        if self.email_engine.is_none() {
            self.email_engine = Some(crate::htmlview::Engine::start(self.ctx.clone()));
        }
        self.poll_email();
        let ppp = ui.ctx().pixels_per_point();
        let width = (rect.width() - 14.0).max(120.0);
        let key = crate::htmlview::key_of(m.id, &m.html_path, width, ppp);
        if self.email_asked.0 != key {
            match crate::htmlview::prepare(&m.html_path, &m.inline) {
                Some(html) => {
                    if let Some(e) = &self.email_engine {
                        e.show(crate::htmlview::Wanted { key, html, width, scale: ppp });
                    }
                }
                None => crate::diag::log("email view: its formatted text can't be read"),
            }
            self.email_asked = (key, m.id);
        }
        let pic_key = self.email_pic.as_ref().filter(|p| p.id == m.id).map(|p| p.key);
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            egui::ScrollArea::vertical().id_salt(("email", m.id)).auto_shrink([false, false]).show(ui, |ui| {
                let Some(pic) = self.email_pic.as_ref().filter(|p| p.id == m.id) else {
                    ui.add_space(30.0);
                    ui.horizontal(|ui| {
                        ui.add_space(20.0);
                        ui.add(egui::Spinner::new().size(14.0).color(ACCENT()));
                        ui.label(RichText::new("Wczytywanie wiadomości…").color(MUTED()));
                    });
                    return;
                };
                let mut top_left = None;
                let mut hovered = None;
                let mut clicked = None;
                ui.spacing_mut().item_spacing.y = 0.0;
                for (tex, size) in &pic.strips {
                    let (r, resp) = ui.allocate_exact_size(*size, Sense::click());
                    let origin = *top_left.get_or_insert(r.min);
                    match tex {
                        Some(tex) => {
                            ui.painter().image(tex.id(), r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                        }
                        // (this part is a moment away)
                        None => {
                            ui.painter().rect_filled(r, CornerRadius::ZERO, Color32::WHITE);
                        }
                    }
                    if let Some(p) = resp.hover_pos() {
                        hovered = Some(p - origin);
                    }
                    if resp.clicked() {
                        clicked = resp.interact_pointer_pos().map(|p| p - origin);
                    }
                    resp.context_menu(|ui| {
                        if ui.button("📋  Kopiuj tekst wiadomości").clicked() {
                            ui.ctx().copy_text(m.body.clone());
                            ui.close();
                        }
                        if let Some(h) = self.email_hover.1.clone().filter(|_| self.email_hover.0 == pic.key) {
                            if ui.button("🔗  Kopiuj adres linku").clicked() {
                                ui.ctx().copy_text(h);
                                ui.close();
                            }
                        }
                    });
                }
                if let Some(key) = pic_key {
                    let now = self.clock();
                    if let (Some(e), Some(v)) = (&self.email_engine, clicked) {
                        e.link_at(key, v.x, v.y, true);
                    }
                    if let (Some(e), Some(v)) = (&self.email_engine, hovered) {
                        // which link is under the pointer: asked now and then while it moves
                        if ui.ctx().input(|i| i.pointer.is_moving()) && now - self.email_hover.2 > 0.08 {
                            self.email_hover.2 = now;
                            e.link_at(key, v.x, v.y, false);
                        }
                        if self.email_hover.0 == key {
                            if let Some(h) = &self.email_hover.1 {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                // where the link goes, in the corner (like a browser)
                                let txt = h.chars().take(90).collect::<String>();
                                let r = ui.clip_rect();
                                let g = ui.painter().layout_no_wrap(txt, FontId::proportional(12.0), TEXT2());
                                let bg = Rect::from_min_size(Pos2::new(r.left() + 4.0, r.bottom() - g.size().y - 10.0), g.size() + Vec2::new(12.0, 6.0));
                                ui.painter().rect_filled(bg, CornerRadius::same(4), SURFACE());
                                ui.painter().galley(bg.min + Vec2::new(6.0, 3.0), g, TEXT2());
                            }
                        }
                    }
                }
                if !pic.done {
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
                }
            });
        });
    }

    // ------------------------------------------------------------ windows

    /// Messages opened in windows of their own: frameless, movable and resizable, with the
    /// formatted text, the attachments and reply / forward / AI link.
    pub(super) fn readers_ui(&mut self, ctx: &egui::Context) {
        if self.readers.is_empty() {
            return;
        }
        let mut readers = std::mem::take(&mut self.readers);
        let main_pos = ctx.input(|inp| inp.viewport().outer_rect).map(|r| r.min);
        let today = Local::now().date_naive();
        let mut closed: Vec<u64> = vec![];
        for (i, r) in readers.iter_mut().enumerate() {
            let m = r.msg.clone();
            let title = if m.subject.trim().is_empty() { "(bez tematu)".to_string() } else { m.subject.clone() };
            let mut builder = egui::ViewportBuilder::default()
                .with_title(format!("{title}{}", reader_marker(r.key)))
                .with_inner_size([860.0, 720.0])
                .with_min_inner_size([420.0, 300.0])
                .with_decorations(false)
                .with_icon(self.icon.clone());
            if let Some(main) = main_pos {
                builder = builder.with_position(main + Vec2::new(260.0 + 30.0 * i as f32, 40.0 + 30.0 * i as f32));
            }
            let mut open = true;
            ctx.show_viewport_immediate(egui::ViewportId::from_hash_of(("reader", r.key)), builder, |root, _class| {
                if root.ctx().input(|inp| inp.viewport().close_requested()) {
                    open = false;
                }
                egui::CentralPanel::default()
                    .frame(Frame::new().fill(BG()).inner_margin(Margin { left: 18, right: 8, top: 8, bottom: 10 }))
                    .show(root, |ui| {
                        // line 1: the subject (drag to move, double click to maximize) and the window buttons
                        let bar = Rect::from_min_size(ui.max_rect().min - Vec2::new(18.0, 8.0), Vec2::new(ui.max_rect().width() + 26.0, 48.0));
                        let drag = ui.interact(bar, id(("reader-bar", r.key)), Sense::click_and_drag());
                        if drag.double_clicked() {
                            let maxed = ui.ctx().input(|inp| inp.viewport().maximized.unwrap_or(false));
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!maxed));
                        } else if drag.drag_started_by(egui::PointerButton::Primary) {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                        }
                        ui.horizontal(|ui| {
                            let w = ui.available_width() - 130.0;
                            let (tr, _) = ui.allocate_exact_size(Vec2::new(w.max(50.0), 34.0), Sense::hover());
                            line_f(ui.painter(), Pos2::new(tr.left(), tr.top() + 5.0), &title, semibold(18.0), TEXT(), tr.width());
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                let maxed = ui.ctx().input(|inp| inp.viewport().maximized.unwrap_or(false));
                                if window_button(ui, WinButton::Close).on_hover_text("Zamknij").clicked() {
                                    open = false;
                                }
                                let kind = if maxed { WinButton::Restore } else { WinButton::Maximize };
                                if window_button(ui, kind).clicked() {
                                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!maxed));
                                }
                                if window_button(ui, WinButton::Minimize).on_hover_text("Minimalizuj").clicked() {
                                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                                }
                            });
                        });
                        ui.add_space(6.0);
                        // line 2: the sender, the time, and what can be done with the message
                        let (sr, _) = ui.allocate_exact_size(Vec2::new(ui.available_width() - 10.0, 50.0), Sense::hover());
                        self.avatar_or_logo(ui, Pos2::new(sr.left() + 22.0, sr.center().y), 22.0, &m.from_name, &m.from_addr);
                        let who = if m.from_name.is_empty() { m.from_addr.clone() } else { format!("{} <{}>", m.from_name, m.from_addr) };
                        let when = if m.date.date() == today { m.date.format("%H:%M").to_string() } else { format!("{} {}", pl::short_day_month(m.date.date()), m.date.format("%H:%M")) };
                        let p = ui.painter();
                        line_f(p, Pos2::new(sr.left() + 56.0, sr.top() + 4.0), &who, semibold(14.5), ACCENT(), sr.width() - 330.0);
                        let first = people_in(&m.to).into_iter().next().map(|x| x.0).unwrap_or_else(|| "—".into());
                        line(p, Pos2::new(sr.left() + 56.0, sr.top() + 26.0), &format!("do {first}"), 13.0, MUTED(), sr.width() - 330.0);
                        p.text(Pos2::new(sr.right(), sr.top() + 4.0), Align2::RIGHT_TOP, &when, FontId::proportional(13.5), TEXT());
                        let mut x = sr.right() - 12.0;
                        for (k, (icon, tip)) in [("🔗", "Kopiuj link dla AI"), ("➦", "Prześlij dalej"), ("⮪", "Odpowiedz wszystkim"), ("↶", "Odpowiedz")].iter().enumerate() {
                            let rr = Rect::from_center_size(Pos2::new(x, sr.top() + 36.0), Vec2::splat(26.0));
                            let resp = ui.interact(rr, id(("reader-act", r.key, k)), Sense::click());
                            if resp.hovered() {
                                ui.painter().rect_filled(rr, CornerRadius::same(4), HOVER());
                            }
                            ui.painter().text(rr.center(), Align2::CENTER_CENTER, *icon, FontId::proportional(16.0), MUTED());
                            hint_above(ui, &resp, tip);
                            if resp.clicked() {
                                self.actions.push(match k {
                                    0 => Action::CopyForAi(m.id),
                                    1 => Action::Forward(m.id),
                                    2 => Action::Reply(m.id, true),
                                    _ => Action::Reply(m.id, false),
                                });
                            }
                            x -= 30.0;
                        }
                        recipients_row(ui, m.id, &m.to, &m.cc, 56.0, false);
                        // attachments
                        if !m.attachments.is_empty() {
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                                for (k, a) in m.attachments.iter().enumerate() {
                                    let open_it = ui.add(egui::Button::new(RichText::new(format!("📎 {a}")).color(TEXT())).stroke(Stroke::new(1.0, BORDER())));
                                    hint_above(ui, &open_it, "Otwórz");
                                    if open_it.clicked() {
                                        self.actions.push(Action::OpenAttachment(m.id, k));
                                    }
                                    let save = ui.add(egui::Button::new(RichText::new("⬇").color(ACCENT())).frame(false));
                                    hint_above(ui, &save, "Zapisz w folderze Pobrane");
                                    if save.clicked() {
                                        self.actions.push(Action::SaveAttachment(m.id, k));
                                    }
                                }
                            });
                        }
                        ui.add_space(6.0);
                        // the text: formatted (Edge engine) when there is a formatted version
                        let avail = ui.available_rect_before_wrap();
                        let rect = Rect::from_min_max(avail.min, Pos2::new(avail.right() - 10.0, avail.bottom() - 4.0));
                        let formatted = !m.html_path.is_empty() && !self.mail_view.failed && !r.failed && !ui.ctx().embed_viewports();
                        if formatted {
                            ui.allocate_rect(rect, Sense::hover());
                            ui.painter().rect_stroke(rect.expand(1.0), CornerRadius::same(6), Stroke::new(1.0, BORDER()), egui::StrokeKind::Outside);
                            match &r.web {
                                None => {
                                    r.tries += 1;
                                    if let Some(hwnd) = crate::compose_view::find_window(&reader_marker(r.key)) {
                                        match crate::webview::standalone(hwnd, &m, rect) {
                                            Ok(v) => {
                                                r.web = Some(v);
                                                r.last = Some(rect);
                                            }
                                            Err(e) => {
                                                crate::diag::log(format!("reading window: formatted view not available ({e})"));
                                                r.failed = true;
                                            }
                                        }
                                    } else if r.tries > 120 {
                                        r.failed = true;
                                    } else {
                                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
                                    }
                                }
                                Some((v, _)) => {
                                    if r.last != Some(rect) {
                                        crate::webview::place(v, rect);
                                        r.last = Some(rect);
                                    }
                                }
                            }
                        } else {
                            let blocks = self.card_blocks(&m);
                            egui::ScrollArea::vertical().id_salt(("reader-body", r.key)).auto_shrink([false, false]).show(ui, |ui| {
                                Frame::new().stroke(Stroke::new(1.0, BORDER())).corner_radius(CornerRadius::same(6)).inner_margin(Margin::symmetric(24, 20)).show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    match &blocks {
                                        Some(b) if !b.is_empty() => self.render_html(ui, &m, b),
                                        _ => {
                                            ui.add(egui::Label::new(RichText::new(&m.body).size(14.5).color(TEXT())).selectable(true).wrap());
                                        }
                                    }
                                });
                            });
                        }
                    });
                if !root.ctx().embed_viewports() {
                    window_edges(root);
                }
            });
            if !open {
                closed.push(r.key);
            }
        }
        readers.retain(|r| !closed.contains(&r.key));
        // (windows opened meanwhile come after the ones drawn)
        readers.append(&mut self.readers);
        self.readers = readers;
    }

    /// Attachment windows: the file shown by the Edge engine (PDF with its own viewer), with
    /// "Zapisz w Pobranych" and "Otwórz w programie".
    pub(super) fn previews_ui(&mut self, ctx: &egui::Context) {
        let mut previews = std::mem::take(&mut self.previews);
        let main_pos = ctx.input(|inp| inp.viewport().outer_rect).map(|r| r.min);
        let mut closed: Vec<u64> = vec![];
        for (i, p) in previews.iter_mut().enumerate() {
            let mut builder = egui::ViewportBuilder::default()
                .with_title(format!("{}{}", p.name, preview_marker(p.key)))
                .with_inner_size([980.0, 760.0])
                .with_min_inner_size([420.0, 300.0])
                .with_decorations(false)
                .with_icon(self.icon.clone());
            if let Some(main) = main_pos {
                builder = builder.with_position(main + Vec2::new(220.0 + 30.0 * i as f32, 30.0 + 30.0 * i as f32));
            }
            let mut open = true;
            let (mut save, mut outside) = (false, false);
            ctx.show_viewport_immediate(egui::ViewportId::from_hash_of(("preview", p.key)), builder, |root, _class| {
                if root.ctx().input(|inp| inp.viewport().close_requested()) {
                    open = false;
                }
                egui::CentralPanel::default().frame(Frame::new().fill(BG()).inner_margin(Margin { left: 14, right: 6, top: 6, bottom: 8 })).show(root, |ui| {
                    // the top line: the file's name (drag to move), what to do with it, window buttons
                    let bar = Rect::from_min_size(ui.max_rect().min - Vec2::new(14.0, 6.0), Vec2::new(ui.max_rect().width() + 20.0, 46.0));
                    let drag = ui.interact(bar, id(("preview-bar", p.key)), Sense::click_and_drag());
                    if drag.double_clicked() {
                        let maxed = ui.ctx().input(|inp| inp.viewport().maximized.unwrap_or(false));
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!maxed));
                    } else if drag.drag_started_by(egui::PointerButton::Primary) {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("📎 {}", p.name)).font(semibold(15.0)).color(TEXT()));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            let maxed = ui.ctx().input(|inp| inp.viewport().maximized.unwrap_or(false));
                            if window_button(ui, WinButton::Close).on_hover_text("Zamknij").clicked() {
                                open = false;
                            }
                            let kind = if maxed { WinButton::Restore } else { WinButton::Maximize };
                            if window_button(ui, kind).clicked() {
                                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!maxed));
                            }
                            if window_button(ui, WinButton::Minimize).clicked() {
                                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                            }
                            ui.add_space(12.0);
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if ui.button("Otwórz w programie").on_hover_text("Otwiera plik w programie Windows przypisanym do tego typu").clicked() {
                                outside = true;
                            }
                            if ui.add(egui::Button::new(RichText::new("⬇  Zapisz w Pobranych").color(Color32::WHITE)).fill(ACCENT())).clicked() {
                                save = true;
                            }
                        });
                    });
                    ui.add_space(6.0);
                    let avail = ui.available_rect_before_wrap();
                    let rect = Rect::from_min_max(avail.min, Pos2::new(avail.right() - 8.0, avail.bottom() - 2.0));
                    ui.allocate_rect(rect, Sense::hover());
                    ui.painter().rect_stroke(rect.expand(1.0), CornerRadius::same(6), Stroke::new(1.0, BORDER()), egui::StrokeKind::Outside);
                    if p.failed {
                        ui.put(rect, egui::Label::new(RichText::new("Podgląd niedostępny — użyj „Otwórz w programie”.").color(MUTED())));
                        return;
                    }
                    match &p.web {
                        None => {
                            p.tries += 1;
                            if let Some(hwnd) = crate::compose_view::find_window(&preview_marker(p.key)) {
                                match crate::webview::file_view(hwnd, &p.path, rect) {
                                    Ok(v) => {
                                        p.web = Some(v);
                                        p.last = Some(rect);
                                    }
                                    Err(e) => {
                                        crate::diag::log(format!("attachment preview: not available ({e})"));
                                        p.failed = true;
                                    }
                                }
                            } else if p.tries > 120 {
                                p.failed = true;
                            } else {
                                ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
                            }
                        }
                        Some((v, _)) => {
                            if p.last != Some(rect) {
                                crate::webview::place(v, rect);
                                p.last = Some(rect);
                            }
                        }
                    }
                });
                if !root.ctx().embed_viewports() {
                    window_edges(root);
                }
            });
            if save {
                self.actions.push(Action::SaveAttachment(p.msg, p.index));
            }
            if outside {
                open_with_windows(std::path::Path::new(&p.path));
            }
            if !open {
                closed.push(p.key);
            }
        }
        previews.retain(|p| !closed.contains(&p.key));
        previews.append(&mut self.previews);
        self.previews = previews;
    }
}

/// An attachment's name with its size, shortened in the middle (keeping the extension) to fit.
pub(super) fn fit_name(ui: &Ui, name: &str, size: &str, font: &FontId, room: f32) -> String {
    let make = |n: &str| if size.is_empty() { format!("📎 {n}") } else { format!("📎 {n}  ·  {size}") };
    let full = make(name);
    if text_width(ui, &full, font.clone()) <= room {
        return full;
    }
    let chars: Vec<char> = name.chars().collect();
    let ext = name.rfind('.').map(|i| name[i..].chars().count()).filter(|n| *n <= 6).unwrap_or(0);
    let mut keep = chars.len();
    while keep > 4 {
        keep -= 1;
        let head = (keep.saturating_sub(ext)).max(1);
        let short: String = chars[..head.min(chars.len())].iter().collect::<String>() + "…" + &chars[chars.len() - ext..].iter().collect::<String>();
        let t = make(&short);
        if text_width(ui, &t, font.clone()) <= room {
            return t;
        }
    }
    make(&format!("{}…", chars.iter().take(4).collect::<String>()))
}

/// A formatted email drawn by the app: strips of picture (texture, size in points), for one
/// message at one width.
pub(super) struct EmailPic {
    pub(super) key: u64,
    pub(super) id: u64,
    /// the strips drawn so far (None: still coming), each with its size in points
    pub(super) strips: Vec<(Option<egui::TextureHandle>, Vec2)>,
    pub(super) done: bool,
}

/// As many of the first three names as fit in `room` (at least one), and "+N" for the others.
pub(super) fn fit_names(ui: &Ui, prefix: &str, people: &[(String, String)], room: f32) -> String {
    let font = FontId::proportional(13.5);
    for n in (1..=people.len().clamp(1, 3)).rev() {
        let s = format!("{prefix}{}", short_names(people, n));
        if text_width(ui, &s, font.clone()) <= room || n == 1 {
            return s;
        }
    }
    format!("{prefix}—")
}

/// "Anna Kowalska, Piotr Nowak, Ewa Lis +2": the first `n` names, and how many more.
pub(super) fn short_names(people: &[(String, String)], n: usize) -> String {
    if people.is_empty() {
        return "—".to_string();
    }
    let mut out = people.iter().take(n).map(|p| p.0.clone()).collect::<Vec<_>>().join(", ");
    if people.len() > n {
        out.push_str(&format!("  +{}", people.len() - n));
    }
    out
}

/// The people in an address list ("Name <a@b>, \"Surname, Name\" <c@d>"), as (name, address);
/// commas inside quotes or brackets don't split.
pub(super) fn people_in(list: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let (mut quoted, mut angle, mut start) = (false, false, 0);
    let mut push = |piece: &str| {
        if !piece.trim().is_empty() {
            out.push(split_address(piece));
        }
    };
    for (i, ch) in list.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            '<' if !quoted => angle = true,
            '>' if !quoted => angle = false,
            ',' | ';' if !quoted && !angle => {
                push(&list[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    push(&list[start..]);
    out
}

/// Under the sender: "Do:" and "DW:" with the first three people of each; the rest come out
/// with "+N więcej" (and go back with "Zwiń"). Nothing when it went to one person only.
pub(super) fn recipients_row(ui: &mut Ui, msg: u64, to: &str, cc: &str, indent: f32, everyone: bool) {
    const FIRST: usize = 3;
    let (to, cc) = (people_in(to), people_in(cc));
    if !everyone && to.len() <= 1 && cc.is_empty() {
        return;
    }
    let key = egui::Id::new(("rcpt-open", msg));
    let open = ui.data(|d| d.get_temp::<bool>(key)).unwrap_or(false);
    let hidden = if open { 0 } else { to.len().saturating_sub(FIRST) + cc.len().saturating_sub(FIRST) };
    let mut toggle = false;
    ui.add_space(2.0);
    ui.horizontal_top(|ui| {
    ui.add_space(indent);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(0.0, 3.0);
        let size = 12.5;
        for (label, list) in [("Do: ", &to), ("DW: ", &cc)] {
            if list.is_empty() {
                continue;
            }
            if label == "DW: " && !to.is_empty() {
                ui.add_space(14.0);
            }
            ui.label(RichText::new(label).size(size).color(MUTED()));
            let shown = if open { list.len() } else { list.len().min(FIRST) };
            for (k, (name, addr)) in list.iter().take(shown).enumerate() {
                let text = if k + 1 < shown { format!("{name}, ") } else { name.clone() };
                let r = ui.add(egui::Label::new(RichText::new(text).size(size).color(ACCENT())).sense(Sense::click()));
                // the address above the name (below it the email would cover it); a click copies it
                hint_above(ui, &r, &format!("{addr} — kliknij, aby skopiować"));
                if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    ui.ctx().copy_text(addr.clone());
                }
            }
        }
        if hidden > 0 || open {
            ui.add_space(10.0);
            let more = if open { "Zwiń".to_string() } else { format!("+{hidden} więcej") };
            let r = ui.add(egui::Label::new(RichText::new(more).size(size).color(ACCENT()).underline()).sense(Sense::click()));
            if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                toggle = true;
            }
        }
    });
    });
    if toggle {
        ui.data_mut(|d| d.insert_temp(key, !open));
    }
    ui.add_space(2.0);
}

pub(super) fn hint_above(ui: &Ui, resp: &egui::Response, text: &str) {
    if !resp.hovered() || text.is_empty() {
        return;
    }
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(13.0), TEXT());
    let size = galley.size() + Vec2::new(16.0, 8.0);
    let screen = ui.ctx().content_rect();
    let x = (resp.rect.center().x - size.x / 2.0).clamp(screen.left() + 4.0, screen.right() - size.x - 4.0);
    let r = Rect::from_min_size(Pos2::new(x, resp.rect.top() - size.y - 6.0), size);
    #[allow(deprecated)]
    let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("hint-above"));
    let p = ui.ctx().layer_painter(layer);
    p.rect_filled(r.translate(Vec2::new(0.0, 1.5)), CornerRadius::same(5), Color32::from_black_alpha(18));
    p.rect_filled(r, CornerRadius::same(5), BG());
    p.rect_stroke(r, CornerRadius::same(5), Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
    p.galley(r.min + Vec2::new(8.0, 4.0), galley, TEXT());
}
