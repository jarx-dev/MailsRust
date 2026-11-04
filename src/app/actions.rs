use super::*;

pub(super) enum Action {
    /// The person opened the message (click or arrow keys): it counts as read.
    Select(u64),
    /// Shown without the person asking (e.g. the first message of a folder): stays unread.
    Show(u64),
    /// Open the message in a window of its own (double click).
    OpenWindow(u64),
    /// Share the email over Tailscale and copy a link for an AI agent.
    CopyForAi(u64),
    /// Copy the whole email as text for pasting into an AI chat.
    CopyTextForAi(u64),
    StopAiSharing,
    Reply(u64, bool),
    Forward(u64),
    Delete(u64),
    Move(u64, Folder),
    SetRead(u64, bool),
    /// every unread message of (an account, a folder) marked read; None: every account /
    /// every folder but the bin and spam ("Nieprzeczytane")
    MarkAllRead(Option<u32>, Option<Folder>),
    ToggleFlag(u64),
    EditDraft(u64),
    /// a message sent before, opened for sending again (it can be changed first)
    SendAgain(u64),
    /// a received message translated into Polish by AI (or back to the original)
    Translate(u64),
    /// marked as needing no reply (or back), for the list without replies
    NoReplyNeeded(u64, bool),
    NewMessage,
    SendReceive,
    OpenAttachment(u64, usize),
    /// shown in a window of the app (PDF, pictures, text)
    PreviewAttachment(u64, usize),
    SaveAttachment(u64, usize),
    AddTag(u64, String),
    RemoveTag(u64, String),
}

impl MailApp {
    pub(super) fn apply_actions(&mut self) {
        let actions = std::mem::take(&mut self.actions);
        for a in actions {
            // an action may have changed messages: look again
            self.touched = true;
            self.refresh_caches();
            match a {
                Action::Select(id) | Action::Show(id) => {
                    self.trim_pictures();
                    // the message read until now gives its text back to the disk
                    if let Some(prev) = self.selected.filter(|p| *p != id) {
                        if let (Some(db), Some(i)) = (self.db.as_ref(), self.index.get(&prev).copied()) {
                            if let Some(m) = self.store.messages.get_mut(i).filter(|m| m.id == prev) {
                                db.release(m);
                            }
                        }
                    }
                    self.ensure_body(id);
                    self.selected = Some(id);
                    let opened = matches!(a, Action::Select(_));
                    let was_unread = self.msg(id).map(|m| !m.read).unwrap_or(false);
                    if opened && was_unread {
                        if let Some(m) = self.msg_mut(id) {
                            m.read = true;
                        }
                        self.queue_op(id, |uid| net::Op::Seen { uid, on: true });
                    }
                    // a big message stored with its header only: fetch the rest now
                    let partial = self
                        .msg(id)
                        .filter(|m| m.partial && matches!(m.folder, Folder::Inbox | Folder::Sent))
                        .and_then(|m| m.uid.map(|u| (m.folder, u, m.account)));
                    if let Some((folder, uid, acct)) = partial {
                        if self.account_ref(acct).can_connect() && self.fetching_full.insert((folder, uid)) {
                            net::fetch_full(self.account_for(acct), acct, uid, folder, self.tx.clone(), self.ctx.clone());
                        }
                    }
                }
                Action::OpenWindow(id) => {
                    if let Some(r) = self.readers.iter().find(|r| r.msg.id == id) {
                        // already open: brought to the front
                        self.ctx.send_viewport_cmd_to(egui::ViewportId::from_hash_of(("reader", r.key)), egui::ViewportCommand::Focus);
                    } else {
                        self.ensure_body(id);
                        if let Some(m) = self.msg(id).cloned() {
                            let key = self.next_key;
                            self.next_key += 1;
                            self.readers.push(ReaderWin { key, msg: m, web: None, failed: false, tries: 0, last: None });
                        }
                    }
                }
                Action::CopyForAi(id) => {
                    self.ensure_body(id);
                    self.copy_for_ai(id)
                }
                Action::CopyTextForAi(id) => {
                    self.ensure_body(id);
                    if let Some(m) = self.msg(id) {
                        self.ctx.copy_text(crate::share::Shared::of(m).plain());
                        self.flash("Treść e-maila skopiowana do schowka — wklej ją w czacie z AI");
                        self.set_status("Skopiowano treść e-maila — wklej ją w czacie z AI");
                    }
                }
                Action::StopAiSharing => {
                    self.hub.clear();
                    self.prefs.ai_shares.clear();
                    self.save_now = true;
                    self.set_status("Linki dla AI wyłączone");
                }
                Action::AddTag(id, tag) => {
                    let tag = tag.trim().to_string();
                    if tag.is_empty() {
                        continue;
                    }
                    let added = self.msg_mut(id).map(|m| {
                        if m.labels.contains(&tag) {
                            false
                        } else {
                            m.labels.push(tag.clone());
                            true
                        }
                    });
                    if added == Some(true) {
                        let t = tag.clone();
                        self.queue_op(id, move |uid| net::Op::Label { uid, label: t, add: true });
                    }
                }
                Action::RemoveTag(id, tag) => {
                    let removed = self.msg_mut(id).map(|m| {
                        let before = m.labels.len();
                        m.labels.retain(|l| *l != tag);
                        before != m.labels.len()
                    });
                    if removed == Some(true) {
                        let t = tag.clone();
                        self.queue_op(id, move |uid| net::Op::Label { uid, label: t, add: false });
                    }
                }
                Action::Reply(id, all) => {
                    self.ensure_body(id);
                    if let Some(m) = self.msg(id).cloned() {
                        // (the address of the account the message came to)
                        let me = self.account_ref(m.account).email.to_lowercase();
                        // the person written back to, with their name (unless the name would confuse the list)
                        let to = if m.folder == Folder::Sent {
                            m.to.clone()
                        } else if m.from_name.trim().is_empty() || m.from_name == m.from_addr || m.from_name.contains([',', ';', '"', '<', '>']) {
                            m.from_addr.clone()
                        } else {
                            format!("{} <{}>", m.from_name.trim(), m.from_addr)
                        };
                        let cc = if all {
                            m.to.split(',')
                                .chain(m.cc.split(','))
                                .map(|s| split_address(s).1)
                                .filter(|a| !a.is_empty() && a.to_lowercase() != me && *a != m.from_addr && !to.contains(a.as_str()))
                                .collect::<Vec<_>>()
                                .join(", ")
                        } else {
                            String::new()
                        };
                        let subj = if m.subject.to_lowercase().starts_with("re:") {
                            m.subject.clone()
                        } else {
                            format!("Re: {}", m.subject)
                        };
                        self.compose_account = m.account;
                        self.open_composer(to, cc, subj, String::new(), None, Some(Original::from(&m, false)));
                    }
                }
                Action::Forward(id) => {
                    self.ensure_body(id);
                    if let Some(m) = self.msg(id).cloned() {
                        let orig = Original::from(&m, true);
                        self.compose_account = m.account;
                        self.open_composer(String::new(), String::new(), format!("Fwd: {}", m.subject), String::new(), None, Some(orig));
                    }
                }
                Action::Delete(id) => {
                    // the next message of the list (or the one before, at its end): chosen now, from
                    // the list as it is before the message goes
                    let next = self.neighbour(id);
                    let in_trash = self.msg(id).map(|m| m.folder == Folder::Trash).unwrap_or(false);
                    if in_trash {
                        // deleted for good, also on the server
                        self.queue_op(id, |uid| net::Op::Expunge { uid });
                        self.store.messages.retain(|m| m.id != id);
                        self.set_status("Wiadomość usunięta na stałe");
                    } else {
                        // also into the server's Trash (Gmail: Kosz)
                        self.queue_op(id, |uid| net::Op::Trash { uid });
                        if let Some(m) = self.msg_mut(id) {
                            m.folder = Folder::Trash;
                            m.uid = None;
                        }
                        self.set_status("Przeniesiono do kosza");
                    }
                    if self.selected == Some(id) {
                        self.go_to_neighbour(next);
                    }
                }
                Action::Move(id, f) => {
                    let next = self.neighbour(id);
                    let leaves = self.msg(id).is_some_and(|m| m.folder != f);
                    let from = self.msg(id).map(|m| m.folder);
                    match (from, f) {
                        (Some(a), b) if a == b => {}
                        (_, Folder::Trash) => self.queue_op(id, |uid| net::Op::Trash { uid }),
                        (_, Folder::Junk) => self.queue_op(id, |uid| net::Op::Junk { uid }),
                        (Some(Folder::Inbox), Folder::Archive) => self.queue_op(id, |uid| net::Op::Archive { uid }),
                        // out of the bin or spam, back into the inbox
                        (Some(Folder::Trash | Folder::Junk), Folder::Inbox) => self.queue_op(id, |uid| net::Op::ToInbox { uid }),
                        _ => {}
                    }
                    if let Some(m) = self.msg_mut(id) {
                        if m.folder != f {
                            // its number belonged to the old folder; the sync finds it again
                            m.uid = None;
                        }
                        m.folder = f;
                    }
                    self.set_status(format!("Przeniesiono do: {}", f.name()));
                    // moved out of the list: the next one is shown, like after deleting
                    if leaves && self.selected == Some(id) && !self.visible_ids().contains(&id) {
                        self.go_to_neighbour(next);
                    }
                }
                Action::MarkAllRead(acct, folder) => {
                    let ids: Vec<u64> = self
                        .store
                        .messages
                        .iter()
                        .filter(|m| !m.read && acct.is_none_or(|a| m.account == a))
                        .filter(|m| match folder {
                            Some(f) => m.folder == f,
                            None => !matches!(m.folder, Folder::Trash | Folder::Junk),
                        })
                        .map(|m| m.id)
                        .collect();
                    for id in &ids {
                        if let Some(m) = self.msg_mut(*id) {
                            m.read = true;
                        }
                        // (on the server too)
                        self.queue_op(*id, |uid| net::Op::Seen { uid, on: true });
                    }
                    self.save_now = true;
                    self.set_status(format!("Oznaczono jako przeczytane: {}", ids.len()));
                }
                Action::SetRead(id, r) => {
                    if let Some(m) = self.msg_mut(id) {
                        m.read = r;
                    }
                    if !r {
                        // marked unread by hand: it stays so while it is on screen
                        let now = self.clock();
                        self.read_timer = Some((id, now, true));
                    }
                    self.queue_op(id, move |uid| net::Op::Seen { uid, on: r });
                }
                Action::ToggleFlag(id) => {
                    let mut on = false;
                    if let Some(m) = self.msg_mut(id) {
                        m.flagged = !m.flagged;
                        on = m.flagged;
                    }
                    self.queue_op(id, move |uid| net::Op::Flagged { uid, on });
                }
                Action::EditDraft(id) => {
                    self.ensure_body(id);
                    if let Some(m) = self.msg(id).cloned() {
                        let pics = m.inline.clone();
                        self.compose_account = m.account;
                        self.open_composer(m.to, m.cc, m.subject, m.body, Some(id), None);
                        if let Some(c) = self.composers.last_mut() {
                            c.images = pics;
                        }
                    }
                }
                Action::SendAgain(id) => {
                    self.ensure_body(id);
                    if let Some(m) = self.msg(id).cloned() {
                        // the message as it was sent: its formatted text with pictures, signature and
                        // all, and its attached files
                        let html = (!m.html_path.is_empty())
                            .then(|| std::fs::read_to_string(&m.html_path).ok())
                            .flatten()
                            .map(|h| crate::compose_view::show_cids(&crate::compose_view::body_of(&h), &m.inline))
                            // a plain message as it was (its signature is already in the text)
                            .or_else(|| Some(crate::signature::text_to_html(&m.body)));
                        self.compose_start = html;
                        self.compose_account = m.account;
                        self.open_composer(m.to.clone(), m.cc.clone(), m.subject.clone(), m.body.clone(), None, None);
                        self.compose_start = None;
                        if let Some(c) = self.composers.last_mut() {
                            c.images = m.inline.clone();
                            // the text already has its signature: none added when sending
                            c.signature = false;
                            c.files = m.attachment_files.iter().filter(|f| std::path::Path::new(f).is_file()).cloned().collect();
                        }
                    }
                }
                Action::Translate(id) => {
                    if self.translations.contains_key(&id) {
                        // already translated: switch between the translation and the original
                        if !self.show_translated.remove(&id) {
                            self.show_translated.insert(id);
                        }
                        self.mail_view.reload();
                        continue;
                    }
                    if self.translating.contains_key(&id) {
                        continue;
                    }
                    self.ensure_body(id);
                    let Some(m) = self.msg(id).cloned() else { continue };
                    let html = (!m.html_path.is_empty()).then(|| std::fs::read_to_string(&m.html_path).ok()).flatten();
                    let text = m.body.clone();
                    let (tx, rx) = channel();
                    let ctx = self.ctx.clone();
                    std::thread::spawn(move || {
                        let _ = tx.send(crate::translate::translate_email(&crate::ai::load(), html, &text));
                        ctx.request_repaint();
                    });
                    self.translating.insert(id, rx);
                    self.translate_error = None;
                }
                Action::NoReplyNeeded(id, on) => {
                    // the message to show next in the list without replies (worked out while it is still there)
                    let next = (on && self.view == MailView::NoReply && self.selected == Some(id))
                        .then(|| {
                            let ids = self.visible_ids();
                            ids.iter().position(|x| *x == id).and_then(|p| ids.get(p + 1).or_else(|| p.checked_sub(1).and_then(|q| ids.get(q)))).copied()
                        })
                        .flatten();
                    self.prefs.no_reply_needed.retain(|x| *x != id);
                    if on {
                        self.prefs.no_reply_needed.push(id);
                        if self.view == MailView::NoReply && self.selected == Some(id) {
                            self.selected = next;
                        }
                    }
                    self.touched = true;
                    self.save_now = true;
                    self.set_status(if on { "Oznaczono: nie wymaga odpowiedzi" } else { "Oznaczono: wymaga odpowiedzi" });
                }
                Action::NewMessage => {
                    // a new message goes from the account whose folders are shown
                    self.compose_account = self.view_account;
                    self.open_composer(String::new(), String::new(), String::new(), String::new(), None, None)
                }
                Action::SendReceive => {
                    // asked for by the person: always show it
                    self.quiet_sync = false;
                    self.send_receive();
                }
                Action::PreviewAttachment(id, i) => {
                    let Some(m) = self.msg(id) else { continue };
                    let name = safe_name(m.attachments.get(i).map(String::as_str).unwrap_or("załącznik"));
                    let Some(src) = m.attachment_files.get(i).filter(|f| std::path::Path::new(f).exists()).cloned() else {
                        self.set_status("Tego załącznika nie ma jeszcze na dysku — kliknij Odśwież, aby go pobrać");
                        continue;
                    };
                    // a copy with its real name (the viewer and "Otwórz w programie" use it)
                    let dir = crate::model::cache_dir().parent().map(|p| p.join("attachments")).unwrap_or_default();
                    let _ = std::fs::create_dir_all(&dir);
                    let dest = dir.join(&name);
                    if std::fs::copy(&src, &dest).is_err() {
                        self.set_status("Nie udało się otworzyć załącznika");
                        continue;
                    }
                    let key = self.next_key;
                    self.next_key += 1;
                    self.previews.push(PreviewWin { key, msg: id, index: i, name, path: dest.to_string_lossy().into_owned(), web: None, failed: false, tries: 0, last: None });
                }
                Action::OpenAttachment(id, i) | Action::SaveAttachment(id, i) => {
                    let save = matches!(a, Action::SaveAttachment(..));
                    let Some(m) = self.msg(id) else { continue };
                    let name = safe_name(m.attachments.get(i).map(String::as_str).unwrap_or("załącznik"));
                    let Some(src) = m.attachment_files.get(i).filter(|f| std::path::Path::new(f).exists()).cloned() else {
                        self.set_status("Tego załącznika nie ma jeszcze na dysku — kliknij Odśwież, aby go pobrać");
                        continue;
                    };
                    if save {
                        let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
                        let dir = std::path::Path::new(&home).join("Downloads");
                        let _ = std::fs::create_dir_all(&dir);
                        // don't overwrite: "plik.pdf", "plik (1).pdf", …
                        let p = std::path::Path::new(&name);
                        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("załącznik").to_string();
                        let ext = p.extension().and_then(|s| s.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
                        let mut dest = dir.join(&name);
                        let mut n = 1;
                        while dest.exists() {
                            dest = dir.join(format!("{stem} ({n}){ext}"));
                            n += 1;
                        }
                        match std::fs::copy(&src, &dest) {
                            Ok(_) => {
                                let shown = dest.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                                self.set_status(format!("Zapisano w Pobranych: {shown}"));
                                let _ = std::process::Command::new("explorer.exe")
                                    .arg(format!("/select,{}", dest.display()))
                                    .spawn();
                            }
                            Err(e) => self.set_status(format!("Nie udało się zapisać załącznika: {e}")),
                        }
                    } else {
                        // Open a copy with its real name, so the right program opens it.
                        let dir = crate::model::cache_dir().parent().map(|p| p.join("attachments")).unwrap_or_default();
                        let _ = std::fs::create_dir_all(&dir);
                        let dest = dir.join(&name);
                        match std::fs::copy(&src, &dest) {
                            Ok(_) => open_with_windows(&dest),
                            Err(e) => self.set_status(format!("Nie udało się otworzyć załącznika: {e}")),
                        }
                    }
                }
            }
        }
    }

    /// The message after uid=197608(Jarek) gid=197121 groups=197121 in the list shown (or the one before it, when it is the last).
    pub(super) fn neighbour(&self, id: u64) -> Option<u64> {
        let ids = self.visible_ids();
        let pos = ids.iter().position(|x| *x == id)?;
        ids.get(pos + 1).or_else(|| pos.checked_sub(1).and_then(|p| ids.get(p))).copied()
    }

    /// Shows the message chosen next in the reading pane (the list keeps the keyboard; it stays unread until read).
    pub(super) fn go_to_neighbour(&mut self, next: Option<u64>) {
        // (the lists are made again with the message gone)
        self.touched = true;
        self.refresh_caches();
        self.selected = next;
        if let Some(n) = next {
            self.actions.push(Action::Show(n));
        }
    }

    /// Test (on a copy of a mailbox, offline): shows the messages of an account's inbox one
    /// after another, so the log tells how long each email takes to appear; then closes.
    pub(super) fn nav_test_step(&mut self, ctx: &egui::Context) {
        // test: a new message window opened right away (a test folder, to try typing in it)
        if std::env::var("POCZTA_OPEN_COMPOSE").is_ok() && self.loading.is_none() {
            use std::sync::atomic::{AtomicBool, Ordering};
            static DONE: AtomicBool = AtomicBool::new(false);
            if !DONE.swap(true, Ordering::Relaxed) {
                self.account_open = false;
                // (a test email from a file goes to the inbox first, to answer or forward it)
                if let Ok(path) = std::env::var("POCZTA_EML") {
                    if let Some(mut m) = std::fs::read(&path).ok().and_then(|raw| net::parse(&raw, true, false)) {
                        m.id = self.store.new_id();
                        m.folder = Folder::Inbox;
                        m.date = Local::now().naive_local();
                        self.store.messages.push(m);
                        self.touched = true;
                        self.refresh_caches();
                    }
                }
                let first = self.store.messages.iter().filter(|m| m.folder == Folder::Inbox).max_by_key(|m| m.date).map(|m| m.id);
                match (std::env::var("POCZTA_OPEN_COMPOSE").as_deref(), first) {
                    (Ok("reply"), Some(id)) => self.actions.push(Action::Reply(id, false)),
                    (Ok("forward"), Some(id)) => self.actions.push(Action::Forward(id)),
                    _ => self.actions.push(Action::NewMessage),
                }
            }
        }
        let Ok(acct) = std::env::var("POCZTA_NAV_TEST") else { return };
        if self.loading.is_some() || self.store.messages.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
            return;
        }
        let now = self.clock();
        let (n, last) = self.nav_test.unwrap_or((0, now - 10.0));
        if n == 0 && self.nav_test.is_none() {
            self.view = MailView::Folder(Folder::Inbox);
            self.view_account = acct.parse().unwrap_or(0);
        }
        if now - last >= 3.0 {
            if n >= 10 {
                crate::diag::log("nav test: done");
                self.quit();
                return;
            }
            let ids = self.visible_ids();
            if let Some(id) = ids.get(n).copied() {
                crate::diag::log(format!("nav test: message {} of the list", n + 1));
                self.actions.push(Action::Show(id));
            }
            self.nav_test = Some((n + 1, now));
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}
