//! The menus. The tray's says what is downloading (also as the percentage beside the icon on macOS) and holds what a
//! download manager is reached for without its window: a copied link, the folder, pausing or resuming everything,
//! the settings, starting with the computer. On macOS the app also has a menu bar (Lanes, Edit, View, Window, Help),
//! which gives the window's page copy and paste with the usual keys. Both menus use the ids below, which main.rs acts
//! on.

use serde_json::Value;
use tray_icon::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};

pub const OPEN: &str = "open";
pub const ADD_COPIED: &str = "add-copied";
pub const FOLDER: &str = "folder";
pub const PAUSE_ALL: &str = "pause-all";
pub const RESUME_ALL: &str = "resume-all";
pub const AUTOSTART: &str = "autostart";
pub const QUIT: &str = "quit";
pub const SETTINGS: &str = "settings";
#[cfg(target_os = "macos")]
pub const HOME_PAGE: &str = "home-page";
#[cfg(target_os = "macos")]
pub const REPORT: &str = "report";

/// The tray's menu, with the entries that change.
pub struct Tray {
    pub menu: Menu,
    status: MenuItem,
    pause: MenuItem,
    resume: MenuItem,
    pub autostart: CheckMenuItem,
}

pub fn tray(autostart: bool) -> Tray {
    let item = |id: &str, text: &str| MenuItem::with_id(id, text, true, None);
    let tray = Tray {
        menu: Menu::new(),
        status: MenuItem::new(Status::default().line(), false, None),
        pause: MenuItem::with_id(PAUSE_ALL, "Tạm dừng tất cả", false, None),
        resume: MenuItem::with_id(RESUME_ALL, "Tiếp tục tất cả", false, None),
        autostart: CheckMenuItem::with_id(AUTOSTART, "Mở cùng máy tính", true, autostart, None),
    };
    let _ = tray.menu.append_items(&[
        &tray.status,
        &PredefinedMenuItem::separator(),
        &item(OPEN, "Mở Lanes"),
        &item(SETTINGS, "Cài đặt..."),
        &item(ADD_COPIED, "Tải liên kết đã sao chép"),
        &item(FOLDER, "Mở thư mục tải xuống"),
        &PredefinedMenuItem::separator(),
        &tray.pause,
        &tray.resume,
        &PredefinedMenuItem::separator(),
        &tray.autostart,
        &PredefinedMenuItem::separator(),
        &item(QUIT, "Thoát"),
    ]);
    tray
}

impl Tray {
    /// Shows `status`, with Pause all and Resume all usable only when there is something for them to do.
    pub fn update(&self, status: &Status) {
        self.status.set_text(status.line());
        self.pause.set_enabled(status.running > 0);
        self.resume.set_enabled(status.paused > 0);
    }
}

/// What the downloads are doing, over the list the page shows (main.rs `items`).
#[derive(Debug, Default, PartialEq)]
pub struct Status {
    /// Downloading or waiting for a place.
    pub running: usize,
    pub paused: usize,
    /// The share done over the running ones, when every one of them knows its size.
    pub percent: Option<u64>,
    /// Bytes per second over all of them.
    pub speed: u64,
}

impl Status {
    pub fn of(items: &[Value]) -> Status {
        let state = |item: &Value| item["state"].as_u64().unwrap_or(0) as u8;
        let running: Vec<&Value> = items.iter().filter(|i| matches!(state(i), crate::engine::state::QUEUED | crate::engine::state::RUNNING)).collect();
        let sizes: Vec<(u64, u64)> = running.iter().filter_map(|i| Some((i["size"].as_u64().filter(|s| *s > 0)?, i["downloaded"].as_u64().unwrap_or(0)))).collect();
        let total: u64 = sizes.iter().map(|(size, _)| size).sum();
        let done: u64 = sizes.iter().map(|(size, done)| done.min(size)).sum();
        Status {
            running: running.len(),
            paused: items.iter().filter(|i| state(i) == crate::engine::state::PAUSED).count(),
            percent: (!running.is_empty() && sizes.len() == running.len()).then(|| 100 * done / total.max(1)),
            speed: running.iter().map(|i| i["speed"].as_u64().unwrap_or(0)).sum(),
        }
    }

    /// The first line of the tray's menu.
    pub fn line(&self) -> String {
        match (self.running, self.paused) {
            (0, 0) => "Không có gì đang tải".into(),
            (0, paused) => format!("{paused} mục đang tạm dừng"),
            (running, _) => {
                let mut line = format!("Đang tải {running} mục");
                if let Some(percent) = self.percent {
                    line += &format!(", {percent}%");
                }
                if self.speed > 0 {
                    line += &format!(", {}/s", human(self.speed));
                }
                line
            }
        }
    }

    /// Beside the tray icon while something downloads: the share done, or how many when a size is not known yet.
    pub fn title(&self) -> Option<String> {
        (self.running > 0).then(|| self.percent.map_or_else(|| self.running.to_string(), |p| format!("{p}%")))
    }
}

fn human(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let (mut n, mut i) = (bytes as f64, 0);
    while n >= 1024.0 && i < units.len() - 1 {
        n /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{bytes} B") } else { format!("{n:.1} {}", units[i]) }
}

/// The menu bar of the app on macOS, as other Mac apps have it. The predefined entries are the system's own actions;
/// Quit is Lanes' own, so that it leaves the way the tray's Quit does. It is made before the event loop runs and put
/// up once the app has started, when macOS takes it.
#[cfg(target_os = "macos")]
pub struct MenuBar {
    bar: Menu,
    window: tray_icon::menu::Submenu,
    help: tray_icon::menu::Submenu,
}

#[cfg(target_os = "macos")]
impl MenuBar {
    pub fn install(&self) {
        self.bar.init_for_nsapp();
        self.window.set_as_windows_menu_for_nsapp();
        self.help.set_as_help_menu_for_nsapp();
    }
}

#[cfg(target_os = "macos")]
pub fn menu_bar() -> Option<MenuBar> {
    use tray_icon::menu::{
        AboutMetadata, Submenu,
        accelerator::{Accelerator, Code, Modifiers},
    };
    let key = |code| Some(Accelerator::new(Modifiers::META, code));
    let about = AboutMetadata {
        name: Some("Lanes".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        comments: Some("Trình quản lý tải xuống mở".into()),
        copyright: Some("Copyright (c) 2026 Doanh Thai Bui. Giấy phép MIT.".into()),
        ..Default::default()
    };
    let app = Submenu::with_items(
        "Lanes",
        true,
        &[
            &PredefinedMenuItem::about(Some("Giới thiệu Lanes"), Some(about)),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(SETTINGS, "Cài đặt...", true, key(Code::Comma)),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::services(Some("Dịch vụ")),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(Some("Ẩn Lanes")),
            &PredefinedMenuItem::hide_others(Some("Ẩn các ứng dụng khác")),
            &PredefinedMenuItem::show_all(Some("Hiện tất cả")),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(QUIT, "Thoát Lanes", true, key(Code::KeyQ)),
        ],
    );
    let edit = Submenu::with_items(
        "Sửa",
        true,
        &[
            &PredefinedMenuItem::undo(Some("Hoàn tác")),
            &PredefinedMenuItem::redo(Some("Làm lại")),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::cut(Some("Cắt")),
            &PredefinedMenuItem::copy(Some("Sao chép")),
            &PredefinedMenuItem::paste(Some("Dán")),
            &PredefinedMenuItem::select_all(Some("Chọn tất cả")),
        ],
    );
    let view = Submenu::with_items("Xem", true, &[&PredefinedMenuItem::fullscreen(Some("Vào chế độ toàn màn hình"))]);
    let window = Submenu::with_items(
        "Cửa sổ",
        true,
        &[
            &PredefinedMenuItem::minimize(Some("Thu nhỏ")),
            &PredefinedMenuItem::maximize(Some("Phóng to")),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::close_window(Some("Đóng cửa sổ")),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::bring_all_to_front(Some("Đưa tất cả lên trước")),
        ],
    );
    let help = Submenu::with_items(
        "Trợ giúp",
        true,
        &[&MenuItem::with_id(HOME_PAGE, "Lanes trên GitHub", true, None), &MenuItem::with_id(REPORT, "Báo lỗi", true, None)],
    );
    let bar = Menu::new();
    let (app, edit, view, window, help) = (app.ok()?, edit.ok()?, view.ok()?, window.ok()?, help.ok()?);
    bar.append_items(&[&app, &edit, &view, &window, &help]).ok()?;
    Some(MenuBar { bar, window, help })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_tray_says_what_runs_with_the_share_done_and_the_speed() {
        let item = |state: u8, size: Option<u64>, downloaded: u64, speed: u64| json!({ "state": state, "size": size, "downloaded": downloaded, "speed": speed });
        let idle = Status::of(&[item(3, Some(10), 10, 0), item(4, None, 0, 0)]);
        assert_eq!((idle.line(), idle.title()), ("Không có gì đang tải".into(), None));
        let paused = Status::of(&[item(2, Some(10), 5, 0), item(2, None, 0, 0)]);
        assert_eq!((paused.line(), paused.title()), ("2 mục đang tạm dừng".into(), None));
        let running = Status::of(&[item(1, Some(300), 100, 1024 * 1024), item(0, Some(100), 0, 0), item(2, Some(9), 1, 0)]);
        assert_eq!(running, Status { running: 2, paused: 1, percent: Some(25), speed: 1024 * 1024 });
        assert_eq!((running.line(), running.title()), ("Đang tải 2 mục, 25%, 1.0 MB/s".into(), Some("25%".into())));
        let unknown = Status::of(&[item(1, Some(300), 100, 512), item(1, None, 50, 0)]);
        assert_eq!(unknown.percent, None, "a share over some of them would mislead");
        assert_eq!((unknown.line(), unknown.title()), ("Đang tải 2 mục, 512 B/s".into(), Some("2".into())));
    }
}
