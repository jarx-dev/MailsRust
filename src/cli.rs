//! Command-line checks and tools (`--sync-check`, the `--…-selftest`s, `--save-icon`…): each one
//! runs instead of the window and then the program ends.
use crate::*;

/// Runs the command-line check named in `args`; `true` when one ran (no window is opened then).
pub fn run(args: &[String]) -> bool {
    if args.iter().any(|a| a == "--chat-check") {
        // A live check of the chat: a browser sign-in adding the chat scopes (the token stays in
        // memory), one round, then the first conversation opened. Sends nothing, marks nothing
        // read, touches no mail, and logs counts only. The chat cache (chat.redb) is written.
        let Some(mut store) = db::Db::open().ok().and_then(|mut d| d.load()) else {
            println!("no saved mailbox");
            return true;
        };
        let a = &mut store.account;
        let scopes = format!("{} {}", oauth::SCOPES, chat::SCOPES);
        match oauth::login_scopes(&a.oauth_client_id, &a.oauth_client_secret, &scopes) {
            Ok(r) if r.email.eq_ignore_ascii_case(&a.email) => {
                diag::log(format!("chat check: signed in, Workspace domain {:?}, chat allowed: {}", r.hd, r.scopes.contains(chat::SCOPE_READ)));
                a.oauth_refresh_token.clone_from(&r.refresh_token);
                a.oauth_scopes = r.scopes;
                if !r.sub.is_empty() {
                    a.google_hd = Some(r.hd);
                    a.google_sub = r.sub;
                }
            }
            Ok(r) => {
                diag::log(format!("chat check: another Google account was picked ({})", r.email));
                return true;
            }
            Err(e) => {
                diag::log(format!("chat check: sign-in failed: {e}"));
                return true;
            }
        }
        let (tx, rx) = chat::start(store.account.clone(), eframe::egui::Context::default());
        let _ = tx.send(chat::Cmd::Pace(chat::Pace::Background));
        // the list comes before the round's end: the newest one is kept while waiting
        let mut rows: Vec<chat::Row> = vec![];
        let wait = |what: &str, done: &dyn Fn(&chat::Update) -> bool, rows: &mut Vec<chat::Row>| -> Option<chat::Update> {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(180);
            while let Ok(u) = rx.recv_timeout(until.saturating_duration_since(std::time::Instant::now())) {
                if done(&u) {
                    return Some(u);
                }
                if let chat::Update::Rows(r) = u {
                    *rows = r;
                }
            }
            diag::log(format!("chat check: no {what} in time"));
            None
        };
        if let Some(chat::Update::Done(r)) = wait("round", &|u| matches!(u, chat::Update::Done(_)), &mut rows) {
            diag::log(format!("chat check: round {}", r.map(|_| "ok".to_string()).unwrap_or_else(|e| e)));
        }
        let first = rows.first().map(|r| r.name.clone());
        let named = rows.iter().filter(|r| r.title != "Rozmowa").count();
        diag::log(format!(
            "chat check: {} conversations ({named} named, {} with people from outside), {} unread",
            rows.len(),
            rows.iter().filter(|r| r.external).count(),
            rows.iter().map(|r| r.unread).sum::<usize>()
        ));
        if let Some(first) = first {
            let t = std::time::Instant::now();
            let _ = tx.send(chat::Cmd::Open(first));
            if let Some(chat::Update::Messages(_, m)) = wait("messages", &|u| matches!(u, chat::Update::Messages(..)), &mut rows) {
                diag::took("chat check: newest conversation opened", t, format!("{} messages", m.len()));
            }
        }
        drop(tx);
        chat::close();
        return true;
    }
    if args.iter().any(|a| a == "--sync-benchmark") {
        // Read-only speed test with the account saved in the app (see net::benchmark).
        let store = db::Db::open().ok().and_then(|mut d| d.load()).or_else(|| {
            // not moved to the database yet: read the older settings file
            let path = std::path::Path::new(&std::env::var("APPDATA").unwrap_or_default()).join("Mailroom").join("data").join("app.ron");
            let text = std::fs::read_to_string(path).ok()?;
            let map: std::collections::HashMap<String, String> = ron::from_str(&text).ok()?;
            ron::from_str::<model::Store>(map.get("store")?).ok()
        });
        let Some(mut store) = store else {
            println!("no saved account found");
            return true;
        };
        if store.account.password.is_empty() {
            if let Some(p) = secret::unprotect(&store.account.password_saved) {
                store.account.password = p;
            }
        }
        if !store.account.can_connect() {
            println!("the saved account can't connect (no password or Google sign-in)");
            return true;
        }
        if let Some(u) = args.iter().position(|a| a == "--peek").and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()) {
            println!("{:?}", net::peek_header(&store.account, u));
            return true;
        }
        let n = args.iter().position(|a| a == "--sync-benchmark").and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(200);
        if let Err(e) = net::benchmark(&store.account, n) {
            diag::log(format!("speed test failed: {e}"));
        }
        for l in diag::lines() {
            println!("{l}");
        }
        return true;
    }
    if args.iter().any(|a| a == "--sync-check") {
        // One normal incremental sync against the saved mailbox, without saving anything:
        // reports what would be added and how many of those are unread.
        let Some(mut store) = db::Db::open().ok().and_then(|mut d| d.load()) else {
            println!("no saved mailbox");
            return true;
        };
        if let Some(p) = secret::unprotect(&store.account.password_saved) {
            store.account.password = p;
        }
        let known: std::collections::HashSet<u32> =
            store.messages.iter().filter(|m| m.folder == model::Folder::Inbox).filter_map(|m| m.uid).collect();
        let sent_known: std::collections::HashSet<u32> =
            store.messages.iter().filter(|m| m.folder == model::Folder::Sent).filter_map(|m| m.uid).collect();
        // SYNC_ROUNDS=2: a second round right after, as the app's later checks run
        let rounds: u32 = std::env::var("SYNC_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
        for round in 1..=rounds {
            diag::log(format!("== sync-check round {round}"));
            let (tx, rx) = std::sync::mpsc::channel();
            net::receive(
                store.account.clone(),
                0,
                store.sync,
                known.clone(),
                vec![
                    (model::Folder::Sent, store.sent_sync, sent_known.clone()),
                    (model::Folder::Trash, store.trash_sync, Default::default()),
                    (model::Folder::Junk, store.junk_sync, Default::default()),
                ],
                tx,
                eframe::egui::Context::default(),
            );
            while let Ok(r) = rx.recv() {
                match r {
                    net::NetResult::Received(s) => {
                        let here = if s.folder == model::Folder::Sent { &sent_known } else { &known };
                        let fresh: Vec<&model::Message> = s.new.iter().filter(|m| m.uid.is_some_and(|u| !here.contains(&u))).collect();
                        println!(
                            "{:?} batch: {} messages, {} not here yet ({} unread: {:?}), last={}",
                            s.folder,
                            s.new.len(),
                            fresh.len(),
                            fresh.iter().filter(|m| !m.read).count(),
                            fresh.iter().filter(|m| !m.read).map(|m| m.uid).collect::<Vec<_>>(),
                            s.last
                        );
                        if s.last {
                            break;
                        }
                    }
                    net::NetResult::Failed { error, .. } => {
                        println!("failed: {error}");
                        break;
                    }
                    _ => {}
                }
            }
        }
        return true;
    }
    if args.iter().any(|a| a == "--db-timing") {
        // How long each part of reading the mailbox takes (no contents shown).
        let t = std::time::Instant::now();
        let mut d = db::Db::open().expect("open");
        println!("open: {:?}", t.elapsed());
        let t = std::time::Instant::now();
        let s = d.load().expect("load");
        println!("full read: {:?}, {} messages", t.elapsed(), s.messages.len());
        let t = std::time::Instant::now();
        let mut s2 = s.clone();
        println!("copy in memory: {:?}", t.elapsed());
        let t = std::time::Instant::now();
        d.save(&mut s2, true, None).expect("save");
        println!("save with nothing changed: {:?}", t.elapsed());
        let body: usize = s.messages.iter().map(|m| m.body.len()).sum();
        println!("texts: {} MB", body / 1_048_576);
        return true;
    }
    if let Some(uid) = args.iter().position(|a| a == "--eml-check").and_then(|i| args.get(i + 1)).and_then(|v| v.parse::<u32>().ok()) {
        // Read-only: fetch one inbox message's original .eml and check it reads back (no contents shown).
        let Some(mut store) = db::Db::open().ok().and_then(|mut d| d.load()) else {
            println!("no saved mailbox");
            return true;
        };
        if let Some(p) = secret::unprotect(&store.account.password_saved) {
            store.account.password = p;
        }
        let t = std::time::Instant::now();
        match net::fetch_raw(&store.account, uid) {
            Ok(raw) => {
                let p = mail_parser::MessageParser::default().parse(&raw[..]);
                println!(
                    "original .eml: {} KB in {:?}; reads back: {}, parts with files: {}",
                    raw.len() / 1024,
                    t.elapsed(),
                    p.is_some(),
                    p.as_ref().map(|p| p.attachments().count()).unwrap_or(0)
                );
            }
            Err(e) => println!("failed: {e}"),
        }
        return true;
    }
    if args.iter().any(|a| a == "--suggest-selftest") {
        // The recipients box with its suggestion list, driven by simulated mouse and keys.
        use eframe::egui;
        let sample = model::Store::sample();
        let people = contacts::collect(&sample.messages, &sample.account.email);
        let ctx = egui::Context::default();
        let mut text = String::from("ann");
        let list_rect: std::cell::Cell<Option<egui::Rect>> = std::cell::Cell::new(None);
        let edit_id = std::cell::Cell::new(egui::Id::NULL);
        let frame = |events: Vec<egui::Event>, text: &mut String, focus: bool| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                let r = contacts::recipients_field(ui, text, "", 400.0, &people, ("to", 1));
                edit_id.set(r.id);
                if focus {
                    r.request_focus();
                }
            });
            out.textures_delta.clear();
            // where the list is drawn (the popup area)
            #[allow(deprecated)]
            let list_id = egui::Id::new(("recipients", ("to", 1))).with("list");
            list_rect.set(ctx.memory(|m| m.area_rect(list_id)));
        };
        frame(vec![], &mut text, true);
        frame(vec![], &mut text, false);
        frame(vec![], &mut text, false);
        let Some(r) = list_rect.get() else {
            println!("no suggestion list shown");
            return true;
        };
        let p = r.left_top() + egui::vec2(40.0, 20.0);
        let press = |pressed| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        frame(vec![egui::Event::PointerMoved(p)], &mut text, false);
        frame(vec![press(true)], &mut text, false);
        frame(vec![press(false)], &mut text, false);
        frame(vec![], &mut text, false);
        println!("after clicking the first suggestion: {text:?}");
        return true;
    }
    if args.iter().any(|a| a == "--size-report") {
        // How much each part of the message details takes (in memory, roughly).
        let Some(store) = db::Db::open().ok().and_then(|mut d| d.load()) else { return true };
        let n = store.messages.len();
        let mut f: Vec<(&str, usize, usize)> = vec![];
        let mut add = |name: &'static str, bytes: usize, allocs: usize| {
            match f.iter_mut().find(|x| x.0 == name) {
                Some(x) => {
                    x.1 += bytes;
                    x.2 += allocs;
                }
                None => f.push((name, bytes, allocs)),
            }
        };
        for m in &store.messages {
            for (name, s) in [("from_name", &m.from_name), ("from_addr", &m.from_addr), ("to", &m.to), ("cc", &m.cc), ("subject", &m.subject), ("html_path", &m.html_path), ("body_path", &m.body_path), ("snippet", &m.snippet)] {
                add(name, s.len(), (!s.is_empty()) as usize);
            }
            add("attachments", m.attachments.iter().map(|x| x.len()).sum(), m.attachments.len() + (!m.attachments.is_empty()) as usize);
            add("attachment_files", m.attachment_files.iter().map(|x| x.len()).sum(), m.attachment_files.len() + (!m.attachment_files.is_empty()) as usize);
            add("inline", m.inline.iter().map(|x| x.0.len() + x.1.len()).sum(), m.inline.len() * 2 + (!m.inline.is_empty()) as usize);
            add("labels", m.labels.iter().map(|x| x.len()).sum(), m.labels.len() + (!m.labels.is_empty()) as usize);
        }
        println!("{n} messages; message itself: {} bytes each ({} MB)", std::mem::size_of::<model::Message>(), n * std::mem::size_of::<model::Message>() / 1_048_576);
        f.sort_by(|a, b| b.1.cmp(&a.1));
        for (name, bytes, allocs) in f {
            println!("{name:>18}: {:>6.1} MB text, {allocs} pieces (+{:.1} MB bookkeeping)", bytes as f64 / 1_048_576.0, allocs as f64 * 32.0 / 1_048_576.0);
        }
        return true;
    }
    if args.iter().any(|a| a == "--tokenizer-selftest") {
        // The compact word-piece splitter against the library one, on real messages.
        let dir = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join("Poczta").join("model");
        let m0 = diag::mem_mb();
        let t = std::time::Instant::now();
        let mine = unigram::Unigram::from_json(&dir.join("tokenizer.json")).expect("vocab");
        println!("compact: {} pieces in {:?}, memory +{} MB", mine.len(), t.elapsed(), diag::mem_mb().saturating_sub(m0));
        let _ = mine.save(&dir.join("vocab-test.bin"));
        let t = std::time::Instant::now();
        let mine2 = unigram::Unigram::load(&dir.join("vocab-test.bin")).expect("load");
        println!("compact file loads in {:?}", t.elapsed());
        let _ = std::fs::remove_file(dir.join("vocab-test.bin"));
        let store = db::Db::open().ok().and_then(|mut d| d.load()).unwrap_or_else(model::Store::sample);
        let t = std::time::Instant::now();
        let n: usize = store.messages.iter().take(500).map(|m| mine2.encode(&format!("{}
{}", m.subject, m.body)).len()).sum();
        println!("500 messages split into {n} pieces in {:?}", t.elapsed());
        return true;
    }
    if args.iter().any(|a| a == "--idle-check") {
        // read-only: the mailbox's account (run on a copy, POCZTA_DATA_DIR), a wait on its inbox
        let Some(mut store) = db::Db::open().ok().and_then(|mut d| d.load()) else { println!("no mailbox"); return true; };
        if let Some(p) = secret::unprotect(&store.account.password_saved) {
            store.account.password = p;
        }
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let t = std::time::Instant::now();
        net::watch_inbox(store.account.clone(), stop.clone(), tx, eframe::egui::Context::default());
        let secs: u64 = std::env::var("IDLE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(25);
        while t.elapsed().as_secs() < secs {
            if rx.recv_timeout(std::time::Duration::from_millis(500)).is_ok() {
                println!("signal from the server after {:.1} s", t.elapsed().as_secs_f32());
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for l in diag::lines().iter().filter(|l| l.contains("watching")) {
            println!("{l}");
        }
        return true;
    }
    if let Some(pos) = args.iter().position(|a| a == "--google-check") {
        // read-only: asks the person's Google script for the calendar and the contacts
        let (enc, key) = (args.get(pos + 1).cloned().unwrap_or_default(), args.get(pos + 2).cloned().unwrap_or_default());
        let text = secret::unprotect(&enc).unwrap_or_default();
        let Some(url) = gcal::script_url(&text) else {
            println!("no script address saved");
            return true;
        };
        let (cal, people) = gcal::check(&url, &key);
        println!("calendar: {cal:?}");
        println!("contacts: {people:?}");
        return true;
    }
    if args.iter().any(|a| a == "--gcal-selftest") {
        // a calendar file as Google writes it, read for November 2026
        let from = chrono::NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        let to = chrono::NaiveDate::from_ymd_opt(2026, 11, 30).unwrap();
        let mut evs = gcal::parse(gcal::SAMPLE, 1, from, to);
        evs.sort_by_key(|e| (e.date, e.start));
        for e in &evs {
            println!("{} {} {:>4} min {}{} {}", e.date, e.start.format("%H:%M"), e.minutes, if e.all_day { "[cały dzień] " } else { "" }, e.title, e.location);
        }
        let n = |t: &str| evs.iter().filter(|e| e.title == t).count();
        println!("weekday meetings in November (21 workdays, 1 skipped, 1 moved): {}", n("Spotkanie IT"));
        println!("moved one on 6.11: {}", evs.iter().any(|e| e.title.contains("przeniesione") && e.start.format("%H:%M").to_string() == "11:30"));
        println!("last Friday report: {:?}", evs.iter().find(|e| e.title == "Raport miesięczny").map(|e| e.date));
        println!("cancelled left out: {}", n("Odwołane") == 0);
        if let Some(dir) = args.iter().position(|a| a == "--write").and_then(|i| args.get(i + 1)) {
            let _ = std::fs::write(dir, gcal::SAMPLE);
        }
        // the script's answer as it comes from Google
        let ms = |y: i32, mo: u32, d: u32, h: u32, mi: u32| {
            use chrono::TimeZone;
            chrono::Local.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap().timestamp_millis()
        };
        let answer = serde_json::json!({"events": [
            {"id": "abc@google.com", "cal": 0, "title": "Wdrożenie u klienta", "location": "Kraków", "allDay": false, "start": ms(2026, 11, 4, 10, 0), "end": ms(2026, 11, 4, 12, 30), "editable": true},
            {"id": "rep@google.com", "cal": 1, "title": "Urlop", "location": "", "allDay": true, "start": ms(2026, 11, 12, 0, 0), "end": ms(2026, 11, 14, 0, 0), "editable": false}
        ], "calendars": ["Jarek", "Urlopy"]});
        for e in gcal::parse_script(&answer, 0) {
            println!("script: {} {} {} min {} editable {}", e.date, e.start.format("%H:%M"), e.minutes, e.title, !e.gid.is_empty());
        }
        if let Some(path) = args.iter().position(|a| a == "--script").and_then(|i| args.get(i + 1)) {
            let _ = std::fs::write(path, gcal::script_text("testowy-klucz"));
        }
        return true;
    }
    if args.iter().any(|a| a == "--ai-modes") {
        // every model and thinking level once, through CLIProxy: accepted, and how long it takes
        let base = ai::load();
        let texts = vec!["Hello Jarek, thank you for your order.".to_string(), "Best regards".to_string()];
        for model in ["claude-haiku-5-5", "claude-sonnet-5-5"] {
            for effort in ai::EFFORTS {
                let mut s = base.clone();
                s.translate_model = model.into();
                s.translate_effort = effort.into();
                let t = std::time::Instant::now();
                let r = ai::translate_texts(&s, &texts, true);
                println!("{model} {effort}: {:.1} s {:?}", t.elapsed().as_secs_f32(), r);
            }
        }
        return true;
    }
    if let Some(file) = args.iter().position(|a| a == "--translate-file").and_then(|i| args.get(i + 1)) {
        // timing: an email page (HTML) translated as the app does it
        let html = std::fs::read_to_string(file).unwrap_or_default();
        println!("{} pieces, {} characters of text", translate::pieces(&html).len(), translate::pieces(&html).iter().map(|(a, b)| b - a).sum::<usize>());
        if std::env::var("SHOW_PIECES").is_ok() {
            for (a, b) in translate::pieces(&html) {
                println!("  {:?}", &html[a..b]);
            }
            return true;
        }
        let t = std::time::Instant::now();
        match translate::translate_email(&ai::load(), Some(html), "", true) {
            Ok(path) => println!("done in {:.1} s -> {path}", t.elapsed().as_secs_f32()),
            Err(e) => println!("error after {:.1} s: {e}", t.elapsed().as_secs_f32()),
        }
        for l in diag::lines() {
            println!("{l}");
        }
        return true;
    }
    if args.iter().any(|a| a == "--translate-selftest") {
        let html = "<html><head><style>p{color:red}</style><title>X</title></head><body><table style=\"background:#eef\"><tr><td><h2>Hello Jarek,</h2><p>Thank you for your order <b>#12345</b>. It will ship on <a href=\"https://example.com/track\">Monday</a>.</p><p>Best regards,<br>Natalia Kalka</p></td></tr></table></body></html>";
        let pieces = translate::pieces(html);
        println!("pieces: {:?}", pieces.iter().map(|(a, b)| &html[*a..*b]).collect::<Vec<_>>());
        match translate::translate_email(&ai::load(), Some(html.to_string()), "", true) {
            Ok(path) => println!("{}", std::fs::read_to_string(path).unwrap_or_default()),
            Err(e) => println!("error: {e}"),
        }
        return true;
    }
    if args.iter().any(|a| a == "--ai-set-key") {
        // the CLIProxy key read from the input (never shown), saved encrypted
        let mut key = String::new();
        let _ = std::io::stdin().read_line(&mut key);
        let mut s = ai::load();
        s.key = key.trim().to_string();
        ai::save(&s);
        println!("key saved: {}", !ai::load().key.is_empty());
        return true;
    }
    if args.iter().any(|a| a == "--ai-selftest") {
        // a real correction through CLIProxy: a text with mistakes, a picture mark, each style
        let s = ai::load();
        println!("proxy {} model {}", s.url, s.model);
        match ai::test(&s) {
            Ok(m) => println!("check: {m}"),
            Err(e) => println!("check failed: {e}"),
        }
        let html = "<div>Dzien dobry,</div><div>prosze zrestartowac komputer i sprawdzic czy drukarka działa. Dedykowany sterownik jest w załączniku.</div><div>[[obrazek-1]]</div><div><br></div>";
        let styles: Vec<&str> = if args.iter().any(|a| a == "--all") { ai::STYLES.iter().map(|s| s.0).collect() } else { vec!["correct"] };
        for st in styles {
            match ai::rewrite(&s, st, html) {
                Ok(h) => println!("{st}: OK, picture kept {}\n{h}\n", h.contains("[[obrazek-1]]")),
                Err(e) => println!("{st}: ERROR {e}"),
            }
        }
        for (what, reply, ok) in [
            ("comment around", "Oto wynik:\n<wynik><div>Tekst [[obrazek-1]]</div></wynik>\nGotowe.", true),
            ("picture lost", "<wynik><div>Tekst</div></wynik>", false),
            ("no result", "Nie mogę tego zrobić.", false),
        ] {
            let r = ai::restore(reply, "<div>Tekt [[obrazek-1]]</div>");
            println!("restore {what}: {}", if r.is_ok() == ok { "ok" } else { "WRONG" });
        }
        return true;
    }
    if args.iter().any(|a| a == "--rich-selftest") {
        // Markdown as an AI agent writes it, the toolbar's marks and a pasted picture → the email sent.
        let png = std::fs::read("assets/poczta.png").unwrap_or_default();
        let (cid, file, w) = rich::store_picture(&png, "png").expect("picture");
        let md = format!(
            "## Podsumowanie\n\nZmiany są **gotowe** i *sprawdzone*:\n\n- wpis SPF\n- wpis `DKIM`\n\n| Domena | Status |\n|---|---|\n| naturbay.de | OK |\n\nTo jest <span style=\"color:#c62828\">ważne</span> i <span style=\"font-size:18px\">duże</span>.\nDruga linia.\n\n{}\n",
            rich::image_tag(&cid, w)
        );
        let html = rich::to_html(&md);
        for (what, ok) in [
            ("heading", html.contains("<h2>Podsumowanie</h2>")),
            ("bold", html.contains("<strong>gotowe</strong>")),
            ("italics", html.contains("<em>sprawdzone</em>")),
            ("list", html.contains("<li>wpis SPF</li>")),
            ("code", html.contains("<code>DKIM</code>")),
            ("table", html.contains("<table>") && html.contains("naturbay.de")),
            ("colour kept", html.contains("color:#c62828")),
            ("line break kept", html.contains("<br")),
            ("picture fits the message", html.contains("max-width:100%") && html.contains(&format!("cid:{cid}"))),
        ] {
            println!("{what}: {ok}");
        }
        println!("picture width in the message: {w} px (picture is 256 px, default cap {})", rich::DEFAULT_WIDTH);
        println!("plain text: {:?}", rich::plain("To jest <span style=\"color:red\">ważne</span> **teraz** <img src=\"cid:x\" width=\"300\">"));
        let mut t = String::from("ala ma kota");
        let sel = rich::wrap(&mut t, 4, 6, "**", "**");
        let once = t.clone();
        rich::wrap(&mut t, sel.0, sel.1, "**", "**");
        println!("bold on: {once:?}, again (off): {t:?}");
        // what is drawn while writing must be exactly the text written (else the cursor drifts)
        let two = format!("Obrazy:
{}
{}
- punkt
> cytat
koniec", rich::image_tag("a@x", 600), rich::image_tag("b@x", 320));
        for sample in [md.as_str(), two.as_str(), "**ż** *ó* <u>ę</u> ~~x~~ `k` <span style=\"color:#c62828\">ą</span>"] {
            let job = rich::highlight(sample, 500.0, eframe::egui::Color32::BLACK);
            println!("drawn text identical: {}", job.text == sample);
        }
        // two pictures one after the other: both stay whole
        let mut body = String::from("Tekst
");
        for (cid, w) in [("p1@poczta", 600u32), ("p2@poczta", 480)] {
            let at = body.chars().count();
            let byte = body.char_indices().nth(at).map(|(b, _)| b).unwrap_or(body.len());
            body.insert_str(byte, &format!("
{}
", rich::image_tag(cid, w)));
        }
        println!("pictures found after two pastes: {:?}", rich::images(&body).iter().map(|p| (p.1.clone(), p.2)).collect::<Vec<_>>());
        let acc = model::Account::default();
        let out = net::Outgoing {
            id: 1,
            to: "anna@example.com".into(),
            cc: String::new(),
            subject: "Test".into(),
            body: rich::plain(&md),
            html: Some(html),
            inline: vec![(cid.clone(), file)],
            files: vec![],
        };
        match net::build_email(&acc, &out) {
            Ok(e) => {
                let b = e.formatted();
                let p = mail_parser::MessageParser::default().parse(&b[..]);
                println!(
                    "email: formatted version {}, pictures inside {}",
                    p.as_ref().and_then(|p| p.body_html(0)).map(|h| h.contains("<h2>")).unwrap_or(false),
                    p.as_ref().map(|p| p.attachments().count()).unwrap_or(0)
                );
            }
            Err(e) => println!("email: {e}"),
        }
        // attached files: with the formatted message and with a plain one
        for formatted in [true, false] {
            let mut out2 = out.clone_for_test();
            out2.files = vec!["Cargo.toml".into(), "assets/poczta.png".into()];
            if !formatted {
                out2.html = None;
                out2.inline = vec![];
            }
            match net::build_email(&acc, &out2) {
                Ok(e) => {
                    let b = e.formatted();
                    let p = mail_parser::MessageParser::default().parse(&b[..]);
                    let names: Vec<String> = p.as_ref().map(|p| p.attachments().filter_map(|a| a.attachment_name().map(String::from)).collect()).unwrap_or_default();
                    println!("email with files (formatted {formatted}): text kept {}, attachments {:?}", p.as_ref().and_then(|p| p.body_text(0)).map(|t| t.contains("gotowe")).unwrap_or(false), names);
                }
                Err(e) => println!("email with files: {e}"),
            }
        }
        return true;
    }
    if args.iter().any(|a| a == "--signature-selftest") {
        let store = model::Store::sample();
        let base = store.messages[0].clone();
        let logo = model::cache_put(&std::fs::read("assets/poczta.png").unwrap_or_default(), "png");
        let sent = |n: u64, body: &str, html: Option<String>| {
            let mut m = base.clone();
            m.id = 1000 + n;
            m.folder = model::Folder::Sent;
            m.sample = false;
            m.date = base.date - chrono::Duration::days(n as i64);
            m.body = body.to_string();
            m.html_path = html.map(|h| model::cache_put(h.as_bytes(), "html")).unwrap_or_default();
            m.inline = vec![("logo1@test".into(), logo.clone())];
            m
        };
        // 1. Gmail marks the signature; a quoted reply carries someone else's
        let gmail: Vec<model::Message> = (0..3)
            .map(|i| {
                let html = format!(
                    "<div>Treść numer {i}</div><div class=\"gmail_signature\" data-smartmail=\"gmail_signature\">Jan Kowalski<br>Dział IT<br>tel. 600 100 200<br><img src=\"cid:logo1@test\"></div>\
                     <blockquote class=\"gmail_quote\"><div class=\"gmail_signature\">Inna Osoba<br>Księgowość</div></blockquote>"
                );
                sent(i, &format!("Treść numer {i}\n\nJan Kowalski\nDział IT\ntel. 600 100 200"), Some(html))
            })
            .collect();
        match signature::detect(&gmail) {
            Some(f) => println!("marked signature: text {:?}, picture kept: {}", f.text, f.images.len() == 1 && f.html.contains("cid:logo1@test")),
            None => println!("marked signature: not found"),
        }
        // 2. no marks: the shared ending of the texts
        let plain: Vec<model::Message> = (0..4)
            .map(|i| {
                sent(
                    i,
                    &format!("Dzień dobry,\nodpowiedź {i} w sprawie.\n\nZ poważaniem\nJan Kowalski\nspecjalista ds. Informatyki\ntel.: 601-088-185\n\n------ Wiadomość oryginalna ------\nOd: ktoś\n> cytat {i}"),
                    None,
                )
            })
            .collect();
        match signature::detect(&plain) {
            Some(f) => println!("shared ending: {:?}", f.text),
            None => println!("shared ending: not found"),
        }
        // 3. a reply sent with the signature: text, formatted version and the picture
        let mut acc = model::Account::default();
        if let Some(f) = signature::detect(&gmail) {
            acc.signature_html = f.html;
            acc.signature_text = f.text;
            acc.signature_images = f.images;
        }
        let out = net::Outgoing {
            id: 1,
            to: "anna@example.com".into(),
            cc: String::new(),
            subject: "Re: Test".into(),
            body: "Dziękuję!".into(),
            html: Some("<p>Dziękuję!</p><br>".to_string() + &acc.signature_html),
            inline: acc.signature_images.clone(),
            files: vec![],
        };
        match net::build_email(&acc, &out) {
            Ok(e) => {
                let bytes = e.formatted();
                let p = mail_parser::MessageParser::default().parse(&bytes[..]);
                println!(
                    "email to send: reads back {}, text has signature: {}, formatted version: {}, pictures: {}",
                    p.is_some(),
                    p.as_ref().and_then(|p| p.body_text(0)).map(|t| t.contains("Dziękuję")).unwrap_or(false),
                    p.as_ref().and_then(|p| p.body_html(0)).map(|h| h.contains("Jan Kowalski")).unwrap_or(false),
                    p.as_ref().map(|p| p.attachments().count()).unwrap_or(0)
                );
            }
            Err(e) => println!("email to send: {e}"),
        }
        return true;
    }
    if args.iter().any(|a| a == "--share-selftest") {
        // The "copy for AI" server, on this computer only, with a sample email: what an agent sees.
        use std::io::{Read, Write};
        println!("Tailscale on this computer: {}", if share::tailscale_ip().is_some() { "yes" } else { "no" });
        println!("name in the Tailscale network: {:?}", share::tailscale_name());
        let store = model::Store::sample();
        let mut m = store.messages.iter().find(|m| !m.attachments.is_empty()).unwrap_or(&store.messages[0]).clone();
        // a screenshot inside the email and a real attachment file, as in a typical email
        let png = crate::model::cache_put(&std::fs::read("assets/poczta.png").unwrap_or_default(), "png");
        let other = crate::model::cache_put(b"zestawienie;kwota\n1;100\n", "csv");
        m.inline = vec![("image001.png@01DC4D2A.5F0A1B20".into(), png)];
        m.attachment_files = vec![other];
        m.attachments = vec!["zestawienie.csv".into()];
        let m = &m;
        let mut hub = share::Hub::new(None);
        // another program on the first port, like on the person's computer
        let _taken = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 8765));
        let addr = hub.start(std::net::Ipv4Addr::LOCALHOST, 0).expect("start");
        println!("server on port {} (8765 was taken)", addr.port());
        let token = share::new_token();
        hub.put(&token, share::Shared::of(m));
        println!("link the app copies: {}", hub.url(&token).unwrap_or_default());
        let ask = |req: String| -> String {
            let mut c = std::net::TcpStream::connect(addr).expect("connect");
            c.write_all(req.as_bytes()).unwrap();
            let mut out = Vec::new();
            let _ = c.read_to_end(&mut out);
            String::from_utf8_lossy(&out).to_string()
        };
        let get = |path: &str| ask(format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n\r\n"));
        let r = get(&format!("/m/{token}"));
        println!("── text ──\n{}", r.split("\r\n\r\n").nth(1).unwrap_or(""));
        let r = get(&format!("/m/{token}.eml"));
        let body = r.split_once("\r\n\r\n").map(|x| x.1).unwrap_or("");
        println!("── eml: {} | {}", r.lines().next().unwrap_or(""), r.lines().find(|l| l.starts_with("Content-Disposition")).unwrap_or(""));
        match mail_parser::MessageParser::default().parse(body.as_bytes()) {
            Some(p) => println!(
                "   read back: subject {:?}, text ok: {}, html: {}, pictures+attachments: {}, draft header: {}",
                p.subject().unwrap_or(""),
                p.body_text(0).map(|t| t.contains("Cześć Aleks")).unwrap_or(false),
                p.body_html(0).is_some(),
                p.attachments().count(),
                body.starts_with("X-Reply-Draft-URL:")
            ),
            None => println!("   could not read the .eml back"),
        }
        let r = get(&format!("/m/{token}.json"));
        println!("── json: {} … ({} bytes)", r.lines().next().unwrap_or(""), r.len());
        let r = get(&format!("/m/{token}/a/0"));
        println!("── attachment: {} | {}", r.lines().next().unwrap_or(""), r.lines().find(|l| l.starts_with("Content-Type")).unwrap_or(""));
        let r = get(&format!("/m/{token}/i/0"));
        println!("── picture in the email: {} | {} | {} bytes", r.lines().next().unwrap_or(""), r.lines().find(|l| l.starts_with("Content-Type")).unwrap_or(""), r.len());
        let r = get("/m/0123456789abcdef0123456789abcdef");
        println!("── wrong link: {}", r.lines().next().unwrap_or(""));
        let text = "Dzień dobry,\ndziękuję — potwierdzam udział w czwartek.\nPozdrawiam";
        let r = ask(format!("POST /m/{token}/draft HTTP/1.1\r\nHost: {addr}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n\r\n{text}", text.len()));
        println!("── draft: {}", r.split("\r\n\r\n").nth(1).unwrap_or("").trim());
        match hub.drafts.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok((id, t)) => println!("app received a draft for email {id} (same email: {}), text kept: {}", id == m.id, t == text),
            Err(_) => println!("no draft arrived"),
        }
        return true;
    }
    if args.iter().any(|a| a == "--contacts-selftest") {
        let sample = model::Store::sample();
        let all = contacts::collect(&sample.messages, &sample.account.email);
        for q in ["m", "ann", "kow", "zesp"] {
            let found: Vec<String> = contacts::find(&all, q, "", 6).iter().map(|c| c.text()).collect();
            println!("sample „{q}” → {found:?}");
        }
        if let Some(store) = db::Db::open().ok().and_then(|mut d| d.load()) {
            let t = std::time::Instant::now();
            let all = contacts::collect(&store.messages, &store.account.email);
            println!("saved mailbox: {} messages → {} people in {:?}", store.messages.len(), all.len(), t.elapsed());
            let t = std::time::Instant::now();
            let n: usize = ["a", "ma", "jan", "kow"].iter().map(|q| contacts::find(&all, q, "", 6).len()).sum();
            println!("4 lookups: {n} suggestions in {:?}", t.elapsed());
        }
        return true;
    }
    if args.iter().any(|a| a == "--unread-report") {
        // Newest inbox messages in the saved mailbox and whether they count as read (no contents).
        let Some(store) = db::Db::open().ok().and_then(|mut d| d.load()) else {
            println!("no saved mailbox");
            return true;
        };
        let mut inbox: Vec<&model::Message> = store.messages.iter().filter(|m| m.folder == model::Folder::Inbox).collect();
        inbox.sort_by(|a, b| b.date.cmp(&a.date));
        println!("inbox: {}, unread: {}", inbox.len(), inbox.iter().filter(|m| !m.read).count());
        for m in inbox.iter().take(15) {
            println!("uid {:?}  {}  read={}", m.uid, m.date, m.read);
        }
        for m in inbox.iter().filter(|m| m.date.format("%Y-%m-%d %H:%M").to_string() == "2026-11-03 08:00") {
            println!("08:00 → uid {:?} {} read={} subject chars {} from {}", m.uid, m.date, m.read, m.subject.chars().count(), m.from_addr.split('@').nth(1).unwrap_or(""));
        }
        if let Some(u) = args.iter().position(|a| a == "--unread-report").and_then(|i| args.get(i + 1)).and_then(|v| v.parse::<u32>().ok()) {
            for m in store.messages.iter().filter(|m| m.uid == Some(u)) {
                println!("uid {u}: folder {:?}, {}, read={}, partial={}", m.folder, m.date, m.read, m.partial);
            }
            println!("messages with uid {u}: {}", store.messages.iter().filter(|m| m.uid == Some(u)).count());
        }
        return true;
    }
    if let Some(i) = args.iter().position(|a| a == "--email-bench") {
        // how long the app takes to draw the newest emails of its cache (nothing shown, no
        // internet): the slowest ones listed with their sizes only
        let n: usize = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(200);
        let mut pages: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(model::cache_dir())
            .map(|r| r.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "html") && !p.ends_with("view.html")).filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p))).collect())
            .unwrap_or_default();
        pages.sort_by(|a, b| b.0.cmp(&a.0));
        pages.truncate(n);
        let fonts = blitz_dom::FontContext::default();
        let mut times: Vec<(u128, u128, u128, u128, u32, u64)> = vec![];
        let all = std::time::Instant::now();
        for (_, p) in &pages {
            let Ok(html) = std::fs::read_to_string(p) else { continue };
            let size = html.len() as u64;
            let t = std::time::Instant::now();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| htmlview::bench(&html, 700.0, 1.25, &fonts)));
            let total = t.elapsed().as_millis();
            match r {
                Ok((a, b, c, h)) => times.push((total, a, b, c, h, size)),
                Err(_) => println!("panicked: {} KB", size / 1024),
            }
        }
        times.sort_by(|a, b| b.0.cmp(&a.0));
        let sum: u128 = times.iter().map(|t| t.0).sum();
        println!("{} emails in {} ms (average {} ms)", times.len(), all.elapsed().as_millis(), sum / times.len().max(1) as u128);
        for t in times.iter().take(12) {
            println!("  {} ms total — read {} ms, styles+layout {} ms, drawing {} ms, {} px tall, {} KB", t.0, t.1, t.2, t.3, t.4, t.5 / 1024);
        }
        return true;
    }
    if args.iter().any(|a| a == "--nav-bench") {
        // a test mailbox (POCZTA_DATA_DIR): 40 000 messages in two accounts, their formatted
        // texts copied from the app's cache, to time how fast emails appear (POCZTA_NAV_TEST)
        let Ok(dir) = std::env::var("POCZTA_DATA_DIR") else {
            println!("set POCZTA_DATA_DIR to a test folder first");
            return true;
        };
        let pages_dir = PathBuf::from(&dir).join("pages");
        let _ = std::fs::create_dir_all(&pages_dir);
        let mut pages: Vec<PathBuf> = std::fs::read_dir(model::cache_dir())
            .map(|r| r.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "html") && !p.ends_with("view.html")).collect())
            .unwrap_or_default();
        pages.sort();
        pages.truncate(40);
        let pages: Vec<PathBuf> = pages
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                let to = pages_dir.join(format!("{i}.html"));
                std::fs::copy(p, &to).ok().map(|_| to)
            })
            .collect();
        println!("{} pages copied", pages.len());
        let _ = std::fs::remove_file(db::path());
        let mut store = model::Store::sample();
        let template = store.messages.iter().find(|m| m.folder == model::Folder::Inbox).cloned().expect("a message");
        store.messages.clear();
        let mut extra = model::Account { display_name: "Konto testowe".into(), email: "test@example.com".into(), ..Default::default() };
        extra.use_gmail();
        store.extra.push(model::ExtraAccount { account: extra, ..Default::default() });
        let now = chrono::Local::now().naive_local();
        for i in 0..40_000u64 {
            let mut m = template.clone();
            m.id = i + 1;
            m.uid = Some(i as u32 + 1);
            m.account = (i % 2) as u32;
            m.subject = format!("Wiadomość testowa numer {i}");
            m.body = "Treść wiadomości testowej. ".repeat(40);
            m.date = now - chrono::Duration::minutes(i as i64 * 7);
            m.html_path = pages.get(i as usize % pages.len().max(1)).map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
            m.read = true;
            store.messages.push(m);
        }
        let mut d = db::Db::open().expect("open");
        d.save(&mut store, true, None).expect("save");
        println!("test mailbox made: {} messages", store.messages.len());
        return true;
    }
    if args.iter().any(|a| a == "--db-selftest") {
        // run with POCZTA_DATA_DIR pointing at a test folder
        if std::env::var("POCZTA_DATA_DIR").is_err() {
            println!("set POCZTA_DATA_DIR to a test folder first");
            return true;
        }
        let _ = std::fs::remove_file(db::path());
        let mut store = model::Store::sample();
        let template = store.messages[0].clone();
        store.messages.clear();
        for i in 0..20_000u64 {
            let mut m = template.clone();
            m.id = i + 1;
            m.uid = Some(i as u32 + 1);
            m.subject = format!("Wiadomość testowa numer {i}");
            m.body = "Treść wiadomości testowej. ".repeat(80);
            store.messages.push(m);
        }
        let mut d = db::Db::open().expect("open");
        let t = std::time::Instant::now();
        d.save(&mut store, true, None).expect("save");
        println!("first save of 20 000 messages: {:?}", t.elapsed());
        for m in store.messages.iter_mut().take(10) {
            m.read = !m.read;
        }
        let t = std::time::Instant::now();
        d.save(&mut store, true, None).expect("save");
        println!("save after changing 10 messages: {:?}", t.elapsed());
        drop(d);
        let t = std::time::Instant::now();
        let mut d = db::Db::open().expect("open");
        let loaded = d.load().expect("load");
        let reader = d.reader();
        let texts_ok = loaded.messages.iter().take(200).all(|m| m.on_disk && m.body.is_empty() && reader.text(m.id).is_some_and(|t| t.len() > 100) && !m.snippet.trim().is_empty());
        println!("open + load all: {:?} ({} messages, texts on disk and readable: {texts_ok})", t.elapsed(), loaded.messages.len());
        let changed_ok = loaded.messages.iter().zip(store.messages.iter()).take(20).all(|(a, b)| a.id == b.id && a.read == b.read);
        println!("changes kept after reopening: {changed_ok}");
        // a changed text and a changed tag must be saved too
        let mut d = d;
        let mut again = loaded;
        again.messages[3].body = "Nowa treść".into();
        again.messages[3].on_disk = false;
        again.messages[5].labels.push("Test".into());
        d.save(&mut again, true, None).expect("save");
        drop(d);
        println!("after saving, the text went back to disk: {}", again.messages[3].on_disk && again.messages[3].body.is_empty());
        drop(reader);
        let mut d2 = db::Db::open().expect("open");
        let back = d2.load().expect("load");
        let id3 = back.messages[3].id;
        println!(
            "new text kept: {}, its excerpt: {:?}, new tag kept: {}",
            d2.text(id3).as_deref() == Some("Nowa treść"),
            back.messages[3].snippet,
            back.messages[5].labels.contains(&"Test".to_string())
        );
        println!("file size: {} MB", std::fs::metadata(db::path()).map(|m| m.len() / 1_048_576).unwrap_or(0));
        return true;
    }
    if args.iter().any(|a| a == "--semantic-selftest") {
        if !semantic::is_downloaded() {
            let (tx, rx) = std::sync::mpsc::channel();
            semantic::download(tx, eframe::egui::Context::default());
            let mut shown = 0;
            loop {
                match rx.recv() {
                    Ok(semantic::Progress::Downloading(p)) => {
                        let pct = (p * 100.0) as i32;
                        if pct >= shown + 10 {
                            shown = pct / 10 * 10;
                            println!("download {pct}%");
                        }
                    }
                    Ok(semantic::Progress::Done) => break,
                    Ok(semantic::Progress::Failed(e)) => {
                        println!("download failed: {e}");
                        return true;
                    }
                    Err(_) => return true,
                }
            }
        }
        let t = std::time::Instant::now();
        let model = match semantic::Model::load() {
            Ok(m) => m,
            Err(e) => {
                println!("load failed: {e}");
                return true;
            }
        };
        println!("model loaded in {:?}, {} numbers per text", t.elapsed(), model.dim);
        let docs = [
            "Faktura VAT za październik w załączniku, termin płatności 14 dni",
            "Zaproszenie na spotkanie zespołu w czwartek o 10:00 w sali B",
            "Zdjęcia z wakacji nad morzem, było super!",
            "Przypomnienie: rachunek za prąd do zapłaty",
        ];
        let vecs: Vec<Vec<i8>> = docs.iter().map(|d| semantic::pack(&model.embed(d))).collect();
        for q in ["płatność za usługi", "narada w biurze", "urlop i plaża"] {
            let qv = model.embed(q);
            let mut s: Vec<(f32, &str)> = vecs.iter().zip(docs).map(|(v, d)| (semantic::similarity(&qv, v), d)).collect();
            s.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
            println!("\n„{q}”");
            for (score, d) in s {
                println!("  {score:.2}  {d}");
            }
        }
        let t = std::time::Instant::now();
        for _ in 0..1000 {
            let _ = model.embed(docs[0]);
        }
        println!("\n1000 texts in {:?}", t.elapsed());
        return true;
    }
    if args.iter().any(|a| a == "--labels-selftest") {
        // What Gmail answers to FETCH (X-GM-LABELS), with a Polish label in IMAP's letter encoding.
        let raw = "* 12 FETCH (X-GM-LABELS (\\Important \"Faktury i rachunki\" Praca \"Wymaga dzia&AUI-ania\") UID 345)\r\n* 13 FETCH (X-GM-LABELS ())\r\n";
        let map = net::parse_gm_labels(raw.as_bytes());
        println!("uid 345 -> {:?}, 13 -> {:?}", map.get(&345), map.get(&13));
        // The same through the connection's line reader, with a label sent as a raw block {12}.
        let wire = b"* 7 FETCH (UID 900 X-GM-LABELS (Praca {10}\r\nZ \"cudzys\" \\Starred))\r\n* 8 FETCH (UID 901 X-GM-LABELS (\"Do zrobienia\"))\r\n";
        let lines = rawimap::split_answer(wire);
        let map = net::parse_gm_labels(lines.join("\r\n").as_bytes());
        println!("uid 900 -> {:?}, uid 901 -> {:?}", map.get(&900), map.get(&901));
        let enc = net::encode_mutf7("Wymaga działania & faktury");
        println!("encoded: {enc}  back: {}", net::decode_mutf7(&enc));
        return true;
    }
    if args.iter().any(|a| a == "--attachment-selftest") {
        // A small email with a text attachment ("Hello attachment") and a PDF name.
        let raw = "From: Test <t@example.com>\r\nTo: me@example.com\r\nSubject: Raport\r\nDate: Mon, 2 Nov 2026 10:00:00 +0100\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b1\"\r\n\r\n--b1\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nW załączniku raport.\r\n--b1\r\nContent-Type: application/pdf; name=\"raport dzienny.pdf\"\r\nContent-Disposition: attachment; filename=\"raport dzienny.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nSGVsbG8gYXR0YWNobWVudA==\r\n--b1--\r\n";
        match net::parse(raw.as_bytes(), false, false) {
            Some(m) => {
                let content = m.attachment_files.first().and_then(|f| std::fs::read_to_string(f).ok());
                println!("attachments: {:?}, saved content: {:?}", m.attachments, content);
            }
            None => println!("could not read the test email"),
        }
        return true;
    }
    if args.iter().any(|a| a == "--secret-selftest") {
        let saved = secret::protect("abcd efgh ijkl mnop");
        let back = secret::unprotect(&saved);
        println!(
            "encrypted: {} chars, plain text visible: {}, round trip ok: {}",
            saved.len(),
            saved.contains("abcd"),
            back.as_deref() == Some("abcd efgh ijkl mnop")
        );
        return true;
    }
    if args.iter().any(|a| a == "--oauth-selftest") {
        // Talks to Google's token server with dummy values: it should answer "invalid_client".
        match oauth::access_token("selftest.apps.googleusercontent.com", "x", "x") {
            Ok(_) => println!("unexpected: got a token"),
            Err(e) => println!("Google answered as expected: {e}"),
        }
        return true;
    }
    if let Some(path) = args
        .iter()
        .position(|a| a == "--save-icon")
        .and_then(|i| args.get(i + 1))
    {
        let px = icon::rgba();
        if path.to_lowercase().ends_with(".ico") {
            // An .ico file with the icon in several sizes (PNG images), so Windows shows a
            // sharp picture everywhere from the taskbar to large Explorer views.
            let big = image::RgbaImage::from_raw(icon::SIZE, icon::SIZE, px).expect("icon size");
            let sizes = [16u32, 20, 24, 32, 40, 48, 64, 128, 256];
            let pngs: Vec<Vec<u8>> = sizes
                .iter()
                .map(|&n| {
                    let img = if n == icon::SIZE { big.clone() } else { image::imageops::resize(&big, n, n, image::imageops::FilterType::Lanczos3) };
                    let mut png = Vec::new();
                    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).expect("could not encode the icon");
                    png
                })
                .collect();
            let mut ico = vec![0u8, 0, 1, 0];
            ico.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
            let mut offset = 6 + 16 * sizes.len() as u32;
            for (n, png) in sizes.iter().zip(&pngs) {
                let b = if *n >= 256 { 0 } else { *n as u8 };
                ico.extend_from_slice(&[b, b, 0, 0, 1, 0, 32, 0]);
                ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
                ico.extend_from_slice(&offset.to_le_bytes());
                offset += png.len() as u32;
            }
            for png in &pngs {
                ico.extend_from_slice(png);
            }
            std::fs::write(path, ico).expect("could not save the icon");
        } else {
            image::save_buffer(path, &px, icon::SIZE, icon::SIZE, image::ColorType::Rgba8)
                .expect("could not save the icon");
        }
        return true;
    }
    false
}
