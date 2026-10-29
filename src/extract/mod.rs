//! Lanes' own reading of video pages, in place of yt-dlp. Each site's module turns a page into the video's title and
//! its variants: a file that carries its sound, or a picture and a sound to join. The engine downloads their streams in
//! pieces over several connections and joins them with the ffmpeg the app carries (engine `run_media`). yt-dlp, when
//! the user has it, is asked only when no module could read the page.

use crate::ytdlp::{Choice, Formats};
use serde_json::Value;
use std::io::Read;

pub mod facebook;
pub mod generic;
pub mod instagram;
pub mod jsc;
pub mod tiktok;
pub mod twitter;
pub mod youtube;

/// Before the id of a variant read here in the menu's format, so a download knows it is not one of yt-dlp's.
pub const PREFIX: &str = "lanes.";
/// The largest piece asked for in one request, unless a stream says otherwise.
pub const PIECE: u64 = 10 * 1024 * 1024;
/// The browser pages are asked as, when a site has no app of its own to be asked as.
pub(crate) const BROWSER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36";
/// The most of a page or an API answer read.
const MAX_BODY: u64 = 32 * 1024 * 1024;

/// One stream as the site serves it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Stream {
    pub url: String,
    /// An HLS playlist rather than a file.
    pub hls: bool,
    /// The bytes of the file, when the site says; a file without it is probed.
    pub size: Option<u64>,
    /// The file type: "mp4", "webm", "m4a".
    pub ext: String,
    /// What the server of the stream wants with each request (User-Agent, Referer), sent to the stream's host as they are.
    /// A cookie only when the site set it for this stream (TikTok's `tt_chain_token`) or the stream is on the page's own
    /// host: never the user's cookies to another host.
    pub headers: Vec<(String, String)>,
}

/// One version of the video to download.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Variant {
    /// Unique within the page and stable between two readings of it, so a download can find it again: format
    /// characters only (letters, digits, `+`, `-`, `_`, `.`).
    pub id: String,
    /// None for sound only.
    pub height: Option<u32>,
    pub fps: u32,
    /// H.264, which every player plays.
    pub h264: bool,
    /// The picture, with its sound when `audio` is None. None for sound only.
    pub video: Option<Stream>,
    /// The sound, joined to the picture by ffmpeg, or the whole of a sound-only variant.
    pub audio: Option<Stream>,
}

impl Variant {
    pub fn size(&self) -> Option<u64> {
        let sizes: Vec<Option<u64>> = [&self.video, &self.audio].into_iter().flatten().map(|s| s.size).collect();
        sizes.iter().copied().sum()
    }

    /// The streams to fetch, the picture first.
    pub fn streams(&self) -> Vec<&Stream> {
        [&self.video, &self.audio].into_iter().flatten().collect()
    }

    /// The type of the file made: an MP4 when a picture and a sound are joined.
    pub fn ext(&self) -> String {
        match (&self.video, &self.audio) {
            (Some(_), Some(_)) => "mp4".into(),
            (Some(stream), None) | (None, Some(stream)) => stream.ext.clone(),
            (None, None) => "mp4".into(),
        }
    }
}

/// What a page offers.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Video {
    pub title: String,
    pub variants: Vec<Variant>,
}

/// A site's reader: the page's address and the Cookie header the browser sends to it (possibly empty).
type Reader = fn(&str, &str) -> Result<Video, String>;

fn host(url: &str) -> Option<String> {
    Some(url::Url::parse(url).ok()?.host_str()?.to_ascii_lowercase())
}

/// The reader for the site of `url`; the generic one for a site with no module of its own.
fn reader(url: &str) -> Reader {
    let host = host(url).unwrap_or_default();
    let on = |site: &str| host == site || host.ends_with(&format!(".{site}"));
    if on("youtube.com") || on("youtu.be") || on("youtube-nocookie.com") {
        youtube::read
    } else if on("facebook.com") || on("fb.watch") {
        facebook::read
    } else if on("instagram.com") {
        instagram::read
    } else if on("tiktok.com") {
        tiktok::read
    } else if on("x.com") || on("twitter.com") {
        twitter::read
    } else {
        generic::read
    }
}

fn cookie_of(headers: &[(String, String)]) -> &str {
    headers.iter().find(|(name, _)| name.eq_ignore_ascii_case("cookie")).map_or("", |(_, value)| value.as_str())
}

/// The page read by its site's module.
pub fn read(page: &str, headers: &[(String, String)]) -> Result<Video, String> {
    let video = reader(page)(page, cookie_of(headers))?;
    if video.variants.is_empty() {
        return Err("Trang này không có video tải được".into());
    }
    Ok(video)
}

/// The formats of the page, as the menu offers them.
pub fn formats(page: &str, headers: &[(String, String)]) -> Result<Formats, String> {
    let video = read(page, headers)?;
    Ok(Formats { title: video.title.clone(), choices: choices(&video), hint: None })
}

/// The menu's choices, best first: one per height, H.264 first since every player plays it, then the higher frame
/// rate, then the larger file; then sound only. Pictures of a height nobody gives are each kept, after the others.
pub fn choices(video: &Video) -> Vec<Choice> {
    let mut best: Vec<&Variant> = Vec::new();
    let key = |v: &Variant| (v.h264, v.fps, v.size());
    for variant in video.variants.iter().filter(|v| v.video.is_some()) {
        match best.iter_mut().find(|b| b.height.is_some() && b.height == variant.height) {
            Some(slot) if key(variant) > key(slot) => *slot = variant,
            Some(_) => {}
            None => best.push(variant),
        }
    }
    best.sort_by_key(|v| std::cmp::Reverse(v.height.is_some().then_some(v.height)));
    let sound = video.variants.iter().filter(|v| v.video.is_none() && v.audio.is_some()).max_by_key(|v| v.size());
    best.into_iter()
        .chain(sound)
        .map(|v| Choice { format: format!("{PREFIX}{}", v.id), height: v.height, size: v.size(), ext: v.ext() })
        .collect()
}

/// A GET for a page or an API, with `headers`; the body as text.
pub(crate) fn get_text(url: &str, headers: &[(String, String)]) -> Result<String, String> {
    let mut request = crate::engine::agent(None).get(url);
    for (name, value) in headers {
        request = request.set(name, value);
    }
    body(request.call())
}

/// A POST of `json` to an API, with `headers`; the answer as JSON.
pub(crate) fn post_json(url: &str, headers: &[(String, String)], json: &Value) -> Result<Value, String> {
    let mut request = crate::engine::agent(None).post(url).set("Content-Type", "application/json");
    for (name, value) in headers {
        request = request.set(name, value);
    }
    let text = body(request.send_string(&json.to_string()))?;
    serde_json::from_str(&text).map_err(|e| format!("Trang trả về dữ liệu lạ: {e}"))
}

fn body(response: Result<ureq::Response, ureq::Error>) -> Result<String, String> {
    let response = match response {
        Ok(response) => response,
        Err(ureq::Error::Status(code, _)) => return Err(format!("Máy chủ trả mã {code}")),
        Err(e) => return Err(format!("Không kết nối được: {e}")),
    };
    let mut text = String::new();
    response.into_reader().take(MAX_BODY).read_to_string(&mut text).map_err(|e| format!("Không đọc được trang: {e}"))?;
    Ok(text)
}

/// The first capture of `pattern` in `text`, a JSON string literal's content unescaped. For the values pages put in
/// their scripts, such as `"VISITOR_DATA":"..."`.
pub(crate) fn json_string_after(text: &str, key: &str) -> Option<String> {
    let start = text.find(&format!("\"{key}\":\""))? + key.len() + 4;
    let mut end = start;
    let bytes = text.as_bytes();
    while end < bytes.len() && bytes[end] != b'"' {
        end += if bytes[end] == b'\\' { 2 } else { 1 };
    }
    serde_json::from_str(&format!("\"{}\"", text.get(start..end)?)).ok()
}

/// The JSON object that starts at the first `marker` in `text`, such as `var ytInitialPlayerResponse = {...};`: read
/// as far as its braces balance, strings included.
pub(crate) fn json_object_after(text: &str, marker: &str) -> Option<Value> {
    let from = text.find(marker)? + marker.len();
    let start = from + text[from..].find('{')?;
    let bytes = text.as_bytes();
    let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return serde_json::from_str(&text[start..=i]).ok();
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stream(size: u64, ext: &str) -> Option<Stream> {
        Some(Stream { url: "https://cdn.example/x".into(), size: Some(size), ext: ext.into(), ..Stream::default() })
    }

    #[test]
    fn the_menu_offers_one_choice_a_height_h264_first_then_the_best_sound() {
        let v = |id: &str, height: u32, h264: bool, fps: u32, size: u64| Variant { id: id.into(), height: Some(height), fps, h264, video: stream(size, "mp4"), audio: stream(10, "m4a") };
        let video = Video {
            title: "t".into(),
            variants: vec![
                v("399+140", 1080, false, 60, 100),
                v("299+140", 1080, true, 60, 200),
                v("136+140", 720, true, 30, 50),
                Variant { id: "18".into(), height: Some(360), fps: 30, h264: true, video: stream(20, "mp4"), audio: None },
                Variant { id: "140".into(), audio: stream(10, "m4a"), ..Variant::default() },
                Variant { id: "139".into(), audio: stream(5, "m4a"), ..Variant::default() },
            ],
        };
        let choices = choices(&video);
        let formats: Vec<&str> = choices.iter().map(|c| c.format.as_str()).collect();
        assert_eq!(formats, ["lanes.299+140", "lanes.136+140", "lanes.18", "lanes.140"]);
        assert_eq!((choices[0].size, choices[0].ext.as_str()), (Some(210), "mp4"), "picture and sound joined into an MP4");
        assert_eq!((choices[2].size, choices[2].ext.as_str()), (Some(20), "mp4"));
        assert_eq!((choices[3].height, choices[3].ext.as_str()), (None, "m4a"));
        assert!(choices.iter().all(|c| crate::ytdlp::valid_format(&c.format)), "the extension sends them back as formats");
        let unknown = Variant { video: Some(Stream { size: None, ..Stream::default() }), audio: stream(1, "m4a"), ..Variant::default() };
        assert_eq!(unknown.size(), None, "a size that is not known is not guessed");
    }

    #[test]
    fn each_site_has_its_reader() {
        let is = |url: &str, f: Reader| reader(url) as usize == f as usize;
        assert!(is("https://www.youtube.com/watch?v=x", youtube::read));
        assert!(is("https://youtu.be/x", youtube::read));
        assert!(is("https://m.facebook.com/reel/1", facebook::read));
        assert!(is("https://www.instagram.com/reel/x/", instagram::read));
        assert!(is("https://www.tiktok.com/@a/video/1", tiktok::read));
        assert!(is("https://x.com/a/status/1", twitter::read));
        assert!(is("https://twitter.com/a/status/1", twitter::read));
        assert!(is("https://notyoutube.com/v", generic::read), "a host that only ends like a site is not it");
    }

    #[test]
    fn values_are_read_out_of_page_scripts() {
        let page = r#"<script>ytcfg.set({"VISITOR_DATA":"Cgt=ab\"c","X":1});var ytInitialPlayerResponse = {"a":{"b":"}{"},"c":[1,2]};var x;</script>"#;
        assert_eq!(json_string_after(page, "VISITOR_DATA").as_deref(), Some("Cgt=ab\"c"));
        assert_eq!(json_string_after(page, "MISSING"), None);
        assert_eq!(json_object_after(page, "ytInitialPlayerResponse = "), Some(json!({"a": {"b": "}{"}, "c": [1, 2]})));
    }
}
