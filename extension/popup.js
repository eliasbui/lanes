"use strict";

const capture = document.getElementById("capture");
const videoButton = document.getElementById("videoButton");
const status = document.getElementById("status");

chrome.storage.local.get(["capture", "videoButton"]).then(({ capture: on = true, videoButton: shown = true }) => {
  capture.checked = on;
  videoButton.checked = shown;
});

capture.addEventListener("change", () => {
  chrome.storage.local.set({ capture: capture.checked });
});

videoButton.addEventListener("change", () => {
  chrome.storage.local.set({ videoButton: videoButton.checked });
});

// Whether the app answers, and which version: the extension and the app go together.
chrome.runtime.sendNativeMessage("com.eliasbui.lanes", { cmd: "ping" }, (reply) => {
  const ok = !chrome.runtime.lastError && reply?.ok;
  status.textContent = ok ? `Đã kết nối với Lanes ${reply.version}.` : "Chưa kết nối được với app Lanes. Hãy mở Lanes một lần rồi thử lại.";
});
