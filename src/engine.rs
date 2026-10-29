//! The download engine: several connections at once, each pulling one byte range of the file straight into its
//! place in the final file, with resume kept in a sidecar so a stopped download continues rather than restarts.
//!
//! One file is opened and grown to its full size up front; every connection writes its range at the right offset
//! with a positioned write, so no parts are joined at the end. What each connection has written is saved beside the
//! file a few times a minute, and on resume each one starts again from where it left off. The server is asked for
//! the same file by its ETag or modification time, so a file that changed on the server restarts instead of mixing
//! old and new bytes.
//!
//! The file starts as equal segments, one connection each. A connection that finishes its own takes over the second
//! half of what is left of the largest one, as IDM does, so a fast connection, or a fast network link with net-merge,
//! keeps working while a slow one would otherwise hold up the end of the file.

use crate::{extract, hls, netmerge, ytdlp};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// The most connections a download may open, and the fallback default.
pub const MAX_CONNECTIONS: usize = 32;
pub const DEFAULT_CONNECTIONS: usize = 8;
/// The smallest stretch one connection is given: a file is split only when it holds at least two of these, and the
/// connection count drops for a small file rather than cutting it into slivers that cost more in requests than they
/// save. Each connection then gets at least this much.
pub const MIN_PIECE: u64 = 1 << 20;
/// Read this many bytes from the socket before each positioned write.
const CHUNK: usize = 256 * 1024;
/// How often each connection's progress is written to the sidecar.
const SAVE_EVERY: Duration = Duration::from_secs(2);
/// A stalled connection is retried this many times before the download is called failed.
const RETRIES: u32 = 10;
const SECONDS: u64 = 30;

/// The state of a download, shared with the page as a small number.
pub mod state {
    pub const QUEUED: u8 = 0;
    pub const RUNNING: u8 = 1;
    pub const PAUSED: u8 = 2;
    pub const DONE: u8 = 3;
    pub const ERROR: u8 = 4;
}

/// What a download's URL is.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Kind {
    /// A file, fetched over several connections.
    #[default]
    File,
    /// An HLS playlist from the video button, its segments joined into the file (`run_hls`), and the playlist of its
    /// sound when that is apart from the picture, joined to it by ffmpeg; empty when the sound is in the picture's.
    Hls { audio: String },
    /// A video page (YouTube and the like) with the format picked in the video button: one Lanes reads itself
    /// (`extract::PREFIX` and a variant's id, `run_media`), or one of yt-dlp's (`run_page`).
    Page { format: String },
}

/// What a probe of the URL found, before any bytes are taken.
#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    pub size: Option<u64>,
    pub resumable: bool,
    pub filename: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// One byte range and how much of it has been written, saved so a resume knows where to carry on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub start: u64,
    /// Inclusive end, or `None` for a stream of unknown length taken on one connection.
    pub end: Option<u64>,
    pub done: u64,
}

/// The sidecar beside the `.part` file: enough to resume, and to be sure the server still has the same file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub url: String,
    pub size: Option<u64>,
    pub resumable: bool,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub last_modified: Option<String>,
    pub segments: Vec<Segment>,
}

impl Meta {
    /// The bytes written so far across every segment.
    pub fn done(&self) -> u64 {
        self.segments.iter().map(|s| s.done).sum()
    }
}

/// The end of a part of unknown length, which is read to EOF and never split.
const OPEN_END: u64 = u64::MAX;

/// A segment being fetched, shared by its connection and the one that may take over its second half: `end` (inclusive)
/// comes down when that happens, and the connection stops there.
struct Part {
    start: u64,
    end: AtomicU64,
    done: AtomicU64,
}

impl Part {
    fn new(segment: &Segment) -> Arc<Part> {
        Arc::new(Part { start: segment.start, end: AtomicU64::new(segment.end.unwrap_or(OPEN_END)), done: AtomicU64::new(segment.done) })
    }

    /// A connection that shows in the count while a playlist or yt-dlp does the work.
    fn idle() -> Arc<Part> {
        Part::new(&Segment { start: 0, end: None, done: 0 })
    }

    /// The bytes still to fetch, or `None` for a part of unknown length.
    fn left(&self) -> Option<u64> {
        let end = self.end.load(Ordering::SeqCst);
        (end != OPEN_END).then(|| (end + 1).saturating_sub(self.start + self.done.load(Ordering::Relaxed)))
    }

    fn finished(&self) -> bool {
        self.left() == Some(0)
    }

    fn segment(&self) -> Segment {
        let end = self.end.load(Ordering::SeqCst);
        Segment { start: self.start, end: (end != OPEN_END).then_some(end), done: self.done.load(Ordering::Relaxed) }
    }
}

/// For a connection that has finished its part: the second half of what is left of the part with the most left, as a
/// new part with its index, or `None` when no part has enough left (two `MIN_PIECE`s) to be worth another connection.
fn steal(parts: &Mutex<Vec<Arc<Part>>>) -> Option<(Arc<Part>, usize)> {
    let mut parts = parts.lock().ok()?;
    let (victim, left) = parts.iter().filter_map(|p| p.left().map(|left| (p, left))).max_by_key(|&(_, left)| left)?;
    if left < 2 * MIN_PIECE {
        return None;
    }
    let end = victim.end.load(Ordering::SeqCst);
    // The half after the connection, at least a MIN_PIECE ahead of it, so it never gets there before it sees the new end.
    let at = end + 1 - left / 2;
    victim.end.store(at - 1, Ordering::SeqCst);
    let part = Arc::new(Part { start: at, end: AtomicU64::new(end), done: AtomicU64::new(0) });
    parts.push(part.clone());
    Some((part, parts.len() - 1))
}

/// One whole-file segment, with no end for a file of unknown length so the connection reads to EOF.
fn whole(size: u64) -> Vec<Segment> {
    vec![Segment { start: 0, end: (size > 0).then(|| size - 1), done: 0 }]
}

/// Equal byte ranges for up to `conns` connections over a file of `size`, each at least `min_piece`. Inclusive
/// ends. One range for a file too small to split, or of unknown length.
pub fn plan_segments(size: u64, conns: usize, min_piece: u64) -> Vec<Segment> {
    let conns = conns.clamp(1, MAX_CONNECTIONS) as u64;
    let min_piece = min_piece.max(1);
    if conns <= 1 || size < min_piece.saturating_mul(2) {
        return whole(size);
    }
    let wanted = conns.min(size / min_piece).max(1);
    if wanted <= 1 {
        return whole(size);
    }
    let each = size / wanted;
    (0..wanted)
        .map(|i| {
            let start = i * each;
            let end = if i + 1 == wanted { size - 1 } else { start + each - 1 };
            Segment { start, end: Some(end), done: 0 }
        })
        .collect()
}

/// The name to save under: the filename in a Content-Disposition header, else the last part of the URL path, else
/// a plain default. Always a safe single filename.
pub fn filename_from(url: &str, content_disposition: Option<&str>) -> String {
    if let Some(cd) = content_disposition {
        if let Some(name) = disposition_filename(cd) {
            let name = sanitize(&name);
            if !name.is_empty() {
                return name;
            }
        }
    }
    // Drop the scheme and host, then the query and fragment, and take the last part of the path.
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let no_query = rest.split(['?', '#']).next().unwrap_or(rest);
    let path = no_query.split_once('/').map(|(_, p)| p).unwrap_or("");
    let last = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("");
    let name = sanitize(&percent_decode(last));
    if name.is_empty() { "download".into() } else { name }
}

/// The filename from a Content-Disposition header: `filename*=UTF-8''name` first, then a quoted or bare `filename=`.
fn disposition_filename(cd: &str) -> Option<String> {
    for part in cd.split(';').map(str::trim) {
        if let Some(rest) = part.strip_prefix("filename*=") {
            let value = rest.rsplit("''").next().unwrap_or(rest);
            return Some(percent_decode(value.trim_matches('"')));
        }
    }
    for part in cd.split(';').map(str::trim) {
        if let Some(rest) = part.strip_prefix("filename=") {
            return Some(rest.trim_matches('"').to_owned());
        }
    }
    None
}

/// One filename with no path separators or characters illegal on macOS or Windows, no trailing dot or space that
/// Windows forbids, and a sane length. A leading dot is kept, so a dotfile stays one.
pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c })
        .collect();
    cleaned.trim().trim_end_matches(['.', ' ']).chars().take(200).collect()
}

/// Percent-decoding for a URL path or an encoded filename; invalid escapes are left as written.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let hex = |b: u8| (b as char).to_digit(16);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The categories of IDM: the folder inside the download folder a file goes to by its extension. A file in none of them
/// (IDM's "General") stays in the download folder itself. An entry ending in `*` matches by prefix, for the parts of a
/// split archive (r00, r01, ...). IDM's own lists, with the macOS installers (dmg, pkg) added to Programs.
const CATEGORIES: [(&str, &[&str]); 5] = [
    ("Compressed", &["zip", "rar", "r0*", "r1*", "arj", "gz", "sit", "sitx", "sea", "ace", "bz2", "7z"]),
    ("Documents", &["doc", "pdf", "ppt", "pps", "docx", "pptx"]),
    ("Music", &["mp3", "wav", "wma", "mpa", "ram", "ra", "aac", "aif", "m4a", "tsa"]),
    ("Programs", &["exe", "msi", "dmg", "pkg"]),
    ("Video", &["avi", "mpg", "mpe", "mpeg", "asf", "wmv", "mov", "qt", "rm", "mp4", "flv", "m4v", "webm", "ogv", "ogg", "mkv", "ts", "tsv"]),
];

/// The category folder for `filename`, or None for a general file.
pub fn category(filename: &str) -> Option<&'static str> {
    let ext = filename.rsplit_once('.')?.1.to_ascii_lowercase();
    CATEGORIES.iter().find_map(|(folder, exts)| {
        exts.iter()
            .any(|e| match e.strip_suffix('*') {
                Some(prefix) => ext.starts_with(prefix),
                None => ext == *e,
            })
            .then_some(*folder)
    })
}

/// A free path when `wanted` is taken: `name (2).ext`, `name (3).ext` and so on.
pub fn unique_path(dir: &Path, wanted: &str) -> PathBuf {
    let first = dir.join(wanted);
    if !first.exists() && !part_of(&first).exists() {
        return first;
    }
    let (stem, ext) = match wanted.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_owned(), format!(".{e}")),
        _ => (wanted.to_owned(), String::new()),
    };
    for n in 2..10_000 {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() && !part_of(&candidate).exists() {
            return candidate;
        }
    }
    first
}

/// The partly-downloaded file's name, `name.ext.lanesdl`, kept until the download is whole.
fn part_of(final_path: &Path) -> PathBuf {
    let mut name = final_path.file_name().unwrap_or_default().to_os_string();
    name.push(".lanesdl");
    final_path.with_file_name(name)
}

/// The sidecar's name, beside the part file.
fn meta_of(final_path: &Path) -> PathBuf {
    let mut name = final_path.file_name().unwrap_or_default().to_os_string();
    name.push(".lanesdl.meta");
    final_path.with_file_name(name)
}

/// Writes at a given offset without moving a shared cursor, so every connection can write its range at once.
fn write_at(file: &fs::File, offset: u64, buf: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.write_all_at(buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut written = 0;
        while written < buf.len() {
            let n = file.seek_write(&buf[written..], offset + written as u64)?;
            if n == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "seek_write wrote nothing"));
            }
            written += n;
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (file, offset, buf);
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "positioned writes are unsupported here"))
    }
}

/// A running or finished download, shared between the engine and its connection threads.
pub struct Task {
    pub id: i64,
    pub url: String,
    pub final_path: PathBuf,
    /// net-merge (beta): the mirror URLs for the same file, the primary first. One entry is an ordinary download;
    /// several spread the segments over the mirrors and merge their bandwidth, each segment moving to the next
    /// mirror when one fails. Narrowed in `run` to the mirrors that agree on the size and allow ranges.
    sources: Mutex<Vec<String>>,
    /// Referer, Cookie and User-Agent from the page, so a site that checks them serves the same file.
    headers: Vec<(String, String)>,
    kind: Kind,
    /// The name net-merge knows the download by, and its proxy for ureq (`socks5`) and for yt-dlp (`socks5h`); `None`
    /// without net-merge.
    job: String,
    via: Option<String>,
    via_ytdlp: Option<String>,
    state: AtomicU8,
    downloaded: AtomicU64,
    total: AtomicU64,
    error: Mutex<String>,
    segments: Mutex<Vec<Arc<Part>>>,
    meta: Mutex<Meta>,
}

/// What the page is shown for one download.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub id: i64,
    pub url: String,
    pub filename: String,
    pub state: u8,
    pub size: Option<u64>,
    pub downloaded: u64,
    pub connections: usize,
    /// How many mirror sources are in use (net-merge beta); 1 for an ordinary download.
    pub sources: usize,
    /// How many network links carry its connections now (net-merge).
    pub links: usize,
    pub error: String,
}

impl Task {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            id: self.id,
            url: self.url.clone(),
            filename: self.final_path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
            state: self.state.load(Ordering::Relaxed),
            size: match self.total.load(Ordering::Relaxed) {
                0 => None,
                n => Some(n),
            },
            downloaded: self.downloaded.load(Ordering::Relaxed),
            connections: self.segments.lock().map(|s| s.iter().filter(|p| !p.finished()).count()).unwrap_or(0),
            sources: self.sources.lock().map(|s| s.len().max(1)).unwrap_or(1),
            links: 0,
            error: self.error.lock().map(|e| e.clone()).unwrap_or_default(),
        }
    }

    fn set_error(&self, message: String) {
        if let Ok(mut slot) = self.error.lock() {
            *slot = message;
        }
        self.state.store(state::ERROR, Ordering::SeqCst);
    }

    fn paused_or_removed(&self) -> bool {
        !matches!(self.state.load(Ordering::Relaxed), state::RUNNING | state::QUEUED)
    }
}

/// The engine: the live downloads, each on its own threads, polled by the page for progress.
#[derive(Default)]
pub struct Engine {
    tasks: Mutex<Vec<Arc<Task>>>,
    connections: AtomicU64,
    /// net-merge: whether a download that starts goes through it, and the proxy, there from the first such download
    /// until none runs. Locked before `tasks` wherever both are.
    net_merge: AtomicBool,
    merge: Mutex<Option<netmerge::Merge>>,
}

impl Engine {
    pub fn new() -> Self {
        Engine {
            tasks: Mutex::new(Vec::new()),
            connections: AtomicU64::new(DEFAULT_CONNECTIONS as u64),
            net_merge: AtomicBool::new(false),
            merge: Mutex::new(None),
        }
    }

    /// Whether downloads started from now on spread their connections over every network link.
    pub fn set_net_merge(&self, on: bool) {
        self.net_merge.store(on, Ordering::Relaxed);
    }

    /// The links as net-merge sees them: live while a download goes through it, else as they are found.
    pub fn net_stats(&self) -> netmerge::Stats {
        match self.merge.lock().ok().as_deref().and_then(Option::as_ref) {
            Some(merge) => merge.stats(),
            None => netmerge::idle_stats(),
        }
    }

    /// Lets the proxy go once no download runs; the next download through net-merge starts it again.
    pub fn release_idle(&self) {
        let Ok(mut merge) = self.merge.lock() else { return };
        let running = self.tasks.lock().map(|t| t.iter().any(|t| !t.paused_or_removed())).unwrap_or(true);
        if merge.is_some() && !running {
            *merge = None;
        }
    }

    pub fn set_connections(&self, n: usize) {
        self.connections.store(n.clamp(1, MAX_CONNECTIONS) as u64, Ordering::Relaxed);
    }

    /// The speed limit over every download together, in bytes a second; 0 for none. It takes effect at once.
    pub fn set_speed_limit(&self, bytes_per_second: u64) {
        LIMIT.store(bytes_per_second, Ordering::Relaxed);
    }

    fn connection_count(&self) -> usize {
        self.connections.load(Ordering::Relaxed) as usize
    }

    /// Starts a fresh download, or picks up one left half-done from a sidecar beside `final_path`. `sources` is the
    /// mirror pool for net-merge, the primary first (a one-item list is an ordinary download). The headers are the
    /// ones the browser carried, replayed on every connection. `kind` says what the URL is.
    pub fn start(self: &Arc<Self>, id: i64, sources: Vec<String>, final_path: PathBuf, headers: Vec<(String, String)>, kind: Kind) {
        let url = sources.first().cloned().unwrap_or_default();
        let resume = load_meta(&final_path).filter(|m| m.url == url);
        // Held until the task is listed, so that `release_idle` cannot drop the proxy it is given.
        let mut merge = self.merge.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.net_merge.load(Ordering::Relaxed) && merge.is_none() {
            match netmerge::Merge::start() {
                Ok(started) => *merge = Some(started),
                Err(e) => eprintln!("net-merge did not start: {e}"),
            }
        }
        let job = format!("lanes{id}");
        let via = |scheme| merge.as_ref().filter(|_| self.net_merge.load(Ordering::Relaxed)).map(|m| m.proxy_url(scheme, &job));
        let (via, via_ytdlp) = (via("socks5"), via("socks5h"));
        let task = Arc::new(Task {
            id,
            url: url.clone(),
            final_path: final_path.clone(),
            sources: Mutex::new(sources),
            headers,
            kind,
            job,
            via,
            via_ytdlp,
            state: AtomicU8::new(state::RUNNING),
            downloaded: AtomicU64::new(resume.as_ref().map(Meta::done).unwrap_or(0)),
            total: AtomicU64::new(resume.as_ref().and_then(|m| m.size).unwrap_or(0)),
            error: Mutex::new(String::new()),
            segments: Mutex::new(Vec::new()),
            meta: Mutex::new(resume.clone().unwrap_or(Meta {
                url: url.to_owned(),
                size: None,
                resumable: false,
                etag: None,
                last_modified: None,
                segments: Vec::new(),
            })),
        });
        if let Ok(mut tasks) = self.tasks.lock() {
            tasks.retain(|t| t.id != id);
            tasks.push(task.clone());
        }
        drop(merge);
        let engine = self.clone();
        thread::spawn(move || engine.run(task, resume));
    }

    /// Flips a running download to paused; its connections stop and the sidecar keeps the progress.
    pub fn pause(&self, id: i64) {
        if let Some(task) = self.find(id) {
            let _ = task.state.compare_exchange(state::RUNNING, state::PAUSED, Ordering::SeqCst, Ordering::SeqCst);
        }
    }

    /// Stops a download and, when asked, deletes the part file and its sidecar.
    pub fn remove(&self, id: i64, delete_files: bool) {
        let task = self.find(id);
        if let Some(task) = &task {
            task.state.store(state::ERROR, Ordering::SeqCst);
        }
        if let Ok(mut tasks) = self.tasks.lock() {
            tasks.retain(|t| t.id != id);
        }
        if delete_files {
            if let Some(task) = task {
                discard(&task.final_path);
            }
        }
    }

    pub fn snapshots(&self) -> Vec<Snapshot> {
        let merge = self.merge.lock();
        let merge = merge.as_ref().ok().and_then(|m| m.as_ref());
        let tasks = self.tasks.lock().map(|t| t.clone()).unwrap_or_default();
        tasks
            .iter()
            .map(|t| Snapshot { links: merge.map_or(0, |m| m.links_used(&t.job)), ..t.snapshot() })
            .collect()
    }

    fn find(&self, id: i64) -> Option<Arc<Task>> {
        self.tasks.lock().ok()?.iter().find(|t| t.id == id).cloned()
    }

    /// The manager thread of one download: probe, open and size the part file, fan out one thread per segment,
    /// save progress while they run, then put the finished file in place.
    fn run(&self, task: Arc<Task>, resume: Option<Meta>) {
        match &task.kind {
            Kind::Hls { .. } => return self.run_hls(&task),
            Kind::Page { format } => {
                return match format.strip_prefix(extract::PREFIX) {
                    Some(id) => self.run_media(&task, id),
                    None => run_page(&task, format),
                };
            }
            Kind::File => {}
        }
        let probe = match resume.as_ref() {
            // A resume trusts the sidecar; it checked the URL already, and re-probing could change the segments.
            Some(meta) => Probe {
                size: meta.size,
                resumable: meta.resumable,
                filename: String::new(),
                etag: meta.etag.clone(),
                last_modified: meta.last_modified.clone(),
            },
            None => match probe(&task.url, &task.headers) {
                Ok(probe) => probe,
                Err(e) => return task.set_error(e),
            },
        };
        if let Some(size) = probe.size {
            task.total.store(size, Ordering::Relaxed);
        }

        // net-merge (beta): keep the mirrors that agree on the size and allow ranges; otherwise fall back to the one
        // primary source, since a merge needs every mirror to serve the same bytes by range.
        {
            let listed = task.sources.lock().map(|s| s.clone()).unwrap_or_default();
            let active = match probe.size {
                Some(size) if probe.resumable && listed.len() > 1 => narrow_sources(&listed, size, &task.headers),
                _ => listed.into_iter().take(1).collect(),
            };
            if let Ok(mut slot) = task.sources.lock() {
                *slot = active;
            }
        }

        let part = part_of(&task.final_path);
        let file = match open_part(&part, probe.size) {
            Ok(file) => Arc::new(file),
            Err(e) => return task.set_error(format!("Không tạo được tệp tạm: {e}")),
        };

        let segments = match resume {
            Some(meta) if !meta.segments.is_empty() => meta.segments,
            _ => match probe.size {
                Some(size) if probe.resumable => plan_segments(size, self.connection_count(), MIN_PIECE),
                Some(size) => whole(size),
                None => whole(0),
            },
        };

        let parts: Vec<Arc<Part>> = segments.iter().map(Part::new).collect();
        if let Ok(mut slot) = task.segments.lock() {
            *slot = parts.clone();
        }
        if let Ok(mut meta) = task.meta.lock() {
            *meta = Meta {
                url: task.url.clone(),
                size: probe.size,
                resumable: probe.resumable,
                etag: probe.etag.clone(),
                last_modified: probe.last_modified.clone(),
                segments: segments.clone(),
            };
        }
        // Count only what this resume still has to fetch, not what the sidecar already holds.
        task.downloaded.store(segments.iter().map(|s| s.done).sum(), Ordering::Relaxed);

        let mut handles = Vec::new();
        // A resume leaves the parts that were finished alone: they are on disk, and each connection had one part.
        for (i, part) in parts.iter().cloned().enumerate().filter(|(_, p)| !p.finished()) {
            let (task, file) = (task.clone(), file.clone());
            handles.push(thread::spawn(move || {
                // Each part starts on a different mirror, so the sources are used in parallel from the first moment.
                let (mut part, mut i) = (part, i);
                loop {
                    if !download_segment(&task, &file, &part, i) {
                        return false;
                    }
                    match steal(&task.segments) {
                        Some((next, n)) => (part, i) = (next, n),
                        None => return true,
                    }
                }
            }));
        }

        // Save the sidecar while the connections run, so a crash still leaves a resumable file. The flag stops the
        // saver once the connections have joined; it cannot wait on the state, which is only set after this returns.
        let saving = Arc::new(AtomicBool::new(true));
        let saver = {
            let (task, saving) = (task.clone(), saving.clone());
            thread::spawn(move || {
                while saving.load(Ordering::Relaxed) {
                    save_meta(&task);
                    for _ in 0..20 {
                        if !saving.load(Ordering::Relaxed) {
                            break;
                        }
                        thread::sleep(SAVE_EVERY / 20);
                    }
                }
                save_meta(&task);
            })
        };

        let all_ok = handles.into_iter().all(|h| h.join().unwrap_or(false));
        saving.store(false, Ordering::Relaxed);
        let _ = saver.join();

        if task.state.load(Ordering::Relaxed) == state::ERROR {
            return;
        }
        if task.paused_or_removed() {
            return;
        }
        if !all_ok {
            if task.error.lock().map(|e| e.is_empty()).unwrap_or(true) {
                task.set_error("Tải không hoàn tất".into());
            }
            return;
        }
        if let Err(e) = finish(&file, &part, &task.final_path, probe.size) {
            return task.set_error(e);
        }
        let _ = fs::remove_file(meta_of(&task.final_path));
        task.state.store(state::DONE, Ordering::SeqCst);
    }

    /// An HLS video (src/hls.rs): the segments of its playlist, several at a time, each kept as a numbered file in a
    /// folder beside the final file so a resume skips the ones already there; then joined in order into the file, an MP4
    /// when its name asks for one (`settle`). A sound that is a playlist of its own comes the same way, into a folder
    /// of its own inside that one, and ffmpeg puts it with the picture.
    /// The size shown is an estimate from the segments so far until the join gives the real one.
    fn run_hls(&self, task: &Task) {
        let Kind::Hls { audio } = &task.kind else { return };
        let picture = match hls::load(&task.url, &task.headers) {
            Ok(media) => media,
            Err(e) => return task.set_error(e),
        };
        // The headers were chosen for the picture's playlist, so its address is the one a cookie among them is for.
        let sound = match audio.as_str() {
            "" => None,
            url => match hls::media_at(url, &task.url, &task.headers) {
                Ok(media) => Some(media),
                Err(e) => return task.set_error(format!("Không tải được tiếng của video: {e}")),
            },
        };
        let folder = part_of(&task.final_path);
        let sound_folder = folder.join("sound");
        if let Err(e) = fs::create_dir_all(if sound.is_some() { &sound_folder } else { &folder }) {
            return task.set_error(format!("Không tạo được thư mục tạm: {e}"));
        }
        task.downloaded.store(0, Ordering::Relaxed);
        let mut pieces = playlist_pieces(picture, &folder, &task.headers, &task.url);
        let split = pieces.len();
        if let Some(media) = sound {
            pieces.extend(playlist_pieces(media, &sound_folder, &task.headers, &task.url));
        }
        if !self.keep_all(task, &pieces, true) {
            return;
        }
        let paths = |pieces: &[Piece]| pieces.iter().map(|p| p.path.clone()).collect::<Vec<PathBuf>>();
        let (video, sound) = (folder.join("video.ts"), folder.join("sound.ts"));
        let apart = split < pieces.len();
        let made = join_parts(&paths(&pieces[..split]), &folder, &video)
            .and_then(|_| if apart { join_parts(&paths(&pieces[split..]), &folder, &sound).map(|_| ()) } else { Ok(()) })
            .map_err(|e| format!("Không ghép được video: {e}"))
            .and_then(|()| if apart { mux(&video, &sound, &folder, &task.final_path) } else { settle(&video, &folder, &task.final_path) });
        match made.and_then(|()| fs::metadata(&task.final_path).map(|m| m.len()).map_err(|e| e.to_string())) {
            Ok(size) => {
                task.total.store(size, Ordering::Relaxed);
                task.downloaded.store(size, Ordering::Relaxed);
                let _ = fs::remove_dir_all(&folder);
                task.state.store(state::DONE, Ordering::SeqCst);
            }
            Err(e) => task.set_error(e),
        }
    }
}

impl Engine {
    /// The pieces several at a time, each kept in its own file as `keep_part` does; says whether all are there. With
    /// `estimate` the size shown is worked out from the pieces so far, for a playlist whose size nobody gives.
    fn keep_all(&self, task: &Task, pieces: &[Piece], estimate: bool) -> bool {
        let count = pieces.len();
        let workers = self.connection_count().min(count).max(1);
        if let Ok(mut slot) = task.segments.lock() {
            *slot = (0..workers).map(|_| Part::idle()).collect();
        }
        let keys = Mutex::new(HashMap::new());
        let next = AtomicUsize::new(0);
        let (kept, finished) = (AtomicU64::new(0), AtomicU64::new(0));
        thread::scope(|scope| {
            let worker = || loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(piece) = pieces.get(i) else { return true };
                match keep_part(task, piece, &keys) {
                    Ok(size) if estimate => {
                        let kept = kept.fetch_add(size, Ordering::Relaxed) + size;
                        let finished = finished.fetch_add(1, Ordering::Relaxed) + 1;
                        task.total.store(kept / finished * count as u64, Ordering::Relaxed);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        if !task.paused_or_removed() {
                            task.set_error(e);
                        }
                        return false;
                    }
                }
            };
            let handles: Vec<_> = (0..workers).map(|_| scope.spawn(worker)).collect();
            handles.into_iter().all(|h| h.join().unwrap_or(false))
        }) && !task.paused_or_removed()
    }

    /// A video page Lanes reads itself (src/extract): the variant picked in the video button, read again from the page
    /// since stream addresses expire. Each stream comes in pieces (`extract::PIECE` at most, which googlevideo needs to
    /// serve fast), kept as numbered files in a folder beside the final file so a resume skips the ones already there;
    /// each stream is joined, and a picture and a sound are put together by ffmpeg.
    fn run_media(&self, task: &Task, id: &str) {
        let read = extract::read(&task.url, &task.headers).and_then(|video| video.variants.into_iter().find(|v| v.id == id).ok_or_else(|| "Trang không còn bản này, hãy chọn lại trong nút Tải video".to_owned()));
        let variant = match read {
            Ok(variant) => variant,
            Err(e) => return task.set_error(e),
        };
        let folder = part_of(&task.final_path);
        if let Err(e) = fs::create_dir_all(&folder) {
            return task.set_error(format!("Không tạo được thư mục tạm: {e}"));
        }
        let streams = variant.streams();
        let (mut pieces, mut ends) = (Vec::new(), Vec::new());
        for (k, stream) in streams.iter().enumerate() {
            // The pieces of each stream are named by its place and size, so a resume never mixes two versions of it.
            let dir = folder.join(format!("{k}-{}", stream.size.unwrap_or(0)));
            if let Err(e) = fs::create_dir_all(&dir) {
                return task.set_error(format!("Không tạo được thư mục tạm: {e}"));
            }
            let mut these = if stream.hls {
                match hls::load(&stream.url, &stream.headers) {
                    Ok(media) => playlist_pieces(media, &dir, &stream.headers, &stream.url),
                    Err(e) => return task.set_error(e),
                }
            } else {
                let size = stream.size.or_else(|| probe(&stream.url, &stream.headers).ok().and_then(|p| p.size));
                range_pieces(&stream.url, size, &dir, &stream.headers)
            };
            pieces.append(&mut these);
            ends.push(pieces.len());
        }
        let sized = streams.iter().all(|s| !s.hls && s.size.is_some());
        if sized {
            task.total.store(variant.size().unwrap_or(0), Ordering::Relaxed);
        }
        task.downloaded.store(0, Ordering::Relaxed);
        if !self.keep_all(task, &pieces, !sized) {
            return;
        }
        // Each stream into one file, then the picture and the sound into the final one.
        let mut joined = Vec::new();
        let mut start = 0;
        for (k, (stream, end)) in streams.iter().zip(&ends).enumerate() {
            let parts: Vec<PathBuf> = pieces[start..*end].iter().map(|p| p.path.clone()).collect();
            let path = folder.join(format!("stream{k}.{}", stream.ext));
            if let Err(e) = join_parts(&parts, &folder, &path) {
                return task.set_error(format!("Không nối được video: {e}"));
            }
            joined.push(path);
            start = *end;
        }
        let made = match joined.as_slice() {
            [one] => settle(one, &folder, &task.final_path),
            [picture, sound] => mux(picture, sound, &folder, &task.final_path),
            _ => Err("Bản này không có luồng nào".into()),
        };
        match made.and_then(|()| fs::metadata(&task.final_path).map(|m| m.len()).map_err(|e| e.to_string())) {
            Ok(size) => {
                task.total.store(size, Ordering::Relaxed);
                task.downloaded.store(size, Ordering::Relaxed);
                let _ = fs::remove_dir_all(&folder);
                task.state.store(state::DONE, Ordering::SeqCst);
            }
            Err(e) => task.set_error(e),
        }
    }
}

/// One piece to keep in its own file: a segment of a playlist, or a byte range of a stream, and the headers its server
/// wants. A cookie among them goes only to the host of `origin`, the address they were chosen for (hls::headers_for).
struct Piece<'a> {
    segment: hls::Segment,
    path: PathBuf,
    headers: &'a [(String, String)],
    origin: &'a str,
}

/// The pieces of a playlist first fetched at `origin` in order, its init part first, as numbered files in `folder`.
fn playlist_pieces<'a>(media: hls::Media, folder: &Path, headers: &'a [(String, String)], origin: &'a str) -> Vec<Piece<'a>> {
    let init = media.init.map(|segment| Piece { segment, path: folder.join("init"), headers, origin });
    let segments = media.segments.into_iter().enumerate().map(|(i, segment)| Piece { segment, path: folder.join(format!("{i:06}")), headers, origin });
    init.into_iter().chain(segments).collect()
}

/// A stream of `size` bytes as ranges of at most `extract::PIECE`, or as one piece when its size is not known. Its
/// headers are the ones its reader chose for it, so they go to its own host as they are.
fn range_pieces<'a>(url: &'a str, size: Option<u64>, folder: &Path, headers: &'a [(String, String)]) -> Vec<Piece<'a>> {
    let piece = |i: usize, range| Piece { segment: hls::Segment { url: url.to_owned(), range, key: None, sequence: 0 }, path: folder.join(format!("{i:06}")), headers, origin: url };
    match size {
        Some(size) if size > 0 => (0..size).step_by(extract::PIECE as usize).enumerate().map(|(i, from)| piece(i, Some((from, extract::PIECE.min(size - from))))).collect(),
        _ => vec![piece(0, None)],
    }
}

/// A picture and a sound into one MP4 by the ffmpeg the app carries, without encoding them again.
fn mux(picture: &Path, sound: &Path, folder: &Path, final_path: &Path) -> Result<(), String> {
    to_mp4(&[picture, sound], &["-map", "0:v:0", "-map", "1:a:0"], folder, final_path).map_err(|e| format!("Không ghép được hình và tiếng: {e}"))
}

/// A whole joined file into `final_path`. MPEG-TS, what most HLS segments are, becomes the MP4 its name asks for, its
/// streams copied as they are, since few players open a bare .ts. A stream ffmpeg cannot rewrap is kept as it is, which
/// VLC and the like still play, rather than lose the download. Anything else is moved as it is.
fn settle(joined: &Path, folder: &Path, final_path: &Path) -> Result<(), String> {
    let mp4 = final_path.extension().is_some_and(|e| e.eq_ignore_ascii_case("mp4"));
    // Subtitles and timed metadata of a TS have no place in an MP4; the picture and the sound are what is kept.
    if !(mp4 && is_ts(joined)) || to_mp4(&[joined], &["-sn", "-dn"], folder, final_path).is_err() {
        return fs::rename(joined, final_path).map_err(|e| format!("Không đổi tên được tệp: {e}"));
    }
    Ok(())
}

/// MPEG-TS: packets of 188 bytes, each starting with 0x47.
fn is_ts(path: &Path) -> bool {
    let mut head = [0u8; 189];
    fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok() && head[0] == 0x47 && head[188] == 0x47
}

/// `inputs` into one MP4 at `final_path` by the ffmpeg the app carries, with `args` before the streams are copied.
fn to_mp4(inputs: &[&Path], args: &[&str], folder: &Path, final_path: &Path) -> Result<(), String> {
    let ffmpeg = ytdlp::ffmpeg().ok_or("Thiếu ffmpeg")?;
    let out = folder.join("joined.mp4");
    let mut command = crate::cmd(&ffmpeg.to_string_lossy());
    command.args(["-nostdin", "-loglevel", "error", "-y"]);
    for input in inputs {
        command.arg("-i").arg(input);
    }
    let done = command
        .args(args)
        .args(["-c", "copy", "-movflags", "+faststart"])
        .arg(&out)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("Không chạy được ffmpeg: {e}"))?;
    if !done.status.success() {
        let _ = fs::remove_file(&out);
        return Err(String::from_utf8_lossy(&done.stderr).lines().last().unwrap_or_default().to_owned());
    }
    fs::rename(&out, final_path).map_err(|e| format!("Không đổi tên được tệp: {e}"))
}

/// A video page through yt-dlp (src/ytdlp.rs), its pieces kept in the folder beside the file so a resume carries on.
/// Its size stays the estimate the extension gave until the file is whole.
fn run_page(task: &Task, format: &str) {
    let folder = part_of(&task.final_path);
    if let Err(e) = fs::create_dir_all(&folder) {
        return task.set_error(format!("Không tạo được thư mục tạm: {e}"));
    }
    if let Ok(mut slot) = task.segments.lock() {
        *slot = vec![Part::idle()];
    }
    let progress = |done| task.downloaded.store(done, Ordering::Relaxed);
    match ytdlp::download(&task.url, &task.headers, format,&task.final_path, &folder, task.via_ytdlp.as_deref(), progress, || task.paused_or_removed()) {
        Ok(size) => {
            task.total.store(size, Ordering::Relaxed);
            task.downloaded.store(size, Ordering::Relaxed);
            let _ = fs::remove_dir_all(&folder);
            task.state.store(state::DONE, Ordering::SeqCst);
        }
        Err(e) => {
            if !task.paused_or_removed() {
                task.set_error(e);
            }
        }
    }
}

/// Deletes what a download left beside its file: the part file or, for HLS, the folder of segments, and the sidecar.
pub fn discard(final_path: &Path) {
    let part = part_of(final_path);
    let _ = fs::remove_file(&part);
    let _ = fs::remove_dir_all(&part);
    let _ = fs::remove_file(meta_of(final_path));
}

/// One HLS segment, or the init part, into its own file: kept as it is when a resume finds it there, otherwise
/// fetched with retries and decrypted when the playlist gives it a key. Gives the file's size.
fn keep_part(task: &Task, piece: &Piece, keys: &Mutex<HashMap<String, Vec<u8>>>) -> Result<u64, String> {
    let (segment, path) = (&piece.segment, piece.path.as_path());
    if let Ok(meta) = fs::metadata(path) {
        task.downloaded.fetch_add(meta.len(), Ordering::Relaxed);
        return Ok(meta.len());
    }
    let mut attempt = 0;
    loop {
        if task.paused_or_removed() {
            return Err("Đã dừng".into());
        }
        let fetched = fetch_counted(task, piece).and_then(|body| match &segment.key {
            Some(key) => hls::decrypt(&key_bytes(task, &key.url, keys)?, key.iv.unwrap_or_else(|| hls::sequence_iv(segment.sequence)), body),
            None => Ok(body),
        });
        match fetched {
            Ok(body) => {
                let tmp = path.with_extension("tmp");
                fs::write(&tmp, &body).and_then(|()| fs::rename(&tmp, path)).map_err(|e| format!("Không ghi được tệp: {e}"))?;
                return Ok(body.len() as u64);
            }
            Err(e) => {
                attempt += 1;
                if attempt > RETRIES {
                    return Err(e);
                }
                for _ in 0..(attempt.min(6) * 5) {
                    if task.paused_or_removed() {
                        return Err("Đã dừng".into());
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
}

/// The body of one HLS part, counted into the progress as it arrives; what was counted is taken back on a failure.
fn fetch_counted(task: &Task, piece: &Piece) -> Result<Vec<u8>, String> {
    let segment = &piece.segment;
    let mut reader = hls::open(&segment.url, segment.range, piece.origin, piece.headers, task.via.as_deref())?;
    let mut body = Vec::new();
    let mut buf = vec![0u8; CHUNK];
    let result = loop {
        if task.paused_or_removed() {
            break Err("Đã dừng".to_owned());
        }
        match reader.read(&mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => {
                body.extend_from_slice(&buf[..n]);
                task.downloaded.fetch_add(n as u64, Ordering::Relaxed);
                throttle(task, n);
            }
            Err(e) => break Err(format!("Kết nối bị gián đoạn: {e}")),
        }
    };
    if let Err(e) = result {
        task.downloaded.fetch_sub(body.len() as u64, Ordering::Relaxed);
        return Err(e);
    }
    Ok(body)
}

/// The AES-128 key at `url`, fetched once per download and shared by every segment it locks.
fn key_bytes(task: &Task, url: &str, keys: &Mutex<HashMap<String, Vec<u8>>>) -> Result<Vec<u8>, String> {
    let mut keys = keys.lock().map_err(|_| "Khóa giải mã bị lỗi".to_owned())?;
    if let Some(key) = keys.get(url) {
        return Ok(key.clone());
    }
    let mut key = Vec::new();
    hls::open(url, None, &task.url, &task.headers, task.via.as_deref())?.take(64).read_to_end(&mut key).map_err(|e| format!("Không tải được khóa giải mã: {e}"))?;
    keys.insert(url.to_owned(), key.clone());
    Ok(key)
}

/// The parts in order into one file inside `folder`, then moved to `final_path`. Gives the file's size.
fn join_parts(parts: &[PathBuf], folder: &Path, final_path: &Path) -> std::io::Result<u64> {
    let joined = folder.join("joined");
    let mut out = fs::File::create(&joined)?;
    let mut size = 0;
    for part in parts {
        size += std::io::copy(&mut fs::File::open(part)?, &mut out)?;
    }
    out.sync_all()?;
    drop(out);
    fs::rename(&joined, final_path)?;
    Ok(size)
}

/// Opens the part file and grows it to the full size so every connection can write its own stretch at once.
fn open_part(part: &Path, size: Option<u64>) -> std::io::Result<fs::File> {
    let file = fs::OpenOptions::new().create(true).read(true).write(true).open(part)?;
    if let Some(size) = size {
        file.set_len(size)?;
    }
    Ok(file)
}

/// Puts the whole file in place under its real name, replacing anything already there.
fn finish(file: &fs::File, part: &Path, final_path: &Path, size: Option<u64>) -> Result<(), String> {
    file.sync_all().map_err(|e| e.to_string())?;
    if let Some(size) = size {
        let on_disk = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
        if on_disk != size {
            return Err(format!("Tệp tải về dài {on_disk} byte, không phải {size}"));
        }
    }
    fs::rename(part, final_path).map_err(|e| format!("Không đổi tên được tệp: {e}"))
}

/// The speed limit in bytes a second, 0 for none, and the bucket of bytes it fills at that rate, which every read of
/// every download takes from (`throttle`).
/// ponytail: one limit over all downloads; one per download when someone needs it.
static LIMIT: AtomicU64 = AtomicU64::new(0);
static BUCKET: Mutex<Option<(f64, Instant)>> = Mutex::new(None);

/// Holds a connection that has just read `n` bytes until the speed limit allows them. The bucket holds a second's worth
/// at most and goes into debt for a read it cannot cover, which its reader then waits off; the wait is cut short by a
/// pause, so a slow limit never keeps a paused download's connections alive.
fn throttle(task: &Task, n: usize) {
    let rate = LIMIT.load(Ordering::Relaxed);
    if rate == 0 {
        return;
    }
    let Some(wait) = BUCKET.lock().ok().map(|mut bucket| take(&mut bucket, rate, n, Instant::now())) else { return };
    let until = Instant::now() + wait;
    while Instant::now() < until && !task.paused_or_removed() {
        thread::sleep((until - Instant::now()).min(Duration::from_millis(100)));
    }
}

/// Takes `n` bytes out of the bucket at `now`, refilled at `rate` since it was last taken from, and gives how long the
/// reader waits for the debt it leaves.
fn take(bucket: &mut Option<(f64, Instant)>, rate: u64, n: usize, now: Instant) -> Duration {
    let (tokens, last) = bucket.get_or_insert((rate as f64, now));
    *tokens = (*tokens + now.saturating_duration_since(*last).as_secs_f64() * rate as f64).min(rate as f64) - n as f64;
    *last = now;
    Duration::from_secs_f64((-*tokens).max(0.0) / rate as f64)
}

/// One connection: fetches its part from where it left off, writing each chunk straight to its offset, up to the end of
/// the part as it stands, which comes down when another connection takes over its second half. It returns whether the
/// part is complete; a pause or a removal ends it early without an error. With mirrors (net-merge beta) it starts on
/// the mirror `source_start` and moves to the next mirror each time one fails, so a dead mirror never strands a part
/// while others are alive.
fn download_segment(task: &Task, file: &fs::File, part: &Part, source_start: usize) -> bool {
    let sources = task.sources.lock().map(|s| s.clone()).unwrap_or_default();
    let sources = if sources.is_empty() { vec![task.url.clone()] } else { sources };
    let mut si = source_start % sources.len();
    let mut attempt = 0;
    loop {
        if task.paused_or_removed() {
            return false;
        }
        if part.finished() {
            return true;
        }
        let from = part.start + part.done.load(Ordering::Relaxed);
        let end = part.end.load(Ordering::SeqCst);
        let bounded = end != OPEN_END;
        let range = if bounded { format!("bytes={from}-{end}") } else { format!("bytes={from}-") };
        match fetch_range(&sources[si], &range, bounded, &task.headers, task.via.as_deref()) {
            Ok(mut reader) => {
                let mut buf = vec![0u8; CHUNK];
                let mut offset = from;
                loop {
                    if task.paused_or_removed() {
                        return false;
                    }
                    // Another connection may have taken over the end of the part: what lies past the new end is its.
                    let end = part.end.load(Ordering::SeqCst);
                    if offset > end {
                        break;
                    }
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let n = (n as u64).min((end - offset).saturating_add(1)) as usize;
                            if let Err(e) = write_at(file, offset, &buf[..n]) {
                                task.set_error(format!("Không ghi được tệp: {e}"));
                                return false;
                            }
                            offset += n as u64;
                            part.done.fetch_add(n as u64, Ordering::Relaxed);
                            task.downloaded.fetch_add(n as u64, Ordering::Relaxed);
                            throttle(task, n);
                        }
                        Err(_) => break,
                    }
                }
                if !bounded || part.finished() {
                    return true;
                }
                // Short read: the connection dropped before the range was done, so it is retried from the new point.
            }
            Err(e) => {
                attempt += 1;
                // Move to the next mirror for the retry; with one source this stays put.
                si = (si + 1) % sources.len();
                if attempt > RETRIES {
                    task.set_error(e);
                    return false;
                }
            }
        }
        attempt += 1;
        if attempt > RETRIES {
            task.set_error("Kết nối bị gián đoạn nhiều lần".into());
            return false;
        }
        for _ in 0..(attempt.min(6) * 5) {
            if task.paused_or_removed() {
                return false;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

/// An agent of its own for each request; with net-merge its connections go through `proxy`, which gives each to a link.
pub(crate) fn agent(proxy: Option<&str>) -> ureq::Agent {
    let mut builder = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(SECONDS)).timeout_read(Duration::from_secs(SECONDS)).redirects(10);
    if let Some(proxy) = proxy.and_then(|p| ureq::Proxy::new(p).ok()) {
        builder = builder.proxy(proxy);
    }
    builder.build()
}

/// Opens a reader over one byte range. `identity` is asked for so a range maps to the same bytes on disk, and a
/// server that ignores the range (a 200 for a range request) is refused rather than written at the wrong place.
fn fetch_range(url: &str, range: &str, expect_partial: bool, headers: &[(String, String)], proxy: Option<&str>) -> Result<Box<dyn Read + Send>, String> {
    let mut request = agent(proxy).get(url).set("Range", range).set("Accept-Encoding", "identity");
    for (name, value) in headers {
        request = request.set(name, value);
    }
    let response = request.call().map_err(|e| format!("Không kết nối được: {e}"))?;
    let status = response.status();
    if expect_partial && status == 200 {
        return Err("Máy chủ không hỗ trợ tải theo đoạn".into());
    }
    if !(200..300).contains(&status) {
        return Err(format!("Máy chủ trả mã {status}"));
    }
    Ok(Box::new(response.into_reader()))
}

/// Asks for the first byte to learn the size, whether ranges work, the name and the file's tag, without pulling
/// the file. A 206 answer proves ranges work and carries the total in Content-Range.
pub fn probe(url: &str, headers: &[(String, String)]) -> Result<Probe, String> {
    let mut request = agent(None).get(url).set("Range", "bytes=0-0").set("Accept-Encoding", "identity");
    for (name, value) in headers {
        request = request.set(name, value);
    }
    let response = request.call().map_err(|e| format!("Không kết nối được: {e}"))?;
    let status = response.status();
    if !(200..300).contains(&status) {
        return Err(format!("Máy chủ trả mã {status}"));
    }
    let etag = response.header("etag").map(str::to_owned);
    let last_modified = response.header("last-modified").map(str::to_owned);
    let disposition = response.header("content-disposition").map(str::to_owned);
    let filename = filename_from(url, disposition.as_deref());
    let (size, resumable) = if status == 206 {
        (response.header("content-range").and_then(parse_content_range_total), true)
    } else {
        let accepts = response.header("accept-ranges").is_some_and(|v| v.to_ascii_lowercase().contains("bytes"));
        (response.header("content-length").and_then(|v| v.parse().ok()), accepts)
    };
    Ok(Probe { size, resumable, filename, etag, last_modified })
}

/// For net-merge: the mirrors from `sources` (the primary first, always kept) that a probe shows serve the same file,
/// by the same size with ranges allowed. A mirror that disagrees or cannot be reached is dropped, so a merge only
/// ever pulls bytes that line up across its sources.
fn narrow_sources(sources: &[String], size: u64, headers: &[(String, String)]) -> Vec<String> {
    let mut active: Vec<String> = sources.first().cloned().into_iter().collect();
    for url in sources.iter().skip(1) {
        if probe(url, headers).is_ok_and(|p| p.resumable && p.size == Some(size)) {
            active.push(url.clone());
        }
    }
    active
}

/// The total length from a `Content-Range: bytes 0-0/12345` header.
fn parse_content_range_total(value: &str) -> Option<u64> {
    value.rsplit('/').next().filter(|t| *t != "*").and_then(|t| t.trim().parse().ok())
}

fn load_meta(final_path: &Path) -> Option<Meta> {
    let text = fs::read_to_string(meta_of(final_path)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Writes the sidecar for a resume: every part as it stands, with the ones split off by connections that finished.
fn save_meta(task: &Task) {
    let parts: Vec<Segment> = task.segments.lock().map(|p| p.iter().map(|p| p.segment()).collect()).unwrap_or_default();
    let Ok(mut meta) = task.meta.lock() else { return };
    meta.segments = parts;
    if let Ok(text) = serde_json::to_string(&*meta) {
        let target = meta_of(&task.final_path);
        let tmp = target.with_extension("meta.tmp");
        if fs::write(&tmp, &text).is_ok() {
            let _ = fs::rename(&tmp, &target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_cover_the_file_without_gaps_or_overlap() {
        let segs = plan_segments(1000, 4, 1);
        assert_eq!(segs.len(), 4);
        assert_eq!(segs[0].start, 0);
        assert_eq!(segs.last().unwrap().end, Some(999));
        for pair in segs.windows(2) {
            assert_eq!(pair[0].end.unwrap() + 1, pair[1].start);
        }
        let total: u64 = segs.iter().map(|s| s.end.unwrap() + 1 - s.start).sum();
        assert_eq!(total, 1000);
    }

    #[test]
    fn small_files_and_streams_take_one_connection() {
        assert_eq!(plan_segments(500, 8, 1 << 20).len(), 1, "below the split size");
        // A file exactly the minimum piece is still one connection, since it holds only one piece.
        assert_eq!(plan_segments(1 << 20, 8, 1 << 20)[0].end, Some((1 << 20) - 1));
        // Each connection gets at least the minimum piece: a 1 MiB file with a 512 KiB piece is two segments.
        assert_eq!(plan_segments(1 << 20, 8, 1 << 19).len(), 2);
        // A file of unknown length is one stream with no end.
        let stream = plan_segments(0, 8, 1);
        assert_eq!(stream.len(), 1);
        assert_eq!(stream[0].end, None);
    }

    /// The parts of a table that have an end, in order: they must cover the file once, without a gap or an overlap.
    fn covered(parts: &Mutex<Vec<Arc<Part>>>) -> Vec<(u64, u64)> {
        let mut ranges: Vec<(u64, u64)> = parts.lock().unwrap().iter().filter_map(|p| p.segment().end.map(|end| (p.start, end))).collect();
        ranges.sort();
        for pair in ranges.windows(2) {
            assert_eq!(pair[0].1 + 1, pair[1].0, "{ranges:?}");
        }
        ranges
    }

    #[test]
    fn a_finished_connection_takes_the_second_half_of_the_largest_part_left() {
        let mib = MIN_PIECE;
        let parts = Mutex::new(vec![
            Part::new(&Segment { start: 0, end: Some(10 * mib - 1), done: 2 * mib }),
            Part::new(&Segment { start: 10 * mib, end: Some(12 * mib - 1), done: 2 * mib }),
            Part::new(&Segment { start: 0, end: None, done: 5 }),
        ]);
        assert!(parts.lock().unwrap()[1].finished());
        let (part, index) = steal(&parts).expect("8 MiB left is worth a connection of its own");
        assert_eq!(index, 3);
        assert_eq!(part.segment(), Segment { start: 6 * mib, end: Some(10 * mib - 1), done: 0 });
        assert_eq!(parts.lock().unwrap()[0].segment().end, Some(6 * mib - 1), "the first now stops where it starts");
        assert_eq!(parts.lock().unwrap()[2].segment().end, None, "a stream of unknown length is never split");
        // 4 MiB left in two parts: one of them is split again, and the file is still covered once.
        let (again, _) = steal(&parts).unwrap();
        assert_eq!(again.left(), Some(2 * mib));
        let ranges = covered(&parts);
        assert_eq!((ranges[0].0, ranges.last().unwrap().1), (0, 12 * mib - 1));
        // With less than two MIN_PIECEs left anywhere, no connection is added.
        for p in parts.lock().unwrap().iter().filter(|p| p.left().is_some()) {
            let len = p.end.load(Ordering::SeqCst) + 1 - p.start;
            p.done.store(len.saturating_sub(mib), Ordering::Relaxed);
        }
        assert!(steal(&parts).is_none());
    }

    /// A server of one file that honours ranges, slowly for a range that starts at 0, so that the connection with the
    /// first part is the slow one.
    fn serve_ranges(body: Arc<Vec<u8>>) -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/file.bin", listener.local_addr().unwrap());
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let body = body.clone();
                thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    let mut range = (0, body.len() - 1);
                    while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                        if let Some(value) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                            let (from, to) = value.trim().split_once('-').unwrap();
                            range = (from.parse().unwrap(), to.parse().unwrap_or(body.len() - 1).min(body.len() - 1));
                        }
                        line.clear();
                    }
                    let (from, to) = range;
                    let mut stream = stream;
                    let head = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {from}-{to}/{}\r\nConnection: close\r\n\r\n",
                        to + 1 - from,
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    for chunk in body[from..=to].chunks(64 * 1024) {
                        if from == 0 {
                            thread::sleep(Duration::from_millis(20));
                        }
                        if stream.write_all(chunk).is_err() {
                            return;
                        }
                    }
                });
            }
        });
        base
    }

    /// The fast connection does not wait for the slow one: it takes over the second half of the slow part, and the file
    /// is still whole, byte for byte.
    #[test]
    fn a_fast_connection_takes_over_from_a_slow_one_and_the_file_is_whole() {
        let body: Arc<Vec<u8>> = Arc::new((0..8 * MIN_PIECE as usize).map(|i| (i * 7 % 251) as u8).collect());
        let url = serve_ranges(body.clone());
        let dir = std::env::temp_dir().join(format!("lanes-steal-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("file.bin");
        let engine = Arc::new(Engine::new());
        engine.set_connections(2);
        engine.start(1, vec![url], path.clone(), Vec::new(), Kind::File);
        loop {
            let snap = engine.snapshots().into_iter().next().expect("the task is listed");
            match snap.state {
                state::DONE => break,
                state::ERROR => panic!("download failed: {}", snap.error),
                _ => thread::sleep(Duration::from_millis(20)),
            }
        }
        assert!(fs::read(&path).unwrap() == *body, "the file is whole");
        let task = engine.find(1).unwrap();
        let parts = task.segments.lock().unwrap().len();
        assert!(parts >= 3, "the fast connection took over part of the slow one: {parts} parts");
        assert_eq!(covered(&task.segments).len(), parts);
        assert!(!meta_of(&path).exists(), "the sidecar is gone with the download done");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_filename_comes_from_the_disposition_then_the_url() {
        assert_eq!(filename_from("https://x.com/a/file.zip?k=1", None), "file.zip");
        assert_eq!(filename_from("https://x.com/", None), "download");
        assert_eq!(filename_from("https://x.com/%E2%9C%93.pdf", None), "✓.pdf");
        assert_eq!(filename_from("https://x.com/a", Some("attachment; filename=\"real name.bin\"")), "real name.bin");
        assert_eq!(filename_from("https://x.com/a", Some("attachment; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf")), "résumé.pdf");
    }

    #[test]
    fn filenames_lose_path_and_illegal_characters() {
        assert_eq!(sanitize("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(sanitize("a:b*c?.txt"), "a_b_c_.txt");
        assert_eq!(sanitize("  spaced.name.  "), "spaced.name");
        assert_eq!(sanitize(".bashrc"), ".bashrc", "a leading dot is kept");
        assert_eq!(filename_from("https://x.com/a/..", None), "download");
    }

    #[test]
    fn content_range_gives_the_total() {
        assert_eq!(parse_content_range_total("bytes 0-0/12345"), Some(12345));
        assert_eq!(parse_content_range_total("bytes 0-0/*"), None);
        assert_eq!(parse_content_range_total("nonsense"), None);
    }

    #[test]
    fn meta_round_trips_and_sums_progress() {
        let meta = Meta {
            url: "https://x.com/f".into(),
            size: Some(300),
            resumable: true,
            etag: Some("\"abc\"".into()),
            last_modified: None,
            segments: vec![
                Segment { start: 0, end: Some(149), done: 100 },
                Segment { start: 150, end: Some(299), done: 50 },
            ],
        };
        let text = serde_json::to_string(&meta).unwrap();
        let back: Meta = serde_json::from_str(&text).unwrap();
        assert_eq!(back, meta);
        assert_eq!(back.done(), 150);
    }

    #[test]
    fn files_fall_into_the_categories_of_idm() {
        assert_eq!(category("a.ZIP"), Some("Compressed"));
        assert_eq!(category("movie.part.r01"), Some("Compressed"));
        assert_eq!(category("backup.tar.gz"), Some("Compressed"));
        assert_eq!(category("report.pdf"), Some("Documents"));
        assert_eq!(category("song.m4a"), Some("Music"));
        assert_eq!(category("Lanes.dmg"), Some("Programs"));
        assert_eq!(category("Trang 720p.ts"), Some("Video"));
        assert_eq!(category("notes.txt"), None, "general");
        assert_eq!(category("README"), None);
        assert_eq!(category("x.rss"), None, "r0* and r1* do not catch every r");
    }

    #[test]
    fn a_taken_name_gets_a_number() {
        let dir = std::env::temp_dir().join(format!("lanes-unique-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique_path(&dir, "a.zip"), dir.join("a.zip"));
        fs::write(dir.join("a.zip"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a.zip"), dir.join("a (2).zip"));
        fs::write(part_of(&dir.join("a (2).zip")), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a.zip"), dir.join("a (3).zip"));
        fs::remove_dir_all(&dir).unwrap();
    }

    /// The positioned writes of several connections build the right file, each writing only its own range.
    #[test]
    fn positioned_writes_from_segments_rebuild_the_file() {
        let dir = std::env::temp_dir().join(format!("lanes-writeat-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.bin");
        let data: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
        let file = open_part(&path, Some(data.len() as u64)).unwrap();
        for seg in plan_segments(data.len() as u64, 4, 1) {
            let (start, end) = (seg.start as usize, seg.end.unwrap() as usize);
            write_at(&file, seg.start, &data[start..=end]).unwrap();
        }
        file.sync_all().unwrap();
        assert_eq!(fs::read(&path).unwrap(), data);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A local server answering GET by path from `files`, so the HLS path runs without the network.
    fn serve(files: Vec<(&'static str, Vec<u8>)>) -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let files = Arc::new(files);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let files = files.clone();
                thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut request = String::new();
                    let _ = reader.read_line(&mut request);
                    let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
                    let mut line = String::new();
                    while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                        line.clear();
                    }
                    let (status, body) = match files.iter().find(|(p, _)| *p == path) {
                        Some((_, body)) => ("200 OK", body.clone()),
                        None => ("404 Not Found", Vec::new()),
                    };
                    let mut stream = stream;
                    let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                    let _ = stream.write_all(&body);
                });
            }
        });
        base
    }

    /// An HLS video through its master: plain and AES-128 segments, the second key run taking its IV from the
    /// sequence number, fetched in parallel and joined in order into one file, with the segment folder gone after.
    #[test]
    fn the_speed_limit_lets_a_second_through_then_makes_readers_wait() {
        let (mut bucket, start) = (None, Instant::now());
        let ms = |d: Duration| d.as_millis();
        assert_eq!(ms(take(&mut bucket, 1000, 1000, start)), 0, "a full second's worth at once");
        assert_eq!(ms(take(&mut bucket, 1000, 500, start)), 500, "then half a second for half again");
        assert_eq!(ms(take(&mut bucket, 1000, 500, start + Duration::from_millis(500))), 500, "the debt is paid off over time");
        assert_eq!(ms(take(&mut bucket, 1000, 0, start + Duration::from_secs(60))), 0);
        assert_eq!(bucket.unwrap().0, 1000.0, "an idle bucket holds no more than a second");
    }

    #[test]
    fn an_hls_video_is_fetched_decrypted_and_joined() {
        use cbc::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
        let key = [9u8; 16];
        let lock = |plain: &[u8], iv: [u8; 16]| {
            let mut buf = plain.to_vec();
            buf.resize(plain.len() + 16, 0);
            let len = cbc::Encryptor::<aes::Aes128>::new(&key.into(), &iv.into()).encrypt_padded_mut::<Pkcs7>(&mut buf, plain.len()).unwrap().len();
            buf.truncate(len);
            buf
        };
        let parts: Vec<Vec<u8>> = (0..4u8).map(|i| vec![b'a' + i; 1000 + i as usize * 333]).collect();
        let explicit = [3u8; 16];
        let media = "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:5\n#EXTINF:2,\ns0.ts\n\
            #EXT-X-KEY:METHOD=AES-128,URI=\"/key\",IV=0x03030303030303030303030303030303\n#EXTINF:2,\ns1.ts\n\
            #EXT-X-KEY:METHOD=AES-128,URI=\"/key\"\n#EXTINF:2,\ns2.ts\n#EXTINF:2,\ns3.ts\n#EXT-X-ENDLIST\n";
        let base = serve(vec![
            ("/master.m3u8", b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,RESOLUTION=2x2\nv/media.m3u8\n".to_vec()),
            ("/v/media.m3u8", media.as_bytes().to_vec()),
            ("/key", key.to_vec()),
            ("/v/s0.ts", parts[0].clone()),
            ("/v/s1.ts", lock(&parts[1], explicit)),
            ("/v/s2.ts", lock(&parts[2], hls::sequence_iv(7))),
            ("/v/s3.ts", lock(&parts[3], hls::sequence_iv(8))),
        ]);

        let dir = std::env::temp_dir().join(format!("lanes-hls-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("video.ts");
        let engine = Arc::new(Engine::new());
        engine.set_connections(3);
        engine.start(1, vec![format!("{base}/master.m3u8")], path.clone(), Vec::new(), Kind::Hls { audio: String::new() });
        let snap = loop {
            let snap = engine.snapshots().into_iter().next().expect("the task is listed");
            match snap.state {
                state::DONE => break snap,
                state::ERROR => panic!("hls failed: {}", snap.error),
                _ => thread::sleep(Duration::from_millis(50)),
            }
        };
        assert_eq!(fs::read(&path).unwrap(), parts.concat());
        assert_eq!(snap.size, Some(parts.concat().len() as u64));
        assert!(!part_of(&path).exists(), "the segment folder is removed");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// HLS over the network: the smallest resolution of a public MPEG-TS stream and of a fragmented-MP4 one cut into
    /// byte ranges whose sound is a playlist of its own, each made into an MP4 with its sound by ffmpeg. Run with
    /// `cargo test -- --ignored hls_from`.
    #[test]
    #[ignore = "needs the network"]
    fn hls_from_the_network_makes_whole_files() {
        let streams = [
            "https://test-streams.mux.dev/x36xhzz/x36xhzz.m3u8",
            "https://devstreaming-cdn.apple.com/videos/streaming/examples/img_bipbop_adv_example_fmp4/master.m3u8",
        ];
        let dir = std::env::temp_dir().join(format!("lanes-hls-net-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let engine = Arc::new(Engine::new());
        engine.set_connections(8);
        for (id, master) in streams.into_iter().enumerate() {
            let choices = hls::choices(master, &[]).unwrap();
            assert_eq!(choices.ext, "mp4");
            let smallest = choices.variants.last().unwrap().clone();
            let path = dir.join(format!("{id}.mp4"));
            engine.start(id as i64, vec![smallest.url], path.clone(), Vec::new(), Kind::Hls { audio: smallest.audio.unwrap_or_default() });
            loop {
                let snap = engine.snapshots().into_iter().find(|s| s.id == id as i64).unwrap();
                match snap.state {
                    state::DONE => break,
                    state::ERROR => panic!("{master}: {}", snap.error),
                    _ => thread::sleep(Duration::from_millis(200)),
                }
            }
            let bytes = fs::read(&path).unwrap();
            eprintln!("{master}: {} bytes", bytes.len());
            assert_eq!(&bytes[4..8], b"ftyp", "an MP4 starts with its ftyp box, the MPEG-TS one rewrapped");
            let probe = std::process::Command::new(ytdlp::ffmpeg().unwrap()).arg("-i").arg(&path).output().unwrap();
            let streams = String::from_utf8_lossy(&probe.stderr);
            assert!(streams.contains("Video:") && streams.contains("Audio:"), "{master}: a picture and a sound\n{streams}");
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    /// net-merge over the links of this computer: a release of Lanes with eight connections spread over the links that
    /// can reach GitHub, whole and matching its hash, printing what each link carried. `LANES_NET_URL` downloads another
    /// file instead and checks only that it is whole. Run with `cargo test -- --ignored --nocapture net_merge_over`.
    #[test]
    #[ignore = "needs the network"]
    fn net_merge_over_the_links_of_this_computer() {
        use sha2::{Digest, Sha256};
        let other = std::env::var("LANES_NET_URL").ok();
        let url = other.clone().unwrap_or_else(|| "https://github.com/eliasbui/lanes/releases/download/v0.1.0/Lanes-0.1.0-macos.zip".into());
        let dir = std::env::temp_dir().join(format!("lanes-netlinks-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out");
        let engine = Arc::new(Engine::new());
        engine.set_connections(8);
        // `LANES_NET_OFF` measures the same download without net-merge.
        engine.set_net_merge(std::env::var_os("LANES_NET_OFF").is_none());
        let started = std::time::Instant::now();
        engine.start(1, vec![url.clone()], path.clone(), Vec::new(), Kind::File);
        let mut most = 0;
        let snap = loop {
            let snap = engine.snapshots().into_iter().next().expect("the task is listed");
            most = most.max(snap.links);
            match snap.state {
                state::DONE => break snap,
                state::ERROR => panic!("download failed: {}", snap.error),
                _ => thread::sleep(Duration::from_millis(100)),
            }
        };
        let seconds = started.elapsed().as_secs_f64();
        let size = fs::metadata(&path).unwrap().len();
        eprintln!("{size} bytes in {seconds:.1} s, {:.1} MB/s, at most {most} links at once", size as f64 / seconds / 1e6);
        for link in engine.net_stats().links {
            eprintln!("{} ({}) {}: {:.1} MB", link.label, link.name, link.state, link.received as f64 / 1e6);
        }
        assert_eq!(Some(size), snap.size);
        if other.is_none() {
            let mut hasher = Sha256::new();
            std::io::copy(&mut fs::File::open(&path).unwrap(), &mut hasher).unwrap();
            let got: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!(got, "bdbb3da3ce0dbb3cf0d7d73fafc4ee50fc168cf3350db98ed0f67e27ae5d2d7b");
        }
        engine.release_idle();
        assert!(engine.merge.lock().unwrap().is_none(), "the proxy goes once nothing runs");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// net-merge over the network: the same file given as two mirrors downloads whole, its segments spread over both
    /// sources and written into one file that matches the published hash. Ignored by default since it needs the net;
    /// run with `cargo test -- --ignored net_merge`.
    #[test]
    #[ignore = "needs the network"]
    fn net_merge_from_two_mirrors_matches_the_hash() {
        use sha2::{Digest, Sha256};
        let url = "https://github.com/eliasbui/lanes/releases/download/v0.1.0/Lanes-0.1.0-macos.zip".to_string();
        let want = "bdbb3da3ce0dbb3cf0d7d73fafc4ee50fc168cf3350db98ed0f67e27ae5d2d7b";
        let dir = std::env::temp_dir().join(format!("lanes-netmerge-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.zip");

        let engine = std::sync::Arc::new(Engine::new());
        engine.set_connections(8);
        engine.start(1, vec![url.clone(), url], path.clone(), Vec::new(), Kind::File);
        loop {
            let snap = engine.snapshots().into_iter().next().expect("the task is listed");
            assert!(snap.sources >= 1);
            match snap.state {
                state::DONE => break,
                state::ERROR => panic!("download failed: {}", snap.error),
                _ => thread::sleep(Duration::from_millis(200)),
            }
        }
        let mut hasher = Sha256::new();
        std::io::copy(&mut fs::File::open(&path).unwrap(), &mut hasher).unwrap();
        let got: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(got, want);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A video page read by Lanes itself (YouTube unless `LANES_VIDEO` names another), its streams fetched in pieces and
    /// joined by ffmpeg. Live, and it needs an ffmpeg on PATH:
    /// `LANES_VIDEO=<page> LANES_HEIGHT=1080 cargo test media_from_a_page -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn media_from_a_page() {
        let page = std::env::var("LANES_VIDEO").unwrap_or_else(|_| "https://www.youtube.com/watch?v=jNQXAC9IVRw".into());
        let video = extract::read(&page, &[]).expect("the page is read");
        let height = std::env::var("LANES_HEIGHT").ok().and_then(|h| h.parse().ok());
        let choice = extract::choices(&video).into_iter().find(|c| height.is_none() || c.height == height).expect("a choice");
        println!("{}: {} {:?} {:?}", video.title, choice.format, choice.height, choice.size);
        let dir = std::env::temp_dir().join(format!("lanes-media-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("video.mp4");
        let engine = std::sync::Arc::new(Engine::new());
        engine.set_connections(8);
        let started = std::time::Instant::now();
        engine.start(1, vec![page], path.clone(), Vec::new(), Kind::Page { format: choice.format });
        loop {
            let snap = engine.snapshots().into_iter().next().expect("the task is listed");
            match snap.state {
                state::DONE => break,
                state::ERROR => panic!("download failed: {}", snap.error),
                _ => thread::sleep(Duration::from_millis(200)),
            }
        }
        let size = fs::metadata(&path).unwrap().len();
        let seconds = started.elapsed().as_secs_f64();
        println!("{size} bytes in {seconds:.1} s, {:.1} MB/s", size as f64 / seconds / 1e6);
        let probe = std::process::Command::new(ytdlp::ffmpeg().unwrap()).arg("-i").arg(&path).output().unwrap();
        let info = String::from_utf8_lossy(&probe.stderr);
        assert!(info.contains("Video:") && info.contains("Audio:"), "one file with a picture and a sound: {info}");
        fs::remove_dir_all(&dir).unwrap();
    }
}
