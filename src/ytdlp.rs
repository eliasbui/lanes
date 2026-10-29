//! Video pages Lanes cannot read itself (src/extract reads YouTube, Facebook, Instagram, TikTok, X and what a page
//! names plainly), through yt-dlp when the user has it: it lists a page's formats, and downloads the one picked, video
//! and sound joined by ffmpeg. yt-dlp is the user's own install (Homebrew, winget), looked for where those put it,
//! since a browser or Finder starts the app with a bare PATH. ffmpeg comes with the app (scripts/build-ffmpeg.sh), the
//! user's own only in a build made without it. Without ffmpeg only formats that carry their own sound are offered,
//! which on YouTube is 360p.

use serde::Serialize;
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::Duration,
};

/// Folders Homebrew and winget put programs in, searched after PATH.
#[cfg(not(windows))]
const EXTRA: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];

/// One choice of the menu: the yt-dlp format to fetch, and what the user sees of it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Choice {
    pub format: String,
    /// None for sound only.
    pub height: Option<u32>,
    pub size: Option<u64>,
    pub ext: String,
}

/// What a page offers.
#[derive(Debug, Serialize)]
pub struct Formats {
    pub title: String,
    pub choices: Vec<Choice>,
    /// What to install for the higher resolutions, when ffmpeg is missing.
    pub hint: Option<String>,
}

fn install_hint(tools: &str) -> String {
    if cfg!(windows) {
        format!("Cài bằng PowerShell: winget install {tools}")
    } else {
        format!("Cài bằng Terminal: brew install {tools}")
    }
}

/// The folders to look for a program in: PATH, then where Homebrew or winget put it.
fn folders() -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    #[cfg(not(windows))]
    folders.extend(EXTRA.map(PathBuf::from));
    #[cfg(windows)]
    folders.extend(std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join(r"Microsoft\WinGet\Links")));
    folders
}

/// Where an installed program is, by its name without `.exe`.
pub fn program(name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() };
    folders().into_iter().map(|f| f.join(&file)).find(|p| p.is_file())
}

/// The ffmpeg the app carries, empty in a build made without it.
#[cfg(ffmpeg)]
const FFMPEG: &[u8] = include_bytes!(env!("LANES_FFMPEG"));
#[cfg(not(ffmpeg))]
const FFMPEG: &[u8] = &[];

/// ffmpeg: the one the app carries, put in the data folder, or else the user's own.
pub(crate) fn ffmpeg() -> Option<PathBuf> {
    let carried = crate::data_dir().join(if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" });
    (!FFMPEG.is_empty() && put_program(&carried, FFMPEG)).then_some(carried).or_else(|| program("ffmpeg"))
}

/// Puts `bytes` at `path` as a program, unless they are there already, and says whether one is there. They are
/// written beside it and renamed over it, so the app and the browser's host doing it at once never leave half a
/// program. On Windows a copy in use cannot be replaced, and the one there still joins files.
fn put_program(path: &Path, bytes: &[u8]) -> bool {
    if fs::read(path).is_ok_and(|b| b == bytes) {
        return true;
    }
    let part = path.with_extension(format!("{}.part", std::process::id()));
    let written = fs::write(&part, bytes).and_then(|()| executable(&part)).and_then(|()| fs::rename(&part, path));
    let _ = fs::remove_file(&part);
    written.is_ok() || path.is_file()
}

#[cfg(unix)]
fn executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn executable(_: &Path) -> std::io::Result<()> {
    Ok(())
}

/// yt-dlp with the folders above on its PATH, where it finds deno, which YouTube's player now needs.
fn yt_dlp() -> Result<Command, String> {
    let program = program("yt-dlp").ok_or_else(|| {
        let tools = if cfg!(windows) { "yt-dlp.yt-dlp DenoLand.Deno" } else { "yt-dlp" };
        format!("Cần yt-dlp để tải video từ trang này. {}", install_hint(tools))
    })?;
    let mut command = Command::new(program);
    if let Ok(path) = std::env::join_paths(folders()) {
        command.env("PATH", path);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command.stdin(Stdio::null());
    Ok(command)
}

/// The first `ERROR:` line yt-dlp wrote, or its last line.
fn error_of(stderr: &str, fallback: &str) -> String {
    let lines: Vec<&str> = stderr.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let line = lines.iter().find(|l| l.starts_with("ERROR:")).or(lines.last()).copied().unwrap_or(fallback);
    line.trim_start_matches("ERROR:").trim().chars().take(300).collect()
}

/// The browser's cookies for a page, in a file yt-dlp reads with `--cookies`, so a site that shows a video only to
/// someone signed in (Facebook, Instagram, X) shows it to yt-dlp too. The file is readable by this user only and goes
/// when yt-dlp is done.
struct Cookies(PathBuf);

impl Drop for Cookies {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn cookies(url: &str, headers: &[(String, String)]) -> Option<Cookies> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let header = headers.iter().find(|(name, _)| name.eq_ignore_ascii_case("cookie"))?;
    let text = cookie_file(url, &header.1)?;
    let path = std::env::temp_dir().join(format!("lanes-cookies-{}-{}.txt", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(&path).ok()?, text.as_bytes()).ok()?;
    Some(Cookies(path))
}

/// A Cookie header sent to `url` as a Netscape cookie file, each cookie for that site and its subdomains.
fn cookie_file(url: &str, header: &str) -> Option<String> {
    let site = site(url)?;
    let mut text = String::from("# Netscape HTTP Cookie File\n");
    for (name, value) in header.split(';').filter_map(|pair| pair.trim().split_once('=')) {
        if !name.is_empty() && !format!("{name}{value}").contains(['\t', '\r', '\n']) {
            text += &format!(".{site}\tTRUE\t/\tFALSE\t0\t{name}\t{value}\n");
        }
    }
    Some(text)
}

/// The host of `url` without `www.` or `m.`.
fn site(url: &str) -> Option<String> {
    let host = url::Url::parse(url).ok()?.host_str()?.to_ascii_lowercase();
    Some(host.strip_prefix("www.").or_else(|| host.strip_prefix("m.")).unwrap_or(&host).to_owned())
}

fn youtube(url: &str) -> bool {
    site(url).is_some_and(|site| ["youtube.com", "youtu.be", "youtube-nocookie.com"].iter().any(|y| site == *y || site.ends_with(&format!(".{y}"))))
}

/// What is said when a page shows the video only to someone signed in, in place of yt-dlp's advice about its options.
pub const SIGN_IN: &str = "Video này chỉ xem được khi đã đăng nhập (giới hạn độ tuổi, dành cho hội viên hoặc riêng tư). Hãy đăng nhập trang đó trong trình duyệt rồi bấm lại.";

/// Runs `run` with the file of the browser's cookies for the page. YouTube first goes without them and has them only
/// when it asks for someone signed in (an age check, a members' video), since yt-dlp warns that an account it uses
/// may be flagged. yt-dlp asks for cookies by naming its `--cookies` option.
fn signed_in<T>(url: &str, headers: &[(String, String)], run: impl Fn(Option<&Path>) -> Result<T, String>) -> Result<T, String> {
    let jar = cookies(url, headers);
    let jar = jar.as_ref().map(|jar| jar.0.as_path());
    let wants = |e: &String| e.contains("--cookies");
    let result = match run(jar.filter(|_| !youtube(url))) {
        Err(e) if wants(&e) && youtube(url) && jar.is_some() => run(jar),
        other => other,
    };
    result.map_err(|e| if wants(&e) { SIGN_IN.into() } else { e })
}

/// The formats of the page at `url`, as the menu offers them.
pub fn formats(url: &str, headers: &[(String, String)]) -> Result<Formats, String> {
    signed_in(url, headers, |jar| list(url, jar))
}

fn list(url: &str, jar: Option<&Path>) -> Result<Formats, String> {
    let mut command = yt_dlp()?;
    if let Some(jar) = jar {
        command.arg("--cookies").arg(jar);
    }
    let out = command.args(["-J", "--no-playlist", "--no-warnings", "--", url]).output().map_err(|e| format!("Không chạy được yt-dlp: {e}"))?;
    if !out.status.success() {
        return Err(error_of(&String::from_utf8_lossy(&out.stderr), "yt-dlp không đọc được trang này"));
    }
    let page: Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("yt-dlp trả về dữ liệu lạ: {e}"))?;
    let merge = ffmpeg().is_some();
    let choices = choices(&page, merge);
    if choices.is_empty() {
        return Err("Trang này không có video tải được".into());
    }
    // Said only when ffmpeg would add resolutions.
    let more = !merge && self::choices(&page, true).len() > choices.len();
    let hint = more.then(|| format!("Cần ffmpeg để tải bản 720p trở lên. {}", install_hint(if cfg!(windows) { "Gyan.FFmpeg" } else { "ffmpeg" })));
    Ok(Formats { title: page["title"].as_str().unwrap_or_default().to_owned(), choices, hint })
}

/// One format of yt-dlp's list, as far as the choices need it.
struct Format<'a> {
    id: &'a str,
    ext: &'a str,
    height: Option<u32>,
    video: bool,
    audio: bool,
    h264: bool,
    fps: u64,
    size: Option<u64>,
    stereo: bool,
}

fn format(f: &Value) -> Option<Format<'_>> {
    let codec = |key: &str| f[key].as_str().filter(|c| *c != "none");
    let format = Format {
        id: f["format_id"].as_str()?,
        ext: f["ext"].as_str().unwrap_or("mp4"),
        height: f["height"].as_u64().map(|h| h as u32),
        video: codec("vcodec").is_some(),
        audio: codec("acodec").is_some(),
        h264: codec("vcodec").is_some_and(|c| c.starts_with("avc1") || c.starts_with("h264")),
        fps: f["fps"].as_f64().unwrap_or(0.0) as u64,
        size: f["filesize"].as_u64().or_else(|| f["filesize_approx"].as_u64()),
        stereo: f["audio_channels"].as_u64().is_none_or(|c| c <= 2),
    };
    // Storyboards and YouTube's dynamic-range-compressed sound are not what a user means to download.
    (format.ext != "mhtml" && !format.id.contains("drc")).then_some(format)
}

/// The menu's choices from yt-dlp's description of a page, best first: one per resolution, then sound only. With
/// ffmpeg, each resolution is its best video stream, H.264 first since every player plays it, with the best stereo
/// M4A sound, joined into an MP4. Without it, only formats that already carry their sound. Each format names the exact
/// streams, then falls back on a looser rule in case the page changed by the time it is fetched.
pub fn choices(page: &Value, merge: bool) -> Vec<Choice> {
    let formats: Vec<Format> = page["formats"].as_array().map(|list| list.iter().filter_map(format).collect()).unwrap_or_default();
    // With ffmpeg a video stream joined to the best sound beats a format with its own, usually weaker, sound.
    let key = |f: &Format| (merge && !f.audio, f.h264, f.fps, f.size);
    let better = |a: &Format, b: &Format| key(a) > key(b);
    let sound = formats
        .iter()
        .filter(|f| f.audio && !f.video && f.ext == "m4a" && f.stereo)
        .max_by_key(|f| f.size);
    let mut best: Vec<&Format> = Vec::new();
    for f in &formats {
        let usable = f.video && f.height.is_some() && if merge { !f.audio && sound.is_some() || f.audio } else { f.audio };
        if !usable {
            continue;
        }
        match best.iter_mut().find(|b| b.height == f.height) {
            Some(slot) if better(f, slot) => *slot = f,
            Some(_) => {}
            None => best.push(f),
        }
    }
    best.sort_by_key(|f| std::cmp::Reverse(f.height));
    let mut choices: Vec<Choice> = best
        .into_iter()
        .map(|v| {
            let h = v.height.unwrap_or_default();
            match sound.filter(|_| !v.audio) {
                Some(a) => Choice {
                    format: format!("{}+{}/bv*[height<={h}]+ba/b[height<={h}]", v.id, a.id),
                    height: v.height,
                    size: v.size.zip(a.size).map(|(v, a)| v + a),
                    ext: "mp4".into(),
                },
                None => Choice { format: format!("{}/b[height<={h}]", v.id), height: v.height, size: v.size, ext: v.ext.into() },
            }
        })
        .collect();
    if let Some(a) = sound {
        choices.push(Choice { format: format!("{}/ba[ext=m4a]", a.id), height: None, size: a.size, ext: "m4a".into() });
    }
    choices
}

/// Whether a format from the extension is one `choices` could have made: format ids, `+`, `/` and simple filters.
pub fn valid_format(format: &str) -> bool {
    !format.is_empty() && format.len() <= 200 && format.chars().all(|c| c.is_ascii_alphanumeric() || "+/[]<=>*_.-".contains(c))
}

/// Downloads `format` of the page at `url` into `final_path`, its pieces kept in `temp` so a resume carries on from
/// them. `progress` gets the bytes so far, over every piece; `stop` is asked a few times a second and kills yt-dlp
/// when it says so. With net-merge, `proxy` is the app's proxy, which gives each connection to a network link. Gives
/// the file's size.
///
/// yt-dlp's end is its exit, not the end of its output: a process started alongside it (another download's yt-dlp, its
/// ffmpeg) can hold the output pipe open after it is gone, so the readers run on their own and are not waited for.
#[allow(clippy::too_many_arguments)]
pub fn download(url: &str, headers: &[(String, String)], format: &str, final_path: &Path, temp: &Path, proxy: Option<&str>, progress: impl Fn(u64), stop: impl Fn() -> bool) -> Result<u64, String> {
    signed_in(url, headers, |jar| fetch(url, jar, format, final_path, temp, proxy, &progress, &stop))
}

#[allow(clippy::too_many_arguments)]
fn fetch(url: &str, jar: Option<&Path>, format: &str, final_path: &Path, temp: &Path, proxy: Option<&str>, progress: &impl Fn(u64), stop: &impl Fn() -> bool) -> Result<u64, String> {
    let (Some(home), Some(name)) = (final_path.parent(), final_path.file_name()) else {
        return Err("Đường dẫn lưu không hợp lệ".into());
    };
    let mut command = yt_dlp()?;
    if let Some(jar) = jar {
        command.arg("--cookies").arg(jar);
    }
    command.args(["--no-playlist", "--quiet", "--progress", "--newline", "--no-warnings", "--no-mtime", "-f", format]);
    command.args(["--progress-template", "download:lanes %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s"]);
    command.args(["--merge-output-format", "mp4"]);
    if let Some(ffmpeg) = ffmpeg() {
        command.arg("--ffmpeg-location").arg(ffmpeg);
    }
    command.args(net_merge(proxy));
    // The name is taken as written: `%` would start one of yt-dlp's template fields.
    command.arg("-P").arg(format!("home:{}", home.display())).arg("-P").arg(format!("temp:{}", temp.display()));
    command.arg("-o").arg(name.to_string_lossy().replace('%', "%%")).arg("--").arg(url);
    let mut child = command.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("Không chạy được yt-dlp: {e}"))?;
    let (Some(stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err("Không đọc được yt-dlp".into());
    };
    let (lines, progressed) = mpsc::channel();
    thread::spawn(move || {
        let mut counter = Counter::default();
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if counter.read(&line).is_some_and(|done| lines.send(done).is_err()) {
                return;
            }
        }
    });
    let (text, written) = mpsc::channel();
    thread::spawn(move || {
        let mut errors = String::new();
        let _ = stderr.read_to_string(&mut errors);
        let _ = text.send(errors);
    });
    let status = loop {
        match progressed.recv_timeout(Duration::from_millis(200)) {
            Ok(done) => progress(done),
            Err(RecvTimeoutError::Disconnected) => thread::sleep(Duration::from_millis(200)),
            Err(RecvTimeoutError::Timeout) => {}
        }
        if stop() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Đã dừng".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => return Err(format!("yt-dlp bị lỗi: {e}")),
        }
    };
    progressed.try_iter().for_each(progress);
    match status.success() && final_path.is_file() {
        true => Ok(std::fs::metadata(final_path).map(|m| m.len()).unwrap_or(0)),
        false => {
            let errors = written.recv_timeout(Duration::from_secs(1)).unwrap_or_default();
            Err(error_of(&errors, &format!("yt-dlp dừng với mã {}", status.code().unwrap_or(-1))))
        }
    }
}

/// What net-merge adds to a download: the proxy, and a video in pieces (DASH, HLS, and YouTube's files cut into pieces
/// of 10 MiB by `dashy`, which keeps their format ids) eight at a time, each on a connection the proxy gives to a link.
/// A piece that cannot be had fails the download rather than leave a hole in the video.
fn net_merge(proxy: Option<&str>) -> Vec<String> {
    let Some(proxy) = proxy else { return Vec::new() };
    ["--proxy", proxy, "--concurrent-fragments", "8", "--extractor-args", "youtube:formats=dashy", "--abort-on-unavailable-fragments"].map(str::to_owned).into()
}

/// The bytes downloaded across the pieces yt-dlp fetches one after the other (the video, then the sound), from its
/// progress lines: when a piece starts again from zero, the one before it counts as whole.
#[derive(Default)]
struct Counter {
    before: u64,
    now: u64,
    total: u64,
}

impl Counter {
    fn read(&mut self, line: &str) -> Option<u64> {
        let mut words = line.split_whitespace();
        if words.next() != Some("lanes") {
            return None;
        }
        let mut number = || words.next().and_then(|w| w.parse::<f64>().ok()).map(|n| n as u64);
        let (now, total, estimate) = (number()?, number(), number());
        if now < self.now {
            self.before += self.total.max(self.now);
        }
        self.now = now;
        self.total = total.or(estimate).unwrap_or(now);
        Some(self.before + now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A YouTube page as yt-dlp describes it, cut down to the kinds of format it lists.
    fn page() -> Value {
        let f = |id: &str, ext: &str, height: Option<u32>, vcodec: &str, acodec: &str, fps: u32, size: Option<u64>| {
            json!({ "format_id": id, "ext": ext, "height": height, "vcodec": vcodec, "acodec": acodec, "fps": fps, "filesize": size, "audio_channels": if acodec == "none" { None } else { Some(2) } })
        };
        json!({ "title": "Big Buck Bunny", "formats": [
            f("sb0", "mhtml", Some(90), "none", "none", 0, None),
            f("139-drc", "m4a", None, "none", "mp4a.40.5", 0, Some(3)),
            f("140", "m4a", None, "none", "mp4a.40.2", 0, Some(10)),
            f("251", "webm", None, "none", "opus", 0, Some(11)),
            f("18", "mp4", Some(360), "avc1.42001E", "mp4a.40.2", 30, Some(28)),
            f("134", "mp4", Some(360), "avc1.4D401E", "none", 30, Some(18)),
            f("243", "webm", Some(360), "vp9", "none", 30, Some(24)),
            f("136", "mp4", Some(720), "avc1.4D401F", "none", 30, Some(87)),
            f("298", "mp4", Some(720), "avc1.4D4020", "none", 60, Some(150)),
            f("398", "mp4", Some(720), "av01.0.08M.08", "none", 60, Some(71)),
            f("312", "mp4", Some(1080), "avc1.64002A", "none", 60, None),
            f("401", "mp4", Some(2160), "av01.0.13M.08", "none", 60, Some(712)),
        ]})
    }

    #[test]
    fn the_browser_cookies_go_to_yt_dlp_for_the_site_only() {
        let file = cookie_file("https://www.facebook.com/reel/1", "c_user=7; xs=a:b=c ; bad; tab=x\ty").unwrap();
        assert_eq!(file, "# Netscape HTTP Cookie File\n.facebook.com\tTRUE\t/\tFALSE\t0\tc_user\t7\n.facebook.com\tTRUE\t/\tFALSE\t0\txs\ta:b=c\n");
        assert!(cookie_file("https://m.facebook.com/watch/?v=1", "a=1").unwrap().starts_with("# Netscape HTTP Cookie File\n.facebook.com\t"));
        assert!(cookie_file("https://www.youtube.com/watch?v=x", "SID=1").unwrap().contains(".youtube.com\tTRUE\t/\tFALSE\t0\tSID\t1"));
        let jar = cookies("https://x.com/a/status/1", &[("Cookie".into(), "auth_token=1".into())]).unwrap();
        let path = jar.0.clone();
        assert!(fs::read_to_string(&path).unwrap().contains(".x.com\tTRUE\t/\tFALSE\t0\tauth_token\t1"));
        #[cfg(unix)]
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&path).unwrap().permissions()) & 0o777, 0o600);
        drop(jar);
        assert!(!path.exists(), "the file goes with yt-dlp");
        assert!(cookies("https://x.com/a", &[]).is_none(), "no cookies, no file");
    }

    #[test]
    fn youtube_has_the_cookies_only_once_it_asks_for_someone_signed_in() {
        use std::cell::RefCell;
        let cookie = [("Cookie".to_owned(), "SID=1".to_owned())];
        let age = "[youtube] x: Sign in to confirm your age. Use --cookies-from-browser or --cookies for the authentication.";
        let tries = RefCell::new(Vec::new());
        let run = |jar: Option<&Path>| {
            tries.borrow_mut().push(jar.is_some());
            if jar.is_some() { Ok(1) } else { Err(age.to_owned()) }
        };
        assert_eq!(signed_in("https://www.youtube.com/watch?v=x", &cookie, run), Ok(1));
        assert_eq!(*tries.borrow(), [false, true], "without, then with");
        tries.borrow_mut().clear();
        assert_eq!(signed_in("https://www.youtube.com/watch?v=x", &[], run), Err(SIGN_IN.to_owned()), "not signed in in the browser");
        assert_eq!(*tries.borrow(), [false]);
        tries.borrow_mut().clear();
        let fine = |jar: Option<&Path>| {
            tries.borrow_mut().push(jar.is_some());
            Ok::<_, String>(2)
        };
        assert_eq!(signed_in("https://www.youtube.com/watch?v=x", &cookie, fine), Ok(2));
        assert_eq!(signed_in("https://www.facebook.com/reel/1", &cookie, fine), Ok(2));
        assert_eq!(*tries.borrow(), [false, true], "YouTube goes without when it can; other sites have them at once");
        let other = |_: Option<&Path>| Err::<u8, _>("HTTP Error 404".to_owned());
        assert_eq!(signed_in("https://www.youtube.com/watch?v=x", &cookie, other), Err("HTTP Error 404".to_owned()), "other errors are left as they are");
    }

    #[test]
    fn with_ffmpeg_each_resolution_joins_its_best_video_to_stereo_m4a() {
        let choices = choices(&page(), true);
        let heights: Vec<_> = choices.iter().map(|c| c.height).collect();
        assert_eq!(heights, vec![Some(2160), Some(1080), Some(720), Some(360), None]);
        assert_eq!(choices[2].format, "298+140/bv*[height<=720]+ba/b[height<=720]", "H.264 at 60 fps before AV1");
        assert_eq!(choices[2].size, Some(160));
        assert_eq!(choices[2].ext, "mp4");
        assert_eq!(choices[0].format, "401+140/bv*[height<=2160]+ba/b[height<=2160]", "no H.264 at 2160p");
        assert_eq!(choices[1].size, None, "a size yt-dlp does not know stays unknown");
        assert_eq!(choices[3].format, "134+140/bv*[height<=360]+ba/b[height<=360]");
        assert_eq!(choices[4], Choice { format: "140/ba[ext=m4a]".into(), height: None, size: Some(10), ext: "m4a".into() });
    }

    #[test]
    fn without_ffmpeg_only_formats_with_their_own_sound() {
        let choices = choices(&page(), false);
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0], Choice { format: "18/b[height<=360]".into(), height: Some(360), size: Some(28), ext: "mp4".into() });
        assert_eq!(choices[1].height, None);
        assert!(choices.iter().all(|c| valid_format(&c.format)));
        assert!(!valid_format("18 --exec rm"), "nothing but format characters");
        assert!(!valid_format(""));
    }

    #[test]
    fn the_carried_program_is_put_once_and_replaced_when_it_changes() {
        let dir = std::env::temp_dir().join(format!("lanes-ffmpeg-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ffmpeg");
        assert!(put_program(&path, b"one"));
        assert_eq!(fs::read(&path).unwrap(), b"one");
        #[cfg(unix)]
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&path).unwrap().permissions()) & 0o777, 0o755);
        assert!(put_program(&path, b"two"), "a newer app's ffmpeg replaces the old one");
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1, "no part file is left behind");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn net_merge_sends_yt_dlp_through_the_proxy_in_pieces_and_nothing_without_it() {
        assert!(net_merge(None).is_empty());
        let args = net_merge(Some("socks5h://lanes1:t@127.0.0.1:9"));
        let after = |flag: &str| args.iter().position(|a| a == flag).map(|i| args[i + 1].as_str());
        assert_eq!(after("--proxy"), Some("socks5h://lanes1:t@127.0.0.1:9"));
        assert_eq!(after("--concurrent-fragments"), Some("8"));
        assert_eq!(after("--extractor-args"), Some("youtube:formats=dashy"));
        assert!(args.iter().any(|a| a == "--abort-on-unavailable-fragments"));
    }

    #[test]
    fn progress_runs_on_across_the_video_and_the_sound() {
        let mut counter = Counter::default();
        assert_eq!(counter.read("[download] something else"), None);
        assert_eq!(counter.read("lanes 100 1000 NA"), Some(100));
        assert_eq!(counter.read("lanes 1000 1000 NA"), Some(1000));
        assert_eq!(counter.read("lanes 50 NA 300"), Some(1050), "the sound starts after the whole video");
        assert_eq!(counter.read("lanes NA NA NA"), None);
    }
}
