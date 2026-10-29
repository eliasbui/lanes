//! X (Twitter), read the way its web app asks for a post (yt-dlp's `TwitterIE`, GraphQL API). A guest asks
//! `api.x.com/1.1/guest/activate.json` for a guest token with the web app's public bearer, then `TweetResultByRestId`
//! for the post; a signed-in user (an `auth_token` and a `ct0` among the cookies) asks the second with the cookies
//! instead, so posts for adults or followers only come too. The answer lists each video as MP4 files and an HLS
//! playlist; the MP4s carry their sound and video.twimg.com serves them in ranges with no cookie, so they are offered.
//!
//! ponytail: a video offered only as HLS (sound in its own playlist) is not read; yt-dlp takes it. Read the master's
//! audio rendition into `Variant::audio` when one turns up.

use super::{Stream, Variant, Video};
use serde_json::{Value, json};

/// The web app's public bearer, the one yt-dlp sends (yt_dlp/extractor/twitter.py, `_AUTH`).
const BEARER: &str = "Bearer AAAAAAAAAAAAAAAAAAAAANRILgAAAAAAnNwIzUejRCOuH5E6I8xnZz4puTs%3D1Zv7ttfk8LF81IUq16cHjhLTvJu4FA33AGWWjCpTnA";
const GUEST: &str = "https://api.x.com/1.1/guest/activate.json";
const POST: &str = "https://x.com/i/api/graphql/2ICDjqPd81tulZcYrtpTuQ/TweetResultByRestId";
/// The features the web app declares; X has refused queries that leave some out.
const FEATURES: &str = r#"{"creator_subscriptions_tweet_preview_api_enabled":true,"tweetypie_unmention_optimization_enabled":true,"responsive_web_edit_tweet_api_enabled":true,"graphql_is_translatable_rweb_tweet_is_translatable_enabled":true,"view_counts_everywhere_api_enabled":true,"longform_notetweets_consumption_enabled":true,"responsive_web_twitter_article_tweet_consumption_enabled":false,"tweet_awards_web_tipping_enabled":false,"freedom_of_speech_not_reach_fetch_enabled":true,"standardized_nudges_misinfo":true,"tweet_with_visibility_results_prefer_gql_limited_actions_policy_enabled":true,"longform_notetweets_rich_text_read_enabled":true,"longform_notetweets_inline_media_enabled":true,"responsive_web_graphql_exclude_directive_enabled":true,"verified_phone_label_enabled":false,"responsive_web_media_download_video_enabled":false,"responsive_web_graphql_skip_user_profile_image_extensions_enabled":false,"responsive_web_graphql_timeline_navigation_enabled":true,"responsive_web_enhance_cards_enabled":false}"#;

/// The post's id at `url`, and the number of the media the link points at (`/video/2`), counted from 1.
pub fn status_id(url: &str) -> Option<(String, Option<usize>)> {
    let parsed = url::Url::parse(url).ok()?;
    let segments: Vec<&str> = parsed.path_segments()?.collect();
    let at = segments.iter().position(|s| *s == "status" || *s == "statuses")?;
    let id = segments.get(at + 1).filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))?;
    let index = match segments.get(at + 2..at + 4) {
        Some([kind, n]) if matches!(*kind, "video" | "photo") => n.parse().ok().filter(|n| *n > 0),
        _ => None,
    };
    Some((id.to_string(), index))
}

fn cookie_value<'a>(cookie: &'a str, name: &str) -> Option<&'a str> {
    cookie.split(';').filter_map(|pair| pair.trim().split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| v).filter(|v| !v.is_empty())
}

pub fn read(page: &str, cookie: &str) -> Result<Video, String> {
    let (id, index) = status_id(page).ok_or("Hãy mở bài đăng có video ra trang riêng của nó để tải.")?;
    let mut headers = vec![("Authorization".to_owned(), BEARER.to_owned())];
    // The cookies go to x.com only: twitter.com is the same site under its old name, and leads there.
    match (cookie_value(cookie, "auth_token"), cookie_value(cookie, "ct0")) {
        (Some(_), Some(csrf)) => headers.extend([("Cookie", cookie), ("x-csrf-token", csrf), ("x-twitter-auth-type", "OAuth2Session")].map(|(k, v)| (k.to_owned(), v.to_owned()))),
        _ => {
            let answer = super::post_json(GUEST, &headers, &json!({}))?;
            let token = answer["guest_token"].as_str().ok_or("X không cấp mã khách, hãy thử lại")?;
            headers.push(("x-guest-token".to_owned(), token.to_owned()));
        }
    }
    let variables = json!({ "tweetId": id, "withCommunity": false, "includePromotedContent": false, "withVoice": false });
    let mut url = url::Url::parse(POST).expect("a valid address");
    url.query_pairs_mut().append_pair("variables", &variables.to_string()).append_pair("features", FEATURES);
    let text = super::get_text(url.as_str(), &headers)?;
    let answer: Value = serde_json::from_str(&text).map_err(|e| format!("Trang trả về dữ liệu lạ: {e}"))?;
    video_of(&answer, index)
}

/// The video of the post in the API's answer: the one `index` points at, else the post's first, else that of the post
/// it quotes or of its card.
pub fn video_of(answer: &Value, index: Option<usize>) -> Result<Video, String> {
    let mut post = &answer["data"]["tweetResult"]["result"];
    if post.is_null() {
        // "Dependency: Unspecified" comes with good answers too, yt-dlp issue 15963.
        let errors: Vec<&str> = answer["errors"].as_array().into_iter().flatten().filter_map(|e| e["message"].as_str()).filter(|m| *m != "Dependency: Unspecified").collect();
        return Err(if errors.is_empty() { "Bài đăng không tồn tại hoặc đã bị xóa".into() } else { format!("X báo lỗi: {}", errors.join(", ")) });
    }
    match post["__typename"].as_str() {
        Some("TweetUnavailable") => {
            return match post["reason"].as_str().unwrap_or_default() {
                "NsfwLoggedOut" | "NsfwViewerHasNoStatedAge" | "Protected" => Err(crate::ytdlp::SIGN_IN.into()),
                reason => Err(format!("X không cho xem bài đăng này ({reason})")),
            };
        }
        // With a reason when the post is gone; without one when a guest may not see it (for adults only).
        Some("TweetTombstone") => {
            return Err(match post["tombstone"]["text"]["text"].as_str() {
                Some(text) => format!("X không cho xem bài đăng này: {}", text.trim_end_matches(" Learn more").trim_end_matches('.')),
                None => crate::ytdlp::SIGN_IN.into(),
            });
        }
        Some("TweetWithVisibilityResults") => post = &post["tweet"],
        _ => {}
    }
    if post["legacy"]["retweeted_status_result"]["result"].is_object() {
        post = &post["legacy"]["retweeted_status_result"]["result"];
        if post["__typename"] == "TweetWithVisibilityResults" {
            post = &post["tweet"];
        }
    }
    let media = |post: &Value| post["legacy"]["extended_entities"]["media"].as_array().cloned().unwrap_or_default();
    let own = media(post);
    let chosen = match index {
        Some(n) => {
            let m = own.get(n - 1).ok_or(format!("Bài đăng không có video số {n}"))?;
            if m["type"] == "photo" {
                return Err(format!("Mục số {n} của bài đăng là ảnh, không phải video"));
            }
            Some(m.clone())
        }
        None => {
            // A promoted post keeps its video in its card, as JSON in a string.
            let card = post["card"]["legacy"]["binding_values"].as_array().into_iter().flatten().find(|b| b["key"] == "unified_card");
            let card: Value = card.and_then(|b| serde_json::from_str(b["value"]["string_value"].as_str()?).ok()).unwrap_or_default();
            let carded = card["media_entities"].as_object().map(|m| m.values().cloned().collect()).unwrap_or_default();
            [own, media(&post["quoted_status_result"]["result"]), carded].concat().into_iter().find(|m| m["type"] == "video" || m["type"] == "animated_gif")
        }
    };
    let mut variants: Vec<Variant> = Vec::new();
    for v in chosen.iter().flat_map(|m| m["video_info"]["variants"].as_array().cloned().unwrap_or_default()) {
        let Some(url) = v["url"].as_str().filter(|_| v["content_type"] == "video/mp4") else { continue };
        let id = format!("http-{}", v["bitrate"].as_u64().unwrap_or(0) / 1000);
        if variants.iter().any(|known| known.id == id) {
            continue;
        }
        let height = dimensions(url).map(|(_, h)| h).or_else(|| chosen.as_ref()?["original_info"]["height"].as_u64().map(|h| h as u32));
        let stream = Stream { url: url.to_owned(), hls: false, size: None, ext: "mp4".into(), headers: Vec::new() };
        variants.push(Variant { id, height, fps: 0, h264: !url.contains("/hevc/"), video: Some(stream), audio: None });
    }
    Ok(Video { title: title(post), variants })
}

/// The `WxH` an MP4's address names, as in `/vid/avc1/720x1280/`.
fn dimensions(url: &str) -> Option<(u32, u32)> {
    url.split('?').next()?.split('/').find_map(|s| {
        let (w, h) = s.split_once('x')?;
        Some((w.parse().ok()?, h.parse().ok()?))
    })
}

/// "Author - text", without the t.co link to the media itself.
fn title(post: &Value) -> String {
    let user = &post["core"]["user_results"]["result"];
    let name = user["core"]["name"].as_str().or(user["legacy"]["name"].as_str()).unwrap_or("X");
    let text = post["legacy"]["full_text"].as_str().unwrap_or_default();
    let text: Vec<&str> = text.split_whitespace().filter(|w| !w.starts_with("https://t.co/")).collect();
    if text.is_empty() { name.to_owned() } else { format!("{name} - {}", text.join(" ")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_post_and_its_media_number_are_found_in_every_kind_of_link() {
        let id = status_id;
        assert_eq!(id("https://x.com/historyinmemes/status/1790637656616943991?s=20"), Some(("1790637656616943991".into(), None)));
        assert_eq!(id("https://twitter.com/CTVJLaidlaw/status/1600649710662213632/video/2"), Some(("1600649710662213632".into(), Some(2))));
        assert_eq!(id("https://mobile.twitter.com/i/web/status/910031516746514432"), Some(("910031516746514432".into(), None)));
        assert_eq!(id("https://twitter.com/statuses/910031516746514432"), Some(("910031516746514432".into(), None)));
        assert_eq!(id("https://x.com/historyinmemes"), None);
        assert_eq!(id("https://x.com/a/status/12ab"), None);
    }

    #[test]
    fn a_signed_in_users_cookies_are_recognised() {
        let cookie = "guest_id=v1%3A1; auth_token=abc; ct0=tok=en";
        assert_eq!((cookie_value(cookie, "auth_token"), cookie_value(cookie, "ct0")), (Some("abc"), Some("tok=en")));
        assert_eq!(cookie_value("ct0=", "ct0"), None);
        assert_eq!(cookie_value("", "auth_token"), None);
    }

    fn mp4(bitrate: u64, size: &str) -> Value {
        json!({ "bitrate": bitrate, "content_type": "video/mp4", "url": format!("https://video.twimg.com/ext_tw_video/1/pu/vid/avc1/{size}/a.mp4?tag=12") })
    }

    fn video(variants: Value) -> Value {
        json!({ "type": "video", "original_info": { "width": 886, "height": 876 }, "video_info": { "variants": variants } })
    }

    fn answer(post: Value) -> Value {
        json!({ "data": { "tweetResult": { "result": post } } })
    }

    #[test]
    fn each_mp4_of_the_video_is_a_variant_and_the_playlist_is_left() {
        let post = json!({
            "__typename": "Tweet",
            "core": { "user_results": { "result": { "legacy": { "name": "Historic Vids", "screen_name": "historyinmemes" } } } },
            "legacy": { "full_text": "One of the most\nintense moments https://t.co/Zgzhvix8ES", "extended_entities": { "media": [
                { "type": "photo" },
                video(json!([
                    { "content_type": "application/x-mpegURL", "url": "https://video.twimg.com/ext_tw_video/1/pu/pl/a.m3u8?tag=12" },
                    mp4(288000, "272x270"), mp4(2176000, "728x720"), mp4(2176000, "728x720"),
                ])),
                video(json!([mp4(832000, "364x360")])),
            ] } },
        });
        let read = video_of(&answer(post.clone()), None).unwrap();
        assert_eq!(read.title, "Historic Vids - One of the most intense moments");
        let ids: Vec<(&str, Option<u32>)> = read.variants.iter().map(|v| (v.id.as_str(), v.height)).collect();
        assert_eq!(ids, [("http-288", Some(270)), ("http-2176", Some(720))], "the first video, its MP4s once each");
        let best = &read.variants[1];
        assert!(best.h264 && best.audio.is_none() && best.video.as_ref().unwrap().headers.is_empty(), "the CDN wants no cookie");
        assert_eq!(video_of(&answer(post.clone()), Some(3)).unwrap().variants[0].id, "http-832", "/video/3 is the third media");
        assert!(video_of(&answer(post.clone()), Some(1)).unwrap_err().contains("ảnh"));
        assert!(video_of(&answer(post), Some(4)).is_err());
    }

    #[test]
    fn a_retweet_a_quote_a_card_and_a_gif_lead_to_their_video() {
        let quoting = json!({ "__typename": "TweetWithVisibilityResults", "tweet": {
            "core": { "user_results": { "result": { "core": { "name": "New Name" }, "legacy": {} } } },
            "legacy": { "full_text": "look" },
            "quoted_status_result": { "result": { "legacy": { "extended_entities": { "media": [video(json!([mp4(950000, "480x852")]))] } } } },
        }});
        let read = video_of(&answer(quoting), None).unwrap();
        assert_eq!((read.title.as_str(), read.variants[0].height), ("New Name - look", Some(852)));
        let retweet = json!({ "legacy": { "full_text": "RT", "retweeted_status_result": { "result": { "legacy": { "full_text": "orig", "extended_entities": { "media": [video(json!([mp4(632000, "320x568")]))] } } } } } });
        assert_eq!(video_of(&answer(retweet), None).unwrap().variants[0].id, "http-632");
        let card = json!({ "media_entities": { "13_2": video(json!([mp4(2176000, "1280x720")])) } }).to_string();
        let promoted = json!({ "legacy": { "full_text": "ad" }, "card": { "legacy": { "binding_values": [{ "key": "unified_card", "value": { "string_value": card } }] } } });
        assert_eq!(video_of(&answer(promoted), None).unwrap().variants[0].height, Some(720));
        let gif = json!({ "legacy": { "extended_entities": { "media": [{ "type": "animated_gif", "original_info": { "height": 270 }, "video_info": { "variants": [
            { "bitrate": 0, "content_type": "video/mp4", "url": "https://video.twimg.com/tweet_video/abc.mp4" }] } }] } } });
        let gif = video_of(&answer(gif), None).unwrap();
        assert_eq!((gif.variants[0].id.as_str(), gif.variants[0].height), ("http-0", Some(270)), "a GIF's size is the original's");
        assert!(video_of(&answer(json!({ "legacy": { "full_text": "text only" } })), None).unwrap().variants.is_empty());
    }

    #[test]
    fn a_post_x_will_not_show_says_why() {
        let gone = json!({ "data": { "tweetResult": {} } });
        assert!(video_of(&gone, None).unwrap_err().contains("không tồn tại"));
        let bad = json!({ "errors": [{ "message": "Dependency: Unspecified" }, { "message": "value out of range" }] });
        assert_eq!(video_of(&bad, None).unwrap_err(), "X báo lỗi: value out of range");
        let adult = json!({ "__typename": "TweetUnavailable", "reason": "NsfwLoggedOut" });
        assert_eq!(video_of(&answer(adult), None).unwrap_err(), crate::ytdlp::SIGN_IN);
        let hidden = json!({ "__typename": "TweetTombstone" });
        assert_eq!(video_of(&answer(hidden), None).unwrap_err(), crate::ytdlp::SIGN_IN, "what a guest gets for a post for adults");
        let suspended = json!({ "__typename": "TweetTombstone", "tombstone": { "text": { "text": "This Post is from a suspended account. Learn more" } } });
        assert_eq!(video_of(&answer(suspended), None).unwrap_err(), "X không cho xem bài đăng này: This Post is from a suspended account");
    }

    /// Live: `cargo test --quiet extract::twitter -- --ignored`, or with LANES_X=<post url>.
    #[test]
    #[ignore = "needs the network"]
    fn a_public_post_is_read_and_its_cdn_serves_ranges() {
        let page = std::env::var("LANES_X").unwrap_or_else(|_| "https://x.com/historyinmemes/status/1790637656616943991".into());
        let video = read(&page, "").expect("the post is read");
        let choice = super::super::choices(&video).into_iter().next().expect("a choice");
        println!("{}: {:?}, best {} {:?}p", video.title, video.variants.iter().map(|v| &v.id).collect::<Vec<_>>(), choice.format, choice.height);
        let variant = video.variants.iter().find(|v| choice.format == format!("{}{}", super::super::PREFIX, v.id)).unwrap();
        let response = crate::engine::agent(None).get(&variant.video.as_ref().unwrap().url).set("Range", "bytes=0-1023").call().expect("the CDN answers");
        assert_eq!(response.status(), 206);
        println!("206 {}", response.header("content-range").unwrap_or_default());
        let mut head = Vec::new();
        std::io::Read::read_to_end(&mut response.into_reader(), &mut head).unwrap();
        assert_eq!((head.len(), &head[4..8]), (1024, &b"ftyp"[..]), "the bytes of an MP4");
    }
}
