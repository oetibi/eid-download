# Eid Download — Architecture (v2, checkpoint for review)

Inputs compared: `RESEARCH.md` (ours) · `RESEARCH-grok.md` (parallel report) · `docs/codex/arch_round1.md`, `arch_round2.md` (Codex, independent) · user sketches `architecture-ref.png` / `architecture-ref-2.png` ("Arabic UI = Tauri+Svelte; CLI; Rust engine; **a layer per OS** (Windows/macOS/Linux); extension→app via Native Messaging; **video with the user's consent**: yt-dlp·FFmpeg; light + correct Arabic; Rust = memory safety").
Status: design only + two build spikes. No product code written yet.

> **v3 (2026-11-13) — تغييرات بعد دراسة منهجية زاجل (أفكار فقط)**: (1) ملفات الأساس والمراحل بشروط نجاح: [PLAN.md](PLAN.md)، [REQUIREMENTS.md](REQUIREMENTS.md)، ADRs في [adr/](adr/)، قواعد الوكلاء [../AGENTS.md](../AGENTS.md). (2) سيرفر اختبار سيّئ `eid-testserver` crate مستقل (§9). (3) قائمة أمان دائمة (مسارات، حقن، أسرار، `CREATE_NO_WINDOW`) مع اختبارات مسمّاة في PLAN. (4) كل العمليات الفرعية عبر `eid_platform::process::spawn` فقط. (5) الكوكيز في الذاكرة فقط، لا تُحفظ على القرص؛ لا حفظ لبيانات دخول المواقع في v1 (يُحذف قسم DPAPI للكوكيز؛ DPAPI يبقى للمستقبل فقط). (6) فتح الملفات `open_regular_nofollow` وفحص الملف المفتوح. (7) «لم يُختبر يدوياً» قسم إلزامي في كل إصدار؛ [STATUS.md](STATUS.md) و[MANUAL-TESTS.md](MANUAL-TESTS.md).
> الأقسام أدناه تبقى التصميم المعتمد إلا إذا تعارضت مع ADR أحدث.

## 1. UI stack — Tauri 2 + Svelte 5 (not Slint)
| | Tauri 2 + Svelte | Slint |
|---|---|---|
| Arabic/RTL | browser text engine: shaping, bidi, `dir=rtl`, CSS logical props → whole layout mirrors; file names/URLs isolated with `<bdi>` | shaping OK, layout not auto-mirrored, mixed-direction text fragile |
| Fonts | system Segoe UI (has Arabic) + small bundled variable Arabic font (Noto Sans Arabic subset, ~100 KB woff2) for identical look everywhere | needs font embedding too |
| Weight | ~8–10 MB exe, ~60–100 MB RAM (WebView2 shared) | lighter |
| Win10 | WebView2 bootstrapper in installer | none |
| Testing on this Linux box | Tauri shell can't build here (no webkit2gtk, no root). UI has a thin `api.ts`; browser build uses a mock → headless-Chrome screenshots (ar/en × light/dark × widths). Real window only on Windows CI | Xvfb possible |

Cost accepted: the real Tauri window is not runnable here. Mitigation: `src-tauri` is a thin layer; everything else is headless-testable.

## 2. Process model & IPC
* **App owns everything**: tray app, engine in-process, window close → hide to tray, downloads continue. Single instance (per-user mutex / lock-socket; 2nd launch forwards its command and exits).
* **IPC**: per-user local socket via `interprocess` (Windows **named pipe** `\\.\pipe\EidDownload-<user-SID>` with protected user-only DACL + remote clients rejected; Unix socket in `$XDG_RUNTIME_DIR` on Linux/macOS for tests). Handshake with random 256-bit token stored in a user-only file (defence in depth; it does *not* stop same-user malware — documented). Length-prefixed JSON, versioned, bounded (≤1 MiB), idempotent request IDs.
* **CLI `eid`**: *standalone* (embeds engine: `eid get URL -c 16 --limit 2M --sha256 …`, `eid video URL`) and *remote* (`eid add|list|pause|resume|remove|queue` against the running app). `eid serve` = same server headless (Linux tests/servers).
* **Native host `eid-native-host`**: stdio↔pipe relay; if pipe absent it starts the app (`--background`), retries readiness with a deadline, then sends idempotent request. **stdout carries protocol only; logs to stderr/file; stdio set binary on Windows.**
* No localhost TCP port at all (Ezoo-style loopback HTTP rejected: DNS-rebinding/port-clash surface).
* Daemon/service: rejected for v1 (no admin ⇒ no service); same server code can be split later.

## 3. Platform layer (new — from sketch 2 "طبقة لكل نظام")
Crate **`eid-platform`**: one trait set + `cfg`-selected impls, so engine/CLI build & test on Linux:
```rust
trait Platform {
  fn data_dir()/config_dir()/default_download_dir() -> PathBuf;
  fn local_listener(name) / local_connect(name) -> IPC stream (pipe | unix socket);
  fn secret_protect(&[u8]) / secret_unprotect(&[u8])      // DPAPI | 0600 file / keyring later
  fn preallocate(&File, len) -> Result<Prealloc>           // FSCTL_SET_SPARSE+set_len | fallocate/set_len
  fn replace_file_atomic(tmp, dest, backup)                // ReplaceFileW | rename
  fn register_native_host(browsers, manifest_path) / unregister   // HKCU registry | ~/.config/**/NativeMessagingHosts JSON
  fn set_autostart(bool); fn system_action(Shutdown|Sleep|Hibernate|Lock) -> countdown handled by app
  fn mark_of_the_web(path); fn kill_process_tree(child) // Job object | killpg
  fn clipboard_text() ; fn notify(..)
}
```
`windows.rs` (real, `windows-sys`), `unix.rs` (Linux real where cheap; macOS = best effort stub, compiled-checked only), `stub.rs` for unit tests. Windows impl is compiled in CI; on Linux we cross-compile-check it (`cargo check --target x86_64-pc-windows-gnu/msvc`).

## 4. Crates
```
crates/eid-engine/  segments, http, limiter, store, queue/scheduler, categorize, filename, grabber, batch, import/export (no UI, no platform)
crates/eid-media/   yt-dlp/ffmpeg supervision (probe, formats, playlist, progress, tool manager, consent state)
crates/eid-ipc/     protocol, framing (shared with native messaging), client/server
crates/eid-platform/ OS layer above
crates/eid-cli/     bin eid
crates/eid-native-host/ bin eid-native-host
src-tauri/          Tauri commands, tray, clipboard watcher, autostart, window mgmt
ui/                 Svelte 5 + Vite + TS (i18n ar/en, light/dark/system, virtual list)
extension/          MV3 Chrome/Edge + Firefox manifest variant
installer/          NSIS hooks (HKCU registration), WebView2 config
```

## 5. Engine (eid-engine)
**HTTP**: tokio + reqwest 0.12, `http1_only()` + keep-alive for segment connections (separate TCP connections are the whole point; HTTP/2 shares one congestion window). `Accept-Encoding: identity`. **TLS: target-specific features** — Windows `native-tls-alpn` (SChannel: Windows trust store/AV roots, ALPN, no C toolchain), others `rustls-tls`. *Spike verified*: this feature split builds with `cargo zigbuild --target x86_64-pc-windows-gnu` **and** `cargo xwin build --target x86_64-pc-windows-msvc` (plain rustls/ring failed for msvc: needs `clang-cl`). Redirects ≤10, refuse https→http, drop credentials/cookies across origins. Browser-like headers; fallback ladder on 403/406/416: no-Range → drop optional headers → IPv4-only → diagnose "link expired".

**Probe**: `GET Range: bytes=0-`; the open response body *is* segment 0 (one-time links aren't burned). Range support is trusted only after valid `206` + `Content-Range` total consistent with length + identity; `200` ⇒ single stream (no resume, UI says so); no length ⇒ growing-file single stream.

**Segments (decided: IDM-style gradual ramp + coordinator, reservation protocol)**
* Start with 1 worker; every ~300 ms add a worker up to target (default **8**, max **32**, per-host cap) by splitting the *largest remaining* segment in half. Min split: remainder ≥ 8 MiB, each half ≥ 4 MiB (configurable). Finished worker with no pending segment helps the slowest by the same rule; keep-alive connection reused.
* **Race-free handoff** (differs from Codex's pause/ack): each segment `{start, pos, end}` sits behind a tiny mutex. A worker *reserves* `n = min(buf, end-pos)` bytes under the lock, then writes them (positional) and marks them *completed*; the coordinator shrinks `end` to `mid = max(pos, (pos+end)/2)` under the same lock and hands `[mid,end)` to a new lease. A reserved range can never be cut, so no pause/ack round trip and no lost/duplicated byte. Verified by randomized split-schedule test comparing SHA-256 (Codex's concern addressed by test, not protocol).
* Completed-bytes tracking: per-segment contiguous `done` watermark + out-of-order chunk set merged into intervals; "on_disk" watermark only advances after `sync_data`.
* Slow worker: below ¼ of peer median for 15 s (excluding limiter/disk waits) ⇒ drop and re-request its remainder (≤2×/min). Repeated concurrency-correlated 429/503/resets ⇒ halve host cap, persist for 24 h, honour `Retry-After`.
* **Retry**: backoff+jitter; network errors retried w/ budget; 401/403/404/410 = fatal-ish ("expired – refresh from browser/page", re-extract for video); HTML-instead-of-file (captcha/login page) detected by Content-Type vs expected.

**Files**: `<name>.part` in a configurable temp/destination dir. `Platform::preallocate` = **FSCTL_SET_SPARSE then set_len** on NTFS (avoids zero-fill stall on far offsets; verified-by-reasoning, tested on Windows CI), `fallocate`/`set_len` on Linux; FAT/exFAT/unsupported ⇒ sequential growing-file fallback (single stream) and FAT32 4 GiB check; ENOSPC handled at every write; free-space check before start. Positional writes only. Final: coverage verify (no gaps/overlaps; sum==len), optional SHA-256, `sync_all`, atomic rename; never overwrite silently (` (1)` or ask). Long paths: `\\?\` + `longPathAware` manifest. Optional Mark-of-the-Web (`Zone.Identifier`, setting, default on for executables/archives).

**Resume/persistence**: strong ETag → `If-Range`; else Last-Modified+length (IDM-style "ignore mtime changes" setting); weak ETag never; `200` on resume / validator change / bad Content-Range ⇒ restart with notice; no trustworthy validator ⇒ restart (Codex: no 64-byte-overlap heuristic, rejected as unsafe; grok proposed it). Store = **one atomic JSON per job** in `jobs/` + `queues.json` + `settings.json` (no C dependency; SQLite remains a documented future option for >20k jobs). Single writer, checkpoint dirty *active* jobs every 2–5 s and on pause/exit. In the engine, a dedicated task (one per `run_ranged`) performs each checkpoint; the coordinator only signals it and never awaits the I/O, so a slow fsync cannot stall ramping or splitting. Requests coalesce (at most one in flight; the running checkpoint takes a fresh snapshot immediately before `sync_data`). The task is joined before any later state write (final checkpoint, finalize/`remove_all`, ServerChanged restart, cancel). `MissedTickBehavior::Delay` on the ramp and checkpoint intervals. Discipline: snapshot the table → `sync_data` the .part → serialize → write unique temp in same dir → fsync/close → `Platform::replace_file_atomic` (ReplaceFileW with `.bak` on Windows; bounded retries on sharing violations from AV) → on failure keep old checkpoint & show "degraded persistence"; validate schema/coverage on load, quarantine bad file as `.broken`, recover from `.bak`. After crash anything past the last checkpoint is simply re-downloaded.

**Limiter**: hierarchical token bucket (global → queue → job), async acquire, bounded burst (no multi-second debt across 32 connections), live-adjustable. External yt-dlp processes can't share it → media jobs get an explicit budget slice via `--limit-rate`.
**Queues/scheduler**: named queues {max_parallel, window start/stop, weekdays, retries, after-done action: none/exit/sleep/hibernate/shutdown}; failed job → end of queue; "per-host 1 connection" exceptions; shutdown only after a **cancellable 60 s countdown**, re-checks that nothing new is active. Download-first dialog optional ("start while dialog open" off by default).
**Categories** by extension/MIME → folder (editable). **Clipboard** watcher opt-in (app layer; URL parser in engine). **Grabber** = bounded single-page HTML link extractor (no JS, no crawl) with ext/regex filters, user ticks results. **Batch**: `(001-050)`/`*` patterns + pasted lists; import/export txt/JSON/CSV.
**Events**: sequence-numbered `broadcast`, progress ≤10 Hz; UI re-snapshots after reconnect.

## 6. Media (eid-media) — consent first
* **Consent gate** (sketch: «الفيديو، بموافقتك»): video features are *off* until the user accepts a dialog (AR/EN) stating what will be installed/used (yt-dlp, FFmpeg, a JS runtime), that these are third-party tools **downloaded/unpacked from their official releases with SHA-256 verification** (or taken from the installer-bundled copy), and that the user is responsible for rights/ToS; DRM refused. Consent stored per-user (`settings.media_consent = {version, ts}`); revoke in Settings (deletes tools). CLI: `eid video` prompts once / `--yes-i-agree`. Extension video overlay stays inert until consent.
* Tools: `yt-dlp`, `ffmpeg`, `ffprobe`, **deno** (YouTube EJS; *verified on this box*: yt-dlp 2026.10.26 warns "No supported JavaScript runtime…"; we pass `--js-runtimes deno:<path>` on Windows, `node` for Linux tests), pinned versions + hashes in `tools/manifest.json`, **downloaded from the official release after consent — never redistributed inside our installer** (see DEPENDENCIES.md), kept in `%LOCALAPPDATA%\EidDownload\tools`; update button with SHA-256 check, atomic replace, rollback. Always `--ignore-config --no-cache-dir --no-warnings … --` before URL; PATH tools never trusted.
* **Transfer ownership (disagreement with RESEARCH-grok.md, Codex sided with us)**: `yt-dlp -J` (single) / `--flat-playlist -J` (playlist) → `MediaInfo` → user picks quality / audio-only(mp3) / playlist items → **yt-dlp performs the transfer and muxing** (`-f`, `--merge-output-format mp4`, `--concurrent-fragments`, `--newline --progress-template`). Our engine downloads ordinary HTTP/direct media files only (and later HLS/DASH segments natively). Reason: YouTube needs chunked ranges to avoid throttling, PO tokens, fragmented formats, URL expiry (~6 h) — re-implementing those is the bug-prone path. Persist *source page URL + format policy*, never the direct URL; re-extract on resume/"later". Experimental flag `--engine-direct` for progressive HTTPS formats, measured before ever becoming default. **No cookies for video in v1** (sign-in-required media unsupported ⇒ no temp cookie file; consistent with "cookies in memory only").
* Tool errors shown after **secret redaction** (URLs/query/cookies/tokens replaced; sentinel-value test over every output + created file) + hint (bot-check/sign-in, 429, geo, IP block, missing runtime). Process tree killed on cancel (Job object / killpg).

## 7. Extension (Chrome/Edge MV3; Firefox variant)
`downloads.onDeterminingFilename/onCreated` interception by extension list + min size + site excludes + **Bypass key**; **never cancel the browser download until the app has acked** (host/app unreachable ⇒ browser proceeds); `webRequest` header sniffing (media/HLS/DASH, tab+frame tagged, held in SW memory/session storage); context menus (download link / all links / page video); cookies+referer+UA for that URL only. Content script: `<video>` finder + **Shadow-DOM** overlay «تحميل هذا الفيديو / Download this video» (+ Video Box in popup fallback); content script never talks to native host directly — only via service worker with sender checks. SW uses `connectNative("com.eid.download")`, reconnects on `onDisconnect`. "Download later" stores page URL + chosen quality. Host manifests registered per user (HKCU Chrome/Edge/Brave/Chromium/Mozilla), re-written on app start if path changed; `allowed_origins` pinned by manifest `key`. Limits: DRM, `blob:`/MSE, sandboxed cross-origin frames.

## 8. Packaging
Tauri 2 NSIS `installMode: currentUser`, `webviewInstallMode: embedBootstrapper` (+ `minimumWebview2Version`), Arabic+English installer, installer hooks write/remove HKCU native-messaging keys; `longPathAware` + per-monitor DPI manifest; no admin. App updates: manual/signed-check only (no silent mandatory updater). Authenticode signing = needs user's certificate (documented; unsigned ⇒ SmartScreen warning).
**Spike results on this box**: engine/CLI/host `.exe` buildable on Linux via zigbuild (gnu) and xwin (msvc) with the TLS split above ✅. NSIS (`makensis`), clang-cl/llvm-rc absent & no root ⇒ Tauri exe + NSIS installer expected only from **GitHub Actions windows-latest** (workflow will be written; needs a repo — no account will be connected). Will still *try* `cargo xwin` Tauri build with a `llvm-rc` shim (`zig rc`) as a bonus, not a promise.

## 9. Test plan (write tests first, in this order — Codex)
1 range/identity validation · 2 split/cancel/write races (randomized schedules ⇒ SHA-256) · 3 flush/checkpoint/crash (kill -9, truncated JSON, `.bak` recovery) · 4 sparse/ENOSPC/rename collisions (Windows CI) · 5 IPC startup/auth/idempotency/fuzz.
Plus: local fixture server (Range, no-Range, flaky, throttled, changing ETag, redirects, CD filenames, 429/Retry-After); limiter accuracy ±10 %; filename table tests; **real** large public file (~hundreds MB, 8–32 conns, throughput numbers); kill+resume checksum; no-Range fallback; yt-dlp CC video + `ffprobe` (video+audio) — YouTube reachable from this box today (`-J` on `jNQXAC9IVRw` works). UI: Vitest + headless-Chrome RTL screenshots. Windows-only items → CI + `docs/WINDOWS_TEST_CHECKLIST.md`.

## 10. Where we differ from `RESEARCH-grok.md` (and why)
| Topic | grok report | Final | Why |
|---|---|---|---|
| Video transfer | yt-dlp `-J` only, our engine downloads googlevideo URLs, ffmpeg merge | **yt-dlp transfers+muxes** for YouTube/sites; engine for plain files; engine-direct experimental | URL expiry, chunking/throttling, PO tokens, fragments; Codex agreed; fewer bugs |
| Metadata store | sidecar `.meta.json` per `.part` | central `jobs/<id>.json` (+`.bak`,`.broken`) | one place for queue state/UI; same atomic discipline |
| Resume w/o validators | compare ~64 bytes overlap | restart | Codex: overlap proves nothing |
| Localhost HTTP fallback | "if kept, 127.0.0.1+auth" | none | no port at all |
| Pipe auth | random token | **DACL + token** | Codex: token alone is not user isolation |
| Installer | NSIS *or* Inno | NSIS (Tauri) | single toolchain |
| Segment start | 1 conn then halve (same as IDM) | **adopted** | better than my n-way initial split |
| Defaults | 8 / 16–32 | default 8, ceiling 32 | adopted |
| Sparse NTFS, `on_disk` watermark, learned per-host cap, TLS SChannel, Deno, `--ignore-config`/`--`, DPAPI, longPathAware, MotW, no silent updater, one-shot probe | proposed | **adopted** | verified or sound; MotW as setting |
| Ezoo | idea-only | idea-only | its LICENSE forbids copying; we read only README, never its source |
| Handoff protocol | — | reservation-under-lock (not Codex's pause/ack) | simpler, race-free by construction, test-verified |
