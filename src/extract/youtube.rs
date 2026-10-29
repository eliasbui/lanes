//! YouTube, read the way its own apps ask for a video. The watch page gives the visitor id YouTube wants with each
//! request; the player API, asked as the visionOS app, answers with plain addresses for every stream up to 4K, with no
//! script of the player's to run on them. googlevideo serves those addresses fast only in pieces of at most 10 MiB
//! (a larger request is held to well under 1 MB/s), which is the engine's piece size.
//!
//! The visionOS app is not given a video for signed-in viewers only (an age check, members, private). With the
//! browser's YouTube session, such a video is asked for again as the Samsung TV app, signed in: the API is told who by
//! the session's cookies and a SAPISIDHASH, and its addresses carry a signature and an `n` value that the player's own
//! script transforms (jsc.rs). Without a session it says so.

use super::{Stream, Variant, Video, jsc};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};

/// The browser the watch page is asked as, and the consent YouTube would otherwise ask for first.
const PAGE_AGENT: &str = super::BROWSER_AGENT;
const PAGE_COOKIE: &str = "PREF=hl=en&tz=UTC; SOCS=CAI";
/// The visionOS app, as yt-dlp's `visionos` client describes it (yt_dlp/extractor/youtube/_base.py).
const CLIENT_NAME: &str = "VISIONOS";
const CLIENT_ID: &str = "101";
const CLIENT_VERSION: &str = "1.02";
const CLIENT_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";
const PLAYER_API: &str = "https://www.youtube.com/youtubei/v1/player?prettyPrint=false";
/// The Samsung TV app, yt-dlp's `tv_samsung`, which takes a signed-in session.
const TV_NAME: &str = "TVHTML5";
const TV_ID: &str = "7";
const TV_VERSION: &str = "5.20260707";
const TV_AGENT: &str = "Mozilla/5.0 (SMART-TV; Linux; Tizen 2.4.0) AppleWebKit/538.1 (KHTML, like Gecko) Version/2.4.0 TV Safari/538.1";
const ORIGIN: &str = "https://www.youtube.com";

/// The 11-character id of the video at `url`: a watch page, a short, an embed, a live page or a youtu.be link.
pub fn video_id(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    let candidate = if host == "youtu.be" {
        parsed.path_segments()?.next()?.to_owned()
    } else if let Some((_, id)) = parsed.query_pairs().find(|(k, _)| k == "v") {
        id.into_owned()
    } else {
        let mut segments = parsed.path_segments()?;
        match segments.next()? {
            "shorts" | "embed" | "live" | "v" => segments.next()?.to_owned(),
            _ => return None,
        }
    };
    (candidate.len() == 11 && candidate.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')).then_some(candidate)
}

pub fn read(page: &str, cookie: &str) -> Result<Video, String> {
    let id = video_id(page).ok_or("Hãy mở video này ra trang riêng của nó để tải.")?;
    match read_as_app(&id) {
        Err(e) if e == crate::ytdlp::SIGN_IN && signed_in(cookie) => read_signed_in(&id, cookie),
        other => other,
    }
}

fn watch_page(id: &str, cookie: &str) -> Result<String, String> {
    super::get_text(
        &format!("https://www.youtube.com/watch?v={id}&bpctr=9999999999&has_verified=1"),
        &[("User-Agent".into(), PAGE_AGENT.into()), ("Cookie".into(), cookie.into())],
    )
}

fn read_as_app(id: &str) -> Result<Video, String> {
    let watch = watch_page(id, PAGE_COOKIE)?;
    let visitor = super::json_string_after(&watch, "VISITOR_DATA").ok_or("YouTube không trả mã khách, hãy thử lại")?;
    let player = super::post_json(PLAYER_API, &api_headers(&visitor), &request(id))?;
    video_of(&player)
}

/// The value of one cookie in a Cookie header.
fn cookie_value<'a>(cookie: &'a str, name: &str) -> Option<&'a str> {
    cookie.split(';').filter_map(|pair| pair.trim().split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| v).filter(|v| !v.is_empty())
}

/// Whether the cookies are those of a signed-in YouTube session, the way yt-dlp tells.
fn signed_in(cookie: &str) -> bool {
    cookie_value(cookie, "LOGIN_INFO").is_some() && ["SAPISID", "__Secure-1PAPISID", "__Secure-3PAPISID"].iter().any(|n| cookie_value(cookie, n).is_some())
}

/// The Authorization the API wants from a signed-in session: for each of the session's API ids, a SHA-1 of the time,
/// the id and the origin (with the user's session id before them when there is one), as YouTube's own pages send it.
fn authorization(cookie: &str, user_session: Option<&str>, now: u64) -> Option<String> {
    let sapisid = cookie_value(cookie, "SAPISID").or_else(|| cookie_value(cookie, "__Secure-3PAPISID"));
    let schemes = [("SAPISIDHASH", sapisid), ("SAPISID1PHASH", cookie_value(cookie, "__Secure-1PAPISID")), ("SAPISID3PHASH", cookie_value(cookie, "__Secure-3PAPISID"))];
    let parts: Vec<String> = schemes
        .iter()
        .filter_map(|(scheme, sid)| {
            let sid = (*sid)?;
            let mut text = user_session.map(|u| format!("{u} ")).unwrap_or_default();
            text += &format!("{now} {sid} {ORIGIN}");
            let hash: String = Sha1::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
            let tail = user_session.map(|u| format!("_{u}")).unwrap_or_default();
            Some(format!("{scheme} {now}_{hash}{tail}"))
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// A signed-in viewer's video: the watch page with the session gives the visitor id, the session ids and the version
/// of the player; the TV app's answer is solved with the player's script.
fn read_signed_in(id: &str, cookie: &str) -> Result<Video, String> {
    let watch = watch_page(id, cookie)?;
    let visitor = super::json_string_after(&watch, "VISITOR_DATA").ok_or("YouTube không trả mã khách, hãy thử lại")?;
    let script = super::json_string_after(&watch, "PLAYER_JS_URL").or_else(|| super::json_string_after(&watch, "jsUrl")).ok_or("Không thấy trình phát của YouTube")?;
    let sts = number_after(&watch, "STS").ok_or("Không thấy phiên bản trình phát của YouTube")?;
    let sync = super::json_string_after(&watch, "DATASYNC_ID").unwrap_or_default();
    let (delegated, user) = match sync.split_once("||") {
        Some((first, second)) if !second.is_empty() => (Some(first.to_owned()), Some(second.to_owned())),
        Some((first, _)) => (None, Some(first.to_owned())),
        None => (None, (!sync.is_empty()).then(|| sync.clone())),
    };
    let user = super::json_string_after(&watch, "USER_SESSION_ID").or(user);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mut headers = vec![
        ("User-Agent".to_owned(), TV_AGENT.to_owned()),
        ("X-YouTube-Client-Name".into(), TV_ID.into()),
        ("X-YouTube-Client-Version".into(), TV_VERSION.into()),
        ("Origin".into(), ORIGIN.into()),
        ("X-Goog-Visitor-Id".into(), visitor),
        ("Cookie".into(), cookie.into()),
        ("X-Goog-AuthUser".into(), number_after(&watch, "SESSION_INDEX").unwrap_or(0).to_string()),
    ];
    if let Some(delegated) = delegated {
        headers.push(("X-Goog-PageId".into(), delegated));
    }
    if let Some(auth) = authorization(cookie, user.as_deref(), now) {
        headers.push(("Authorization".into(), auth));
        headers.push(("X-Origin".into(), ORIGIN.into()));
    }
    if watch.contains("\"LOGGED_IN\":true") {
        headers.push(("X-Youtube-Bootstrap-Logged-In".into(), "true".into()));
    }
    let body = json!({
        "context": { "client": { "clientName": TV_NAME, "clientVersion": TV_VERSION, "deviceMake": "Samsung", "deviceModel": "UKS9800", "userAgent": TV_AGENT, "osName": "Tizen", "osVersion": "2.4.0", "hl": "en", "timeZone": "UTC", "utcOffsetMinutes": 0 } },
        "videoId": id,
        "playbackContext": { "contentPlaybackContext": { "html5Preference": "HTML5_PREF_WANTS", "signatureTimestamp": sts } },
        "contentCheckOk": true,
        "racyCheckOk": true,
    });
    let mut player = super::post_json(PLAYER_API, &headers, &body)?;
    solve_addresses(&mut player, &script)?;
    let mut video = video_of(&player)?;
    for stream in video.variants.iter_mut().flat_map(|v| [&mut v.video, &mut v.audio]).flatten() {
        stream.headers = vec![("User-Agent".into(), TV_AGENT.into())];
    }
    Ok(video)
}

/// The number after `"key":` in a page, such as `"STS":20753`.
fn number_after(text: &str, key: &str) -> Option<u64> {
    let start = text.find(&format!("\"{key}\":"))? + key.len() + 3;
    let digits: String = text[start..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The player's version id in its address (`/s/player/<id>/...`), which names the prepared player in the cache.
fn player_id(script: &str) -> Option<&str> {
    script.split("/s/player/").nth(1)?.split('/').next().filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// Puts a plain address on every format: the signature of a `signatureCipher` and the `n` of every address, solved by
/// the player's script (jsc.rs), all in one run.
fn solve_addresses(player: &mut Value, script: &str) -> Result<(), String> {
    let id = player_id(script).ok_or("Không đọc được phiên bản trình phát của YouTube")?.to_owned();
    let (mut sigs, mut ns) = (Vec::new(), Vec::new());
    each_format(player, |f| {
        let (url, cipher) = address(f);
        sigs.extend(cipher.map(|c| c.s));
        ns.extend(url.as_deref().and_then(|u| query(u, "n")));
        Ok(())
    })?;
    sigs.sort();
    sigs.dedup();
    ns.sort();
    ns.dedup();
    let page = format!("{ORIGIN}{script}");
    let answers = jsc::solve(&id, || super::get_text(&page, &[("User-Agent".into(), PAGE_AGENT.into())]), &[("n", ns), ("sig", sigs)])?;
    each_format(player, |f| {
        let (Some(url), cipher) = address(f) else { return Ok(()) };
        let mut url = url::Url::parse(&url).map_err(|e| e.to_string())?;
        let pairs: Vec<(String, String)> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        let mut query = url.query_pairs_mut();
        query.clear();
        for (k, v) in pairs {
            let v = if k == "n" { answers.get("n").and_then(|a| a.get(&v)).cloned().ok_or("Không giải được mã n của YouTube")? } else { v };
            query.append_pair(&k, &v);
        }
        if let Some(cipher) = cipher {
            let sig = answers.get("sig").and_then(|a| a.get(&cipher.s)).ok_or("Không giải được chữ ký của YouTube")?;
            query.append_pair(&cipher.sp, sig);
        }
        drop(query);
        f["url"] = json!(url.to_string());
        Ok(())
    })
}

fn each_format(player: &mut Value, mut each: impl FnMut(&mut Value) -> Result<(), String>) -> Result<(), String> {
    for key in ["formats", "adaptiveFormats"] {
        for format in player["streamingData"][key].as_array_mut().into_iter().flatten() {
            each(format)?;
        }
    }
    Ok(())
}

struct Cipher {
    s: String,
    sp: String,
}

/// A format's address and, when it has a `signatureCipher`, the signature to solve and the name it goes under.
fn address(f: &Value) -> (Option<String>, Option<Cipher>) {
    if let Some(url) = f["url"].as_str() {
        return (Some(url.to_owned()), None);
    }
    let Some(cipher) = f["signatureCipher"].as_str() else { return (None, None) };
    let pairs: Vec<(String, String)> = url::form_urlencoded::parse(cipher.as_bytes()).map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    let get = |key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    match (get("url"), get("s")) {
        (Some(url), Some(s)) => (Some(url), Some(Cipher { s, sp: get("sp").unwrap_or_else(|| "signature".into()) })),
        _ => (None, None),
    }
}

fn query(url: &str, key: &str) -> Option<String> {
    url::Url::parse(url).ok()?.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned())
}

fn api_headers(visitor: &str) -> Vec<(String, String)> {
    [
        ("User-Agent", CLIENT_AGENT),
        ("X-YouTube-Client-Name", CLIENT_ID),
        ("X-YouTube-Client-Version", CLIENT_VERSION),
        ("Origin", "https://www.youtube.com"),
        ("X-Goog-Visitor-Id", visitor),
    ]
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .into()
}

fn request(id: &str) -> Value {
    json!({
        "context": { "client": {
            "clientName": CLIENT_NAME, "clientVersion": CLIENT_VERSION, "deviceMake": "Apple", "deviceModel": "RealityDevice17,1",
            "userAgent": CLIENT_AGENT, "osName": "visionOS", "osVersion": "26.5.23O471", "hl": "en", "timeZone": "UTC", "utcOffsetMinutes": 0,
        }},
        "videoId": id,
        "contentCheckOk": true,
        "racyCheckOk": true,
    })
}

/// The video in the player API's answer: each picture joined to the best AAC sound, the formats that carry their own
/// sound, and the sound alone.
pub fn video_of(player: &Value) -> Result<Video, String> {
    let status = &player["playabilityStatus"];
    match status["status"].as_str() {
        Some("OK") => {}
        Some("LOGIN_REQUIRED") | Some("AGE_CHECK_REQUIRED") | Some("CONTENT_CHECK_REQUIRED") => return Err(crate::ytdlp::SIGN_IN.into()),
        _ => return Err(status["reason"].as_str().unwrap_or("YouTube không cho xem video này").to_owned()),
    }
    if player["videoDetails"]["isLive"].as_bool() == Some(true) {
        return Err("Chưa tải được video đang phát trực tiếp".into());
    }
    let data = &player["streamingData"];
    let list = |key: &str| data[key].as_array().cloned().unwrap_or_default();
    let adaptive: Vec<Format> = list("adaptiveFormats").iter().filter_map(Format::of).collect();
    let sound = adaptive.iter().filter(|f| !f.video && f.ext == "m4a").max_by_key(|f| (f.default_track, f.bitrate));
    let mut variants = Vec::new();
    for f in adaptive.iter().filter(|f| f.video) {
        if let Some(a) = sound {
            variants.push(Variant { id: format!("{}+{}", f.itag, a.itag), height: f.height, fps: f.fps, h264: f.h264, video: Some(f.stream()), audio: Some(a.stream()) });
        }
    }
    for f in list("formats").iter().filter_map(Format::of) {
        variants.push(Variant { id: f.itag.to_string(), height: f.height, fps: f.fps, h264: f.h264, video: Some(f.stream()), audio: None });
    }
    if let Some(a) = sound {
        variants.push(Variant { id: a.itag.to_string(), audio: Some(a.stream()), ..Variant::default() });
    }
    Ok(Video { title: player["videoDetails"]["title"].as_str().unwrap_or("YouTube").to_owned(), variants })
}

/// One format of the answer, as far as the variants need it.
struct Format {
    itag: u64,
    url: String,
    ext: String,
    video: bool,
    h264: bool,
    height: Option<u32>,
    fps: u32,
    size: Option<u64>,
    bitrate: u64,
    /// The original sound of a video dubbed into other languages, or the only one.
    default_track: bool,
}

impl Format {
    fn of(f: &Value) -> Option<Format> {
        // A format without a plain address needs the player's script, which this client does not.
        let url = f["url"].as_str()?.to_owned();
        let mime = f["mimeType"].as_str().unwrap_or_default();
        let (kind, codecs) = mime.split_once(';').unwrap_or((mime, ""));
        let ext = match kind.trim() {
            "video/mp4" => "mp4",
            "audio/mp4" => "m4a",
            "video/webm" => "webm",
            "audio/webm" => "webm",
            _ => return None,
        };
        // Sound made quieter for small speakers is not what anyone means to download.
        if f["isDrc"].as_bool() == Some(true) {
            return None;
        }
        Some(Format {
            itag: f["itag"].as_u64()?,
            url,
            ext: ext.into(),
            video: kind.trim().starts_with("video/"),
            h264: codecs.contains("avc1"),
            height: f["height"].as_u64().map(|h| h as u32),
            fps: f["fps"].as_u64().unwrap_or(0) as u32,
            size: f["contentLength"].as_str().and_then(|s| s.parse().ok()),
            bitrate: f["bitrate"].as_u64().unwrap_or(0),
            default_track: f["audioTrack"]["audioIsDefault"].as_bool().unwrap_or(true),
        })
    }

    fn stream(&self) -> Stream {
        Stream { url: self.url.clone(), hls: false, size: self.size, ext: self.ext.clone(), headers: vec![("User-Agent".into(), CLIENT_AGENT.into())] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_id_is_found_in_every_kind_of_youtube_link() {
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=1",
            "https://m.youtube.com/watch?feature=x&v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ?si=a",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ",
            "https://www.youtube.com/embed/dQw4w9WgXcQ",
            "https://www.youtube.com/live/dQw4w9WgXcQ",
        ] {
            assert_eq!(video_id(url).as_deref(), Some("dQw4w9WgXcQ"), "{url}");
        }
        assert_eq!(video_id("https://www.youtube.com/"), None);
        assert_eq!(video_id("https://www.youtube.com/watch?v=short"), None);
    }

    fn answer() -> Value {
        let f = |itag: u64, mime: &str, height: Option<u64>, fps: u64, size: &str, extra: Value| {
            let mut f = json!({ "itag": itag, "url": format!("https://rr.googlevideo.com/videoplayback?itag={itag}"), "mimeType": mime, "fps": fps, "contentLength": size, "bitrate": itag * 10 });
            if let Some(h) = height {
                f["height"] = json!(h);
            }
            if let (Some(f), Some(extra)) = (f.as_object_mut(), extra.as_object()) {
                f.extend(extra.clone());
            }
            f
        };
        json!({
            "playabilityStatus": { "status": "OK" },
            "videoDetails": { "title": "Big Buck Bunny", "isLive": false },
            "streamingData": {
                "formats": [f(18, "video/mp4; codecs=\"avc1.42001E, mp4a.40.2\"", Some(360), 30, "28", json!({}))],
                "adaptiveFormats": [
                    f(299, "video/mp4; codecs=\"avc1.64002A\"", Some(1080), 60, "300", json!({})),
                    f(303, "video/webm; codecs=\"vp9\"", Some(1080), 60, "200", json!({})),
                    f(140, "audio/mp4; codecs=\"mp4a.40.2\"", None, 0, "10", json!({})),
                    f(141, "audio/mp4; codecs=\"mp4a.40.2\"", None, 0, "12", json!({ "isDrc": true })),
                    f(139, "audio/mp4; codecs=\"mp4a.40.5\"", None, 0, "5", json!({ "audioTrack": { "audioIsDefault": false } })),
                    f(251, "audio/webm; codecs=\"opus\"", None, 0, "11", json!({})),
                    json!({ "itag": 137, "signatureCipher": "s=x&url=y", "mimeType": "video/mp4; codecs=\"avc1\"", "height": 1080 }),
                ],
            },
        })
    }

    #[test]
    fn each_picture_is_joined_to_the_best_aac_sound_of_the_original_track() {
        let video = video_of(&answer()).unwrap();
        assert_eq!(video.title, "Big Buck Bunny");
        let ids: Vec<&str> = video.variants.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["299+140", "303+140", "18", "140"], "no DRC sound, no dub, no ciphered address");
        let joined = &video.variants[0];
        assert_eq!((joined.height, joined.fps, joined.h264, joined.size()), (Some(1080), 60, true, Some(310)));
        assert_eq!(joined.video.as_ref().unwrap().headers, [("User-Agent".to_owned(), CLIENT_AGENT.to_owned())], "the address is asked for as the app that got it");
        assert_eq!(video.variants[1].ext(), "mp4", "VP9 and AAC are joined into an MP4");
        assert_eq!(video.variants[3].ext(), "m4a");
    }

    #[test]
    fn a_signed_in_session_signs_its_api_requests_as_youtube_does() {
        let cookie = "PREF=x; LOGIN_INFO=a; SAPISID=sid1; __Secure-3PAPISID=sid3";
        assert!(signed_in(cookie));
        assert!(!signed_in("SAPISID=sid1"), "no LOGIN_INFO: signed out, whatever else is left");
        let auth = authorization(cookie, None, 1700000000).unwrap();
        let hash = |text: &str| Sha1::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(auth, format!("SAPISIDHASH 1700000000_{} SAPISID3PHASH 1700000000_{}", hash("1700000000 sid1 https://www.youtube.com"), hash("1700000000 sid3 https://www.youtube.com")));
        let with_user = authorization("SAPISID=s", Some("42"), 1).unwrap();
        assert_eq!(with_user, format!("SAPISIDHASH 1_{}_42", hash("42 1 s https://www.youtube.com")));
        assert_eq!(number_after(r#"{"STS":20753,"x":1}"#, "STS"), Some(20753));
        assert_eq!(player_id("/s/player/11a45ffb/player_es6.vflset/en_US/base.js"), Some("11a45ffb"));
    }

    #[test]
    fn a_cipher_gives_its_address_its_signature_and_the_name_it_goes_under() {
        let f = json!({ "signatureCipher": "s=AB%3DC&sp=sig&url=https%3A%2F%2Fr.googlevideo.com%2Fvideoplayback%3Fn%3Dxyz" });
        let (url, cipher) = address(&f);
        let cipher = cipher.unwrap();
        assert_eq!((url.as_deref(), cipher.s.as_str(), cipher.sp.as_str()), (Some("https://r.googlevideo.com/videoplayback?n=xyz"), "AB=C", "sig"));
        assert_eq!(query(url.as_deref().unwrap(), "n").as_deref(), Some("xyz"));
    }

    /// The signed-in path (the TV app's answer, solved by the player's script in QuickJS) taken with the visitor cookies
    /// a watch page sets instead of a session, on a public video. `cargo test tv_addresses -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn tv_addresses_are_solved_and_served_fast() {
        let id = "aqz-KE-bpKQ";
        let page = crate::engine::agent(None).get(&format!("https://www.youtube.com/watch?v={id}")).set("User-Agent", PAGE_AGENT).set("Cookie", PAGE_COOKIE).call().unwrap();
        let set: Vec<String> = page.all("set-cookie").iter().filter_map(|c| c.split(';').next().map(str::to_owned)).collect();
        let cookie = format!("{PAGE_COOKIE}; {}", set.join("; "));
        let started = std::time::Instant::now();
        let video = read_signed_in(id, &cookie).unwrap();
        println!("read and solved in {:.1} s, {} variants", started.elapsed().as_secs_f64(), video.variants.len());
        let stream = video.variants.iter().find(|v| v.height == Some(1080)).and_then(|v| v.video.clone()).unwrap();
        let started = std::time::Instant::now();
        let response = crate::engine::agent(None).get(&stream.url).set("User-Agent", TV_AGENT).set("Range", "bytes=0-10485759").call().unwrap();
        assert_eq!(response.status(), 206);
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut response.into_reader(), &mut bytes).unwrap();
        println!("{} bytes in {:.1} s", bytes.len(), started.elapsed().as_secs_f64());
        assert_eq!(bytes.len(), 10485760);
    }

    #[test]
    fn a_video_for_signed_in_viewers_says_so() {
        let age = json!({ "playabilityStatus": { "status": "LOGIN_REQUIRED", "reason": "Sign in to confirm your age" } });
        assert_eq!(video_of(&age).unwrap_err(), crate::ytdlp::SIGN_IN);
        let gone = json!({ "playabilityStatus": { "status": "ERROR", "reason": "Video unavailable" } });
        assert_eq!(video_of(&gone).unwrap_err(), "Video unavailable");
        let live = json!({ "playabilityStatus": { "status": "OK" }, "videoDetails": { "isLive": true } });
        assert!(video_of(&live).is_err());
    }
}
