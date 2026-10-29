"use strict";

const box = document.getElementById("capture");

chrome.storage.local.get("capture").then(({ capture = true }) => {
  box.checked = capture;
});

box.addEventListener("change", () => {
  chrome.storage.local.set({ capture: box.checked });
});
