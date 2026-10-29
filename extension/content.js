// The video button of Lanes and the dock of its downloads. Over a video, and for a moment when one starts playing, a
// round glass button in its top-right corner offers the resolutions Lanes can download: the files and HLS playlists
// the page loaded, which background.js keeps per tab, and the video's own sources. Picking one sends it to the app
// like any other download, and an arrow flies to the corner where the dock shows it: a ring fills with its progress,
// and turns into a tick when it is done. Both draw in closed shadow roots, so the page's styles neither reach them nor
// are touched by them. The button can be turned off in the popup.
"use strict";

(() => {
  // A video smaller than this is a thumbnail or a preview, not one to download.
  const MIN_WIDTH = 200;
  const MIN_HEIGHT = 120;
  const HIDE_AFTER = 1500;
  // How long the button stays once a video starts playing, so it is seen without looking for it.
  const SHOW_ON_PLAY = 4000;
  // The button's size and its distance from the video's corner.
  const BUTTON = 40;
  const INSET = 12;
  const GAP = 8;
  const RING = 2 * Math.PI * 15.5;

  const ICON = {
    down: '<path d="M12 4.5v10m0 0l-4.2-4.2M12 14.5l4.2-4.2"/><path d="M5.5 19.5h13"/>',
    check: '<path class="draw" d="M6 12.5l4 4L18.5 8"/>',
    alert: '<path d="M12 7v6.5"/><path d="M12 17.2v.1"/>',
    pause: '<path d="M9.5 7v10M14.5 7v10"/>',
    folder: '<path d="M3.5 8a2 2 0 012-2h3.8l2 2h7.2a2 2 0 012 2v7a2 2 0 01-2 2h-13a2 2 0 01-2-2z"/>',
    close: '<path d="M7.5 7.5l9 9M16.5 7.5l-9 9"/>',
    audio: '<path d="M9.5 17.5V6.5l9-2v11"/><circle cx="7" cy="17.5" r="2.5"/><circle cx="16" cy="15.5" r="2.5"/>',
  };
  const svg = (name, cls = "") => `<svg class="${cls}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${ICON[name]}</svg>`;

  // The glass both draw with: dark over a video, and in the page's own light or dark for the dock.
  const GLASS = `
    .glass {
      background: rgba(22, 24, 32, 0.74); color: #fff;
      border: 1px solid rgba(255, 255, 255, 0.2);
      box-shadow: 0 10px 30px rgba(0, 0, 0, 0.35), inset 0 1px 0 rgba(255, 255, 255, 0.22);
      backdrop-filter: blur(22px) saturate(180%); -webkit-backdrop-filter: blur(22px) saturate(180%);
    }
    .tile {
      display: grid; place-items: center; border-radius: 50%; color: #fff;
      background: linear-gradient(160deg, #2b93ff, #0040dd);
      box-shadow: 0 4px 14px rgba(10, 70, 200, 0.5), inset 0 1px 0 rgba(255, 255, 255, 0.45);
    }
    svg { display: block; }
    * { box-sizing: border-box; }
    .font { font: 13px/1.35 -apple-system, BlinkMacSystemFont, "SF Pro Text", "Segoe UI", system-ui, sans-serif; -webkit-font-smoothing: antialiased; }`;

  function human(n) {
    const units = ["B", "KB", "MB", "GB", "TB"];
    let i = 0;
    while (n >= 1024 && i < units.length - 1) {
      n /= 1024;
      i++;
    }
    return (i === 0 ? n : n.toFixed(n < 10 ? 1 : 0)) + " " + units[i];
  }

  // The resolution as players name it: 4K and 2K for the big ones.
  function quality(height) {
    if (height >= 2000) return "4K";
    if (height >= 1400) return "2K";
    return `${height}p`;
  }

  // The fullscreen element is the only thing drawn in fullscreen, so what shows goes inside it.
  function attach(host) {
    const full = document.fullscreenElement;
    const parent = full && full.tagName !== "VIDEO" ? full : document.documentElement;
    if (host.parentNode !== parent) parent.appendChild(host);
  }

  // ---- The video button ----

  let enabled = true;
  let video = null;
  // The dock, made in the top frame once there is something to show.
  let dock = null;
  let menuOpen = false;
  let hideTimer = 0;
  let frame = 0;
  let overButton = false;

  chrome.storage.local.get("videoButton").then(({ videoButton = true }) => {
    enabled = videoButton;
    // A video already playing when this script came, as an autoplaying one often is, is announced as if it started.
    const playing = [...document.getElementsByTagName("video")].find((v) => !v.paused);
    if (playing) announce(playing);
  }, () => {});
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
      ${GLASS}
      .box { position: absolute; top: 0; left: 0; transform: translateX(-100%); color: #fff; }
      .go {
        all: unset; cursor: pointer; position: relative; width: ${BUTTON}px; height: ${BUTTON}px; border-radius: 50%;
        display: grid; place-items: center; transition: transform 0.15s ease;
        background: rgba(20, 22, 30, 0.35); backdrop-filter: blur(12px); -webkit-backdrop-filter: blur(12px);
        animation: appear 0.25s cubic-bezier(0.2, 0.9, 0.3, 1.3);
      }
      .go .tile { width: ${BUTTON - 8}px; height: ${BUTTON - 8}px; transition: background 0.2s ease; }
      .go svg { width: 18px; height: 18px; }
      .go:hover { transform: scale(1.08); }
      .go:active { transform: scale(0.94); }
      .go:focus-visible, .item:focus-visible { outline: 2px solid #0a84ff; outline-offset: 2px; }
      /* A ring that spreads from the button when it comes up for a video that started playing. */
      .go.call::after {
        content: ""; position: absolute; inset: 0; border-radius: 50%; border: 2px solid #2b93ff;
        animation: call 1.4s ease-out 2;
      }
      .go.sent .tile { background: linear-gradient(160deg, #3ddc84, #1a9e4b); }
      .go.sent svg { animation: pop 0.35s cubic-bezier(0.2, 0.9, 0.3, 1.4); }
      .menu {
        position: absolute; right: 0; top: calc(100% + ${GAP}px);
        min-width: 230px; max-width: 320px; max-height: 320px; overflow-y: auto;
        padding: 6px; border-radius: 16px; animation: drop 0.18s ease-out;
      }
      .box.up .menu { top: auto; bottom: calc(100% + ${GAP}px); }
      .menu[hidden] { display: none; }
      .item {
        all: unset; box-sizing: border-box; cursor: pointer; width: 100%;
        display: flex; align-items: center; gap: 10px; padding: 7px 8px; border-radius: 10px;
      }
      .item:hover { background: rgba(10, 132, 255, 0.85); }
      .chip {
        min-width: 50px; height: 24px; padding: 0 8px; border-radius: 7px; display: inline-grid; place-items: center;
        font-weight: 700; font-size: 12.5px; letter-spacing: 0.2px; background: rgba(255, 255, 255, 0.14);
      }
      .chip.hd { background: linear-gradient(160deg, #2b93ff, #0050e6); }
      .chip svg { width: 15px; height: 15px; }
      .detail { flex: 1; opacity: 0.72; font-size: 12px; white-space: nowrap; }
      .item > svg { width: 17px; height: 17px; opacity: 0.55; }
      .item:hover > svg { opacity: 1; }
      .note { padding: 8px 10px; opacity: 0.85; font-size: 12.5px; }
      .wait { display: grid; place-items: center; padding: 14px; }
      .spin { width: 22px; height: 22px; border-radius: 50%; border: 2.5px solid rgba(255, 255, 255, 0.2); border-top-color: #fff; animation: spin 0.8s linear infinite; }
      .fly { position: fixed; width: 34px; height: 34px; margin: -17px 0 0 -17px; pointer-events: none; }
      .fly svg { width: 17px; height: 17px; }
      @keyframes appear { from { transform: scale(0.6); opacity: 0; } }
      @keyframes drop { from { transform: translateY(-6px); opacity: 0; } }
      @keyframes call { from { transform: scale(1); opacity: 0.9; } to { transform: scale(1.9); opacity: 0; } }
      @keyframes pop { from { transform: scale(0.4); } 70% { transform: scale(1.25); } }
      @keyframes spin { to { transform: rotate(360deg); } }
      @media (prefers-reduced-motion: reduce) { * { animation: none !important; transition: none !important; } }
    </style>
    <div class="box font">
      <button class="go" type="button" aria-haspopup="menu" aria-label="Tải video bằng Lanes" title="Tải video bằng Lanes">
        <span class="tile">${svg("down")}</span>
      </button>
      <div class="menu glass" role="menu" hidden></div>
    </div>`;
  const box = root.querySelector(".box");
  const go = root.querySelector(".go");
  const tile = go.querySelector(".tile");
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

  // The button sits inside the video's top-right corner, where the eye goes and the player's control bar is not. A
  // video scrolled up past the window keeps it at the window's top while enough of the video is left to hold it.
  function place() {
    if (!video?.isConnected) return hide();
    const r = video.getBoundingClientRect();
    if (r.width < MIN_WIDTH || r.height < MIN_HEIGHT) return hide();
    const top = Math.max(r.top, 0) + INSET;
    if (top + BUTTON > Math.min(r.bottom, window.innerHeight) - INSET) return hide();
    const right = Math.min(r.right, document.documentElement.clientWidth) - INSET;
    const above = top - GAP;
    const below = window.innerHeight - top - BUTTON - GAP;
    box.classList.toggle("up", above > below);
    menu.style.maxHeight = `${Math.max(Math.min(320, Math.max(above, below) - 8), 80)}px`;
    host.style.top = `${top}px`;
    host.style.left = `${right}px`;
  }

  function show(v) {
    clearTimeout(hideTimer);
    hideTimer = 0;
    if (v !== video) {
      closeMenu();
      video = v;
    }
    attach(host);
    host.style.display = "block";
    place();
  }

  function hide() {
    clearTimeout(hideTimer);
    hideTimer = 0;
    closeMenu();
    host.style.display = "none";
    go.classList.remove("call");
    video = null;
  }

  function hideSoon(after = HIDE_AFTER) {
    if (menuOpen || hideTimer || host.style.display === "none") return;
    hideTimer = setTimeout(hide, after);
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
  // A video that starts playing shows the button for a moment, with a ring that draws the eye to it.
  function announce(v) {
    if (!enabled || v?.tagName !== "VIDEO" || menuOpen || (video && video !== v)) return;
    const r = v.getBoundingClientRect();
    if (r.width < MIN_WIDTH || r.height < MIN_HEIGHT) return;
    show(v);
    go.classList.remove("call");
    void go.offsetWidth;
    go.classList.add("call");
    hideSoon(SHOW_ON_PLAY);
  }
  document.addEventListener("play", (e) => announce(e.target), true);
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
  document.addEventListener("fullscreenchange", () => {
    if (video) show(video);
    if (dock) attach(dock);
  });

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

  function itemButton(item) {
    const button = document.createElement("button");
    button.className = "item";
    button.type = "button";
    button.setAttribute("role", "menuitem");
    const chip = document.createElement("span");
    chip.className = item.height >= 720 ? "chip hd" : "chip";
    if (item.height) chip.textContent = quality(item.height);
    else if (item.kind === "page") chip.innerHTML = svg("audio");
    else chip.textContent = item.label;
    const detail = document.createElement("span");
    detail.className = "detail";
    detail.textContent = item.detail;
    button.title = item.filename;
    button.append(chip, detail);
    button.insertAdjacentHTML("beforeend", svg("down"));
    button.addEventListener("click", (e) => {
      e.stopPropagation();
      pick(item, button);
    });
    return button;
  }

  async function openMenu() {
    menuOpen = true;
    menu.hidden = false;
    menu.innerHTML = '<div class="wait"><span class="spin"></span></div>';
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
    for (const item of reply?.items || []) menu.append(itemButton(item));
    const notes = reply?.notes || [];
    if (!reply?.items?.length && !notes.length) note(nothingFound);
    notes.forEach(note);
  }

  // An arrow that flies from where the download was picked down to the dock's corner.
  function fly(from) {
    const r = from.getBoundingClientRect();
    const arrow = document.createElement("div");
    arrow.className = "fly tile";
    arrow.innerHTML = svg("down");
    arrow.style.left = `${r.left + r.width / 2}px`;
    arrow.style.top = `${r.top + r.height / 2}px`;
    root.append(arrow);
    const dx = window.innerWidth - 48 - (r.left + r.width / 2);
    const dy = window.innerHeight - 48 - (r.top + r.height / 2);
    const done = () => arrow.remove();
    if (!arrow.animate || matchMedia("(prefers-reduced-motion: reduce)").matches) return done();
    arrow
      .animate(
        [
          { transform: "translate(0, 0) scale(1)", opacity: 1 },
          { transform: `translate(${dx * 0.35}px, ${Math.min(dy * 0.1, 0) - 50}px) scale(1.1)`, opacity: 1, offset: 0.35 },
          { transform: `translate(${dx}px, ${dy}px) scale(0.45)`, opacity: 0.3 },
        ],
        { duration: 750, easing: "cubic-bezier(0.45, 0, 0.55, 1)" }
      )
      .finished.then(done, done);
  }

  async function pick(item, from) {
    fly(from);
    closeMenu();
    let reply;
    try {
      reply = await chrome.runtime.sendMessage({ type: "download", item });
    } catch {
      reply = { ok: false, error: "Tiện ích Lanes vừa được cập nhật. Hãy tải lại trang." };
    }
    if (!reply?.ok) {
      menuOpen = true;
      menu.hidden = false;
      note(reply?.error || "Lanes không nhận được video.");
      return;
    }
    // The button turns green with a tick for a moment, whatever timer the menu closing under the pointer started.
    clearTimeout(hideTimer);
    hideTimer = 0;
    tile.innerHTML = svg("check");
    go.classList.add("sent");
    setTimeout(() => {
      tile.innerHTML = svg("down");
      go.classList.remove("sent");
    }, 1800);
    hideSoon(2500);
  }

  go.addEventListener("click", (e) => {
    e.preventDefault();
    e.stopPropagation();
    go.classList.remove("call");
    if (menuOpen) closeMenu();
    else openMenu();
  });
  document.addEventListener("keydown", (e) => e.key === "Escape" && menuOpen && closeMenu(), true);
  document.addEventListener("pointerdown", (e) => menuOpen && !e.composedPath().includes(host) && closeMenu(), true);

  // ---- The dock, in the top frame only ----

  if (window.top !== window) return;

  const QUEUED = 0;
  const RUNNING = 1;
  const PAUSED = 2;
  const DONE = 3;
  const MAX_CARDS = 4;

  let list = null;
  const cards = new Map();

  function makeDock() {
    dock = document.createElement("lanes-dock");
    dock.style.cssText = "all: initial; position: fixed; right: 20px; bottom: 20px; z-index: 2147483647;";
    const shadow = dock.attachShadow({ mode: "closed" });
    shadow.innerHTML = `
      <style>
        ${GLASS}
        @media (prefers-color-scheme: light) {
          .glass {
            background: rgba(255, 255, 255, 0.72); color: #1d1d1f;
            border-color: rgba(255, 255, 255, 0.7);
            box-shadow: 0 12px 34px rgba(0, 0, 0, 0.16), inset 0 1px 0 rgba(255, 255, 255, 0.9);
          }
          .track { stroke: rgba(0, 0, 0, 0.1); }
        }
        .list { display: flex; flex-direction: column; gap: 10px; align-items: flex-end; }
        .card {
          width: 320px; display: flex; align-items: center; gap: 12px; padding: 10px 10px 10px 12px;
          border-radius: 18px; animation: enter 0.35s cubic-bezier(0.2, 0.9, 0.3, 1.15);
        }
        .card.leave { animation: leave 0.3s ease-in forwards; }
        .ring { position: relative; width: 38px; height: 38px; flex: none; display: grid; place-items: center; }
        .ring > svg.arc { position: absolute; inset: 0; width: 38px; height: 38px; transform: rotate(-90deg); }
        .track { stroke: rgba(255, 255, 255, 0.16); }
        .bar { stroke: #0a84ff; stroke-dasharray: ${RING}; stroke-dashoffset: ${RING}; transition: stroke-dashoffset 0.45s ease, stroke 0.3s ease; }
        .pct { font-weight: 700; font-size: 10.5px; font-variant-numeric: tabular-nums; }
        .ring .icon { position: absolute; inset: 0; display: grid; place-items: center; }
        .ring .icon svg { width: 17px; height: 17px; }
        .text { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; }
        .name { font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
        .meta { font-size: 12px; opacity: 0.68; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-variant-numeric: tabular-nums; }
        .act {
          all: unset; cursor: pointer; width: 30px; height: 30px; border-radius: 50%; flex: none;
          display: grid; place-items: center; opacity: 0.6; transition: background 0.15s ease, opacity 0.15s ease;
        }
        .act:hover { opacity: 1; background: rgba(127, 127, 127, 0.2); }
        .act:focus-visible { outline: 2px solid #0a84ff; }
        .act svg { width: 17px; height: 17px; }
        .act[hidden] { display: none; }
        /* Waiting for the app, or a size not known yet: an arc goes round and the arrow bobs. */
        .card.wait .arc { animation: spin 1s linear infinite; }
        .card.wait .bar { stroke-dashoffset: ${RING * 0.72}; }
        .card.wait .icon svg { animation: bob 1s ease-in-out infinite; color: #0a84ff; }
        .card.paused .bar { stroke: #8e8e93; }
        .card.done .bar { stroke: #30d158; stroke-dashoffset: 0; }
        .card.done .icon { color: #30d158; }
        .card.done .icon .draw { stroke-dasharray: 20; animation: draw 0.45s 0.15s ease-out backwards; }
        .card.done .ring { animation: pop 0.5s cubic-bezier(0.2, 0.9, 0.3, 1.4); }
        .card.done { animation: glow 1.2s ease-out; }
        .card.failed .bar { stroke: #ff453a; stroke-dashoffset: 0; }
        .card.failed .icon { color: #ff453a; }
        @keyframes enter { from { transform: translateX(30px) scale(0.96); opacity: 0; } }
        @keyframes leave { to { transform: translateX(30px); opacity: 0; } }
        @keyframes spin { to { transform: rotate(270deg); } }
        @keyframes bob { 50% { transform: translateY(2.5px); } }
        @keyframes draw { from { stroke-dashoffset: 20; } to { stroke-dashoffset: 0; } }
        @keyframes pop { 40% { transform: scale(1.18); } }
        @keyframes glow { from { box-shadow: 0 0 0 0 rgba(48, 209, 88, 0.55), 0 10px 30px rgba(0, 0, 0, 0.3); } to { box-shadow: 0 0 0 16px rgba(48, 209, 88, 0), 0 10px 30px rgba(0, 0, 0, 0.3); } }
        @media (prefers-reduced-motion: reduce) { * { animation: none !important; transition: none !important; } }
      </style>
      <div class="list font" role="status" aria-live="polite"></div>`;
    list = shadow.querySelector(".list");
  }

  function makeCard(item) {
    const card = document.createElement("div");
    card.className = "card glass";
    card.innerHTML = `
      <div class="ring">
        <svg class="arc" viewBox="0 0 36 36" fill="none" stroke-width="3" stroke-linecap="round">
          <circle class="track" cx="18" cy="18" r="15.5"/><circle class="bar" cx="18" cy="18" r="15.5"/>
        </svg>
        <span class="pct"></span><span class="icon"></span>
      </div>
      <div class="text"><span class="name"></span><span class="meta"></span></div>
      <button class="act reveal" type="button" title="Mở thư mục chứa file" aria-label="Mở thư mục chứa file" hidden>${svg("folder")}</button>
      <button class="act close" type="button" title="Ẩn" aria-label="Ẩn">${svg("close")}</button>`;
    card.querySelector(".reveal").addEventListener("click", (e) => {
      if (e.isTrusted) chrome.runtime.sendMessage({ type: "reveal", id: item.id }).catch(() => {});
    });
    card.querySelector(".close").addEventListener("click", (e) => {
      if (!e.isTrusted) return;
      chrome.runtime.sendMessage({ type: "dismiss", id: item.id }).catch(() => {});
      leave(item.id);
    });
    return card;
  }

  function leave(id) {
    const card = cards.get(id);
    if (!card) return;
    cards.delete(id);
    card.classList.add("leave");
    card.addEventListener("animationend", () => card.remove(), { once: true });
    setTimeout(() => card.remove(), 400);
  }

  function update(card, item) {
    const known = item.size > 0;
    const pct = known ? Math.min(100, Math.floor((100 * item.downloaded) / item.size)) : 0;
    const state = item.state === DONE ? "done" : item.state === PAUSED ? "paused" : item.state > DONE ? "failed" : item.state === QUEUED || !known ? "wait" : "running";
    if (card.dataset.state !== state) {
      card.dataset.state = state;
      card.className = `card glass ${state}`;
      const icon = { wait: "down", paused: "pause", done: "check", failed: "alert" }[state];
      card.querySelector(".icon").innerHTML = icon ? svg(icon) : "";
      card.querySelector(".reveal").hidden = state !== "done" || typeof item.id !== "number";
    }
    card.querySelector(".pct").textContent = state === "running" ? `${pct}%` : "";
    card.querySelector(".bar").style.strokeDashoffset = state === "running" ? RING * (1 - pct / 100) : "";
    card.querySelector(".name").textContent = item.name;
    card.title = item.name;
    const speed = item.speed ? ` · ${human(item.speed)}/s` : "";
    card.querySelector(".meta").textContent =
      state === "done" ? `Đã tải xong · ${human(item.size || item.downloaded)}`
      : state === "failed" ? item.error || "Không tải được"
      : state === "paused" ? "Tạm dừng"
      : item.state === QUEUED ? "Đang chờ Lanes"
      : known ? `${human(item.downloaded)} / ${human(item.size)}${speed}`
      : `${human(item.downloaded)}${speed}`;
  }

  function render(items) {
    items = (items || []).slice(0, MAX_CARDS);
    if (!items.length && !cards.size) return;
    if (!dock) makeDock();
    attach(dock);
    const ids = new Set(items.map((i) => i.id));
    for (const id of [...cards.keys()]) if (!ids.has(id)) leave(id);
    for (const item of items) {
      let card = cards.get(item.id);
      if (!card) {
        card = makeCard(item);
        cards.set(item.id, card);
        list.prepend(card);
      }
      update(card, item);
    }
  }

  chrome.runtime.onMessage.addListener((message) => {
    if (message?.type === "progress") render(message.items);
  });
  // The background tells only the tab in front; a tab coming back to the front asks for what it missed.
  const ask = () => chrome.runtime.sendMessage({ type: "progress?" }).then((reply) => render(reply?.items), () => {});
  document.addEventListener("visibilitychange", () => !document.hidden && ask());
  if (!document.hidden) ask();
})();
