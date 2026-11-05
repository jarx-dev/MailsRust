use crate::calendar::{self, CalAction, CalView, EditorResult, EventEditor};
use crate::model::{split_address, Event, Folder, Message, Store};
use crate::net::{self, NetResult, Outgoing};
use crate::pl;
use crate::html;
use crate::theme::*;
use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Frame, Key, Layout, Margin, Pos2, Rect,
    RichText, Sense, Stroke, Ui, Vec2,
};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

mod compose;
use compose::*;

mod reading;
use reading::*;

mod list;
use list::*;

mod sidebar;
use sidebar::*;

mod sync;
pub use sync::{start_loading, Loading};

mod actions;
use actions::*;

mod contacts_ui;
use contacts_ui::*;

mod settings;

mod window;
use window::*;

fn id(salt: impl std::hash::Hash + std::fmt::Debug) -> egui::Id {
    #[allow(deprecated)]
    egui::Id::new(salt)
}

/// Layout choices the person made, remembered between runs.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Prefs {
    nav_width: f32,
    list_width: f32,
    side_width: f32,
    tags_open: bool,
    account_expanded: bool,
    show_side: bool,
    side_agenda: bool,
    cal_week: bool,
    /// Tag groups (e.g. "n8n") folded in the sidebar.
    folded_groups: Vec<String>,
    /// the dark theme
    dark: bool,
    /// Emails shared for AI agents over Tailscale: (link name, message id, shared at, unix seconds).
    ai_shares: Vec<(String, u64, i64)>,
    /// the port the sharing server used last time (links keep working after a restart)
    ai_port: u16,
    /// a sound and a note in the corner when new mail comes
    notify: bool,
    /// Google Calendar's secret iCal addresses (one per line), encrypted
    gcal_saved: String,
    /// the secret key written into the calendar script
    gcal_key: String,
    /// messages marked "Nie wymaga odpowiedzi" (left out of the list without replies)
    no_reply_needed: Vec<u64>,
    /// the calendar and the contacts shared with Google (through the Google sign-in)
    google_calendar: bool,
    google_contacts: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            nav_width: 250.0,
            list_width: 320.0,
            side_width: 300.0,
            tags_open: true,
            account_expanded: true,
            show_side: true,
            side_agenda: false,
            cal_week: false,
            folded_groups: vec![],
            ai_shares: vec![],
            dark: false,
            ai_port: 0,
            notify: true,
            gcal_saved: String::new(),
            gcal_key: String::new(),
            no_reply_needed: vec![],
            google_calendar: true,
            google_contacts: true,
        }
    }
}

fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Module {
    Mail,
    Calendar,
    Contacts,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SideMode {
    Day,
    Agenda,
}

/// New mail just received, told about in a note in the corner of the screen.
struct NewMail {
    ids: Vec<u64>,
}

/// A small outlined folder symbol for a group of tags.
/// The biggest part of `area` that `menu` does not cover (left, right, above or below it).
fn beside(area: Rect, menu: Rect) -> Rect {
    if !area.intersects(menu) {
        return area;
    }
    let parts = [
        Rect::from_min_max(area.min, Pos2::new(menu.left().min(area.right()), area.bottom())),
        Rect::from_min_max(Pos2::new(menu.right().max(area.left()), area.top()), area.max),
        Rect::from_min_max(area.min, Pos2::new(area.right(), menu.top().min(area.bottom()))),
        Rect::from_min_max(Pos2::new(area.left(), menu.bottom().max(area.top())), area.max),
    ];
    parts
        .into_iter()
        .filter(|p| p.width() > 0.0 && p.height() > 0.0)
        .max_by(|a, b| a.area().partial_cmp(&b.area()).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(Rect::NOTHING)
}

fn human_size(n: u64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / 1048576.0)
    } else if n >= 1024 {
        format!("{} KB", n / 1024)
    } else {
        format!("{n} B")
    }
}

/// A file name that Windows accepts.
fn safe_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c })
        .collect();
    let s = s.trim().trim_end_matches('.').to_string();
    if s.is_empty() { "załącznik".into() } else { s }
}

/// Opens a file or folder with the program Windows uses for it (no shell in between).
fn open_with_windows(path: &std::path::Path) {
    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
}

pub struct MailApp {
    store: Store,
    module: Module,
    view: MailView,
    selected: Option<u64>,
    search: String,
    scope: Scope,
    /// the conversation of the open message: (message, mailbox fingerprint, other messages oldest first)
    thread: std::cell::RefCell<Option<(u64, u64, Arc<Vec<u64>>)>>,
    /// the message whose whole conversation is unfolded
    thread_open: Option<u64>,
    /// earlier messages of the conversation unfolded in place, and whether the open message
    /// itself is folded to one line (both for `thread_for`)
    thread_unfolded: std::collections::HashSet<u64>,
    main_folded: bool,
    thread_for: Option<u64>,
    show_side: bool,
    side_mode: SideMode,
    side_day: NaiveDate,
    side_picker: bool,
    account_expanded: bool,
    composers: Vec<Compose>,
    /// messages opened in windows of their own
    readers: Vec<ReaderWin>,
    /// attachments shown in windows of their own
    previews: Vec<PreviewWin>,
    /// address suggestions for the recipients boxes, and the mailbox size they were made for
    contacts: Arc<Vec<crate::contacts::Contact>>,
    /// the address book (people and groups, in step with Google Contacts)
    book: crate::book::Book,
    /// shown: None = everyone, Some(MAIL_PEOPLE) = people from the mailbox, else a group
    book_group: Option<String>,
    /// the person being looked at / edited
    book_edit: Option<PersonEdit>,
    book_search: String,
    book_new_group: Option<String>,
    book_rename: Option<String>,
    book_add: String,
    book_rx: Option<Receiver<Result<crate::book::Synced, String>>>,
    book_last: f64,
    book_said: Option<Result<String, String>>,
    contacts_sig: (usize, u64),
    next_key: u64,
    account_open: bool,
    /// the windows with the AI settings and with the Google Calendar
    ai_open: bool,
    gcal_open: bool,
    /// „Popraw z AI” settings while the account window is open
    ai_set: Option<crate::ai::Settings>,
    /// checking the proxy: (answer or error, models offered)
    ai_check: Option<std::sync::mpsc::Receiver<(Result<String, String>, Vec<String>)>>,
    ai_said: Option<Result<String, String>>,
    ai_models: Vec<String>,
    diag_open: bool,
    diag_testing: std::sync::Arc<std::sync::atomic::AtomicBool>,
    status: String,
    status_at: f64,
    busy: usize,
    tx: Sender<NetResult>,
    rx: Receiver<NetResult>,
    cal_view: CalView,
    cal_day: NaiveDate,
    mini_shown: NaiveDate,
    editor: Option<EventEditor>,
    actions: Vec<Action>,
    shot: Option<PathBuf>,
    frames: u32,
    shot_image: ShotState,
    images: std::collections::HashMap<String, ImgState>,
    img_tx: Sender<(String, Option<Arc<[u8]>>)>,
    img_rx: Receiver<(String, Option<Arc<[u8]>>)>,
    html_cache: Option<(u64, Arc<Vec<html::Block>>)>,
    signing_in: bool,
    save_now: bool,
    mail_view: crate::webview::MailView,
    web_target: Option<(u64, egui::Rect)>,
    /// the open message's conversation: its newer messages (shown folded below it)
    thread_later: Vec<u64>,
    /// when the email view was last clicked, and since when it has had the keyboard without one
    email_clicked: f64,
    kb_stray: Option<f64>,
    /// formatted emails drawn by the app itself (see htmlview.rs): the drawing thread, what was
    /// asked for last (key, message), the picture shown, the link under the pointer
    email_engine: Option<crate::htmlview::Engine>,
    email_asked: (u64, u64),
    email_pic: Option<EmailPic>,
    email_hover: (u64, Option<String>, f64),
    /// the picture of the email shown under an open menu: (message, picture), and whether a
    /// menu was open in the frame before
    mail_shot: Option<(u64, egui::TextureHandle)>,
    menu_open: bool,
    /// what the next message window starts with (sending a message again)
    compose_start: Option<String>,
    /// messages translated into Polish (their translated page), those shown translated now,
    /// translations on their way, and the last problem
    translations: HashMap<u64, String>,
    show_translated: HashSet<u64>,
    translating: HashMap<u64, Receiver<Result<String, String>>>,
    translate_error: Option<(u64, String)>,
    list_rect: Option<egui::Rect>,
    label_filter: String,
    tags_open: bool,
    /// changes to make on the servers: (account, change)
    pending_ops: Vec<(u32, net::Op)>,
    /// accounts whose sync has not finished yet
    receiving_left: usize,
    /// the message in the reading pane: (which, since when, already dealt with) — after 3 seconds
    /// on screen it counts as read
    read_timer: Option<(u64, f64, bool)>,
    /// test: going through messages one after another (how many shown, when the last one was)
    nav_test: Option<(usize, f64)>,
    /// the account whose folders are shown (0: the main one)
    view_account: u32,
    /// the account of the next message window (a reply goes from the account it came to)
    compose_account: u32,
    /// adding or changing a further account: (which, its settings being edited)
    extra_edit: Option<(Option<usize>, crate::model::Account)>,
    /// which further accounts have their folders folded in the left bar
    extra_folded: HashSet<usize>,
    ops_running: bool,
    new_tag: String,
    receiving: bool,
    quiet_sync: bool,
    last_sync: f64,
    /// The copied people (DW) of older mail were asked for in this session (see `net::fill_copies`).
    copies_asked: bool,
    prefs: Prefs,
    finder: crate::finder::Finder,
    db: Option<crate::db::Db>,
    /// big messages whose full text is being downloaded (by server number)
    fetching_full: std::collections::HashSet<(Folder, u32)>,
    sign_in_error: Option<String>,
    oauth_setup: bool,
    last_scan: f64,
    icon: Arc<egui::IconData>,
    ctx: egui::Context,
    /// a short fingerprint of the mailbox; when it changes, the cached lists are made again
    msg_sig: u64,
    /// the same, but only what conversations depend on (not read / flagged / tags)
    struct_sig: u64,
    /// conversations: subject without "Re:" etc. → message ids; made in the background
    topics: Option<(u64, Arc<HashMap<String, Vec<u64>>>)>,
    topics_rx: Option<Receiver<(u64, HashMap<String, Vec<u64>>, HashMap<u64, NaiveDateTime>, HashMap<u64, u64>)>>,
    /// messages you forwarded: the message -> your forward of it (same conversation)
    forwarded: Arc<HashMap<u64, u64>>,
    /// received messages answered (by a later message of yours to their sender in the same
    /// conversation): when; with a number that changes whenever this is worked out again
    answered: (u64, Arc<HashMap<u64, NaiveDateTime>>),
    /// how many wait for a reply (for the left bar), for the state it was counted in
    noreply_count: (u64, usize),
    /// message id → its place in the mailbox
    index: HashMap<u64, usize>,
    /// the last few lists shown (going back to one, e.g. after a search, is instant)
    vis: std::cell::RefCell<Vec<Arc<VisList>>>,
    counts: Option<(u64, Arc<Counts>)>,
    /// the mailbox being read from disk in the background at startup
    loading: Option<Loading>,
    /// only the newest messages are in `store` yet (the rest still being read); their texts
    preview: Option<crate::db::TextReader>,
    /// looking for the signature in sent mail (background), and how many sent messages were looked at
    sig_rx: Option<Receiver<Option<crate::signature::Found>>>,
    sig_seen: usize,
    /// formatted versions of conversation messages unfolded in place, by their file (the file
    /// name follows its content, so a draft saved again gets a new entry)
    card_html: HashMap<String, Arc<Vec<html::Block>>>,
    /// the signature's formatted version, ready to show (for its text)
    sig_blocks: Option<(String, Arc<Vec<html::Block>>)>,
    /// another window of the app has the mailbox open: this one doesn't save anything
    read_only: bool,
    /// emails shared with AI agents over Tailscale
    hub: crate::share::Hub,
    /// the icon next to the clock, and what was clicked on it
    _tray: Option<tray_icon::TrayIcon>,
    tray_rx: Receiver<TrayMsg>,
    /// a verified newer build waiting to be swapped in (update.rs), and when the app was last touched
    update_rx: Option<Receiver<PathBuf>>,
    update_staged: Option<PathBuf>,
    last_input: f64,
    /// the taskbar's list of buttons (for the number on the program's button)
    taskbar: Option<crate::notify::Taskbar>,
    taskbar_tried: bool,
    /// the main window (to give it the keyboard back from the email view)
    main_hwnd: Option<isize>,
    /// the number of unread messages shown on the icons, and when it was put there
    badge_shown: Option<usize>,
    badge_at: f64,
    /// the note about new mail in the corner of the screen
    popup: Option<NewMail>,
    /// a short, clearly visible note at the top of the window ("Skopiowano do schowka"): text,
    /// and when it appeared (the app's clock)
    flash: Option<(String, f64)>,
    /// clicks on that note (it lives on a thread of its own)
    popup_click: (Sender<()>, Receiver<()>),
    /// the connection waiting for new mail on the server: (which account, stop, signals)
    watch: Vec<(String, Arc<std::sync::atomic::AtomicBool>, Receiver<()>)>,
    /// the server said something changed: fetch as soon as the current work is done
    watch_due: bool,
    /// Google Calendar: its addresses, the reading on its way, when it was read, what came of it
    gcal_text: String,
    gcal_loaded: bool,
    gcal_rx: Option<Receiver<Result<Vec<Event>, String>>>,
    gcal_last: f64,
    gcal_said: Option<Result<String, String>>,
    /// "Sprawdź połączenie": the calendar and the contacts, each working or what is missing
    google_check: Option<(Result<String, String>, Result<String, String>)>,
    google_check_rx: Option<Receiver<(Result<String, String>, Result<String, String>)>>,
    /// events written to Google: (event here, made here and to be replaced by Google's copy, answer)
    gcal_push: Option<(Sender<(Option<u64>, Result<(), String>)>, Receiver<(Option<u64>, Result<(), String>)>)>,
    /// ending for good (not just closing the window)
    quitting: bool,
    /// the password that `password_saved` holds encrypted
    protected_pw: Option<String>,
    /// messages may have changed since the last save
    db_dirty: bool,
    /// something may have changed the messages since they were last looked through
    touched: bool,
    /// a click or key in the last frame (what it did shows in the next one)
    input_last: bool,
    last_check: f64,
    /// when the app was opened, and how many frames were drawn (for the opening time)
    opened: std::time::Instant,
    drawn: u64,
}

impl MailApp {
    /// `preload`: the mailbox, already being read since the program started (see `start_loading`).
    pub fn new(cc: &eframe::CreationContext<'_>, shot: Option<PathBuf>, preload: Option<Loading>) -> Self {
        // search runs in normal use (and in the search test); not while taking the check picture
        let search_on = (shot.is_none() || std::env::var("POCZTA_SEARCH_TEST").is_ok()) && std::env::var("POCZTA_NO_SEARCH").is_err();
        let t = std::time::Instant::now();
        apply(&cc.egui_ctx);
        crate::diag::took("startup: fonts and colours", t, "");
        // restarted by an update while it was minimized: it stays so (see update.rs)
        if std::env::args().any(|a| a == "--minimized") { cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)); }
        // The mailbox lives in the local database; the very first time, it is moved there from
        // the older single-file storage.
        let started = std::time::Instant::now();
        // (the speed test of the opening uses a copy of a real mailbox while taking its picture)
        let real = shot.is_none() || std::env::var("POCZTA_SHOT_REAL").is_ok();
        // The mailbox is read from the local database in the background, so the window
        // (with its icon) appears at once and fills in a moment later.
        let db: Option<crate::db::Db> = None;
        let (store, loading) = if real {
            let lrx = preload.unwrap_or_else(start_loading);
            let empty = Store { messages: vec![], events: vec![], account: Default::default(), has_real_mail: true, ..Store::sample() };
            (empty, Some(lrx))
        } else {
            (Store::sample(), None)
        };
        let (tx, rx) = channel();
        let (img_tx, img_rx) = channel();
        let tray = if shot.is_none() { make_tray(&cc.egui_ctx) } else { (None, channel().1) };
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let today = Local::now().date_naive();
        let mut app = Self {
            store,
            module: Module::Mail,
            view: MailView::Folder(Folder::Inbox),
            selected: None,
            search: String::new(),
            scope: Scope::All,
            thread: Default::default(),
            thread_open: None,
            thread_unfolded: Default::default(),
            main_folded: false,
            thread_for: None,
            show_side: true,
            side_mode: SideMode::Day,
            side_day: today,
            side_picker: false,
            account_expanded: true,
            composers: vec![],
            readers: vec![],
            previews: vec![],
            contacts: Default::default(),
            contacts_sig: (0, 0),
            book: crate::book::Book::load(),
            book_group: None,
            book_edit: None,
            book_search: String::new(),
            book_new_group: None,
            book_rename: None,
            book_add: String::new(),
            book_rx: None,
            book_last: -1e9,
            book_said: None,
            next_key: 1,
            account_open: false,
            ai_open: false,
            gcal_open: false,
            ai_set: None,
            ai_check: None,
            ai_said: None,
            ai_models: vec![],
            diag_open: false,
            diag_testing: Default::default(),
            status: String::new(),
            status_at: 0.0,
            busy: 0,
            tx,
            rx,
            cal_view: CalView::Month,
            cal_day: today,
            mini_shown: today,
            editor: None,
            actions: vec![],
            shot,
            frames: 0,
            shot_image: Default::default(),
            images: Default::default(),
            img_tx,
            img_rx,
            html_cache: None,
            signing_in: false,
            save_now: false,
            mail_view: Default::default(),
            web_target: None,
            thread_later: vec![],
            email_clicked: -10.0,
            kb_stray: None,
            email_engine: None,
            email_asked: (0, 0),
            email_pic: None,
            email_hover: (0, None, 0.0),
            mail_shot: None,
            menu_open: false,
            compose_start: None,
            translations: HashMap::new(),
            show_translated: HashSet::new(),
            translating: HashMap::new(),
            translate_error: None,
            list_rect: None,
            label_filter: String::new(),
            tags_open: true,
            pending_ops: vec![],
            receiving_left: 0,
            read_timer: None,
            nav_test: None,
            view_account: 0,
            compose_account: 0,
            extra_edit: None,
            extra_folded: HashSet::new(),
            ops_running: false,
            new_tag: String::new(),
            receiving: false,
            quiet_sync: false,
            last_sync: -1.0,
            copies_asked: false,
            prefs: Prefs::default(),
            finder: crate::finder::Finder::new(search_on),
            db,
            fetching_full: Default::default(),
            sign_in_error: None,
            oauth_setup: false,
            last_scan: 0.0,
            icon: crate::icon::icon_data(),
            ctx: cc.egui_ctx.clone(),
            opened: started,
            drawn: 0,
            protected_pw: None,
            _tray: tray.0,
            tray_rx: tray.1,
            update_rx: None,
            update_staged: None,
            last_input: 0.0,
            taskbar: None,
            taskbar_tried: false,
            badge_shown: None,
            main_hwnd: None,
            badge_at: 0.0,
            popup: None,
            flash: None,
            popup_click: {
                let (tx, rx) = channel();
                // the click wakes the app (it may be minimized)
                (tx, rx)
            },
            watch: Vec::new(),
            watch_due: false,
            gcal_text: String::new(),
            gcal_loaded: false,
            gcal_rx: None,
            gcal_last: -1e9,
            gcal_said: None,
            google_check: None,
            google_check_rx: None,
            gcal_push: None,
            quitting: false,
            db_dirty: true,
            touched: true,
            input_last: false,
            last_check: 0.0,
            msg_sig: 0,
            struct_sig: 0,
            topics: None,
            topics_rx: None,
            forwarded: Arc::new(HashMap::new()),
            answered: (0, Arc::new(HashMap::new())),
            noreply_count: (u64::MAX, 0),
            index: HashMap::new(),
            vis: Default::default(),
            counts: None,
            loading,
            preview: None,
            sig_rx: None,
            sig_seen: 0,
            card_html: HashMap::new(),
            sig_blocks: None,
            read_only: false,
            hub: crate::share::Hub::new(Some(cc.egui_ctx.clone())),
        };
        // Panel sizes come from the app's own saved layout, not from the toolkit's older copy.
        cc.egui_ctx.memory_mut(|mem| mem.data.clear());
        // The layout the person left the app in last time.
        if app.shot.is_none() {
            if let Some(p) = cc.storage.and_then(|s| eframe::get_value::<Prefs>(s, "prefs")) {
                app.tags_open = p.tags_open;
                app.account_expanded = p.account_expanded;
                app.show_side = p.show_side;
                app.side_mode = if p.side_agenda { SideMode::Agenda } else { SideMode::Day };
                app.cal_view = if p.cal_week { CalView::Week } else { CalView::Month };
                if p.dark {
                    crate::theme::set_dark(true);
                    crate::theme::apply_colors(&cc.egui_ctx);
                }
                app.prefs = p;
                // widths saved while the window was minimized (tiny) are not kept
                if app.prefs.nav_width < 200.0 {
                    app.prefs.nav_width = Prefs::default().nav_width;
                }
                if app.prefs.list_width < 260.0 {
                    app.prefs.list_width = Prefs::default().list_width;
                }
                if app.prefs.side_width < 220.0 {
                    app.prefs.side_width = Prefs::default().side_width;
                }
            }
        }
        // A Google client set up once for this copy of the app makes sign-in a single click.
        if app.store.account.oauth_client_id.trim().is_empty() {
            if let Some((id, secret)) = crate::oauth::builtin_client() {
                app.store.account.oauth_client_id = id;
                app.store.account.oauth_client_secret = secret;
            }
        }
        // a key pressed inside the email wakes the app
        app.mail_view.wake = Some(cc.egui_ctx.clone());
        // test pictures: the AI settings, the Google Calendar window
        app.ai_open = std::env::var("POCZTA_START").as_deref() == Ok("ai-settings");
        app.gcal_open = matches!(std::env::var("POCZTA_START").as_deref(), Ok("account-calendar") | Ok("google-check"));
        app.gcal_open |= std::env::var("POCZTA_START").as_deref() == Ok("google-scopes");
        // (tests: a search typed at the start)
        if let Ok(q) = std::env::var("POCZTA_QUERY") {
            app.search = q;
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("account") {
            app.account_open = true;
            app.store.account.use_gmail();
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("diag") {
            app.diag_open = true;
            crate::diag::log("── sync started (incremental)");
            crate::diag::log("connect + sign in: 712 ms — imap.gmail.com");
            crate::diag::log("older mail: 200 of 1200 downloaded (14 messages/s)");
        }
        if matches!(std::env::var("POCZTA_START").as_deref(), Ok("rich") | Ok("rich-preview")) {
            // test picture of the writing window, drawn inside the main window
            cc.egui_ctx.set_embed_viewports(true);
            let png = std::fs::read("assets/poczta.png").unwrap_or_default();
            let pic = crate::rich::store_picture(&png, "png");
            let mut body = "## Podsumowanie zmian

Wpisy są **gotowe** i *sprawdzone*:
- SPF dla `naturbay.de`
- nowy klucz DKIM

To jest <span style=\"color:#c62828\">ważne</span> — proszę o <span style=\"font-size:18px\">potwierdzenie</span>.
".to_string();
            let pic2 = crate::rich::store_picture(&png, "png");
            for p in [&pic, &pic2].into_iter().flatten() {
                body.push_str(&format!("
{}
", crate::rich::image_tag(&p.0, p.2)));
            }
            app.open_composer("Anna Kowalska <anna.kowalska@example.com>".into(), String::new(), "Re: Wpisy DNS".into(), body, None, None);
            if let Some(c) = app.composers.last_mut() {
                for (cid, file, _) in [pic, pic2].into_iter().flatten() {
                    c.images.push((cid, file));
                }
                c.preview = std::env::var("POCZTA_START").as_deref() == Ok("rich-preview");
                c.focused = true;
            }
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("account-signature") {
            app.account_open = true;
            app.store.account.use_gmail();
            app.store.account.signature_text = "Z poważaniem
Aleks Nowicki
specjalista ds. Informatyki
tel.: 600-100-200".into();
            // laid out like a typical Gmail signature: round photo, name, small icons in the text
            app.store.account.signature_html = "<table style=\"width:600px;font-family:Arial,sans-serif;font-size:14px;color:rgb(51,51,51)\"><tbody><tr>\
                <td style=\"width:140px;vertical-align:top;text-align:center\"><div style=\"width:130px;height:130px;overflow:hidden;border-radius:50%\"><img src=\"cid:photo\"></div></td>\
                <td style=\"vertical-align:top;padding-left:10px\"><strong style=\"font-size:16px;color:rgb(0,91,170)\">Aleks <span style=\"color:rgb(198,40,40)\">Nowicki</span></strong><br>specjalista ds. Informatyki<br>Wsparcie IT<br><br>\
                <img src=\"cid:mail\" alt=\"email\" style=\"vertical-align: middle;\"> <a href=\"mailto:aleks@example.com\">aleks@example.com</a><br>\
                <img src=\"cid:phone\" alt=\"phone\" style=\"vertical-align: middle;\"> 600-100-200<br><br>\
                <a href=\"https://example.com\"><img src=\"cid:fb\" alt=\"Facebook\"> </a><a href=\"https://example.org\"><img src=\"cid:li\" alt=\"LinkedIn\"></a></td></tr>\
                <tr><td colspan=\"2\" style=\"padding-top:10px;font-size:11px;color:rgb(119,119,119)\">Wiadomość jest przeznaczona tylko dla jej adresata.</td></tr></tbody></table>".into();
            let icon = |n: u32, c: [u8; 4]| {
                let img = image::RgbaImage::from_fn(n, n, |x, y| {
                    let d = ((x as f32 - n as f32 / 2.0).powi(2) + (y as f32 - n as f32 / 2.0).powi(2)).sqrt();
                    if d < n as f32 / 2.0 - 1.0 { image::Rgba(c) } else { image::Rgba([0, 0, 0, 0]) }
                });
                let mut b = Vec::new();
                let _ = img.write_to(&mut std::io::Cursor::new(&mut b), image::ImageFormat::Png);
                crate::model::cache_put(&b, "png")
            };
            let png = crate::model::cache_put(&std::fs::read("assets/poczta.png").unwrap_or_default(), "png");
            app.store.account.signature_images = vec![
                ("photo".into(), png),
                ("mail".into(), icon(20, [30, 30, 30, 255])),
                ("phone".into(), icon(20, [30, 30, 30, 255])),
                ("fb".into(), icon(24, [24, 119, 242, 255])),
                ("li".into(), icon(24, [10, 102, 194, 255])),
            ];
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("account-setup") {
            app.account_open = true;
            app.store.account.use_gmail();
            app.oauth_setup = true;
        }
        if matches!(std::env::var("POCZTA_START").as_deref(), Ok("extra") | Ok("extra-dialog") | Ok("extra-unread")) || std::env::var("POCZTA_EXTRA").is_ok() {
            // test picture: a second mail account with a few messages of its own
            let (name, mail) = if std::env::var("POCZTA_EXTRA").is_ok() { ("Aleksandra Przykładowa-Kowalewska", "aleksandra.przykladowa-kowalewska@firma-przykladowa.pl") } else { ("Konto firmowe", "biuro@example.com") };
            let mut x = crate::model::Account { display_name: name.into(), email: mail.into(), username: mail.into(), password: "x".into(), ..Default::default() };
            x.use_gmail();
            x.use_oauth = false;
            app.store.extra.push(crate::model::ExtraAccount { account: x.clone(), ..Default::default() });
            let mine: Vec<crate::model::Message> = app.store.messages.iter().filter(|m| m.folder == Folder::Inbox).take(3).cloned().collect();
            for mut m in mine {
                m.id = app.store.new_id();
                m.account = 1;
                m.read = false;
                m.uid = None;
                m.subject = format!("[biuro] {}", m.subject);
                app.store.messages.push(m);
            }
            match std::env::var("POCZTA_START").as_deref() {
                Ok("extra-dialog") => app.extra_edit = Some((Some(0), x)),
                Ok("extra-unread") => app.view = MailView::Unread,
                _ => {
                    app.view = MailView::Folder(Folder::Inbox);
                    app.view_account = 1;
                    app.account_expanded = false;
                }
            }
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("cc-one") {
            // test picture: one person in "Do", one in "DW"
            if let Some(m) = app.store.messages.iter_mut().filter(|m| m.folder == Folder::Inbox).max_by_key(|m| m.date) {
                m.to = "Zbigniew Szczerbik <zbigniew@example.com>".into();
                m.cc = "Tomasz Ławniczak <tomasz@example.com>".into();
            }
        }
        if matches!(std::env::var("POCZTA_START").as_deref(), Ok("cc") | Ok("cc-open")) {
            // test picture: a message to two people, copied to seven
            if let Some(m) = app.store.messages.iter_mut().filter(|m| m.folder == Folder::Inbox).max_by_key(|m| m.date) {
                m.to = "Aleks Nowicki <aleks@example.com>, \"Wiśniewska, Marta\" <marta@example.com>".into();
                m.cc = ["Piotr Nowak <piotr@example.com>", "Ewa Lewandowska <ewa@example.com>", "Tomasz Zieliński <tomasz@example.com>", "Ola Kamińska <ola@example.com>", "jan@example.com", "Biuro <biuro@example.com>", "Kasia Wójcik <kasia@example.com>"].join(", ");
                if std::env::var("POCZTA_START").as_deref() == Ok("cc-open") {
                    let id = m.id;
                    cc.egui_ctx.data_mut(|d| d.insert_temp(egui::Id::new(("rcpt-open", id)), true));
                }
            }
        }
        if let Ok(path) = std::env::var("POCZTA_HTML") {
            // test picture: the open message shows this formatted text
            if let Some(m) = app.store.messages.iter_mut().filter(|m| m.folder == Folder::Inbox).max_by_key(|m| m.date) {
                m.html_path = path;
            }
        }
        if matches!(std::env::var("POCZTA_START").as_deref(), Ok("delete-unread") | Ok("delete-inbox")) {
            // test picture: the open message deleted; the next one of the list is shown
            if std::env::var("POCZTA_START").as_deref() == Ok("delete-unread") {
                app.view = MailView::Unread;
            }
            app.selected = None;
            app.selected = app.visible_ids().first().copied();
            if let Some(id) = app.selected {
                app.actions.push(Action::Delete(id));
            }
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("thread-later") {
            // test picture: a conversation with an earlier message and a newer reply of yours
            if let Some(base) = app.store.messages.iter().filter(|m| m.folder == Folder::Inbox).max_by_key(|m| m.date).cloned() {
                let mut before = base.clone();
                before.id = app.store.new_id();
                before.date = base.date - Duration::hours(3);
                before.body = "Wcześniejsza wiadomość tej rozmowy.".into();
                before.html_path = String::new();
                let mut after = base.clone();
                after.id = app.store.new_id();
                after.folder = Folder::Sent;
                after.date = base.date + Duration::hours(1);
                after.subject = format!("Re: {}", base.subject);
                after.from_name = app.store.account.display_name.clone();
                after.from_addr = app.store.account.email.clone();
                after.to = format!("{} <{}>", base.from_name, base.from_addr);
                after.body = "Dziękuję, będę w czwartek.".into();
                after.html_path = String::new();
                after.attachments.clear();
                app.store.messages.push(before);
                app.store.messages.push(after);
                app.selected = Some(base.id);
                app.thread_for = Some(base.id);
                app.main_folded = std::env::var("POCZTA_FOLD_MAIN").is_ok();
            }
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("mark-all") {
            // test picture: the inbox marked read as a whole (its number gone)
            app.actions.push(Action::MarkAllRead(Some(0), Some(Folder::Inbox)));
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("unread-kept") {
            // test picture: "Nieprzeczytane" with the open message read meanwhile (it stays listed)
            app.view = MailView::Unread;
            app.selected = None;
            app.selected = app.visible_ids().first().copied();
            if let Some(id) = app.selected {
                if let Some(m) = app.store.messages.iter_mut().find(|m| m.id == id) {
                    m.read = true;
                }
            }
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("many-files") {
            if let Some(m) = app.store.messages.iter_mut().filter(|m| m.folder == Folder::Inbox).max_by_key(|m| m.date) {
                m.attachments = (1..=7).map(|k| format!("Zestawienie operacji na rachunku nr _{k}5987927 za 2026-10 — bardzo długa nazwa pliku.pdf")).collect();
                m.attachment_files = vec![];
            }
        }
        if let Ok(q) = std::env::var("POCZTA_SEARCH") {
            app.search = q;
        }
        if std::env::var("POCZTA_DARK").is_ok() {
            // test picture of the dark theme
            app.prefs.dark = true;
            crate::theme::set_dark(true);
            crate::theme::apply_colors(&cc.egui_ctx);
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("folded") {
            app.show_side = false;
        }
        if std::env::var("POCZTA_TAG_TEST").is_ok() {
            let sets: [&[&str]; 4] = [
                &["\u{1F680}n8n/\u{1F4BC}Wymaga działania", "Natubay"],
                &["\u{1F680}n8n/\u{1F527} Support", "OTRS"],
                &["\u{1F680}n8n/\u{1F4B0}Faktury i rachunki"],
                &["\u{1F680}n8n/\u{1F4F0} Newsletter", "aptekiprima"],
            ];
            for (m, tags) in app.store.messages.iter_mut().filter(|m| m.folder == Folder::Inbox).zip(sets.iter().cycle()) {
                m.labels = tags.iter().map(|t| t.to_string()).collect();
            }
        }
        if std::env::var("POCZTA_LOGO_TEST").is_ok() {
            for (who, addr) in [("Zespół rozliczeń", "rozliczenia@wikipedia.org"), ("Dział IT", "it@mozilla.org")] {
                if let Some(m) = app.store.messages.iter_mut().find(|m| m.from_name == who) {
                    m.from_addr = addr.into();
                }
            }
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("thread") {
            // test picture: an earlier exchange about the same meeting
            if let Some(first) = app.store.messages.iter().find(|m| m.folder == Folder::Inbox).cloned() {
                let me = (app.store.account.display_name.clone(), app.store.account.email.clone());
                let lines = [
                    (true, "Cześć Anno, czy możemy umówić spotkanie startowe projektu w tym tygodniu?"),
                    (false, "Jasne, proponuję czwartek o 10:00. Przygotuję krótki plan."),
                    (true, "Czwartek pasuje. Zaproszę też Piotra i Martę."),
                    (false, "Super, dzięki. Wyślę plan dzień wcześniej."),
                    (true, "Dodaj proszę punkt o terminach pierwszego przeglądu."),
                ];
                for (i, (mine, text)) in lines.iter().enumerate() {
                    let mut m = first.clone();
                    m.id = app.store.new_id();
                    m.date = first.date - Duration::days(6 - i as i64) - Duration::hours(3);
                    m.subject = if i == 0 { first.subject.clone() } else { format!("RE: {}", first.subject) };
                    m.body = text.to_string();
                    m.html_path.clear();
                    m.attachments.clear();
                    m.labels.clear();
                    m.flagged = false;
                    m.read = true;
                    if i == 4 {
                        // a formatted one, to see it unfolded with its look
                        m.html_path = crate::model::cache_put(
                            "<p>Dodaj proszę punkt o <b>terminach pierwszego przeglądu</b>:</p><ul><li>wstępny harmonogram</li><li>kto przygotowuje dane</li></ul><p style=\"color:#1a6\">Dzięki!</p>".as_bytes(),
                            "html",
                        );
                    }
                    if *mine {
                        m.folder = Folder::Sent;
                        m.to = format!("{} <{}>", first.from_name, first.from_addr);
                        m.from_name = me.0.clone();
                        m.from_addr = me.1.clone();
                    }
                    app.store.messages.push(m);
                }
                // the newest earlier one unfolded, as after a click
                if let Some(last) = app.store.messages.last() {
                    app.thread_unfolded.insert(last.id);
                    app.thread_for = Some(first.id);
                    app.main_folded = std::env::var("POCZTA_FOLD_MAIN").is_ok();
                }
                if std::env::var("POCZTA_OPEN_OLDEST").is_ok() {
                    // test picture: the oldest one open, more newer ones below it than fit
                    let topic = thread_subject(&first.subject);
                    let oldest = app.store.messages.iter().filter(|m| thread_subject(&m.subject) == topic).min_by_key(|m| m.date).map(|m| m.id);
                    app.selected = oldest;
                    app.thread_for = oldest;
                    app.thread_unfolded.clear();
                }
            }
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("syncing") {
            app.busy = 1;
            app.receiving = true;
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("html") {
            app.selected = app.store.messages.iter().find(|m| !m.html_path.is_empty()).map(|m| m.id);
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("contacts") {
            // test picture: the contacts, the first group and its first person
            app.module = Module::Contacts;
            app.book_group = app.book.groups().first().map(|g| g.id.clone());
            if let Some(p) = app.book_group.clone().and_then(|g| app.book.members(&g).first().map(|p| (*p).clone())) {
                app.book_edit = Some(PersonEdit { id: p.id.clone(), name: p.name.clone(), emails: p.emails.join("
"), groups: p.groups.clone(), new: false });
            }
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("calendar") {
            app.module = Module::Calendar;
        }
        if app.selected.is_none() {
            app.selected = app.visible_ids().first().copied();
        }
        if let Some(id) = app.selected {
            if std::env::var("POCZTA_START").as_deref() == Ok("reply") {
                app.actions.push(Action::Reply(id, false));
            }
            if std::env::var("POCZTA_START").as_deref() == Ok("suggest") {
                app.open_composer("Ewa Kowalska <ewa@example.com>, m".into(), String::new(), String::new(), String::new(), None, None);
            }
        }
        app
    }

    /// Switches between the light and the dark theme (remembered for next time).
    fn set_dark(&mut self, on: bool) {
        self.prefs.dark = on;
        crate::theme::set_dark(on);
        crate::theme::apply_colors(&self.ctx);
        // the email shown, in its new colours
        self.mail_view.retheme();
        self.save_now = true;
        self.ctx.request_repaint();
    }

    /// Ends the app for good (from the tray icon or the menu).
    fn quit(&mut self) {
        self.quitting = true;
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Seconds since the app started (keeps going while the window is minimized, unlike the
    /// screen's own clock).
    fn clock(&self) -> f64 {
        self.opened.elapsed().as_secs_f64()
    }

    fn set_status(&mut self, s: impl Into<String>) {
        self.status = s.into();
        self.status_at = self.ctx.input(|i| i.time);
    }

    fn msg(&self, id: u64) -> Option<&Message> {
        if let Some(&i) = self.index.get(&id) {
            if let Some(m) = self.store.messages.get(i).filter(|m| m.id == id) {
                return Some(m);
            }
        }
        self.store.messages.iter().find(|m| m.id == id)
    }

    fn msg_mut(&mut self, id: u64) -> Option<&mut Message> {
        if let Some(&i) = self.index.get(&id) {
            if self.store.messages.get(i).is_some_and(|m| m.id == id) {
                return self.store.messages.get_mut(i);
            }
        }
        self.store.messages.iter_mut().find(|m| m.id == id)
    }

    /// Writes the mailbox and account to disk; the password only in its Windows-encrypted form.
    fn save_to(&mut self, storage: &mut dyn eframe::Storage) {
        // nothing to save before the mailbox has been read (it would overwrite it with nothing),
        // nor from a second window of the app
        if self.shot.is_some() || self.loading.is_some() || self.read_only {
            return;
        }
        // the password is encrypted again only when it changed (the encrypted form differs every
        // time, which would otherwise make every save look like a change)
        let a = &mut self.store.account;
        if self.protected_pw.as_deref() != Some(a.password.as_str()) {
            a.password_saved = crate::secret::protect(&a.password);
            self.protected_pw = Some(a.password.clone());
        }
        for x in self.store.extra.iter_mut() {
            let a = &mut x.account;
            if crate::secret::unprotect(&a.password_saved).as_deref() != Some(a.password.as_str()) {
                a.password_saved = crate::secret::protect(&a.password);
            }
        }
        // The mailbox goes into the local database, writing only what changed.
        match self.db.as_mut() {
            Some(db) => {
                let t = std::time::Instant::now();
                // the messages are looked through only if something may have changed them
                let dirty = std::mem::take(&mut self.db_dirty) || self.touched;
                // (the message being read keeps its text in memory; the others go back to disk)
                if let Err(e) = db.save(&mut self.store, dirty, self.selected) {
                    eprintln!("database: {e}");
                    crate::diag::log(format!("database error: {e}"));
                }
                crate::diag::took("app: save to database", t, format!("{} messages", self.store.messages.len()));
                // the old single-file copy is no longer needed: keep that file small
                eframe::set_value(storage, "store", &"moved to mail.redb");
            }
            None => eframe::set_value(storage, "store", &self.store),
        }
        // layout choices
        let p = &mut self.prefs;
        p.tags_open = self.tags_open;
        p.account_expanded = self.account_expanded;
        p.show_side = self.show_side;
        p.side_agenda = self.side_mode == SideMode::Agenda;
        p.cal_week = self.cal_view == CalView::Week;
        eframe::set_value(storage, "prefs", &self.prefs);
        self.finder.save(&self.store.messages);
        crate::diag::give_back_memory();
    }

}

impl eframe::App for MailApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let _timer = FrameTimer { start: std::time::Instant::now(), first: self.drawn == 0, opened: self.opened };
        self.drawn += 1;
        let _whole = crate::prof::span("frame (all)");
        {
            // speed measuring: the sums go to the log every 3 seconds
            use std::sync::atomic::{AtomicU64, Ordering};
            static LAST: AtomicU64 = AtomicU64::new(0);
            let now = self.opened.elapsed().as_secs() / 3;
            if LAST.swap(now, Ordering::Relaxed) != now {
                crate::prof::report();
                if std::env::var("POCZTA_PROFILE").is_ok() {
                    crate::diag::mem("now");
                }
            }
        }
        self.bench_step(&ctx);
        // closing the window only minimizes the app (checked here too: the request arrives with
        // the screen's own input)
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        self.web_target = None;
        if self.loading_screen(ui) { return; }
        if self.preview.is_some() {
            // only the newest messages: to look at until the whole mailbox is in (anything done
            // to them now would be lost when it takes their place)
            ui.disable();
            let _ = self.mail_view.keys();
        }
        // the same for the main window (the keyboard back from the email view)
        if self.main_hwnd.is_some_and(|h| focus_state(h) == "this window") && !ctx.input(|i| i.focused) {
            ctx.input_mut(|i| i.focused = true);
        }
        if self.preview.is_none() {
            self.read_after_a_while(&ctx);
        }
        self.nav_test_step(&ctx);
        {
            let _p = crate::prof::span("refresh caches");
            // clicks and keys (not mere mouse moves) may change messages, now or in the frame after
            let input = ctx.input(|i| i.events.iter().any(|e| !matches!(e, egui::Event::PointerMoved(_) | egui::Event::MouseMoved(_))));
            if input || self.input_last {
                self.touched = true;
            }
            self.input_last = input;
            self.refresh_caches();
        }
        if self.preview.is_none() {
            self.take_dropped_files(&ctx);
            self.keyboard(&ctx);
        }

        egui::Panel::top("top")
            .frame(Frame::new().fill(BG()).inner_margin(Margin { left: 8, right: 4, top: 6, bottom: 6 }))
            .show_separator_line(true)
            .show(ui, |ui| {
                let _p = crate::prof::span("top bar");
                self.top_bar(ui)
            });


        let nav = egui::Panel::left("nav")
            .default_size(self.prefs.nav_width)
            .size_range(150.0..=380.0)
            .resizable(true)
            .frame(Frame::new().fill(BG()).inner_margin(Margin { left: 6, right: 6, top: 0, bottom: 0 }))
            .show(ui, |ui| {
                let _p = crate::prof::span("left bar");
                self.nav_bar(ui)
            });
        if !minimized_or_tiny(&ctx) {
            self.prefs.nav_width = nav.response.rect.width();
        }
        if self.show_side {
            let side = egui::Panel::right("side")
                .default_size(self.prefs.side_width)
                .size_range(240.0..=420.0)
                .frame(Frame::new().fill(BG()))
                .show(ui, |ui| {
                    let _p = crate::prof::span("calendar side");
                    self.side_bar(ui)
                });
            if !minimized_or_tiny(&ctx) {
                self.prefs.side_width = side.response.rect.width();
            }
        } else {
            // Folded calendar: a slim strip at the edge; one click brings it back.
            egui::Panel::right("side-folded")
                .exact_size(38.0)
                .resizable(false)
                .frame(Frame::new().fill(BG()))
                .show(ui, |ui| {
                    ui.add_space(8.0);
                    ui.vertical_centered(|ui| {
                        if fold_button(ui, false).on_hover_text("Pokaż kalendarz").clicked() {
                            self.show_side = true;
                            self.save_now = true;
                        }
                        ui.add_space(6.0);
                        let (r, resp) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
                        if resp.hovered() {
                            ui.painter().rect_filled(r, CornerRadius::same(4), HOVER());
                        }
                        ui.painter().text(r.center(), Align2::CENTER_CENTER, "📅", FontId::proportional(17.0), MUTED());
                        if resp.on_hover_text("Pokaż kalendarz").clicked() {
                            self.show_side = true;
                            self.save_now = true;
                        }
                    });
                });
        }
        match self.module {
            Module::Mail => {
                let list = egui::Panel::left("list")
                    .default_size(self.prefs.list_width)
                    .size_range(260.0..=520.0)
                    .frame(Frame::new().fill(BG()))
                    .show(ui, |ui| {
                        let _p = crate::prof::span("message list");
                        self.message_list(ui)
                    });
                if !minimized_or_tiny(&ctx) {
                    self.prefs.list_width = list.response.rect.width();
                }
                egui::CentralPanel::default()
                    .frame(Frame::new().fill(BG()))
                    .show(ui, |ui| {
                        let _p = crate::prof::span("reading pane");
                        self.reading_pane(ui)
                    });
            }
            Module::Contacts => {
                egui::CentralPanel::default()
                    .frame(Frame::new().fill(BG()).inner_margin(Margin::symmetric(14, 10)))
                    .show(ui, |ui| self.contacts_main(ui));
            }
            Module::Calendar => {
                egui::CentralPanel::default()
                    .frame(Frame::new().fill(BG()).inner_margin(Margin::symmetric(14, 10)))
                    .show(ui, |ui| {
                        let today = Local::now().date_naive();
                        let title = match self.cal_view {
                            CalView::Month => pl::month_year(self.cal_day),
                            CalView::Week => {
                                let s = calendar::week_start(self.cal_day);
                                let e = s + Duration::days(6);
                                format!("{} – {} {}", pl::day_month(s), pl::day_month(e), e.year())
                            }
                        };
                        ui.label(RichText::new(title).font(semibold(18.0)).color(TEXT()));
                        ui.add_space(6.0);
                        let act = match self.cal_view {
                            CalView::Month => calendar::month_grid(ui, &self.store, self.cal_day, today),
                            CalView::Week => calendar::week_grid(ui, &self.store, self.cal_day, today),
                        };
                        if let Some(a) = act {
                            self.cal_action(a);
                        }
                    });
            }
        }

        self.composers_ui(&ctx);
        self.readers_ui(&ctx);
        self.previews_ui(&ctx);
        self.account_ui(&ctx);
        self.diag_ui(&ctx);
        self.side_settings_ui(&ctx);
        self.extra_account_ui(&ctx);
        self.editor_ui(&ctx);
        window_edges(ui);
        self.toast(&ctx);
        self.flash_ui(&ctx);
        // The browser view sits on top of everything, so it steps aside while a menu or dialog is open.
        // (and in the background: an open writing window keeps the screen drawn, but the view stays closed)
        let covered = self.account_open || self.extra_edit.is_some() || self.ai_open || self.gcal_open || self.diag_open || self.editor.is_some() || self.module != Module::Mail || self.mail_view.in_background();
        let want = if covered {
            None
        } else {
            self.web_target.and_then(|(id, r)| {
                self.msg(id).cloned().map(|mut m| {
                    if let Some(page) = self.translations.get(&id).filter(|_| self.show_translated.contains(&id)) {
                        m.html_path = page.clone();
                    }
                    (m, r)
                })
            })
        };
        // A picture of the email arrived: ready to be shown under menus.
        let fresh = self.mail_view.shot.lock().ok().and_then(|mut g| g.take());
        if let Some((sid, png)) = fresh {
            if let Ok(img) = image::load_from_memory(&png) {
                let img = img.to_rgba8();
                let size = [img.width() as usize, img.height() as usize];
                let tex = ctx.load_texture("mail-shot", egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()), egui::TextureOptions::LINEAR);
                self.mail_shot = Some((sid, tex));
            }
        }
        // A menu open over the email: the browser view (always on top of the app's own drawing)
        // gives way to its picture, so the menu lies over the email and nothing moves. Without
        // a picture yet, the view steps aside for a moment.
        let menu = ctx.any_popup_open();
        let want = match want {
            Some((m, r)) if menu => {
                let menus: Vec<Rect> = ctx.memory(|mem| {
                    mem.areas()
                        .visible_layer_ids()
                        .into_iter()
                        .filter(|l| l.order == egui::Order::Foreground && l.id != id("sync-toast") && l.id != id("window-outline"))
                        .filter_map(|l| mem.area_rect(l.id))
                        .collect()
                });
                let over = menus.iter().any(|p| p.intersects(r));
                if !over {
                    Some((m, r))
                } else if self.mail_shot.as_ref().is_some_and(|(sid, _)| *sid == m.id) {
                    None
                } else {
                    let free = menus.iter().fold(r, |area, p| beside(area, p.expand(4.0)));
                    (free.width() >= 120.0 && free.height() >= 80.0).then_some((m, free))
                }
            }
            other => other,
        };
        if menu != self.menu_open {
            self.menu_open = menu;
            ctx.request_repaint();
        }
        // pictures kept fresh: when the email is shown and the pointer moves over the app's
        // own part (on its way to a button or a list), at most about once a second
        if !menu && want.is_some() && self.mail_view.shown().is_some() && ctx.input(|i| i.pointer.is_moving()) {
            self.mail_view.capture();
        }
        let p_web = crate::prof::span("mail view (browser)");
        self.mail_view.sync(frame, want.as_ref().map(|(m, r)| (m, *r)));
        drop(p_web);
        self.apply_actions();
        self.screenshot_step(&ctx);

    }

    /// Runs before every screen and also while the window is minimized: news from the network,
    /// saving, the regular check for new mail, the tray icon, and closing → minimizing.
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.poll_loading(frame);
        {
            let _p = crate::prof::span("poll network");
            self.poll_net();
        }
        self.poll_ai_drafts();
        let done: Vec<(u64, Result<String, String>)> = self.translating.iter().filter_map(|(id, rx)| rx.try_recv().ok().map(|r| (*id, r))).collect();
        for (id, res) in done {
            self.translating.remove(&id);
            match res {
                Ok(page) => {
                    self.translations.insert(id, page);
                    self.show_translated.insert(id);
                    self.mail_view.reload();
                }
                Err(e) => {
                    crate::diag::log(format!("translation: {e}"));
                    self.translate_error = Some((id, e));
                }
            }
            self.touched = true;
        }
        self.update_badges(frame);
        if std::env::var("POCZTA_START").as_deref() == Ok("google-scopes") && self.loading.is_none() && self.store.account.oauth_scopes.is_empty() {
            // test picture: signed in with Google for the mail only (calendar and contacts not allowed yet)
                        let a = &mut self.store.account;
            a.use_oauth = true;
            a.oauth_refresh_token = "test".into();
            a.oauth_client_id = "test.apps.googleusercontent.com".into();
            a.email = "jan.kowalski@example.com".into();
            a.oauth_scopes = "https://mail.google.com/ openid email profile".into();
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("translate") && self.loading.is_none() && self.clock() > 3.0 {
            use std::sync::atomic::{AtomicBool, Ordering};
            static ASKED: AtomicBool = AtomicBool::new(false);
            if !ASKED.swap(true, Ordering::Relaxed) {
                // test: the message shown, translated
                if let Some(id) = self.selected {
                    self.actions.push(Action::Translate(id));
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("resend") && self.loading.is_none() {
            use std::sync::atomic::{AtomicBool, Ordering};
            static SENT: AtomicBool = AtomicBool::new(false);
            if !SENT.swap(true, Ordering::Relaxed) {
                // test: a sent message opened to be sent again
                if let Some(id) = self.store.messages.iter().find(|m| m.folder == Folder::Sent).map(|m| m.id) {
                    self.actions.push(Action::SendAgain(id));
                }
            }
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("noreply") && self.loading.is_none() {
            use std::sync::atomic::{AtomicU8, Ordering};
            static STEP: AtomicU8 = AtomicU8::new(0);
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
            if STEP.load(Ordering::Relaxed) == 0 {
                // test: the list without replies, the first one marked as needing no reply later
                STEP.store(1, Ordering::Relaxed);
                self.view = MailView::NoReply;
                self.selected = self.visible_ids().first().copied();
            } else if STEP.load(Ordering::Relaxed) == 1 && self.clock() > 25.0 {
                STEP.store(2, Ordering::Relaxed);
                crate::diag::log(format!("noreplytest: before {} waiting", self.visible_ids().len()));
                self.selected = self.visible_ids().first().copied();
                if let Some(id) = self.selected {
                    self.actions.push(Action::NoReplyNeeded(id, true));
                }
            } else if STEP.load(Ordering::Relaxed) == 2 && self.clock() > 27.0 {
                STEP.store(3, Ordering::Relaxed);
                crate::diag::log(format!("noreplytest: after {} waiting", self.visible_ids().len()));
            }
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("flash") && self.loading.is_none() && self.clock() > 3.0 && self.flash.is_none() {
            use std::sync::atomic::{AtomicBool, Ordering};
            static SHOWN: AtomicBool = AtomicBool::new(false);
            if !SHOWN.swap(true, Ordering::Relaxed) {
                // test picture: the note after copying a link
                self.flash("Link dla AI skopiowany do schowka — wklej go agentowi (działa w Twojej sieci Tailscale przez 7 dni)");
            }
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("htmlfirst") && self.loading.is_none() && self.clock() > 6.0 {
            use std::sync::atomic::{AtomicBool, Ordering};
            static PRESSED: AtomicBool = AtomicBool::new(false);
            if !PRESSED.swap(true, Ordering::Relaxed) {
                // test: Delete pressed inside the email
                crate::diag::log(format!("keytest: selected before {:?}", self.selected));
                self.mail_view.run_js("document.body.dispatchEvent(new KeyboardEvent('keydown',{key:'Delete',bubbles:true}))");
            }
        }
        if let Ok(file) = std::env::var("POCZTA_PREVIEW_TEST") {
            // test picture: an attachment in its preview window
            if self.previews.is_empty() && self.loading.is_none() && self.clock() > 2.0 {
                use std::sync::atomic::{AtomicBool, Ordering};
                static DONE: AtomicBool = AtomicBool::new(false);
                if !DONE.swap(true, Ordering::Relaxed) {
                    let key = self.next_key;
                    self.next_key += 1;
                    let name = std::path::Path::new(&file).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    self.previews.push(PreviewWin { key, msg: 0, index: 0, name, path: file, web: None, failed: false, tries: 0, last: None });
                }
            }
        }
        if std::env::var("POCZTA_START").as_deref() == Ok("menu") && self.loading.is_none() && self.clock() > 3.0 && self.mail_shot.is_none() {
            // test: a picture of the email taken before the menu opens (as when the pointer moves)
            self.mail_view.capture();
        }
        if matches!(std::env::var("POCZTA_START").as_deref(), Ok("menu") | Ok("htmlfirst")) && self.loading.is_none() {
            use std::sync::atomic::{AtomicBool, Ordering};
            static SET: AtomicBool = AtomicBool::new(false);
            if !SET.swap(true, Ordering::Relaxed) {
                // test: the shown message as a formatted email, with the menu opened over it later
                let page = "<html><body style='font-family:Segoe UI;margin:24px'><h2>Asystent Farmaceuty</h2><p style='color:#1e6fd9'>Ta treść powinna być widoczna obok otwartego menu.</p><p>".to_string() + &"Linia tekstu. ".repeat(80) + "</p></body></html>";
                let file = crate::model::cache_put(page.as_bytes(), "html");
                let first = self.visible_ids().first().copied();
                if let Some(m) = first.and_then(|id| self.msg_mut(id)) {
                    m.html_path = file;
                }
                self.selected = first;
                self.touched = true;
                self.read_only = true; // the swapped text is never saved into the test's mailbox
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }
        self.gcal_tick();
        self.book_tick();
        if std::env::var("POCZTA_NOTIFY_TEST").is_ok() {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        if std::env::var("POCZTA_NOTIFY_TEST").as_deref() == Ok("min2") && self.loading.is_none() && self.clock() > 2.0 {
            use std::sync::atomic::{AtomicBool, Ordering};
            static MINIMIZED: AtomicBool = AtomicBool::new(false);
            if !MINIMIZED.swap(true, Ordering::Relaxed) {
                // test: minimized first, the mail comes a few seconds later
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
        }
        if std::env::var("POCZTA_NOTIFY_TEST").is_ok() && self.loading.is_none() && self.clock() > if std::env::var("POCZTA_NOTIFY_TEST").as_deref() == Ok("min2") { 6.0 } else { 2.0 } {
            use std::sync::atomic::{AtomicBool, Ordering};
            static DONE: AtomicBool = AtomicBool::new(false);
            if !DONE.swap(true, Ordering::Relaxed) {
                // as if two messages came: the newest unread (or newest) mail of the inbox
                let mut ids: Vec<(NaiveDateTime, u64)> = self.store.messages.iter().filter(|m| m.folder == Folder::Inbox).map(|m| (m.date, m.id)).collect();
                ids.sort();
                let ids: Vec<u64> = ids.iter().rev().take(2).map(|x| x.1).collect();
                if std::env::var("POCZTA_NOTIFY_TEST").as_deref() == Ok("min") {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
                self.new_mail(ids);
            }
        }
        // a click on the note about new mail: the window in front with the newest new message
        if self.popup_click.1.try_iter().count() > 0 {
            if let Some(p) = self.popup.take() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                self.module = Module::Mail;
                self.view = MailView::Folder(Folder::Inbox);
                self.search.clear();
                if let Some(id) = p.ids.first() {
                    self.actions.push(Action::Select(*id));
                }
            }
        }
        self.poll_topics();
        self.check_signature();
        self.flush_ops();
        let allow_download = self.store.account.can_connect() && self.store.has_real_mail;
        if self.loading.is_none() {
            let _p = crate::prof::span("search upkeep");
            self.finder.tick(&self.store.messages, self.struct_sig, &self.search, allow_download, ctx);
        }
        self.auto_sync();
        self.watch_inbox();
        self.install_update(ctx);
        self.background_memory(ctx);
        if self.save_now {
            self.save_now = false;
            if let Some(storage) = frame.storage_mut() {
                self.save_to(storage);
                storage.flush();
            }
        }
        // speed test of closing: the app closes itself after the given time
        if let Some(ms) = std::env::var("POCZTA_CLOSE_AFTER_MS").ok().and_then(|v| v.parse::<u128>().ok()) {
            if self.opened.elapsed().as_millis() > ms && self.loading.is_none() {
                crate::diag::log("closing: asked to close");
                self.quit();
            }
            ctx.request_repaint_after(std::time::Duration::from_millis((ms.saturating_sub(self.opened.elapsed().as_millis()) as u64).clamp(50, 60_000)));
        }
        // the tray icon: show the window again, or quit for good
        while let Ok(t) = self.tray_rx.try_recv() {
            match t {
                TrayMsg::Show => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                TrayMsg::Quit => self.quit(),
            }
        }
        // closing the window (its button, Alt+F4, the taskbar) only minimizes the app; it keeps
        // receiving mail. It ends from the tray icon or the menu's "Zakończ".
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let t = std::time::Instant::now();
        self.save_to(storage);
        crate::diag::took("app: save (automatic or on closing)", t, "");
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Everything is saved by now (the toolkit saves just before this). Taking apart tens of
        // thousands of messages in memory and the browser view one by one only kept the window
        // on screen for another second or two, so the app ends here and Windows frees it all.
        // the database is closed properly (else it checks itself on the next start)
        self.finder.wait_saved();
        let t = std::time::Instant::now();
        let closed = self.db.take().is_some_and(|d| d.close());
        crate::diag::took("closing: database closed", t, if closed { "" } else { "NOT closed (still in use)" });
        crate::diag::log("closing: saved, ending the app");
        std::process::exit(0);
    }

    /// The app remembers its own layout (see `Prefs`), so the toolkit's separate copy of
    /// panel sizes isn't kept; one source of truth means the layout always comes back as left.
    fn persist_egui_memory(&self) -> bool {
        false
    }
}

/// Notes slow screen redraws (and how long the first one took) in the diagnostics log.
struct FrameTimer {
    start: std::time::Instant,
    first: bool,
    opened: std::time::Instant,
}

impl Drop for FrameTimer {
    fn drop(&mut self) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NOTED: AtomicU64 = AtomicU64::new(0);
        let ms = self.start.elapsed().as_millis() as u64;
        if self.first {
            crate::diag::log(format!("startup: first screen drawn in {ms} ms, {} ms after opening", self.opened.elapsed().as_millis()));
            return;
        }
        // at most one note every 5 seconds
        let now = self.opened.elapsed().as_secs();
        if ms >= 100 && now >= NOTED.load(Ordering::Relaxed) + 5 {
            NOTED.store(now, Ordering::Relaxed);
            crate::diag::log(format!("slow screen redraw: {ms} ms"));
        }
    }
}
