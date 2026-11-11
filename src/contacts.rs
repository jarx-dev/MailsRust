//! Address suggestions while writing: everyone found in the mailbox (senders, and the people
//! you wrote to), best first. People you wrote to yourself count more than senders.

use crate::model::{split_address, Folder, Message};
use eframe::egui::{self, Align2, FontId, Key, Modifiers, Sense, Ui, Vec2};
use std::collections::HashMap;

#[derive(Clone)]
pub struct Contact {
    pub name: String,
    pub addr: String,
    score: f32,
    /// lower-case name and address, for matching
    key: String,
    /// a group of the address book: everyone's address ("Name <a@b>")
    pub members: Vec<String>,
}

impl Contact {
    /// A group of the address book, offered while writing: choosing it writes to everyone.
    pub fn group(name: &str, members: Vec<String>) -> Contact {
        Contact { name: name.to_string(), addr: String::new(), score: f32::MAX, key: fold(&format!("{name} grupa group")), members }
    }

    /// A person from the address book.
    pub fn person(name: &str, addr: &str) -> Contact {
        Contact { name: name.to_string(), addr: addr.to_string(), score: 0.5, key: fold(&format!("{name} {addr}")), members: vec![] }
    }

    pub fn text(&self) -> String {
        if !self.members.is_empty() {
            return self.members.join(", ");
        }
        if self.name.is_empty() || self.name.eq_ignore_ascii_case(&self.addr) {
            self.addr.clone()
        } else {
            format!("{} <{}>", self.name, self.addr)
        }
    }
}

fn fold(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'ą' => 'a',
            'ć' => 'c',
            'ę' => 'e',
            'ł' => 'l',
            'ń' => 'n',
            'ó' => 'o',
            'ś' => 's',
            'ź' | 'ż' => 'z',
            c => c,
        })
        .collect()
}

/// Collects everyone from the mailbox. `me` is left out.
pub fn collect(msgs: &[Message], me: &str) -> Vec<Contact> {
    let me = me.trim().to_lowercase();
    let newest = msgs.iter().map(|m| m.date).max();
    let mut map: HashMap<String, Contact> = HashMap::new();
    let mut add = |name: &str, addr: &str, weight: f32| {
        let addr = addr.trim().trim_matches(|c| c == '<' || c == '>' || c == '"');
        if !addr.contains('@') || addr.contains(' ') {
            return;
        }
        let low = addr.to_lowercase();
        if low == me || low.contains("no-reply") || low.contains("noreply") || low.contains("do-not-reply") || low.starts_with("mailer-daemon") {
            return;
        }
        let name = name.trim().trim_matches('"').trim();
        let name = if name.eq_ignore_ascii_case(addr) { "" } else { name };
        let e = map.entry(low.clone()).or_insert_with(|| Contact { name: String::new(), addr: addr.to_string(), score: 0.0, key: String::new(), members: vec![] });
        e.score += weight;
        if e.name.is_empty() && !name.is_empty() {
            e.name = name.to_string();
        }
    };
    for m in msgs {
        // newer mail counts more: full weight this year, fading over the years before
        let age_days = newest.map(|n| (n - m.date).num_days().max(0) as f32).unwrap_or(0.0);
        let fresh = 1.0 / (1.0 + age_days / 365.0);
        match m.folder {
            Folder::Sent | Folder::Outbox | Folder::Drafts => {
                for part in m.to.split(',').chain(m.cc.split(',')) {
                    if part.trim().is_empty() {
                        continue;
                    }
                    let (n, a) = split_address(part);
                    add(&n, &a, 4.0 * fresh);
                }
            }
            Folder::Junk | Folder::Trash => {}
            _ => add(&m.from_name, &m.from_addr, fresh),
        }
    }
    let mut out: Vec<Contact> = map
        .into_values()
        .map(|mut c| {
            c.key = fold(&format!("{} {}", c.name, c.addr));
            c
        })
        .collect();
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// Best matches for what was typed: any word of the name or the address starting with it.
pub fn find<'a>(all: &'a [Contact], typed: &str, skip: &str, limit: usize) -> Vec<&'a Contact> {
    let q = fold(typed.trim());
    if q.is_empty() {
        return vec![];
    }
    let skip = skip.to_lowercase();
    all.iter()
        .filter(|c| !c.members.is_empty() || !skip.contains(&c.addr.to_lowercase()))
        .filter(|c| {
            c.key.starts_with(&q)
                || c.key.split(|ch: char| ch == ' ' || ch == '.' || ch == '@' || ch == '_' || ch == '-').any(|w| w.starts_with(&q))
        })
        .take(limit)
        .collect()
}

/// Whether the list of suggestions of this recipients box is showing.
/// Where the list of suggestions of a recipients box lies now (when it is open).
pub fn list_rect(ctx: &egui::Context, salt: impl std::hash::Hash + std::fmt::Debug) -> Option<egui::Rect> {
    #[allow(deprecated)]
    let id = egui::Id::new(("recipients", salt));
    if !ctx.data(|d| d.get_temp(id.with("open")).unwrap_or(false)) {
        return None;
    }
    ctx.memory(|m| m.area_rect(id.with("list")))
}

/// A one-line recipients box with suggestions under it. Arrow keys pick, Enter or Tab accepts.
pub fn recipients_field(ui: &mut Ui, text: &mut String, hint: &str, width: f32, contacts: &[Contact], salt: impl std::hash::Hash + std::fmt::Debug) -> egui::Response {
    #[allow(deprecated)]
    let id = egui::Id::new(("recipients", salt));
    let edit_id = id.with("edit");
    let sel_id = id.with("pick");
    let open_id = id.with("open");
    let was_open: bool = ui.data(|d| d.get_temp(open_id).unwrap_or(false));
    let mut pick: usize = ui.data(|d| d.get_temp(sel_id).unwrap_or(0));
    // keys for the list are taken before the box sees them (Enter would leave the box, Tab move on)
    let (mut accept, mut close) = (false, false);
    if was_open && ui.memory(|m| m.has_focus(edit_id)) {
        ui.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                pick += 1;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                pick = pick.saturating_sub(1);
            }
            if i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Tab) {
                accept = true;
            }
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                close = true;
            }
        });
    }
    let resp = ui.add(egui::TextEdit::singleline(text).id(edit_id).hint_text(hint).desired_width(width));
    if resp.changed() {
        pick = 0;
    }
    // what is being typed: the part after the last comma
    let start = text.rfind([',', ';']).map(|i| i + 1).unwrap_or(0);
    let typed = text[start..].trim().to_string();
    let before = text[..start].to_string();
    let found = if close || typed.contains('<') { vec![] } else { find(contacts, &typed, &before, 6) };
    let hovering: bool = ui.data(|d| d.get_temp(id.with("hover")).unwrap_or(false));
    let show = !found.is_empty() && (resp.has_focus() || hovering);
    pick = pick.min(found.len().saturating_sub(1));
    let mut chosen: Option<String> = None;
    if accept && show {
        chosen = Some(found[pick].text());
    }
    if show && chosen.is_none() {
        // The list is a small window of its own, over everything (like a Windows drop-down):
        // a browser view in the window below (the formatted writing field) stays where it is.
        let w = resp.rect.width().clamp(260.0, 480.0);
        let size = Vec2::new(w + 10.0, found.len() as f32 * 38.0 + 10.0);
        let origin = ui.ctx().input(|i| i.viewport().inner_rect).map(|r| r.min).unwrap_or(egui::Pos2::ZERO);
        let at = origin + resp.rect.left_bottom().to_vec2() + Vec2::new(0.0, 2.0);
        let builder = egui::ViewportBuilder::default()
            .with_title(t!("compose.contacts.suggestions").into_owned())
            .with_decorations(false)
            .with_resizable(false)
            .with_taskbar(false)
            .with_always_on_top()
            .with_active(false)
            .with_position(at)
            .with_inner_size(size);
        let mut over = false;
        ui.ctx().show_viewport_immediate(egui::ViewportId::from_hash_of(id.with("listwin")), builder, |root, _| {
            egui::CentralPanel::default().frame(egui::Frame::popup(root.style()).inner_margin(4)).show(root, |ui| {
                over = ui.ctx().pointer_hover_pos().is_some();
                {
                    {
                    for (i, c) in found.iter().enumerate() {
                        let (r, row) = ui.allocate_exact_size(Vec2::new(w, 38.0), Sense::click());
                        if i == pick || row.hovered() {
                            ui.painter().rect_filled(r, 4, crate::theme::ACCENT_SOFT());
                        }
                        let group = !c.members.is_empty();
                        let name = if group {
                            format!("👥 {}", c.name)
                        } else if c.name.is_empty() {
                            c.addr.clone()
                        } else {
                            c.name.clone()
                        };
                        let second = if group { t!("contacts.group_of", people = crate::i18n::plural("people", c.members.len())).into_owned() } else { c.addr.clone() };
                        ui.painter().text(r.left_top() + Vec2::new(10.0, 3.0), Align2::LEFT_TOP, &name, FontId::proportional(13.5), crate::theme::TEXT());
                        ui.painter().text(
                            r.left_top() + Vec2::new(10.0, 20.0),
                            Align2::LEFT_TOP,
                            &second,
                            FontId::proportional(11.5),
                            crate::theme::MUTED(),
                        );
                        // chosen as soon as the button goes down
                        if row.clicked() || (row.hovered() && ui.input(|i| i.pointer.primary_pressed())) {
                            chosen = Some(c.text());
                        }
                    }
                }
                }
            });
        });
        ui.data_mut(|d| d.insert_temp(id.with("hover"), over));
    } else {
        ui.data_mut(|d| d.insert_temp(id.with("hover"), false));
    }
    if let Some(addr) = chosen {
        let lead = if before.trim().is_empty() { String::new() } else { format!("{} ", before.trim_end()) };
        *text = format!("{lead}{addr}, ");
        // back into the box, with the cursor at the end, ready for the next person
        ui.memory_mut(|m| m.request_focus(edit_id));
        if let Some(mut st) = egui::text_edit::TextEditState::load(ui.ctx(), edit_id) {
            let n = text.chars().count();
            st.cursor.set_char_range(Some(egui::text_selection::CCursorRange::one(egui::text::CCursor::new(n))));
            st.store(ui.ctx(), edit_id);
        }
        pick = 0;
        ui.data_mut(|d| d.insert_temp(id.with("hover"), false));
        // the message window has the keyboard again (the list's window was clicked)
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Focus);
    }
    ui.data_mut(|d| {
        d.insert_temp(open_id, show);
        d.insert_temp(sel_id, pick);
    });
    resp
}
