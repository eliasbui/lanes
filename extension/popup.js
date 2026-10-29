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

// Whether the app answers, and which version. It goes through the background, which also brings the extension up to
// the app's version when the app has updated.
chrome.runtime.sendMessage({ type: "ping" }).then(
  (reply) => {
    status.textContent = reply?.ok ? `Đã kết nối với Lanes ${reply.version}.` : "Chưa kết nối được với app Lanes. Hãy mở Lanes một lần rồi thử lại.";
  },
  () => (status.textContent = "Chưa kết nối được với app Lanes. Hãy mở Lanes một lần rồi thử lại.")
);
