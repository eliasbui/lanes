//! HLS video, an .m3u8 playlist of short segments, as the video button of the browser extension finds it. A master
//! playlist lists the same video at several resolutions; a media playlist lists the segments of one of them. The
//! extension shows the resolutions of a master to choose from (`choices`), and the engine downloads the segments of
//! the chosen one several at a time and joins them in order into one MP4: MPEG-TS segments are rewrapped by the ffmpeg
//! the app carries, since few players open a bare .ts (`engine::Engine::run_hls`, `engine::settle`).
//!
//! Plain or AES-128 segments, MPEG-TS or fragmented MP4, whole files or byte ranges of one. Never DRM: a SAMPLE-AES or
//! other protected stream is refused. A resolution whose sound is a playlist of its own (`Variant::audio`) has that
//! playlist downloaded beside it and joined to it by ffmpeg.
//! ponytail: no live streams, and one sound per video, its group's default; add a choice of language when a site
//! the user needs offers several.

use cbc::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use serde::Serialize;
use std::io::Read;
use url::Url;

/// A playlist is text; anything bigger than this is not one.
const MAX_PLAYLIST: u64 = 8 << 20;

/// One resolution of a master playlist.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Variant {
    pub url: String,
    pub height: Option<u32>,
    pub bandwidth: Option<u64>,
    /// The playlist of its sound, when that is apart from the picture: its group's default, else the group's first.
    pub audio: Option<String>,
}

/// The AES-128 key of a run of segments, and its IV when the playlist gives one.
#[derive(Debug, Clone, PartialEq)]
pub struct Key {
    pub url: String,
    pub iv: Option<[u8; 16]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub url: String,
    /// The offset and length of the segment in `url`, when the playlist cuts one file into byte ranges.
    pub range: Option<(u64, u64)>,
    pub key: Option<Key>,
    /// The media sequence number, the IV of a key that gives none.
    pub sequence: u64,
}

/// The segments of one resolution, after the init part a fragmented MP4 stream starts with.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Media {
    pub init: Option<Segment>,
    pub segments: Vec<Segment>,
    /// Seconds, the sum of the segment lengths.
    pub duration: f64,
}

#[derive(Debug, PartialEq)]
pub enum Playlist {
    /// The resolutions, best first, and the audio and subtitle playlists the master also names.
    Master { variants: Vec<Variant>, renditions: Vec<String> },
    Media(Media),
}

/// What the extension shows for one playlist: its resolutions, how long it runs to estimate each one's size, and the
/// type of file it makes, always MP4. `renditions` are playlists the master names that are not resolutions, so the extension does
/// not offer them on their own.
#[derive(Debug, Serialize)]
pub struct Choices {
    pub variants: Vec<Variant>,
    pub renditions: Vec<String>,
    pub duration: f64,
    pub ext: &'static str,
}

/// The `NAME=value,NAME="quoted, value"` list after a tag's colon.
fn attributes(list: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = list.trim();
    while let Some((name, after)) = rest.split_once('=') {
        let (value, next) = match after.strip_prefix('"') {
            Some(quoted) => quoted.split_once('"').unwrap_or((quoted, "")),
            None => after.split_once(',').unwrap_or((after, "")),
        };
        out.push((name.trim().to_ascii_uppercase(), value.to_owned()));
        rest = next.trim_start_matches([',', ' ']);
    }
    out
}

fn attribute<'a>(list: &'a [(String, String)], name: &str) -> Option<&'a str> {
    list.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
}

/// An IV written as `0x` and up to 32 hex digits.
fn parse_iv(text: &str) -> Result<[u8; 16], String> {
    let hex = text.trim_start_matches("0x").trim_start_matches("0X");
    let value = u128::from_str_radix(hex, 16).map_err(|_| format!("IV không hợp lệ: {text}"))?;
    Ok(value.to_be_bytes())
}

/// A `length[@offset]` byte range; with no offset it follows on from `previous_end`.
fn byte_range(text: &str, previous_end: u64) -> Result<(u64, u64), String> {
    let bad = || format!("Đoạn byte không hợp lệ: {text}");
    let (length, offset) = match text.trim().split_once('@') {
        Some((length, offset)) => (length, offset.parse().map_err(|_| bad())?),
        None => (text.trim(), previous_end),
    };
    Ok((offset, length.parse().map_err(|_| bad())?))
}

/// Reads a playlist fetched from `base` (its address after redirects, which relative segment addresses hang off).
pub fn parse(text: &str, base: &str) -> Result<Playlist, String> {
    if !text.trim_start_matches('\u{feff}').trim_start().starts_with("#EXTM3U") {
        return Err("Đây không phải danh sách HLS".into());
    }
    let base = Url::parse(base).map_err(|e| format!("Liên kết không hợp lệ: {e}"))?;
    let join = |uri: &str| base.join(uri.trim()).map(String::from).map_err(|e| format!("Liên kết đoạn không hợp lệ: {e}"));
    let lines = text.lines().map(str::trim).filter(|l| !l.is_empty());

    if text.contains("#EXT-X-STREAM-INF") {
        let mut renditions = Vec::new();
        // Each sound playlist with its group and whether it is the group's default.
        let mut sounds: Vec<(String, String, bool)> = Vec::new();
        let mut found: Vec<(Variant, Option<String>)> = Vec::new();
        let mut pending = None;
        for line in lines {
            if let Some(list) = line.strip_prefix("#EXT-X-MEDIA:") {
                let list = attributes(list);
                if let Some(uri) = attribute(&list, "URI") {
                    renditions.push(join(uri)?);
                    if let (Some("AUDIO"), Some(group)) = (attribute(&list, "TYPE"), attribute(&list, "GROUP-ID")) {
                        sounds.push((group.to_owned(), join(uri)?, attribute(&list, "DEFAULT") == Some("YES")));
                    }
                }
            } else if let Some(list) = line.strip_prefix("#EXT-X-STREAM-INF:") {
                pending = Some(attributes(list));
            } else if !line.starts_with('#') {
                if let Some(list) = pending.take() {
                    let height = attribute(&list, "RESOLUTION").and_then(|r| r.split_once(['x', 'X'])).and_then(|(_, h)| h.parse().ok());
                    let bandwidth = attribute(&list, "BANDWIDTH").and_then(|b| b.parse().ok());
                    let variant = Variant { url: join(line)?, height, bandwidth, audio: None };
                    found.push((variant, attribute(&list, "AUDIO").map(str::to_owned)));
                }
            }
        }
        let mut variants: Vec<Variant> = found
            .into_iter()
            .map(|(mut v, group)| {
                let group: Vec<&(String, String, bool)> = sounds.iter().filter(|(g, ..)| Some(g) == group.as_ref()).collect();
                v.audio = group.iter().find(|(.., default)| *default).or(group.first()).map(|(_, uri, _)| uri.clone());
                v
            })
            .collect();
        variants.sort_by_key(|v| std::cmp::Reverse((v.height, v.bandwidth)));
        variants.dedup_by(|a, b| a.url == b.url);
        if variants.is_empty() {
            return Err("Danh sách HLS không có bản nào".into());
        }
        return Ok(Playlist::Master { variants, renditions });
    }

    let mut media = Media::default();
    let mut key = None;
    let mut sequence = 0;
    let mut ended = false;
    let mut range = None;
    let mut previous_end = 0;
    for line in lines {
        if let Some(n) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            sequence = n.trim().parse().unwrap_or(0);
        } else if let Some(list) = line.strip_prefix("#EXT-X-KEY:") {
            let list = attributes(list);
            key = match attribute(&list, "METHOD") {
                None | Some("NONE") => None,
                Some("AES-128") => Some(Key {
                    url: join(attribute(&list, "URI").ok_or("Khóa giải mã không có địa chỉ")?)?,
                    iv: attribute(&list, "IV").map(parse_iv).transpose()?,
                }),
                Some(_) => return Err("Video được bảo vệ bản quyền (DRM), Lanes không tải được".into()),
            };
        } else if let Some(list) = line.strip_prefix("#EXT-X-MAP:") {
            let list = attributes(list);
            let url = join(attribute(&list, "URI").ok_or("Phần mở đầu video không có địa chỉ")?)?;
            let range = attribute(&list, "BYTERANGE").map(|r| byte_range(r, 0)).transpose()?;
            media.init = Some(Segment { url, range, key: None, sequence: 0 });
        } else if let Some(length) = line.strip_prefix("#EXTINF:") {
            media.duration += length.split(',').next().and_then(|s| s.trim().parse::<f64>().ok()).unwrap_or(0.0);
        } else if let Some(text) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            let (offset, length) = byte_range(text, previous_end)?;
            previous_end = offset + length;
            range = Some((offset, length));
        } else if line.starts_with("#EXT-X-ENDLIST") {
            ended = true;
        } else if !line.starts_with('#') {
            media.segments.push(Segment { url: join(line)?, range: range.take(), key: key.clone(), sequence });
            sequence += 1;
        }
    }
    if !ended {
        return Err("Đây là luồng phát trực tiếp, bản này chưa ghi được".into());
    }
    if media.segments.is_empty() {
        return Err("Danh sách HLS không có đoạn nào".into());
    }
    Ok(Playlist::Media(media))
}

/// The headers for one request of a playlist first fetched at `origin`. The page's cookies go only to the host they
/// came from, so a segment or key on another server never receives them.
fn headers_for<'a>(url: &str, origin: &str, headers: &'a [(String, String)]) -> impl Iterator<Item = &'a (String, String)> {
    let host = |u: &str| Url::parse(u).ok().and_then(|u| u.host_str().map(str::to_owned));
    let same_host = host(url).is_some() && host(url) == host(origin);
    headers.iter().filter(move |(name, _)| same_host || !name.eq_ignore_ascii_case("cookie"))
}

/// One request; with net-merge (`proxy`) its connection goes through the app's proxy.
fn get(url: &str, origin: &str, headers: &[(String, String)], proxy: Option<&str>) -> Result<ureq::Response, String> {
    let mut request = crate::engine::agent(proxy).get(url);
    for (name, value) in headers_for(url, origin, headers) {
        request = request.set(name, value);
    }
    match request.call() {
        Ok(response) => Ok(response),
        Err(ureq::Error::Status(code, _)) => Err(format!("Máy chủ trả mã {code}")),
        Err(e) => Err(format!("Không kết nối được: {e}")),
    }
}

/// A reader over one segment, key or init part of the playlist at `origin`, or over its byte range.
pub fn open(url: &str, range: Option<(u64, u64)>, origin: &str, headers: &[(String, String)], proxy: Option<&str>) -> Result<Box<dyn Read + Send>, String> {
    let mut extra = headers.to_vec();
    if let Some((offset, length)) = range.filter(|&(_, length)| length > 0) {
        extra.push(("Range".into(), format!("bytes={offset}-{}", offset + length - 1)));
    }
    let response = get(url, origin, &extra, proxy)?;
    // A server that ignores the range sends the whole file, which must not be taken for the segment.
    if range.is_some() && response.status() != 206 {
        return Err("Máy chủ không hỗ trợ tải theo đoạn".into());
    }
    Ok(response.into_reader())
}

/// A playlist's address after redirects, and its text.
fn fetch(url: &str, origin: &str, headers: &[(String, String)]) -> Result<(String, String), String> {
    let response = get(url, origin, headers, None)?;
    let base = response.get_url().to_owned();
    let mut text = String::new();
    response.into_reader().take(MAX_PLAYLIST).read_to_string(&mut text).map_err(|e| format!("Không đọc được danh sách HLS: {e}"))?;
    Ok((base, text))
}

/// The media playlist at `url`, which a master must not lead to again. `origin` is the address the headers were
/// chosen for, so a cookie among them goes only to its host.
pub fn media_at(url: &str, origin: &str, headers: &[(String, String)]) -> Result<Media, String> {
    let (base, text) = fetch(url, origin, headers)?;
    match parse(&text, &base)? {
        Playlist::Media(media) => Ok(media),
        Playlist::Master { .. } => Err("Danh sách HLS lồng nhau, bản này chưa hỗ trợ".into()),
    }
}

/// The segments to download for the playlist at `url`; a master is followed to its best resolution.
pub fn load(url: &str, headers: &[(String, String)]) -> Result<Media, String> {
    let (base, text) = fetch(url, url, headers)?;
    match parse(&text, &base)? {
        Playlist::Media(media) => Ok(media),
        Playlist::Master { variants, .. } => media_at(&variants[0].url, url, headers),
    }
}

/// The resolutions the extension offers for the playlist at `url`. A media playlist is one choice of unknown
/// resolution; for a master, its best resolution is read once for the length and the file type.
pub fn choices(url: &str, headers: &[(String, String)]) -> Result<Choices, String> {
    let (base, text) = fetch(url, url, headers)?;
    match parse(&text, &base)? {
        Playlist::Media(media) => Ok(Choices {
            variants: vec![Variant { url: url.to_owned(), height: None, bandwidth: None, audio: None }],
            renditions: Vec::new(),
            duration: media.duration,
            ext: "mp4",
        }),
        Playlist::Master { variants, renditions } => {
            let media = media_at(&variants[0].url, url, headers)?;
            Ok(Choices { variants, renditions, duration: media.duration, ext: "mp4" })
        }
    }
}

/// The IV of a segment whose key gives none: its sequence number, big-endian over 16 bytes.
pub fn sequence_iv(sequence: u64) -> [u8; 16] {
    u128::from(sequence).to_be_bytes()
}

/// One AES-128-CBC segment back to its plain bytes, with the PKCS#7 padding taken off.
pub fn decrypt(key: &[u8], iv: [u8; 16], mut data: Vec<u8>) -> Result<Vec<u8>, String> {
    let key: [u8; 16] = key.try_into().map_err(|_| format!("Khóa giải mã dài {} byte, không phải 16", key.len()))?;
    let plain = cbc::Decryptor::<aes::Aes128>::new(&key.into(), &iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut data)
        .map_err(|_| "Không giải mã được đoạn video".to_owned())?
        .len();
    data.truncate(plain);
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER: &str = "#EXTM3U
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aac\",NAME=\"vi\",URI=\"audio/vi.m3u8\"
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aac\",NAME=\"en\",DEFAULT=YES,URI=\"audio/en.m3u8\"
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"low\",NAME=\"en\",URI=\"audio/low.m3u8\"
#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,CODECS=\"avc1.4d401e,mp4a.40.2\",AUDIO=\"low\"
360/index.m3u8
#EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=90000,URI=\"iframes.m3u8\"
#EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080,AUDIO=\"aac\"
https://cdn.example/1080/index.m3u8?t=1
#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720
/abs/720.m3u8
";

    #[test]
    fn a_master_lists_its_resolutions_best_first() {
        let Playlist::Master { variants, renditions } = parse(MASTER, "https://site.example/v/master.m3u8?k=2").unwrap() else {
            panic!("a master");
        };
        let heights: Vec<_> = variants.iter().map(|v| v.height).collect();
        assert_eq!(heights, vec![Some(1080), Some(720), Some(360)]);
        assert_eq!(variants[0].url, "https://cdn.example/1080/index.m3u8?t=1");
        assert_eq!(variants[0].audio.as_deref(), Some("https://site.example/v/audio/en.m3u8"), "its group's default sound");
        assert_eq!(variants[1].url, "https://site.example/abs/720.m3u8");
        assert_eq!(variants[1].audio, None, "the sound is in the picture's own segments");
        assert_eq!(variants[2].url, "https://site.example/v/360/index.m3u8");
        assert_eq!(variants[2].bandwidth, Some(800_000));
        assert_eq!(variants[2].audio.as_deref(), Some("https://site.example/v/audio/low.m3u8"), "a group with no default gives its first");
        assert_eq!(renditions.len(), 3);
    }

    #[test]
    fn a_media_playlist_gives_its_segments_keys_and_length() {
        let text = "#EXTM3U
#EXT-X-TARGETDURATION:6
#EXT-X-MEDIA-SEQUENCE:7
#EXTINF:6.0,
seg7.ts
#EXT-X-KEY:METHOD=AES-128,URI=\"../key.bin\",IV=0x000102030405060708090a0b0c0d0e0f
#EXTINF:4.5,title
seg8.ts
#EXT-X-KEY:METHOD=NONE
#EXTINF:2,
https://other.example/seg9.ts
#EXT-X-ENDLIST
";
        let Playlist::Media(media) = parse(text, "https://s.example/a/b/list.m3u8").unwrap() else { panic!("media") };
        assert_eq!(media.duration, 12.5);
        assert_eq!(media.segments.len(), 3);
        assert_eq!(media.segments[0], Segment { url: "https://s.example/a/b/seg7.ts".into(), range: None, key: None, sequence: 7 });
        let key = media.segments[1].key.as_ref().unwrap();
        assert_eq!(key.url, "https://s.example/a/key.bin");
        assert_eq!(key.iv, Some([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]));
        assert_eq!(media.segments[1].sequence, 8);
        assert_eq!(media.segments[2].key, None);
        let fmp4 = "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4,\n1.m4s\n#EXT-X-ENDLIST\n";
        let Playlist::Media(media) = parse(fmp4, "https://s.example/list.m3u8").unwrap() else { panic!("media") };
        assert_eq!(media.init.as_ref().unwrap().url, "https://s.example/init.mp4");
        let ranges = "#EXTM3U\n#EXT-X-MAP:URI=\"main.mp4\",BYTERANGE=\"720@0\"\n#EXTINF:4,\n#EXT-X-BYTERANGE:1000@720\nmain.mp4\n\
                      #EXTINF:4,\n#EXT-X-BYTERANGE:500\nmain.mp4\n#EXT-X-ENDLIST\n";
        let Playlist::Media(media) = parse(ranges, "https://s.example/list.m3u8").unwrap() else { panic!("media") };
        assert_eq!(media.init.unwrap().range, Some((0, 720)));
        assert_eq!(media.segments[0].range, Some((720, 1000)));
        assert_eq!(media.segments[1].range, Some((1720, 500)), "no offset follows on from the segment before");
    }

    #[test]
    fn drm_live_and_other_text_are_refused() {
        let drm = "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://x\"\n#EXTINF:4,\n1.ts\n#EXT-X-ENDLIST\n";
        assert!(parse(drm, "https://s.example/l.m3u8").unwrap_err().contains("DRM"));
        let live = "#EXTM3U\n#EXTINF:4,\n1.ts\n";
        assert!(parse(live, "https://s.example/l.m3u8").unwrap_err().contains("trực tiếp"));
        assert!(parse("<html>", "https://s.example/l.m3u8").is_err());
        assert!(parse("#EXTM3U\n#EXT-X-ENDLIST\n", "https://s.example/l.m3u8").is_err());
    }

    #[test]
    fn cookies_stay_on_their_own_host() {
        let headers = vec![("Cookie".to_string(), "s=1".to_string()), ("Referer".to_string(), "https://site/".to_string())];
        let names = |url| headers_for(url, "https://site.example/m.m3u8", &headers).map(|(n, _)| n.as_str()).collect::<Vec<_>>();
        assert_eq!(names("https://site.example/seg.ts"), vec!["Cookie", "Referer"]);
        assert_eq!(names("https://cdn.example/seg.ts"), vec!["Referer"]);
    }

    #[test]
    fn segments_decrypt_with_the_key_and_iv() {
        use cbc::cipher::BlockEncryptMut;
        let (key, iv) = ([7u8; 16], sequence_iv(42));
        let plain = b"0123456789 a segment that is not a whole block".to_vec();
        let mut locked = plain.clone();
        locked.resize(plain.len() + 16, 0);
        let len = cbc::Encryptor::<aes::Aes128>::new(&key.into(), &iv.into()).encrypt_padded_mut::<Pkcs7>(&mut locked, plain.len()).unwrap().len();
        locked.truncate(len);
        assert_eq!(decrypt(&key, iv, locked).unwrap(), plain);
        assert!(decrypt(&[1u8; 8], iv, vec![0; 16]).is_err(), "a key is 16 bytes");
        assert_eq!(sequence_iv(1)[15], 1);
    }
}
