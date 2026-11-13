**Build Eid Download around a persistent background daemon.** Tauri, CLI, and browser integrations are clients; closing the window must not interrupt downloads.

### 1. Crate layout

Use a Cargo workspace:

- `eid-core`: job models, commands/events, scheduling policies, backend traits.
- `eid-http`: HTTP transfers, segmentation, rate limiting, retry logic.
- `eid-store`: SQLite migrations, checkpoints, recovery.
- `eid-media`: yt-dlp/FFmpeg process supervision.
- `eid-platform`: Windows pipes, clipboard, shutdown, credential protection; portable test implementations.
- `eid-daemon`: sole owner of jobs, files, database, and scheduler.
- `eid-protocol`: versioned IPC schemas.
- `eid-cli`, `eid-native-host`, `eid-desktop`: thin clients.

Keep Tauri dependencies out of engine crates. Svelte provides Arabic-first translations, document-level RTL, CSS logical properties, bidi-isolated URLs/filenames, English switching, and light/dark/system themes.

### 2. Engine design

**HTTP:** Tokio + reqwest/rustls, shared connection pools. Disable transparent decompression for byte-range transfers; request `Accept-Encoding: identity`. Probe with `GET Range: bytes=0-0`; HEAD alone cannot establish range support. Require consistent `206`, `Content-Range`, length, and representation identity.

**Persistence:** SQLite WAL stores jobs, queues, validators, destination, backend, and segments: `(start, end_exclusive, durable_next, state, generation)`. Separate volatile progress from durable checkpoints. Flush file data before committing completed offsets; after crashes, replay uncheckpointed bytes. Reset leased segments on recovery.

**Segmentation:** Start with four requests; adapt toward a configurable ceiling of 32 using throughput gains and origin limits. Idle workers steal the largest remaining interval. Split only above a minimum remainder, e.g. 8 MiB. Pause the owning worker at a write boundary, revoke its generation, partition its unwritten interval, then dispatch new leases. Never mutate boundaries beneath an active writer. Slow workers retain a small prefix while others steal their tail.

**Storage:** Write bounded buffers through blocking positional I/O, never shared seek cursors. Use a `.part` file beside the destination. Set logical length and attempt actual allocation; do not assume length reservation guarantees disk space. Handle disk-full errors throughout. Flush and rename after coverage verification; resolve name collisions explicitly.

**Resume:** Prefer strong ETag with `If-Range`; otherwise use Last-Modified plus length conservatively. Never use weak ETags as strong validators. Changed identity, inconsistent ranges, `200` during resume, or ambiguous `416` triggers revalidation/restart. Without trustworthy validators, default to restart.

**Limits/recovery:** Hierarchical global/per-job token buckets with fair scheduling, small bursts, and bounded reads. Retry transient failures with exponential jitter, attempt budgets, and `Retry-After`; refresh authentication separately. No-Range servers get one sequential stream; interrupted transfers restart unless ranges subsequently validate.

**Metadata/security:** Bound redirects, reject HTTPS downgrades by default, and strip credentials across origins. Prefer `Content-Disposition` `filename*`, then `filename`, then URL; sanitize Windows reserved names, traversal, and collisions. Accept explicitly authorized, origin-scoped cookies/referer from extensions; protect stored secrets with DPAPI and redact logs.

### 3. GUI and browser integration

Tauri commands proxy daemon commands; throttled events carry sequence numbers, with snapshots after reconnect. Use a per-user Windows named pipe secured to that user’s SID, a per-user mutex, and idempotent daemon startup.

Choose a **small Native Messaging host**, not localhost HTTP. Browser-launched hosts relay length-prefixed JSON to the daemon and can start it. Register manifests under HKCU for Chrome/Edge; Firefox needs its own manifest/extension-ID configuration. Bound and paginate messages. [Chrome documentation](https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging)

The extension adds player overlays using content scripts and mutation observers, sending page/media candidates for extraction. Support Firefox afterward. Treat overlays as discovery buttons: blob URLs, cross-origin frames, DRM, and Manifest V3 prevent universal capture.

### 4. Media and distribution

Bundle pinned Windows x64 yt-dlp, FFmpeg/ffprobe, and a supported JavaScript runtime such as Deno; current YouTube extraction also depends on yt-dlp’s EJS support. [yt-dlp documentation](https://github.com/yt-dlp/yt-dlp)

Probe JSON, display formats and playlist items, then execute explicit selections. Preserve yt-dlp partial files; supervise process trees without shell interpolation. Let yt-dlp own media transfer/muxing.

Ship dependency updates through signed manifests, verified hashes, atomic replacement, rollback, and retained license notices. Use NSIS `perUser` plus embedded WebView2 bootstrapper; installation may still require network access. [Tauri documentation](https://v2.tauri.app/distribute/windows-installer/)

### 5. Eight risks and tests

1. **Split races:** randomized schedules; compare complete output hashes.
2. **Crash corruption:** kill between writes, flushes, and database commits.
3. **Broken servers:** fixture servers returning malformed ranges, changed validators, redirects, and truncation.
4. **Limiter starvation:** deterministic-clock fairness and burst tests.
5. **Windows storage:** NTFS disk-full, Unicode, locked files, collisions, antivirus interference.
6. **IPC attacks:** fuzz framing, oversized messages, malformed commands, and access controls.
7. **Media churn:** pinned fixtures plus optional live extraction smoke tests.
8. **UI/installer gaps:** mocked-IPC Chromium RTL screenshots; real Windows installation, WebView2, restart, and accessibility tests.

### 6. What I would challenge

Thirty-two connections is a ceiling, not a performance target. Start smaller.

Linux can test engine/CLI and cross-build artifacts; cross-compilation and mocked screenshots cannot validate Windows behavior. Require Windows CI before release.

Page Grabber should initially mean bounded HTML/link extraction, not a full crawler. Make clipboard monitoring opt-in. Persist queue priorities, categories, timezone-aware schedules, batch imports/exports, and shutdown policy; recheck all work and show a cancelable countdown before shutdown. Media concurrency and speed limits need explicit budgeting because external processes bypass the Rust limiter.