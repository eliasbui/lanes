//! Instagram, read the way yt-dlp reads it (yt_dlp/extractor/instagram.py). Signed in (a `sessionid` in the browser's
//! cookie), the web app's API answers for the post with its files, the private ones too. Signed out, the post's page
//! carries the same answer in its scripts; when it does not, the GraphQL query the page itself makes is asked, with
//! the page's LSD token. The files and the DASH manifest are Meta's, read as the Facebook module reads them.

use super::facebook::{self, File};
use super::Video;
use serde_json::Value;

/// The web app, as yt-dlp's `_api_headers` name it.
const APP_ID: &str = "936619743392459";
const ASBD_ID: &str = "359341";
/// The query of a post for someone signed out (`PolarisLoggedOutDesktopWWWPostRootContentQuery`).
const DOC_ID: &str = "27130156389949648";
/// The letters of a shortcode, which is the post's number in base 64.
const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub fn read(page: &str, cookie: &str) -> Result<Video, String> {
    let code = shortcode(page).ok_or("Hãy mở video này ra trang riêng của nó để tải.")?;
    let id = media_id(&code).ok_or("Hãy mở video này ra trang riêng của nó để tải.")?;
    // The video of a carousel the viewer has moved to, as the page's address says.
    let index = url::Url::parse(page).ok().and_then(|u| u.query_pairs().find(|(k, _)| k == "img_index").and_then(|(_, v)| v.parse().ok()));
    let signed_in = cookie.split(';').any(|c| c.trim().strip_prefix("sessionid=").is_some_and(|v| !v.is_empty()));
    if signed_in {
        // A session that has run out is sent to the login page, which is no JSON: then read as someone signed out.
        let info = super::get_text(&format!("https://www.instagram.com/api/v1/media/{id}/info/"), &api_headers(Some(cookie)));
        let item = info.ok().and_then(|text| serde_json::from_str::<Value>(&text).ok()).map(|answer| answer["items"][0].clone());
        if let Some(item) = item.filter(Value::is_object) {
            return video_of(&item, index, &code);
        }
    }
    let headers: Vec<(String, String)> = facebook::PAGE_HEADERS.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let html = super::get_text(&format!("https://www.instagram.com/p/{code}/"), &headers)?;
    match from_page(&html, id).or_else(|| graphql(&html, id)) {
        Some(post) => video_of(&post, index, &code),
        None if signed_in => Err("Instagram không trả video của bài này".into()),
        None => Err(crate::ytdlp::SIGN_IN.into()),
    }
}

/// The shortcode of the post at `url`: /p/, /reel/, /reels/ or /tv/, after a user's name or not.
pub fn shortcode(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let segments: Vec<&str> = parsed.path_segments()?.filter(|s| !s.is_empty()).collect();
    let at = segments.iter().position(|s| matches!(*s, "p" | "tv" | "reel" | "reels"))?;
    // A share link's code is not the post's; /reels/audio/ is a sound's page.
    if at > 1 || segments[0] == "share" {
        return None;
    }
    let code = segments.get(at + 1)?;
    (*code != "audio" && code.bytes().all(|b| ALPHABET.contains(&b))).then(|| code.to_string())
}

/// The post's number, which its shortcode spells in base 64; a private post's shortcode has 28 more letters after it.
pub fn media_id(code: &str) -> Option<u128> {
    let code = if code.len() > 28 { &code[..code.len() - 28] } else { code };
    code.bytes().try_fold(0u128, |n, b| n.checked_mul(64)?.checked_add(ALPHABET.iter().position(|&a| a == b)? as u128))
}

/// The headers of the web app's API (yt-dlp's `_api_headers`), with the browser's cookie when it is signed in. The
/// Sec-Fetch-Site of a request from the page itself is what Instagram wants before it answers with JSON.
fn api_headers(cookie: Option<&str>) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = [
        ("User-Agent", facebook::PAGE_HEADERS[0].1),
        ("X-IG-App-ID", APP_ID),
        ("X-ASBD-ID", ASBD_ID),
        ("X-IG-WWW-Claim", "0"),
        ("Origin", "https://www.instagram.com"),
        ("Accept", "*/*"),
        ("Sec-Fetch-Site", "same-origin"),
    ]
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .into();
    headers.extend(cookie.map(|c| ("Cookie".to_owned(), c.to_owned())));
    headers
}

/// The post as its page's scripts carry it for someone signed out, when it is the post `id`.
pub fn from_page(html: &str, id: u128) -> Option<Value> {
    let id = id.to_string();
    let hit = |o: &Value| o["xig_polaris_media"]["pk"].as_str() == Some(id.as_str()) && o["xig_polaris_media"]["if_not_gated_logged_out"].is_object();
    let data: Vec<Value> = facebook::scripts(html).collect();
    data.iter().find_map(|s| facebook::find(s, &hit)).map(|o| o["xig_polaris_media"]["if_not_gated_logged_out"].clone())
}

/// The post as the page's own GraphQL query for someone signed out gets it, with the page's LSD token.
fn graphql(html: &str, id: u128) -> Option<Value> {
    let at = html.find("[\"LSD\",[],{\"token\":\"")?;
    let lsd = super::json_string_after(&html[at..], "token")?;
    let variables = format!("{{\"media_id\":\"{id}\"}}");
    let mut request = crate::engine::agent(None).post("https://www.instagram.com/api/graphql");
    for (name, value) in api_headers(None) {
        request = request.set(&name, &value);
    }
    let form = [
        ("lsd", lsd.as_str()),
        ("fb_api_caller_class", "RelayModern"),
        ("fb_api_req_friendly_name", "PolarisLoggedOutDesktopWWWPostRootContentQuery"),
        ("server_timestamps", "true"),
        ("variables", variables.as_str()),
        ("doc_id", DOC_ID),
    ];
    let response = request.set("X-FB-LSD", &lsd).set("X-FB-Friendly-Name", form[2].1).send_form(&form);
    let answer: Value = serde_json::from_str(&super::body(response).ok()?).ok()?;
    Some(answer["data"]["xig_polaris_media"]["if_not_gated_logged_out"].clone()).filter(Value::is_object)
}

/// The video of a post as the API or the page gives it: of a carousel, the one at `index` (from 1) when it is a
/// video, else the first video.
pub fn video_of(post: &Value, index: Option<usize>, code: &str) -> Result<Video, String> {
    let items: Vec<&Value> = match post["carousel_media"].as_array() {
        Some(list) => list.iter().collect(),
        None => vec![post],
    };
    let playable = |m: &Value| m["video_versions"].as_array().is_some_and(|v| !v.is_empty()) || m["video_dash_manifest"].is_string();
    let at = index.and_then(|i| items.get(i.checked_sub(1)?)).filter(|m| playable(m));
    let media = at.or_else(|| items.iter().find(|m| playable(m))).ok_or("Bài này không có video")?;
    let shape = (media["original_width"].as_u64().unwrap_or(0), media["original_height"].as_u64().unwrap_or(0));
    let mut files = Vec::new();
    for v in media["video_versions"].as_array().into_iter().flatten() {
        // yt-dlp names a file by its id, else its type; the type keeps it apart from the manifest's ids.
        let (Some(url), Some(id)) = (v["url"].as_str(), v["type"].as_u64().map(|t| t.to_string()).or_else(|| v["id"].as_str().map(str::to_owned))) else { continue };
        files.push(File::new(&id, url, v["height"].as_u64().map(|h| h as u32), shape));
    }
    let reps = media["video_dash_manifest"].as_str().map(facebook::representations).unwrap_or_default();
    let headers = [("Referer".to_owned(), "https://www.instagram.com/".to_owned())];
    let title = [&post["title"], &post["caption"]["text"]]
        .iter()
        .filter_map(|t| t.as_str()?.lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_owned))
        .next()
        .or_else(|| post["user"]["username"].as_str().map(|u| format!("Video by {u}")))
        .unwrap_or_else(|| format!("Instagram {code}"));
    Ok(Video { title, variants: facebook::variants(files, reps, &headers) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_shortcode_is_found_in_every_kind_of_instagram_link() {
        for url in [
            "https://www.instagram.com/p/Chunk8-jurw/",
            "https://instagram.com/reel/Chunk8-jurw/?igsh=abc",
            "https://www.instagram.com/reels/Chunk8-jurw/",
            "https://www.instagram.com/tv/Chunk8-jurw",
            "https://www.instagram.com/instagram/reel/Chunk8-jurw/",
            "https://www.instagram.com/p/Chunk8-jurw/?img_index=2",
        ] {
            assert_eq!(shortcode(url).as_deref(), Some("Chunk8-jurw"), "{url}");
        }
        for url in ["https://www.instagram.com/reels/audio/123/", "https://www.instagram.com/instagram/", "https://www.instagram.com/share/reel/BAxyz/", "https://www.instagram.com/reels/"] {
            assert_eq!(shortcode(url), None, "{url}");
        }
        assert_eq!(media_id("Chunk8-jurw"), Some(2913440072144448240), "as yt-dlp's _id_to_pk");
        assert_eq!(media_id(&format!("Chunk8-jurw{}", "A".repeat(28))), Some(2913440072144448240), "a private post's long shortcode");
    }

    const MANIFEST: &str = r#"<?xml version="1.0"?><MPD><Period><AdaptationSet maxWidth="720" maxHeight="1280" id="0"><Representation id="632728168860047v" mimeType="video/mp4" codecs="avc1.64001E" width="720" height="1280" frameRate="30" bandwidth="927053" FBContentLength="2194026"><BaseURL>https://instagram.fhan1-1.fna.fbcdn.net/o1/v/t2/f2/m86/v720.mp4?a=1&amp;b=2</BaseURL></Representation><Representation id="581503357187932v" mimeType="video/mp4" codecs="avc1.4D401E" width="480" height="854" frameRate="30" bandwidth="455564" FBContentLength="1078170"><BaseURL>https://instagram.fhan1-1.fna.fbcdn.net/o1/v/t2/f2/m86/v480.mp4</BaseURL></Representation></AdaptationSet><AdaptationSet id="1"><Representation id="724623455865596a" mimeType="audio/mp4" codecs="mp4a.40.5" bandwidth="93533" FBContentLength="223412"><BaseURL>https://instagram.fhan1-1.fna.fbcdn.net/o1/v/t2/f2/m69/a.mp4</BaseURL></Representation></AdaptationSet></Period></MPD>"#;

    fn post() -> Value {
        // efg = {"vencode_tag":"xpv_progressive.INSTAGRAM.CLIPS.C3.720.dash_baseline_1_v1"}
        let file = "https://instagram.fhan1-1.fna.fbcdn.net/o1/v/t2/f2/m82/p.mp4?efg=eyJ2ZW5jb2RlX3RhZyI6Inhwdl9wcm9ncmVzc2l2ZS5JTlNUQUdSQU0uQ0xJUFMuQzMuNzIwLmRhc2hfYmFzZWxpbmVfMV92MSJ9&oh=x";
        json!({
            "pk": "3038227208318201999", "code": "Cop84x6u7CP", "media_type": 2, "has_audio": true,
            "original_width": 720, "original_height": 1280,
            "caption": { "text": "\nIndiana Jones - Follow for more.\n#movies" },
            "user": { "username": "enbiggen" },
            "video_dash_manifest": MANIFEST,
            "video_versions": [{ "type": 101, "url": file }, { "type": 102, "url": file }, { "type": 103, "url": file }],
        })
    }

    #[test]
    fn each_dash_picture_is_joined_to_the_sound_and_the_file_carries_its_own() {
        let video = video_of(&post(), None, "Cop84x6u7CP").unwrap();
        assert_eq!(video.title, "Indiana Jones - Follow for more.");
        let ids: Vec<&str> = video.variants.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["632728168860047v+724623455865596a", "581503357187932v+724623455865596a", "101", "724623455865596a"], "one file for the three types of one address");
        let best = &video.variants[0];
        assert_eq!((best.height, best.fps, best.h264, best.size()), (Some(1280), 30, true, Some(2194026 + 223412)));
        assert_eq!(best.video.as_ref().unwrap().url, "https://instagram.fhan1-1.fna.fbcdn.net/o1/v/t2/f2/m86/v720.mp4?a=1&b=2");
        assert_eq!(best.video.as_ref().unwrap().headers, [("Referer".to_owned(), "https://www.instagram.com/".to_owned())], "never the cookie");
        let file = &video.variants[2];
        assert_eq!((file.height, file.audio.is_none()), (Some(1280), true), "720 wide at 9:16");
        assert!(video.variants.iter().all(|v| crate::ytdlp::valid_format(&format!("{}{}", super::super::PREFIX, v.id))));
    }

    #[test]
    fn the_signed_in_answer_gives_sizes_and_a_carousel_its_chosen_video() {
        let item = |height: u64| json!({ "media_type": 2, "video_versions": [{ "type": 101, "width": 720, "height": height, "url": format!("https://x.cdninstagram.com/{height}.mp4"), "id": "1v" }] });
        let carousel = json!({ "carousel_media": [{ "media_type": 1, "image_versions2": {} }, item(1280), item(640)], "user": { "username": "instagram" } });
        let chosen = video_of(&carousel, Some(3), "x").unwrap();
        assert_eq!(chosen.variants[0].height, Some(640));
        assert_eq!(chosen.title, "Video by instagram");
        assert_eq!(video_of(&carousel, Some(1), "x").unwrap().variants[0].height, Some(1280), "a picture is passed over for the first video");
        assert_eq!(video_of(&carousel, None, "x").unwrap().variants[0].id, "101");
        assert!(video_of(&json!({ "media_type": 1 }), None, "x").is_err());
    }

    #[test]
    fn the_post_is_read_out_of_its_page() {
        let media = json!({ "__typename": "XIGPolarisVideoMedia", "pk": "3038227208318201999", "code": "Cop84x6u7CP", "if_not_gated_logged_out": post() });
        let script = json!({ "require": [["ScheduledServerJS", "handle", null, [{ "__bbox": { "require": [["RelayPrefetchedStreamCache", "next", [], ["adp", { "__bbox": { "result": { "data": { "xig_polaris_media": media } } } }]]] } }]]] });
        let html = format!(r#"<html><script>["LSD",[],{{"token":"AdTtll"}}]</script><script type="application/json" data-content-len="1" data-sjs>{script}</script></html>"#);
        let id = media_id("Cop84x6u7CP").unwrap();
        assert_eq!(from_page(&html, id).unwrap()["video_dash_manifest"], MANIFEST);
        assert_eq!(from_page(&html, id + 1), None, "another post's is not taken");
        let at = html.find("[\"LSD\",[],{\"token\":\"").unwrap();
        assert_eq!(super::super::json_string_after(&html[at..], "token").as_deref(), Some("AdTtll"));
    }

    /// Live: `cargo test instagram_live -- --ignored`, signed out, against a public reel, its streams asked for a range.
    #[test]
    #[ignore]
    fn instagram_live() {
        let page = std::env::var("LANES_IG").unwrap_or_else(|_| "https://www.instagram.com/reel/Cop84x6u7CP/".into());
        let cookie = std::env::var("LANES_IG_COOKIE").unwrap_or_default();
        let video = read(&page, &cookie).expect("the page is read");
        println!("{}", video.title);
        for v in &video.variants {
            println!("{} {:?} fps={} h264={} size={:?} sound={}", v.id, v.height, v.fps, v.h264, v.size(), v.audio.is_some());
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
