//! The browser extension in extension/: it takes over a download the browser started and sends it here, through
//! native messaging, with the address, the name, and the headers the page used (Referer, Cookie, User-Agent) so a
//! site that checks them serves the file to Lanes as it did to the browser. Its video button asks here for the
//! resolutions of an HLS video and sends the one picked the same way. The browser starts the app itself as the host;
//! that copy opens no window, only answers, or drops the job in an inbox in the data folder and starts the app in the
//! tray when it is not running. The running app takes the jobs and hands them to the engine.
//!
//! The extension's files are carried in the app and written to the data folder at every start, so the copy the
//! browser loads always matches the app.
//!
//! The extension also shows the downloads' progress in the browser. The running app writes its latest downloads to
//! the data folder whenever they change (`publish`), and a host the extension keeps connected (`watch`) passes them on.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// The name the extension asks the browser for.
pub const HOST: &str = "com.eliasbui.lanes";
/// What the browser puts before the extension's ID in the argument it starts the host with.
pub const ORIGIN: &str = "chrome-extension://";
/// Fixed by the public key in extension/manifest.json, so the unpacked extension always has this id.
const EXTENSION_ID: &str = "cpceimnjichdagbgnhbkcnciogbdahol";
/// A download job is small; anything bigger is not from the extension.
const MAX_MESSAGE: u32 = 64 * 1024;
const INBOX: &str = "inbox.jsonl";
/// The latest downloads as the extension shows them, and how many.
const PROGRESS: &str = "progress.json";
const RECENT: usize = 20;
/// How often a connected extension is told of a change.
const WATCH_EVERY: std::time::Duration = std::time::Duration::from_millis(500);
const MAX_URL: usize = 4000;
const MAX_HEADER: usize = 8000;
/// The extension, as the browser loads it from `<data folder>/extension`.
const EXTENSION: [(&str, &[u8]); 6] = [
    ("manifest.json", include_bytes!("../extension/manifest.json")),
    ("background.js", include_bytes!("../extension/background.js")),
    ("content.js", include_bytes!("../extension/content.js")),
    ("popup.html", include_bytes!("../extension/popup.html")),
    ("popup.js", include_bytes!("../extension/popup.js")),
    ("icon-128.png", include_bytes!("../extension/icon-128.png")),
];

/// The browsers by the folder they keep their data in, where a NativeMessagingHosts folder tells them of hosts.
#[cfg(target_os = "macos")]
const BROWSERS: [&str; 5] = ["Google/Chrome", "Microsoft Edge", "BraveSoftware/Brave-Browser", "Chromium", "CocCoc"];
#[cfg(all(unix, not(target_os = "macos")))]
const BROWSERS: [&str; 4] = ["google-chrome", "microsoft-edge", "BraveSoftware/Brave-Browser", "chromium"];
/// The browsers by their key under HKEY_CURRENT_USER\Software, whose NativeMessagingHosts key tells them of hosts.
#[cfg(windows)]
const BROWSERS: [&str; 5] = [r"Google\Chrome", r"Microsoft\Edge", r"BraveSoftware\Brave-Browser", "Chromium", r"CocCoc\Browser"];

/// A download taken from the browser: the address, a suggested name, and the headers the page used.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Job {
    pub url: String,
    pub filename: String,
    pub referrer: String,
    pub cookie: String,
    pub user_agent: String,
    /// The URL is an HLS playlist the video button picked.
    pub hls: bool,
    /// The URL is a video page, and this the yt-dlp format the video button picked.
    pub format: String,
    /// The size the video button estimated, for a page; 0 when unknown.
    pub size: u64,
}

impl Job {
    /// What a message of the extension asks to download, or None when it has no usable URL.
    fn from_message(message: &Value) -> Option<Job> {
        let field = |key: &str, max: usize| -> String { message[key].as_str().unwrap_or_default().chars().take(max).collect() };
        let url = field("url", MAX_URL);
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return None;
        }
        Some(Job {
            url,
            filename: crate::engine::sanitize(&field("filename", 300)),
            referrer: field("referrer", MAX_URL),
            cookie: field("cookie", MAX_HEADER),
            user_agent: field("userAgent", 1000),
            hls: message["kind"] == "hls",
            format: match message["kind"].as_str() {
                Some("page") => Some(field("format", 200)).filter(|f| crate::ytdlp::valid_format(f))?,
                _ => String::new(),
            },
            size: message["size"].as_f64().map_or(0, |n| n as u64),
        })
    }

    pub fn kind(&self) -> crate::engine::Kind {
        use crate::engine::Kind;
        match (self.hls, self.format.is_empty()) {
            (_, false) => Kind::Page { format: self.format.clone() },
            (true, true) => Kind::Hls,
            (false, true) => Kind::File,
        }
    }

    /// The request headers for this job, only the ones the page actually carried.
    pub fn headers(&self) -> Vec<(String, String)> {
        let mut headers = Vec::new();
        if !self.referrer.is_empty() {
            headers.push(("Referer".into(), self.referrer.clone()));
        }
        if !self.cookie.is_empty() {
            headers.push(("Cookie".into(), self.cookie.clone()));
        }
        if !self.user_agent.is_empty() {
            headers.push(("User-Agent".into(), self.user_agent.clone()));
        }
        headers
    }
}

/// Reads one native-messaging message: its length in four bytes in the machine's byte order, then the JSON.
pub fn read_message(input: &mut impl Read) -> io::Result<Value> {
    let mut len = [0; 4];
    input.read_exact(&mut len)?;
    let len = u32::from_ne_bytes(len);
    if len > MAX_MESSAGE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("a message of {len} bytes")));
    }
    let mut body = vec![0; len as usize];
    input.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Writes one native-messaging message, framed as `read_message` reads it.
pub fn write_message(output: &mut impl Write, message: &Value) -> io::Result<()> {
    let body = message.to_string();
    output.write_all(&(body.len() as u32).to_ne_bytes())?;
    output.write_all(body.as_bytes())?;
    output.flush()
}

/// The app as the browser's host: one message in, one answer out, or the progress for as long as the extension
/// keeps the port open when it asks to `watch`.
pub fn host(dir: &Path) {
    // Read before the match, which would otherwise hold stdin's lock for as long as `watch` runs.
    let message = read_message(&mut io::stdin().lock());
    let answer = match message {
        Ok(message) if message["cmd"] == "watch" => return watch(dir),
        Ok(message) => answer(dir, &message),
        Err(e) => json!({ "ok": false, "error": format!("Lanes không đọc được yêu cầu, hãy cập nhật tiện ích ({e})") }),
    };
    if let Err(e) = write_message(&mut io::stdout().lock(), &answer) {
        eprintln!("native messaging: {e}");
    }
}

/// Writes the latest of the downloads the window lists for a connected extension, when they changed since `last`.
pub fn publish(dir: &Path, items: &[Value], last: &mut String) {
    let text = Value::from(&items[..items.len().min(RECENT)]).to_string();
    if text != *last && fs::write(dir.join(PROGRESS), &text).is_ok() {
        *last = text;
    }
}

/// Sends the extension the latest downloads and whether the app is running, at once and then at each change, until
/// the browser closes the port: its end of stdin closes, which ends the host even while nothing changes.
fn watch(dir: &Path) {
    std::thread::spawn(|| {
        let _ = io::copy(&mut io::stdin(), &mut io::sink());
        std::process::exit(0);
    });
    let mut last = Value::Null;
    loop {
        // A list read while the app writes it is half a list, and waits for the next round.
        let list = match fs::read(dir.join(PROGRESS)) {
            Ok(bytes) => serde_json::from_slice(&bytes).ok(),
            Err(_) => Some(json!([])),
        };
        if let Some(list) = list {
            let now = json!({ "event": "progress", "live": running(dir), "list": list, "version": env!("CARGO_PKG_VERSION") });
            if now != last {
                if write_message(&mut io::stdout().lock(), &now).is_err() {
                    return;
                }
                last = now;
            }
        }
        std::thread::sleep(WATCH_EVERY);
    }
}

/// Shows a finished download in Finder or Explorer. Only one the progress lists, by its id, so the extension can
/// show no other file.
fn reveal(dir: &Path, id: i64) -> Value {
    let list: Value = fs::read(dir.join(PROGRESS)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let item = list.as_array().and_then(|l| l.iter().find(|i| i["id"] == id));
    let path = item.and_then(|i| Some(Path::new(i["dir"].as_str()?).join(i["filename"].as_str()?)));
    match path.filter(|p| p.exists()).map(|p| crate::reveal(&p)) {
        Some(Ok(_)) => json!({ "ok": true }),
        Some(Err(e)) => json!({ "ok": false, "error": e }),
        None => json!({ "ok": false, "error": "Không thấy file này nữa" }),
    }
}

/// The answer to one message of the extension: `ping` for the popup, `variants` for the resolutions of an HLS video,
/// `page` for the formats yt-dlp finds on a video page, `reveal` to show a finished download, and anything else is a
/// download, put in the inbox. Every answer carries the app's version, which tells the extension when the app has
/// updated its files (background.js).
fn answer(dir: &Path, message: &Value) -> Value {
    let mut reply = reply_to(dir, message);
    reply["version"] = json!(env!("CARGO_PKG_VERSION"));
    reply
}

fn reply_to(dir: &Path, message: &Value) -> Value {
    let invalid = || json!({ "ok": false, "error": "Liên kết không hợp lệ" });
    match message["cmd"].as_str() {
        Some("ping") => json!({ "ok": true }),
        Some("reveal") => reveal(dir, message["id"].as_i64().unwrap_or(0)),
        Some("variants") => match Job::from_message(message) {
            Some(job) => match crate::hls::choices(&job.url, &job.headers()) {
                Ok(choices) => {
                    let mut reply = json!(choices);
                    reply["ok"] = json!(true);
                    reply
                }
                Err(error) => json!({ "ok": false, "error": error }),
            },
            None => invalid(),
        },
        Some("page") => match Job::from_message(message) {
            Some(job) => match crate::ytdlp::formats(&job.url) {
                Ok(formats) => {
                    let mut reply = json!(formats);
                    reply["ok"] = json!(true);
                    reply
                }
                Err(error) => json!({ "ok": false, "error": error, "missing": crate::ytdlp::program("yt-dlp").is_none() }),
            },
            None => invalid(),
        },
        _ => match Job::from_message(message) {
            Some(job) => match push(dir, &job) {
                Ok(()) => {
                    wake(dir);
                    json!({ "ok": true })
                }
                Err(e) => json!({ "ok": false, "error": format!("Lanes không nhận được liên kết này ({e})") }),
            },
            None => invalid(),
        },
    }
}

/// Starts the app in the tray when it is not running, so a download sent from the browser begins at once instead of
/// waiting in the inbox for the next start. A bundle is started through `open`, so it is not a child of the browser.
fn wake(dir: &Path) {
    if running(dir) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let background = crate::autostart::BACKGROUND;
    let bundle = exe.ancestors().nth(3).filter(|p| cfg!(target_os = "macos") && p.extension().is_some_and(|e| e == "app"));
    let mut command = match bundle {
        Some(app) => {
            let mut c = Command::new("/usr/bin/open");
            c.args(["-g", "-a"]).arg(app).args(["--args", background]);
            c
        }
        None => {
            let mut c = Command::new(&exe);
            c.arg(background);
            c
        }
    };
    // The browser's pipes are this host's stdio; the app must not hold them.
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP, and out of the browser's job when it allows that.
        command.creation_flags(0x8 | 0x200 | 0x0100_0000);
        if command.spawn().is_ok() {
            return;
        }
        command.creation_flags(0x8 | 0x200);
    }
    if let Err(e) = command.spawn() {
        eprintln!("cannot start the app: {e}");
    }
}

/// Whether the app is running: it holds this lock (main.rs), so being able to take it means no app is.
fn running(dir: &Path) -> bool {
    File::create(dir.join("app.lock")).map_or(true, |f| f.try_lock().is_err())
}

/// Where the extension is kept for the browser to load, in the data folder.
pub fn extension_dir(dir: &Path) -> PathBuf {
    dir.join("extension")
}

/// Writes this version's extension to the data folder, file by file only where it changed.
pub fn install_extension(dir: &Path) -> io::Result<()> {
    let folder = extension_dir(dir);
    for (name, bytes) in EXTENSION {
        write_if_changed(&folder.join(name), bytes)?;
    }
    Ok(())
}

/// Adds a job at the end of the inbox, under a lock so the app never takes half of it.
fn push(dir: &Path, job: &Job) -> io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(dir.join(INBOX))?;
    file.lock()?;
    writeln!(file, "{}", json!(job))
}

/// The jobs waiting in the inbox, which is empty afterwards.
pub fn take(dir: &Path) -> Vec<Job> {
    let path = dir.join(INBOX);
    if !fs::metadata(&path).is_ok_and(|m| m.len() > 0) {
        return Vec::new();
    }
    let Ok(mut file) = OpenOptions::new().read(true).write(true).open(&path) else { return Vec::new() };
    if file.lock().is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let _ = file.set_len(0);
    String::from_utf8_lossy(&bytes).lines().filter_map(|line| serde_json::from_str(line).ok()).collect()
}

/// The file that tells a browser how to start the host and which extension may talk to it.
fn manifest(exe: &Path) -> String {
    let manifest = json!({
        "name": HOST,
        "description": "Lanes: nhận liên kết tải từ trình duyệt",
        "path": exe.to_string_lossy(),
        "type": "stdio",
        "allowed_origins": [format!("{ORIGIN}{EXTENSION_ID}/")],
    });
    serde_json::to_string_pretty(&manifest).unwrap_or_default() + "\n"
}

fn write_if_changed(path: &Path, text: impl AsRef<[u8]>) -> io::Result<bool> {
    let text = text.as_ref();
    if fs::read(path).is_ok_and(|old| old == text) {
        return Ok(false);
    }
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    fs::write(path, text).map(|()| true)
}

/// A `reg` command with no console window, used on Windows to point browsers at the host.
#[cfg(windows)]
pub fn reg_command() -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new("reg");
    cmd.creation_flags(0x0800_0000);
    cmd
}

/// Tells each installed browser where the host is, at every start since the app may have moved. A browser that is
/// not installed is passed over, and nothing that fails here stops the app.
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn register(dir: &Path) {
    let Ok(exe) = std::env::current_exe() else { return };
    let text = manifest(&exe);
    let name = format!("{HOST}.json");
    #[cfg(unix)]
    {
        let home = dirs::home_dir().unwrap_or_default();
        let base = if cfg!(target_os = "macos") { home.join("Library/Application Support") } else { home.join(".config") };
        for browser in BROWSERS.map(|b| base.join(b)).iter().filter(|b| b.is_dir()) {
            if let Err(e) = write_if_changed(&browser.join("NativeMessagingHosts").join(&name), &text) {
                eprintln!("native messaging host for {}: {e}", browser.display());
            }
        }
    }
    #[cfg(windows)]
    {
        let file = dir.join(&name);
        if let Err(e) = write_if_changed(&file, &text) {
            eprintln!("native messaging host: {e}");
            return;
        }
        let file = file.display().to_string();
        for browser in BROWSERS {
            let key = format!(r"HKCU\Software\{browser}\NativeMessagingHosts\{HOST}");
            let query = reg_command().args(["query", &key, "/ve"]).output();
            if query.is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains(&file)) {
                continue;
            }
            let added = reg_command()
                .args(["add", &key, "/ve", "/t", "REG_SZ", "/d", &file, "/f"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            if !added.is_ok_and(|s| s.success()) {
                eprintln!("native messaging host: cannot write {key}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_framed_by_their_length() {
        let mut wire = Vec::new();
        write_message(&mut wire, &json!({ "url": "https://x.com/a.zip" })).unwrap();
        let len = u32::from_ne_bytes(wire[..4].try_into().unwrap()) as usize;
        assert_eq!(len, wire.len() - 4);
        assert_eq!(read_message(&mut wire.as_slice()).unwrap()["url"], "https://x.com/a.zip");
        assert!(read_message(&mut &wire[..wire.len() - 1]).is_err());
        assert!(read_message(&mut &(MAX_MESSAGE + 1).to_ne_bytes()[..]).is_err());
    }

    #[test]
    fn a_message_becomes_a_job_with_its_headers() {
        let job = Job::from_message(&json!({
            "url": "https://x.com/f.zip", "filename": "f.zip", "referrer": "https://x.com/p",
            "cookie": "a=1", "userAgent": "Mozilla/5.0"
        }))
        .unwrap();
        assert_eq!(job.url, "https://x.com/f.zip");
        assert_eq!(job.headers(), vec![
            ("Referer".into(), "https://x.com/p".into()),
            ("Cookie".into(), "a=1".into()),
            ("User-Agent".into(), "Mozilla/5.0".into()),
        ]);
        assert!(!job.hls);
        assert!(Job::from_message(&json!({ "url": "https://x.com/m.m3u8", "kind": "hls" })).unwrap().hls);
        let page = Job::from_message(&json!({ "url": "https://youtube.com/watch?v=x", "kind": "page", "format": "136+140", "size": 1e6 })).unwrap();
        assert_eq!(page.kind(), crate::engine::Kind::Page { format: "136+140".into() });
        assert_eq!(page.size, 1_000_000);
        assert_eq!(Job::from_message(&json!({ "url": "https://youtube.com/watch?v=x", "kind": "page", "format": "18 --exec x" })), None);
        assert_eq!(Job::from_message(&json!({ "url": "javascript:alert(1)" })), None);
        assert_eq!(Job::from_message(&json!({ "filename": "a" })), None);
        assert!(Job::from_message(&json!({ "url": "https://x.com/a" })).unwrap().headers().is_empty());
    }

    #[test]
    fn the_inbox_hands_over_its_jobs_once() {
        let dir = std::env::temp_dir().join(format!("lanes-inbox-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let _ = fs::remove_file(dir.join(INBOX));
        assert!(take(&dir).is_empty());
        push(&dir, &Job { url: "https://x.com/a.zip".into(), filename: "a.zip".into(), ..Default::default() }).unwrap();
        push(&dir, &Job { url: "https://x.com/b.zip".into(), ..Default::default() }).unwrap();
        let jobs = take(&dir);
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].filename, "a.zip");
        assert!(take(&dir).is_empty(), "taken once");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_host_answers_a_ping_and_refuses_a_bad_link() {
        let dir = std::env::temp_dir();
        let pong = answer(&dir, &json!({ "cmd": "ping" }));
        assert_eq!(pong["ok"], true);
        assert_eq!(pong["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(answer(&dir, &json!({ "cmd": "variants", "url": "file:///etc/passwd" }))["ok"], false);
        let refused = answer(&dir, &json!({ "url": "ftp://x/a" }));
        assert_eq!(refused["ok"], false);
        assert_eq!(refused["version"], env!("CARGO_PKG_VERSION"), "every answer carries the version");
    }

    #[test]
    fn every_extension_file_is_carried_and_installed() {
        let mut on_disk: Vec<String> =
            fs::read_dir("extension").unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| !n.starts_with('.')).collect();
        let mut carried: Vec<String> = EXTENSION.iter().map(|(name, _)| name.to_string()).collect();
        on_disk.sort();
        carried.sort();
        assert_eq!(on_disk, carried, "a file in extension/ the app does not carry");

        let dir = std::env::temp_dir().join(format!("lanes-ext-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        install_extension(&dir).unwrap();
        assert_eq!(fs::read(extension_dir(&dir).join("manifest.json")).unwrap(), EXTENSION[0].1);
        install_extension(&dir).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_extension_id_comes_from_its_key() {
        use base64::Engine;
        use sha2::Digest;
        let manifest: Value = serde_json::from_str(include_str!("../extension/manifest.json")).unwrap();
        let key = base64::engine::general_purpose::STANDARD.decode(manifest["key"].as_str().unwrap()).unwrap();
        let id: String = sha2::Sha256::digest(&key)[..16].iter().flat_map(|b| [b >> 4, b & 15]).map(|n| char::from(b'a' + n)).collect();
        assert_eq!(id, EXTENSION_ID);
    }

    #[test]
    fn the_host_file_names_the_app_and_the_extension() {
        let text = manifest(Path::new("/Applications/Lanes.app/Contents/MacOS/lanes"));
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["name"], HOST);
        assert_eq!(parsed["path"], "/Applications/Lanes.app/Contents/MacOS/lanes");
        assert_eq!(parsed["allowed_origins"], json!([format!("chrome-extension://{EXTENSION_ID}/")]));
    }
}
