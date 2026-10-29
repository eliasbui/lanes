// No console window behind the app on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod browser;
mod engine;
mod hls;
mod store;
mod update;

use engine::Engine;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs::File,
    path::{Path, PathBuf},
    sync::Arc,
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
    menu::{Menu, MenuEvent, MenuId, MenuItem},
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
}

impl Default for Settings {
    fn default() -> Self {
        Settings { download_dir: downloads_dir().to_string_lossy().into_owned(), connections: engine::DEFAULT_CONNECTIONS, classify: true }
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
/// the primary first; an ordinary download has one entry. `hls` marks an HLS playlist from the video button.
struct AddReq {
    sources: Vec<String>,
    filename: String,
    size: Option<u64>,
    headers: Vec<(String, String)>,
    hls: bool,
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
    let engine = Arc::new(Engine::new());
    engine.set_connections(settings.connections);
    let background = std::env::args().any(|a| a == autostart::BACKGROUND);

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
                    let probe = if job.hls { None } else { engine::probe(&job.url, &headers).ok() };
                    let size = probe.as_ref().and_then(|p| p.size);
                    let filename = match job.filename {
                        name if !name.is_empty() => name,
                        _ if job.hls => "video.ts".into(),
                        _ => probe.map(|p| p.filename).unwrap_or_else(|| engine::filename_from(&job.url, None)),
                    };
                    let _ = proxy.send_event(UserEvent::Added(AddReq { sources: vec![job.url], filename, size, headers, hls: job.hls }));
                }
                thread::sleep(INBOX_EVERY);
            }
        });
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
        tray: None,
        open_id: None,
        quit_id: None,
        speeds: HashMap::new(),
    };
    // Pick up where downloads that were running last time left off.
    app.resume_unfinished();

    event_loop.run(move |event, target, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(tao::event::StartCause::Init) => {
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
            }
            Event::UserEvent(UserEvent::Ipc(body)) => app.ipc(&body, target),
            Event::UserEvent(UserEvent::Post(message)) => app.post(&message),
            Event::UserEvent(UserEvent::Added(req)) => app.add(req),
            Event::UserEvent(UserEvent::Tick) => app.tick(),
            Event::UserEvent(UserEvent::TrayClick) => app.show(target),
            Event::UserEvent(UserEvent::Menu(id)) => {
                if app.quit_id.as_ref() == Some(&id) {
                    *flow = ControlFlow::Exit;
                } else if app.open_id.as_ref() == Some(&id) {
                    app.show(target);
                }
            }
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
    tray: Option<TrayIcon>,
    open_id: Option<MenuId>,
    quit_id: Option<MenuId>,
    /// Per-download bytes-and-time of the last tick, to work out the speed.
    speeds: HashMap<i64, (u64, Instant)>,
}

impl App {
    fn make_tray(&mut self) {
        let open = MenuItem::new(format!("Mở {APP_NAME}"), true, None);
        let quit = MenuItem::new("Thoát", true, None);
        let menu = Menu::new();
        if menu.append_items(&[&open, &quit]).is_err() {
            return;
        }
        let icon = decode_png(TRAY_ICON).and_then(|(rgba, w, h)| tray_icon::Icon::from_rgba(rgba, w, h).ok());
        let mut builder = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip(APP_NAME);
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
        self.open_id = Some(open.id().clone());
        self.quit_id = Some(quit.id().clone());
    }

    fn show(&mut self, target: &EventLoopWindowTarget<UserEvent>) {
        if let Some((window, _)) = &self.window {
            window.set_visible(true);
            window.set_minimized(false);
            window.set_focus();
            return;
        }
        match self.open_window(target) {
            Ok(pair) => self.window = Some(pair),
            Err(e) => eprintln!("window: {e}"),
        }
    }

    fn open_window(&self, target: &EventLoopWindowTarget<UserEvent>) -> Result<(Window, WebView), String> {
        let icon = decode_png(WINDOW_ICON).and_then(|(rgba, w, h)| Icon::from_rgba(rgba, w, h).ok());
        let window = WindowBuilder::new()
            .with_title(APP_NAME)
            .with_inner_size(LogicalSize::new(940.0, 640.0))
            .with_min_inner_size(LogicalSize::new(560.0, 480.0))
            .with_window_icon(icon)
            .with_theme(None)
            .build(target)
            .map_err(|e| e.to_string())?;
        let proxy = self.proxy.clone();
        let builder = WebViewBuilder::new()
            .with_html(INDEX_HTML)
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

    fn theme_name(&self) -> &'static str {
        match self.window.as_ref().map(|(w, _)| w.theme()) {
            Some(Theme::Light) => "light",
            _ => "dark",
        }
    }

    /// Restarts every download that was still going when the app last closed.
    fn resume_unfinished(&self) {
        for row in store::list(&self.db).unwrap_or_default() {
            if matches!(row.status, engine::state::RUNNING | engine::state::QUEUED) {
                self.engine.start(row.id, row.source_list(), row.final_path(), row.header_pairs(), row.hls);
            }
        }
    }

    /// One or more URLs typed or pasted into the page. Several URLs are the mirrors of one file (net-merge, beta): the
    /// first is probed for the name and size, and all of them become the download's sources. The probe runs off the
    /// event loop.
    fn add_sources(&self, urls: Vec<String>) {
        let sources: Vec<String> = urls.into_iter().map(|u| u.trim().to_owned()).filter(|u| !u.is_empty()).collect();
        if !sources.iter().all(|u| u.starts_with("http://") || u.starts_with("https://")) || sources.is_empty() {
            self.post(&json!({ "event": "toast", "data": "Hãy dán liên kết http hoặc https" }));
            return;
        }
        let proxy = self.proxy.clone();
        thread::spawn(move || {
            let primary = sources[0].clone();
            let probe = engine::probe(&primary, &[]).ok();
            let filename = probe.as_ref().map(|p| p.filename.clone()).unwrap_or_else(|| engine::filename_from(&primary, None));
            let size = probe.and_then(|p| p.size);
            let _ = proxy.send_event(UserEvent::Added(AddReq { sources, filename, size, headers: Vec::new(), hls: false }));
        });
    }

    /// Records a probed download, starts it, and shows it in the list.
    fn add(&mut self, req: AddReq) {
        let mut dir = PathBuf::from(&self.settings.download_dir);
        if let Some(folder) = engine::category(&req.filename).filter(|_| self.settings.classify) {
            dir.push(folder);
        }
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return self.fail(format!("Không ghi được vào thư mục tải xuống {}: {e}", dir.display()));
        }
        let path = engine::unique_path(&dir, &req.filename);
        let filename = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let headers_json = serde_json::to_string(&req.headers).unwrap_or_else(|_| "[]".into());
        // Several sources are stored for a net-merge resume; one source stores an empty list and falls back to the url.
        let sources_json = if req.sources.len() > 1 { serde_json::to_string(&req.sources).unwrap_or_else(|_| "[]".into()) } else { "[]".into() };
        let id = match store::add(&self.db, req.url(), &filename, &dir.to_string_lossy(), &headers_json, &sources_json, req.hls, now()) {
            Ok(id) => id,
            Err(e) => return self.fail(format!("Lanes không thêm được {filename}: {e}")),
        };
        if let Some(size) = req.size {
            let _ = store::set_size(&self.db, id, size);
        }
        let _ = store::set_status(&self.db, id, engine::state::RUNNING, "");
        self.engine.start(id, req.sources, path, req.headers, req.hls);
        self.push_list();
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
        if self.window.is_some() {
            self.push_list();
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
            let _ = notify_rust::Notification::new().summary(APP_NAME).body(&format!("Đã tải xong {}", row.filename)).show();
        }
    }

    /// The whole list as the page shows it: the stored facts, with the engine's live progress and a speed over it.
    fn push_list(&mut self) {
        let rows = store::list(&self.db).unwrap_or_default();
        let snaps: HashMap<i64, engine::Snapshot> = self.engine.snapshots().into_iter().map(|s| (s.id, s)).collect();
        let mut items = Vec::new();
        for row in &rows {
            let snap = snaps.get(&row.id);
            let downloaded = snap.map(|s| s.downloaded).unwrap_or(0);
            let state = snap.map(|s| s.state).unwrap_or(row.status);
            let size = snap.and_then(|s| s.size).or(row.size.map(|n| n as u64));
            let speed = self.speed(row.id, state, downloaded);
            items.push(json!({
                "id": row.id,
                "url": row.url,
                "filename": row.filename,
                "dir": row.dir,
                "state": state,
                "size": size,
                "downloaded": downloaded,
                "connections": snap.map(|s| s.connections).unwrap_or(0),
                "sources": snap.map(|s| s.sources).unwrap_or(1),
                "speed": speed,
                "error": snap.map(|s| s.error.clone()).filter(|e| !e.is_empty()).unwrap_or(row.error.clone()),
            }));
        }
        self.post(&json!({ "event": "list", "data": items }));
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
                self.start_update_check();
            }
            "add" => {
                // `urls` is the net-merge mirror list; `url` is the single-source form.
                let urls = match args["urls"].as_array() {
                    Some(arr) => arr.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect(),
                    None => vec![args["url"].as_str().unwrap_or_default().to_owned()],
                };
                self.add_sources(urls);
                reply(self, Ok(Value::Null));
            }
            "pause" => {
                let id = num("id");
                self.engine.pause(id);
                let _ = store::set_status(&self.db, id, engine::state::PAUSED, "");
                self.push_list();
                reply(self, Ok(Value::Null));
            }
            "resume" => {
                let id = num("id");
                if let Ok(Some(row)) = store::get(&self.db, id) {
                    let _ = store::set_status(&self.db, id, engine::state::RUNNING, "");
                    self.engine.start(id, row.source_list(), row.final_path(), row.header_pairs(), row.hls);
                }
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
            "set_classify" => {
                self.settings.classify = args["on"].as_bool().unwrap_or(true);
                self.settings.save(&self.dir);
                reply(self, Ok(json!(self.settings.classify)));
            }
            "choose_folder" => reply(self, choose_folder().map(|p| json!(p))),
            "set_autostart" => reply(self, autostart::set(args["on"].as_bool().unwrap_or(false)).map(|()| json!(autostart::enabled()))),
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
    }
}
