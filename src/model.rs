use chrono::{Duration, Local, NaiveDate, NaiveDateTime, NaiveTime};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Folder {
    Inbox,
    Drafts,
    Outbox,
    Sent,
    Archive,
    Junk,
    Trash,
}

impl Folder {
    pub const ALL: [Folder; 7] = [
        Folder::Inbox,
        Folder::Drafts,
        Folder::Outbox,
        Folder::Sent,
        Folder::Archive,
        Folder::Junk,
        Folder::Trash,
    ];

    /// The folder's name in the interface language. Read once per language and kept (7 × 2 short
    /// strings), so callers keep getting a `&'static str`.
    pub fn name(self) -> &'static str {
        static NAMES: std::sync::OnceLock<[[&'static str; 7]; 2]> = std::sync::OnceLock::new();
        let names = NAMES.get_or_init(|| {
            let keep = |s: std::borrow::Cow<str>| -> &'static str { Box::leak(s.into_owned().into_boxed_str()) };
            ["en", "pl"].map(|l| {
                [
                    keep(t!("misc.model.inbox", locale = l)),
                    keep(t!("misc.model.drafts", locale = l)),
                    keep(t!("misc.model.outbox", locale = l)),
                    keep(t!("misc.model.sent", locale = l)),
                    keep(t!("misc.model.archive", locale = l)),
                    keep(t!("misc.model.junk", locale = l)),
                    keep(t!("misc.model.trash", locale = l)),
                ]
            })
        });
        names[crate::i18n::is_pl() as usize][self as usize]
    }

    pub fn icon(self) -> &'static str {
        use crate::theme::ic;
        match self {
            Folder::Inbox => ic::INBOX,
            Folder::Drafts => ic::EDIT,
            Folder::Outbox => ic::OUTBOX,
            Folder::Sent => ic::SEND,
            Folder::Archive => ic::ARCHIVE,
            Folder::Junk => ic::BLOCKED,
            Folder::Trash => ic::DELETE,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub id: u64,
    pub folder: Folder,
    /// which mail account it belongs to (0: the main one, 1…: the further ones)
    #[serde(default)]
    pub account: u32,
    pub from_name: String,
    pub from_addr: String,
    pub to: String,
    #[serde(default)]
    pub cc: String,
    pub subject: String,
    pub body: String,
    pub date: NaiveDateTime,
    pub read: bool,
    pub flagged: bool,
    #[serde(default)]
    pub attachments: Vec<String>,
    /// Built-in example mail, removed once real mail arrives.
    #[serde(default)]
    pub sample: bool,
    /// The formatted (HTML) version of the email, kept as a file in the app's cache folder.
    #[serde(default)]
    pub html_path: String,
    /// Pictures carried inside the email: (content id, cached file).
    #[serde(default)]
    pub inline: Vec<(String, String)>,
    /// Cached files of the attachments, in the same order as `attachments`.
    #[serde(default)]
    pub attachment_files: Vec<String>,
    /// Gmail labels ("tags") on this message.
    #[serde(default)]
    pub labels: Vec<String>,
    /// The message's number in the server's inbox, used to save changes back to the server.
    #[serde(default)]
    pub uid: Option<u32>,
    /// The message text kept in its own file, so the saved mailbox stays small even with
    /// thousands of messages offline. Empty until the text has been written out.
    #[serde(default)]
    pub body_path: String,
    /// Only the header is here yet (a big message); the rest comes when it is opened.
    #[serde(default)]
    pub partial: bool,
    /// The start of the text, for the message list (the text itself stays on disk).
    #[serde(default)]
    pub snippet: String,
    /// The text is in the database, not in memory; it is read when the message is opened.
    #[serde(skip)]
    pub on_disk: bool,
}

impl Message {
    /// An empty message (e.g. to show a signature with the email renderer).
    pub fn blank() -> Self {
        Message {
            id: u64::MAX - 1,
            folder: Folder::Drafts,
            account: 0,
            from_name: String::new(),
            from_addr: String::new(),
            to: String::new(),
            cc: String::new(),
            subject: String::new(),
            body: String::new(),
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
        }
    }
}

/// A small formatted newsletter for the sample mailbox: banner picture, heading, list, link (in Polish or English).
fn sample_newsletter_html(pl: bool) -> String {
    use base64::Engine;
    let (w, h) = (560u32, 150u32);
    let img = image::RgbaImage::from_fn(w, h, |x, y| {
        let t = x as f32 / w as f32;
        let hill = ((x as f32 / 70.0).sin() * 14.0 + 105.0) as u32;
        if y > hill {
            image::Rgba([(0x3A as f32 + 30.0 * t) as u8, 0x9E, 0x5B, 255])
        } else {
            let sky = y as f32 / h as f32;
            image::Rgba([(0x7F as f32 + 60.0 * sky) as u8, (0xB8 as f32 + 40.0 * sky) as u8, 0xF5, 255])
        }
    });
    let mut png = Vec::new();
    if img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).is_err() {
        return String::new();
    }
    let data = base64::engine::general_purpose::STANDARD.encode(&png);
    let l = |p: &'static str, e: &'static str| if pl { p } else { e };
    let html = format!(
        r#"<html><head><style>p {{ margin: 0 }}</style></head><body>
<div style="display:none;max-height:0">{}</div>
<table width="100%"><tr><td align="center">
<img src="data:image/png;base64,{data}" width="560" height="150" alt="{}">
<h1>{}</h1>
<p>{}</p>
<p><a href="https://example.org/ogrod">{}</a></p>
</td></tr></table>
<table width="100%" cellspacing="8"><tr>
<td align="center" style="background:#EEF4FD;border:1px solid #D6E4FA"><div style="font-size:26px;font-weight:bold;color:#1E6FD9">58</div><div style="font-size:12px;color:#555">{}</div></td>
<td align="center" style="background:#EEF4FD;border:1px solid #D6E4FA"><div style="font-size:26px;font-weight:bold;color:#1E6FD9">3</div><div style="font-size:12px;color:#555">{}</div></td>
<td align="center" style="background:#EEF4FD;border:1px solid #D6E4FA"><div style="font-size:26px;font-weight:bold;color:#1E6FD9">8</div><div style="font-size:12px;color:#555">{}</div></td>
</tr></table>
<table width="100%" cellspacing="0" style="border:1px solid #e3e3e3">
<thead><tr style="background:#f5f5f5"><th align="left">{}</th><th align="left">{}</th><th>{}</th></tr></thead>
<tbody>
<tr><td>{}<br><span style="font-size:12px;color:#777">{}</span></td><td>{}</td><td align="center">10:00–16:00</td></tr>
<tr><td>{}</td><td>{}</td><td align="center">15:00</td></tr>
<tr><td>{}</td><td>{}</td><td align="center">18:30</td></tr>
</tbody></table>
<p>{}</p>
<ul><li>{}</li><li>{}</li></ul>
<p>{}<br><i>{}</i></p>
<hr><p style="font-size:12px">{}</p>
<img src="https://example.org/pixel.gif" width="1" height="1">
</body></html>"#,
        l("Ukryty tekst podglądu", "Hidden preview text"),
        l("Ogród", "Garden"),
        l("Dzień otwarty ogrodu", "Garden open day"),
        l("W sobotę od <b>10:00</b> do <b>16:00</b> zapraszamy do ogrodu społecznego.", "Join us at the community garden on Saturday from <b>10:00</b> to <b>16:00</b>."),
        l("Zobacz program dnia", "See the day's program"),
        l("zapisanych osób", "people signed up"),
        l("warsztaty", "workshops"),
        l("stoisk", "stalls"),
        l("Wydarzenie", "Event"),
        l("Miejsce", "Place"),
        l("Godzina", "Time"),
        l("Dzień otwarty ogrodu", "Garden open day"),
        l("warsztaty sadzenia · degustacja", "planting workshops · tasting"),
        l("Ogród społeczny", "Community garden"),
        l("Wymiana książek", "Book swap"),
        l("Biblioteka", "Library"),
        l("Spotkanie wolontariuszy", "Volunteer meeting"),
        l("Dom kultury, sala 2", "Community center, room 2"),
        l("Najważniejsze w tym tygodniu:", "This week's highlights:"),
        l("Wymiana książek w bibliotece w niedzielę po południu", "Book swap at the library on Sunday afternoon"),
        l("Szukamy wolontariuszy do wiosennego sprzątania", "We're looking for volunteers for the spring cleanup"),
        l("Do zobaczenia!", "See you there!"),
        l("Zespół newslettera", "The newsletter team"),
        l("Otrzymujesz tę wiadomość, bo zapisałeś się do newslettera osiedlowego.", "You're receiving this because you signed up for the neighborhood newsletter."),
    );
    cache_put(html.as_bytes(), "html")
}

/// Folder for formatted emails and their pictures: %LOCALAPPDATA%\Poczta\cache
pub fn cache_dir() -> std::path::PathBuf {
    let base = std::env::var("LOCALAPPDATA")
        .or_else(|_| std::env::var("APPDATA"))
        .unwrap_or_else(|_| ".".into());
    let dir = std::path::Path::new(&base).join("Poczta").join("cache");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Saves bytes in the cache under a name made from their content; returns the file path.
pub fn cache_put(bytes: &[u8], ext: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    bytes.len().hash(&mut h);
    let path = cache_dir().join(format!("{:016x}.{}", h.finish(), ext));
    if !path.exists() {
        let _ = std::fs::write(&path, bytes);
    }
    path.to_string_lossy().into_owned()
}

impl Message {
    pub fn preview(&self) -> String {
        if self.body.is_empty() {
            return self.snippet.chars().take(110).collect();
        }
        snippet_of(&self.body)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub id: u64,
    pub title: String,
    pub date: NaiveDate,
    pub start: NaiveTime,
    pub minutes: i64,
    pub location: String,
    pub color: usize,
    /// from Google Calendar (read-only here)
    #[serde(default)]
    pub google: bool,
    /// lasts the whole day (no hour)
    #[serde(default)]
    pub all_day: bool,
    /// its id in Google Calendar, when it can be changed from here (through the script)
    #[serde(default)]
    pub gid: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    pub display_name: String,
    pub email: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub username: String,
    /// The password in plain form, only in memory.
    #[serde(skip)]
    pub password: String,
    /// The password encrypted by Windows for this user (see `secret.rs`); this is what is saved.
    #[serde(default)]
    pub password_saved: String,
    /// Sign in with Google's own page (OAuth) instead of a password.
    #[serde(default)]
    pub use_oauth: bool,
    #[serde(default)]
    pub oauth_client_id: String,
    #[serde(default)]
    pub oauth_client_secret: String,
    /// Long-lived permission from Google; lets the app get fresh access without signing in again.
    #[serde(default)]
    pub oauth_refresh_token: String,
    /// what the Google sign-in allows (mail, calendar, contacts), separated by spaces
    #[serde(default)]
    pub oauth_scopes: String,
    /// The Google Workspace domain of the Google sign-in ("" for a personal account, None while
    /// not known yet): the chat is offered only to Workspace accounts.
    #[serde(default)]
    pub google_hd: Option<String>,
    /// The Google account's id (the chat tells the person's own messages by it).
    #[serde(default)]
    pub google_sub: String,
    /// The person's signature (formatted and as text), found in their sent mail.
    #[serde(default)]
    pub signature_html: String,
    #[serde(default)]
    pub signature_text: String,
    /// pictures inside the signature: (content id, cached file)
    #[serde(default)]
    pub signature_images: Vec<(String, String)>,
    /// Add the signature to new messages and replies.
    #[serde(default = "yes")]
    pub signature_on: bool,
    /// Keep taking the signature from sent mail (off once the person removed it).
    #[serde(default = "yes")]
    pub signature_auto: bool,
}

fn yes() -> bool {
    true
}

impl Default for Account {
    fn default() -> Self {
        Self {
            display_name: "Aleks Nowicki".into(),
            email: "aleks@example.com".into(),
            imap_host: String::new(),
            imap_port: 993,
            smtp_host: String::new(),
            smtp_port: 587,
            username: String::new(),
            password: String::new(),
            password_saved: String::new(),
            use_oauth: false,
            oauth_client_id: String::new(),
            oauth_client_secret: String::new(),
            oauth_refresh_token: String::new(),
            oauth_scopes: String::new(),
            google_hd: None,
            google_sub: String::new(),
            signature_html: String::new(),
            signature_text: String::new(),
            signature_images: vec![],
            signature_on: true,
            signature_auto: true,
        }
    }
}

impl Account {
    pub fn is_configured(&self) -> bool {
        !self.imap_host.trim().is_empty() && !self.username.trim().is_empty()
    }

    /// Everything needed to talk to the server is present.
    pub fn can_connect(&self) -> bool {
        self.is_configured()
            && if self.use_oauth {
                !self.oauth_refresh_token.is_empty() && !self.oauth_client_id.trim().is_empty()
            } else {
                !self.password.is_empty()
            }
    }

    pub fn use_gmail(&mut self) {
        self.imap_host = "imap.gmail.com".into();
        self.imap_port = 993;
        self.smtp_host = "smtp.gmail.com".into();
        self.smtp_port = 465;
        self.use_oauth = true;
        if self.username.trim().is_empty() || ["aleks@example.com", "alex@example.com"].contains(&self.username.as_str()) {
            self.username = self.email.clone();
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Store {
    pub next_id: u64,
    pub account: Account,
    pub messages: Vec<Message>,
    pub events: Vec<Event>,
    /// Becomes true once real mail has been received, so the samples are dropped.
    #[serde(default)]
    pub has_real_mail: bool,
    /// further mail accounts (mail only, signed in with a password)
    #[serde(default)]
    pub extra: Vec<ExtraAccount>,
    /// How far the inbox sync has got (for downloading only new mail).
    #[serde(default)]
    pub sync: Option<crate::net::SyncState>,
    /// The same for the folder of sent mail.
    #[serde(default)]
    pub sent_sync: Option<crate::net::SyncState>,
    /// The same for the bin and spam.
    #[serde(default)]
    pub trash_sync: Option<crate::net::SyncState>,
    #[serde(default)]
    pub junk_sync: Option<crate::net::SyncState>,
    /// Accounts (by address) whose stored mail got its copied people (DW) from the server.
    #[serde(default)]
    pub copies_read: Vec<String>,
}

/// A further mail account and how far its folders are synced.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtraAccount {
    pub account: Account,
    pub sync: Option<crate::net::SyncState>,
    pub sent_sync: Option<crate::net::SyncState>,
    pub trash_sync: Option<crate::net::SyncState>,
    pub junk_sync: Option<crate::net::SyncState>,
}

impl Store {
    pub fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn sample() -> Self {
        let now = Local::now().naive_local();
        let today = now.date();
        let at = |days: i64, h: u32, m: u32| -> NaiveDateTime {
            (today - Duration::days(days)).and_hms_opt(h, m, 0).unwrap()
        };
        // the demo mailbox in the interface language: (Polish, English), the same mail either way
        let pl = crate::i18n::is_pl();
        let l = |p: &'static str, e: &'static str| if pl { p } else { e };
        let mut account = Account::default();
        if !pl {
            account.display_name = "Alex Newman".into();
            account.email = "alex@example.com".into();
        }
        let mut s = Store {
            next_id: 0,
            account,
            messages: vec![],
            events: vec![],
            has_real_mail: false,
            extra: vec![],
            sync: None,
            sent_sync: None,
            trash_sync: None,
            junk_sync: None,
            copies_read: vec![],
        };

        type Row<'a> = (Folder, &'a str, &'a str, &'a str, &'a str, NaiveDateTime, bool, bool, Vec<&'a str>, &'a str);
        let mails: Vec<Row> = vec![
            (Folder::Inbox, l("Anna Kowalska", "Emma Clarke"), l("anna.kowalska@example.com", "emma.clarke@example.com"), l("Spotkanie startowe w czwartek", "Kickoff meeting on Thursday"),
             l("Cześć Aleks,\n\ndzięki za udział we wczorajszej rozmowie. Przygotowałam krótki plan czwartkowego spotkania:\n\n  1. Cele na pierwszy kwartał\n  2. Kto za co odpowiada\n  3. Terminy pierwszego przeglądu\n\nCzy możesz przynieść wstępny harmonogram? Jeśli czwartek Ci nie pasuje, daj znać, to przełożę.\n\nPozdrawiam,\nAnna",
               "Hi Alex,\n\nthanks for joining yesterday's call. I've put together a short plan for Thursday's meeting:\n\n  1. Goals for the first quarter\n  2. Who owns what\n  3. Dates for the first review\n\nCould you bring a draft schedule? If Thursday doesn't work for you, let me know and I'll move it.\n\nBest,\nEmma"),
             at(0, 6, 55), true, true, vec!["plan.pdf"], ""),
            (Folder::Inbox, l("Piotr Nowak", "Paul Turner"), l("piotr.nowak@example.org", "paul.turner@example.org"), l("Re: Zdjęcia z weekendu", "Re: Photos from the weekend"),
             l("Wyszły świetnie! Resztę wrzuciłem do wspólnego albumu. To znad jeziora jest moje ulubione.\n\nPiotr",
               "They came out great! I put the rest in the shared album. The one by the lake is my favorite.\n\nPaul"),
             at(1, 19, 46), false, false, vec![l("jezioro.jpg", "lake.jpg"), l("gory.jpg", "mountains.jpg")], ""),
            (Folder::Inbox, l("Zespół rozliczeń", "Billing team"), l("rozliczenia@example.net", "billing@example.net"), l("Twoje miesięczne zestawienie jest gotowe", "Your monthly statement is ready"),
             l("Dzień dobry,\n\nzestawienie za ten miesiąc jest już dostępne. Możesz je zobaczyć po zalogowaniu się do konta.\n\nTo jest wiadomość automatyczna, prosimy na nią nie odpowiadać.",
               "Hello,\n\nyour statement for this month is now available. You can view it after signing in to your account.\n\nThis is an automated message, please do not reply."),
             at(1, 16, 22), true, false, vec![], ""),
            (Folder::Inbox, l("Marta Wiśniewska", "Martha Wells"), l("marta.w@example.com", "martha.w@example.com"), l("Obiad w przyszłym tygodniu?", "Lunch next week?"),
             l("Hej! Dawno się nie widziałyśmy. Masz czas na obiad we wtorek albo w środę? Przy dworcu otworzyli nowe miejsce, które chcę wypróbować.\n\nMarta",
               "Hey! It's been ages. Are you free for lunch on Tuesday or Wednesday? A new place just opened by the station and I'd like to try it.\n\nMartha"),
             at(1, 16, 14), false, false, vec![], ""),
            (Folder::Inbox, l("Ewa Lewandowska", "Eva Lewis"), l("ewa@example.com", "eva@example.com"), l("Zaktualizowany budżet", "Updated budget"),
             l("Cześć,\n\nw załączniku zaktualizowany budżet z liczbami od księgowości. Główna zmiana dotyczy podróży — spadły o około dziesięć procent.\n\nDaj znać, jeśli coś się nie zgadza.\n\nEwa",
               "Hi,\n\nattached is the updated budget with the numbers from accounting. The main change is travel — down by about ten percent.\n\nLet me know if anything looks off.\n\nEva"),
             at(2, 11, 5), true, true, vec![l("budzet-v3.xlsx", "budget-v3.xlsx")], ""),
            (Folder::Inbox, l("Newsletter osiedlowy", "Neighborhood newsletter"), "news@example.org", l("W tym tygodniu: dzień otwarty ogrodu i wymiana książek", "This week: garden open day and book swap"),
             l("Najważniejsze w tym tygodniu:\n\n- Dzień otwarty ogrodu społecznego w sobotę od 10 do 16.\n- Wymiana książek w bibliotece w niedzielę po południu.\n- Szukamy wolontariuszy do wiosennego sprzątania.\n\nDo zobaczenia!",
               "This week's highlights:\n\n- Community garden open day on Saturday from 10 to 4.\n- Book swap at the library on Sunday afternoon.\n- We're looking for volunteers for the spring cleanup.\n\nSee you there!"),
             at(3, 6, 0), true, false, vec![], ""),
            (Folder::Inbox, l("Tomasz Zieliński", "Thomas Green"), l("t.zielinski@example.com", "t.green@example.com"), l("Projekt umowy do przejrzenia", "Draft contract for review"),
             l("Dzień dobry,\n\nw załączniku projekt umowy. Zaznaczyłem fragmenty, o których rozmawialiśmy. Chcielibyśmy podpisać do końca miesiąca.\n\nZ poważaniem,\nTomasz Zieliński",
               "Hello,\n\nplease find the draft contract attached. I've marked the parts we discussed. We'd like to sign by the end of the month.\n\nKind regards,\nThomas Green"),
             at(4, 14, 20), true, false, vec![l("umowa-projekt.docx", "contract-draft.docx")], ""),
            (Folder::Inbox, l("Kamil Wójcik", "Kevin Walsh"), l("kamil@example.net", "kevin@example.net"), l("Slajdy z konferencji", "Slides from the conference"),
             l("Cześć,\n\nzgodnie z obietnicą przesyłam slajdy z mojego wystąpienia. Możesz je udostępnić zespołowi.\n\nKamil",
               "Hi,\n\nas promised, here are the slides from my talk. Feel free to share them with the team.\n\nKevin"),
             at(6, 10, 0), true, false, vec![l("slajdy.pdf", "slides.pdf")], ""),
            (Folder::Inbox, l("Dział IT", "IT department"), "it@example.com", l("Planowana przerwa techniczna w niedzielę", "Planned maintenance on Sunday"),
             l("Informujemy, że poczta i serwer plików będą niedostępne w niedzielę między 22:00 a 2:00 z powodu aktualizacji.\n\nDziękujemy za cierpliwość.",
               "Mail and the file server will be unavailable on Sunday between 22:00 and 2:00 for an update.\n\nThank you for your patience."),
             at(9, 16, 45), true, false, vec![], ""),
            (Folder::Sent, l("Aleks Nowicki", "Alex Newman"), l("aleks@example.com", "alex@example.com"), l("Re: Projekt umowy do przejrzenia", "Re: Draft contract for review"),
             l("Dzień dobry,\n\ndziękuję, przejrzę go w tym tygodniu i wyślę uwagi do piątku.\n\nAleks",
               "Hello,\n\nthank you, I'll review it this week and send my comments by Friday.\n\nAlex"),
             at(3, 9, 0), true, false, vec![], l("t.zielinski@example.com", "t.green@example.com")),
            (Folder::Sent, l("Aleks Nowicki", "Alex Newman"), l("aleks@example.com", "alex@example.com"), l("Termin urlopu", "Vacation dates"),
             l("Cześć Anno,\n\nuprzedzam, że pierwsze dwa tygodnie przyszłego miesiąca będę na urlopie.\n\nAleks",
               "Hi Emma,\n\njust a heads-up that I'll be on vacation for the first two weeks of next month.\n\nAlex"),
             at(8, 15, 30), true, false, vec![], l("anna.kowalska@example.com", "emma.clarke@example.com")),
            (Folder::Drafts, l("Aleks Nowicki", "Alex Newman"), l("aleks@example.com", "alex@example.com"), l("Pomysły na dzień integracyjny", "Team day ideas"),
             l("Na razie:\n- spacer w górach\n- warsztaty kulinarne\n- escape room", "So far:\n- a hike in the mountains\n- a cooking class\n- an escape room"),
             at(2, 20, 12), true, false, vec![], ""),
            (Folder::Archive, l("Piotr Nowak", "Paul Turner"), l("piotr.nowak@example.org", "paul.turner@example.org"), l("Oglądanie mieszkania potwierdzone", "Apartment viewing confirmed"),
             l("Oglądanie potwierdzone na sobotę na 11. Do zobaczenia!", "The viewing is confirmed for Saturday at 11. See you there!"),
             at(30, 12, 0), true, false, vec![], ""),
            (Folder::Junk, l("Biuro nagród", "Prize office"), l("wygrana@example.biz", "winner@example.biz"), l("Zostałeś wybrany!!!", "You have been selected!!!"),
             l("Kliknij tutaj, aby odebrać nagrodę. Tylko dziś.", "Click here to claim your prize. Today only."),
             at(1, 3, 33), false, false, vec![], ""),
            (Folder::Trash, l("Stary sklep", "Old shop"), l("oferty@example.biz", "offers@example.biz"), l("Ostatnia szansa: -20%", "Last chance: 20% off"),
             l("Wyprzedaż kończy się dziś wieczorem.", "The sale ends tonight."),
             at(5, 18, 0), true, false, vec![], ""),
        ];
        let to_me = l("Aleks Nowicki <aleks@example.com>", "Alex Newman <alex@example.com>");
        for (folder, name, addr, subject, body, date, read, flagged, att, to) in mails {
            let id = s.new_id();
            let to = if folder == Folder::Sent || folder == Folder::Drafts {
                to.to_string()
            } else {
                to_me.to_string()
            };
            s.messages.push(Message {
                id,
                folder,
                account: 0,
                from_name: name.into(),
                from_addr: addr.into(),
                to,
                cc: String::new(),
                subject: subject.into(),
                body: body.into(),
                date,
                read,
                flagged,
                attachments: att.into_iter().map(String::from).collect(),
                sample: true,
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
        }

        // Sample tags, like Gmail labels.
        let (work, bills, todo, family) = (l("Praca", "Work"), l("Faktury i rachunki", "Bills"), l("Do zrobienia", "To do"), l("Rodzina", "Family"));
        for (who, tags) in [
            (l("anna.kowalska@example.com", "emma.clarke@example.com"), vec![work]),
            (l("ewa@example.com", "eva@example.com"), vec![work, bills]),
            (l("t.zielinski@example.com", "t.green@example.com"), vec![work, todo]),
            (l("rozliczenia@example.net", "billing@example.net"), vec![bills]),
            (l("piotr.nowak@example.org", "paul.turner@example.org"), vec![family]),
            ("news@example.org", vec!["Newsletter"]),
        ] {
            for m in s.messages.iter_mut().filter(|m| m.from_addr == who && m.folder == Folder::Inbox) {
                m.labels = tags.iter().map(|t| t.to_string()).collect();
            }
        }

        // The newsletter also has a formatted version with a picture.
        if let Some(m) = s.messages.iter_mut().find(|m| m.from_addr == "news@example.org") {
            m.html_path = sample_newsletter_html(pl);
        }

        let ev = |d: i64, h: u32, m: u32| (today + Duration::days(d), NaiveTime::from_hms_opt(h, m, 0).unwrap());
        let events = vec![
            (l("Spotkanie IT", "IT meeting"), ev(0, 9, 0), 60, l("Spotkanie online", "Online meeting"), 0),
            (l("Dentysta", "Dentist"), ev(0, 16, 0), 45, l("ul. Główna 12", "12 Main Street"), 2),
            (l("Spotkanie startowe", "Kickoff meeting"), ev(2, 10, 0), 90, l("Sala A", "Room A"), 1),
            (l("Obiad z Martą", "Lunch with Martha"), ev(6, 12, 30), 60, l("Kawiarnia przy dworcu", "Café by the station"), 3),
            (l("Przegląd budżetu", "Budget review"), ev(8, 14, 0), 60, "Online", 0),
            (l("Wymiana książek", "Book swap"), ev(4, 15, 0), 120, l("Biblioteka", "Library"), 3),
            (l("Przegląd kwartalny", "Quarterly review"), ev(15, 11, 0), 120, l("Sala B", "Room B"), 1),
            (l("Siłownia", "Gym"), ev(-3, 18, 0), 60, "", 2),
        ];
        for (title, (date, start), minutes, location, color) in events {
            let id = s.new_id();
            s.events.push(Event {
                id,
                title: title.into(),
                date,
                start,
                minutes,
                location: location.into(),
                color,
                google: false,
                all_day: false,
                gid: String::new(),
            });
        }

        s
    }
}

pub fn initials(name: &str) -> String {
    let mut out = String::new();
    for w in name.split_whitespace().take(2) {
        if let Some(c) = w.chars().find(|c| c.is_alphanumeric()) {
            out.extend(c.to_uppercase());
        }
    }
    if out.is_empty() {
        "?".into()
    } else {
        out
    }
}

/// Split "Name <addr>" into (name, addr).
pub fn split_address(s: &str) -> (String, String) {
    let s = s.trim();
    if let (Some(a), Some(b)) = (s.find('<'), s.rfind('>')) {
        if a < b {
            let name = s[..a].trim().trim_matches('"').to_string();
            let addr = s[a + 1..b].trim().to_string();
            return (if name.is_empty() { addr.clone() } else { name }, addr);
        }
    }
    (s.to_string(), s.to_string())
}

/// The start of a text in one line, for the message list.
pub fn snippet_of(body: &str) -> String {
    let mut out = String::new();
    for l in body.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('>')) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(l.trim());
        if out.len() > 400 {
            break;
        }
    }
    out.chars().take(160).collect()
}
