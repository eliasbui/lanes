// Lanes for Chrome, Edge, Brave and Coc Coc. It takes a download the browser started and hands it to the Lanes app
// through native messaging, with the page's Referer, Cookie and User-Agent so a site that checks them serves the
// file to Lanes as it did to the browser (src/browser.rs). A right-click menu sends a link or a video on demand;
// auto-capture, on by default, sends ordinary downloads. It also notes the video files and HLS playlists each tab
// loads, which the video button (content.js) offers by resolution; YouTube, and a page where it finds none, the app
// reads through yt-dlp (src/ytdlp.rs). Nothing leaves the machine: the message goes to
// the local app only.
"use strict";

const HOST = "com.eliasbui.lanes";
const MENU = "lanes-download";

// File types the browser is better left to handle, and pages that are not real downloads.
const SKIP = /\.(html?|xml|json|css|js|txt)(\?|$)/i;
// What the video button can offer: a whole video file the page played, or an HLS playlist.
const VIDEO_FILE = /\.(mp4|m4v|webm|mov|mkv|flv|avi|3gp|ogv)$/i;
const HLS_FILE = /\.m3u8$/i;
// YouTube, whose player Lanes cannot read itself, and the addresses of its single videos there.
const YOUTUBE = /^https?:\/\/([^/]+\.)?(youtube\.com|youtube-nocookie\.com|youtu\.be)\//i;
const YOUTUBE_VIDEO = /^https?:\/\/[^/]+\/(watch\b|shorts\/|embed\/|live\/)|youtu\.be\/./i;
// The most media remembered per tab; the oldest goes first.
const MAX_MEDIA = 40;

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({ id: MENU, title: "Tải bằng Lanes", contexts: ["link", "video", "audio"] });
});

async function autoCapture() {
  const { capture = true } = await chrome.storage.local.get("capture");
  return capture;
}

// The cookies a request to this URL would carry, as one header value.
async function cookieHeader(url) {
  try {
    const cookies = await chrome.cookies.getAll({ url });
    return cookies.map((c) => `${c.name}=${c.value}`).join("; ");
  } catch {
    return "";
  }
}

function send(message) {
  return new Promise((resolve) => {
    chrome.runtime.sendNativeMessage(HOST, message, (reply) => {
      if (chrome.runtime.lastError || !reply) {
        resolve({ ok: false, error: "Chưa kết nối được với Lanes. Hãy mở app Lanes một lần để app tự đăng ký với trình duyệt, rồi thử lại." });
      } else {
        resolve(reply);
        catchUp(reply.version);
      }
    });
  });
}

// Whether version `a` is above `b`, number by number.
function newer(a, b) {
  const x = a.split(".").map(Number);
  const y = b.split(".").map(Number);
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    if ((x[i] || 0) !== (y[i] || 0)) return (x[i] || 0) > (y[i] || 0);
  }
  return false;
}

// The app rewrites this extension's files when it updates (src/browser.rs). When it answers with a newer version than
// this one, the extension reloads itself from those files, so the browser need not restart. Once per version: a copy
// loaded from another folder would come back as the same version, and must not reload again and again. The reload
// waits a moment so the answer in hand still reaches the page.
async function catchUp(version) {
  if (typeof version !== "string" || !newer(version, chrome.runtime.getManifest().version)) return;
  const { reloadedFor } = await chrome.storage.local.get("reloadedFor");
  if (reloadedFor === version) return;
  await chrome.storage.local.set({ reloadedFor: version });
  setTimeout(() => chrome.runtime.reload(), 3000);
}

function note(ok, text) {
  chrome.notifications.create({
    type: "basic",
    iconUrl: "icon-128.png",
    title: ok ? "Đã gửi tới Lanes" : "Lanes",
    message: text,
  });
}

// Sends one download to the app with the headers the page used. `extra` says what else the video button knows: its
// `kind` ("hls" for an HLS playlist, "page" for a video page yt-dlp reads), the yt-dlp `format` and the `size`.
async function toLanes(url, filename, referrer, extra = {}) {
  const message = { url, filename: filename || "", referrer: referrer || "", cookie: await cookieHeader(url), userAgent: navigator.userAgent, ...extra };
  const reply = await send(message);
  const name = (filename || url).split("/").pop().slice(0, 40);
  note(!!reply.ok, reply.ok ? `Đang tải "${name}"` : reply.error || "Lanes không nhận được liên kết.");
  return reply;
}

// The right-click menu on a link or a video: always send it, whatever the auto-capture setting.
chrome.contextMenus.onClicked.addListener((info, tab) => {
  const url = info.linkUrl || info.srcUrl || "";
  if (info.menuItemId === MENU && /^https?:\/\//i.test(url)) {
    toLanes(url, "", info.pageUrl || tab?.url || "");
  }
});

// A download the browser started: if auto-capture is on and it is a real file, cancel the browser's copy and let
// Lanes take it instead. The browser download is erased so it leaves no half file in the Downloads folder.
chrome.downloads.onCreated.addListener(async (item) => {
  if (!(await autoCapture())) return;
  const url = item.finalUrl || item.url || "";
  if (!/^https?:\/\//i.test(url)) return;
  if (SKIP.test(url) && !item.filename) return;
  // Only in-progress downloads the browser has not finished; a resumed or already-done one is left alone.
  if (item.state && item.state !== "in_progress") return;
  const filename = (item.filename || "").split(/[\\/]/).pop() || "";
  const sent = await toLanes(url, filename, item.referrer || "");
  if (sent.ok) {
    chrome.downloads.cancel(item.id, () => {
      void chrome.runtime.lastError;
      chrome.downloads.erase({ id: item.id }, () => void chrome.runtime.lastError);
    });
  }
});

// The media each tab loaded, as { url, kind: "file" | "hls", size, frameId }. Kept in session storage too, since the
// service worker sleeps between events and would forget it.
const media = new Map();
const keyOf = (tabId) => `media:${tabId}`;

function mediaOf(tabId) {
  if (!media.has(tabId)) {
    const key = keyOf(tabId);
    media.set(tabId, chrome.storage.session.get(key).then((got) => got[key] || [], () => []));
  }
  return media.get(tabId);
}

async function remember(tabId, item) {
  const list = await mediaOf(tabId);
  const known = list.find((m) => m.url === item.url);
  if (known) {
    known.size = item.size || known.size;
  } else {
    list.push(item);
    if (list.length > MAX_MEDIA) list.shift();
  }
  chrome.storage.session.set({ [keyOf(tabId)]: list }).catch(() => {});
}

function forget(tabId) {
  media.delete(tabId);
  chrome.storage.session.remove(keyOf(tabId)).catch(() => {});
}

chrome.tabs.onUpdated.addListener((tabId, change) => {
  if (change.status === "loading") forget(tabId);
});
chrome.tabs.onRemoved.addListener(forget);

function header(details, name) {
  return details.responseHeaders?.find((h) => h.name.toLowerCase() === name)?.value || "";
}

// The whole file's size: the total of a range answer, or the length of a whole one.
function sizeOf(details) {
  const total = /\/(\d+)\s*$/.exec(header(details, "content-range"));
  if (total) return Number(total[1]);
  return details.statusCode === 200 ? Number(header(details, "content-length")) || 0 : 0;
}

// A whole video file is what a <video> element itself requests. The pieces a streaming player (YouTube, DASH, the
// segments of HLS) fetches go through XHR or fetch instead, so they never count as files here.
chrome.webRequest.onHeadersReceived.addListener(
  (details) => {
    if (details.tabId < 0 || (details.statusCode !== 200 && details.statusCode !== 206)) return;
    const path = details.url.split(/[?#]/)[0];
    const type = header(details, "content-type").toLowerCase();
    let kind = "";
    if (HLS_FILE.test(path) || type.includes("mpegurl")) kind = "hls";
    else if (details.type === "media" && (VIDEO_FILE.test(path) || type.startsWith("video/"))) kind = "file";
    if (!kind) return;
    remember(details.tabId, { url: details.url, kind, size: kind === "file" ? sizeOf(details) : 0, frameId: details.frameId });
  },
  { urls: ["http://*/*", "https://*/*"], types: ["media", "xmlhttprequest", "other"] },
  ["responseHeaders"]
);

function human(n) {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return (i === 0 ? n : n.toFixed(n < 10 ? 1 : 0)) + " " + units[i];
}

// A file name from the page title, the resolution and the type: "Page title 720p.mp4".
function nameFor(title, height, ext) {
  const base = (title || "").replace(/[\\/:*?"<>|\u0000-\u001f]+/g, " ").replace(/\s+/g, " ").trim().slice(0, 120) || "video";
  return `${base}${height ? ` ${height}p` : ""}.${ext}`;
}

function fileItem(m, title) {
  const ext = (VIDEO_FILE.exec(m.url.split(/[?#]/)[0])?.[1] || "mp4").toLowerCase();
  const height = m.height || Number(/(?:^|\D)(\d{3,4})p(?![a-z])/i.exec(m.url)?.[1]) || 0;
  return {
    kind: "file",
    url: m.url,
    height,
    size: m.size || 0,
    label: height ? `${height}p` : ext.toUpperCase(),
    detail: [height ? ext.toUpperCase() : "", m.size ? human(m.size) : ""].filter(Boolean).join(" · "),
    filename: nameFor(title, height, ext),
  };
}

function hlsItem(v, reply, title) {
  const size = v.bandwidth && reply.duration ? (v.bandwidth * reply.duration) / 8 : 0;
  return {
    kind: "hls",
    url: v.url,
    height: v.height || 0,
    size,
    label: v.height ? `${v.height}p` : "HLS",
    detail: [v.height ? "HLS" : "", size ? `khoảng ${human(size)}` : "", v.separate_audio ? "không kèm tiếng" : ""].filter(Boolean).join(" · "),
    filename: nameFor(title, v.height, reply.ext),
  };
}

function pageItem(c, reply, url) {
  return {
    kind: "page",
    url,
    format: c.format,
    height: c.height || 0,
    size: c.size || 0,
    label: c.height ? `${c.height}p` : "Chỉ âm thanh",
    detail: [c.ext.toUpperCase(), c.size ? `khoảng ${human(c.size)}` : ""].filter(Boolean).join(" · "),
    filename: nameFor(reply.title, c.height, c.ext),
  };
}

// What the video button offers for one video. A video playing a file (`direct`) offers its own sources only. A
// streaming one offers the media its frame loaded (or, when that frame loaded none, the whole tab's), less the files
// `others` videos play. Each HLS playlist is opened up into its resolutions by the app.
async function streams(sender, { sources = [], direct = false, others = [], title = "" }) {
  const all = sender.tab ? await mediaOf(sender.tab.id) : [];
  let found = [];
  if (!direct) {
    found = all.filter((m) => m.frameId === sender.frameId);
    if (!found.length) found = all;
    found = found.filter((m) => !others.includes(m.url)).map((m) => ({ ...m }));
  }
  for (const s of sources) {
    const known = found.find((m) => m.url === s.url);
    const seen = all.find((m) => m.url === s.url);
    if (known) known.height = known.height || s.height;
    else found.push({ url: s.url, kind: HLS_FILE.test(s.url.split(/[?#]/)[0]) ? "hls" : "file", size: seen?.size || 0, height: s.height });
  }

  const referrer = sender.url || sender.tab?.url || "";
  const items = found.filter((m) => m.kind === "file").map((m) => fileItem(m, title));
  const notes = [];
  const playlists = found.filter((m) => m.kind === "hls");
  const replies = await Promise.all(
    playlists.map(async (m) => ({
      m,
      reply: await send({ cmd: "variants", url: m.url, referrer, cookie: await cookieHeader(m.url), userAgent: navigator.userAgent }),
    }))
  );
  // A playlist a master names (one of its resolutions, or its sound) is offered through the master only.
  const named = new Set();
  for (const { m, reply } of replies) {
    if (reply.ok && reply.variants.some((v) => v.url !== m.url)) {
      reply.variants.forEach((v) => named.add(v.url));
      reply.renditions.forEach((url) => named.add(url));
    }
  }
  for (const { m, reply } of replies) {
    if (!reply.ok) notes.push(reply.error);
    else if (!named.has(m.url)) items.push(...reply.variants.map((v) => hlsItem(v, reply, title)));
  }

  // YouTube, or a page where nothing was found above: the app asks yt-dlp. Elsewhere its failure says no more than
  // "nothing found", unless yt-dlp is missing.
  const youtube = YOUTUBE.test(referrer);
  if (youtube && !YOUTUBE_VIDEO.test(referrer)) {
    notes.push("Hãy mở video này ra trang riêng của nó để tải.");
  } else if (youtube || !items.length) {
    // An app before 0.4 does not know "page" and would take it for a download of the page itself.
    const app = await send({ cmd: "ping" });
    const reply = app.ok && newer("0.4.0", app.version || "0")
      ? { ok: false, missing: true, error: "Cập nhật app Lanes lên bản mới nhất để tải video từ trang này." }
      : app.ok ? await send({ cmd: "page", url: referrer }) : app;
    if (reply.ok) {
      items.push(...reply.choices.map((c) => pageItem(c, reply, referrer)));
      if (reply.hint) notes.push(reply.hint);
    } else if (youtube || reply.missing) {
      notes.push(reply.error);
    }
  }

  const unique = items.filter((item, i) => items.findIndex((other) => other.url === item.url && other.format === item.format) === i);
  unique.sort((a, b) => b.height - a.height || b.size - a.size);
  return { items: unique, notes: [...new Set(notes)] };
}

chrome.runtime.onMessage.addListener((message, sender, reply) => {
  if (message?.type === "ping") {
    send({ cmd: "ping" }).then(reply);
    return true;
  }
  if (message?.type === "streams") {
    streams(sender, message).then(reply, () => reply({ items: [], notes: ["Không đọc được danh sách video."] }));
    return true;
  }
  if (message?.type === "download") {
    const { url = "", filename = "", kind = "", format = "", size = 0 } = message.item || {};
    const extra = kind === "hls" ? { kind } : kind === "page" ? { kind, format, size } : {};
    toLanes(url, filename, sender.url || sender.tab?.url || "", extra).then(reply);
    return true;
  }
});
