# أبحاث / Research — Eid Download

> هذا الملف يلخص ما استفدناه من البرامج المرجعية **كأفكار فقط**. لم يُنسخ أي كود ولا هوية ولا شعار من أي منها.
> This file records **ideas only** gathered from reference products. No code, branding, or assets were copied.

## 0. Sources & licensing notes

| Product | What we used | License / legal note |
|---|---|---|
| Internet Download Manager (IDM) | Public product pages & user guide (feature list, behaviour) | Proprietary. Behaviour described from public docs and general knowledge; nothing reverse-engineered. |
| Ant Download Manager (AntDM) | Public user guide pages (`antdownloadmanager.com/uguide`, FAQ, HLS page) — "Video Button / Video Box", HLS/DASH, extension → app hand-off | Proprietary. Same note. |
| Ezoo Download (`AhmedEzo686/ezoo-download`) | **Only the public README** (feature table, project layout, behaviours it discloses: loopback port 9614, extension cookie hand-off, yt-dlp/ffmpeg in `tools`, per-user installer, 8 UI languages). We deliberately did **not** read or reuse its source. | LICENSE = *"All rights reserved … copying the code, redistributing, renaming, or attributing it to others is prohibited without written permission."* Therefore it is a pure **idea/benchmark** reference. Its UI stack is Slint; ours is different. |
| yt-dlp / FFmpeg | Used as external programs (separate processes) | yt-dlp: Unlicense. FFmpeg: LGPL/GPL depending on build — we ship an unmodified build as a separate executable, keep notices (`THIRD-PARTY-NOTICES`) and never link it. |
| Tauri / Svelte / Rust crates | Dependencies | MIT/Apache-2.0 (see `cargo about` output in release). |

## 1. Core transfer engine ideas

### 1.1 Segmented ("accelerated") downloading
* All three products open several parallel HTTP connections, each with its own `Range: bytes=a-b`, and write into one pre-sized file. Up to 32 connections (IDM: 32 max; Ezoo advertises 32; AntDM "multi-threaded").
* **IDM "dynamic file segmentation"**: segments are *not* fixed at start. When a connection finishes (or a server rejects more connections), the manager takes the *largest remaining* range of another connection and splits it ("work stealing"), re-using the already-open (keep-alive) connection to avoid new TCP/TLS handshakes. This is the idea we implement: a **segment table with leases**; an idle worker steals the tail half of the segment with the most remaining bytes (above a minimum split size), the owner's end boundary is shrunk atomically.
* Practical caveats found in public discussions of these tools: many CDNs/servers cap per-IP connections (HTTP 429/503) → engine must **adaptively reduce** connection count; start small (e.g. 4–8) and ramp when throughput scales; servers without `Accept-Ranges` / lying about it must fall back to a single stream.
* Fast-start: first connection begins immediately as a `GET` with `Range: bytes=0-` — the response headers double as the probe (size, validators, filename) so no extra round trip.

### 1.2 Resume
* Persist per-download: URL, referer, cookies (protected), validators (`ETag` strong / `Last-Modified`), total length, **segment table** (start, end, downloaded). Resume re-validates with `If-Range`; if the server answers 200 instead of 206, or validators differ → restart (never silently corrupt).
* Work survives app exit and crash: a periodic atomic checkpoint (write temp + rename). On restart bytes written after the last checkpoint are simply re-downloaded (positional writes make that idempotent).
* Data lives in a `.part` file next to the target; rename on success.

### 1.3 Queues, scheduler, limits
* IDM: main queue + additional queues, each with its own start/stop time, max simultaneous downloads, "download only on these days", and "when done: exit / hang up / turn off / sleep / hibernate". We adopt: named queues with concurrency + time window + after-completion action (with a cancellable countdown).
* Speed limiter: IDM has a global limiter (and per-download "speed limiter" in the more recent versions). We implement hierarchical token buckets: global bucket → per-job bucket.
* Auto categories by extension (Compressed / Documents / Music / Video / Programs / Images / Other) → per-category default folder (IDM style).
* Clipboard monitoring: IDM/Ezoo detect copied URLs; we do it opt-in and filtered by known downloadable extensions/video sites.
* Site Grabber (IDM "Grabber"): crawl a page (optionally N levels) and filter links by type/pattern. We do a bounded, single-page grabber first (fetch HTML, extract `<a href>`, `<img>`, `<source>`, `<video>`…, filter by extension / regex, user ticks what to add) — no deep crawl in v1.
* Batch download (IDM "Add batch download"): URL pattern with `*` and ranges (`http://x/img(001-050).jpg`). We implement the same pattern idea plus plain list paste and import/export (text, JSON).

## 2. Browser integration

### 2.1 Link capture
* Chromium extension (MV3) listens to `chrome.downloads.onCreated` (cancel the browser download, hand over to the app) and `webRequest.onHeadersReceived` (look at `Content-Type`, `Content-Disposition`, `Content-Length` to catch files that did not trigger the downloads API, and to detect media). Cookies + referer + user-agent for that URL are attached so login-protected links work (Ezoo discloses doing this; IDM does it via its own integration).
* Context-menu items: "Download with Eid Download", "Download all links on page/selection".

### 2.2 Video overlay ("Download this video" bar)
* AntDM: the extension *monitors network requests* for media (direct mp4/webm, HLS `.m3u8`, DASH `.mpd`). When found it determines the **tab + frame** the media belongs to and renders a small button over the player (top-right of that frame); if tab/frame cannot be resolved it falls back to a **Video Box** in the toolbar popup. Users can disable per site.
* IDM: same concept ("Download this video" panel over the player; list of qualities).
* Our design: (a) network sniffing in the background worker for direct media / HLS / DASH, tagged with `tabId` + `frameId`; (b) a content script that finds `<video>` elements (MutationObserver, also in same-origin iframes — content scripts run with `all_frames`) and renders a **Shadow-DOM** overlay (so page CSS cannot break it, RTL-aware); (c) for supported sites (YouTube etc.) the overlay asks the app to run yt-dlp for formats rather than sniffing (YouTube serves segmented/encrypted streams that sniffing cannot save). `blob:` URLs, DRM/EME and cross-origin frames are inherent limits → fall back to Video Box in popup.

### 2.3 Native Messaging
* Chrome/Edge: extension declares `"nativeMessaging"`; a host manifest JSON (`name`, `path`, `type: stdio`, `allowed_origins: ["chrome-extension://<id>/"]`) is registered under `HKCU\Software\Google\Chrome\NativeMessagingHosts\<name>` (and `…\Microsoft\Edge\NativeMessagingHosts\…`) — **per-user, no admin**. The browser starts the host `.exe`, speaks **length-prefixed (4-byte native-endian) JSON** over stdin/stdout. Max host→browser 1 MB, browser→host 64 MiB.
* Firefox: same protocol but manifest uses `allowed_extensions: ["id@example"]` and registry `HKCU\Software\Mozilla\NativeMessagingHosts\<name>`; extension is MV2/MV3 with `browser_specific_settings.gecko.id`. Feasible; only the manifest + `webRequestBlocking`-free logic differ.
* Ezoo (README) uses a loopback HTTP port (9614) instead; AntDM/IDM ship their own integration binaries. We pick **Native Messaging** (matches the user's sketch): no open port, origin-restricted by the browser itself. The host is tiny and relays to the running app over a **per-user local socket (named pipe on Windows)**; if the app is not running the host launches it.

## 3. Video / playlists
* yt-dlp (`-J` / `--dump-single-json` for metadata, `--flat-playlist` for playlist entries, `-f` format selectors, `--merge-output-format mp4`, `--newline --progress-template` for machine-readable progress) with FFmpeg for muxing/audio extraction (`-x --audio-format mp3`).
* YouTube specifics (2026): extraction increasingly needs a JS runtime (yt-dlp EJS: deno/node/bun) and may demand cookies / PO tokens / block datacenter IPs. We must detect and show these errors exactly (not generic "failed").
* Update strategy: yt-dlp breaks regularly → ship pinned version in the installer and provide "update yt-dlp" (download official release asset + SHA256 verification, atomic replace in `%LOCALAPPDATA%\EidDownload\tools`).
* AntDM/IDM handle HLS themselves (parallel `.ts` download + remux). We delegate HLS/DASH sites to yt-dlp/ffmpeg initially (fewer bugs); native HLS segment downloader through our engine is a later milestone.

## 4. UI / UX ideas
* IDM-style: left category tree, central download list with columns (name, size, progress, speed, ETA, status, date), toolbar (Add URL, Resume, Stop, Stop all, Delete, Options, Scheduler, Grabber).
* Per-download details with live segment map (a bar divided into per-connection progress — a classic IDM feature people love).
* Tray icon, minimize to tray (downloads continue), "download complete" toast, drag-and-drop URLs, multi-select with Ctrl/Shift.
* Arabic-first: `dir="rtl"`, CSS logical properties, bidi isolation (`<bdi>`/`unicode-bidi: isolate`) for URLs and file names, Arabic numerals optional, an Arabic-capable UI font stack (system "Segoe UI" on Windows + bundled variable Arabic font as fallback).

## 5. Installer
* Ezoo: per-user, no admin, auto-update from its own release page. Ours: Tauri 2 NSIS `installMode: currentUser`, `webviewInstallMode: embedBootstrapper` (Windows 10 builds < 1803 or missing runtime → bootstrapper installs WebView2; Win10 ≥1803 & Win11 already have it), HKCU native-messaging registration in installer hooks, uninstaller removes registry keys.

## 6. Take-aways → requirements
1. Segment table + lease/steal splitting with minimum split size; adaptive connection count; robust resume validators.
2. Single-process truth: app owns jobs; CLI/native host are clients **or** run engine standalone.
3. Extension: downloads API + webRequest sniffing + Shadow-DOM video overlay + popup "Video Box" fallback + context menus.
4. Everything per-user, no admin, no open TCP port.
5. Be honest about limits: Windows behaviour cannot be validated from Linux; need Windows CI.
