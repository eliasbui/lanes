//! Douyin, TikTok's Chinese twin, read the way yt-dlp reads it (`DouyinIE`): the web app's detail API answers for the
//! video with the app's data, files that carry their sound at each bitrate. Douyin answers only a browser that has been
//! on the site, signed in or not: without the cookies its scripts set there (`UIFID`, `ttwid`, `s_v_web_id`) it says
//! "Uifid Not Found". The extension sends the browser's own; nothing else is sent to Douyin.
//! ponytail: no `a_bogus` signature, which yt-dlp does not make either; add it when the browser's cookies stop being
//! enough.

use super::{BROWSER_AGENT, Stream, Variant, Video, tiktok};
use serde_json::Value;

const API: &str = "https://www.douyin.com/aweme/v1/web/aweme/detail/";
/// douyinvod serves the files to a request from the site.
const REFERER: &str = "https://www.douyin.com/";
const NOT_A_VIDEO: &str = "Hãy mở video này ra trang riêng của nó để tải.";
const REFUSED: &str = "Douyin không trả video này cho Lanes. Hãy mở video trên douyin.com trong trình duyệt, đợi nó phát rồi bấm lại nút Tải video.";

/// The id of the video at `url`: a /video/ID or /share/video/ID page, or the `modal_id` of a video opened over a feed,
/// a search or a user's page.
pub fn video_id(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let modal = parsed.query_pairs().find(|(k, _)| k == "modal_id").map(|(_, v)| v.into_owned());
    modal.filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).or_else(|| tiktok::video_id(url))
}

pub fn read(page: &str, cookie: &str) -> Result<Video, String> {
    let id = match video_id(page) {
        Some(id) => id,
        // A v.douyin.com short link leads to the video's share page.
        None => video_id(&tiktok::followed(page)?).ok_or(NOT_A_VIDEO)?,
    };
    let headers: Vec<(String, String)> =
        [("User-Agent", BROWSER_AGENT), ("Referer", REFERER), ("Cookie", cookie)].iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let url = format!("{API}?aweme_id={id}&aid=6383&device_platform=webapp");
    // A 403, or an empty answer, is Douyin's refusal of a browser it has not seen; a deleted video is an empty answer too.
    let answer = super::get_text(&url, &headers).map_err(|e| if e.starts_with("Máy chủ trả mã") { REFUSED.to_owned() } else { e })?;
    let detail = serde_json::from_str::<Value>(&answer).ok().map(|a| a["aweme_detail"].clone()).filter(Value::is_object).ok_or(REFUSED)?;
    Ok(video_of(&detail, &id))
}

/// The video in the API's `aweme_detail`: each bitrate's file, then the one the player starts with, then the sound.
pub fn video_of(detail: &Value, id: &str) -> Video {
    let video = &detail["video"];
    let headers = vec![("User-Agent".to_owned(), BROWSER_AGENT.to_owned()), ("Referer".to_owned(), REFERER.to_owned())];
    let stream = |url: &str, size: Option<u64>, ext: &str| Stream { url: url.to_owned(), hls: false, size, ext: ext.to_owned(), headers: headers.clone() };
    let h265 = |o: &Value| o["is_h265"].as_u64() == Some(1) || o["is_bytevc1"].as_u64() == Some(1);
    let mut variants: Vec<Variant> = Vec::new();
    let mut add = |id: String, addr: &Value, height: &Value, fps: &Value, h264: bool| {
        let Some(url) = addr["url_list"].as_array().into_iter().flatten().filter_map(Value::as_str).find(|u| usable(u)) else { return };
        if variants.iter().any(|v| v.id == id || v.video.as_ref().is_some_and(|s| s.url == url)) {
            return;
        }
        let height = addr["height"].as_u64().or(height.as_u64()).map(|h| h as u32);
        variants.push(Variant { id, height, fps: fps.as_u64().unwrap_or(0) as u32, h264, video: Some(stream(url, addr["data_size"].as_u64(), "mp4")), audio: None });
    };
    for rate in video["bit_rate"].as_array().into_iter().flatten() {
        // A DASH bitrate is a picture without its sound.
        if rate["format"].as_str().is_some_and(|f| f != "mp4") {
            continue;
        }
        add(tiktok::format_id(rate["gear_name"].as_str().unwrap_or("rate")), &rate["play_addr"], &Value::Null, &rate["FPS"], !h265(rate));
    }
    add("play".into(), &video["play_addr"], &video["height"], &Value::Null, !h265(video));
    if let Some(url) = detail["music"]["play_url"]["url_list"].as_array().into_iter().flatten().filter_map(Value::as_str).find(|u| usable(u)) {
        variants.push(Variant { id: "audio".into(), audio: Some(stream(url, None, if url.contains(".mp3") { "mp3" } else { "m4a" })), ..Variant::default() });
    }
    let desc = detail["desc"].as_str().unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ");
    Video { title: if desc.is_empty() { format!("Douyin {id}") } else { desc }, variants }
}

/// A file on the CDN: the site's own `www.douyin.com/aweme/v1/play/` addresses only send on to it, and want the
/// browser's cookies to.
fn usable(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://")) && !url.contains("://www.douyin.com/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_id_is_found_in_every_kind_of_douyin_link() {
        for url in [
            "https://www.douyin.com/video/7693093964693490980",
            "https://www.iesdouyin.com/share/video/7693093964693490980/?region=CN",
            "https://www.douyin.com/jingxuan?modal_id=7693093964693490980",
            "https://www.douyin.com/user/MS4wLjABAAAA?from_tab_name=main&modal_id=7693093964693490980",
        ] {
            assert_eq!(video_id(url).as_deref(), Some("7693093964693490980"), "{url}");
        }
        assert_eq!(video_id("https://v.douyin.com/iRNBho6u/"), None, "a short link is followed first");
        assert_eq!(video_id("https://www.douyin.com/jingxuan?modal_id=x1"), None);
    }

    #[test]
    fn each_bitrate_is_a_variant_and_the_cdn_is_asked_from_the_site() {
        let cdn = |n: &str| format!("https://v26-web.douyinvod.com/{n}/video/tos/cn/");
        let detail = json!({
            "desc": "国庆\n #旅行 ",
            "music": { "play_url": { "url_list": ["https://sf5-hl-cdn-tos.douyinstatic.com/obj/ies-music/1.mp3"] } },
            "video": {
                "height": 1920, "is_h265": 0,
                "play_addr": { "url_list": ["https://www.douyin.com/aweme/v1/play/?video_id=v0", cdn("play")] },
                "bit_rate": [
                    { "gear_name": "adapt_lowest_1080_1", "FPS": 30, "is_h265": 1, "format": "mp4",
                      "play_addr": { "height": 1920, "data_size": 900000, "url_list": ["https://www.douyin.com/aweme/v1/play/?x", cdn("hevc")] } },
                    { "gear_name": "normal_720_0", "FPS": 30, "is_h265": 0,
                      "play_addr": { "height": 1280, "data_size": 500000, "url_list": [cdn("h264"), cdn("h264-backup")] } },
                    { "gear_name": "dash_720", "format": "dash", "play_addr": { "url_list": [cdn("dash")] } },
                    { "gear_name": "only_the_site", "play_addr": { "url_list": ["https://www.douyin.com/aweme/v1/play/?y"] } },
                ],
            },
        });
        let video = video_of(&detail, "1");
        assert_eq!(video.title, "国庆 #旅行");
        let ids: Vec<&str> = video.variants.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["adapt_lowest_1080_1", "normal_720_0", "play", "audio"], "no DASH, no address only the site serves");
        let (hevc, h264, play) = (&video.variants[0], &video.variants[1], &video.variants[2]);
        assert_eq!((hevc.height, hevc.fps, hevc.h264, hevc.size()), (Some(1920), 30, false, Some(900000)));
        assert_eq!((h264.height, h264.h264, h264.video.as_ref().unwrap().url.as_str()), (Some(1280), true, cdn("h264").as_str()));
        assert_eq!((play.height, play.h264, play.size()), (Some(1920), true, None), "the player's file takes the video's height");
        assert!(h264.video.as_ref().unwrap().headers.contains(&("Referer".into(), REFERER.into())));
        assert!(h264.video.as_ref().unwrap().headers.iter().all(|(name, _)| name != "Cookie"), "the browser's cookies stay with Douyin's API");
        assert_eq!(video.variants[3].ext(), "mp3");
        assert_eq!(super::super::choices(&video)[0].format, "lanes.play", "at one height the H.264 file before the HEVC one");
        assert_eq!(video_of(&json!({}), "7").title, "Douyin 7");
    }

    /// Live, with the Cookie header a browser that opened douyin.com sends: `LANES_DOUYIN_COOKIE='...' cargo test
    /// douyin_live -- --ignored --nocapture`, or LANES_DOUYIN=<video url> for another video.
    #[test]
    #[ignore = "needs the network and a browser's Douyin cookies"]
    fn douyin_live() {
        let page = std::env::var("LANES_DOUYIN").unwrap_or_else(|_| "https://www.douyin.com/video/7693093964693490980".into());
        let video = read(&page, &std::env::var("LANES_DOUYIN_COOKIE").unwrap_or_default()).expect("the page is read");
        let choice = super::super::choices(&video).into_iter().next().expect("a choice");
        println!("{}: best {} {:?}p {:?} bytes", video.title, choice.format, choice.height, choice.size);
        let variant = video.variants.iter().find(|v| choice.format == format!("{}{}", super::super::PREFIX, v.id)).unwrap();
        let stream = variant.video.as_ref().unwrap();
        let mut request = crate::engine::agent(None).get(&stream.url).set("Range", "bytes=0-1023");
        for (name, value) in &stream.headers {
            request = request.set(name, value);
        }
        let response = request.call().expect("the CDN answers");
        assert_eq!(response.status(), 206);
    }
}
