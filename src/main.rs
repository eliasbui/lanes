// No console window behind the app on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod browser;
mod engine;
mod extract;
mod hls;
mod menus;
mod netmerge;
mod store;
mod ytdlp;
mod update;

use engine::Engine;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs::File,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget},
    window::{Icon, Theme, Window, WindowBuilder},
};
use tray_icon::{
    MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{MenuEvent, MenuId},
};
use wry::{WebView, WebViewBuilder, http::Request};

const APP_NAME: &str = "Lanes";
const INDEX_HTML: &str = include_str!("../ui/index.html");
const WINDOW_ICON: &[u8] = include_bytes!("../assets/icon-256.png");
#[cfg(target_os = "macos")]
const TRAY_ICON: &[u8] = include_bytes!("../assets/tray.png");
#[cfg(not(target_os = "macos"))]
const TRAY_ICON: &[u8] = include_bytes!("../assets/icon-64.png");
/// How often the list of downloads and their speeds are pushed to the page.
const TICK: Duration = Duration::from_millis(500);
/// How often the inbox from the browser extension is checked.
const INBOX_EVERY: Duration = Duration::from_secs(2);
/// How often the clipboard is looked at while it is watched.
const CLIPBOARD_EVERY: Duration = Duration::from_millis(1500);
/// How long the computer waits to sleep or shut down once the downloads are done, for the user to call it off.
const AFTER_WAIT: i64 = 60;
/// The most files one batch link stands for.
const MAX_BATCH: usize = 1000;

/// Whether the app speaks English rather than Vietnamese in its own words: the menus, notifications and messages it
/// makes. The page keeps its own copy of its words (ui/index.html).
pub(crate) static ENGLISH: AtomicBool = AtomicBool::new(false);

/// `vi`, or `en` when the app speaks English.
pub(crate) fn tr<'a>(vi: &'a str, en: &'a str) -> &'a str {
    if ENGLISH.load(Ordering::Relaxed) { en } else { vi }
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn data_dir() -> PathBuf {
    let env = |key: &str| std::env::var_os(key).map(PathBuf::from);
    let home = env("HOME").or_else(|| env("USERPROFILE")).unwrap_or_else(|| ".".into());
    if cfg!(windows) {
        env("APPDATA").unwrap_or(home).join("Lanes")
    } else if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Lanes")
    } else {
        env("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share")).join("lanes")
    }
}

fn downloads_dir() -> PathBuf {
    dirs::download_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| ".".into()).join("Downloads"))
}

/// What the user can change, kept in a small file beside the list. A field missing from an older file takes its
/// default, so an update keeps the rest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    download_dir: String,
    connections: usize,
    /// Files go into IDM's category folders inside the download folder (`engine::category`).
    classify: bool,
    /// net-merge: the connections of a download are spread over every network link (src/netmerge.rs).
    net_merge: bool,
    /// The most downloads that run at once, the others waiting in the queue; 0 for no limit.
    max_active: usize,
    /// The speed limit over every download together, in bytes a second; 0 for none.
    speed_limit: u64,
    /// When the queue starts, in seconds since 1970, which the page works out from the local time; 0 to start at once.
    /// Once that time comes it goes back to 0.
    start_at: i64,
    /// What the computer does once nothing is left to download: "", "sleep" or "shutdown". Done once, then "".
    when_done: String,
    /// A link to a file copied anywhere opens the add sheet with it, as IDM does.
    watch_clipboard: bool,
    /// "vi" or "en".
    language: String,
    /// "system", "light" or "dark".
    theme: String,
    /// The window drawn in Liquid Glass (beta): see-through panels over a coloured backdrop.
    glass: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            download_dir: downloads_dir().to_string_lossy().into_owned(),
            connections: engine::DEFAULT_CONNECTIONS,
            classify: true,
            net_merge: false,
            max_active: 3,
            speed_limit: 0,
            start_at: 0,
            when_done: String::new(),
            watch_clipboard: false,
            language: "vi".into(),
            theme: "system".into(),
            glass: false,
        }
    }
}

impl Settings {
    fn load(dir: &Path) -> Settings {
        std::fs::read_to_string(dir.join("settings.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    fn save(&self, dir: &Path) {
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(dir.join("settings.json"), text);
        }
    }
}

/// A download to add, once its name and size are known from a probe. `sources` holds the net-merge mirror pool with
/// the primary first; an ordinary download has one entry. `kind` says what the URL is.
struct AddReq {
    sources: Vec<String>,
    filename: String,
    size: Option<u64>,
    headers: Vec<(String, String)>,
    kind: engine::Kind,
}

impl AddReq {
    fn url(&self) -> &str {
        self.sources.first().map(String::as_str).unwrap_or_default()
    }
}

enum UserEvent {
    Ipc(String),
    Post(Value),
    Added(AddReq),
    Menu(MenuId),
    TrayClick,
    Tick,
    /// A link to a file the user copied, while the clipboard is watched.
    Copied(String),
}

fn main() {
    let dir = data_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("cannot make {}: {e}", dir.display());
        std::process::exit(1);
    }
    // Started by a browser as the host of the extension, which passes its origin: no window, only the job handed over.
    if std::env::args().skip(1).any(|a| a.starts_with(browser::ORIGIN)) {
        browser::host(&dir);
        return;
    }
    // One copy at a time, or two would fight over the same files.
    let lock = File::create(dir.join("app.lock")).and_then(|f| f.try_lock().map(|_| f).map_err(Into::into));
    let Ok(_lock) = lock else {
        eprintln!("{APP_NAME} is already running");
        return;
    };
    let db = match store::open(&dir.join("downloads.db")) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("cannot open the download list: {e}");
            std::process::exit(1);
        }
    };
    let settings = Settings::load(&dir);
    ENGLISH.store(settings.language == "en", Ordering::Relaxed);
    let engine = Arc::new(Engine::new());
    engine.set_connections(settings.connections);
    engine.set_net_merge(settings.net_merge);
    engine.set_speed_limit(settings.speed_limit);
    let watching = Arc::new(AtomicBool::new(settings.watch_clipboard));
    let background = std::env::args().any(|a| a == autostart::BACKGROUND);
    // Notifications come from Lanes. Left unset, the library looks for an app named "use_default" through AppleScript
    // before the first one, and macOS asks the user where that app is.
    #[cfg(target_os = "macos")]
    let _ = notify_rust::set_application("com.eliasbui.lanes");

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    {
        let proxy = std::sync::Mutex::new(proxy.clone());
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let _ = proxy.lock().map(|p| p.send_event(UserEvent::Menu(e.id)));
        }));
    }
    {
        let proxy = std::sync::Mutex::new(proxy.clone());
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                let _ = proxy.lock().map(|p| p.send_event(UserEvent::TrayClick));
            }
        }));
    }
    // Tell each browser where the native host is, in case the app moved, and put this version's extension where the
    // browser loads it from.
    {
        let dir = dir.clone();
        thread::spawn(move || {
            browser::register(&dir);
            if let Err(e) = browser::install_extension(&dir) {
                eprintln!("extension: {e}");
            }
        });
    }
    // Jobs the browser extension left, now and while the app runs.
    {
        let (dir, proxy) = (dir.clone(), proxy.clone());
        thread::spawn(move || {
            loop {
                for job in browser::take(&dir) {
                    let headers = job.headers();
                    // An HLS playlist has no size until its segments are in; the extension names it with its type. A
                    // file the browser had not named yet takes the server's name (Content-Disposition) from the probe.
                    // A video page's size is the estimate the extension showed.
                    let kind = job.kind();
                    let probe = if kind == engine::Kind::File { engine::probe(&job.url, &headers).ok() } else { None };
                    let size = probe.as_ref().and_then(|p| p.size).or((job.size > 0).then_some(job.size));
                    let filename = match job.filename {
                        name if !name.is_empty() => name,
                        _ if kind != engine::Kind::File => "video.mp4".into(),
                        _ => probe.map(|p| p.filename).unwrap_or_else(|| engine::filename_from(&job.url, None)),
                    };
                    let _ = proxy.send_event(UserEvent::Added(AddReq { sources: vec![job.url], filename, size, headers, kind }));
                }
                thread::sleep(INBOX_EVERY);
            }
        });
    }
    // The clipboard, while copied links to files are offered for download.
    {
        let (watching, proxy) = (watching.clone(), proxy.clone());
        thread::spawn(move || watch_clipboard(&watching, &proxy));
    }
    // The steady heartbeat that refreshes the list and its speeds.
    {
        let proxy = proxy.clone();
        thread::spawn(move || {
            loop {
                thread::sleep(TICK);
                if proxy.send_event(UserEvent::Tick).is_err() {
                    break;
                }
            }
        });
    }

    let mut app = App {
        dir,
        db,
        engine,
        settings,
        proxy,
        window: None,
        page_ready: false,
        queued: Vec::new(),
        tray: None,
        tray_menu: None,
        status: String::new(),
        speeds: HashMap::new(),
        published: String::new(),
        watching,
        busy: false,
        after_at: 0,
        #[cfg(target_os = "macos")]
        menu_bar: menus::menu_bar(),
    };
    // Pick up where downloads that were running last time left off.
    app.resume_unfinished();

    event_loop.run(move |event, target, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(tao::event::StartCause::Init) => {
                #[cfg(target_os = "macos")]
                if let Some(bar) = &app.menu_bar {
                    bar.install();
                }
                app.make_tray();
                if !background {
                    app.show(target);
                }
            }
            #[cfg(target_os = "macos")]
            Event::Reopen { .. } => app.show(target),
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                // The app stays in the tray so downloads keep running.
                app.window = None;
                app.page_ready = false;
            }
            Event::UserEvent(UserEvent::Ipc(body)) => app.ipc(&body, target),
            Event::UserEvent(UserEvent::Post(message)) => app.post(&message),
            Event::UserEvent(UserEvent::Added(req)) => app.add(req),
            Event::UserEvent(UserEvent::Tick) => app.tick(),
            Event::UserEvent(UserEvent::TrayClick) => app.show(target),
            Event::UserEvent(UserEvent::Copied(url)) => {
                app.show(target);
                app.post_ready(json!({ "event": "add", "data": url }));
            }
            Event::UserEvent(UserEvent::Menu(id)) if id == menus::QUIT => *flow = ControlFlow::Exit,
            Event::UserEvent(UserEvent::Menu(id)) => app.menu(id.as_ref(), target),
            _ => {}
        }
    });
}

struct App {
    dir: PathBuf,
    db: Connection,
    engine: Arc<Engine>,
    settings: Settings,
    proxy: EventLoopProxy<UserEvent>,
    window: Option<(Window, WebView)>,
    /// Whether the window's page has said it is ready, and what waits to be posted to it until then.
    page_ready: bool,
    queued: Vec<Value>,
    tray: Option<TrayIcon>,
    tray_menu: Option<menus::Tray>,
    /// The status line last shown in the tray, so it is set only when it changes.
    status: String,
    /// Per-download bytes-and-time of the last tick, to work out the speed.
    speeds: HashMap<i64, (u64, Instant)>,
    /// The progress last written for the browser extension, so an unchanged list is not written again.
    published: String,
    /// Whether the clipboard is watched, shared with its thread.
    watching: Arc<AtomicBool>,
    /// Whether anything ran or waited at the last tick, and when the computer sleeps or shuts down (`when_done`); 0
    /// while it does not.
    busy: bool,
    after_at: i64,
    /// The menu bar, made again when the language changes.
    #[cfg(target_os = "macos")]
    menu_bar: Option<menus::MenuBar>,
}

impl App {
    fn make_tray(&mut self) {
        let menu = menus::tray(autostart::enabled());
        let icon = decode_png(TRAY_ICON).and_then(|(rgba, w, h)| tray_icon::Icon::from_rgba(rgba, w, h).ok());
        let mut builder = TrayIconBuilder::new().with_menu(Box::new(menu.menu.clone())).with_tooltip(APP_NAME);
        if let Some(icon) = icon {
            #[cfg(target_os = "macos")]
            {
                builder = builder.with_icon_templated(icon);
            }
            #[cfg(not(target_os = "macos"))]
            {
                builder = builder.with_icon(icon);
            }
        }
        match builder.build() {
            Ok(tray) => self.tray = Some(tray),
            Err(e) => eprintln!("tray: {e}"),
        }
        self.tray_menu = Some(menu);
    }

    /// The tray's menu and the menu bar in the language just chosen; the tray keeps its icon.
    fn relabel(&mut self) {
        let menu = menus::tray(autostart::enabled());
        if let Some(tray) = &self.tray {
            tray.set_menu(Some(Box::new(menu.menu.clone())));
        }
        self.tray_menu = Some(menu);
        self.status.clear();
        #[cfg(target_os = "macos")]
        {
            self.menu_bar = menus::menu_bar();
            if let Some(bar) = &self.menu_bar {
                bar.install();
            }
        }
    }

    /// An entry of the tray's menu or of the menu bar; Quit is handled by the event loop.
    fn menu(&mut self, id: &str, target: &EventLoopWindowTarget<UserEvent>) {
        match id {
            menus::OPEN => self.show(target),
            menus::ADD_COPIED => {
                let proxy = self.proxy.clone();
                thread::spawn(move || match clipboard_link() {
                    Some(url) => add_probed(&proxy, vec![url]),
                    None => {
                        let _ = notify_rust::Notification::new().summary(APP_NAME).body(tr("Chưa sao chép liên kết http hoặc https nào", "No http or https link has been copied")).show();
                    }
                });
            }
            menus::FOLDER => {
                let _ = open_path(Path::new(&self.settings.download_dir));
            }
            menus::PAUSE_ALL | menus::RESUME_ALL => {
                for row in store::list(&self.db).unwrap_or_default() {
                    match row.status {
                        engine::state::RUNNING | engine::state::QUEUED if id == menus::PAUSE_ALL => self.pause(row.id),
                        engine::state::PAUSED if id == menus::RESUME_ALL => self.resume(row.id),
                        _ => {}
                    }
                }
                self.push_list();
            }
            menus::AUTOSTART => {
                let on = self.tray_menu.as_ref().is_some_and(|t| t.autostart.is_checked());
                if let Err(e) = autostart::set(on) {
                    self.fail(e);
                }
                self.sync_autostart();
            }
            menus::SETTINGS => {
                self.show(target);
                self.post_ready(json!({ "event": "settings" }));
            }
            #[cfg(target_os = "macos")]
            menus::HOME_PAGE => {
                let _ = open_path(Path::new(&format!("https://github.com/{}", update::REPO)));
            }
            #[cfg(target_os = "macos")]
            menus::REPORT => {
                let _ = open_path(Path::new(&format!("https://github.com/{}/issues", update::REPO)));
            }
            _ => {}
        }
    }

    /// The tray's switch and the page's follow whether Lanes starts with the computer.
    fn sync_autostart(&self) {
        let on = autostart::enabled();
        if let Some(menu) = &self.tray_menu {
            menu.autostart.set_checked(on);
        }
        self.post(&json!({ "event": "autostart", "data": on }));
    }

    /// Posts to the page once it has loaded: at once when it has, else after its `ready`.
    fn post_ready(&mut self, message: Value) {
        if self.page_ready {
            self.post(&message);
        } else {
            self.queued.push(message);
        }
    }

    fn pause(&self, id: i64) {
        self.engine.pause(id);
        let _ = store::set_status(&self.db, id, engine::state::PAUSED, "");
    }

    /// A download resumed goes back in the queue, and starts at once when there is room.
    fn resume(&mut self, id: i64) {
        let _ = store::set_status(&self.db, id, engine::state::QUEUED, "");
        self.start_queued();
    }

    /// Starts the queued downloads, oldest first, while fewer than the most at once run and the schedule has come. A
    /// queued download is not in the engine: its row says all it needs to start.
    fn start_queued(&mut self) {
        if self.settings.start_at > now() {
            return;
        }
        if self.settings.start_at != 0 {
            self.settings.start_at = 0;
            self.settings.save(&self.dir);
            self.post(&json!({ "event": "schedule", "data": 0 }));
        }
        let limit = match self.settings.max_active {
            0 => usize::MAX,
            n => n,
        };
        let mut running = self.engine.snapshots().iter().filter(|s| s.state == engine::state::RUNNING).count();
        for row in store::list(&self.db).unwrap_or_default().iter().rev().filter(|r| r.status == engine::state::QUEUED) {
            if running >= limit {
                break;
            }
            let _ = store::set_status(&self.db, row.id, engine::state::RUNNING, "");
            self.engine.start(row.id, row.source_list(), row.final_path(), row.header_pairs(), row.kind());
            running += 1;
        }
    }

    /// Once nothing runs or waits any more, the computer sleeps or shuts down as `when_done` asks, a minute later so
    /// the user can still call it off; it is done once, then `when_done` is cleared.
    fn after_done(&mut self, items: &[Value]) {
        let busy = items.iter().any(|i| matches!(i["state"].as_u64().map(|s| s as u8), Some(engine::state::QUEUED | engine::state::RUNNING)));
        let was_busy = std::mem::replace(&mut self.busy, busy);
        if self.settings.when_done.is_empty() || busy {
            self.after_at = 0;
        } else if was_busy {
            self.after_at = now() + AFTER_WAIT;
            let message = match (self.settings.when_done == "shutdown", ENGLISH.load(Ordering::Relaxed)) {
                (true, false) => "Đã tải xong. Lanes sẽ tắt máy sau một phút, chọn Không làm gì trong Cài đặt để hủy.",
                (false, false) => "Đã tải xong. Lanes sẽ cho máy ngủ sau một phút, chọn Không làm gì trong Cài đặt để hủy.",
                (true, true) => "Downloads are done. Lanes shuts the computer down in a minute; choose Do nothing in Settings to cancel.",
                (false, true) => "Downloads are done. Lanes puts the computer to sleep in a minute; choose Do nothing in Settings to cancel.",
            };
            let _ = notify_rust::Notification::new().summary(APP_NAME).body(&message).show();
            self.post(&json!({ "event": "toast", "data": message }));
        } else if self.after_at != 0 && now() >= self.after_at {
            self.after_at = 0;
            let action = std::mem::take(&mut self.settings.when_done);
            self.settings.save(&self.dir);
            self.post(&json!({ "event": "when_done", "data": "" }));
            if let Err(e) = power(&action) {
                self.fail(e);
            }
        }
    }

    fn show(&mut self, target: &EventLoopWindowTarget<UserEvent>) {
        if let Some((window, _)) = &self.window {
            window.set_visible(true);
            window.set_minimized(false);
            window.set_focus();
            return;
        }
        self.page_ready = false;
        match self.open_window(target) {
            Ok(pair) => self.window = Some(pair),
            Err(e) => eprintln!("window: {e}"),
        }
    }

    fn open_window(&self, target: &EventLoopWindowTarget<UserEvent>) -> Result<(Window, WebView), String> {
        let icon = decode_png(WINDOW_ICON).and_then(|(rgba, w, h)| Icon::from_rgba(rgba, w, h).ok());
        let builder = WindowBuilder::new()
            .with_title(APP_NAME)
            .with_inner_size(LogicalSize::new(940.0, 640.0))
            .with_min_inner_size(LogicalSize::new(640.0, 480.0))
            .with_window_icon(icon)
            .with_theme(self.window_theme());
        // On macOS the page runs up under a see-through title bar, so its sidebar holds the window's buttons as Finder's
        // does, and its toolbar moves the window (ipc "drag").
        #[cfg(target_os = "macos")]
        let builder = {
            use tao::platform::macos::WindowBuilderExtMacOS;
            builder.with_titlebar_transparent(true).with_title_hidden(true).with_fullsize_content_view(true).with_traffic_light_inset(tao::dpi::LogicalPosition::new(20.0, 20.0))
        };
        let window = builder.build(target).map_err(|e| e.to_string())?;
        let proxy = self.proxy.clone();
        let builder = WebViewBuilder::new()
            .with_html(page(&self.settings))
            .with_ipc_handler(move |request: Request<String>| {
                let _ = proxy.send_event(UserEvent::Ipc(request.into_body()));
            })
            .with_devtools(cfg!(debug_assertions));
        #[cfg(not(target_os = "linux"))]
        let webview = builder.build(&window);
        #[cfg(target_os = "linux")]
        let webview = {
            use tao::platform::unix::WindowExtUnix;
            use wry::WebViewBuilderExtUnix;
            builder.build_gtk(window.default_vbox().ok_or("no GTK box in the window")?)
        };
        Ok((window, webview.map_err(|e| e.to_string())?))
    }

    fn post(&self, message: &Value) {
        if let Some((_, webview)) = &self.window {
            let _ = webview.evaluate_script(&format!("window.app && app.receive({message})"));
        }
    }

    /// The window's own light or dark, for what the page does not draw: the window's buttons, the system's menus.
    fn window_theme(&self) -> Option<Theme> {
        match self.settings.theme.as_str() {
            "light" => Some(Theme::Light),
            "dark" => Some(Theme::Dark),
            _ => None,
        }
    }

    fn theme_name(&self) -> &'static str {
        match self.window.as_ref().map(|(w, _)| w.theme()) {
            Some(Theme::Light) => "light",
            _ => "dark",
        }
    }

    /// Puts every download that was still going when the app last closed back in the queue, which starts them.
    fn resume_unfinished(&mut self) {
        for row in store::list(&self.db).unwrap_or_default() {
            if row.status == engine::state::RUNNING {
                let _ = store::set_status(&self.db, row.id, engine::state::QUEUED, "");
            }
        }
        self.start_queued();
    }

    /// One or more URLs typed or pasted into the page. Several URLs are the mirrors of one file (net-merge, beta): the
    /// first is probed for the name and size, and all of them become the download's sources. The probe runs off the
    /// event loop.
    fn add_sources(&self, urls: Vec<String>) {
        let sources: Vec<String> = urls.into_iter().map(|u| u.trim().to_owned()).filter(|u| !u.is_empty()).collect();
        if !sources.iter().all(|u| u.starts_with("http://") || u.starts_with("https://")) || sources.is_empty() {
            self.post(&json!({ "event": "toast", "data": tr("Hãy dán liên kết http hoặc https", "Paste an http or https link") }));
            return;
        }
        let proxy = self.proxy.clone();
        thread::spawn(move || add_probed(&proxy, sources));
    }

    /// Records a probed download, starts it, and shows it in the list.
    fn add(&mut self, req: AddReq) {
        let mut dir = PathBuf::from(&self.settings.download_dir);
        if let Some(folder) = engine::category(&req.filename).filter(|_| self.settings.classify) {
            dir.push(folder);
        }
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return self.fail(format!("{} {}: {e}", tr("Không ghi được vào thư mục tải xuống", "Cannot write to the download folder"), dir.display()));
        }
        let path = engine::unique_path(&dir, &req.filename);
        let filename = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let headers_json = serde_json::to_string(&req.headers).unwrap_or_else(|_| "[]".into());
        // Several sources are stored for a net-merge resume; one source stores an empty list and falls back to the url.
        let sources_json = if req.sources.len() > 1 { serde_json::to_string(&req.sources).unwrap_or_else(|_| "[]".into()) } else { "[]".into() };
        let id = match store::add(&self.db, req.url(), &filename, &dir.to_string_lossy(), &headers_json, &sources_json, &req.kind, now()) {
            Ok(id) => id,
            Err(e) => return self.fail(format!("{} {filename}: {e}", tr("Lanes không thêm được", "Lanes could not add"))),
        };
        if let Some(size) = req.size {
            let _ = store::set_size(&self.db, id, size);
        }
        let _ = store::set_status(&self.db, id, engine::state::QUEUED, "");
        self.start_queued();
        self.push_list();
    }

    /// The files a batch link stands for (`expand`), each added as its own download.
    fn add_batch(&self, urls: Vec<String>) {
        self.post(&json!({ "event": "toast", "data": format!("{} {}", tr("Đang thêm tệp:", "Adding files:"), urls.len()) }));
        let proxy = self.proxy.clone();
        thread::spawn(move || {
            for url in urls {
                add_probed(&proxy, vec![url]);
            }
        });
    }

    /// Says why a download could not be added, in the window and as a notification, since a download the browser handed
    /// over is no longer in the browser and the window may be closed.
    fn fail(&self, message: String) {
        eprintln!("{message}");
        let _ = notify_rust::Notification::new().summary(APP_NAME).body(&message).show();
        self.post(&json!({ "event": "toast", "data": message }));
    }

    /// Moves the engine's live state into the stored list, and sends the list with fresh speeds to the page.
    fn tick(&mut self) {
        let mut live = std::collections::HashSet::new();
        for snap in self.engine.snapshots() {
            live.insert(snap.id);
            if let Some(size) = snap.size {
                let _ = store::set_size(&self.db, snap.id, size);
            }
            match snap.state {
                engine::state::DONE => {
                    if self.reconcile(snap.id, engine::state::DONE, "") {
                        self.notify_done(snap.id);
                    }
                }
                engine::state::ERROR => {
                    self.reconcile(snap.id, engine::state::ERROR, &snap.error);
                }
                _ => {}
            }
        }
        // Forget the speed of anything no longer running.
        self.speeds.retain(|id, _| live.contains(id));
        self.engine.release_idle();
        self.start_queued();
        let items = self.items();
        self.after_done(&items);
        browser::publish(&self.dir, &items, &mut self.published);
        self.show_status(&items);
        if self.window.is_some() {
            self.post(&json!({ "event": "list", "data": items }));
            if self.settings.net_merge {
                self.post(&json!({ "event": "net", "data": net_json(&self.engine.net_stats()) }));
            }
        }
    }

    /// Writes a new state to the list once, and says whether it was a change (so a finish is announced only once).
    fn reconcile(&self, id: i64, status: u8, error: &str) -> bool {
        match store::get(&self.db, id) {
            Ok(Some(row)) if row.status != status => {
                let _ = store::set_status(&self.db, id, status, error);
                true
            }
            _ => false,
        }
    }

    fn notify_done(&self, id: i64) {
        if let Ok(Some(row)) = store::get(&self.db, id) {
            let _ = notify_rust::Notification::new().summary(APP_NAME).body(&format!("{} {}", tr("Đã tải xong", "Downloaded"), row.filename)).show();
        }
    }

    /// What runs, in the tray's menu, its tooltip and beside its icon.
    fn show_status(&mut self, items: &[Value]) {
        let status = menus::Status::of(items);
        let line = status.line();
        if line == self.status {
            return;
        }
        if let Some(menu) = &self.tray_menu {
            menu.update(&status);
        }
        if let Some(tray) = &self.tray {
            tray.set_title(status.title());
            let _ = tray.set_tooltip(Some(format!("{APP_NAME}\n{line}")));
        }
        self.status = line;
    }

    fn push_list(&mut self) {
        let items = self.items();
        self.post(&json!({ "event": "list", "data": items }));
    }

    /// The whole list as the page shows it: the stored facts, with the engine's live progress and a speed over it.
    fn items(&mut self) -> Vec<Value> {
        let rows = store::list(&self.db).unwrap_or_default();
        let snaps: HashMap<i64, engine::Snapshot> = self.engine.snapshots().into_iter().map(|s| (s.id, s)).collect();
        let mut items = Vec::new();
        for row in &rows {
            let snap = snaps.get(&row.id);
            let downloaded = snap.map(|s| s.downloaded).unwrap_or(0);
            // A queued row waits outside the engine, which may still hold the task paused before it was queued.
            let state = if row.status == engine::state::QUEUED { row.status } else { snap.map(|s| s.state).unwrap_or(row.status) };
            let size = snap.and_then(|s| s.size).or(row.size.map(|n| n as u64));
            let speed = self.speed(row.id, state, downloaded);
            items.push(json!({
                "id": row.id,
                "url": row.url,
                "filename": row.filename,
                "category": engine::category(&row.filename),
                "dir": row.dir,
                "state": state,
                "size": size,
                "downloaded": downloaded,
                "connections": snap.map(|s| s.connections).unwrap_or(0),
                "sources": snap.map(|s| s.sources).unwrap_or(1),
                "links": snap.map(|s| s.links).unwrap_or(0),
                "speed": speed,
                "error": snap.map(|s| s.error.clone()).filter(|e| !e.is_empty()).unwrap_or(row.error.clone()),
            }));
        }
        items
    }

    /// Bytes per second since the last tick for a running download, or 0.
    fn speed(&mut self, id: i64, state: u8, downloaded: u64) -> u64 {
        if state != engine::state::RUNNING {
            self.speeds.remove(&id);
            return 0;
        }
        let now = Instant::now();
        let speed = match self.speeds.get(&id) {
            Some((last, when)) => {
                let secs = now.duration_since(*when).as_secs_f64();
                if secs > 0.0 { ((downloaded.saturating_sub(*last)) as f64 / secs) as u64 } else { 0 }
            }
            None => 0,
        };
        self.speeds.insert(id, (downloaded, now));
        speed
    }

    fn ipc(&mut self, body: &str, target: &EventLoopWindowTarget<UserEvent>) {
        let Ok(request) = serde_json::from_str::<Value>(body) else { return };
        let id = request["id"].clone();
        let cmd = request["cmd"].as_str().unwrap_or_default();
        let args = &request["args"];
        let num = |key: &str| args[key].as_i64().unwrap_or(0);
        let reply = |app: &App, result: Result<Value, String>| {
            let message = match result {
                Ok(data) => json!({ "id": id, "ok": true, "data": data }),
                Err(error) => json!({ "id": id, "ok": false, "error": error }),
            };
            app.post(&message);
        };
        match cmd {
            "ready" => {
                self.post(&json!({ "event": "hello", "data": {
                    "version": update::CURRENT,
                    "theme": self.theme_name(),
                    "settings": &self.settings,
                    "autostart": autostart::enabled(),
                    "max_connections": engine::MAX_CONNECTIONS,
                    "extension_dir": browser::extension_dir(&self.dir),
                }}));
                self.push_list();
                self.page_ready = true;
                for message in std::mem::take(&mut self.queued) {
                    self.post(&message);
                }
                self.start_update_check();
            }
            "add" => {
                // `urls` is the net-merge mirror list; `url` is the single-source form.
                let urls = match args["urls"].as_array() {
                    Some(arr) => arr.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect(),
                    None => vec![args["url"].as_str().unwrap_or_default().to_owned()],
                };
                // One link with a [from-to] in it is a batch of files.
                match urls.as_slice() {
                    [one] => match expand(one.trim()) {
                        Some(Ok(batch)) => self.add_batch(batch),
                        Some(Err(e)) => self.post(&json!({ "event": "toast", "data": e })),
                        None => self.add_sources(urls),
                    },
                    _ => self.add_sources(urls),
                }
                reply(self, Ok(Value::Null));
            }
            "pause" => {
                self.pause(num("id"));
                self.push_list();
                reply(self, Ok(Value::Null));
            }
            "resume" => {
                self.resume(num("id"));
                self.push_list();
                reply(self, Ok(Value::Null));
            }
            "remove" => {
                let id = num("id");
                let delete = args["delete"].as_bool().unwrap_or(false);
                self.engine.remove(id, true);
                if let Ok(Some(row)) = store::get(&self.db, id) {
                    // A download that is not running (paused before a restart) is not in the engine, so its part file
                    // or folder of segments is cleared here.
                    engine::discard(&row.final_path());
                    if delete {
                        let _ = std::fs::remove_file(row.final_path());
                    }
                }
                let _ = store::remove(&self.db, id);
                self.push_list();
                reply(self, Ok(Value::Null));
            }
            "pause_all" | "resume_all" => {
                self.menu(if cmd == "pause_all" { menus::PAUSE_ALL } else { menus::RESUME_ALL }, target);
                reply(self, Ok(Value::Null));
            }
            // The page's toolbar stands in for the title bar it covers on macOS (open_window).
            "drag" | "zoom" => {
                if let Some((window, _)) = &self.window {
                    match cmd {
                        "drag" => drop(window.drag_window()),
                        _ => window.set_maximized(!window.is_maximized()),
                    }
                }
                reply(self, Ok(Value::Null));
            }
            "open" => reply(self, self.file_of(num("id")).and_then(|p| open_path(&p))),
            "reveal" => reply(self, self.file_of(num("id")).and_then(|p| reveal(&p))),
            "reveal_extension" => reply(self, reveal(&browser::extension_dir(&self.dir))),
            "open_url" => {
                if let Some(url) = args["url"].as_str() {
                    if update::is_release_url(url) || url == format!("https://github.com/{}", update::REPO) {
                        let _ = open_path(Path::new(url));
                    }
                }
                reply(self, Ok(Value::Null));
            }
            "set_connections" => {
                let n = args["connections"].as_u64().unwrap_or(engine::DEFAULT_CONNECTIONS as u64) as usize;
                self.settings.connections = n.clamp(1, engine::MAX_CONNECTIONS);
                self.engine.set_connections(self.settings.connections);
                self.settings.save(&self.dir);
                reply(self, Ok(json!(self.settings.connections)));
            }
            "set_folder" => {
                if let Some(path) = args["path"].as_str().filter(|p| !p.is_empty()) {
                    self.settings.download_dir = path.to_owned();
                    self.settings.save(&self.dir);
                }
                reply(self, Ok(json!(self.settings.download_dir)));
            }
            "set_max_active" => {
                self.settings.max_active = args["n"].as_u64().unwrap_or(0).min(50) as usize;
                self.settings.save(&self.dir);
                self.start_queued();
                reply(self, Ok(json!(self.settings.max_active)));
            }
            "set_speed_limit" => {
                self.settings.speed_limit = args["bytes"].as_u64().unwrap_or(0);
                self.engine.set_speed_limit(self.settings.speed_limit);
                self.settings.save(&self.dir);
                reply(self, Ok(json!(self.settings.speed_limit)));
            }
            "set_schedule" => {
                self.settings.start_at = args["start_at"].as_i64().unwrap_or(0).max(0);
                self.settings.save(&self.dir);
                self.start_queued();
                reply(self, Ok(json!(self.settings.start_at)));
            }
            "set_when_done" => {
                let action = args["action"].as_str().unwrap_or_default();
                self.settings.when_done = if matches!(action, "sleep" | "shutdown") { action.to_owned() } else { String::new() };
                if self.settings.when_done.is_empty() {
                    self.after_at = 0;
                }
                self.settings.save(&self.dir);
                reply(self, Ok(json!(self.settings.when_done)));
            }
            "set_watch_clipboard" => {
                self.settings.watch_clipboard = args["on"].as_bool().unwrap_or(false);
                self.watching.store(self.settings.watch_clipboard, Ordering::Relaxed);
                self.settings.save(&self.dir);
                reply(self, Ok(json!(self.settings.watch_clipboard)));
            }
            "set_language" => {
                self.settings.language = if args["language"] == "en" { "en".into() } else { "vi".into() };
                ENGLISH.store(self.settings.language == "en", Ordering::Relaxed);
                self.settings.save(&self.dir);
                self.relabel();
                reply(self, Ok(json!(self.settings.language)));
            }
            "set_theme" => {
                let theme = args["theme"].as_str().unwrap_or_default();
                self.settings.theme = if matches!(theme, "light" | "dark") { theme.to_owned() } else { "system".into() };
                self.settings.save(&self.dir);
                if let Some((window, _)) = &self.window {
                    window.set_theme(self.window_theme());
                }
                reply(self, Ok(json!(self.settings.theme)));
            }
            "set_glass" => {
                self.settings.glass = args["on"].as_bool().unwrap_or(false);
                self.settings.save(&self.dir);
                reply(self, Ok(json!(self.settings.glass)));
            }
            "set_classify" => {
                self.settings.classify = args["on"].as_bool().unwrap_or(true);
                self.settings.save(&self.dir);
                reply(self, Ok(json!(self.settings.classify)));
            }
            "set_net_merge" => {
                self.settings.net_merge = args["on"].as_bool().unwrap_or(false);
                self.engine.set_net_merge(self.settings.net_merge);
                self.settings.save(&self.dir);
                let net = self.settings.net_merge.then(|| net_json(&self.engine.net_stats()));
                reply(self, Ok(json!({ "on": self.settings.net_merge, "net": net })));
            }
            "choose_folder" => reply(self, choose_folder().map(|p| json!(p))),
            "set_autostart" => {
                reply(self, autostart::set(args["on"].as_bool().unwrap_or(false)).map(|()| json!(autostart::enabled())));
                if let Some(menu) = &self.tray_menu {
                    menu.autostart.set_checked(autostart::enabled());
                }
            }
            "check_update" => {
                self.start_update_check();
                reply(self, Ok(Value::Null));
            }
            "install_update" => {
                self.install_update();
                reply(self, Ok(Value::Null));
            }
            "restart" => reply(self, update::relaunch()),
            "show" => {
                self.show(target);
                reply(self, Ok(Value::Null));
            }
            other => reply(self, Err(format!("Lệnh không rõ: {other}"))),
        }
    }

    fn file_of(&self, id: i64) -> Result<PathBuf, String> {
        store::get(&self.db, id).map_err(|e| e.to_string())?.map(|r| r.final_path()).ok_or_else(|| "Không thấy mục này".into())
    }

    fn start_update_check(&self) {
        let proxy = self.proxy.clone();
        thread::spawn(move || {
            if let Some(app) = update::app_path() {
                update::tidy(&app);
            }
            if let Ok(check) = update::check() {
                if check.newer {
                    let _ = proxy.send_event(UserEvent::Post(json!({ "event": "update", "data": check })));
                }
            }
        });
    }

    fn install_update(&self) {
        let proxy = self.proxy.clone();
        thread::spawn(move || {
            let run = || -> Result<(), String> {
                let app = update::installed().ok_or("Bản này không tự cập nhật được, hãy tải từ trang phát hành")?;
                let check = update::check()?;
                update::install(&check, &app, |pct| {
                    let _ = proxy.send_event(UserEvent::Post(json!({ "event": "update_progress", "data": pct })));
                })
            };
            let message = match run() {
                Ok(()) => json!({ "event": "update_ready" }),
                Err(error) => json!({ "event": "toast", "data": error }),
            };
            let _ = proxy.send_event(UserEvent::Post(message));
        });
    }
}

/// Probes the first source for the name and size, off the event loop, then hands the download to it.
fn add_probed(proxy: &EventLoopProxy<UserEvent>, sources: Vec<String>) {
    let primary = sources[0].clone();
    let probe = engine::probe(&primary, &[]).ok();
    let filename = probe.as_ref().map(|p| p.filename.clone()).unwrap_or_else(|| engine::filename_from(&primary, None));
    let size = probe.and_then(|p| p.size);
    let _ = proxy.send_event(UserEvent::Added(AddReq { sources, filename, size, headers: Vec::new(), kind: engine::Kind::File }));
}

/// The first http or https link on the clipboard.
fn clipboard_link() -> Option<String> {
    clipboard_text()?.split_whitespace().find(|w| w.starts_with("http://") || w.starts_with("https://")).map(str::to_owned)
}

/// The text on the clipboard.
fn clipboard_text() -> Option<String> {
    let mut command = if cfg!(target_os = "macos") {
        cmd("/usr/bin/pbpaste")
    } else if cfg!(windows) {
        let mut c = cmd("powershell");
        c.args(["-NoProfile", "-Command", "Get-Clipboard"]);
        c
    } else {
        return None;
    };
    let out = command.output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Offers each link to a file the user copies while `on` is set, once. The clipboard is read every so often, on
/// Windows only when its count of changes moved, since reading it there starts PowerShell. What it held when watching
/// began is not offered.
fn watch_clipboard(on: &AtomicBool, proxy: &EventLoopProxy<UserEvent>) {
    let mut last: Option<String> = None;
    loop {
        thread::sleep(CLIPBOARD_EVERY);
        if !on.load(Ordering::Relaxed) {
            last = None;
            continue;
        }
        let changed = clipboard_changed();
        if last.is_some() && !changed {
            continue;
        }
        let text = clipboard_text().unwrap_or_default();
        if last.replace(text.clone()).is_some_and(|seen| seen != text) {
            if let Some(url) = file_link(&text) {
                let _ = proxy.send_event(UserEvent::Copied(url));
            }
        }
    }
}

#[cfg(windows)]
fn clipboard_changed() -> bool {
    static SEEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let count = unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() };
    SEEN.swap(count, Ordering::Relaxed) != count
}

#[cfg(not(windows))]
fn clipboard_changed() -> bool {
    true
}

/// `text` when it is one http or https link to a file of a type Lanes sorts (`engine::category`), the links IDM's
/// clipboard watch takes; not a page's.
fn file_link(text: &str) -> Option<String> {
    let text = text.trim();
    let url = url::Url::parse(text).ok().filter(|u| matches!(u.scheme(), "http" | "https") && !text.contains(char::is_whitespace))?;
    let name = url.path_segments()?.next_back()?;
    engine::category(name).map(|_| text.to_owned())
}

/// The addresses a batch link stands for, as IDM writes one: a `[from-to]` of numbers, `[001-120]` keeping its zeros,
/// or of letters, `[a-z]`. None for an ordinary link; an error past `MAX_BATCH` files.
fn expand(url: &str) -> Option<Result<Vec<String>, String>> {
    let open = url.find('[')?;
    let close = open + url[open..].find(']')?;
    let (from, to) = url[open + 1..close].split_once('-')?;
    let (head, tail) = (&url[..open], &url[close + 1..]);
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let items: Vec<String> = if digits(from) && digits(to) {
        let (a, b) = (from.parse::<u64>().ok()?, to.parse::<u64>().ok()?);
        let (a, b) = (a.min(b), a.max(b));
        if b - a >= MAX_BATCH as u64 {
            return Some(Err(format!("{} {MAX_BATCH}", tr("Số tệp tối đa của một loạt:", "The most files in one batch:"))));
        }
        let width = if from.starts_with('0') { from.len() } else { 0 };
        (a..=b).map(|n| format!("{n:0width$}")).collect()
    } else {
        let (&[a], &[b]) = (from.as_bytes(), to.as_bytes()) else { return None };
        if !(a.is_ascii_alphabetic() && b.is_ascii_alphabetic() && a.is_ascii_lowercase() == b.is_ascii_lowercase()) {
            return None;
        }
        (a.min(b)..=a.max(b)).map(|c| (c as char).to_string()).collect()
    };
    Some(Ok(items.iter().map(|i| format!("{head}{i}{tail}")).collect()))
}

/// Puts the computer to sleep or shuts it down, for `when_done`.
fn power(action: &str) -> Result<(), String> {
    let (program, args): (&str, &[&str]) = match (action, cfg!(windows)) {
        ("sleep", false) => ("/usr/bin/pmset", &["sleepnow"]),
        ("shutdown", false) => ("/usr/bin/osascript", &["-e", "tell application \"System Events\" to shut down"]),
        ("sleep", true) => ("rundll32.exe", &["powrprof.dll,SetSuspendState", "0,1,0"]),
        ("shutdown", true) => ("shutdown", &["/s", "/t", "0"]),
        _ => return Ok(()),
    };
    match cmd(program).args(args).status() {
        Ok(status) if status.success() => Ok(()),
        Ok(_) | Err(_) if action == "shutdown" => Err(tr("Lanes không tắt được máy. Trên macOS, hãy cho Lanes điều khiển System Events trong Cài đặt hệ thống, Quyền riêng tư và bảo mật, Tự động hóa.", "Lanes could not shut the computer down. On macOS, let Lanes control System Events in System Settings, Privacy and Security, Automation.").into()),
        _ => Err(tr("Lanes không cho máy ngủ được", "Lanes could not put the computer to sleep").into()),
    }
}

/// The page with the language, the light or dark and the glass of the settings on its root, so its first frame is
/// already drawn in them.
fn page(settings: &Settings) -> String {
    let glass = if settings.glass { " data-glass" } else { "" };
    let root = format!("<html lang=\"{}\" data-theme=\"{}\"{glass}>", settings.language, settings.theme);
    INDEX_HTML.replacen("<html lang=\"vi\">", &root, 1)
}

/// The links of net-merge as the page shows them; the addresses stay here.
fn net_json(stats: &netmerge::Stats) -> Value {
    let links: Vec<Value> = stats
        .links
        .iter()
        .map(|l| json!({ "name": l.name, "label": l.label, "state": l.state, "primary": l.primary, "connections": l.connections, "speed": l.speed }))
        .collect();
    json!({ "links": links, "paused": stats.paused && !stats.links.is_empty() })
}

/// Opens a folder picker and gives the chosen path, or None when the user cancels.
fn choose_folder() -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    {
        let script = "POSIX path of (choose folder with prompt \"Chọn thư mục tải xuống\")";
        let out = std::process::Command::new("/usr/bin/osascript").args(["-e", script]).output().map_err(|e| e.to_string())?;
        let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        return Ok(if path.is_empty() { Value::Null } else { json!(path.trim_end_matches('/')) });
    }
    #[cfg(windows)]
    {
        let script = "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.FolderBrowserDialog; if ($d.ShowDialog() -eq 'OK') { $d.SelectedPath }";
        let out = cmd("powershell").args(["-NoProfile", "-STA", "-Command", script]).output().map_err(|e| e.to_string())?;
        let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        return Ok(if path.is_empty() { Value::Null } else { json!(path) });
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Ok(Value::Null)
    }
}

fn cmd(program: &str) -> std::process::Command {
    #[allow(unused_mut)]
    let mut c = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c
}

/// Opens a file or URL with the system handler.
fn open_path(path: &Path) -> Result<Value, String> {
    let mut command = if cfg!(target_os = "macos") {
        cmd("/usr/bin/open")
    } else if cfg!(windows) {
        let mut c = cmd("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    } else {
        cmd("xdg-open")
    };
    command.arg(path.as_os_str()).spawn().map(|_| Value::Null).map_err(|e| e.to_string())
}

/// Shows a file in Finder or Explorer, selected.
fn reveal(path: &Path) -> Result<Value, String> {
    if cfg!(target_os = "macos") {
        cmd("/usr/bin/open").args(["-R".as_ref(), path.as_os_str()]).spawn().map(|_| Value::Null).map_err(|e| e.to_string())
    } else if cfg!(windows) {
        cmd("explorer").arg(format!("/select,{}", path.display())).spawn().map(|_| Value::Null).map_err(|e| e.to_string())
    } else {
        open_path(path.parent().unwrap_or(path))
    }
}

/// The RGBA pixels and side of a PNG icon.
fn decode_png(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    (info.color_type == png::ColorType::Rgba).then_some((buf, info.width, info.height))
}

#[cfg(test)]
mod tests {
    /// A settings file from before a field existed keeps what it has, and the new field takes its default.
    #[test]
    fn older_settings_keep_the_folder_and_classify_by_default() {
        let settings: super::Settings = serde_json::from_str(r#"{"download_dir": "/x", "connections": 4}"#).unwrap();
        assert_eq!((settings.download_dir.as_str(), settings.connections, settings.classify), ("/x", 4, true));
        assert_eq!((settings.max_active, settings.speed_limit, settings.start_at, settings.when_done.as_str()), (3, 0, 0, ""));
    }

    #[test]
    fn the_page_comes_in_the_language_and_look_of_the_settings() {
        let settings = super::Settings { language: "en".into(), theme: "dark".into(), glass: true, ..Default::default() };
        assert!(super::page(&settings).contains(r#"<html lang="en" data-theme="dark" data-glass>"#), "the root of ui/index.html is still <html lang=\"vi\">");
        assert!(super::page(&super::Settings::default()).contains(r#"<html lang="vi" data-theme="system">"#));
    }

    #[test]
    fn a_batch_link_stands_for_each_file_of_its_range() {
        let all = |url: &str| super::expand(url).unwrap().unwrap();
        assert_eq!(all("https://x.example/p[08-10].jpg"), ["https://x.example/p08.jpg", "https://x.example/p09.jpg", "https://x.example/p10.jpg"]);
        assert_eq!(all("https://x.example/[9-11]/a.zip"), ["https://x.example/9/a.zip", "https://x.example/10/a.zip", "https://x.example/11/a.zip"]);
        assert_eq!(all("https://x.example/disk[c-a].iso").len(), 3, "a range given backwards");
        assert_eq!(super::expand("https://x.example/a.zip?f[0]=1"), None, "a bracket that is no range");
        assert_eq!(super::expand("https://x.example/[a-Z].zip"), None);
        assert_eq!(super::expand("https://x.example/[aa-zz].zip"), None);
        assert!(super::expand("https://x.example/[0-5000].jpg").unwrap().is_err());
    }

    #[test]
    fn only_a_lone_link_to_a_file_is_taken_from_the_clipboard() {
        assert_eq!(super::file_link(" https://dl.example/setup.exe?x=1 \n").as_deref(), Some("https://dl.example/setup.exe?x=1"));
        assert_eq!(super::file_link("https://cdn.example/Music/a.mp3").as_deref(), Some("https://cdn.example/Music/a.mp3"));
        assert_eq!(super::file_link("https://example.com/page"), None, "a page");
        assert_eq!(super::file_link("see https://dl.example/setup.exe"), None, "a sentence with a link in it");
        assert_eq!(super::file_link("ftp://dl.example/setup.exe"), None);
    }
}
