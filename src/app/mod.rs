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

/// A person of the address book being edited.
#[derive(Clone)]
struct PersonEdit {
    id: String,
    name: String,
    /// one address per line
    emails: String,
    groups: Vec<String>,
    /// not in the book yet (e.g. someone from the mailbox)
    new: bool,
}

/// The list of people found in the mailbox (not saved in the address book).
const MAIL_PEOPLE: &str = "__mail";

enum Action {
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
    /// looking for the signature in sent mail (background), and how many sent messages were looked at
    sig_rx: Option<Receiver<Option<crate::signature::Found>>>,
    sig_seen: usize,
    /// formatted versions of conversation messages unfolded in place
    card_html: HashMap<u64, Arc<Vec<html::Block>>>,
    /// the signature's formatted version, ready to show (for its text)
    sig_blocks: Option<(String, Arc<Vec<html::Block>>)>,
    /// another window of the app has the mailbox open: this one doesn't save anything
    read_only: bool,
    /// emails shared with AI agents over Tailscale
    hub: crate::share::Hub,
    /// the icon next to the clock, and what was clicked on it
    _tray: Option<tray_icon::TrayIcon>,
    tray_rx: Receiver<TrayMsg>,
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
    watch: Option<(String, Arc<std::sync::atomic::AtomicBool>, Receiver<()>)>,
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
            watch: None,
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

    /// Takes over the mailbox once it has been read from disk (see `new`).
    fn poll_loading(&mut self, frame: &eframe::Frame) {
        let Some(rx) = &self.loading else { return };
        let Ok((db, from_db, entries)) = rx.try_recv() else {
            // meanwhile, get the browser view for formatted mail ready (it takes a moment to
            // start), from the second frame on so the window itself shows first
            // (only for the browser view kept as a fallback: emails are drawn by the app)
            if self.drawn >= 2 && self.shot.is_none() && std::env::var("POCZTA_BLITZ").is_err() {
                self.mail_view.prepare(frame);
            }
            self.ctx.request_repaint_after(std::time::Duration::from_millis(30));
            return;
        };
        self.loading = None;
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

    /// "Copy link for AI": shares the email on this computer's Tailscale address and copies a
    /// short note with the link. Without Tailscale, the email's text is copied instead.
    fn copy_for_ai(&mut self, id: u64) {
        let Some(m) = self.msg(id).cloned() else { return };
        let port = self.prefs.ai_port;
        let started = crate::share::tailscale_ip()
            .ok_or_else(|| "Tailscale nie jest włączony na tym komputerze — włącz go i spróbuj ponownie".to_string())
            .and_then(|ip| self.hub.start(ip, port));
        match started {
            Err(e) => {
                // no link without Tailscale: say why, rather than copying something else
                crate::diag::log(format!("copy for AI: {e}"));
                self.set_status(format!("Nie udało się utworzyć linku: {e}"));
                return;
            }
            Ok(addr) if addr.port() != port => {
                self.prefs.ai_port = addr.port();
                self.save_now = true;
            }
            Ok(_) => {}
        }
        let now = Local::now().timestamp();
        // the same email keeps its link while it is valid
        let token = match self.prefs.ai_shares.iter().find(|s| s.1 == id && now - s.2 < crate::share::DAYS * 86400) {
            Some(s) => s.0.clone(),
            None => {
                let t = crate::share::new_token();
                self.prefs.ai_shares.push((t.clone(), id, now));
                self.save_now = true;
                t
            }
        };
        self.hub.set_account(&self.store.account);
        let shared = crate::share::Shared::of(&m);
        if !m.partial {
            self.hub.warm(&shared);
        }
        self.hub.put(&token, shared);
        // the link is the whole email as a .eml file (with attachments and pictures)
        let Some(url) = self.hub.url(&token).map(|u| format!("{u}.eml")) else { return };
        // just the one link: the page behind it lists everything else (attachments, pictures,
        // formatted version, how to leave a reply draft)
        self.ctx.copy_text(url);
        self.flash("Link dla AI skopiowany do schowka — wklej go agentowi (działa w Twojej sieci Tailscale przez 7 dni)");
        // a big message with only its header here: get the rest, the shared copy follows
        if m.partial && matches!(m.folder, Folder::Inbox | Folder::Sent) {
            if let Some(uid) = m.uid {
                if self.store.account.can_connect() && self.fetching_full.insert((m.folder, uid)) {
                    net::fetch_full(self.account_for(m.account), m.account, uid, m.folder, self.tx.clone(), self.ctx.clone());
                }
            }
        }
    }

    /// Links shared before (and not expired) work again after the app restarts.
    fn restore_ai_shares(&mut self) {
        let now = Local::now().timestamp();
        let ids: std::collections::HashSet<u64> = self.store.messages.iter().map(|m| m.id).collect();
        self.prefs.ai_shares.retain(|s| now - s.2 < crate::share::DAYS * 86400 && ids.contains(&s.1));
        if self.prefs.ai_shares.is_empty() || self.shot.is_some() {
            return;
        }
        let Some(ip) = crate::share::tailscale_ip() else { return };
        match self.hub.start(ip, self.prefs.ai_port) {
            Ok(addr) if addr.port() != self.prefs.ai_port => {
                // another program took the old port: the old links can't work any more
                self.prefs.ai_port = addr.port();
                self.prefs.ai_shares.clear();
                self.save_now = true;
                return;
            }
            Ok(_) => {}
            Err(_) => return,
        }
        self.hub.set_account(&self.store.account);
        for (token, id, _) in self.prefs.ai_shares.clone() {
            if let Some(m) = self.msg(id) {
                self.hub.put(&token, crate::share::Shared::of(m));
            }
        }
    }

    /// Reply drafts written by an AI agent: they go to Drafts, ready to check and send.
    fn poll_ai_drafts(&mut self) {
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

    /// Speed test (POCZTA_BENCH): goes through messages one after another, searches, comes
    /// back, as a person would, and writes the measured times to the log every 3 seconds.
    fn bench_step(&mut self, ctx: &egui::Context) {
        if std::env::var("POCZTA_BENCH").is_err() || self.loading.is_some() {
            return;
        }
        ctx.request_repaint();
        let n = self.drawn;
        if n % 6 == 0 && self.search.is_empty() {
            let ids = self.visible_ids();
            if !ids.is_empty() {
                let k = ((n / 6) as usize) % ids.len().min(60);
                self.actions.push(Action::Select(ids[k]));
            }
        }
        match n % 600 {
            300 => self.search = "faktura".into(),
            420 => self.search = "spotkanie w czwartek".into(),
            500 => self.search.clear(),
            _ => {}
        }

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

    /// New unread mail came: a sound, the taskbar button blinks, and (when the window is not in
    /// front) a note in the corner of the screen.
    fn new_mail(&mut self, ids: Vec<u64>) {
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

    /// The number of unread messages in the inbox on the program's icons: the tray icon (with
    /// its hint) and the taskbar button.
    fn update_badges(&mut self, frame: &mut eframe::Frame) {
        if self.loading.is_some() {
            return;
        }
        let unread = self.counts().inbox_unread_all;
        // the taskbar forgets the sign when its button is made again (e.g. after Explorer restarts)
        let again = self.clock() - self.badge_at > 30.0;
        if self.badge_shown == Some(unread) && !again {
            return;
        }
        let changed = self.badge_shown != Some(unread);
        if let (Ok(_), Ok(dir)) = (std::env::var("POCZTA_NOTIFY_TEST"), std::env::var("POCZTA_DATA_DIR")) {
            // test: the pictures put on the icons, to look at
            let _ = crate::notify::icon_with_count(32, unread).save(std::path::Path::new(&dir).join("badge-tray.png"));
            let _ = crate::notify::badge_only(32, unread).save(std::path::Path::new(&dir).join("badge-taskbar.png"));
            let _ = crate::notify::icon_with_count(32, 128).save(std::path::Path::new(&dir).join("badge-tray-128.png"));
        }
        self.badge_shown = Some(unread);
        self.badge_at = self.clock();
        if changed {
            if let Some(t) = &self._tray {
                let img = crate::notify::icon_with_count(32, unread);
                if let Ok(i) = tray_icon::Icon::from_rgba(img.into_raw(), 32, 32) {
                    let _ = t.set_icon(Some(i));
                }
                let tip = if unread == 0 { "Poczta".to_string() } else { format!("Poczta — {}", crate::notify::unread_text(unread)) };
                let _ = t.set_tooltip(Some(tip));
            }
        }
        #[cfg(windows)]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let hwnd = match frame.window_handle().map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
                _ => return,
            };
            self.main_hwnd = Some(hwnd);
            if self.taskbar.is_none() && !self.taskbar_tried {
                self.taskbar_tried = true;
                self.taskbar = crate::notify::Taskbar::new();
            }
            if let Some(tb) = &self.taskbar {
                if unread == 0 {
                    tb.overlay(hwnd, None, "");
                } else {
                    let png = crate::notify::png(&crate::notify::badge_only(32, unread));
                    tb.overlay(hwnd, Some(&png), &crate::notify::unread_text(unread));
                }
            }
        }
        #[cfg(not(windows))]
        let _ = frame;
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

    /// The other messages of the same conversation (received and sent), oldest first: same
    /// subject without "Re:", "Fwd:" and the like, and at least one person in common.
    /// Groups all messages by conversation subject, on another thread (it takes a moment with
    /// tens of thousands of messages); opening a message then finds its conversation at once.
    fn start_topics(&mut self) {
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

    fn apply_actions(&mut self) {
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
    fn neighbour(&self, id: u64) -> Option<u64> {
        let ids = self.visible_ids();
        let pos = ids.iter().position(|x| *x == id)?;
        ids.get(pos + 1).or_else(|| pos.checked_sub(1).and_then(|p| ids.get(p))).copied()
    }

    /// Shows the message chosen next in the reading pane (the list keeps the keyboard; it stays unread until read).
    fn go_to_neighbour(&mut self, next: Option<u64>) {
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
    fn nav_test_step(&mut self, ctx: &egui::Context) {
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

    /// The settings of an account (0: the main one).
    fn account_ref(&self, a: u32) -> &crate::model::Account {
        if a == 0 {
            &self.store.account
        } else {
            self.store.extra.get(a as usize - 1).map(|x| &x.account).unwrap_or(&self.store.account)
        }
    }

    fn account_for(&self, a: u32) -> crate::model::Account {
        self.account_ref(a).clone()
    }

    /// How far a folder of an account is synced.
    fn sync_slot(&mut self, a: u32, f: Folder) -> &mut Option<net::SyncState> {
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
    fn start_receive(&mut self, a: u32) {
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
    fn queue_op(&mut self, id: u64, make: impl FnOnce(u32) -> net::Op) {
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
    fn flush_ops(&mut self) {
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

    /// A small floating bubble at the bottom of the message list: "syncing…" while mail is being
    /// sent or received, then the result for a few seconds (longer for problems).
    /// A short note at the top of the window, e.g. after copying something.
    fn flash(&mut self, text: &str) {
        self.flash = Some((text.to_string(), self.clock()));
        self.ctx.request_repaint();
    }

    /// The note at the top centre of the window (above everything, the email view does not
    /// cover it there), for three seconds.
    fn flash_ui(&mut self, ctx: &egui::Context) {
        let Some((text, at)) = self.flash.clone() else { return };
        const KEEP: f64 = 3.0;
        let age = self.clock() - at;
        if age > KEEP {
            self.flash = None;
            return;
        }
        let alpha = ((KEEP - age) / 0.4).clamp(0.0, 1.0) as f32;
        let s = ctx.content_rect();
        egui::Area::new(id("flash"))
            .order(egui::Order::Tooltip)
            .fixed_pos(Pos2::new(s.center().x, s.top() + 74.0))
            .pivot(Align2::CENTER_TOP)
            .interactable(false)
            .show(ctx, |ui| {
                ui.set_opacity(alpha);
                Frame::new()
                    .fill(Color32::from_rgb(0x1F, 0x8A, 0x4C))
                    .corner_radius(CornerRadius::same(20))
                    .inner_margin(Margin::symmetric(18, 10))
                    .shadow(egui::epaint::Shadow { offset: [0, 3], blur: 16, spread: 0, color: Color32::from_black_alpha(60) })
                    .show(ui, |ui| {
                        ui.set_max_width(640.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("📋").size(15.0).color(Color32::WHITE));
                            ui.add(egui::Label::new(RichText::new(text).size(14.0).color(Color32::WHITE)).wrap());
                        });
                    });
            });
        ctx.request_repaint_after(std::time::Duration::from_millis(60));
    }

    fn toast(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        // Only a refresh you asked for shows "syncing"; automatic checks and saving
        // changes happen quietly in the background.
        let syncing = self.receiving && !self.quiet_sync;
        let problem = self.status.contains("nie powiodło") || self.status.contains("Najpierw") || self.status.contains("Wpisz");
        let keep = if problem { 8.0 } else { 4.0 };
        let age = now - self.status_at;
        if !syncing && (self.status.is_empty() || age > keep) {
            return;
        }
        let text = if syncing { "Synchronizuję dane…".to_string() } else { self.status.clone() };
        // fade out during the last half second
        let alpha = if syncing { 1.0 } else { ((keep - age) / 0.5).clamp(0.0, 1.0) as f32 };
        let pos = match (self.module, self.list_rect) {
            (Module::Mail, Some(r)) => Pos2::new(r.center().x, r.bottom() - 18.0),
            _ => {
                let s = ctx.content_rect();
                Pos2::new(s.left() + 140.0, s.bottom() - 70.0)
            }
        };
        let max_w = self.list_rect.map(|r| r.width() - 24.0).unwrap_or(320.0).max(180.0);
        egui::Area::new(id("sync-toast"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .pivot(Align2::CENTER_BOTTOM)
            .interactable(false)
            .show(ctx, |ui| {
                ui.set_opacity(alpha);
                Frame::new()
                    .fill(BG())
                    .stroke(Stroke::new(1.0, BORDER()))
                    .corner_radius(CornerRadius::same(18))
                    .inner_margin(Margin::symmetric(14, 8))
                    .shadow(egui::epaint::Shadow { offset: [0, 2], blur: 12, spread: 0, color: Color32::from_black_alpha(40) })
                    .show(ui, |ui| {
                        ui.set_max_width(max_w);
                        ui.horizontal(|ui| {
                            if syncing {
                                ui.add(egui::Spinner::new().size(14.0).color(ACCENT()));
                            } else if problem {
                                ui.label(RichText::new("⚠").color(FLAG));
                            } else {
                                ui.label(RichText::new("✓").color(Color32::from_rgb(0x2E, 0x9E, 0x5B)));
                            }
                            ui.add(egui::Label::new(RichText::new(text).size(13.0).color(TEXT())).wrap());
                        });
                    });
            });
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
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

    /// Fills in the Google client from a built-in setup or a file downloaded from Google Cloud.
    fn find_google_client(&mut self) -> bool {
        if let Some((id, secret)) = crate::oauth::builtin_client().or_else(crate::oauth::find_downloaded_client) {
            self.store.account.oauth_client_id = id;
            self.store.account.oauth_client_secret = secret;
            self.set_status("Znaleziono dane aplikacji z Google");
            true
        } else {
            false
        }
    }

    /// A Google client file dropped onto the window.
    fn take_dropped_files(&mut self, ctx: &egui::Context) {
        let files = ctx.input(|i| i.raw.dropped_files.clone());
        for f in files {
            let text = f.bytes().ok().and_then(|b| String::from_utf8(b).ok());
            match text.as_deref().and_then(crate::oauth::parse_client_json) {
                Some((id, secret)) => {
                    let a = &mut self.store.account;
                    a.oauth_client_id = id;
                    a.oauth_client_secret = secret;
                    a.use_oauth = true;
                    if a.imap_host.is_empty() {
                        a.use_gmail();
                    }
                    self.oauth_setup = false;
                    self.account_open = true;
                    self.set_status("Wczytano plik z Google — kliknij Zaloguj przez Google");
                }
                None => self.set_status("To nie jest plik klienta z Google Cloud (client_secret_….json)"),
            }
        }
    }

    fn can_connect(&self) -> bool {
        self.store.account.can_connect()
    }

    fn send_receive(&mut self) {
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
    fn auto_sync(&mut self) {
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
    fn watch_inbox(&mut self) {
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

    fn poll_topics(&mut self) {
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

    fn poll_net(&mut self) {
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
    fn poll_email(&mut self) {
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

    /// Everyone to suggest while writing: the address book's groups first, then people from
    /// the mailbox and from the address book (worked out again only when something changed).
    fn mail_contacts(&mut self) -> Arc<Vec<crate::contacts::Contact>> {
        let sig = (self.store.messages.len(), self.store.next_id.wrapping_add(self.book.next_rev << 32).wrapping_add(self.book.people.len() as u64));
        if sig != self.contacts_sig {
            let t = std::time::Instant::now();
            let mut list: Vec<crate::contacts::Contact> = self
                .book
                .groups()
                .iter()
                .map(|g| crate::contacts::Contact::group(&g.name, self.book.group_addresses(&g.id)))
                .filter(|c| !c.members.is_empty())
                .collect();
            let mut mail = crate::contacts::collect(&self.store.messages, &self.store.account.email);
            let known: HashSet<String> = mail.iter().map(|c| c.addr.to_lowercase()).collect();
            for p in self.book.people() {
                for e in &p.emails {
                    if !known.contains(&e.to_lowercase()) {
                        mail.push(crate::contacts::Contact::person(&p.name, e));
                    }
                }
            }
            list.extend(mail);
            self.contacts = Arc::new(list);
            self.contacts_sig = sig;
            crate::diag::took("address suggestions ready", t, format!("{} people and groups", self.contacts.len()));
        }
        self.contacts.clone()
    }

    // ------------------------------------------------------------ address book

    /// The book in step with Google Contacts (through the Google script): soon after a change
    /// made here, and every 15 minutes.
    fn book_tick(&mut self) {
        if let Some(res) = self.book_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.book_rx = None;
            match res {
                Ok(done) => {
                    let renamed = done.renamed.clone();
                    self.book.merge_after_sync(done.book, done.sent_rev, &done.renamed);
                    let rename = |id: &str| renamed.iter().find(|(a, _)| a == id).map(|(_, b)| b.clone());
                    if let Some(g) = self.book_group.clone().and_then(|g| rename(&g)) {
                        self.book_group = Some(g);
                    }
                    if let Some(e) = self.book_edit.as_mut() {
                        if let Some(n) = rename(&e.id) {
                            e.id = n;
                        }
                        e.groups = e.groups.iter().map(|g| rename(g).unwrap_or_else(|| g.clone())).collect();
                    }
                    self.book.save();
                    let n = self.book.people().count();
                    self.book_said = Some(Ok(format!("Kontakty Google: {n} {}, {}", crate::pl_people(n), self.book.synced)));
                    crate::diag::log(format!("contacts: in step with Google ({} sent, {n} people)", done.sent));
                }
                Err(e) => {
                    crate::diag::log(format!("contacts: {e}"));
                    self.book_said = Some(Err(e));
                }
            }
            self.touched = true;
        }
        if !self.google_contacts() {
            return;
        }
        if self.book_rx.is_some() || self.loading.is_some() || self.shot.is_some() || std::env::var("POCZTA_OFFLINE").is_ok() {
            return;
        }
        let now = self.clock();
        let failed = matches!(self.book_said, Some(Err(_)));
        let due = if self.book.dirty_count() > 0 && !failed { now - self.book_last > 2.0 } else { now - self.book_last > 15.0 * 60.0 };
        if !due {
            self.ctx.request_repaint_after(std::time::Duration::from_secs(if self.book.dirty_count() > 0 { 2 } else { 60 }));
            return;
        }
        self.book_last = now;
        let (tx, rx) = channel();
        let (acc, snapshot, ctx) = (self.store.account.clone(), self.book.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(crate::book::sync(&acc, &snapshot));
            ctx.request_repaint();
        });
        self.book_rx = Some(rx);
    }

    fn book_changed(&mut self) {
        self.book.save();
        // a failed sync is tried again with the change
        if matches!(self.book_said, Some(Err(_))) {
            self.book_said = None;
        }
        self.touched = true;
    }

    fn book_new_person(&mut self) {
        self.module = Module::Contacts;
        let groups = self.book_group.clone().filter(|g| g != MAIL_PEOPLE).into_iter().collect();
        self.book_edit = Some(PersonEdit { id: crate::book::new_id(), name: String::new(), emails: String::new(), groups, new: true });
    }

    /// A new message to everyone in a group.
    fn write_to_group(&mut self, group: &str) {
        let to = self.book.group_addresses(group).join(", ");
        if to.is_empty() {
            self.set_status("W tej grupie nie ma jeszcze nikogo z adresem e-mail");
            return;
        }
        self.module = Module::Mail;
        self.open_composer(to, String::new(), String::new(), String::new(), None, None);
    }

    /// The left side in the contacts: everyone, people from the mailbox, the groups, and the
    /// state of the link with Google Contacts.
    fn contacts_side(&mut self, ui: &mut Ui) {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(RichText::new("Kontakty").font(semibold(18.0)).color(TEXT()));
        });
        ui.add_space(6.0);
        let everyone = self.book.people().count();
        if folder_row(ui, "👤", MUTED(), "Wszystkie kontakty", everyone, self.book_group.is_none(), 28.0) {
            self.book_group = None;
        }
        let from_mail = self.mail_people().len();
        if folder_row(ui, "✉", MUTED(), "Z poczty", from_mail, self.book_group.as_deref() == Some(MAIL_PEOPLE), 28.0) {
            self.book_group = Some(MAIL_PEOPLE.into());
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(RichText::new("Grupy").small().color(MUTED()));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                if ui.add(egui::Button::new(RichText::new("+ Nowa grupa").size(12.5).color(ACCENT())).frame(false)).clicked() {
                    self.book_new_group = Some(String::new());
                }
            });
        });
        if let Some(name) = self.book_new_group.as_mut() {
            let (mut done, mut cancel) = (false, false);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                let r = ui.add(egui::TextEdit::singleline(name).hint_text("Nazwa grupy").desired_width(ui.available_width() - 70.0));
                r.request_focus();
                if (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))) || ui.button("OK").clicked() {
                    done = true;
                }
                if ui.input(|i| i.key_pressed(Key::Escape)) {
                    cancel = true;
                }
            });
            if cancel {
                self.book_new_group = None;
            }
            if done {
                let name = self.book_new_group.take().unwrap_or_default();
                if !name.trim().is_empty() {
                    let id = self.book.put_group(None, name.trim());
                    self.book_group = Some(id);
                    self.book_changed();
                }
            }
        }
        let groups: Vec<(String, String, usize)> = self.book.groups().iter().map(|g| (g.id.clone(), g.name.clone(), self.book.members(&g.id).len())).collect();
        for (id, name, n) in groups {
            if folder_row(ui, "👥", MUTED(), &name, n, self.book_group.as_deref() == Some(id.as_str()), 28.0) {
                self.book_group = Some(id);
            }
        }
        ui.add_space(14.0);
        Frame::new().inner_margin(Margin::symmetric(14, 0)).show(ui, |ui| {
            if !self.google_contacts() {
                ui.label(RichText::new("Kontakty są na tym komputerze. Aby mieć je wspólne z Kontaktami Google:").small().color(MUTED()));
                if ui.link(RichText::new("☰ → Kalendarz i kontakty Google").small()).clicked() {
                    self.gcal_open = true;
                }
            } else {
                if self.book_rx.is_some() {
                    ui.label(RichText::new("⟳ synchronizacja z Kontaktami Google…").small().color(MUTED()));
                } else {
                    match &self.book_said {
                        Some(Ok(m)) => {
                            ui.label(RichText::new(format!("✓ {m}")).small().color(Color32::from_rgb(0x2E, 0x9E, 0x5B)));
                        }
                        Some(Err(e)) => {
                            ui.label(RichText::new(e.as_str()).small().color(FLAG));
                        }
                        None => {}
                    }
                }
                let waiting = self.book.dirty_count();
                if waiting > 0 {
                    ui.label(RichText::new(format!("{waiting} zmian czeka na wysłanie do Google")).small().color(MUTED()));
                }
                if self.book_rx.is_none() && ui.link(RichText::new("Synchronizuj teraz").small()).clicked() {
                    self.book_last = -1e9;
                    self.book_said = None;
                }
            }
        });
    }

    /// People found in the mailbox who are not in the address book yet.
    fn mail_people(&mut self) -> Vec<(String, String)> {
        let all = self.mail_contacts();
        all.iter()
            .filter(|c| c.members.is_empty() && !c.addr.is_empty() && self.book.person_by_email(&c.addr).is_none())
            .map(|c| (c.name.clone(), c.addr.clone()))
            .collect()
    }

    /// The middle of the contacts: the list (of a group, everyone, or from the mailbox) and the
    /// person chosen, with their groups.
    fn contacts_main(&mut self, ui: &mut Ui) {
        let group = self.book_group.clone();
        let in_group = group.clone().filter(|g| g != MAIL_PEOPLE);
        let title = match group.as_deref() {
            None => "Wszystkie kontakty".to_string(),
            Some(MAIL_PEOPLE) => "Z poczty — osoby, które do Ciebie pisały albo do których pisałeś".to_string(),
            Some(g) => self.book.groups.iter().find(|x| x.id == g).map(|x| x.name.clone()).unwrap_or_default(),
        };
        ui.horizontal(|ui| {
            if let (Some(g), Some(name)) = (in_group.clone(), self.book_rename.as_mut()) {
                let r = ui.add(egui::TextEdit::singleline(name).desired_width(260.0).font(FontId::proportional(18.0)));
                if (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))) || ui.button("Zapisz").clicked() {
                    let n = self.book_rename.take().unwrap_or_default();
                    if !n.trim().is_empty() {
                        self.book.put_group(Some(&g), n.trim());
                        self.book_changed();
                    }
                }
            } else {
                ui.label(RichText::new(&title).font(semibold(18.0)).color(TEXT()));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(g) = in_group.clone() {
                    if ui.button("🗑 Usuń grupę").on_hover_text("Osoby zostają w kontaktach, znika tylko grupa").clicked() {
                        self.book.delete_group(&g);
                        self.book_group = None;
                        self.book_changed();
                    }
                    if ui.button("✏ Zmień nazwę").clicked() {
                        self.book_rename = Some(title.clone());
                    }
                    let n = self.book.group_addresses(&g).len();
                    if ui
                        .add(egui::Button::new(RichText::new(format!("✉  Napisz do grupy ({n})")).color(Color32::WHITE)).fill(ACCENT()))
                        .on_hover_text("Nowa wiadomość do wszystkich osób z tej grupy")
                        .clicked()
                    {
                        self.write_to_group(&g);
                    }
                }
                if ui.button("+ Nowy kontakt").clicked() {
                    self.book_new_person();
                }
            });
        });
        ui.add_space(6.0);
        // adding people to the group: type a name or an address (suggestions from the mailbox)
        if let Some(g) = in_group.clone() {
            let contacts = self.mail_contacts();
            ui.horizontal(|ui| {
                ui.label(RichText::new("Dodaj do grupy:").color(MUTED()));
                let w = (ui.available_width() - 90.0).min(520.0);
                crate::contacts::recipients_field(ui, &mut self.book_add, "Wpisz imię lub adres…", w, &contacts, ("book-add", 0));
                if ui.button("Dodaj").clicked() {
                    let text = std::mem::take(&mut self.book_add);
                    for part in text.split([',', ';']).map(str::trim).filter(|p| p.contains('@')) {
                        let (name, addr) = crate::model::split_address(part);
                        self.book.add_to_group(&g, &name, &addr);
                    }
                    self.book_changed();
                }
            });
            ui.add_space(6.0);
        }
        ui.add(egui::TextEdit::singleline(&mut self.book_search).hint_text("🔍 Szukaj w kontaktach").desired_width(320.0));
        ui.add_space(6.0);
        let q = self.book_search.trim().to_lowercase();
        // (id, name, first address, group names, from the mailbox)
        let rows: Vec<(String, String, String, String, bool)> = if group.as_deref() == Some(MAIL_PEOPLE) {
            self.mail_people().into_iter().map(|(n, a)| (format!("mail:{a}"), n, a, String::new(), true)).collect()
        } else {
            let people: Vec<&crate::book::Person> = match in_group.as_deref() {
                Some(g) => self.book.members(g),
                None => {
                    let mut v: Vec<&crate::book::Person> = self.book.people().collect();
                    v.sort_by_key(|p| p.name.to_lowercase());
                    v
                }
            };
            people
                .into_iter()
                .map(|p| {
                    let gnames: Vec<String> = p.groups.iter().filter_map(|g| self.book.groups.iter().find(|x| x.id == *g && !x.deleted).map(|x| x.name.clone())).collect();
                    (p.id.clone(), p.name.clone(), p.emails.first().cloned().unwrap_or_default(), gnames.join(", "), false)
                })
                .collect()
        };
        let rows: Vec<_> = rows.into_iter().filter(|r| q.is_empty() || r.1.to_lowercase().contains(&q) || r.2.to_lowercase().contains(&q)).collect();
        let avail = ui.available_rect_before_wrap();
        let list_w = (avail.width() * 0.5).clamp(300.0, 520.0);
        ui.horizontal_top(|ui| {
            // the list
            ui.allocate_ui_with_layout(Vec2::new(list_w, avail.height()), Layout::top_down(Align::Min), |ui| {
                if rows.is_empty() {
                    ui.add_space(20.0);
                    ui.label(RichText::new(if in_group.is_some() { "W tej grupie nikogo jeszcze nie ma — dodaj osoby polem powyżej." } else { "Brak kontaktów" }).color(MUTED()));
                }
                egui::ScrollArea::vertical().id_salt("book-list").auto_shrink([false, false]).show_rows(ui, 52.0, rows.len(), |ui, range| {
                    for (id, name, addr, gnames, from_mail) in &rows[range] {
                        let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 52.0), Sense::click());
                        let on = self.book_edit.as_ref().is_some_and(|e| e.id == *id || (*from_mail && e.emails.trim() == addr.as_str()));
                        if on {
                            ui.painter().rect_filled(r, CornerRadius::same(4), SELECTED());
                        } else if resp.hovered() {
                            ui.painter().rect_filled(r, CornerRadius::same(4), HOVER());
                        }
                        let shown = if name.trim().is_empty() { addr.clone() } else { name.clone() };
                        avatar(ui.painter(), Pos2::new(r.left() + 24.0, r.center().y), 16.0, &shown);
                        let p = ui.painter();
                        line_f(p, Pos2::new(r.left() + 50.0, r.top() + 6.0), &shown, semibold(14.0), TEXT(), r.width() - 60.0);
                        let sub = if gnames.is_empty() { addr.clone() } else { format!("{addr}  ·  👥 {gnames}") };
                        line(p, Pos2::new(r.left() + 50.0, r.top() + 27.0), &sub, 12.5, MUTED(), r.width() - 60.0);
                        if resp.clicked() {
                            self.book_edit = Some(match self.book.people.iter().find(|p| p.id == *id) {
                                Some(p) => PersonEdit { id: p.id.clone(), name: p.name.clone(), emails: p.emails.join("\n"), groups: p.groups.clone(), new: false },
                                None => PersonEdit { id: crate::book::new_id(), name: name.clone(), emails: addr.clone(), groups: vec![], new: true },
                            });
                        }
                        if in_group.is_some() && !from_mail {
                            resp.context_menu(|ui| {
                                if ui.button("Usuń z grupy").clicked() {
                                    if let Some(g) = in_group.clone() {
                                        self.book.remove_from_group(&g, id);
                                        self.book_changed();
                                    }
                                    ui.close();
                                }
                            });
                        }
                    }
                });
            });
            ui.separator();
            // the person
            ui.vertical(|ui| self.person_editor(ui));
        });
    }

    /// The chosen person: name, addresses, groups; saved to the book (and to Google).
    fn person_editor(&mut self, ui: &mut Ui) {
        let groups: Vec<(String, String)> = self.book.groups().iter().map(|g| (g.id.clone(), g.name.clone())).collect();
        let Some(e) = self.book_edit.as_mut() else {
            ui.add_space(30.0);
            ui.label(RichText::new("Wybierz osobę z listy albo dodaj nowy kontakt.").color(MUTED()));
            return;
        };
        let (mut save, mut delete, mut write) = (false, false, false);
        ui.add_space(4.0);
        ui.label(RichText::new(if e.new { "Nowy kontakt" } else { "Kontakt" }).font(semibold(16.0)).color(TEXT()));
        ui.add_space(6.0);
        egui::Grid::new("person").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Imię i nazwisko");
            ui.add(egui::TextEdit::singleline(&mut e.name).desired_width(320.0));
            ui.end_row();
            ui.label("E-mail");
            ui.add(egui::TextEdit::multiline(&mut e.emails).hint_text("adres@firma.pl\n(kilka adresów: każdy w osobnej linii)").desired_rows(2).desired_width(320.0));
            ui.end_row();
            ui.label("Grupy");
            ui.vertical(|ui| {
                if groups.is_empty() {
                    ui.label(RichText::new("Brak grup — utwórz grupę po lewej („+ Nowa grupa”).").small().color(MUTED()));
                }
                for (id, name) in &groups {
                    let mut on = e.groups.contains(id);
                    if ui.checkbox(&mut on, name).changed() {
                        if on {
                            e.groups.push(id.clone());
                        } else {
                            e.groups.retain(|g| g != id);
                        }
                    }
                }
            });
            ui.end_row();
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new(RichText::new(if e.new { "Zapisz w kontaktach" } else { "Zapisz" }).color(Color32::WHITE)).fill(ACCENT())).clicked() {
                save = true;
            }
            if !e.emails.trim().is_empty() && ui.button("✉ Napisz").clicked() {
                write = true;
            }
            if !e.new && ui.button("🗑 Usuń kontakt").clicked() {
                delete = true;
            }
        });
        let e = e.clone();
        let first = e.emails.lines().map(str::trim).find(|l| l.contains('@')).unwrap_or("").to_string();
        if save {
            let emails: Vec<String> = e.emails.lines().map(str::trim).filter(|l| l.contains('@')).map(String::from).collect();
            if emails.is_empty() && e.name.trim().is_empty() {
                self.set_status("Wpisz imię i nazwisko albo adres e-mail");
            } else {
                self.book.put_person(crate::book::Person { id: e.id.clone(), name: e.name.trim().to_string(), emails, groups: e.groups.clone(), ..Default::default() });
                if let Some(x) = self.book_edit.as_mut() {
                    x.new = false;
                }
                self.book_changed();
                self.set_status("Kontakt zapisany");
            }
        }
        if delete {
            self.book.delete_person(&e.id);
            self.book_edit = None;
            self.book_changed();
            self.set_status("Kontakt usunięty");
        }
        if write {
            let to = if e.name.trim().is_empty() { first } else { format!("{} <{}>", e.name.trim(), first) };
            self.module = Module::Mail;
            self.open_composer(to, String::new(), String::new(), String::new(), None, None);
        }
    }

    /// What the sync did, step by step with timings, plus a read-only speed test.
    fn diag_ui(&mut self, ctx: &egui::Context) {
        if !self.diag_open {
            return;
        }
        let mut open = true;
        let lines = crate::diag::lines();
        let testing = self.diag_testing.load(std::sync::atomic::Ordering::Relaxed);
        crate::theme::dialog(ctx, "diag", "Diagnostyka synchronizacji", 760.0, &mut open, |ui| {
                ui.horizontal(|ui| {
                    let can = self.store.account.can_connect() && !testing;
                    if ui
                        .add_enabled(can, egui::Button::new(RichText::new("  Test szybkości  ").color(Color32::WHITE)).fill(PRIMARY))
                        .on_hover_text("Pobiera 200 najnowszych wiadomości tylko do odczytu i mierzy czas każdego kroku")
                        .clicked()
                    {
                        let acc = self.store.account.clone();
                        let flag = self.diag_testing.clone();
                        let ctx = ctx.clone();
                        flag.store(true, std::sync::atomic::Ordering::Relaxed);
                        std::thread::spawn(move || {
                            let r = std::panic::catch_unwind(|| crate::net::benchmark(&acc, 200))
                                .unwrap_or_else(|_| Err("nieoczekiwany błąd".into()));
                            if let Err(e) = r {
                                crate::diag::log(format!("speed test failed: {e}"));
                            }
                            flag.store(false, std::sync::atomic::Ordering::Relaxed);
                            ctx.request_repaint();
                        });
                    }
                    if testing {
                        ui.spinner();
                        ui.label(RichText::new("trwa test…").color(MUTED()));
                    }
                    if ui.button("Kopiuj").clicked() {
                        self.flash = Some(("Dziennik skopiowany do schowka".into(), self.clock()));
                        ui.ctx().copy_text(lines.join("
"));
                    }
                    if ui.button("Otwórz plik dziennika").clicked() {
                        let _ = std::process::Command::new("notepad").arg(crate::diag::file()).spawn();
                    }
                });
                ui.label(
                    RichText::new("Każdy krok synchronizacji z czasem trwania. Zapisywane też w pliku dziennika.")
                        .small()
                        .color(MUTED()),
                );
                ui.separator();
                egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                    if lines.is_empty() {
                        ui.label(RichText::new("Jeszcze nic się nie działo — kliknij Odśwież albo Test szybkości.").color(MUTED()));
                    }
                    for l in &lines {
                        let strong = l.contains("──");
                        let mut t = RichText::new(l).monospace().size(11.5);
                        if strong {
                            t = t.strong().color(PRIMARY);
                        } else if l.contains("failed") {
                            t = t.color(Color32::from_rgb(190, 40, 40));
                        }
                        ui.label(t);
                    }
                });
            });
        if testing {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        if !open {
            self.diag_open = false;
        }
    }

    /// Google Calendar read from its secret iCal address, every 15 minutes and on "Odśwież".
    fn gcal_tick(&mut self) {
        if !self.gcal_loaded {
            self.gcal_loaded = true;
            self.gcal_text = crate::secret::unprotect(&self.prefs.gcal_saved).unwrap_or_default();
            if let Ok(t) = std::env::var("POCZTA_GCAL_TEST") {
                self.gcal_text = t;
            }
        }
        if let Some(res) = self.gcal_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.gcal_rx = None;
            match res {
                Ok(evs) => {
                    let n = evs.len();
                    self.store.events.retain(|e| !e.google);
                    self.store.events.extend(evs);
                    let word = match n {
                        1 => "wydarzenie",
                        n if (2..=4).contains(&(n % 10)) && !(12..=14).contains(&(n % 100)) => "wydarzenia",
                        _ => "wydarzeń",
                    };
                    self.gcal_said = Some(Ok(format!("{n} {word} z Google, {}", Local::now().format("%H:%M"))));
                    crate::diag::log(format!("Google calendar: {n} events"));
                    self.save_now = true;
                    self.touched = true;
                }
                Err(e) => {
                    crate::diag::log(format!("Google calendar: {e}"));
                    self.gcal_said = Some(Err(e));
                }
            }
        }
        // answers about events written to Google: the copy made here gives way to Google's own
        let pushed: Vec<(Option<u64>, Result<(), String>)> = self.gcal_push.as_ref().map(|(_, rx)| rx.try_iter().collect()).unwrap_or_default();
        for (local, res) in pushed {
            match res {
                Ok(()) => {
                    if let Some(id) = local {
                        self.store.events.retain(|e| e.id != id);
                    }
                    self.gcal_last = -1e9;
                    self.set_status("Zapisano w Kalendarzu Google");
                }
                Err(e) => {
                    crate::diag::log(format!("Google calendar save: {e}"));
                    self.set_status(format!("Nie udało się zapisać w Kalendarzu Google: {e}"));
                }
            }
            self.touched = true;
        }
        if self.prefs.gcal_key.is_empty() {
            self.prefs.gcal_key = crate::share::new_token() + &crate::share::new_token();
            self.save_now = true;
        }
        let oauth = self.google_calendar();
        let urls: Vec<String> = crate::gcal::urls(&self.gcal_text).into_iter().filter(|u| !crate::gcal::is_script(u)).collect();
        if (!oauth && urls.is_empty()) || self.gcal_rx.is_some() || self.loading.is_some() || self.shot.is_some() {
            return;
        }
        let now = self.clock();
        if now - self.gcal_last < 15.0 * 60.0 {
            self.ctx.request_repaint_after(std::time::Duration::from_secs_f64((15.0 * 60.0 - (now - self.gcal_last)).clamp(5.0, 900.0)));
            return;
        }
        self.gcal_last = now;
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        let key = self.prefs.gcal_key.clone();
        let acc = self.store.account.clone();
        std::thread::spawn(move || {
            let _ = tx.send(if oauth { crate::google::calendar_events(&acc) } else { crate::gcal::sync(&urls, &key) });
            ctx.request_repaint();
        });
        self.gcal_rx = Some(rx);
    }

    /// The calendar comes from Google through the sign-in (allowed and switched on).
    fn google_calendar(&self) -> bool {
        let a = &self.store.account;
        self.prefs.google_calendar && crate::google::ready(a) && crate::google::has_scope(a, crate::google::SCOPE_CALENDAR)
    }

    /// The contacts are shared with Google through the sign-in (allowed and switched on).
    fn google_contacts(&self) -> bool {
        let a = &self.store.account;
        self.prefs.google_contacts && crate::google::ready(a) && crate::google::has_scope(a, crate::google::SCOPE_CONTACTS)
    }

    /// Writes an event to Google Calendar through the script (in the background). `local`: the
    /// copy made here, removed once Google has it.
    fn gcal_write(&mut self, local: Option<u64>, gid: String, ev: Option<Event>) {
        if !self.google_calendar() {
            return;
        }
        let acc = self.store.account.clone();
        let tx = self.gcal_push.get_or_insert_with(channel).0.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let res = match &ev {
                Some(e) => crate::google::save_event(&acc, &gid, &e.title, &e.location, e.date.and_time(e.start), e.minutes).map(|_| ()),
                None => crate::google::delete_event(&acc, &gid),
            };
            let _ = tx.send((local, res));
            ctx.request_repaint();
        });
    }

    /// "Kalendarz i kontakty Google": through the Google sign-in (as for the mail).
    fn gcal_settings_ui(&mut self, ui: &mut Ui) {
        if let Some(r) = self.google_check_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.google_check = Some(r);
            self.google_check_rx = None;
        }
        let a = self.store.account.clone();
        let ready = crate::google::ready(&a);
        let (cal_ok, people_ok) = (crate::google::has_scope(&a, crate::google::SCOPE_CALENDAR), crate::google::has_scope(&a, crate::google::SCOPE_CONTACTS));
        ui.label(
            RichText::new("Kalendarz i kontakty łączą się z Google tym samym logowaniem co poczta (konto Google, OAuth). Wydarzenia i kontakty z Google są tu na bieżąco, a zmiany zrobione w programie trafiają do Google.")
                .small()
                .color(MUTED()),
        );
        ui.add_space(4.0);
        let ok = Color32::from_rgb(0x2E, 0x9E, 0x5B);
        if !ready {
            ui.label(RichText::new("Poczta nie jest zalogowana kontem Google (OAuth). Zaloguj się w „Konto pocztowe” → Konto Google.").color(FLAG));
            if ui.button("Otwórz konto pocztowe").clicked() {
                self.account_open = true;
                self.gcal_open = false;
            }
            return;
        }
        ui.label(RichText::new(format!("Konto Google: {}", a.email)).strong());
        for (label, granted) in [("Poczta", true), ("Kalendarz", cal_ok), ("Kontakty", people_ok)] {
            if granted {
                ui.label(RichText::new(format!("✓  {label} — dostęp udzielony")).color(ok));
            } else {
                ui.label(RichText::new(format!("✗  {label} — brak zgody (zaloguj się ponownie poniżej)")).color(FLAG));
            }
        }
        if !(cal_ok && people_ok) {
            ui.add_space(6.0);
            ui.label(RichText::new("Jednorazowo, w Twoim projekcie Google Cloud (tym od logowania poczty):").small());
            ui.horizontal_wrapped(|ui| {
                if ui.small_button("1. Włącz Google Calendar API ↗").clicked() {
                    crate::oauth::open_url("https://console.cloud.google.com/apis/library/calendar-json.googleapis.com");
                }
                if ui.small_button("2. Włącz People API ↗").clicked() {
                    crate::oauth::open_url("https://console.cloud.google.com/apis/library/people.googleapis.com");
                }
            });
            ui.label(RichText::new("3. Potem zaloguj się ponownie — Google zapyta o dostęp do poczty, kalendarza i kontaktów; zaznacz wszystkie pola i kliknij Kontynuuj.").small());
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let label = if cal_ok && people_ok { "Zaloguj ponownie przez Google" } else { "  G   Zaloguj ponownie przez Google (poczta, kalendarz, kontakty)  " };
            let btn = egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(ACCENT());
            if ui.add_enabled(!self.signing_in, btn).clicked() {
                self.signing_in = true;
                self.sign_in_error = None;
                net::google_sign_in(a.oauth_client_id.clone(), a.oauth_client_secret.clone(), self.tx.clone(), self.ctx.clone());
                self.set_status("Otwieram stronę logowania Google w przeglądarce…");
            }
            if self.signing_in {
                ui.label(RichText::new("dokończ logowanie w przeglądarce…").small().color(MUTED()));
            }
        });
        if let Some(e) = &self.sign_in_error {
            ui.colored_label(FLAG, e);
        }
        ui.add_space(6.0);
        let mut changed = false;
        changed |= ui.checkbox(&mut self.prefs.google_calendar, "Synchronizuj kalendarz z Google").changed();
        changed |= ui.checkbox(&mut self.prefs.google_contacts, "Synchronizuj kontakty i grupy z Google").changed();
        if changed {
            self.save_now = true;
            self.gcal_last = -1e9;
            self.book_last = -1e9;
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            let busy = self.google_check_rx.is_some();
            let auto = std::env::var("POCZTA_START").as_deref() == Ok("google-check") && {
                use std::sync::atomic::{AtomicBool, Ordering};
                static ONCE: AtomicBool = AtomicBool::new(false);
                !ONCE.swap(true, Ordering::Relaxed)
            };
            if ui.add_enabled(!busy, egui::Button::new("Sprawdź połączenie")).on_hover_text("Sprawdza dostęp do kalendarza i kontaktów (tylko odczyt)").clicked() || auto {
                let (tx, rx) = channel();
                let (acc, ctx) = (a.clone(), ui.ctx().clone());
                std::thread::spawn(move || {
                    let _ = tx.send(crate::google::check(&acc));
                    ctx.request_repaint();
                });
                self.google_check_rx = Some(rx);
                self.google_check = None;
            }
            if ui.add_enabled(self.gcal_rx.is_none(), egui::Button::new("Synchronizuj teraz")).clicked() {
                self.gcal_last = -1e9;
                self.book_last = -1e9;
                self.gcal_said = None;
                self.book_said = None;
            }
            if busy || self.gcal_rx.is_some() || self.book_rx.is_some() {
                ui.label(RichText::new("pracuję…").small().color(MUTED()));
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
            }
        });
        if let Some((cal, people)) = &self.google_check {
            for (what, r) in [("Kalendarz", cal), ("Kontakty", people)] {
                match r {
                    Ok(m) => {
                        ui.label(RichText::new(format!("✓  {m}")).small().color(ok));
                    }
                    Err(e) => {
                        ui.label(RichText::new(format!("✗  {what}: {e}")).small().color(FLAG));
                    }
                }
            }
        }
        for said in [&self.gcal_said, &self.book_said] {
            match said {
                Some(Ok(m)) => {
                    ui.label(RichText::new(format!("✓  {m}")).small().color(ok));
                }
                Some(Err(e)) => {
                    ui.label(RichText::new(e.as_str()).small().color(FLAG));
                }
                None => {}
            }
        }
        ui.label(RichText::new("Wydarzenia powtarzające się, całodniowe i z cudzych kalendarzy są tylko do odczytu — zmieniasz je w Google.").small().color(MUTED()));
    }

    /// „Poprawianie tekstu (AI)”: where CLIProxy is, its key and the model that corrects.
    fn ai_settings_ui(&mut self, ui: &mut Ui) {
        if let Some((said, models)) = self.ai_check.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.ai_said = Some(said);
            if !models.is_empty() {
                self.ai_models = models;
            }
            self.ai_check = None;
        }
        let s = self.ai_set.get_or_insert_with(crate::ai::load);
        let mut changed = false;
        ui.label(RichText::new("Poprawianie tekstu (AI)").strong());
        ui.label(
            RichText::new("Przycisk „✨ Popraw z AI” w oknie pisania poprawia tekst przez CLIProxy, w jednym z czterech stylów.")
                .small()
                .color(MUTED()),
        );
        ui.add_space(4.0);
        egui::Grid::new("accgrid-ai").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Adres CLIProxy");
            changed |= ui.add(egui::TextEdit::singleline(&mut s.url).hint_text("http://100.100.1.2:8317").desired_width(290.0)).changed();
            ui.end_row();
            ui.label("Klucz");
            changed |= ui.add(egui::TextEdit::singleline(&mut s.key).password(true).hint_text("sk-…").desired_width(290.0)).changed();
            ui.end_row();
            ui.label("Model");
            ui.horizontal(|ui| {
                changed |= ui.add(egui::TextEdit::singleline(&mut s.model).hint_text("claude-sonnet-5-5").desired_width(200.0)).changed();
                let mut list: Vec<String> = vec!["claude-sonnet-5-5".into(), "claude-opus-5-5".into(), "claude-haiku-5-5".into()];
                for m in &self.ai_models {
                    if !list.contains(m) {
                        list.push(m.clone());
                    }
                }
                egui::ComboBox::from_id_salt("ai-model").selected_text("").width(24.0).show_ui(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                        for m in list {
                            if ui.selectable_label(s.model == m, &m).clicked() {
                                s.model = m;
                                changed = true;
                            }
                        }
                    });
                });
            });
            ui.end_row();
            let effort_box = |ui: &mut Ui, salt: &str, value: &mut String| -> bool {
                let mut ch = false;
                let shown = crate::ai::EFFORTS.iter().find(|(k, _)| *k == value.as_str()).map(|(_, l)| *l).unwrap_or("bez myślenia (najszybciej)");
                egui::ComboBox::from_id_salt(salt).selected_text(shown).width(200.0).show_ui(ui, |ui| {
                    for (k, l) in crate::ai::EFFORTS {
                        if ui.selectable_label(value.as_str() == k, l).clicked() {
                            *value = k.to_string();
                            ch = true;
                        }
                    }
                });
                ch
            };
            ui.label("Myślenie");
            changed |= effort_box(ui, "ai-effort", &mut s.effort);
            ui.end_row();
            ui.label("Tłumaczenie");
            ui.horizontal(|ui| {
                changed |= ui.add(egui::TextEdit::singleline(&mut s.translate_model).hint_text("claude-haiku-5-5").desired_width(200.0)).changed();
                egui::ComboBox::from_id_salt("ai-tr-model").selected_text("").width(24.0).show_ui(ui, |ui| {
                    for m in ["claude-haiku-5-5", "claude-sonnet-5-5", "claude-opus-5-5"] {
                        if ui.selectable_label(s.translate_model == m, m).clicked() {
                            s.translate_model = m.to_string();
                            changed = true;
                        }
                    }
                });
            });
            ui.end_row();
            ui.label("Myślenie (tłum.)");
            changed |= effort_box(ui, "ai-tr-effort", &mut s.translate_effort);
            ui.end_row();
        });
        ui.label(
            RichText::new("Model i myślenie z góry służą do „Popraw z AI”. Do tłumaczenia wystarczy szybki Haiku bez myślenia; wyższe myślenie daje staranniejszy wynik, ale trwa dłużej.")
                .small()
                .color(MUTED()),
        );
        if changed {
            crate::ai::save(s);
            self.ai_said = None;
        }
        ui.horizontal(|ui| {
            let checking = self.ai_check.is_some();
            if ui.add_enabled(!checking, egui::Button::new("Sprawdź połączenie").small()).clicked() {
                let (tx, rx) = std::sync::mpsc::channel();
                let set = s.clone();
                let ctx = ui.ctx().clone();
                std::thread::spawn(move || {
                    let said = crate::ai::test(&set);
                    let models = crate::ai::models(&set).unwrap_or_default();
                    let _ = tx.send((said, models));
                    ctx.request_repaint();
                });
                self.ai_check = Some(rx);
                self.ai_said = None;
            }
            if checking {
                ui.label(RichText::new("sprawdzam…").small().color(MUTED()));
            }
            match &self.ai_said {
                Some(Ok(m)) => {
                    ui.label(RichText::new(format!("✓  {m}")).small().color(Color32::from_rgb(0x2E, 0x9E, 0x5B)));
                }
                Some(Err(e)) => {
                    ui.label(RichText::new(e.as_str()).small().color(FLAG));
                }
                None => {}
            }
        });
        if self.ai_check.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
        }
    }

    /// The AI settings and the Google Calendar each in a window of their own.
    fn side_settings_ui(&mut self, ctx: &egui::Context) {
        if self.ai_open {
            let mut open = true;
            crate::theme::dialog(ctx, "ai", "Ustawienia AI", 600.0, &mut open, |ui| self.ai_settings_ui(ui));
            if !open {
                self.ai_open = false;
            }
        } else {
            self.ai_set = None;
        }
        if self.gcal_open {
            let mut open = true;
            crate::theme::dialog(ctx, "google", "Kalendarz i kontakty Google", 620.0, &mut open, |ui| self.gcal_settings_ui(ui));
            if !open {
                self.gcal_open = false;
            }
        }
    }

    /// A further mail account (mail only, signed in with a password — for Gmail an app password).
    fn extra_account_ui(&mut self, ctx: &egui::Context) {
        let Some((which, mut a)) = self.extra_edit.clone() else { return };
        let mut open = true;
        let (mut save, mut remove, mut cancel) = (false, false, false);
        let title = if which.is_some() { "Konto pocztowe" } else { "Dodaj konto pocztowe" };
        crate::theme::dialog(ctx, "extra-account", title, 540.0, &mut open, |ui| {
            ui.label(RichText::new("Dodatkowe konto służy tylko do poczty. Jego nieprzeczytane wiadomości trafiają do wspólnej listy „Nieprzeczytane”, a odpowiedź wychodzi z konta, na które przyszła wiadomość.").small().color(MUTED()));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new("  G   To jest Gmail  ").color(Color32::WHITE)).fill(PRIMARY)).on_hover_text("Wpisuje ustawienia serwerów Gmaila").clicked() {
                    a.use_gmail();
                    a.use_oauth = false;
                }
                if ui.link(RichText::new("Utwórz hasło aplikacji Google ↗").small()).clicked() {
                    crate::oauth::open_url("https://myaccount.google.com/apppasswords");
                }
            });
            ui.add_space(6.0);
            egui::Grid::new("extra-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Imię i nazwisko");
                ui.add(egui::TextEdit::singleline(&mut a.display_name).desired_width(320.0));
                ui.end_row();
                ui.label("Adres e-mail");
                let r = ui.add(egui::TextEdit::singleline(&mut a.email).hint_text("ty@firma.pl").desired_width(320.0));
                if r.changed() && (a.username.is_empty() || a.imap_host == "imap.gmail.com") {
                    a.username = a.email.clone();
                }
                ui.end_row();
                ui.label("Hasło");
                ui.add(egui::TextEdit::singleline(&mut a.password).password(true).hint_text("Gmail: 16-znakowe hasło aplikacji").desired_width(320.0));
                ui.end_row();
            });
            egui::CollapsingHeader::new("Ustawienia serwerów").id_salt("extra-servers").default_open(a.imap_host.is_empty()).show(ui, |ui| {
                egui::Grid::new("extra-servers-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("Serwer IMAP");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut a.imap_host).hint_text("imap.example.com").desired_width(220.0));
                        ui.add(egui::DragValue::new(&mut a.imap_port).range(1..=65535));
                    });
                    ui.end_row();
                    ui.label("Serwer SMTP");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut a.smtp_host).hint_text("smtp.example.com").desired_width(220.0));
                        ui.add(egui::DragValue::new(&mut a.smtp_port).range(1..=65535));
                    });
                    ui.end_row();
                    ui.label("Login");
                    ui.add(egui::TextEdit::singleline(&mut a.username).hint_text("zwykle adres e-mail").desired_width(300.0));
                    ui.end_row();
                });
            });
            ui.label(RichText::new("Hasło jest zapisane w zaszyfrowanej postaci (tylko dla Twojego konta Windows).").small().color(MUTED()));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ready = !a.email.trim().is_empty() && !a.imap_host.trim().is_empty() && !a.password.is_empty();
                if ui.add_enabled(ready, egui::Button::new(RichText::new("Zapisz i sprawdź pocztę").color(Color32::WHITE)).fill(ACCENT())).clicked() {
                    save = true;
                }
                if ui.button("Anuluj").clicked() {
                    cancel = true;
                }
                if which.is_some() {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button(RichText::new("Usuń konto").color(FLAG)).on_hover_text("Usuwa konto i jego wiadomości z programu (na serwerze nic się nie zmienia)").clicked() {
                            remove = true;
                        }
                    });
                }
            });
        });
        if save {
            a.use_oauth = false;
            if a.username.trim().is_empty() {
                a.username = a.email.trim().to_string();
            }
            if a.display_name.trim().is_empty() {
                a.display_name = a.email.trim().to_string();
            }
            match which {
                Some(k) if k < self.store.extra.len() => self.store.extra[k].account = a,
                _ => {
                    self.store.extra.push(crate::model::ExtraAccount { account: a, ..Default::default() });
                    let acct = self.store.extra.len() as u32;
                    self.view = MailView::Folder(Folder::Inbox);
                    self.view_account = acct;
                }
            }
            self.extra_edit = None;
            self.save_now = true;
            self.touched = true;
            self.quiet_sync = false;
            self.send_receive();
            return;
        }
        if remove {
            if let Some(k) = which.filter(|k| *k < self.store.extra.len()) {
                let acct = k as u32 + 1;
                self.store.extra.remove(k);
                // its messages go; the accounts after it move one place up
                self.store.messages.retain(|m| m.account != acct);
                for m in self.store.messages.iter_mut().filter(|m| m.account > acct) {
                    m.account -= 1;
                }
                self.pending_ops.retain(|(x, _)| *x != acct);
                for (x, _) in self.pending_ops.iter_mut().filter(|(x, _)| *x > acct) {
                    *x -= 1;
                }
                self.view_account = 0;
                self.view = MailView::Folder(Folder::Inbox);
                self.selected = None;
                self.save_now = true;
                self.touched = true;
                self.set_status("Konto usunięte z programu");
            }
            self.extra_edit = None;
            return;
        }
        if cancel || !open {
            self.extra_edit = None;
        } else {
            self.extra_edit = Some((which, a));
        }
    }

    fn account_ui(&mut self, ctx: &egui::Context) {
        if !self.account_open {
            return;
        }
        let mut open = true;
        let mut save_and_check = false;
        let mut start_sign_in = false;
        let mut refresh_signature = false;
        // the window fits on the screen; what does not fit scrolls
        crate::theme::dialog(ctx, "account", "Konto pocztowe", 560.0, &mut open, |ui| {
                let signing_in = self.signing_in;
                let sign_in_error = self.sign_in_error.clone();
                let show_setup = self.oauth_setup;
                let a = &mut self.store.account;
                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Button::new(RichText::new("  G   Użyj Gmaila  ").color(Color32::WHITE)).fill(PRIMARY))
                        .on_hover_text("Wpisz ustawienia serwerów Gmaila i logowanie przez Google")
                        .clicked()
                    {
                        a.use_gmail();
                    }
                    ui.label(RichText::new("lub wpisz ustawienia swojego dostawcy").small().color(MUTED()));
                });
                ui.add_space(8.0);
                egui::Grid::new("accgrid-top").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("Imię i nazwisko");
                    ui.add(egui::TextEdit::singleline(&mut a.display_name).desired_width(300.0));
                    ui.end_row();
                    ui.label("Adres e-mail");
                    let r = ui.add(egui::TextEdit::singleline(&mut a.email).hint_text("ty@gmail.com").desired_width(300.0));
                    if r.changed() && a.imap_host == "imap.gmail.com" {
                        a.username = a.email.clone();
                    }
                    ui.end_row();
                    ui.label("Logowanie");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut a.use_oauth, true, "Konto Google");
                        ui.radio_value(&mut a.use_oauth, false, "Hasło");
                    });
                    ui.end_row();
                });
                ui.add_space(6.0);
                if a.use_oauth {
                    if !a.oauth_refresh_token.is_empty() {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("✓  Zalogowano przez Google jako {}", a.email))
                                    .color(Color32::from_rgb(0x2E, 0x9E, 0x5B)),
                            );
                            if ui.small_button("Wyloguj").clicked() {
                                a.oauth_refresh_token.clear();
                            }
                        });
                    } else {
                        ui.horizontal(|ui| {
                            let btn = egui::Button::new(RichText::new("   G    Zaloguj przez Google   ").size(15.0).color(TEXT()))
                                .fill(BG())
                                .stroke(Stroke::new(1.0, BORDER()))
                                .min_size(Vec2::new(260.0, 38.0));
                            if ui.add_enabled(!signing_in, btn).clicked() {
                                start_sign_in = true;
                            }
                            if signing_in {
                                ui.spinner();
                                ui.label(RichText::new("Dokończ logowanie w przeglądarce…").color(MUTED()));
                            }
                        });
                    }
                    if let Some(e) = &sign_in_error {
                        ui.colored_label(FLAG, e);
                    }
                    if show_setup && a.oauth_client_id.trim().is_empty() {
                        ui.add_space(6.0);
                        Frame::new()
                            .fill(SURFACE())
                            .stroke(Stroke::new(1.0, BORDER()))
                            .corner_radius(CornerRadius::same(6))
                            .inner_margin(Margin::same(10))
                            .show(ui, |ui| {
                                ui.label(RichText::new("Jednorazowa konfiguracja (ok. 5 minut)").font(semibold(14.0)));
                                ui.label(
                                    RichText::new(
                                        "Google wymaga, by każda aplikacja miała własny identyfikator. Oryginalny program \
                                         dostał go od swojego producenta; tę aplikację trzeba raz zarejestrować na swoim koncie.",
                                    )
                                    .small()
                                    .color(MUTED()),
                                );
                                ui.add_space(4.0);
                                let steps: [(&str, &str); 4] = [
                                    ("1. Utwórz projekt w Google Cloud", "https://console.cloud.google.com/projectcreate"),
                                    ("2. Włącz Gmail API", "https://console.cloud.google.com/apis/library/gmail.googleapis.com"),
                                    ("3. Ekran zgody: Zewnętrzny, dodaj siebie jako testera", "https://console.cloud.google.com/auth/audience"),
                                    ("4. Utwórz klienta „Aplikacja komputerowa” i pobierz plik JSON", "https://console.cloud.google.com/auth/clients"),
                                ];
                                for (label, url) in steps {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new(label).small());
                                        if ui.small_button("Otwórz ↗").clicked() {
                                            crate::oauth::open_url(url);
                                        }
                                    });
                                }
                                ui.label(
                                    RichText::new("5. Pobrany plik zostanie znaleziony sam (folder Pobrane) — albo przeciągnij go na to okno.")
                                        .small(),
                                );
                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Bez konfiguracji:").small().color(MUTED()));
                                    if ui.link(RichText::new("zaloguj hasłem aplikacji").small()).clicked() {
                                        a.use_oauth = false;
                                        crate::oauth::open_url("https://myaccount.google.com/apppasswords");
                                    }
                                });
                            });
                    }
                    egui::CollapsingHeader::new(RichText::new("Zaawansowane").small().color(MUTED()))
                        .id_salt("oauth-advanced")
                        .show(ui, |ui| {
                            egui::Grid::new("accgrid-oauth").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                                ui.label(RichText::new("Client ID").small());
                                ui.add(
                                    egui::TextEdit::singleline(&mut a.oauth_client_id)
                                        .hint_text("….apps.googleusercontent.com")
                                        .desired_width(280.0),
                                );
                                ui.end_row();
                                ui.label(RichText::new("Client secret").small());
                                ui.add(
                                    egui::TextEdit::singleline(&mut a.oauth_client_secret)
                                        .password(true)
                                        .hint_text("GOCSPX-…")
                                        .desired_width(280.0),
                                );
                                ui.end_row();
                            });
                        });
                } else {
                    egui::Grid::new("accgrid-pass").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        ui.label("Hasło");
                        ui.add(
                            egui::TextEdit::singleline(&mut a.password)
                                .password(true)
                                .hint_text("Gmail: 16-znakowe hasło aplikacji")
                                .desired_width(300.0),
                        );
                        ui.end_row();
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Hasło jest zapisane w zaszyfrowanej postaci (tylko dla Twojego konta Windows).").small().color(MUTED()));
                        if a.imap_host == "imap.gmail.com" && ui.link(RichText::new("Utwórz hasło aplikacji ↗").small()).clicked() {
                            crate::oauth::open_url("https://myaccount.google.com/apppasswords");
                        }
                    });
                }
                ui.add_space(4.0);
                egui::CollapsingHeader::new("Ustawienia serwerów")
                    .id_salt("servers")
                    .default_open(!a.use_oauth && a.imap_host.is_empty())
                    .show(ui, |ui| {
                        egui::Grid::new("accgrid-servers").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                            ui.label("Serwer IMAP");
                            ui.horizontal(|ui| {
                                ui.add(egui::TextEdit::singleline(&mut a.imap_host).hint_text("imap.example.com").desired_width(210.0));
                                ui.add(egui::DragValue::new(&mut a.imap_port).range(1..=65535));
                            });
                            ui.end_row();
                            ui.label("Serwer SMTP");
                            ui.horizontal(|ui| {
                                ui.add(egui::TextEdit::singleline(&mut a.smtp_host).hint_text("smtp.example.com").desired_width(210.0));
                                ui.add(egui::DragValue::new(&mut a.smtp_port).range(1..=65535));
                            });
                            ui.end_row();
                            ui.label("Login");
                            ui.add(egui::TextEdit::singleline(&mut a.username).hint_text("zwykle adres e-mail").desired_width(290.0));
                            ui.end_row();
                        });
                    });
                ui.add_space(8.0);
                ui.separator();
                // Signature: taken from the person's own sent mail (Gmail adds it there)
                {
                    ui.label(RichText::new("Podpis").strong());
                    ui.checkbox(&mut self.store.account.signature_on, "Dodawaj podpis do nowych wiadomości i odpowiedzi");
                    if self.store.account.signature_text.trim().is_empty() {
                        ui.label(
                            RichText::new("Nie znaleziono jeszcze podpisu — pojawi się, gdy zsynchronizują się Twoje wiadomości wysłane.")
                                .small()
                                .color(MUTED()),
                        );
                    } else {
                        Frame::new().stroke(Stroke::new(1.0, BORDER())).corner_radius(CornerRadius::same(4)).inner_margin(Margin::same(8)).show(ui, |ui| {
                            egui::ScrollArea::vertical().id_salt("sig-settings").max_height(220.0).show(ui, |ui| self.signature_preview(ui));
                        });
                    }
                    let a = &mut self.store.account;
                    ui.horizontal(|ui| {
                        if ui.small_button("Pobierz ponownie z wysłanych").on_hover_text("Szuka podpisu w Twoich ostatnich wiadomościach wysłanych").clicked() {
                            a.signature_auto = true;
                            refresh_signature = true;
                        }
                        if !a.signature_text.is_empty() && ui.small_button("Usuń podpis").clicked() {
                            a.signature_text.clear();
                            a.signature_html.clear();
                            a.signature_images.clear();
                            a.signature_auto = false;
                        }
                    });
                    ui.label(RichText::new("Podpis jest brany z Twoich wiadomości wysłanych przez Gmaila, razem z wyglądem i zdjęciem.").small().color(MUTED()));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new(RichText::new("Zapisz i sprawdź pocztę").color(Color32::WHITE)).fill(ACCENT())).clicked() {
                        save_and_check = true;
                    }
                    if ui.button("Zamknij").clicked() {
                        self.account_open = false;
                    }
                });
            });
        if refresh_signature {
            self.sig_seen = 0;
            self.save_now = true;
        }
        if !open {
            self.account_open = false;
            self.save_now = true;
        }
        // While the one-time setup is shown, keep looking for Google's downloaded file.
        let now = ctx.input(|i| i.time);
        if self.oauth_setup && self.store.account.oauth_client_id.trim().is_empty() && now - self.last_scan > 2.0 {
            self.last_scan = now;
            if self.find_google_client() {
                self.oauth_setup = false;
                start_sign_in = true;
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(2));
        }
        if start_sign_in {
            if self.store.account.oauth_client_id.trim().is_empty() && !self.find_google_client() {
                // Nothing set up yet: show the short one-time setup instead of an error.
                self.oauth_setup = true;
            } else {
                self.signing_in = true;
                self.sign_in_error = None;
                let a = &self.store.account;
                net::google_sign_in(a.oauth_client_id.clone(), a.oauth_client_secret.clone(), self.tx.clone(), self.ctx.clone());
                self.set_status("Otwieram stronę logowania Google w przeglądarce…");
            }
        }
        if save_and_check {
            self.account_open = false;
            self.save_now = true;
            self.send_receive();
        }
    }

    fn keyboard(&mut self, ctx: &egui::Context) {
        // Delete pressed while reading inside the email
        for k in self.mail_view.keys() {
            if k == "clicked" {
                // the email was clicked: it has the keyboard, no field of the app is chosen
                self.email_clicked = self.clock();
                ctx.memory_mut(|m| {
                    if let Some(f) = m.focused() {
                        m.surrender_focus(f);
                    }
                });
                continue;
            }
            crate::diag::log(format!("key from the email view: {k}"));
            if k == "delete" && self.module == Module::Mail {
                if let Some(id) = self.selected {
                    self.actions.push(Action::Delete(id));
                }
            }
        }
        // A field of the app is chosen (search, …) but the keyboard is in the email view, which
        // nobody clicked: it took it by itself. After a moment (a real click reports itself
        // first) the keyboard goes back to the field.
        let field = ctx.memory(|m| m.focused().is_some());
        let inside = self.main_hwnd.is_some_and(|h| focus_state(h) == "inside it");
        if field && inside && self.clock() - self.email_clicked > 1.0 {
            let since = *self.kb_stray.get_or_insert(self.clock());
            if self.clock() - since > 0.4 {
                self.kb_stray = None;
                self.take_keyboard_back();
                crate::diag::log("keyboard: the email view had it without a click; back to the chosen field");
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        } else {
            self.kb_stray = None;
        }
        // a click on the app's own part (list, folders, buttons) takes the keyboard back from
        // the email view, so Delete and the arrows work on the list again
        if ctx.input(|i| i.pointer.any_pressed()) {
            self.take_keyboard_back();
        }
        if ctx.memory(|m| m.focused().is_some()) {
            return;
        }
        let (del, up, down, n, r) = ctx.input(|i| {
            (
                i.key_pressed(Key::Delete),
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::ArrowDown),
                i.modifiers.command && i.key_pressed(Key::N),
                i.modifiers.command && i.key_pressed(Key::R),
            )
        });
        if n {
            self.actions.push(Action::NewMessage);
        }
        if self.module != Module::Mail {
            return;
        }
        if let Some(id) = self.selected {
            if del {
                self.actions.push(Action::Delete(id));
            }
            if r {
                self.actions.push(Action::Reply(id, false));
            }
            if up || down {
                let ids = self.visible_ids();
                if let Some(p) = ids.iter().position(|x| *x == id) {
                    let np = if up { p.saturating_sub(1) } else { (p + 1).min(ids.len() - 1) };
                    self.actions.push(Action::Select(ids[np]));
                }
            }
        }
    }

    /// The keyboard back to the main window (from the email view inside it).
    fn take_keyboard_back(&self) {
        if let Some(h) = self.main_hwnd {
            if keyboard_to(h) {
                crate::diag::log("keyboard: taken back from the email view");
            }
        }
    }

    fn screenshot_step(&mut self, ctx: &egui::Context) {
        let Some(path) = self.shot.clone() else { return };
        self.frames += 1;
        ctx.request_repaint();
        let compose_shot = matches!(std::env::var("POCZTA_START").as_deref(), Ok("reply") | Ok("suggest")) && !self.composers.is_empty();
        if !compose_shot {
            shoot_step(ctx, &path, self.frames, &self.shot_image);
        }
        if self.frames > 900 + std::env::var("POCZTA_SHOT_FRAMES").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0) {
            eprintln!("Screenshot was not delivered in time");
            std::process::exit(1);
        }
    }
}

/// Picture-taking for `--screenshot`: asks for a picture of the current window after 120 frames,
/// saves it as PNG and closes the app.
fn shoot_step(ctx: &egui::Context, path: &std::path::Path, frames: u32, state: &ShotState) {
    let wait = std::env::var("POCZTA_SHOT_FRAMES").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(120);
    if frames >= wait {
        let mut s = state.lock().unwrap();
        if !s.0 {
            s.0 = true;
            let slot = state.clone();
            let ctx2 = ctx.clone();
            ctx.request_screenshot(move |img| {
                slot.lock().unwrap().1 = Some(img);
                ctx2.request_repaint();
            });
        }
    }
    let img = state.lock().unwrap().1.take();
    if let Some(img) = img {
        let [w, h] = img.size;
        let mut bytes = Vec::with_capacity(w * h * 4);
        for p in &img.pixels {
            bytes.extend_from_slice(&p.to_srgba_unmultiplied());
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match image::save_buffer(path, &bytes, w as u32, h as u32, image::ColorType::Rgba8) {
            Ok(()) => println!("Saved screenshot to {}", path.display()),
            Err(e) => eprintln!("Could not save screenshot: {e}"),
        }
        std::process::exit(0);
    }
}

type ShotState = Arc<Mutex<(bool, Option<Arc<egui::ColorImage>>)>>;

#[derive(Clone, Copy, PartialEq)]
enum WinButton {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// Minimize / maximize / close, drawn inside the app's own top bar.
fn window_button(ui: &mut Ui, kind: WinButton) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(44.0, 34.0), Sense::click());
    let p = ui.painter();
    let hot = resp.hovered();
    if hot {
        let fill = if kind == WinButton::Close { Color32::from_rgb(0xE8, 0x11, 0x23) } else { SELECTED() };
        p.rect_filled(r, CornerRadius::same(4), fill);
    }
    let col = if hot && kind == WinButton::Close { Color32::WHITE } else { ICON() };
    let s = Stroke::new(1.2, col);
    let c = r.center();
    let h = 5.5;
    match kind {
        WinButton::Minimize => {
            p.hline(c.x - h..=c.x + h, c.y, s);
        }
        WinButton::Maximize => {
            p.rect_stroke(Rect::from_center_size(c, Vec2::splat(h * 2.0)), CornerRadius::same(1), s, egui::StrokeKind::Middle);
        }
        WinButton::Restore => {
            let back = Rect::from_center_size(c + Vec2::new(2.0, -2.0), Vec2::splat(h * 1.7));
            let front = Rect::from_center_size(c + Vec2::new(-1.5, 1.5), Vec2::splat(h * 1.7));
            p.hline(back.left()..=back.right(), back.top(), s);
            p.vline(back.right(), back.top()..=back.bottom(), s);
            p.rect_filled(front, CornerRadius::ZERO, if hot { SELECTED() } else { BG() });
            p.rect_stroke(front, CornerRadius::same(1), s, egui::StrokeKind::Middle);
        }
        WinButton::Close => {
            p.line_segment([c + Vec2::new(-h, -h), c + Vec2::new(h, h)], s);
            p.line_segment([c + Vec2::new(-h, h), c + Vec2::new(h, -h)], s);
        }
    }
    resp
}

/// Without the system frame the window needs its own resize edges and a thin outline.
fn window_edges(ui: &mut Ui) {
    use egui::{CursorIcon, ResizeDirection as D};
    let ctx = ui.ctx().clone();
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    let screen = ctx.content_rect();
    if !maximized {
        ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, id("window-outline")))
            .rect_stroke(screen, CornerRadius::ZERO, Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
    } else {
        return;
    }
    let e = 5.0;
    let c = 12.0;
    let (l, r, t, b) = (screen.left(), screen.right(), screen.top(), screen.bottom());
    let zones = [
        (Rect::from_min_max(Pos2::new(l, t), Pos2::new(l + c, t + c)), D::NorthWest, CursorIcon::ResizeNorthWest),
        (Rect::from_min_max(Pos2::new(r - c, t), Pos2::new(r, t + c)), D::NorthEast, CursorIcon::ResizeNorthEast),
        (Rect::from_min_max(Pos2::new(l, b - c), Pos2::new(l + c, b)), D::SouthWest, CursorIcon::ResizeSouthWest),
        (Rect::from_min_max(Pos2::new(r - c, b - c), Pos2::new(r, b)), D::SouthEast, CursorIcon::ResizeSouthEast),
        (Rect::from_min_max(Pos2::new(l + c, t), Pos2::new(r - c, t + e)), D::North, CursorIcon::ResizeNorth),
        (Rect::from_min_max(Pos2::new(l + c, b - e), Pos2::new(r - c, b)), D::South, CursorIcon::ResizeSouth),
        (Rect::from_min_max(Pos2::new(l, t + c), Pos2::new(l + e, b - c)), D::West, CursorIcon::ResizeWest),
        (Rect::from_min_max(Pos2::new(r - e, t + c), Pos2::new(r, b - c)), D::East, CursorIcon::ResizeEast),
    ];
    for (i, (rect, dir, cursor)) in zones.into_iter().enumerate() {
        let resp = ui.interact(rect, id(("resize-edge", i)), Sense::drag()).on_hover_cursor(cursor);
        if resp.drag_started_by(egui::PointerButton::Primary) {
            ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
        }
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
        // the same for the main window (the keyboard back from the email view)
        if self.main_hwnd.is_some_and(|h| focus_state(h) == "this window") && !ctx.input(|i| i.focused) {
            ctx.input_mut(|i| i.focused = true);
        }
        self.read_after_a_while(&ctx);
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
        self.take_dropped_files(&ctx);
        self.keyboard(&ctx);

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
        let covered = self.account_open || self.extra_edit.is_some() || self.ai_open || self.gcal_open || self.diag_open || self.editor.is_some() || self.module != Module::Mail;
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
        let t = std::time::Instant::now();
        drop(self.db.take());
        crate::diag::took("closing: database closed", t, "");
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

/// After reading the mailbox: the saved password back in memory, and message texts that older
/// versions kept in separate files.
fn prepare_store(store: &mut Store) {
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

/// The mailbox being read from disk: the database and what was in it.
pub type Loading = Receiver<(Option<crate::db::Db>, Option<Store>, Option<Vec<crate::search::Entry>>)>;

/// Starts reading the mailbox in the background. Called as early as possible (before the
/// window is even made), so the mail is ready about when the window is.
pub fn start_loading() -> Loading {
    let (ltx, lrx) = channel();
    std::thread::spawn(move || {
        let t = std::time::Instant::now();
        let mut db = crate::db::Db::open().map_err(|e| crate::diag::log(format!("database error: {e}"))).ok();
        crate::diag::took("  database: opened", t, "");
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
        let _ = ltx.send((db, store, entries));
    });
    lrx
}

/// A hint shown above a button instead of below it. The formatted email is shown by a separate
/// browser view that always lies on top of the app's own drawing, so a hint pointing down into
/// it would be hidden.
/// Where the mouse pointer is on the screen (pixels).
fn cursor_at() -> Option<(i32, i32)> {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::POINT;
        use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let mut p = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut p) != 0 {
            return Some((p.x, p.y));
        }
    }
    None
}

/// Where the keyboard is now, for the log ("this window", "inside it", "elsewhere", "none").
fn focus_state(h: isize) -> &'static str {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetFocus;
        use windows_sys::Win32::UI::WindowsAndMessaging::IsChild;
        let now = GetFocus();
        return if now.is_null() {
            "none"
        } else if now as isize == h {
            "this window"
        } else if IsChild(h as _, now) != 0 {
            "inside it"
        } else {
            "elsewhere"
        };
    }
    #[allow(unreachable_code)]
    {
        let _ = h;
        "unknown"
    }
}

/// The keyboard to window `h` itself (it was clicked), wherever it is now.
fn window_keyboard(h: isize) {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
        SetFocus(h as _);
    }
    let _ = h;
}

/// The keyboard to window `h`, when it is in a browser view inside it (a click on the app's own
/// part of that window). Never from another window. True when it moved.
fn keyboard_to(h: isize) -> bool {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
        use windows_sys::Win32::UI::WindowsAndMessaging::IsChild;
        let now = GetFocus();
        if !now.is_null() && now as isize != h && IsChild(h as _, now) != 0 {
            SetFocus(h as _);
            return true;
        }
    }
    let _ = h;
    false
}

enum TrayMsg {
    Show,
    Quit,
}

/// The icon next to the clock: a click brings the window back, its menu shows or quits.
fn make_tray(ctx: &egui::Context) -> (Option<tray_icon::TrayIcon>, Receiver<TrayMsg>) {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    let (tx, rx) = channel();
    let show = MenuItem::new("Pokaż Pocztę", true, None);
    let quit = MenuItem::new("Zakończ", true, None);
    let menu = Menu::new();
    let _ = menu.append_items(&[&show, &PredefinedMenuItem::separator(), &quit]);
    let big = image::RgbaImage::from_raw(crate::icon::SIZE, crate::icon::SIZE, crate::icon::rgba());
    let icon = big
        .map(|b| image::imageops::resize(&b, 32, 32, image::imageops::FilterType::Lanczos3))
        .and_then(|small| tray_icon::Icon::from_rgba(small.into_raw(), 32, 32).ok());
    let mut builder = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("Poczta").with_menu_on_left_click(false);
    if let Some(i) = icon {
        builder = builder.with_icon(i);
    }
    let tray = match builder.build() {
        Ok(t) => Some(t),
        Err(e) => {
            crate::diag::log(format!("tray icon not available: {e}"));
            None
        }
    };
    let (quit_id, t2, c2) = (quit.id().clone(), tx.clone(), ctx.clone());
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let _ = t2.send(if e.id == quit_id { TrayMsg::Quit } else { TrayMsg::Show });
        c2.request_repaint();
    }));
    let c3 = ctx.clone();
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
            let _ = tx.send(TrayMsg::Show);
            c3.request_repaint();
        }
    }));
    (tray, rx)
}

/// The window is minimized or (while minimizing) tiny: its sizes are not worth remembering.
fn minimized_or_tiny(ctx: &egui::Context) -> bool {
    let (min, size) = ctx.input(|i| (i.viewport().minimized.unwrap_or(false), i.content_rect().size()));
    min || size.x < 700.0 || size.y < 400.0
}
