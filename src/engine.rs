//! The download engine: several connections at once, each pulling one byte range of the file straight into its
//! place in the final file, with resume kept in a sidecar so a stopped download continues rather than restarts.
//!
//! One file is opened and grown to its full size up front; every connection writes its range at the right offset
//! with a positioned write, so no parts are joined at the end. What each connection has written is saved beside the
//! file a few times a minute, and on resume each one starts again from where it left off. The server is asked for
//! the same file by its ETag or modification time, so a file that changed on the server restarts instead of mixing
//! old and new bytes.
//!
//! Static equal segments, split once at the start.
//! ponytail: equal split, add IDM-style "split the largest remaining segment when a connection goes idle" when the
//! speed-up over a plain equal split is worth the extra bookkeeping.

use crate::hls;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
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
    /// An HLS video: `url` is its playlist, and the segments are joined into the file (`run_hls`).
    hls: bool,
    state: AtomicU8,
    downloaded: AtomicU64,
    total: AtomicU64,
    error: Mutex<String>,
    segments: Mutex<Vec<Arc<AtomicU64>>>,
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
            connections: self.segments.lock().map(|s| s.len()).unwrap_or(0),
            sources: self.sources.lock().map(|s| s.len().max(1)).unwrap_or(1),
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
}

impl Engine {
    pub fn new() -> Self {
        Engine { tasks: Mutex::new(Vec::new()), connections: AtomicU64::new(DEFAULT_CONNECTIONS as u64) }
    }

    pub fn set_connections(&self, n: usize) {
        self.connections.store(n.clamp(1, MAX_CONNECTIONS) as u64, Ordering::Relaxed);
    }

    fn connection_count(&self) -> usize {
        self.connections.load(Ordering::Relaxed) as usize
    }

    /// Starts a fresh download, or picks up one left half-done from a sidecar beside `final_path`. `sources` is the
    /// mirror pool for net-merge, the primary first (a one-item list is an ordinary download). The headers are the
    /// ones the browser carried, replayed on every connection. `hls` marks the URL as an HLS playlist.
    pub fn start(self: &Arc<Self>, id: i64, sources: Vec<String>, final_path: PathBuf, headers: Vec<(String, String)>, hls: bool) {
        let url = sources.first().cloned().unwrap_or_default();
        let resume = load_meta(&final_path).filter(|m| m.url == url);
        let task = Arc::new(Task {
            id,
            url: url.clone(),
            final_path: final_path.clone(),
            sources: Mutex::new(sources),
            headers,
            hls,
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
        self.tasks.lock().map(|t| t.iter().map(|t| t.snapshot()).collect()).unwrap_or_default()
    }

    fn find(&self, id: i64) -> Option<Arc<Task>> {
        self.tasks.lock().ok()?.iter().find(|t| t.id == id).cloned()
    }

    /// The manager thread of one download: probe, open and size the part file, fan out one thread per segment,
    /// save progress while they run, then put the finished file in place.
    fn run(&self, task: Arc<Task>, resume: Option<Meta>) {
        if task.hls {
            return self.run_hls(&task);
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

        let dones: Vec<Arc<AtomicU64>> = segments.iter().map(|s| Arc::new(AtomicU64::new(s.done))).collect();
        if let Ok(mut slot) = task.segments.lock() {
            *slot = dones.clone();
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
        for (i, (segment, done)) in segments.iter().cloned().zip(dones.iter().cloned()).enumerate() {
            let (task, file) = (task.clone(), file.clone());
            // Each segment starts on a different mirror, so the sources are used in parallel from the first moment.
            handles.push(thread::spawn(move || download_segment(&task, &file, segment, &done, i)));
        }

        // Save the sidecar while the connections run, so a crash still leaves a resumable file. The flag stops the
        // saver once the connections have joined; it cannot wait on the state, which is only set after this returns.
        let saving = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let saver = {
            let (task, dones, saving) = (task.clone(), dones.clone(), saving.clone());
            thread::spawn(move || {
                while saving.load(Ordering::Relaxed) {
                    save_meta(&task, &dones);
                    for _ in 0..20 {
                        if !saving.load(Ordering::Relaxed) {
                            break;
                        }
                        thread::sleep(SAVE_EVERY / 20);
                    }
                }
                save_meta(&task, &dones);
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
    /// folder beside the final file so a resume skips the ones already there; then joined in order into the file.
    /// The size shown is an estimate from the segments so far until the join gives the real one.
    fn run_hls(&self, task: &Task) {
        let media = match hls::load(&task.url, &task.headers) {
            Ok(media) => media,
            Err(e) => return task.set_error(e),
        };
        let folder = part_of(&task.final_path);
        if let Err(e) = fs::create_dir_all(&folder) {
            return task.set_error(format!("Không tạo được thư mục tạm: {e}"));
        }
        let count = media.segments.len();
        let workers = self.connection_count().min(count);
        if let Ok(mut slot) = task.segments.lock() {
            *slot = (0..workers).map(|_| Arc::new(AtomicU64::new(0))).collect();
        }
        task.downloaded.store(0, Ordering::Relaxed);
        let keys = Mutex::new(HashMap::new());
        let mut parts = Vec::with_capacity(count + 1);
        if let Some(init) = &media.init {
            let path = folder.join("init");
            if let Err(e) = keep_part(task, init, &path, &keys) {
                return task.set_error(e);
            }
            parts.push(path);
        }
        parts.extend((0..count).map(|i| folder.join(format!("{i:06}"))));

        let next = AtomicUsize::new(0);
        let (kept, finished) = (AtomicU64::new(0), AtomicU64::new(0));
        let first = parts.len() - count;
        let all_ok = thread::scope(|scope| {
            let worker = || loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(segment) = media.segments.get(i) else { return true };
                match keep_part(task, segment, &parts[first + i], &keys) {
                    Ok(size) => {
                        let kept = kept.fetch_add(size, Ordering::Relaxed) + size;
                        let finished = finished.fetch_add(1, Ordering::Relaxed) + 1;
                        task.total.store(kept / finished * count as u64, Ordering::Relaxed);
                    }
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
        });
        if !all_ok || task.paused_or_removed() {
            return;
        }
        match join_parts(&parts, &folder, &task.final_path) {
            Ok(size) => {
                task.total.store(size, Ordering::Relaxed);
                task.downloaded.store(size, Ordering::Relaxed);
                let _ = fs::remove_dir_all(&folder);
                task.state.store(state::DONE, Ordering::SeqCst);
            }
            Err(e) => task.set_error(format!("Không ghép được video: {e}")),
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
fn keep_part(task: &Task, segment: &hls::Segment, path: &Path, keys: &Mutex<HashMap<String, Vec<u8>>>) -> Result<u64, String> {
    if let Ok(meta) = fs::metadata(path) {
        task.downloaded.fetch_add(meta.len(), Ordering::Relaxed);
        return Ok(meta.len());
    }
    let mut attempt = 0;
    loop {
        if task.paused_or_removed() {
            return Err("Đã dừng".into());
        }
        let fetched = fetch_counted(task, segment).and_then(|body| match &segment.key {
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
fn fetch_counted(task: &Task, segment: &hls::Segment) -> Result<Vec<u8>, String> {
    let mut reader = hls::open(&segment.url, segment.range, &task.url, &task.headers)?;
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
    hls::open(url, None, &task.url, &task.headers)?.take(64).read_to_end(&mut key).map_err(|e| format!("Không tải được khóa giải mã: {e}"))?;
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

/// One connection: fetches its byte range from where it left off, writing each chunk straight to its offset. It
/// returns whether the range is complete; a pause or a removal ends it early without an error. With net-merge it
/// starts on the mirror `source_start` and moves to the next mirror each time one fails, so a dead mirror never
/// strands a segment while others are alive.
fn download_segment(task: &Task, file: &fs::File, segment: Segment, done: &AtomicU64, source_start: usize) -> bool {
    let sources = task.sources.lock().map(|s| s.clone()).unwrap_or_default();
    let sources = if sources.is_empty() { vec![task.url.clone()] } else { sources };
    let mut si = source_start % sources.len();
    let len = segment.end.map(|end| end + 1 - segment.start);
    let mut attempt = 0;
    loop {
        if task.paused_or_removed() {
            return false;
        }
        let already = done.load(Ordering::Relaxed);
        if len.is_some_and(|len| already >= len) {
            return true;
        }
        let from = segment.start + already;
        let range = match segment.end {
            Some(end) => format!("bytes={from}-{end}"),
            None => format!("bytes={from}-"),
        };
        match fetch_range(&sources[si], &range, segment.end.is_some(), &task.headers) {
            Ok(mut reader) => {
                let mut buf = vec![0u8; CHUNK];
                let mut offset = from;
                loop {
                    if task.paused_or_removed() {
                        return false;
                    }
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if let Err(e) = write_at(file, offset, &buf[..n]) {
                                task.set_error(format!("Không ghi được tệp: {e}"));
                                return false;
                            }
                            offset += n as u64;
                            done.fetch_add(n as u64, Ordering::Relaxed);
                            task.downloaded.fetch_add(n as u64, Ordering::Relaxed);
                        }
                        Err(_) => break,
                    }
                }
                if len.is_none_or(|len| done.load(Ordering::Relaxed) >= len) {
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

pub(crate) fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(SECONDS))
        .timeout_read(Duration::from_secs(SECONDS))
        .redirects(10)
        .build()
}

/// Opens a reader over one byte range. `identity` is asked for so a range maps to the same bytes on disk, and a
/// server that ignores the range (a 200 for a range request) is refused rather than written at the wrong place.
fn fetch_range(url: &str, range: &str, expect_partial: bool, headers: &[(String, String)]) -> Result<Box<dyn Read + Send>, String> {
    let mut request = agent().get(url).set("Range", range).set("Accept-Encoding", "identity");
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
    let mut request = agent().get(url).set("Range", "bytes=0-0").set("Accept-Encoding", "identity");
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

/// Writes the sidecar for a resume, copying each connection's live progress into the saved segments.
fn save_meta(task: &Task, dones: &[Arc<AtomicU64>]) {
    let Ok(mut meta) = task.meta.lock() else { return };
    for (segment, done) in meta.segments.iter_mut().zip(dones) {
        segment.done = done.load(Ordering::Relaxed);
    }
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
        engine.start(1, vec![format!("{base}/master.m3u8")], path.clone(), Vec::new(), true);
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
    /// byte ranges, each joined into a file that starts as its container should. Run with
    /// `cargo test -- --ignored hls_from`.
    #[test]
    #[ignore = "needs the network"]
    fn hls_from_the_network_makes_whole_files() {
        let streams = [
            ("https://test-streams.mux.dev/x36xhzz/x36xhzz.m3u8", "ts"),
            ("https://devstreaming-cdn.apple.com/videos/streaming/examples/img_bipbop_adv_example_fmp4/master.m3u8", "mp4"),
        ];
        let dir = std::env::temp_dir().join(format!("lanes-hls-net-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let engine = Arc::new(Engine::new());
        engine.set_connections(8);
        for (id, (master, ext)) in streams.into_iter().enumerate() {
            let choices = hls::choices(master, &[]).unwrap();
            assert_eq!(choices.ext, ext);
            let smallest = choices.variants.last().unwrap().url.clone();
            let path = dir.join(format!("{id}.{ext}"));
            engine.start(id as i64, vec![smallest], path.clone(), Vec::new(), true);
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
            if ext == "ts" {
                assert!(bytes.len() > 188 * 100 && bytes.iter().step_by(188).take(100).all(|&b| b == 0x47), "MPEG-TS packets");
            } else {
                assert_eq!(&bytes[4..8], b"ftyp", "an MP4 starts with its ftyp box");
            }
        }
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
        engine.start(1, vec![url.clone(), url], path.clone(), Vec::new(), false);
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
}
