//! The browser extension in extension/: it takes over a download the browser started and sends it here, through
//! native messaging, with the address, the name, and the headers the page used (Referer, Cookie, User-Agent) so a
//! site that checks them serves the file to Lanes as it did to the browser. The browser starts the app itself as the
//! host; that copy opens no window, only drops the job in an inbox in the data folder and answers. The running app,
//! now or at its next start, takes the jobs and hands them to the engine.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::Path,
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
const MAX_URL: usize = 4000;
const MAX_HEADER: usize = 8000;

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
        })
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

/// The app as the browser's host: one message in, its job into the inbox, one answer out.
pub fn host(dir: &Path) {
    let answer = match read_message(&mut io::stdin().lock()) {
        Ok(message) => match Job::from_message(&message) {
            Some(job) => match push(dir, &job) {
                Ok(()) => json!({ "ok": true }),
                Err(e) => json!({ "ok": false, "error": format!("Lanes không nhận được liên kết này ({e})") }),
            },
            None => json!({ "ok": false, "error": "Liên kết không hợp lệ" }),
        },
        Err(e) => json!({ "ok": false, "error": format!("Lanes không đọc được yêu cầu, hãy cập nhật tiện ích ({e})") }),
    };
    if let Err(e) = write_message(&mut io::stdout().lock(), &answer) {
        eprintln!("native messaging: {e}");
    }
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

fn write_if_changed(path: &Path, text: &str) -> io::Result<bool> {
    if fs::read_to_string(path).is_ok_and(|old| old == text) {
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
