// The video button of Lanes. Over a video the pointer is on, a small glass button offers the resolutions Lanes can
// download: the files and HLS playlists the page loaded, which background.js keeps per tab, and the video's own
// sources. Picking one sends it to the app like any other download. It draws in a closed shadow root, so the page's
// styles neither reach it nor are touched by it, and it can be turned off in the popup.
"use strict";

(() => {
  // A video smaller than this is a thumbnail or a preview, not one to download.
  const MIN_WIDTH = 200;
  const MIN_HEIGHT = 120;
  const HIDE_AFTER = 1500;
  // The button's height and its distance from the video when it sits under it.
  const BUTTON = 32;
  const GAP = 6;
  // Room left for a player's own control bar along the bottom of its video.
  const CONTROLS = 64;

  let enabled = true;
  let video = null;
  let menuOpen = false;
  let hideTimer = 0;
  let frame = 0;
  let overButton = false;

  chrome.storage.local.get("videoButton").then(({ videoButton = true }) => (enabled = videoButton), () => {});
  chrome.storage.onChanged.addListener((changes) => {
    if (!changes.videoButton) return;
    enabled = changes.videoButton.newValue !== false;
    if (!enabled) hide();
  });

  const host = document.createElement("lanes-video-button");
  host.style.cssText = "all: initial; position: fixed; top: 0; left: 0; z-index: 2147483647; display: none;";
  const root = host.attachShadow({ mode: "closed" });
  root.innerHTML = `
    <style>
      .box {
        position: absolute; top: 0; left: 0; transform: translateX(-100%);
        font: 13px/1.35 -apple-system, BlinkMacSystemFont, "SF Pro Text", "Segoe UI", system-ui, sans-serif;
        color: #fff; -webkit-font-smoothing: antialiased;
      }
      .go {
        all: unset; cursor: pointer; white-space: nowrap; box-sizing: border-box; height: ${BUTTON}px;
        display: inline-flex; align-items: center; gap: 7px;
        padding: 0 13px 0 6px; border-radius: 999px; font-weight: 600; letter-spacing: -0.1px;
        transition: background 0.15s ease, transform 0.12s ease;
      }
      /* The Lanes mark, as in the app: a white arrow on a blue tile. */
      .mark {
        width: 20px; height: 20px; border-radius: 6px; flex: none; display: grid; place-items: center;
        background: linear-gradient(160deg, #2b93ff, #0040dd);
        box-shadow: 0 2px 6px rgba(10, 70, 200, 0.45), inset 0 1px 0 rgba(255, 255, 255, 0.45);
      }
      .mark svg { width: 13px; height: 13px; }
      .glass {
        background: rgba(22, 24, 32, 0.74);
        border: 1px solid rgba(255, 255, 255, 0.2);
        box-shadow: 0 10px 30px rgba(0, 0, 0, 0.35), inset 0 1px 0 rgba(255, 255, 255, 0.22);
        backdrop-filter: blur(22px) saturate(180%);
        -webkit-backdrop-filter: blur(22px) saturate(180%);
      }
      .go.glass:hover { background: rgba(10, 132, 255, 0.82); }
      .go:active { transform: scale(0.97); }
      .go:focus-visible, .item:focus-visible { outline: 2px solid #0a84ff; outline-offset: 2px; }
      .menu {
        position: absolute; right: 0; top: calc(100% + 6px);
        box-sizing: border-box; min-width: 240px; max-width: 340px; max-height: 320px; overflow-y: auto;
        padding: 6px; border-radius: 14px;
      }
      /* The menu opens toward the side of the button with more room in the window, upward over the video as a rule. */
      .box.up .menu { top: auto; bottom: calc(100% + 6px); }
      .menu[hidden] { display: none; }
      .item {
        all: unset; box-sizing: border-box; cursor: pointer; width: 100%;
        display: flex; align-items: baseline; justify-content: space-between; gap: 14px;
        padding: 8px 10px; border-radius: 9px;
      }
      .item:hover { background: rgba(10, 132, 255, 0.85); }
      .item b { font-weight: 650; white-space: nowrap; }
      .item span { opacity: 0.75; font-size: 12px; text-align: right; }
      .note { padding: 8px 10px; opacity: 0.82; font-size: 12.5px; }
    </style>
    <div class="box">
      <button class="go glass" type="button" aria-haspopup="menu">
        <span class="mark"><svg viewBox="0 0 24 24" fill="none" aria-hidden="true"><path d="M12 3v11m0 0l-4.5-4.5M12 14l4.5-4.5" stroke="#fff" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round"/><path d="M5 19h14" stroke="#fff" stroke-width="2.6" stroke-linecap="round"/></svg></span>
        Tải video
      </button>
      <div class="menu glass" role="menu" hidden></div>
    </div>`;
  const box = root.querySelector(".box");
  const go = root.querySelector(".go");
  const menu = root.querySelector(".menu");

  // The largest big-enough video under the point. Players lay controls and overlays over their video, so the point
  // is tested against each video's box rather than taken from the element under the pointer.
  function videoAt(x, y) {
    let best = null;
    let area = 0;
    for (const v of document.getElementsByTagName("video")) {
      const r = v.getBoundingClientRect();
      if (r.width < MIN_WIDTH || r.height < MIN_HEIGHT) continue;
      if (x < r.left || x > r.right || y < r.top || y > r.bottom) continue;
      if (r.width * r.height > area) {
        best = v;
        area = r.width * r.height;
      }
    }
    return best;
  }

  // The button sits just under the video's bottom-right corner, as download managers do. A video that reaches the
  // bottom of the window (a tall one, or fullscreen) has no room under it, so there the button sits inside its
  // bottom-right corner, above the player's control bar.
  function place() {
    if (!video?.isConnected) return hide();
    const r = video.getBoundingClientRect();
    if (r.width < MIN_WIDTH || r.height < MIN_HEIGHT) return hide();
    const right = Math.min(r.right, document.documentElement.clientWidth);
    const under = r.bottom + GAP + BUTTON <= window.innerHeight;
    const top = under ? r.bottom + GAP : Math.max(Math.min(r.bottom, window.innerHeight) - CONTROLS - BUTTON, r.top + 10);
    const above = top - GAP;
    const below = window.innerHeight - top - BUTTON - GAP;
    box.classList.toggle("up", above >= below);
    menu.style.maxHeight = `${Math.max(Math.min(320, Math.max(above, below) - 8), 80)}px`;
    host.style.top = `${top}px`;
    host.style.left = `${under ? right : right - 10}px`;
  }

  function show(v) {
    clearTimeout(hideTimer);
    hideTimer = 0;
    if (v !== video) {
      closeMenu();
      video = v;
    }
    // In fullscreen only the fullscreen element and what is inside it is drawn.
    const full = document.fullscreenElement;
    const parent = full && full.tagName !== "VIDEO" ? full : document.documentElement;
    if (host.parentNode !== parent) parent.appendChild(host);
    host.style.display = "block";
    place();
  }

  function hide() {
    clearTimeout(hideTimer);
    hideTimer = 0;
    closeMenu();
    host.style.display = "none";
    video = null;
  }

  function hideSoon() {
    if (menuOpen || hideTimer || host.style.display === "none") return;
    hideTimer = setTimeout(hide, HIDE_AFTER);
  }

  document.addEventListener(
    "mousemove",
    (e) => {
      if (!enabled || frame) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        const v = videoAt(e.clientX, e.clientY);
        if (v) show(v);
        else if (!overButton) hideSoon();
      });
    },
    { capture: true, passive: true }
  );
  // Under the video the button is outside the video's box, so being on it keeps it shown.
  host.addEventListener("pointerenter", () => {
    overButton = true;
    clearTimeout(hideTimer);
    hideTimer = 0;
  });
  host.addEventListener("pointerleave", () => {
    overButton = false;
    hideSoon();
  });
  const follow = () => host.style.display !== "none" && place();
  window.addEventListener("scroll", follow, { capture: true, passive: true });
  window.addEventListener("resize", follow, { passive: true });
  document.addEventListener("fullscreenchange", () => video && show(video));

  function closeMenu() {
    menuOpen = false;
    menu.hidden = true;
    menu.textContent = "";
  }

  function note(text) {
    const div = document.createElement("div");
    div.className = "note";
    div.textContent = text;
    menu.append(div);
  }

  // The video's own addresses, with any resolution its <source> tags state.
  function sourcesOf(v) {
    const out = [];
    const add = (url, height) => {
      if (/^https?:\/\//i.test(url || "") && !out.some((s) => s.url === url)) out.push({ url, height: height || 0 });
    };
    add(v.currentSrc, v.videoHeight);
    add(v.src, 0);
    for (const s of v.querySelectorAll("source")) {
      const hint = ["size", "res", "label", "title", "data-res", "data-quality", "data-label"].map((a) => s.getAttribute(a) || "").join(" ");
      add(s.src, Number(/(\d{3,4})/.exec(hint)?.[1]) || 0);
    }
    return out;
  }

  const nothingFound = "Chưa thấy bản tải được. Hãy phát video vài giây rồi bấm lại. Video có DRM thì không tải được.";

  async function openMenu() {
    menuOpen = true;
    menu.hidden = false;
    menu.textContent = "";
    note("Đang tìm các bản tải được…");
    let reply;
    try {
      // A video playing a file of its own offers only its own sources; a streaming one (a blob: address) offers what
      // the page loaded, less the files other videos on the page are playing.
      const others = [...document.getElementsByTagName("video")].filter((v) => v !== video).map((v) => v.currentSrc);
      const direct = /^https?:\/\//i.test(video.currentSrc);
      reply = await chrome.runtime.sendMessage({ type: "streams", sources: sourcesOf(video), direct, others, title: document.title });
    } catch {
      reply = { items: [], notes: ["Tiện ích Lanes vừa được cập nhật. Hãy tải lại trang."] };
    }
    if (!menuOpen) return;
    menu.textContent = "";
    for (const item of reply?.items || []) {
      const button = document.createElement("button");
      button.className = "item";
      button.type = "button";
      button.setAttribute("role", "menuitem");
      const label = document.createElement("b");
      label.textContent = item.label;
      const detail = document.createElement("span");
      detail.textContent = item.detail;
      button.append(label, detail);
      button.addEventListener("click", (e) => {
        e.stopPropagation();
        pick(item);
      });
      menu.append(button);
    }
    const notes = reply?.notes || [];
    if (!reply?.items?.length && !notes.length) note(nothingFound);
    notes.forEach(note);
  }

  async function pick(item) {
    menu.textContent = "";
    note("Đang gửi tới Lanes…");
    let reply;
    try {
      reply = await chrome.runtime.sendMessage({ type: "download", item });
    } catch {
      reply = { ok: false, error: "Tiện ích Lanes vừa được cập nhật. Hãy tải lại trang." };
    }
    menu.textContent = "";
    note(reply?.ok ? `Đã gửi tới Lanes: ${item.filename}` : reply?.error || "Lanes không nhận được video.");
    setTimeout(() => menuOpen && closeMenu(), 2500);
  }

  go.addEventListener("click", (e) => {
    e.preventDefault();
    e.stopPropagation();
    if (menuOpen) closeMenu();
    else openMenu();
  });
  document.addEventListener("keydown", (e) => e.key === "Escape" && menuOpen && closeMenu(), true);
  document.addEventListener("pointerdown", (e) => menuOpen && !e.composedPath().includes(host) && closeMenu(), true);
})();
