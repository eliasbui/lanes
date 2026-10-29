"use strict";

const capture = document.getElementById("capture");
const videoButton = document.getElementById("videoButton");
const status = document.getElementById("status");
const dot = document.getElementById("dot");
const downloads = document.getElementById("downloads");

// The engine's states (src/engine.rs).
const QUEUED = 0;
const RUNNING = 1;
const PAUSED = 2;
const DONE = 3;

const ICON = {
  down: '<path d="M12 4.5v10m0 0l-4.2-4.2M12 14.5l4.2-4.2"/><path d="M5.5 19.5h13"/>',
  check: '<path d="M6 12.5l4 4L18.5 8"/>',
  alert: '<path d="M12 7v6.5"/><path d="M12 17.2v.1"/>',
  pause: '<path d="M9.5 7v10M14.5 7v10"/>',
  folder: '<path d="M3.5 8a2 2 0 012-2h3.8l2 2h7.2a2 2 0 012 2v7a2 2 0 01-2 2h-13a2 2 0 01-2-2z"/>',
};
const svg = (name, size = 15) =>
  `<svg width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${ICON[name]}</svg>`;

chrome.storage.local.get(["capture", "videoButton"]).then(({ capture: on = true, videoButton: shown = true }) => {
  capture.checked = on;
  videoButton.checked = shown;
});
capture.addEventListener("change", () => chrome.storage.local.set({ capture: capture.checked }));
videoButton.addEventListener("change", () => chrome.storage.local.set({ videoButton: videoButton.checked }));

function human(n) {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return (i === 0 ? n : n.toFixed(n < 10 ? 1 : 0)) + " " + units[i];
}

function row(item) {
  const div = document.createElement("div");
  div.className = "item";
  const size = item.size || 0;
  const pct = size ? Math.min(100, Math.floor((100 * item.downloaded) / size)) : 0;
  const state = item.state === DONE ? "done" : item.state === PAUSED ? "paused" : item.state > DONE ? "failed" : "running";
  const icon = { done: "check", paused: "pause", failed: "alert", running: "down" }[state];
  const running = item.state === QUEUED || item.state === RUNNING;
  const speed = item.speed ? ` · ${human(item.speed)}/s` : "";
  div.innerHTML = `<span class="state ${state}">${svg(icon)}</span><div class="body"><div class="name"></div>${running ? '<div class="bar"><span></span></div>' : ""}<div class="meta"></div></div>`;
  div.querySelector(".name").textContent = item.filename || item.name;
  div.title = item.filename || item.name;
  if (running) {
    const bar = div.querySelector(".bar");
    if (size) bar.firstChild.style.width = `${pct}%`;
    else bar.classList.add("wait");
  }
  div.querySelector(".meta").textContent =
    state === "done" ? human(size || item.downloaded)
    : state === "failed" ? item.error || "Không tải được"
    : state === "paused" ? `Tạm dừng · ${human(item.downloaded)}`
    : size ? `${pct}% · ${human(item.downloaded)} / ${human(size)}${speed}`
    : `${human(item.downloaded)}${speed}`;
  if (state === "done") {
    const open = document.createElement("button");
    open.className = "open";
    open.type = "button";
    open.title = "Mở thư mục chứa file";
    open.setAttribute("aria-label", "Mở thư mục chứa file");
    open.innerHTML = svg("folder", 17);
    open.addEventListener("click", () => chrome.runtime.sendMessage({ type: "reveal", id: item.id }).catch(() => {}));
    div.append(open);
  }
  return div;
}

// The app's latest downloads, live while the popup is open.
function show({ recent = [], live = false } = {}) {
  downloads.textContent = "";
  if (!recent.length) {
    downloads.innerHTML = `<div class="empty">${svg("down", 22)}<span>Chưa có download nào</span></div>`;
  }
  recent.slice(0, 6).forEach((item) => downloads.append(row(item)));
  if (recent.length) dot.classList.toggle("on", live);
}

chrome.runtime.onMessage.addListener((message) => message?.type === "progress" && show(message));
chrome.runtime.sendMessage({ type: "progress?", watch: true }).then(show, () => show());

// Whether the app answers, and which version. It goes through the background, which also brings the extension up to
// the app's version when the app has updated.
chrome.runtime.sendMessage({ type: "ping" }).then(
  (reply) => connected(!!reply?.ok, reply?.version),
  () => connected(false)
);

function connected(ok, version) {
  dot.classList.toggle("on", ok);
  dot.setAttribute("aria-label", ok ? "Đã kết nối với app" : "Chưa kết nối với app");
  document.getElementById("version").textContent = ok && version ? version : "";
  status.hidden = ok;
  status.textContent = ok ? "" : "Chưa kết nối được với app Lanes. Hãy mở Lanes một lần rồi thử lại.";
}
