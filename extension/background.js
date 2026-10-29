// Lanes for Chrome, Edge, Brave and Coc Coc. It takes a download the browser started and hands it to the Lanes app
// through native messaging, with the page's Referer, Cookie and User-Agent so a site that checks them serves the
// file to Lanes as it did to the browser (src/browser.rs). A right-click menu sends a link on demand; auto-capture,
// on by default, sends ordinary downloads. Nothing leaves the machine: the message goes to the local app only.
"use strict";

const HOST = "com.eliasbui.lanes";
const MENU = "lanes-download";

// File types the browser is better left to handle, and pages that are not real downloads.
const SKIP = /\.(html?|xml|json|css|js|txt)(\?|$)/i;

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({ id: MENU, title: "Tải bằng Lanes", contexts: ["link"] });
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
      }
    });
  });
}

function note(ok, text) {
  chrome.notifications.create({
    type: "basic",
    iconUrl: "icon-128.png",
    title: ok ? "Đã gửi tới Lanes" : "Lanes",
    message: text,
  });
}

// Sends one download to the app with the headers the page used.
async function toLanes(url, filename, referrer) {
  const message = { url, filename: filename || "", referrer: referrer || "", cookie: await cookieHeader(url), userAgent: navigator.userAgent };
  const reply = await send(message);
  const name = (filename || url).split("/").pop().slice(0, 40);
  note(!!reply.ok, reply.ok ? `Đang tải "${name}"` : reply.error || "Lanes không nhận được liên kết.");
  return reply.ok;
}

// The right-click menu on a link: always send it, whatever the auto-capture setting.
chrome.contextMenus.onClicked.addListener((info, tab) => {
  if (info.menuItemId === MENU && info.linkUrl) {
    toLanes(info.linkUrl, "", info.pageUrl || tab?.url || "");
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
  if (sent) {
    chrome.downloads.cancel(item.id, () => {
      void chrome.runtime.lastError;
      chrome.downloads.erase({ id: item.id }, () => void chrome.runtime.lastError);
    });
  }
});
