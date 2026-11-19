// In-memory stand-in for the Rust side, used only when the page is opened outside the app (a
// plain browser, for layout checks and benchmarks). Never loaded inside Eid Download.
// It follows the command shapes of docs/V0.1-CONTRACT.md, including `list({ since })`
// revisions. URL parameters: `?n=1000` (synthetic list size), `&running=50` (how many of them
// transfer), `&old=1` (answer `list` in the v0.0.1 shape, without revisions).
// A mock is not proof that the real backend or YouTube works.
export function mockApi(params = new URLSearchParams()) {
  const DIR = "C:\\Users\\me\\Downloads";
  const oldShape = params.get("old") === "1";
  let rev = 1;
  let next = 1;
  const items = new Map(); // id -> { item, rev }
  const removed = []; // [rev, id]
  const touch = (it) => { rev++; items.set(it.id, { item: it, rev }); };
  const mk = (o) => ({ id: next++, status: "pending", name: "", url: "", folder: DIR, attempts: 0, error_kind: null, error: null, rate: null, ...o });

  const n = Math.max(0, Math.min(100000, Number(params.get("n")) || 0));
  if (n > 0) {
    const R = Math.min(n, Number(params.get("running") ?? 20));
    const ST = ["done", "pending", "paused", "failed", "done"];
    const EXT = ["iso", "zip", "pdf", "mp4", "mp3", "exe", "png", "docx", "mkv", "7z"];
    for (let i = 0; i < n; i++) {
      const running = i >= n - R;
      const st = running ? "running" : ST[i % ST.length];
      const total = 1024 * 1024 * (5 + ((i * 7919) % 4000));
      touch(mk({
        status: st,
        name: i % 4 === 0 ? `تقرير رقم ${i} - final.${EXT[i % EXT.length]}` : `file-${i}-sample.${EXT[i % EXT.length]}`,
        url: `https://releases.example.com/path/${i}/file-${i}.${EXT[i % EXT.length]}`,
        folder: i % 9 === 0 ? "D:\\Apps" : DIR,
        attempts: st === "failed" ? 3 : 1,
        error_kind: st === "failed" ? "not_found" : null,
        error: st === "failed" ? `server error: HTTP 404 for https://releases.example.com/path/${i}` : null,
        rate: st === "pending" ? null : { done: st === "done" ? total : Math.floor(total * ((i % 97) / 100)), total, speed: running ? 2e6 + (i % 50) * 1e5 : 0, eta: null },
      }));
    }
  } else {
    touch(mk({ status: "pending", name: "music.flac", url: "https://example.com/music.flac", rate: null }));
    touch(mk({ status: "done", name: "setup.exe", url: "https://example.com/setup.exe", folder: "D:\\Apps", attempts: 1, rate: { done: 48234496, total: 48234496, speed: 0, eta: null } }));
    touch(mk({ status: "done", name: "holiday-photos.zip", url: "https://example.com/holiday-photos.zip", folder: "D:\\Downloads", attempts: 1, rate: { done: 10485760, total: 10485760, speed: 0, eta: null } }));
    touch(mk({ status: "failed", name: "missing.zip", url: "https://example.com/missing.zip", attempts: 3, error_kind: "not_found", error: "server error: HTTP 404 for https://example.com/missing.zip" }));
    touch(mk({ status: "paused", name: "تقرير 2026 النهائي - final report.pdf", url: "https://example.com/files/report.pdf", attempts: 1, rate: { done: 734003, total: 2097152, speed: 0, eta: null } }));
    touch(mk({ status: "running", name: "ubuntu-24.04-desktop-amd64.iso", url: "https://releases.example.com/ubuntu-24.04-desktop-amd64.iso", attempts: 1, rate: { done: 1932735283, total: 6114770944, speed: 12582912, eta: 332 } }));
    touch(mk({ status: "running", name: "‮gpj.exe — RTL override in a name", url: "https://example.com/odd", attempts: 1, rate: { done: 1000, total: null, speed: 40960, eta: null } }));
  }

  let last = performance.now();
  function tick() {
    const now = performance.now();
    const dt = Math.min(5, (now - last) / 1000);
    last = now;
    let running = 0;
    for (const { item: it } of items.values()) {
      if (it.status !== "running") continue;
      running++;
      const r = it.rate ?? (it.rate = { done: 0, total: null, speed: 0, eta: null });
      const speed = Math.max(1, Math.round((r.speed || 3e6) * (0.9 + Math.random() * 0.2)));
      const done = r.total ? Math.min(r.total, r.done + Math.round(speed * dt)) : r.done + Math.round(speed * dt);
      const nr = { done, total: r.total, speed, eta: r.total ? Math.ceil((r.total - done) / speed) : null };
      const ni = { ...it, rate: nr };
      if (r.total && done >= r.total) { ni.status = "done"; ni.rate = { ...nr, speed: 0, eta: null }; running--; }
      touch(ni);
    }
    // start queued ones while fewer than 3 transfer (only in the small sample list)
    if (n === 0) {
      for (const { item: it } of items.values()) {
        if (running >= 3) break;
        if (it.status === "pending") { touch({ ...it, status: "running", attempts: it.attempts + 1, rate: it.rate ?? { done: 0, total: 734003200, speed: 4e6, eta: null } }); running++; }
      }
    }
  }
  const find = (id) => {
    const e = items.get(id);
    if (!e) throw "no_such_item";
    return e.item;
  };
  const sorted = () => [...items.values()].map((e) => e.item).sort((a, b) => b.id - a.id);

  // ---- video
  const QUALS = ["360p", "480p", "720p", "1080p", "best", "audio"];
  const jobs = [];
  let nextJob = 1;
  let toolsInstalled = params.get("notools") !== "1";
  let engineSettings = { max_parallel: 3, connections: 8, speed_limit_kib: 0, schedule: null, tray: true, close_to_tray: false, clipboard: false, power_actions: false };
  // events the Rust side would emit (tray menu, clipboard); tests trigger them
  const listeners = new Map();
  window.__eidMockEmit = (name, payload) => { for (const f of listeners.get(name) ?? []) f(payload); };
  let trayTexts = null;
  window.__eidMockTrayTexts = () => trayTexts;
  const opened = [];
  window.__eidMockOpened = opened;
  // test hook (mock only): change an item as if the engine did, e.g. finish or fail it
  window.__eidMockSet = (id, patch) => touch({ ...find(id), ...patch });
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  function jobTick() {
    for (const j of jobs) {
      if (j.state !== "running" && j.state !== "queued") continue;
      j.state = "running";
      const cur = j.items.find((i) => i.state === "running" || i.state === "queued");
      if (!cur) { j.state = j.failed_items ? "failed" : "done"; j.speed = 0; continue; }
      cur.state = "running";
      cur.progress = Math.min(1, cur.progress + 0.08 + Math.random() * 0.05);
      j.speed = 3e6 + Math.random() * 1e6;
      j.bytes = (j.bytes ?? 0) + Math.round(j.speed * 0.5);
      if (cur.progress >= 1) {
        if (/fail/i.test(cur.title) && !cur.retried) { cur.state = "failed"; cur.error = "HTTP Error 403: Forbidden"; j.failed_items++; }
        else { cur.state = "done"; cur.progress = 1; cur.got_height = j.quality === "audio" ? undefined : Math.min(1080, parseInt(j.quality) || 1080); j.done_items++; }
      }
      j.got_height = j.quality === "audio" ? undefined : Math.min(1080, parseInt(j.quality) || 1080);
    }
  }
  setInterval(jobTick, 500);
  const job = (id) => { const j = jobs.find((x) => x.job === id); if (!j) throw "no_such_job"; return j; };

  // v0.2 queues + "when finished" (a stand-in; the real countdown is the engine's)
  const queues = [{ name: "main", dir: DIR, max_parallel: 3, paused: false, after_done: "none" }, { name: "folder-1", dir: "D:\\Apps", max_parallel: 2, paused: false, after_done: "none" }];
  const power = { countdown: null, last: null };
  window.__eidMockCountdown = (secs, action) => { power.countdown = secs ? [secs, action] : null; };
  const limits = new Map();
  const warnings = [];
  window.__eidMockWarn = (id, text) => warnings.push([id, text]);
  window.__eidMockLimit = (id) => limits.get(id);
  const handlers = {
    list: ({ since }) => {
      tick();
      if (oldShape) return { items: sorted(), default_dir: DIR };
      while (removed.length > 2000) removed.shift();
      const tooOld = since == null || since < rev - 100000 || (removed.length && since < removed[0][0] - 1 && removed.length >= 2000);
      if (tooOld) return { rev, full: true, items: sorted(), removed: [], default_dir: DIR };
      const changed = [...items.values()].filter((e) => e.rev > since).map((e) => e.item);
      const pw = { ...power }; power.last = null;
      const ws = warnings.splice(0);
      return { rev, full: false, items: changed, removed: removed.filter(([r]) => r > since).map(([, id]) => id), default_dir: DIR, power: pw, warnings: ws };
    },
    // `?check=from_zip,no_native_host`: what the startup self-check reports
    self_check: () => (params.get("check") ?? "").split(",").filter(Boolean),
    categories: () => ["Compressed", "Documents", "Music", "Video", "Pictures", "Programs"],
    import_list: () => { const r = window.__eidMockImport ?? null; if (r) for (const u of r.urls) touch(mk({ name: u.split("/").pop(), url: u })); return r && { added: r.urls.length, refused: r.refused ?? 0, duplicates: 0, credentials_dropped: 0 }; },
    export_list: () => (window.__eidMockExportCancel ? null : items.size),
    add: ({ links, name, folder, category }) => {
      if (category != null && !["none", "auto", "Compressed", "Documents", "Music", "Video", "Pictures", "Programs"].includes(category)) throw "bad_category";
      window.__eidMockLastAdd = { folder, category };
      const ls = String(links).split(/\s+/).filter(Boolean);
      if (!ls.length) throw "no_links";
      if (ls.length > 200) throw "too_many_links";
      if (name && ls.length > 1) throw "name_needs_one_link";
      const added = [], refused = [];
      for (const l of ls) {
        if (!/^https?:\/\/[^/]+/i.test(l)) { refused.push([l, "bad_url"]); continue; }
        const it = mk({ name: name || decodeURIComponent(l.split("/").pop() || "") || l, url: l, folder: folder || DIR });
        touch(it);
        added.push(it.id);
      }
      return { added, refused };
    },
    pause: ({ id }) => { const it = find(id); if (["pending", "waiting", "running"].includes(it.status)) touch({ ...it, status: "paused", rate: it.rate && { ...it.rate, speed: 0, eta: null } }); },
    resume: ({ id }) => { const it = find(id); if (["paused", "failed"].includes(it.status)) touch({ ...it, status: n ? "running" : "pending", error: null, error_kind: null }); },
    remove: ({ id }) => {
      if (find(id).status === "running") throw "pause_first";
      items.delete(id); rev++; removed.push([rev, id]);
    },
    clear_finished: () => {
      let k = 0;
      for (const [id, e] of items) if (e.item.status === "done") { items.delete(id); rev++; removed.push([rev, id]); k++; }
      return k;
    },
    pick_folder: () => "D:\\Downloads\\مجلد",
    reveal: ({ id }) => { if (find(id).status !== "done") throw "not_finished"; },
    open_file: ({ id }) => {
      const it = find(id);
      if (it.status !== "done") throw "not_finished";
      if (/\.(exe|msi|bat|cmd|ps1|js|vbs|scr|lnk)$/i.test(it.name ?? "")) throw "refused_executable";
      opened.push(id);
    },
    segments: ({ id }) => {
      const it = find(id);
      const r = it.rate;
      if (it.status === "done") return { source: "finished", total: r?.total ?? null, done: r?.total ?? 0, segments: 0, cells: Array(120).fill(100), rows: [] };
      if (!r || !r.total) return { source: "none", total: null, done: 0, segments: 0, cells: [], rows: [] };
      // 8 equal segments, each saved from its start up to the item's fraction (a stand-in)
      const S = 8, seg = Math.floor(r.total / S), f = r.done / r.total;
      const rows = Array.from({ length: S }, (_, k) => {
        const start = k * seg, end = k === S - 1 ? r.total : (k + 1) * seg;
        return { start, end, done: Math.floor((end - start) * Math.min(1, f * (1 + (k % 3) * 0.2))) };
      });
      const cells = Array.from({ length: 120 }, (_, c) => {
        const k = Math.min(S - 1, Math.floor((c / 120) * S)), row = rows[k];
        const pos = (c / 120) * r.total;
        const savedEnd = row.start + row.done;
        return pos + r.total / 120 <= savedEnd ? 100 : pos >= savedEnd ? 0 : 50;
      });
      return { source: "checkpoint", total: r.total, done: rows.reduce((a, x) => a + x.done, 0), segments: S, cells, rows };
    },
    queues: () => queues.map((q) => {
      const its = [...items.values()].map((e) => e.item).filter((i) => (q.name === "main" ? i.folder !== "D:\\Apps" : i.folder === "D:\\Apps"));
      return { ...q, total: its.length, done: its.filter((i) => i.status === "done").length, running: its.filter((i) => i.status === "running").length, failed: its.filter((i) => i.status === "failed").length };
    }),
    queue_set: ({ patch }) => {
      const keys = ["name", "max_parallel", "paused", "after_done"];
      if (!patch || Object.keys(patch).some((k) => !keys.includes(k))) throw "invalid input: unknown field";
      const q = queues.find((x) => x.name === patch.name);
      if (!q) throw "no_such_queue";
      if (patch.max_parallel != null && !(patch.max_parallel >= 1 && patch.max_parallel <= 8)) throw "invalid input: max_parallel";
      if (patch.after_done != null && !["none", "exit", "sleep", "hibernate", "shutdown"].includes(patch.after_done)) throw "invalid input: after_done";
      if (["sleep", "hibernate", "shutdown"].includes(patch.after_done) && !engineSettings.power_actions) throw "power_actions_off";
      Object.assign(q, Object.fromEntries(Object.entries(patch).filter(([k, v]) => k !== "name" && v != null)));
    },
    item_limit: ({ id, limit_kib }) => {
      find(id);
      if (limit_kib != null && !(Number.isInteger(limit_kib) && limit_kib >= 1 && limit_kib <= 12500000)) throw "invalid input: speed limit";
      limits.set(id, limit_kib);
      touch({ ...find(id), limit_kib });
    },
    cancel_countdown: () => { const had = !!power.countdown; power.countdown = null; if (had) { power.last = "cancelled"; for (const q of queues) q.after_done = "none"; } return had; },
    settings_get: () => ({ ...engineSettings }),
    settings_set: ({ settings: s }) => {
      const keys = ["max_parallel", "connections", "speed_limit_kib", "schedule", "tray", "close_to_tray", "clipboard", "power_actions"];
      if (Object.keys(s).some((k) => !keys.includes(k))) throw "invalid input: unknown field";
      if (!(s.max_parallel >= 1 && s.max_parallel <= 8)) throw "invalid input: max_parallel 1..8";
      if (!(s.connections >= 1 && s.connections <= 32)) throw "invalid input: connections 1..32";
      if (!(s.speed_limit_kib >= 0 && s.speed_limit_kib <= 12500000)) throw "invalid input: speed limit";
      if (s.schedule && (!s.schedule.weekdays || s.schedule.start_min === s.schedule.end_min)) throw "invalid input: schedule";
      engineSettings = { ...s };
      return { ...engineSettings };
    },
    tools_status: () => ({ installed: toolsInstalled, consent: toolsInstalled, ytdlp_version: toolsInstalled ? "2026.11.01 (mock)" : undefined, ffmpeg_version: toolsInstalled ? "7.1 (mock)" : undefined }),
    tools_consent_text: ({ lang }) => (lang === "ar" ? "نص الموافقة (تجريبي): تُنزَّل yt-dlp وFFmpeg وDeno من مصادرها الرسمية..." : "Agreement (mock): yt-dlp, FFmpeg and Deno are downloaded from their official sources..."),
    tools_install: async ({ accept }) => { if (accept !== true) throw "consent_required"; await sleep(300); toolsInstalled = true; },
    video_probe: async ({ url }) => {
      await sleep(600);
      if (!toolsInstalled) throw "TOOLS_MISSING: yt-dlp is not installed";
      if (!/^https?:\/\//i.test(url)) throw "bad_url";
      if (/fail/i.test(url)) throw "ERROR: [youtube] This video is unavailable";
      if (/[?&]list=/.test(url)) {
        const count = /big/.test(url) ? 300 : 12;
        return {
          kind: "playlist", title: "قائمة دروس Rust — Rust lessons", uploader: "Example Channel",
          qualities: ["360p", "480p", "720p", "1080p", "1440p", "2160p", "4320p", "best", "audio"], // 4320p must be hidden by the UI
          items: Array.from({ length: count }, (_, i) => ({ index: i + 1, id: `id${i + 1}`, title: i === 4 ? "الدرس 5 (will fail once)" : `Lesson ${i + 1}: ${["ownership", "borrowing", "traits", "async", "errors"][i % 5]}`, duration: 300 + i * 17 })),
        };
      }
      return { kind: "video", title: "Big Buck Bunny — sample video", uploader: "Example Channel", duration: 596, qualities: QUALS };
    },
    video_add: ({ url, quality }) => {
      if (!["360p", "480p", "720p", "1080p", "1440p", "2160p", "best", "audio"].includes(quality)) throw "bad_quality";
      const j = { job: nextJob++, kind: "video", title: "Big Buck Bunny — sample video", url, state: "queued", quality, done_items: 0, total_items: 1, failed_items: 0, bytes: 0, total_bytes: null, speed: 0, items: [{ index: 1, title: "Big Buck Bunny — sample video", state: "queued", progress: 0 }] };
      jobs.unshift(j);
      return { job: j.job };
    },
    playlist_add: ({ url, quality, items: pick, if_missing }) => {
      if (!["360p", "480p", "720p", "1080p", "1440p", "2160p", "best", "audio"].includes(quality)) throw "bad_quality";
      if (if_missing != null && !["lower", "higher", "skip"].includes(if_missing)) throw "invalid input: if_missing";
      window.__eidMockLastPlaylist = { url, quality, if_missing };
      const count = /big/.test(url) ? 300 : 12;
      const idx = pick ?? Array.from({ length: count }, (_, i) => i + 1);
      const its = idx.map((i) => ({ index: i, title: i === 5 ? "الدرس 5 (will fail once)" : `Lesson ${i}`, state: "queued", progress: 0 }));
      const j = { job: nextJob++, kind: "playlist", title: "قائمة دروس Rust — Rust lessons", url, state: "queued", quality, done_items: 0, total_items: its.length, failed_items: 0, bytes: 0, total_bytes: null, speed: 0, items: its };
      jobs.unshift(j);
      return { job: j.job };
    },
    video_jobs: () => structuredClone(jobs),
    video_pause: ({ job: id }) => { const j = job(id); if (j.state === "running" || j.state === "queued") { j.state = "paused"; j.speed = 0; for (const i of j.items) if (i.state === "running") i.state = "queued"; } },
    video_resume: ({ job: id }) => { const j = job(id); if (j.state === "paused") j.state = "queued"; },
    video_retry: ({ job: id }) => {
      const j = job(id);
      for (const i of j.items) if (i.state === "failed") { i.state = "queued"; i.progress = 0; i.retried = true; delete i.error; j.failed_items--; }
      j.state = "queued";
    },
    video_remove: ({ job: id }) => { const j = job(id); if (j.state === "running") throw "pause_first"; jobs.splice(jobs.indexOf(j), 1); },
  };
  handlers.tray_texts = ({ texts }) => {
    const k = ["show", "pause_all", "resume_all", "quit"];
    if (!texts || Object.keys(texts).some((x) => !k.includes(x)) || k.some((x) => !texts[x])) throw "invalid input: texts";
    trayTexts = { ...texts };
  };
  return {
    on: (name, f) => { if (!listeners.has(name)) listeners.set(name, []); listeners.get(name).push(f); },
    call: async (cmd, args) => {
      const h = handlers[cmd];
      if (!h) throw `Command ${cmd} not found`;
      return structuredClone(await h(args ?? {}));
    },
  };
}
