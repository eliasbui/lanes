//! TikTok, read from the video's own page the way a browser opens it (yt-dlp's `TikTokIE`, web path). The page carries
//! the video's data in its `__UNIVERSAL_DATA_FOR_REHYDRATION__` script. A first visit may get a "Please wait..." page
//! instead, a proof of work: a SHA-256 to find a number for, whose answer goes back as a cookie and the page comes
//! whole. Neither needs a browser's TLS fingerprint; plain HTTP/1.1 from ureq gets the same pages as Chrome.
//!
//! The video CDN answers only a request with the `tt_chain_token` cookie the page's answer set (the stream addresses
//! say `tk=tt_chain_token`) and a TikTok Referer. That one cookie, set by TikTok for this reading, goes in the stream's
//! headers; none of the user's cookies do.

use base64::Engine;
use super::{Stream, Variant, Video};
use serde_json::Value;
use sha2::{Digest, Sha256};

const AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36";
const REFERER: &str = "https://www.tiktok.com/";
/// The script that holds the page's data.
const DATA: &str = "id=\"__UNIVERSAL_DATA_FOR_REHYDRATION__\"";
const NOT_A_VIDEO: &str = "Hãy mở video này ra trang riêng của nó để tải.";

/// The id of the video at `url`: a /@user/video/ID page, a share or embed link.
pub fn video_id(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let segments: Vec<&str> = parsed.path_segments()?.collect();
    segments.windows(2).find_map(|w| {
        let id = w[1].trim_end_matches(".html");
        (matches!(w[0], "video" | "embed" | "v2" | "v") && !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then(|| id.to_owned())
    })
}

pub fn read(page: &str, cookie: &str) -> Result<Video, String> {
    let id = match video_id(page) {
        Some(id) => id,
        // A vm.tiktok.com, vt.tiktok.com or /t/ short link leads to the video's page.
        None => video_id(&followed(page)?).ok_or(NOT_A_VIDEO)?,
    };
    let url = format!("https://www.tiktok.com/@_/video/{id}");
    let (mut html, mut set) = fetch(&url, cookie)?;
    if !html.contains(DATA) {
        let answer = solve(&html).ok_or("TikTok chặn trình đọc của Lanes, hãy thử lại sau")?;
        let cookie = if cookie.is_empty() { answer } else { format!("{cookie}; {answer}") };
        (html, set) = fetch(&url, &cookie)?;
    }
    let data = super::json_object_after(&html, DATA).ok_or("TikTok không trả dữ liệu video, hãy thử lại")?;
    let chain = set.iter().find_map(|c| c.strip_prefix("tt_chain_token=")).and_then(|c| c.split(';').next()).filter(|c| !c.is_empty());
    video_of(&data, &id, chain.ok_or("TikTok không cấp mã tải video, hãy thử lại")?)
}

/// Where a short link leads.
pub(super) fn followed(url: &str) -> Result<String, String> {
    match crate::engine::agent(None).head(url).set("User-Agent", "facebookexternalhit/1.1").call() {
        Ok(response) => Ok(response.get_url().to_owned()),
        Err(ureq::Error::Status(code, _)) => Err(format!("Máy chủ trả mã {code}")),
        Err(e) => Err(format!("Không kết nối được: {e}")),
    }
}

/// The page and the cookies its answer sets.
fn fetch(url: &str, cookie: &str) -> Result<(String, Vec<String>), String> {
    let mut request = crate::engine::agent(None).get(url).set("User-Agent", AGENT);
    if !cookie.is_empty() {
        request = request.set("Cookie", cookie);
    }
    let response = request.call();
    let mut set = Vec::new();
    if let Ok(answer) = &response {
        if url::Url::parse(answer.get_url()).is_ok_and(|u| u.path() == "/login") {
            return Err(crate::ytdlp::SIGN_IN.into());
        }
        set = answer.all("set-cookie").into_iter().map(str::to_owned).collect();
    }
    Ok((super::body(response)?, set))
}

/// The cookie that answers the "Please wait..." page, as its script would: the number `d` whose decimal digits,
/// after the bytes of `v.a`, hash to `v.c`; the challenge with `d` added goes back under the name the page gives.
fn solve(page: &str) -> Option<String> {
    let challenge = String::from_utf8(base64_decode(class_of(page, "cs")?)?).ok()?;
    let json: Value = serde_json::from_str(&challenge).ok()?;
    let want = base64_decode(json["v"]["c"].as_str()?)?;
    let base = Sha256::new_with_prefix(base64_decode(json["v"]["a"].as_str()?)?);
    let n = (0..=1_000_000u32).find(|n| base.clone().chain_update(n.to_string()).finalize().as_slice() == want)?;
    let answered = format!("{},\"d\":\"{}\"}}", challenge.trim_end().strip_suffix('}')?, base64_encode(n.to_string().as_bytes()));
    let mut cookie = format!("{}={}", class_of(page, "wci")?, base64_encode(answered.as_bytes()));
    if let (Some(name), Some(value)) = (class_of(page, "rci"), class_of(page, "rs")) {
        cookie += &format!("; {name}={value}");
    }
    Some(cookie)
}

/// The class of the element with `id`, where the challenge page keeps its values; None when empty.
fn class_of<'a>(page: &'a str, id: &str) -> Option<&'a str> {
    let at = page.find(&format!(" id=\"{id}\""))?;
    let start = page[..at].rfind('<')?;
    let tag = &page[start..at + page[at..].find('>')?];
    let value = tag.split(" class=\"").nth(1)?.split('"').next()?;
    (!value.is_empty()).then_some(value)
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim_end_matches('=').replace('-', "+").replace('_', "/");
    base64::engine::general_purpose::STANDARD_NO_PAD.decode(text).ok()
}

fn base64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The video in the page's data. `chain` is the `tt_chain_token` the page's answer set, which the CDN wants back.
pub fn video_of(data: &Value, id: &str, chain: &str) -> Result<Video, String> {
    let detail = &data["__DEFAULT_SCOPE__"]["webapp.video-detail"];
    match detail["statusCode"].as_i64() {
        Some(0) => {}
        // A private post or a private account.
        Some(10216 | 10222) => return Err(crate::ytdlp::SIGN_IN.into()),
        // "author_invalid,status_deleted", and posts blocked in the user's country.
        Some(10204) => return Err("Video này không còn trên TikTok hoặc bị chặn ở nơi bạn ở".into()),
        Some(code) => return Err(format!("TikTok không cho xem video này (mã {code})")),
        None => return Err("TikTok trả về trang lạ, hãy thử lại".into()),
    }
    let item = &detail["itemInfo"]["itemStruct"];
    let video = &item["video"];
    if !video.is_object() && item["isContentClassified"].as_bool() == Some(true) {
        return Err(crate::ytdlp::SIGN_IN.into());
    }
    let stream = |url: &str, size: &Value, ext: &str| {
        let mut headers = vec![("User-Agent".to_owned(), AGENT.to_owned()), ("Referer".to_owned(), REFERER.to_owned())];
        if url.contains("tk=tt_chain_token") {
            headers.push(("Cookie".to_owned(), format!("tt_chain_token={chain}")));
        }
        let size = size.as_u64().or_else(|| size.as_str()?.parse().ok());
        Stream { url: url.to_owned(), hls: false, size, ext: ext.to_owned(), headers }
    };
    let mut variants: Vec<Variant> = Vec::new();
    let mut add = |id: String, height: &Value, fps: &Value, codec: &str, url: &str, size: &Value| {
        if variants.iter().any(|v| v.id == id || v.video.as_ref().is_some_and(|s| s.url == url)) {
            return;
        }
        let (height, fps) = (height.as_u64().map(|h| h as u32), fps.as_u64().unwrap_or(0) as u32);
        variants.push(Variant { id, height, fps, h264: codec == "h264", video: Some(stream(url, size, "mp4")), audio: None });
    };
    for info in video["bitrateInfo"].as_array().into_iter().flatten() {
        let (addr, codec) = (&info["PlayAddr"], info["CodecType"].as_str().unwrap_or_default());
        let Some(url) = addr["UrlList"].as_array().into_iter().flatten().filter_map(Value::as_str).find(|u| usable(u)) else { continue };
        // bytevc2 is ByteDance's own H.266, which no player plays.
        if codec == "bytevc2" {
            continue;
        }
        // yt-dlp's format id: `h264_540p_875617` out of `v12044gd..._h264_540p_875617`.
        let key = addr["UrlKey"].as_str().and_then(|k| k.split_once('_')).map(|(_, f)| f).or(info["GearName"].as_str()).unwrap_or("play");
        add(format_id(key), &addr["Height"], &info["BitrateFPS"], codec, url, &addr["DataSize"]);
    }
    if let Some(url) = video["playAddr"].as_str().filter(|u| usable(u)) {
        add("play".into(), &video["height"], &Value::Null, video["codecType"].as_str().unwrap_or("h264"), url, &video["size"]);
    }
    if let Some(url) = item["music"]["playUrl"].as_str().filter(|u| u.starts_with("https://")) {
        let ext = if url.contains("mime_type=audio_mpeg") { "mp3" } else { "m4a" };
        variants.push(Variant { id: "audio".into(), audio: Some(stream(url, &Value::Null, ext)), ..Variant::default() });
    }
    let desc = item["desc"].as_str().unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ");
    Ok(Video { title: if desc.is_empty() { format!("TikTok {id}") } else { desc }, variants })
}

/// A stream address that serves the file: TikTok's own `www.tiktok.com/aweme/v1/play/` ones break (yt-dlp issue
/// 11034), and those under `/media-video-hvc1/` may be a picture without its sound (issues 16622, 17372).
fn usable(url: &str) -> bool {
    url.starts_with("https://") && !url.starts_with("https://www.tiktok.com/") && !url.split('?').next().unwrap_or_default().ends_with("/media-video-hvc1/")
}

pub(super) fn format_id(key: &str) -> String {
    key.chars().map(|c| if c.is_ascii_alphanumeric() || "+-_.".contains(c) { c } else { '_' }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_id_is_found_in_every_kind_of_tiktok_link() {
        for url in [
            "https://www.tiktok.com/@pokemonlife22/video/7059698374567611694?lang=en",
            "https://www.tiktok.com/@/video/7059698374567611694/",
            "https://www.tiktok.com/share/video/7059698374567611694",
            "https://www.tiktok.com/embed/v2/7059698374567611694",
            "https://m.tiktok.com/v/7059698374567611694.html",
        ] {
            assert_eq!(video_id(url).as_deref(), Some("7059698374567611694"), "{url}");
        }
        assert_eq!(video_id("https://vm.tiktok.com/ZTR45GpSF/"), None, "a short link is followed first");
        assert_eq!(video_id("https://www.tiktok.com/@pokemonlife22"), None);
    }

    #[test]
    fn the_challenge_is_answered_as_its_script_would() {
        let seed = b"seed bytes";
        let want = Sha256::new_with_prefix(seed).chain_update("4242").finalize();
        let challenge = format!(r#"{{"v":{{"a":"{}","b":1793250894,"c":"{}"}},"s":"x+/="}}"#, base64_encode(seed), base64_encode(&want));
        let cs = base64_encode(challenge.as_bytes()).trim_end_matches('=').to_owned();
        let page = format!(r#"<body> Please wait... <p id="wci" class="_wafchallengeid"></p> <p id="cs" class="{cs}"></p> <p id="rci" class="waforiginalreid"></p> <p id="rs" class=""></p> <p id="rs_id" class="50"></p></body>"#);
        let cookie = solve(&page).expect("solved");
        let (name, value) = cookie.split_once('=').unwrap();
        assert_eq!(name, "_wafchallengeid");
        assert!(!cookie.contains("waforiginalreid"), "an empty second cookie is not sent");
        let answered = String::from_utf8(base64_decode(value).unwrap()).unwrap();
        assert_eq!(answered, format!(r#"{}","d":"NDI0Mg=="}}"#, challenge.strip_suffix("\"}").unwrap()), "the challenge as given, `d` added last");
        assert_eq!(base64_decode("TWFu").unwrap(), b"Man");
        assert_eq!((base64_encode(b"Ma"), base64_encode(b"M")), ("TWE=".into(), "TQ==".into()));
        assert_eq!(solve("<html>no challenge</html>"), None);
    }

    fn page() -> Value {
        let cdn = |n: &str| format!("https://v16-webapp-prime.tiktok.com/video/tos/{n}/?a=1988&tk=tt_chain_token");
        json!({ "__DEFAULT_SCOPE__": { "webapp.video-detail": { "statusCode": 0, "itemInfo": { "itemStruct": {
            "id": "1", "desc": "Pikachu\n #pokemon ",
            "video": {
                "height": 1024, "codecType": "h264", "size": "658136",
                "playAddr": cdn("h264-540"),
                "bitrateInfo": [
                    { "CodecType": "h264", "BitrateFPS": 30, "GearName": "normal_540_0", "PlayAddr": {
                        "DataSize": "658136", "Height": 1024, "UrlKey": "v12044gd0000c7sh_h264_540p_875617",
                        "UrlList": ["https://www.tiktok.com/aweme/v1/play/?item_id=1", cdn("h264-540"), "https://v19-webapp-prime.tiktok.com/x"] } },
                    { "CodecType": "bytevc1", "BitrateFPS": 30, "PlayAddr": { "DataSize": 900000, "Height": 1920, "UrlKey": "v1_bytevc1_1080p_1200000", "UrlList": [cdn("hevc-1080")] } },
                    { "CodecType": "bytevc2", "PlayAddr": { "Height": 1920, "UrlKey": "v1_bytevc2_1080p_900000", "UrlList": [cdn("vvc")] } },
                    { "CodecType": "bytevc1", "PlayAddr": { "Height": 1280, "UrlKey": "v1_bytevc1_720p_600000", "UrlList": ["https://v16.tiktokcdn.com/x/media-video-hvc1/?a=1"] } },
                    { "CodecType": "h264", "PlayAddr": { "Height": 720, "UrlKey": "v1_h264_720p_1", "UrlList": ["https://www.tiktok.com/aweme/v1/play/?item_id=1"] } },
                ],
            },
            "music": { "playUrl": "https://v16m.tiktokcdn.com/music/?mime_type=video_mp4" },
        }}}}})
    }

    #[test]
    fn each_playable_file_is_a_variant_with_its_own_sound() {
        let video = video_of(&page(), "1", "chain=").unwrap();
        assert_eq!(video.title, "Pikachu #pokemon");
        let ids: Vec<&str> = video.variants.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["h264_540p_875617", "bytevc1_1080p_1200000", "audio"], "no H.266, no silent HEVC, no www.tiktok.com address, the play address once");
        let best = &video.variants[0];
        assert_eq!((best.height, best.fps, best.h264, best.size(), best.audio.is_none()), (Some(1024), 30, true, Some(658136), true));
        let headers = &best.video.as_ref().unwrap().headers;
        assert!(headers.contains(&("Cookie".into(), "tt_chain_token=chain=".into())), "the CDN wants the page's chain token back");
        assert!(headers.contains(&("Referer".into(), REFERER.into())));
        assert!(!video.variants[1].h264);
        let sound = &video.variants[2];
        assert_eq!((sound.height, sound.ext().as_str()), (None, "m4a"));
        assert!(sound.audio.as_ref().unwrap().headers.iter().all(|(name, _)| name != "Cookie"), "only an address that asks for the token gets it");
        assert!(video.variants.iter().all(|v| crate::ytdlp::valid_format(&format!("{}{}", super::super::PREFIX, v.id))));
    }

    #[test]
    fn a_post_tiktok_will_not_show_says_why() {
        let status = |code: i64| json!({ "__DEFAULT_SCOPE__": { "webapp.video-detail": { "statusCode": code, "itemInfo": {} } } });
        assert_eq!(video_of(&status(10216), "1", "c").unwrap_err(), crate::ytdlp::SIGN_IN);
        assert_eq!(video_of(&status(10222), "1", "c").unwrap_err(), crate::ytdlp::SIGN_IN);
        assert!(video_of(&status(10204), "1", "c").unwrap_err().contains("không còn"));
        assert!(video_of(&status(10101), "1", "c").unwrap_err().contains("10101"));
        assert!(video_of(&json!({}), "1", "c").is_err());
        let classified = json!({ "__DEFAULT_SCOPE__": { "webapp.video-detail": { "statusCode": 0, "itemInfo": { "itemStruct": { "isContentClassified": true } } } } });
        assert_eq!(video_of(&classified, "1", "c").unwrap_err(), crate::ytdlp::SIGN_IN);
        let photos = json!({ "__DEFAULT_SCOPE__": { "webapp.video-detail": { "statusCode": 0, "itemInfo": { "itemStruct": { "desc": "" } } } } });
        assert_eq!(video_of(&photos, "7", "c").unwrap(), Video { title: "TikTok 7".into(), variants: vec![] });
    }

    /// Live: `cargo test --quiet extract::tiktok -- --ignored`, or with LANES_TIKTOK=<video url>.
    #[test]
    #[ignore = "needs the network"]
    fn a_public_tiktok_is_read_and_its_cdn_serves_ranges() {
        let page = std::env::var("LANES_TIKTOK").unwrap_or_else(|_| "https://www.tiktok.com/@pokemonlife22/video/7059698374567611694".into());
        let video = read(&page, "").expect("the page is read");
        let choice = super::super::choices(&video).into_iter().next().expect("a choice");
        println!("{}: {:?}, best {} {:?}p {:?} bytes", video.title, video.variants.iter().map(|v| &v.id).collect::<Vec<_>>(), choice.format, choice.height, choice.size);
        let variant = video.variants.iter().find(|v| choice.format == format!("{}{}", super::super::PREFIX, v.id)).unwrap();
        let stream = variant.video.as_ref().unwrap();
        let mut request = crate::engine::agent(None).get(&stream.url).set("Range", "bytes=0-1023");
        for (name, value) in &stream.headers {
            request = request.set(name, value);
        }
        let response = request.call().expect("the CDN answers");
        assert_eq!(response.status(), 206, "{}", response.header("content-range").unwrap_or_default());
        println!("206 {}", response.header("content-range").unwrap_or_default());
        let mut head = Vec::new();
        std::io::Read::read_to_end(&mut response.into_reader(), &mut head).unwrap();
        assert_eq!((head.len(), &head[4..8]), (1024, &b"ftyp"[..]), "the bytes of an MP4");
    }
}
