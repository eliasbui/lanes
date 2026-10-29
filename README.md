<p align="center">
  <img src="assets/icon.png" width="112" alt="" />
</p>

<h1 align="center">Lanes</h1>

<p align="center">
  A fast, open download manager for macOS and Windows.<br />
  Many connections per file, resume after a stop, and one-click capture from the browser, in a Liquid&nbsp;Glass window.
</p>

<p align="center">
  <a href="README.md"><img src="https://img.shields.io/badge/English-blue?style=for-the-badge" alt="English (current)" /></a>
  <a href="README.vi.md"><img src="https://img.shields.io/badge/Ti%E1%BA%BFng%20Vi%E1%BB%87t-grey?style=for-the-badge" alt="Tiếng Việt" /></a>
</p>

<p align="center">
  <a href="https://github.com/eliasbui/lanes/releases/latest"><img src="https://img.shields.io/github/v/release/eliasbui/lanes?label=release" alt="Latest release" /></a>
  <img src="https://img.shields.io/badge/macOS-13%2B-black" alt="macOS 13 or later" />
  <img src="https://img.shields.io/badge/Windows-10%2B-black" alt="Windows 10 or later" />
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT License" /></a>
</p>

Lanes is a download manager written in Rust. It opens many connections to a file at once, writes each range straight
into its place, and keeps a sidecar so a stopped download resumes instead of starting over. A browser extension hands
downloads to the app with the page's own headers, so a site that served the file to the browser serves it to Lanes
too, and puts a download button on the videos you watch, with a choice of resolution. The window is a single web page over a glass background; every rule behind it is tested Rust. It runs in the menu
bar, and is in English and Vietnamese.

## Features

- **Many connections per file.** Each file is split into equal ranges, up to 32 connections, each writing directly
  into the final file at its offset. No slow join step at the end.
- **Resume.** Progress is saved to a sidecar a few times a minute; after a crash, a power cut or a quit, the download
  carries on from where it stopped. The server is asked for the same file by its ETag, so a changed file restarts
  rather than mixing old and new bytes.
- **Browser capture.** The extension takes a download the browser started and sends it to Lanes with the Referer,
  Cookie and User-Agent of the page, which is what gets most "403 Forbidden" downloads through. Works in Chrome, Edge,
  Brave and Cốc Cốc; a right-click "Download with Lanes" is there too. If Lanes is not running, the extension starts
  it in the menu bar so the download begins at once.
- **Video button.** Hover a video and a "Tải video" button with the Lanes mark appears under it, as in IDM (inside its bottom corner when the video reaches the bottom of the window); it lists what Lanes can download
  for that video by resolution (1080p, 720p, ...) with the size or an estimate. Whole video files go through the
  normal multi-connection engine; HLS streams (`.m3u8`) are fetched segment by segment in parallel, AES-128 included,
  resumable, and joined into one `.ts` or `.mp4`. YouTube, and pages where Lanes finds no stream of its own (DASH
  sites), go through yt-dlp: the menu lists every resolution up to 2160p plus sound only, and the picked one is
  downloaded and joined into an MP4. Not supported: live streams, and anything with DRM, which Lanes never touches.
- **yt-dlp** is needed for YouTube and the DASH sites; Lanes finds it where Homebrew or winget puts it. On macOS:
  `brew install yt-dlp`. On Windows: `winget install yt-dlp.yt-dlp DenoLand.Deno`. The ffmpeg that joins a video to
  its sound (720p to 4K) comes inside Lanes: a 4 MB LGPL build of FFmpeg that only joins streams, made by
  `scripts/build-ffmpeg.sh` from the FFmpeg source.
- **net-merge (beta).** Paste several mirror URLs of one file and Lanes spreads the segments across them, merging
  their bandwidth. A mirror that disagrees on the size or allows no ranges is dropped, and each segment moves to the
  next mirror when one fails. (Lanes' net-merge is multi-source merging; it is unrelated to the NetMerge
  network-bonding tools.)
- **Pause, resume, speed and ETA** for every download, live.
- **Categories as in IDM.** Files go into `Compressed`, `Documents`, `Music`, `Programs` and `Video` inside the
  download folder by their extension (IDM's lists, plus dmg and pkg as programs); anything else stays in the folder
  itself. One switch in Settings turns it off.
- **A connection setting**, launch at login, and in-app updates.
- **Liquid Glass interface**, light and dark, that follows the system.

## Install

### macOS

An Apple silicon or Intel Mac with macOS 13 or later. With Homebrew:

    brew tap eliasbui/lanes
    brew install --cask eliasbui/lanes/lanes

Without Homebrew:

    curl -fsSL https://raw.githubusercontent.com/eliasbui/lanes/main/scripts/install.sh | sh

Writing the cask out in full installs it from the tap and trusts only that cask, so no `brew trust` is needed. The
app is signed ad hoc and not notarized, so both ways remove the quarantine flag after installing, as other open
source apps do. `brew uninstall --cask lanes` removes it, and `--zap` the download list in
`~/Library/Application Support/Lanes` too.

### Windows

In PowerShell:

    irm https://raw.githubusercontent.com/eliasbui/lanes/main/scripts/install.ps1 | iex

It installs for the current user into `%LOCALAPPDATA%\Programs\Lanes`, checks the download against the SHA-256 GitHub
publishes, and adds a Start-menu shortcut. Run the same line again to update; remove it from Settings, Apps.

### Browser extension

The app carries the extension and writes it to `~/Library/Application Support/Lanes/extension` (macOS) or
`%APPDATA%\Lanes\extension` (Windows) at every start; Settings, Browser extension shows the folder. Open
`chrome://extensions` (or the Edge, Brave, Cốc Cốc equivalent), turn on Developer mode and drag that folder onto the
page, or use "Load unpacked". The app refreshes the folder when it updates, and the extension reloads itself from it
as soon as the app answers with a newer version, so no browser restart is needed. Its id is fixed by the key in `extension/manifest.json`, so Lanes recognises it
wherever you load it from. The popup shows whether the extension reaches the app, and turns capture and the video
button on and off.

## Build and run

Rust 1.85 or later.

    cargo run

    cargo test                       # the engine, the store, the updater and the browser host
    cargo run --example make_icons   # redraw the icons in assets/
    scripts/bundle-macos.sh          # build dist/Lanes.app for this Mac

## How it works

- `src/engine.rs` — the download engine: probing for size and range support, planning equal segments, one connection
  per segment with positioned writes, the resume sidecar, and retries.
- `src/hls.rs` — HLS playlists: resolutions of a master, segments, keys and byte ranges of a media playlist, and
  AES-128 decryption.
- `src/ytdlp.rs` — video pages through yt-dlp: the formats as menu choices, and the download with its progress.
- `src/store.rs` — the list of downloads in SQLite.
- `src/browser.rs` — the native-messaging host, the inbox the extension drops jobs into, and the extension the app
  carries.
- `src/update.rs` — the in-app update: read the latest release, verify its SHA-256, swap the app in place, restart.
- `src/autostart.rs` — launch at login (a LaunchAgent on macOS, the Run key on Windows).
- `src/main.rs` — the window, the tray and the glue between the page and the engine.
- `ui/index.html` — the whole interface, one file.

## Data and privacy

The download list and settings live in `~/Library/Application Support/Lanes` (macOS) or `%APPDATA%\Lanes` (Windows).
Lanes talks to the servers you download from and to GitHub for the update check; the browser extension sends a job
only to the local app, never over the network.

## Licence

MIT. See [LICENSE](LICENSE). Lanes is an original program; it is not affiliated with any other download manager.
