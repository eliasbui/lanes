//! Any other site: the video a page names for others to embed or share (Open Graph's `og:video`, Twitter's player
//! stream, a JSON-LD `VideoObject`'s `contentUrl`), the files its `<video>` and `<source>` tags play, and the MP4, WebM
//! and HLS addresses written out in its scripts. Only what the page itself says; a player that builds its addresses in
//! script is the video button's to catch as the page plays it (extension/background.js).

use super::{Stream, Variant, Video};

/// The most addresses taken from one page, so that a page that lists a whole catalogue offers its first few.
const MAX_FOUND: usize = 12;

pub fn read(page: &str, cookie: &str) -> Result<Video, String> {
    let html = super::get_text(page, &headers(page, page, cookie))?;
    let video = video_of(page, &html, cookie);
    if video.variants.is_empty() {
        return Err("Chưa thấy bản tải được. Hãy phát video vài giây rồi bấm lại. Video có DRM thì không tải được.".into());
    }
    Ok(video)
}

/// The headers for the page, or for a stream at `url`: the page's cookie only when it is on the page's own host.
fn headers(page: &str, url: &str, cookie: &str) -> Vec<(String, String)> {
    let mut headers = vec![("User-Agent".to_owned(), super::BROWSER_AGENT.to_owned()), ("Referer".to_owned(), page.to_owned())];
    if !cookie.is_empty() && super::host(url).is_some() && super::host(url) == super::host(page) {
        headers.push(("Cookie".into(), cookie.into()));
    }
    headers
}

/// The videos the page names, in the order it names them, each as one variant.
pub fn video_of(page: &str, html: &str, cookie: &str) -> Video {
    let base = url::Url::parse(page).ok();
    let mut found: Vec<String> = Vec::new();
    let mut add = |raw: &str| {
        let raw = raw.trim().replace("\\/", "/").replace("&amp;", "&");
        let Some(url) = base.as_ref().and_then(|b| b.join(&raw).ok()).filter(|u| matches!(u.scheme(), "http" | "https")) else { return };
        let url = url.to_string();
        if media_kind(&url).is_some() && !found.contains(&url) && found.len() < MAX_FOUND {
            found.push(url);
        }
    };
    for key in ["og:video:secure_url", "og:video:url", "og:video", "twitter:player:stream"] {
        for value in meta_values(html, key) {
            add(&value);
        }
    }
    for value in json_values(html, "contentUrl") {
        add(&value);
    }
    for tag in ["video", "source"] {
        for value in attribute_values(html, tag, "src") {
            add(&value);
        }
    }
    for value in script_addresses(html) {
        add(&value);
    }
    let title = meta_values(html, "og:title").into_iter().next().or_else(|| between(html, "<title>", "</title>").map(str::to_owned)).map(|t| unescape(t.trim())).unwrap_or_default();
    let variants = found
        .into_iter()
        .enumerate()
        .map(|(i, url)| {
            let (hls, ext) = media_kind(&url).unwrap_or((false, "mp4"));
            let height = height_in(&url);
            let headers = headers(page, &url, cookie);
            let stream = Stream { url, hls, size: None, ext: ext.into(), headers };
            Variant { id: format!("g{i}"), height, video: Some(stream), ..Variant::default() }
        })
        .collect();
    Video { title, variants }
}

/// Whether an address is a video file or an HLS playlist, by its path: (HLS, the file type).
fn media_kind(url: &str) -> Option<(bool, &'static str)> {
    let path = url.split(['?', '#']).next()?.to_ascii_lowercase();
    if path.ends_with(".m3u8") {
        Some((true, "mp4"))
    } else if path.ends_with(".mp4") || path.ends_with(".m4v") {
        Some((false, "mp4"))
    } else if path.ends_with(".webm") {
        Some((false, "webm"))
    } else if path.ends_with(".mov") {
        Some((false, "mov"))
    } else {
        None
    }
}

/// A height written in an address the way sites name their renditions: `720p`, `_1080.mp4`.
fn height_in(url: &str) -> Option<u32> {
    let bytes = url.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric()) {
            let digits: String = url[i..].chars().take_while(char::is_ascii_digit).collect();
            let after = url[i + digits.len()..].chars().next();
            if (3..=4).contains(&digits.len()) && matches!(after, Some('p') | Some('.') | Some('_')) {
                if let Ok(height) = digits.parse::<u32>() {
                    if (144..=4320).contains(&height) {
                        return Some(height);
                    }
                }
            }
            i += digits.len().max(1);
        } else {
            i += 1;
        }
    }
    None
}

/// The `content` of each `<meta property|name="key">`.
fn meta_values(html: &str, key: &str) -> Vec<String> {
    tags(html, "meta")
        .filter(|tag| attribute(tag, "property").or_else(|| attribute(tag, "name")).is_some_and(|k| k.eq_ignore_ascii_case(key)))
        .filter_map(|tag| attribute(tag, "content"))
        .collect()
}

fn attribute_values(html: &str, tag: &str, name: &str) -> Vec<String> {
    tags(html, tag).filter_map(|t| attribute(t, name)).collect()
}

/// Each opening `<name ...>` tag's text.
fn tags<'a>(html: &'a str, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    let lower = html.to_ascii_lowercase();
    let open = format!("<{name}");
    let mut starts = Vec::new();
    let mut from = 0;
    while let Some(at) = lower[from..].find(&open) {
        let start = from + at;
        let next = lower.as_bytes().get(start + open.len()).copied();
        if matches!(next, Some(b' ' | b'\t' | b'\n' | b'\r' | b'/' | b'>')) {
            starts.push(start);
        }
        from = start + open.len();
    }
    starts.into_iter().filter_map(move |start| html[start..].find('>').map(|end| &html[start..start + end]))
}

/// The value of one attribute in a tag's text, quoted or not.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find(name) {
        let start = from + at;
        let before = lower.as_bytes().get(start.wrapping_sub(1)).copied();
        let rest = tag[start + name.len()..].trim_start();
        if matches!(before, Some(b' ' | b'\t' | b'\n' | b'\r')) && rest.starts_with('=') {
            let value = rest[1..].trim_start();
            let text = match value.chars().next() {
                Some(q @ ('"' | '\'')) => value[1..].split(q).next().unwrap_or_default(),
                _ => value.split(|c: char| c.is_whitespace() || c == '>').next().unwrap_or_default(),
            };
            return Some(unescape(text));
        }
        from = start + name.len();
    }
    None
}

/// Each string value of `"key":"..."` in the page's JSON (JSON-LD and the like).
fn json_values(html: &str, key: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find(&format!("\"{key}\"")) {
        rest = &rest[at + key.len() + 2..];
        if let Some(value) = rest.trim_start().strip_prefix(':').map(str::trim_start).and_then(|v| v.strip_prefix('"')) {
            if let Some(end) = value.find('"') {
                values.push(value[..end].to_owned());
            }
        }
    }
    values
}

/// Absolute MP4, WebM and HLS addresses written out anywhere in the page, scripts included.
fn script_addresses(html: &str) -> Vec<String> {
    let text = html.replace("\\/", "/");
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find("http") {
        let start = from + at;
        let end = text[start..].find(|c: char| c.is_whitespace() || "\"'<>\\`()".contains(c)).map_or(text.len(), |e| start + e);
        let candidate = &text[start..end];
        if (candidate.starts_with("https://") || candidate.starts_with("http://")) && media_kind(candidate).is_some() {
            found.push(candidate.to_owned());
        }
        from = end.max(start + 4);
    }
    found
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    Some(&text[start..start + text[start..].find(close)?])
}

fn unescape(text: &str) -> String {
    text.replace("&amp;", "&").replace("&quot;", "\"").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_offers_the_videos_it_names_in_the_order_it_names_them() {
        let html = r#"<html><head><title>Fallback</title>
            <meta property="og:title" content="A &amp; B">
            <meta property="og:video:secure_url" content="https://cdn.example.com/v/clip_720p.mp4?t=1&amp;x=2">
            <meta property="og:video" content="https://cdn.example.com/v/clip_720p.mp4?t=1&x=2">
            <meta name="twitter:player" content="https://example.com/embed/1">
            <script type="application/ld+json">{"@type":"VideoObject","contentUrl":"https:\/\/cdn.example.com\/v\/master.m3u8"}</script>
            </head><body><video src="/media/local.webm"></video>
            <video><source src='rel/1080.mp4' type="video/mp4"></video>
            <script>var player = {"file":"https:\/\/cdn.example.com\/v\/extra_480p.mp4"};</script>
            <a href="https://example.com/page.html">not a video</a></body></html>"#;
        let video = video_of("https://example.com/watch/1", html, "");
        assert_eq!(video.title, "A & B");
        let urls: Vec<&str> = video.variants.iter().map(|v| v.video.as_ref().unwrap().url.as_str()).collect();
        assert_eq!(urls, [
            "https://cdn.example.com/v/clip_720p.mp4?t=1&x=2",
            "https://cdn.example.com/v/master.m3u8",
            "https://example.com/media/local.webm",
            "https://example.com/watch/rel/1080.mp4",
            "https://cdn.example.com/v/extra_480p.mp4",
        ]);
        let ids: Vec<&str> = video.variants.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["g0", "g1", "g2", "g3", "g4"]);
        assert_eq!(video.variants[0].height, Some(720));
        assert!(video.variants[1].video.as_ref().unwrap().hls);
        assert_eq!(video.variants[2].ext(), "webm");
        assert_eq!(video.variants[3].height, Some(1080));
        assert!(video.variants.iter().all(|v| v.audio.is_none()), "each file carries its own sound");
    }

    #[test]
    fn a_page_without_a_video_offers_nothing() {
        let video = video_of("https://example.com/", "<html><title>x</title><img src='a.png'><a href='b.mp4.html'>b</a></html>", "");
        assert!(video.variants.is_empty());
        assert_eq!(height_in("https://x/v_1280x720.mp4"), None, "a width is not a height");
        assert_eq!(height_in("https://x/2160p/v.mp4"), Some(2160));
    }
}
