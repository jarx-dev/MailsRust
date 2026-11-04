use super::*;

impl MailApp {
    /// A newer build is swapped in when nothing would be lost or disturbed: nothing being written
    /// or sent, and the window minimized or untouched for 10 minutes. The app then closes as
    /// usual (saving everything) and the new build starts in its place.
    pub(super) fn install_update(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| !i.events.is_empty() || i.pointer.is_moving()) {
            self.last_input = self.clock();
        }
        if let Some(p) = self.update_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.update_staged = Some(p);
        }
        let Some(staged) = self.update_staged.clone() else { return };
        let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        let quiet = minimized || self.clock() - self.last_input > 600.0;
        let working = !self.composers.is_empty() || self.busy > 0 || self.ops_running || self.receiving || !self.pending_ops.is_empty() || self.quitting || self.signing_in || self.book_rx.is_some();
        // forms hold their text only in memory; reading windows get input the main window doesn't see
        let open = self.editor.is_some() || self.book_edit.is_some() || self.extra_edit.is_some() || !self.readers.is_empty() || !self.previews.is_empty();
        if !quiet || working || open {
            ctx.request_repaint_after(std::time::Duration::from_secs(30));
            return;
        }
        self.update_staged = None;
        match crate::update::apply(&staged, minimized) {
            Ok(()) => {
                crate::diag::log("update: new build in place, closing for the restart");
                self.quit();
            }
            Err(e) => {
                crate::diag::log(format!("update: not installed: {e}"));
                // looked for again (the staged file is reused), e.g. after a passing file lock
                self.update_rx = crate::update::start();
            }
        }
    }

    /// Takes over the mailbox once it has been read from disk (see `new`).
    pub(super) fn poll_loading(&mut self, frame: &eframe::Frame) {
        let Some(rx) = &self.loading else { return };
        let (db, from_db, entries) = match rx.try_recv() {
            Ok(Loaded::All(db, from_db, entries)) => (db, from_db, entries),
            Ok(Loaded::Newest(store, reader)) => {
                // the newest messages to look at while the rest is read (nothing is saved, sent
                // or done with them: the window is just to look at, see `ui`)
                self.store = store;
                self.preview = Some(reader);
                self.touched = true;
                self.refresh_caches();
                if self.selected.is_none() {
                    self.selected = self.visible_ids().first().copied();
                }
                crate::diag::log(format!("startup: newest messages shown {} ms after opening", self.opened.elapsed().as_millis()));
                return;
            }
            Err(_) => {
                // meanwhile, get the browser view for formatted mail ready (it takes a moment to
                // start), from the second frame on so the window itself shows first
                // (only for the browser view kept as a fallback: emails are drawn by the app)
                if self.drawn >= 2 && self.shot.is_none() && std::env::var("POCZTA_BLITZ").is_err() {
                    self.mail_view.prepare(frame);
                }
                self.ctx.request_repaint_after(std::time::Duration::from_millis(30));
                return;
            }
        };
        self.loading = None;
        // the message picked among the newest ones only: the real newest is chosen below (older
        // mail fetched later has higher numbers, so the preview may not have it)
        if self.preview.take().is_some() {
            self.selected = None;
        }
        // (a build that can't read the mailbox it finds is not confirmed to its update guard)
        let loaded = from_db.is_some();
        // nothing in the database yet: the older single-file storage, moved over once
        let old = if from_db.is_none() { frame.storage().and_then(|s| eframe::get_value::<Store>(s, "store")) } else { None };
        let migrating = old.is_some() && db.is_some();
        let mut store = match (from_db, old) {
            (Some(s), _) => s,
            (None, Some(mut s)) => {
                prepare_store(&mut s);
                s
            }
            (None, None) => Store::sample(),
        };
        if store.account.oauth_client_id.trim().is_empty() {
            if let Some((id, secret)) = crate::oauth::builtin_client() {
                store.account.oauth_client_id = id;
                store.account.oauth_client_secret = secret;
            }
        }
        if db.is_none() && crate::db::path().exists() {
            // the mailbox is open in another window of the app (or can't be opened): never
            // save anything from here, it would overwrite the real mailbox
            self.read_only = true;
            crate::diag::log("startup: the mailbox is in use by another window; this one won't save");
        }
        // the saved encrypted password belongs to the password just read from it
        self.protected_pw = Some(store.account.password.clone());
        self.store = store;
        self.db = db;
        self.touched = true;
        if !self.read_only && self.shot.is_none() {
            // the mailbox works: a freshly installed build tells its guard, then updates are looked for
            crate::update::startup_complete(loaded);
            self.update_rx = crate::update::start();
        }
        if self.read_only {
            self.set_status("Poczta jest już otwarta w innym oknie — zamknij to okno");
        }
        if migrating {
            self.save_now = true;
        }
        self.refresh_caches();
        if let Some(d) = self.db.as_ref() {
            self.finder.set_reader(d.reader());
        }
        if let Some(e) = entries {
            self.finder.prebuild(e, self.struct_sig, &self.ctx);
        }
        if self.selected.is_none() {
            self.selected = self.visible_ids().first().copied();
            if let Some(id) = self.selected {
                self.actions.push(Action::Show(id));
            }
        }
        crate::diag::log(format!("startup: mailbox shown {} ms after opening", self.opened.elapsed().as_millis()));
        crate::diag::give_back_memory();
        if std::env::var("POCZTA_START").as_deref() == Ok("openwindow") {
            // test: the formatted newsletter opened in a window of its own, as after a double click
            if let Some(id) = self.store.messages.iter().find(|m| !m.html_path.is_empty()).map(|m| m.id) {
                self.actions.push(Action::OpenWindow(id));
            }
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("closetest") {
            // test: the window's close button pressed (it should only minimize)
            self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("emptyreply") {
            // test: a reply opened and closed without writing anything
            if let Some(id) = self.selected {
                self.actions.push(Action::Reply(id, false));
            }
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("webreply") {
            // test: a reply in the formatted writing field, with a signature that has a photo
            let photo = crate::model::cache_put(&std::fs::read("assets/poczta.png").unwrap_or_default(), "png");
            let a = &mut self.store.account;
            a.signature_on = true;
            a.signature_text = "Aleks Nowicki
specjalista ds. Informatyki".into();
            a.signature_html = "<table style=\"font-family:Arial,sans-serif;font-size:14px\"><tr><td style=\"width:90px;vertical-align:top\"><img src=\"cid:photo\" width=\"72\" style=\"border-radius:50%\"></td><td style=\"vertical-align:top\"><strong style=\"font-size:16px;color:rgb(0,91,170)\">Aleks <span style=\"color:rgb(198,40,40)\">Nowicki</span></strong><br>specjalista ds. Informatyki<br><span style=\"color:#777\">tel. 600-100-200</span></td></tr></table>".into();
            a.signature_images = vec![("photo".into(), photo)];
            if let Some(id) = self.selected {
                self.actions.push(Action::Reply(id, false));
            }
        }
        self.restore_ai_shares();
    }

    /// Reply drafts written by an AI agent: they go to Drafts, ready to check and send.
    pub(super) fn poll_ai_drafts(&mut self) {
        while let Ok((oid, text)) = self.hub.drafts.try_recv() {
            self.ensure_body(oid);
            let Some(orig) = self.msg(oid).cloned() else { continue };
            let subject = if orig.subject.to_lowercase().starts_with("re:") { orig.subject.clone() } else { format!("Re: {}", orig.subject) };
            let to = if orig.from_name.is_empty() || orig.from_name == orig.from_addr {
                orig.from_addr.clone()
            } else {
                format!("{} <{}>", orig.from_name, orig.from_addr)
            };
            let mut body = text;
            body.push_str(&Original::from(&orig, false).as_text());
            let id = self.store.new_id();
            let acc = self.store.account.clone();
            self.store.messages.push(Message {
                id,
                folder: Folder::Drafts,
                account: 0,
                from_name: acc.display_name.clone(),
                from_addr: acc.email.clone(),
                to,
                cc: String::new(),
                subject,
                body,
                date: Local::now().naive_local(),
                read: true,
                flagged: false,
                attachments: vec![],
                sample: false,
                html_path: String::new(),
                inline: vec![],
                attachment_files: vec![],
                labels: vec![],
                uid: None,
                body_path: String::new(),
                partial: false,
                snippet: String::new(),
                on_disk: false,
            });
            self.save_now = true;
            self.set_status("AI przygotowało szkic odpowiedzi — znajdziesz go w Szkicach");
            self.touched = true;
        }
    }

    /// New unread mail came: a sound, the taskbar button blinks, and (when the window is not in
    /// front) a note in the corner of the screen.
    pub(super) fn new_mail(&mut self, ids: Vec<u64>) {
        crate::diag::log(format!("new mail: {} unread", ids.len()));
        if !self.prefs.notify || ids.is_empty() {
            return;
        }
        crate::notify::sound();
        let (focused, minimized) = self.ctx.input(|i| (i.viewport().focused.unwrap_or(false), i.viewport().minimized.unwrap_or(false)));
        if focused && !minimized && std::env::var("POCZTA_NOTIFY_TEST").is_err() {
            return;
        }
        self.ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
        let newest = ids.iter().filter_map(|id| self.msg(*id)).max_by_key(|m| m.date);
        let (from, subject) = newest
            .map(|m| (if m.from_name.trim().is_empty() { m.from_addr.clone() } else { m.from_name.clone() }, m.subject.clone()))
            .unwrap_or_default();
        let n = ids.len();
        let head = match n {
            1 => "Nowa wiadomość".to_string(),
            2..=4 => format!("{n} nowe wiadomości"),
            _ => format!("{n} nowych wiadomości"),
        };
        let subject_shown = if subject.trim().is_empty() { "(bez tematu)".to_string() } else { subject.clone() };
        crate::popup::show(crate::popup::Note { head, from: from.clone(), subject: subject_shown, dark: self.prefs.dark }, 8, self.popup_click.0.clone(), self.ctx.clone());
        self.popup = Some(NewMail { ids });
        self.ctx.request_repaint();
    }

    /// The other messages of the same conversation (received and sent), oldest first: same
    /// subject without "Re:", "Fwd:" and the like, and at least one person in common.
    /// Groups all messages by conversation subject, on another thread (it takes a moment with
    /// tens of thousands of messages); opening a message then finds its conversation at once.
    pub(super) fn start_topics(&mut self) {
        // (id, subject, sent by me, date, sender, recipients)
        let subjects: Vec<(u64, String, bool, NaiveDateTime, String, String)> = self
            .store
            .messages
            .iter()
            .filter(|x| !matches!(x.folder, Folder::Trash | Folder::Junk | Folder::Drafts | Folder::Outbox))
            .map(|x| {
                let sent = x.folder == Folder::Sent;
                (x.id, x.subject.clone(), sent, x.date, if sent { String::new() } else { x.from_addr.to_lowercase() }, if sent { format!("{},{}", x.to, x.cc).to_lowercase() } else { String::new() })
            })
            .collect();
        let sig = self.struct_sig;
        let (tx, rx) = channel();
        self.topics_rx = Some(rx);
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let t = std::time::Instant::now();
            let mut map: HashMap<String, Vec<u64>> = HashMap::new();
            let mut info: HashMap<u64, (bool, NaiveDateTime, String, String)> = HashMap::new();
            let mut fwd: std::collections::HashSet<u64> = std::collections::HashSet::new();
            for (id, subj, sent, date, from, rcpt) in subjects {
                if sent && is_forward_subject(&subj) {
                    fwd.insert(id);
                }
                let topic = thread_subject(&subj);
                if topic.len() >= 3 {
                    map.entry(topic).or_default().push(id);
                    info.insert(id, (sent, date, from, rcpt));
                }
            }
            map.retain(|_, v| v.len() > 1);
            // a message you forwarded: your "Fwd:" sent after it in the same conversation (the
            // latest message before the forward is the one that went on)
            let mut forwarded: HashMap<u64, u64> = HashMap::new();
            for ids in map.values() {
                for f in ids {
                    let Some((true, fdate, _, _)) = info.get(f) else { continue };
                    if !fwd.contains(f) {
                        continue;
                    }
                    let original = ids
                        .iter()
                        .filter(|i| *i != f && !fwd.contains(*i))
                        .filter_map(|i| info.get(i).map(|x| (*i, x.1)))
                        .filter(|(_, d)| d <= fdate)
                        .max_by_key(|(_, d)| *d)
                        .map(|(i, _)| i);
                    if let Some(o) = original {
                        forwarded.entry(o).or_insert(*f);
                    }
                }
            }
            // a received message is answered when you wrote to its sender later in the same conversation
            let mut answered: HashMap<u64, NaiveDateTime> = HashMap::new();
            for ids in map.values() {
                let replies: Vec<(&NaiveDateTime, &String)> = ids.iter().filter_map(|i| info.get(i)).filter(|x| x.0).map(|x| (&x.1, &x.3)).collect();
                if replies.is_empty() {
                    continue;
                }
                for i in ids {
                    let Some((false, date, from, _)) = info.get(i) else { continue };
                    if from.is_empty() {
                        continue;
                    }
                    if let Some(when) = replies.iter().filter(|(d, to)| *d > date && to.contains(from.as_str())).map(|(d, _)| **d).min() {
                        answered.insert(*i, when);
                    }
                }
            }
            crate::diag::took("conversations grouped", t, format!("{} conversations, {} answered messages", map.len(), answered.len()));
            let _ = tx.send((sig, map, answered, forwarded));
            ctx.request_repaint();
        });
    }

    /// The settings of an account (0: the main one).
    pub(super) fn account_ref(&self, a: u32) -> &crate::model::Account {
        if a == 0 {
            &self.store.account
        } else {
            self.store.extra.get(a as usize - 1).map(|x| &x.account).unwrap_or(&self.store.account)
        }
    }

    pub(super) fn account_for(&self, a: u32) -> crate::model::Account {
        self.account_ref(a).clone()
    }

    /// How far a folder of an account is synced.
    pub(super) fn sync_slot(&mut self, a: u32, f: Folder) -> &mut Option<net::SyncState> {
        if a == 0 {
            return match f {
                Folder::Sent => &mut self.store.sent_sync,
                Folder::Trash => &mut self.store.trash_sync,
                Folder::Junk => &mut self.store.junk_sync,
                _ => &mut self.store.sync,
            };
        }
        let x = &mut self.store.extra[a as usize - 1];
        match f {
            Folder::Sent => &mut x.sent_sync,
            Folder::Trash => &mut x.trash_sync,
            Folder::Junk => &mut x.junk_sync,
            _ => &mut x.sync,
        }
    }

    /// Starts receiving the mail of one account (inbox, then sent mail, the bin and spam).
    pub(super) fn start_receive(&mut self, a: u32) {
        let mut state = *self.sync_slot(a, Folder::Inbox);
        if let Some(s) = state.as_mut() {
            if s.oldest_uid == 0 {
                s.oldest_uid = self.store.messages.iter().filter(|m| m.folder == Folder::Inbox && m.account == a).filter_map(|m| m.uid).min().unwrap_or(0);
            }
        }
        let known: std::collections::HashSet<u32> = self.store.messages.iter().filter(|m| m.folder == Folder::Inbox && m.account == a).filter_map(|m| m.uid).collect();
        let others: Vec<(Folder, Option<net::SyncState>, std::collections::HashSet<u32>)> = [Folder::Sent, Folder::Trash, Folder::Junk]
            .into_iter()
            .map(|f| {
                let known: std::collections::HashSet<u32> = self.store.messages.iter().filter(|m| m.folder == f && m.account == a).filter_map(|m| m.uid).collect();
                let mut st = *self.sync_slot(a, f);
                if let Some(s) = st.as_mut() {
                    if s.oldest_uid == 0 {
                        s.oldest_uid = known.iter().copied().min().unwrap_or(0);
                    }
                }
                (f, st, known)
            })
            .collect();
        net::receive(self.account_for(a), a, state, known, others, self.tx.clone(), self.ctx.clone());
    }

    /// Remembers a change to make on the server too, for messages that came from the server's inbox.
    pub(super) fn queue_op(&mut self, id: u64, make: impl FnOnce(u32) -> net::Op) {
        let Some(m) = self.msg(id) else { return };
        let a = m.account;
        if !matches!(m.folder, Folder::Inbox | Folder::Sent | Folder::Trash | Folder::Junk) || !self.account_ref(a).can_connect() {
            return;
        }
        let folder = m.folder;
        if let Some(uid) = m.uid {
            let op = make(uid);
            // the change is made in the folder the message is in on the server
            self.pending_ops.push((a, if folder == Folder::Inbox { op } else { net::Op::In(folder, Box::new(op)) }));
        }
    }

    /// Sends queued changes to the server, one batch at a time.
    pub(super) fn flush_ops(&mut self) {
        if self.ops_running || self.pending_ops.is_empty() || std::env::var("POCZTA_OFFLINE").is_ok() {
            return;
        }
        // one account at a time (its own server and sign-in); the others go next
        let a = self.pending_ops[0].0;
        let (mine, rest): (Vec<(u32, net::Op)>, Vec<(u32, net::Op)>) = std::mem::take(&mut self.pending_ops).into_iter().partition(|(x, _)| *x == a);
        self.pending_ops = rest;
        self.ops_running = true;
        self.busy += 1;
        net::run_ops(self.account_for(a), mine.into_iter().map(|(_, o)| o).collect(), self.tx.clone(), self.ctx.clone());
    }

    pub(super) fn can_connect(&self) -> bool {
        self.store.account.can_connect()
    }

    pub(super) fn send_receive(&mut self) {
        if !self.quiet_sync {
            self.gcal_last = -1e9;
        }
        if !self.store.account.is_configured() {
            self.account_open = true;
            self.set_status("Najpierw skonfiguruj konto");
            return;
        }
        if !self.store.account.can_connect() {
            self.account_open = true;
            self.set_status(if self.store.account.use_oauth {
                "Zaloguj się przez Google, aby się połączyć"
            } else {
                "Wpisz hasło (dla Gmaila: hasło aplikacji), aby się połączyć"
            });
            return;
        }
        // waiting messages: their texts are read from disk before sending
        let waiting: Vec<u64> = self.store.messages.iter().filter(|m| m.folder == Folder::Outbox).map(|m| m.id).collect();
        for id in &waiting {
            self.ensure_body(*id);
        }
        let outbox: Vec<Message> = waiting.iter().filter_map(|id| self.msg(*id).cloned()).collect();
        for m in outbox {
            self.busy += 1;
            net::send(
                self.account_for(m.account),
                Outgoing {
                    id: m.id,
                    to: m.to,
                    cc: m.cc,
                    subject: m.subject,
                    body: m.body,
                    html: if m.html_path.is_empty() { None } else { std::fs::read_to_string(&m.html_path).ok() },
                    inline: m.inline,
                    files: m.attachment_files,
                },
                self.tx.clone(),
                self.ctx.clone(),
            );
        }
        let now = self.clock();
        if self.receiving {
            if now - self.last_sync < 180.0 {
                // A check is already running (perhaps the automatic one): from now on show it,
                // so clicking Odśwież always gives feedback.
                self.quiet_sync = false;
                return;
            }
            // No answer for 3 minutes: treat it as lost and start again.
        }
        self.receiving = true;
        self.last_sync = now;
        self.busy += 1;
        // the main account, then every further one that can sign in
        self.receiving_left = 0;
        for acct in 0..=self.store.extra.len() as u32 {
            if self.account_ref(acct).can_connect() {
                self.start_receive(acct);
                self.receiving_left += 1;
            }
        }
        // (each account answers on its own; the counter of what is running counts them)
        self.busy += self.receiving_left.saturating_sub(1);
    }

    /// Checks for new mail on its own every couple of minutes, without the "syncing" bubble.
    pub(super) fn auto_sync(&mut self) {
        if self.loading.is_some() || self.read_only || std::env::var("POCZTA_OFFLINE").is_ok() {
            return;
        }
        const EVERY: f64 = 120.0;
        if self.shot.is_some() || !self.store.account.can_connect() {
            return;
        }
        let now = self.clock();
        let due = self.last_sync < 0.0 || now - self.last_sync >= EVERY;
        if due && !self.receiving && !self.ops_running && self.pending_ops.is_empty() {
            self.quiet_sync = true;
            self.send_receive();
        }
        self.ctx.request_repaint_after(std::time::Duration::from_secs(15));
    }

    /// New mail at once: a connection waits on the server (IMAP IDLE) and the inbox is fetched
    /// as soon as the server reports a change (the regular check every 2 minutes stays as a backup).
    pub(super) fn watch_inbox(&mut self) {
        use std::sync::atomic::Ordering;
        let usable = self.loading.is_none()
            && !self.read_only
            && self.shot.is_none()
            && std::env::var("POCZTA_OFFLINE").is_err()
            && self.store.account.can_connect()
            && self.store.has_real_mail
            && !self.quitting;
        let a = &self.store.account;
        let who = format!("{}|{}|{}|{}", a.username, a.imap_host, a.imap_port, a.use_oauth);
        if !usable || self.watch.as_ref().is_some_and(|(w, ..)| *w != who) {
            if let Some((_, stop, _)) = self.watch.take() {
                stop.store(true, Ordering::Relaxed);
            }
        }
        if !usable {
            return;
        }
        if self.watch.is_none() {
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (tx, rx) = channel();
            net::watch_inbox(self.store.account.clone(), stop.clone(), tx, self.ctx.clone());
            self.watch = Some((who, stop, rx));
        }
        if let Some((_, _, rx)) = &self.watch {
            if rx.try_iter().count() > 0 {
                self.watch_due = true;
            }
        }
        if self.watch_due && !self.receiving && !self.ops_running && self.pending_ops.is_empty() {
            self.watch_due = false;
            crate::diag::log("new mail reported by the server: fetching");
            self.quiet_sync = true;
            self.send_receive();
        } else if self.watch_due {
            self.ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
    }

    pub(super) fn poll_topics(&mut self) {
        let Some(rx) = &self.topics_rx else { return };
        if let Ok((sig, map, answered, forwarded)) = rx.try_recv() {
            self.topics_rx = None;
            self.forwarded = Arc::new(forwarded);
            // replies found are kept even if the mailbox changed meanwhile (only better next time)
            self.answered = (self.answered.0.wrapping_add(1), Arc::new(answered));
            self.touched = true;
            if sig == self.struct_sig {
                self.topics = Some((sig, Arc::new(map)));
                *self.thread.borrow_mut() = None;
            } else {
                // the mailbox changed meanwhile: group again
                self.start_topics();
            }
        }
    }

    pub(super) fn poll_net(&mut self) {
        while let Ok((url, bytes)) = self.img_rx.try_recv() {
            if let Some(rest) = url.strip_prefix(LOGO_URL) {
                // A company logo: keep it only if it is a real picture of a useful size, and
                // remember the answer on disk (an empty file means "no logo").
                let domain = rest.split('&').next().unwrap_or("").to_string();
                let good = bytes.filter(|b| {
                    image::load_from_memory(b).map(|img| img.width() >= 32 && img.height() >= 32).unwrap_or(false)
                });
                let file = crate::model::cache_dir().join(format!("logo-{domain}.img"));
                let _ = std::fs::write(&file, good.as_deref().unwrap_or(&[]));
                self.images.insert(format!("logo:{domain}"), good.map(ImgState::Ready).unwrap_or(ImgState::Failed));
                continue;
            }
            self.images.insert(url, bytes.map(ImgState::Ready).unwrap_or(ImgState::Failed));
        }
        while let Ok(r) = self.rx.try_recv() {
            self.touched = true;
            self.busy = self.busy.saturating_sub(1);
            match r {
                NetResult::Received(sync) => {
                    // the inbox or the folder of sent mail
                    let f = sync.folder;
                    let acct = sync.acct;
                    let applied = std::time::Instant::now();
                    let batch_size = sync.new.len();
                    let last = sync.last;
                    // the first step brings new mail and recent changes; later steps older mail
                    let backfill = sync.flags.is_empty() && sync.window_from.is_none();
                    if last {
                        self.receiving_left = self.receiving_left.saturating_sub(1);
                        if self.receiving_left == 0 {
                            self.receiving = false;
                        }
                    }
                    let quiet = if last { std::mem::take(&mut self.quiet_sync) } else { self.quiet_sync };
                    if !self.store.has_real_mail {
                        self.store.messages.retain(|m| !m.sample);
                        self.store.has_real_mail = true;
                        self.selected = None;
                    }
                    // A full download after the server renumbered its inbox: old numbers mean nothing now.
                    if sync.full {
                        for m in self.store.messages.iter_mut().filter(|m| m.folder == f && m.account == acct) {
                            m.uid = None;
                        }
                    }
                    let changes_waiting = !self.pending_ops.is_empty() || self.ops_running;
                    // Read / flag / tag changes made elsewhere (e.g. on the phone), unless our own
                    // changes are still on their way to the server.
                    if !backfill && !changes_waiting {
                        for m in self.store.messages.iter_mut().filter(|m| m.folder == f && m.account == acct) {
                            let Some(u) = m.uid else { continue };
                            if let Some((seen, flagged)) = sync.flags.get(&u) {
                                m.read = *seen;
                                m.flagged = *flagged;
                            }
                            if !sync.labels.is_empty() && sync.flags.contains_key(&u) {
                                m.labels = sync.labels.get(&u).cloned().unwrap_or_default();
                            }
                        }
                        // Gone from the server's inbox (deleted or archived elsewhere): remove here too.
                        if let Some(from) = sync.window_from {
                            let before = self.store.messages.len();
                            self.store.messages.retain(|m| {
                                !(m.folder == f && m.account == acct && m.uid.is_some_and(|u| u >= from && !sync.flags.contains_key(&u)))
                            });
                            if self.store.messages.len() != before && self.selected.is_some_and(|id| self.msg(id).is_none()) {
                                self.selected = None;
                            }
                        }
                    }
                    if acct as usize <= self.store.extra.len() {
                        *self.sync_slot(acct, f) = Some(sync.state);
                    }
                    let mut slots = vec![self.store.sync, self.store.sent_sync, self.store.trash_sync, self.store.junk_sync];
                    for x in &self.store.extra {
                        slots.extend([x.sync, x.sent_sync, x.trash_sync, x.junk_sync]);
                    }
                    let unfinished = slots.iter().any(|s| s.is_some_and(|s| !s.complete));
                    if last && unfinished {
                        // the sync stopped after its time limit: carry on in a few seconds
                        self.last_sync = self.clock() - 120.0 + 3.0;
                    }
                    // Finding messages already here: by server number, or by subject + sender + date.
                    // Looked up in a table instead of going through the whole mailbox each time.
                    let fp = |subject: &str, from: &str, date: &NaiveDateTime| {
                        use std::hash::{Hash, Hasher};
                        let mut h = std::collections::hash_map::DefaultHasher::new();
                        (subject, from, date).hash(&mut h);
                        h.finish()
                    };
                    let mut by_uid: HashMap<u32, usize> = HashMap::new();
                    let mut by_key: HashMap<u64, usize> = HashMap::new();
                    for (i, x) in self.store.messages.iter().enumerate() {
                        if x.folder == f && x.account == acct {
                            if let Some(u) = x.uid {
                                by_uid.insert(u, i);
                            }
                        }
                        // sent mail is matched with sent mail, the bin and spam with themselves,
                        // the inbox with everything received (also what is on its way to the bin)
                        let same = x.account == acct && match f {
                            Folder::Sent => x.folder == Folder::Sent,
                            Folder::Trash | Folder::Junk => x.folder == f,
                            _ => x.folder != Folder::Sent,
                        };
                        if same {
                            by_key.insert(fp(&x.subject, &x.from_addr, &x.date), i);
                        }
                    }
                    let mut added = 0;
                    // new unread mail in the inbox (not the first download, not older mail coming in)
                    let tell = f == Folder::Inbox && !backfill && !by_uid.is_empty();
                    let recent = Local::now().naive_local() - Duration::days(3);
                    let mut fresh: Vec<u64> = vec![];
                    for mut m in sync.new {
                        m.folder = f;
                        m.account = acct;
                        let key = fp(&m.subject, &m.from_addr, &m.date);
                        let mut found = m.uid.and_then(|u| by_uid.get(&u).copied()).or_else(|| by_key.get(&key).copied());
                        if found.is_none() && f == Folder::Sent {
                            // sent from this app: the copy here has no server number yet and a
                            // time a moment different from the server's; same subject, a few minutes apart
                            found = self.store.messages.iter().position(|x| {
                                x.folder == Folder::Sent
                                    && x.account == acct
                                    && x.uid.is_none()
                                    && x.subject.trim() == m.subject.trim()
                                    && (x.date - m.date).num_minutes().abs() <= 15
                            });
                        }
                        if let Some(i) = found {
                            let old = &mut self.store.messages[i];
                            old.labels = m.labels.clone();
                            if old.folder == f {
                                old.uid = m.uid;
                            }
                            if old.partial && !m.partial {
                                // the full version of a message that only had its header
                                old.body = m.body;
                        old.on_disk = false;
                                old.on_disk = false;
                                old.html_path = m.html_path;
                                old.inline = m.inline;
                                old.attachments = m.attachments;
                                old.attachment_files = m.attachment_files;
                                old.partial = false;
                                continue;
                            }
                            if old.attachment_files.len() < m.attachment_files.len() {
                                old.attachments = m.attachments.clone();
                                old.attachment_files = m.attachment_files.clone();
                            }
                            if old.html_path.is_empty() && !m.html_path.is_empty() {
                                let oid = old.id;
                                old.html_path = m.html_path;
                                old.inline = m.inline;
                                self.mail_view.reload();
                                if self.html_cache.as_ref().map(|c| c.0) == Some(oid) {
                                    self.html_cache = None;
                                }
                            }
                        } else {
                            m.id = self.store.new_id();
                            let i = self.store.messages.len();
                            if let Some(u) = m.uid {
                                by_uid.insert(u, i);
                            }
                            by_key.insert(key, i);
                            if tell && !m.read && m.date >= recent {
                                fresh.push(m.id);
                            }
                            self.store.messages.push(m);
                            added += 1;
                        }
                    }
                    if !fresh.is_empty() {
                        self.new_mail(fresh);
                    }
                    // older mail arrives quietly (the note in the left bar shows the progress)
                    if !backfill && f == Folder::Inbox {
                        if added > 0 {
                            self.set_status(match added {
                                1 => "1 nowa wiadomość".to_string(),
                                2..=4 => format!("{added} nowe wiadomości"),
                                _ => format!("{added} nowych wiadomości"),
                            });
                        } else if !quiet {
                            self.set_status("Wszystko aktualne");
                        }
                    }
                    if last || !backfill {
                        self.save_now = true;
                    }
                    crate::diag::took("app: add to mailbox", applied, format!("{batch_size} messages, {added} new, {} in total", self.store.messages.len()));
                    if self.selected.is_none() {
                        self.selected = self.visible_ids().first().copied();
                    }
                }
                NetResult::Full { acct, uid, folder, message } => {
                    self.fetching_full.remove(&(folder, uid));
                    let m = *message;
                    if let Some(old) = self.store.messages.iter_mut().find(|x| x.folder == folder && x.account == acct && x.uid == Some(uid)) {
                        let oid = old.id;
                        old.body = m.body;
                        old.html_path = m.html_path;
                        old.inline = m.inline;
                        old.attachments = m.attachments;
                        old.attachment_files = m.attachment_files;
                        old.partial = false;
                        self.mail_view.reload();
                        if self.html_cache.as_ref().map(|c| c.0) == Some(oid) {
                            self.html_cache = None;
                        }
                        self.save_now = true;
                        if let Some(m) = self.msg(oid) {
                            self.hub.update(m);
                        }
                    }
                }
                NetResult::Sent { id } => {
                    if let Some(m) = self.msg_mut(id) {
                        m.folder = Folder::Sent;
                        m.date = Local::now().naive_local();
                    }
                    self.set_status("Wiadomość wysłana");
                }
                NetResult::Synced => {
                    self.ops_running = false;
                }
                NetResult::Failed { what, error, id } => {
                    if what == "Synchronizacja" {
                        self.ops_running = false;
                    }
                    if what == "Receiving mail" {
                        self.receiving = false;
                        // an automatic check that fails (e.g. no internet) doesn't need a warning
                        if std::mem::take(&mut self.quiet_sync) {
                            continue;
                        }
                    }
                    let what = if what.starts_with("Sending") {
                        "Wysyłanie"
                    } else if what == "Synchronizacja" {
                        "Zapisywanie zmian na serwerze"
                    } else {
                        "Odbieranie"
                    };
                    let mut s = format!("{what} nie powiodło się: {error}");
                    if id.is_some() {
                        s.push_str(" (zostaje w skrzynce nadawczej)");
                    }
                    self.set_status(s);
                }
                NetResult::SignedIn(r) => {
                    self.save_now = true;
                    self.signing_in = false;
                    let a = &mut self.store.account;
                    a.oauth_refresh_token = r.refresh_token;
                    a.oauth_scopes = r.scopes;
                    // calendar and contacts read at once with the new permissions
                    self.gcal_last = -1e9;
                    self.book_last = -1e9;
                    self.book_said = None;
                    self.gcal_said = None;
                    if !r.email.is_empty() {
                        a.email = r.email.clone();
                        a.username = r.email;
                    }
                    if !r.name.is_empty() && (a.display_name.is_empty() || a.display_name == "Aleks Nowicki") {
                        a.display_name = r.name;
                    }
                    let who = a.email.clone();
                    self.sign_in_error = None;
                    self.set_status(format!("Zalogowano przez Google jako {who}"));
                    // Fetch mail straight away.
                    self.send_receive();
                }
                NetResult::SignInFailed(e) => {
                    self.signing_in = false;
                    self.sign_in_error = Some(e.clone());
                    self.set_status(format!("Logowanie przez Google nie powiodło się: {e}"));
                }
            }
        }
    }

    // ------------------------------------------------------------ top bar (one line)

    /// Takes in what the drawing thread finished: email pictures and link answers.
    pub(super) fn poll_email(&mut self) {
        let Some(engine) = &self.email_engine else { return };
        let mut drawn = vec![];
        let mut links = vec![];
        while let Ok(o) = engine.out.try_recv() {
            match o {
                crate::htmlview::Out::Drawn(d) => drawn.push(d),
                crate::htmlview::Out::Link { key, href, click } => links.push((key, href, click)),
            }
        }
        for d in drawn.into_iter().filter(|d| d.key == self.email_asked.0) {
            let strip_pts = |top: u32| Vec2::new(d.w as f32 / d.scale, (d.total - top).min(crate::htmlview::STRIP_PX) as f32 / d.scale);
            let count = d.total.div_ceil(crate::htmlview::STRIP_PX) as usize;
            // a new email (or a new height once pictures came in): the strips laid out afresh,
            // the ones already there stay until drawn again
            let fresh = self.email_pic.as_ref().is_none_or(|p| p.key != d.key);
            if fresh || self.email_pic.as_ref().is_some_and(|p| p.strips.len() != count || p.strips.first().is_some_and(|s| s.1.x != d.w as f32 / d.scale)) {
                let old: Vec<Option<egui::TextureHandle>> = if fresh { vec![] } else { self.email_pic.take().map(|p| p.strips.into_iter().map(|s| s.0).collect()).unwrap_or_default() };
                let strips = (0..count).map(|i| (old.get(i).cloned().flatten(), strip_pts(i as u32 * crate::htmlview::STRIP_PX))).collect();
                self.email_pic = Some(EmailPic { key: d.key, id: self.email_asked.1, strips, done: false });
            }
            let img = egui::ColorImage::from_rgba_premultiplied([d.w as usize, d.h as usize], &d.rgba);
            let tex = self.ctx.load_texture(format!("email-{}-{}", d.key, d.top), img, egui::TextureOptions::LINEAR);
            if let Some(p) = self.email_pic.as_mut() {
                let i = (d.top / crate::htmlview::STRIP_PX) as usize;
                if let Some(slot) = p.strips.get_mut(i) {
                    *slot = (Some(tex), strip_pts(d.top));
                }
                if d.top == 0 || d.done {
                    crate::diag::log(format!("email drawn{}: {} ms, {}x{} px", if d.top == 0 { " (first part)" } else { "" }, d.took_ms, d.w, d.total));
                }
                p.done = d.done;
            }
        }
        for (key, href, click) in links {
            if key != self.email_asked.0 {
                continue;
            }
            if click {
                if let Some(h) = href {
                    self.open_link(&h);
                }
            } else {
                self.email_hover = (key, href, self.clock());
            }
        }
    }
}

/// After reading the mailbox: the saved password back in memory, and message texts that older
/// versions kept in separate files.
pub(super) fn prepare_store(store: &mut Store) {
    if store.account.password.is_empty() {
        if let Some(p) = crate::secret::unprotect(&store.account.password_saved) {
            store.account.password = p;
        }
    }
    for x in store.extra.iter_mut() {
        if x.account.password.is_empty() {
            if let Some(p) = crate::secret::unprotect(&x.account.password_saved) {
                x.account.password = p;
            }
        }
    }
    for m in store.messages.iter_mut() {
        if m.body.is_empty() && !m.body_path.is_empty() {
            m.body = std::fs::read_to_string(&m.body_path).unwrap_or_default();
        }
    }
}

/// The mailbox being read from disk: first its newest messages, then the database and all
/// that was in it.
pub enum Loaded {
    Newest(Store, crate::db::TextReader),
    All(Option<crate::db::Db>, Option<Store>, Option<Vec<crate::search::Entry>>),
}
pub type Loading = Receiver<Loaded>;

/// How many of the newest messages are shown while the rest is read.
const NEWEST: usize = 500;

/// Starts reading the mailbox in the background. Called as early as possible (before the
/// window is even made), so the mail is ready about when the window is.
pub fn start_loading() -> Loading {
    let (ltx, lrx) = channel();
    std::thread::spawn(move || {
        let t = std::time::Instant::now();
        let mut db = crate::db::Db::open().map_err(|e| crate::diag::log(format!("database error: {e}"))).ok();
        crate::diag::took("  database: opened", t, "");
        if let Some((s, reader)) = db.as_ref().and_then(|d| Some((d.newest(NEWEST)?, d.reader()))) {
            crate::diag::took("  database: newest messages read", t, "");
            let _ = ltx.send(Loaded::Newest(s, reader));
        }
        let mut store = db.as_mut().and_then(|d| d.load());
        if let Some(s) = store.as_mut() {
            prepare_store(s);
        }
        crate::diag::took("startup: read the mailbox", t, format!("{} messages", store.as_ref().map(|s| s.messages.len()).unwrap_or(0)));
        crate::diag::mem("mailbox read, before the copy for search");
        // the search index needs its own copy of the texts: made here, off the window's thread
        let t = std::time::Instant::now();
        let entries = store.as_ref().map(|s| crate::finder::entries(&s.messages));
        crate::diag::took("startup: texts for search copied", t, "");
        crate::diag::mem("after reading the mailbox");
        let _ = ltx.send(Loaded::All(db, store, entries));
    });
    lrx
}
