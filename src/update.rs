//! Whether a newer version is out, from the latest GitHub release, and putting it in place of the installed app on
//! macOS and Windows: download, check, swap beside the app, then a restart. The installed app is not touched until
//! the new one has passed every check; a failure at any step removes what was downloaded and leaves the app as it was.

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

pub const REPO: &str = "eliasbui/lanes";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const BUNDLE_ID: &str = "com.eliasbui.lanes";
/// The app inside the release zip, whatever the installed copy has been renamed to.
const APP: &str = "Lanes.app";
/// The program in the Windows zip, and the name scripts/install.ps1 gives it.
const EXE: &str = "Lanes.exe";
/// Where the running program waits on Windows while the new one takes its name: a running program cannot be deleted
/// there, only renamed, so it is deleted at the next start.
const OLD_EXE: &str = "Lanes.old.exe";
const PLATFORM: &str = if cfg!(windows) { "windows-x64" } else { "macos" };
const MIN_EXE: u64 = 1 << 20;
const STAGE: &str = ".Lanes-update";
const OLD: &str = ".Lanes-old";

#[derive(Debug, Serialize, PartialEq)]
pub struct Check {
    pub current: String,
    pub latest: String,
    pub url: String,
    pub newer: bool,
    pub notes: String,
    pub installable: bool,
    #[serde(skip)]
    pub asset: Option<Asset>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Asset {
    pub url: String,
    pub size: u64,
    pub sha256: Option<String>,
}

fn releases() -> String {
    format!("https://github.com/{REPO}/releases/")
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(15)).timeout_read(Duration::from_secs(20)).redirects(10).build()
}

/// The body of a URL, and its status, through GitHub's download links rather than the API where it can be, since the
/// API's limit of sixty calls an hour counts a whole network.
fn get(url: &str) -> Result<(u16, String), String> {
    match agent().get(url).set("User-Agent", "lanes").call() {
        Ok(r) => {
            let status = r.status();
            let body = r.into_string().map_err(|e| e.to_string())?;
            Ok((status, body))
        }
        Err(ureq::Error::Status(code, r)) => Ok((code, r.into_string().unwrap_or_default())),
        Err(e) => Err(format!("Không kết nối được: {e}")),
    }
}

/// The newest release. Each release carries `latest.json`, shaped like the API's answer and read through the
/// download links; the API is the fallback for anything without it.
pub fn check() -> Result<Check, String> {
    let manifest = format!("https://github.com/{REPO}/releases/latest/download/latest.json");
    if let Ok((200, body)) = get(&manifest) {
        if let Ok(check) = parse(&body, PLATFORM) {
            return Ok(check);
        }
    }
    let (status, body) = get(&format!("https://api.github.com/repos/{REPO}/releases/latest"))?;
    match status {
        200 => parse(&body, PLATFORM),
        404 => Err("Chưa có bản phát hành nào".into()),
        403 | 429 => Err("GitHub đang giới hạn số lần kiểm tra từ mạng của bạn, hãy thử lại sau ít phút".into()),
        _ => Err(format!("GitHub trả lỗi {status}")),
    }
}

fn parse(body: &str, platform: &str) -> Result<Check, String> {
    let json: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let latest = json["tag_name"].as_str().unwrap_or_default().trim_start_matches('v').to_owned();
    let url = json["html_url"].as_str().unwrap_or_default().to_owned();
    if !is_version(&latest) || !url.starts_with(&releases()) {
        return Err("Bản phát hành không có số phiên bản".into());
    }
    let notes: String = json["body"].as_str().unwrap_or_default().chars().take(2000).collect();
    let name = format!("Lanes-{latest}-{platform}.zip");
    let asset = json["assets"].as_array().and_then(|a| a.iter().find(|a| a["name"] == name.as_str())).and_then(|a| {
        Some(Asset {
            url: a["browser_download_url"].as_str().filter(|u| u.starts_with(&releases()))?.to_owned(),
            size: a["size"].as_u64()?,
            sha256: a["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).filter(|h| h.len() == 64).map(str::to_lowercase),
        })
    });
    let installable = asset.is_some() && installed().is_some();
    Ok(Check { newer: newer(&latest, CURRENT), current: CURRENT.into(), latest, url, notes, installable, asset })
}

/// The app this program runs from: the bundle on macOS, Lanes.exe on Windows. None in a build folder and elsewhere.
pub fn app_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    if cfg!(windows) {
        let name = exe.file_name()?.to_string_lossy().to_ascii_lowercase();
        return (name == EXE.to_ascii_lowercase()).then_some(exe);
    }
    if !cfg!(target_os = "macos") {
        return None;
    }
    bundle_of(&exe.canonicalize().ok()?)
}

pub fn installed() -> Option<PathBuf> {
    app_path().filter(|app| app.parent().is_some_and(writable))
}

fn writable(folder: &Path) -> bool {
    let probe = folder.join(".Lanes-write-test");
    let ok = fs::write(&probe, b"").is_ok();
    let _ = fs::remove_file(&probe);
    ok
}

fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent().filter(|d| d.file_name() == Some(OsStr::new("MacOS")))?;
    let contents = macos.parent().filter(|d| d.file_name() == Some(OsStr::new("Contents")))?;
    let bundle = contents.parent()?;
    bundle.extension().is_some_and(|e| e == "app").then(|| bundle.to_path_buf())
}

fn old_of(app: &Path) -> PathBuf {
    app.with_file_name(if app.extension().is_some_and(|e| e == "exe") { OLD_EXE } else { OLD })
}

/// Removes what a cut-short update left beside the app; the old copy only while the app itself is there.
pub fn tidy(app: &Path) {
    let Some(folder) = app.parent() else { return };
    let _ = fs::remove_dir_all(folder.join(STAGE));
    if app.exists() {
        let old = old_of(app);
        let _ = fs::remove_dir_all(&old);
        let _ = fs::remove_file(&old);
    }
}

pub fn install(check: &Check, app: &Path, progress: impl Fn(u8)) -> Result<(), String> {
    if !cfg!(windows) && plist_value(app, "CFBundleShortVersionString").is_ok_and(|v| v == check.latest) {
        return Ok(());
    }
    let asset = check.asset.as_ref().ok_or("Bản phát hành này chưa có gói cho máy này")?;
    let folder = app.parent().ok_or("Không thấy thư mục của ứng dụng")?;
    tidy(app);
    let stage = folder.join(STAGE);
    fs::create_dir_all(&stage).map_err(|e| format!("Không ghi được vào {}: {e}", folder.display()))?;
    let zip = stage.join("update.zip");
    let done = download(&asset.url, &zip, asset.size, &progress)
        .and_then(|()| if cfg!(windows) { put_exe_in_place(&zip, asset, app) } else { put_in_place(&zip, asset, &check.latest, app) });
    let _ = fs::remove_dir_all(&stage);
    done
}

/// Downloads `url` to `to`, reporting the percent as it comes.
fn download(url: &str, to: &Path, size: u64, progress: &impl Fn(u8)) -> Result<(), String> {
    let response = agent().get(url).set("User-Agent", "lanes").call().map_err(|e| format!("Không tải được bản cập nhật: {e}"))?;
    if response.status() != 200 {
        return Err(format!("Máy chủ trả mã {}", response.status()));
    }
    let mut reader = response.into_reader();
    let mut file = fs::File::create(to).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut got = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut file, &buf[..n]).map_err(|e| e.to_string())?;
        got += n as u64;
        progress((got.saturating_mul(100) / size.max(1)).min(99) as u8);
    }
    Ok(())
}

fn verify(zip: &Path, asset: &Asset) -> Result<(), String> {
    let size = fs::metadata(zip).map_err(|e| e.to_string())?.len();
    if size != asset.size {
        return Err(format!("Gói tải về nặng {size} byte, không phải {}", asset.size));
    }
    if asset.sha256.as_ref().is_some_and(|expected| sha256(zip).ok().as_ref() != Some(expected)) {
        return Err("Gói tải về không khớp mã kiểm tra trên GitHub".into());
    }
    Ok(())
}

fn put_exe_in_place(zip: &Path, asset: &Asset, exe: &Path) -> Result<(), String> {
    verify(zip, asset)?;
    let stage = zip.parent().ok_or("Không thấy thư mục tải về")?;
    let tar = if cfg!(windows) {
        PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())).join(r"System32\tar.exe")
    } else {
        PathBuf::from("/usr/bin/tar")
    };
    run(&tar, [OsStr::new("-xf"), zip.as_os_str(), OsStr::new("-C"), stage.as_os_str()])?;
    let fresh = stage.join(EXE);
    check_exe(&fresh)?;
    let old = old_of(exe);
    let _ = fs::remove_file(&old);
    swap(exe, &fresh, &old)
}

fn check_exe(exe: &Path) -> Result<(), String> {
    let mut head = [0u8; 2];
    let size = fs::metadata(exe).map(|m| m.len()).unwrap_or(0);
    let read = fs::File::open(exe).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head));
    if read.is_err() || &head != b"MZ" || size < MIN_EXE {
        return Err("Gói tải về không có chương trình Lanes".into());
    }
    Ok(())
}

fn put_in_place(zip: &Path, asset: &Asset, version: &str, bundle: &Path) -> Result<(), String> {
    verify(zip, asset)?;
    let stage = zip.parent().ok_or("Không thấy thư mục tải về")?;
    run("/usr/bin/ditto", [OsStr::new("-x"), OsStr::new("-k"), zip.as_os_str(), stage.as_os_str()])?;
    let fresh = stage.join(APP);
    check_app(&fresh, version)?;
    let old = bundle.with_file_name(OLD);
    swap(bundle, &fresh, &old)?;
    let _ = fs::remove_dir_all(&old);
    Ok(())
}

fn sha256(file: &Path) -> Result<String, String> {
    let mut hasher = Sha256::new();
    std::io::copy(&mut fs::File::open(file).map_err(|e| e.to_string())?, &mut hasher).map_err(|e| e.to_string())?;
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn check_app(app: &Path, version: &str) -> Result<(), String> {
    if plist_value(app, "CFBundleIdentifier").ok().as_deref() != Some(BUNDLE_ID) {
        return Err("Gói tải về không phải Lanes".into());
    }
    let found = plist_value(app, "CFBundleShortVersionString")?;
    if found != version {
        return Err(format!("Gói tải về là bản {found}, không phải {version}"));
    }
    if !app.join("Contents/MacOS/lanes").is_file() {
        return Err("Gói tải về không có chương trình".into());
    }
    run("/usr/bin/codesign", [OsStr::new("--verify"), OsStr::new("--strict"), app.as_os_str()]).map(drop)
}

fn plist_value(app: &Path, key: &str) -> Result<String, String> {
    let plist = app.join("Contents/Info.plist");
    let args = [OsStr::new("-extract"), OsStr::new(key), OsStr::new("raw"), OsStr::new("-o"), OsStr::new("-"), plist.as_os_str()];
    run("/usr/bin/plutil", args)
}

/// Moves the installed app aside and the new one in; a failed second move is undone, so an app is always in place.
fn swap(installed: &Path, fresh: &Path, old: &Path) -> Result<(), String> {
    fs::rename(installed, old).map_err(|e| format!("Không dời được bản đang cài: {e}"))?;
    if let Err(e) = fs::rename(fresh, installed) {
        let _ = fs::rename(old, installed);
        return Err(format!("Không đặt được bản mới vào chỗ: {e}"));
    }
    Ok(())
}

/// Starts the installed app again once this copy has quit.
pub fn relaunch() -> Result<Value, String> {
    if cfg!(windows) {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let moved = exe.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(OLD_EXE));
        let exe = if moved { exe.with_file_name(EXE) } else { exe };
        command("powershell")
            .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", "Start-Sleep 1; Start-Process -FilePath $env:LANES_EXE"])
            .env("LANES_EXE", &exe)
            .stdin(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        std::process::exit(0);
    }
    let bundle = app_path().ok_or("Bản này không khởi động lại được")?;
    command("/bin/sh").args(["-c", "sleep 1; /usr/bin/open -n \"$0\""]).arg(&bundle).spawn().map_err(|e| e.to_string())?;
    std::process::exit(0);
}

/// A command that shows no console window on Windows.
fn command(program: impl AsRef<OsStr>) -> std::process::Command {
    #[allow(unused_mut)]
    let mut cmd = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

fn run<'a>(program: impl AsRef<OsStr>, args: impl IntoIterator<Item = &'a OsStr>) -> Result<String, String> {
    let name = program.as_ref().to_string_lossy().into_owned();
    let output = command(program).args(args).stdin(Stdio::null()).output().map_err(|e| format!("{name}: {e}"))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    Err(format!("{name}: {}", String::from_utf8_lossy(&output.stderr).lines().next().unwrap_or("lỗi")))
}

pub fn is_release_url(url: &str) -> bool {
    url.starts_with(&releases())
}

fn is_version(text: &str) -> bool {
    let numbers = text.split('-').next().unwrap_or_default();
    numbers.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `latest` comes after `current`; anything after the numbers (`-beta.1`) is ignored.
pub fn newer(latest: &str, current: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> { v.split('-').next().unwrap_or_default().split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    let (mut a, mut b) = (parts(latest), parts(current));
    let len = a.len().max(b.len());
    a.resize(len, 0);
    b.resize(len, 0);
    a > b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0", "0.9.9"));
        assert!(!newer("1.0.0", "1.0"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(is_version("1.2.3") && is_version("0.1.0"));
        assert!(!is_version("") && !is_version("v1"));
    }

    #[test]
    fn parse_takes_only_this_repositorys_release() {
        let ok = r#"{"tag_name":"v9.0.0","html_url":"https://github.com/eliasbui/lanes/releases/tag/v9.0.0","body":"- new"}"#;
        let c = parse(ok, "macos").unwrap();
        assert!(c.newer && c.latest == "9.0.0" && c.notes == "- new");
        let elsewhere = r#"{"tag_name":"v9.0.0","html_url":"https://evil.example/releases/tag/v9.0.0"}"#;
        assert!(parse(elsewhere, "macos").is_err());
    }

    #[test]
    fn parse_finds_the_zip_of_this_platform() {
        let body = r#"{"tag_name":"v9.0.0","html_url":"https://github.com/eliasbui/lanes/releases/tag/v9.0.0","assets":[
            {"name":"Lanes-9.0.0-macos.zip","size":42,"digest":"sha256:BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD",
             "browser_download_url":"https://github.com/eliasbui/lanes/releases/download/v9.0.0/Lanes-9.0.0-macos.zip"}]}"#;
        let asset = parse(body, "macos").unwrap().asset.unwrap();
        assert_eq!(asset.size, 42);
        assert_eq!(asset.sha256.as_deref(), Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
        let evil = body.replace("https://github.com/eliasbui/lanes/releases/download", "https://evil.example");
        assert!(parse(&evil, "macos").unwrap().asset.is_none());
    }

    #[test]
    fn the_app_around_the_program_is_found() {
        assert_eq!(bundle_of(Path::new("/Applications/Lanes.app/Contents/MacOS/lanes")), Some(PathBuf::from("/Applications/Lanes.app")));
        assert_eq!(bundle_of(Path::new("/x/target/release/lanes")), None);
        assert_eq!(old_of(Path::new("/p/Lanes.exe")), PathBuf::from("/p/Lanes.old.exe"));
        assert_eq!(old_of(Path::new("/A/Lanes.app")), PathBuf::from("/A/.Lanes-old"));
    }
}
