//! Facebook, read the way yt-dlp reads it (yt_dlp/extractor/facebook.py): the video's page, asked for as a browser,
//! carries the video in the JSON of its `data-sjs` scripts, as MP4 files that carry their sound (`sd`, `hd`) and a DASH
//! manifest of pictures and sounds apart. Signed out, Facebook gives a public video at a few qualities; with the
//! browser's cookie of a session, the page has what the signed-in player gets, and the videos only friends or members
//! see.
//!
//! Instagram is Meta's too and serves its videos the same way, so the scripts and the manifest are read here for both.

use super::{Stream, Variant, Video};
use serde_json::Value;
use std::time::Duration;

/// The browser a page is asked as. Without the Accept and the Sec-Fetch-Mode of a navigation, Facebook and Instagram
/// both answer with a page that has no video in it.
pub(super) const PAGE_HEADERS: [(&str, &str); 3] = [
    ("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36"),
    ("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"),
    ("Sec-Fetch-Mode", "navigate"),
];
/// fbcdn holds a browser back; yt-dlp asks for the files as Facebook's own crawler (`process_formats`).
const CDN_AGENT: &str = "facebookexternalhit/1.1";
/// The keys of an object of the page's data that hold a video's addresses.
const PLAYABLE: [&str; 6] =
    ["videoDeliveryLegacyFields", "videoDeliveryResponseFragment", "browser_native_hd_url", "browser_native_sd_url", "playable_url", "playable_url_quality_hd"];

pub fn read(page: &str, cookie: &str) -> Result<Video, String> {
    let mut url = url::Url::parse(page).map_err(|_| "Địa chỉ trang không hợp lệ".to_owned())?;
    // m.facebook.com and the like answer with a page without the video's data; yt-dlp asks www for them too.
    if url.host_str().is_some_and(|h| h.ends_with(".facebook.com")) {
        let _ = url.set_host(Some("www.facebook.com"));
    }
    let (html, last) = fetch(url, cookie)?;
    let id = video_id(&last).or_else(|| video_id(page));
    video_of(&html, id.as_deref(), cookie.contains("c_user="))
}

/// The page at `url` and the address it ends at. ureq drops the Cookie header on every redirect, and Facebook sends most
/// addresses of a video on to its owner's page; so redirects are followed here, the cookie going along only while they
/// stay on the site it is for.
fn fetch(mut url: url::Url, cookie: &str) -> Result<(String, String), String> {
    let home = site(&url);
    let agent = ureq::AgentBuilder::new().redirects(0).timeout_connect(Duration::from_secs(30)).timeout_read(Duration::from_secs(30)).build();
    for _ in 0..10 {
        let mut request = agent.get(url.as_str());
        for (name, value) in PAGE_HEADERS {
            request = request.set(name, value);
        }
        if !cookie.is_empty() && site(&url) == home {
            request = request.set("Cookie", cookie);
        }
        let response = request.call();
        let next = match &response {
            Ok(r) if (300..400).contains(&r.status()) => r.header("Location").and_then(|to| url.join(to).ok()),
            _ => None,
        };
        match next {
            Some(next) => url = next,
            None => return Ok((super::body(response)?, url.into())),
        }
    }
    Err("Trang chuyển hướng quá nhiều lần".into())
}

/// The last two labels of the host: `facebook.com` for www.facebook.com and m.facebook.com, `fb.watch` for fb.watch.
fn site(url: &url::Url) -> String {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').collect();
    labels[labels.len().saturating_sub(2)..].join(".")
}

/// The number of the video at `url`: a reel, a watch page, video.php, a page's videos. None for a post, a share link or
/// a group, whose page has the video first.
pub fn video_id(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    let number = |s: &str| (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.to_owned());
    if let Some(id) = url.query_pairs().find(|(k, _)| k == "v" || k == "video_id").and_then(|(_, v)| number(&v)) {
        return Some(id);
    }
    let segments: Vec<&str> = url.path_segments()?.collect();
    let at = segments.iter().position(|s| *s == "reel" || *s == "videos")?;
    // A page's videos may have a list's name before the number: /<page>/videos/vb.123/456/.
    segments[at + 1..].iter().take(2).find_map(|s| number(s))
}

/// The video in a Facebook page: the object of its data with addresses to play, the one of the video `id` when it is
/// known. A page with none says why: a login wall, or a viewer not signed in.
pub fn video_of(html: &str, id: Option<&str>, signed_in: bool) -> Result<Video, String> {
    let mine = |o: &Value| {
        let same = |k: &str| o[k].as_str().is_some_and(|v| Some(v) == id) || o[k].as_u64().is_some_and(|v| Some(v.to_string().as_str()) == id);
        (id.is_none() || same("id") || same("videoId")) && PLAYABLE.iter().any(|k| o.get(*k).is_some_and(|v| !v.is_null()))
    };
    let data: Vec<Value> = scripts(html).collect();
    let Some(video) = data.iter().find_map(|s| find(s, &mine)) else {
        let wall = ["id=\"login_form\"", ">You must log in to continue", "id=\"loginbutton\""].iter().any(|m| html.contains(m));
        return Err(if wall || !signed_in { crate::ytdlp::SIGN_IN.to_owned() } else { "Không tìm thấy video trong trang Facebook này".to_owned() });
    };
    let legacy = video.get("videoDeliveryLegacyFields").filter(|l| l.is_object()).unwrap_or(video);
    let fresh = &video["videoDeliveryResponseFragment"]["videoDeliveryResponseResult"];
    let shape = (video["width"].as_u64().unwrap_or(0), video["height"].as_u64().unwrap_or(0));
    let mut files = Vec::new();
    for (key, id) in [("playable_url_quality_hd", "hd"), ("browser_native_hd_url", "hd"), ("playable_url", "sd"), ("browser_native_sd_url", "sd")] {
        if let Some(url) = legacy[key].as_str() {
            files.push(File::new(id, url, None, shape));
        }
    }
    for p in fresh["progressive_urls"].as_array().into_iter().flatten() {
        if let (Some(url), Some(quality)) = (p["progressive_url"].as_str(), p["metadata"]["quality"].as_str()) {
            files.push(File::new(&quality.to_ascii_lowercase(), url, None, shape));
        }
    }
    let manifests = std::iter::once(&legacy["dash_manifest_xml_string"]).chain(fresh["dash_manifests"].as_array().into_iter().flatten().map(|m| &m["manifest_xml"]));
    let reps = manifests.filter_map(Value::as_str).flat_map(representations).collect();
    let headers = vec![("User-Agent".to_owned(), CDN_AGENT.to_owned())];
    let mut variants = variants(files, reps, &headers);
    // The newer answer's HLS playlist, only when there is nothing else (yt-dlp reads it alongside the rest).
    if let (true, Some(url)) = (variants.is_empty(), fresh["hls_playlist_urls"][0]["hls_playlist_url"].as_str()) {
        let video = Stream { url: url.to_owned(), hls: true, size: None, ext: "mp4".into(), headers };
        variants.push(Variant { id: "hls".into(), height: (shape.1 > 0).then_some(shape.1 as u32), h264: true, video: Some(video), ..Variant::default() });
    }
    let title = between(html, "<title>", "</title>")
        .map(entities)
        .filter(|t| !t.is_empty() && t != "Facebook")
        .or_else(|| between(html, "<meta name=\"description\" content=\"", "\"").map(entities))
        .and_then(|t| t.lines().next().map(|l| l.trim().to_owned()))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| format!("Facebook {}", id.unwrap_or("video")));
    Ok(Video { title, variants })
}

/// The JSON of each `data-sjs` script of a Facebook or Instagram page, where the page keeps the data it was made with.
pub(super) fn scripts(html: &str) -> impl Iterator<Item = Value> + '_ {
    html.split("data-sjs>").skip(1).filter_map(|s| serde_json::from_str(&s[..s.find("</script>")?]).ok())
}

/// The first object in `value`, depth first, that `hit` accepts.
pub(super) fn find<'a>(value: &'a Value, hit: &dyn Fn(&Value) -> bool) -> Option<&'a Value> {
    match value {
        Value::Object(_) if hit(value) => Some(value),
        Value::Object(map) => map.values().find_map(|v| find(v, hit)),
        Value::Array(list) => list.iter().find_map(|v| find(v, hit)),
        _ => None,
    }
}

/// A file of the video that carries its sound.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct File {
    pub id: String,
    pub url: String,
    pub height: Option<u32>,
}

impl File {
    /// The height, when the site does not give it, from the encoding the file's address names; failing that, the
    /// video's own.
    pub fn new(id: &str, url: &str, height: Option<u32>, shape: (u64, u64)) -> File {
        let height = height.or_else(|| file_height(url, shape)).or((shape.1 > 0).then_some(shape.1 as u32));
        File { id: clean(id), url: url.to_owned(), height }
    }
}

/// The height of a file whose page does not say it. Its address's `efg` is base64 JSON naming its encoding, as
/// `xpv_progressive.FACEBOOK..C3.720.dash_baseline_1_v1`, where the number after C3 is the width; the height follows
/// from the video's width and height.
/// ponytail: rounded to an even height, so a 480-wide 9:16 file reads 854p; read the file's own header if a label must
/// be exact.
fn file_height(url: &str, (width, height): (u64, u64)) -> Option<u32> {
    let efg = url::Url::parse(url).ok()?.query_pairs().find(|(k, _)| k == "efg")?.1.into_owned();
    let tag = String::from_utf8(base64(&efg)).ok()?;
    let wide: f64 = tag.split('.').skip_while(|s| *s != "C3").nth(1)?.parse().ok()?;
    (width > 0).then(|| ((wide * height as f64 / width as f64 / 2.0).round() * 2.0) as u32)
}

fn base64(text: &str) -> Vec<u8> {
    let (mut out, mut acc, mut bits) = (Vec::new(), 0u32, 0);
    for c in text.bytes() {
        let six = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => continue,
        };
        acc = acc << 6 | u32::from(six);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// One Representation of a DASH manifest: a whole file of picture or of sound at its BaseURL.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Rep {
    pub id: String,
    pub url: String,
    pub video: bool,
    pub height: Option<u32>,
    pub fps: u32,
    pub h264: bool,
    pub size: Option<u64>,
    pub bandwidth: u64,
}

/// The files of a DASH manifest as Facebook and Instagram write it: one BaseURL a Representation, the whole file.
/// ponytail: read as text, not as XML; enough for Meta's manifests, not for segment templates (a live video).
pub(super) fn representations(mpd: &str) -> Vec<Rep> {
    let mut reps = Vec::new();
    for set in mpd.split("<AdaptationSet").skip(1) {
        let set_tag = &set[..set.find('>').unwrap_or(set.len())];
        for rep in set.split("<Representation").skip(1) {
            let tag = &rep[..rep.find('>').unwrap_or(rep.len())];
            let get = |name: &str| attribute(tag, name).or_else(|| attribute(set_tag, name));
            let (Some(id), Some(url)) = (get("id"), between(rep, "<BaseURL>", "</BaseURL>")) else { continue };
            let kind = get("mimeType").or_else(|| get("contentType")).unwrap_or_default();
            let fps = get("frameRate").and_then(|r| {
                let (n, d) = r.split_once('/').unwrap_or((&r, "1"));
                Some((n.parse::<f64>().ok()? / d.parse::<f64>().ok()?).round() as u32)
            });
            reps.push(Rep {
                id: clean(&id),
                url: entities(url),
                video: kind.starts_with("video"),
                height: get("height").and_then(|h| h.parse().ok()),
                fps: fps.unwrap_or(0),
                h264: get("codecs").is_some_and(|c| c.starts_with("avc1")),
                size: get("FBContentLength").and_then(|s| s.parse().ok()),
                bandwidth: get("bandwidth").and_then(|b| b.parse().ok()).unwrap_or(0),
            });
        }
    }
    reps
}

/// The variants of a video: each DASH picture joined to the best DASH sound (or alone, when the video has none), the
/// files that carry their sound, and the sound alone. Every stream is asked for with `headers`.
pub(super) fn variants(files: Vec<File>, all: Vec<Rep>, headers: &[(String, String)]) -> Vec<Variant> {
    let stream = |url: &str, size: Option<u64>, ext: &str| Stream { url: url.to_owned(), hls: false, size, ext: ext.into(), headers: headers.to_vec() };
    let mut reps: Vec<Rep> = Vec::new();
    for r in all {
        if !reps.iter().any(|x| x.id == r.id) {
            reps.push(r);
        }
    }
    let sound = reps.iter().filter(|r| !r.video).max_by_key(|r| r.bandwidth);
    let mut out = Vec::new();
    for r in reps.iter().filter(|r| r.video) {
        let id = sound.map_or_else(|| r.id.clone(), |a| format!("{}+{}", r.id, a.id));
        let audio = sound.map(|a| stream(&a.url, a.size, "m4a"));
        out.push(Variant { id, height: r.height, fps: r.fps, h264: r.h264, video: Some(stream(&r.url, r.size, "mp4")), audio });
    }
    for f in files {
        if out.iter().any(|v: &Variant| v.id == f.id || v.video.as_ref().is_some_and(|s| s.url == f.url)) {
            continue;
        }
        // Meta's MP4 files are H.264, for every browser to play.
        out.push(Variant { id: f.id, height: f.height, fps: 0, h264: true, video: Some(stream(&f.url, None, "mp4")), audio: None });
    }
    if let Some(a) = sound {
        out.push(Variant { id: a.id.clone(), audio: Some(stream(&a.url, a.size, "m4a")), ..Variant::default() });
    }
    out
}

/// A variant's id in format characters only.
fn clean(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || "+-_.".contains(c) { c } else { '_' }).collect()
}

/// The value of the attribute `name` in a tag's text.
fn attribute(tag: &str, name: &str) -> Option<String> {
    between(tag, &format!(" {name}=\""), "\"").map(str::to_owned)
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let from = text.find(start)? + start.len();
    Some(&text[from..from + text[from..].find(end)?])
}

/// HTML's and XML's escapes undone: `&amp;`, `&quot;`, `&#xb7;`, `&#39;`.
fn entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let end = rest.find(';').filter(|&e| e <= 10);
        let name = end.map(|e| &rest[1..e]);
        let char = match name {
            Some("amp") => Some('&'),
            Some("lt") => Some('<'),
            Some("gt") => Some('>'),
            Some("quot") => Some('"'),
            Some("apos") => Some('\''),
            Some(n) if n.starts_with("#x") || n.starts_with("#X") => u32::from_str_radix(&n[2..], 16).ok().and_then(char::from_u32),
            Some(n) if n.starts_with('#') => n[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match (char, end) {
            (Some(c), Some(e)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_number_is_found_in_every_kind_of_facebook_link() {
        for url in [
            "https://www.facebook.com/watch/?v=10153231379946729",
            "https://www.facebook.com/video.php?v=10153231379946729&fref=nf",
            "https://www.facebook.com/reel/10153231379946729/?s=fb_shorts_tab",
            "https://www.facebook.com/facebook/videos/10153231379946729/",
            "https://www.facebook.com/facebook/videos/vb.20531316728/10153231379946729/",
            "https://www.facebook.com/video/embed?video_id=10153231379946729",
        ] {
            assert_eq!(video_id(url).as_deref(), Some("10153231379946729"), "{url}");
        }
        assert_eq!(video_id("https://www.facebook.com/share/v/1AbCdEf/"), None, "a share link is followed to the video");
        assert_eq!(video_id("https://fb.watch/abcDEF/"), None);
        assert_eq!(video_id("https://www.facebook.com/page/posts/pfbid0abc"), None, "a post's page has its video first");
    }

    #[test]
    fn the_cookie_stays_on_its_site() {
        let site = |u: &str| site(&url::Url::parse(u).unwrap());
        assert_eq!(site("https://www.facebook.com/x"), "facebook.com");
        assert_eq!(site("https://m.facebook.com/x"), site("https://facebook.com/x"));
        assert_eq!(site("https://fb.watch/x"), "fb.watch");
        assert_ne!(site("https://facebook.com.example.net/x"), "facebook.com");
    }

    /// A file address as fbcdn writes it, its encoding in `efg`.
    fn file(name: &str, tag: &str) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let json = format!("{{\"vencode_tag\":\"{tag}\",\"duration_s\":9}}");
        let bits: Vec<u8> = json.bytes().flat_map(|b| (0..8).rev().map(move |i| b >> i & 1)).collect();
        let mut efg: String = bits.chunks(6).map(|c| ALPHABET[c.iter().fold(0, |n, b| n << 1 | b) as usize * (1 << (6 - c.len()))] as char).collect();
        while efg.len() % 4 != 0 {
            efg.push('=');
        }
        format!("https://video.fhan1-1.fna.fbcdn.net/o1/v/t2/{name}.mp4?_nc_cat=1&efg={}&oh=x", efg.replace('=', "%3D").replace('+', "%2B").replace('/', "%2F"))
    }

    const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD><Period id="0"><AdaptationSet id="0" contentType="video" frameRate="15360/512" par="9:16"><Representation id="111v" bandwidth="648250" codecs="avc1.4d001e" mimeType="video/mp4" width="360" height="640"><BaseURL>https://video.fhan1-1.fna.fbcdn.net/o1/v/t2/f2/m366/a.mp4?_nc_cat=111&amp;oh=00_A</BaseURL><SegmentBase indexRange="887-966"><Initialization range="0-886"/></SegmentBase></Representation><Representation id="222v" bandwidth="935000" codecs="vp09.00.30.08" mimeType="video/mp4" width="720" height="1280" frameRate="30000/1001"><BaseURL>https://video.fhan1-1.fna.fbcdn.net/o1/v/t2/f2/m366/b.mp4</BaseURL></Representation></AdaptationSet><AdaptationSet id="1" contentType="audio"><Representation id="333a" bandwidth="44585" codecs="mp4a.40.5" mimeType="audio/mp4"><BaseURL>https://video.fhan1-1.fna.fbcdn.net/o1/v/t2/f2/m69/c.mp4</BaseURL></Representation><Representation id="444a" bandwidth="96000" codecs="mp4a.40.2" mimeType="audio/mp4"><BaseURL>https://video.fhan1-1.fna.fbcdn.net/o1/v/t2/f2/m69/d.mp4</BaseURL></Representation></AdaptationSet></Period></MPD>"#;

    fn page(title: &str, scripts: &[Value]) -> String {
        let scripts: String = scripts.iter().map(|s| format!("<script type=\"application/json\" data-content-len=\"9\" data-sjs>{s}</script>")).collect();
        format!("<html><head><title>{title}</title><meta name=\"description\" content=\"When your partner &#x201c;helps&#x201d; &amp; more\nsecond line\" /></head><body>{scripts}</body></html>")
    }

    #[test]
    fn each_dash_picture_is_joined_to_the_best_sound_and_the_files_carry_theirs() {
        let other = json!({ "id": "999", "width": 576, "height": 1024, "videoDeliveryLegacyFields": { "browser_native_sd_url": file("other", "x.C3.360.y") } });
        let video = json!({
            "__typename": "Video", "id": "1491046195556859", "width": 720, "height": 1280,
            "videoDeliveryLegacyFields": {
                "id": "1491046195556859",
                "dash_manifest_xml_string": MANIFEST,
                "browser_native_hd_url": file("hd", "xpv_progressive.FACEBOOK..C3.720.dash_h264-basic-gen2_720p"),
                "browser_native_sd_url": file("sd", "xpv_progressive.FACEBOOK..C3.360.dash_baseline_3_v1"),
            },
            "videoDeliveryResponseFragment": null,
        });
        let html = page("Facebook", &[json!({ "require": [["ScheduledServerJS", "handle", null, [{ "__bbox": { "lasso": [other] } }]]] }), json!({ "data": { "video": { "story": { "attachments": [{ "media": video }] } } } })]);
        let got = video_of(&html, Some("1491046195556859"), false).unwrap();
        assert_eq!(got.title, "When your partner \u{201c}helps\u{201d} & more", "the description's first line when the title is only the site's");
        let ids: Vec<&str> = got.variants.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["111v+444a", "222v+444a", "hd", "sd", "444a"], "the video of the page's own id, not the one before it");
        let joined = &got.variants[0];
        assert_eq!((joined.height, joined.fps, joined.h264), (Some(640), 30, true));
        assert_eq!(joined.video.as_ref().unwrap().url, "https://video.fhan1-1.fna.fbcdn.net/o1/v/t2/f2/m366/a.mp4?_nc_cat=111&oh=00_A");
        assert_eq!(joined.audio.as_ref().unwrap().ext, "m4a");
        assert_eq!((got.variants[1].fps, got.variants[1].h264), (30, false), "VP9 at 30000/1001");
        let hd = &got.variants[2];
        assert_eq!((hd.height, hd.audio.as_ref(), hd.ext().as_str()), (Some(1280), None, "mp4"), "a file carries its own sound");
        assert_eq!(got.variants[3].height, Some(640));
        assert_eq!(hd.video.as_ref().unwrap().headers, [("User-Agent".to_owned(), CDN_AGENT.to_owned())], "never the page's cookie");
        assert!(got.variants.iter().all(|v| crate::ytdlp::valid_format(&format!("{}{}", super::super::PREFIX, v.id))));
        let first = video_of(&html, None, false).unwrap();
        assert_eq!(first.variants.len(), 1, "with no number, the page's first video");
    }

    #[test]
    fn the_newer_answer_and_a_title_of_its_own_are_read_too() {
        let video = json!({
            "videoId": "42", "width": 1920, "height": 1080, "videoDeliveryLegacyFields": null,
            "videoDeliveryResponseFragment": { "videoDeliveryResponseResult": {
                "dash_manifests": [{ "manifest_xml": MANIFEST }],
                "progressive_urls": [{ "progressive_url": file("hd", "xpv_progressive.FACEBOOK..C3.1280.hd"), "metadata": { "quality": "HD" } }],
            }},
        });
        let got = video_of(&page("How to share with just friends.", &[json!({ "data": { "video": video } })]), Some("42"), true).unwrap();
        assert_eq!(got.title, "How to share with just friends.");
        let hd = got.variants.iter().find(|v| v.id == "hd").unwrap();
        assert_eq!(hd.height, Some(720), "1280 wide at 16:9");
        assert_eq!(got.variants.len(), 4);
    }

    #[test]
    fn a_page_without_the_video_says_why() {
        let html = page("Facebook", &[json!({ "data": { "viewer": {} } })]);
        assert_eq!(video_of(&html, Some("1"), false).unwrap_err(), crate::ytdlp::SIGN_IN, "signed out: perhaps for friends only");
        assert_ne!(video_of(&html, Some("1"), true).unwrap_err(), crate::ytdlp::SIGN_IN);
        let wall = format!("{html}<form id=\"login_form\"></form>");
        assert_eq!(video_of(&wall, Some("1"), true).unwrap_err(), crate::ytdlp::SIGN_IN, "a session that has run out");
    }

    #[test]
    fn escapes_are_undone() {
        assert_eq!(entities("2.8M views &#xb7; 1.3K &amp; &#39;x&#39; &bogus; & a&lt;b"), "2.8M views \u{b7} 1.3K & 'x' &bogus; & a<b");
        assert_eq!(base64("eyJhIjoxfQ=="), b"{\"a\":1}");
    }

    /// Live: `cargo test facebook_live -- --ignored`, against a public video, its best file asked for a range.
    #[test]
    #[ignore]
    fn facebook_live() {
        let page = std::env::var("LANES_FB").unwrap_or_else(|_| "https://www.facebook.com/watch/?v=10153231379946729".into());
        let video = read(&page, "").expect("the page is read");
        println!("{}", video.title);
        for v in &video.variants {
            println!("{} {:?} fps={} h264={} sound={}", v.id, v.height, v.fps, v.h264, v.audio.is_some());
        }
        let choice = &super::super::choices(&video)[0];
        let variant = video.variants.iter().find(|v| format!("{}{}", super::super::PREFIX, v.id) == choice.format).unwrap();
        for stream in variant.streams() {
            let mut request = crate::engine::agent(None).get(&stream.url).set("Range", "bytes=0-1023");
            for (name, value) in &stream.headers {
                request = request.set(name, value);
            }
            let response = request.call().expect("the stream answers");
            println!("{} -> {} {:?}", variant.id, response.status(), response.header("Content-Range"));
            assert_eq!(response.status(), 206);
        }
    }
}
