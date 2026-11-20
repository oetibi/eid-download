// Pure UI logic (no DOM): merging `list` answers, categories, sorting, selection, polling
// cadence, quality filtering and number formatting. Unit tested with `node --test` (ui/tests).

/** Statuses that mean "not finished and may still move". */
export const ACTIVE = new Set(["pending", "waiting", "running", "paused"]);
/** Statuses during which the engine is (or will soon be) transferring. */
export const BUSY = new Set(["pending", "waiting", "running"]);

/**
 * Merge one `list` answer into `store` (a Map id -> item). Handles both shapes:
 * - old: `{ items, default_dir }` (always the whole list, no `rev`/`full`);
 * - new: `{ rev, full, items, removed, ... }` where `full === false` means `items` are only the
 *   changed ones and `removed` the ids that are gone.
 * Returns `{ rev, changed }`: the revision to send as `since` next time (null = old backend)
 * and whether anything in the store changed (membership or any field).
 */
export function mergeList(store, resp) {
  const items = Array.isArray(resp?.items) ? resp.items : [];
  const full = resp?.full !== false; // absent (old shape) = full
  const rev = typeof resp?.rev === "number" ? resp.rev : null;
  let changed = false;
  if (full) {
    const seen = new Set();
    for (const it of items) {
      seen.add(it.id);
      const old = store.get(it.id);
      if (!old || !sameItem(old, it)) { store.set(it.id, it); changed = true; }
    }
    for (const id of [...store.keys()]) if (!seen.has(id)) { store.delete(id); changed = true; }
  } else {
    for (const it of items) {
      const old = store.get(it.id);
      if (!old || !sameItem(old, it)) { store.set(it.id, it); changed = true; }
    }
    for (const id of resp.removed ?? []) if (store.delete(id)) changed = true;
  }
  return { rev, changed };
}

function sameItem(a, b) {
  if (a.status !== b.status || a.name !== b.name || a.url !== b.url || a.folder !== b.folder
    || a.attempts !== b.attempts || a.error_kind !== b.error_kind || a.error !== b.error) return false;
  const x = a.rate, y = b.rate;
  if (!x || !y) return x === y;
  return x.done === y.done && x.total === y.total && x.speed === y.speed && x.eta === y.eta;
}

const EXT = {
  compressed: ["zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "xz", "zst", "cab", "lz", "lzma"],
  documents: ["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "rtf", "txt", "csv", "epub", "md"],
  music: ["mp3", "flac", "wav", "aac", "ogg", "oga", "m4a", "opus", "wma", "alac", "aiff"],
  video: ["mp4", "mkv", "webm", "avi", "mov", "wmv", "flv", "m4v", "mpg", "mpeg", "3gp", "ts"],
  programs: ["exe", "msi", "msix", "appx", "apk", "dmg", "pkg", "deb", "rpm", "appimage", "iso", "img", "bat", "ps1"],
  pictures: ["jpg", "jpeg", "png", "gif", "webp", "bmp", "svg", "tif", "tiff", "heic", "avif", "ico"],
};
const EXT_TO_CAT = new Map();
for (const [cat, list] of Object.entries(EXT)) for (const e of list) EXT_TO_CAT.set(e, cat);
export const CATEGORIES = Object.keys(EXT);

/** File-type category from a file name ("other" when unknown). */
export function categoryOf(name) {
  const m = /\.([A-Za-z0-9]{1,10})$/.exec(String(name ?? "").trim());
  return (m && EXT_TO_CAT.get(m[1].toLowerCase())) ?? "other";
}

/**
 * Does `it` belong to sidebar entry `view`? `view` is one of: all, active, unfinished,
 * finished, failed, a category name, or `folder:<path>`.
 */
export function inView(it, view) {
  switch (view) {
    case "all": return true;
    case "active": return BUSY.has(it.status);
    case "unfinished": return it.status !== "done";
    case "finished": return it.status === "done";
    case "failed": return it.status === "failed";
    default:
      if (view.startsWith("folder:")) return it.folder === view.slice(7);
      return categoryOf(it.name) === view;
  }
}

/** Fraction 0..1 of an item (null when unknown). */
export function fraction(it) {
  if (it.status === "done") return 1;
  const r = it.rate;
  if (!r || !r.total) return null;
  return Math.min(1, Math.max(0, r.done / r.total));
}

const STATUS_ORDER = { running: 0, waiting: 1, pending: 2, paused: 3, failed: 4, done: 5 };
const KEY = {
  id: (it) => it.id,
  name: (it) => String(it.name ?? "").toLowerCase(),
  size: (it) => it.rate?.total ?? it.rate?.done ?? -1,
  progress: (it) => fraction(it) ?? -1,
  status: (it) => STATUS_ORDER[it.status] ?? 9,
  speed: (it) => (it.status === "running" ? it.rate?.speed ?? 0 : -1),
  eta: (it) => (it.status === "running" && it.rate?.eta != null ? it.rate.eta : Number.MAX_SAFE_INTEGER),
  url: (it) => String(it.url ?? ""),
};
export const SORT_KEYS = Object.keys(KEY);

/** Comparator for column `key`, `dir` = 1 ascending / -1 descending; ties fall back to newest first. */
export function comparator(key, dir) {
  const k = KEY[key] ?? KEY.id;
  return (a, b) => {
    const x = k(a), y = k(b);
    if (x < y) return -dir;
    if (x > y) return dir;
    return b.id - a.id;
  };
}

/** Items of `store` shown for `view`, sorted. */
export function buildView(store, view, key, dir, query = "") {
  const out = [];
  for (const it of store.values()) if (inView(it, view) && matchesQuery(it, query)) out.push(it);
  return out.sort(comparator(key, dir));
}

/**
 * New selection after a click on row `id` in `order` (array of ids as shown).
 * `mods`: { ctrl, shift }. Returns { selected: Set, anchor }.
 */
export function clickSelect(selected, anchor, order, id, mods = {}) {
  if (mods.shift && anchor != null && order.includes(anchor)) {
    const a = order.indexOf(anchor), b = order.indexOf(id);
    const [lo, hi] = a < b ? [a, b] : [b, a];
    const range = order.slice(lo, hi + 1);
    const next = mods.ctrl ? new Set(selected) : new Set();
    for (const x of range) next.add(x);
    return { selected: next, anchor };
  }
  if (mods.ctrl) {
    const next = new Set(selected);
    if (next.has(id)) next.delete(id); else next.add(id);
    return { selected: next, anchor: id };
  }
  return { selected: new Set([id]), anchor: id };
}

/** Delay before the next poll: fast while something moves, slow when idle, none when hidden. */
export function pollDelay({ hidden, busy }) {
  if (hidden) return null;
  return busy ? 500 : 2500;
}

/** Quality policy (docs/V0.1-CONTRACT.md): never above 2160p, only the known choices. */
export const QUALITIES = ["360p", "480p", "720p", "1080p", "1440p", "2160p", "best", "audio"];
export function safeQualities(list) {
  const got = new Set(Array.isArray(list) ? list : []);
  return QUALITIES.filter((q) => got.has(q));
}

/** Human size with the given unit names [B, KB, MB, GB, TB]. */
export function fmtSize(n, units) {
  if (n == null || !Number.isFinite(n) || n < 0) return "";
  let i = 0, v = n;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
  return `${v.toFixed(i === 0 ? 0 : v < 10 ? 2 : 1)} ${units[i]}`;
}

/** h:mm:ss / m:ss (locale-neutral digits). */
export function fmtClock(s) {
  if (s == null || !Number.isFinite(s) || s < 0) return "";
  s = Math.floor(s);
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), sec = s % 60;
  const p = (x) => String(x).padStart(2, "0");
  return h > 0 ? `${h}:${p(m)}:${p(sec)}` : `${m}:${p(sec)}`;
}

/** Overall fraction 0..1 of a video job (bytes when known, otherwise finished items + item progress). */
export function jobFraction(job) {
  if (job.state === "done") return 1;
  // bytes are those of the CURRENT item: a whole-job fraction only for a single video
  if (job.kind !== "playlist" && job.total_bytes > 0 && job.bytes != null) return Math.min(1, job.bytes / job.total_bytes);
  const total = job.total_items || 0;
  if (!total) return 0;
  let sum = 0;
  if (Array.isArray(job.items) && job.items.length) {
    for (const i of job.items) sum += i.state === "done" ? 1 : Math.min(1, Math.max(0, i.progress || 0));
    return Math.min(1, sum / total);
  }
  return Math.min(1, (job.done_items || 0) / total);
}

/** Parse a TOOLS_MISSING error from video_probe. */
export function isToolsMissing(err) {
  return /^TOOLS_MISSING:/.test(String(err?.message ?? err ?? ""));
}

/**
 * Segment map answer (`segments({ id })`, see segmap.rs) -> what the details window draws.
 * Never trusts the shape: bad or missing fields give an empty map. `cells` are clamped to
 * 0..100 and capped at 1000 entries; `rows` to 64.
 */
export function segmentView(m) {
  const out = { source: "none", cells: [], rows: [], segments: 0, done: 0, total: null };
  if (!m || typeof m !== "object") return out;
  const src = ["checkpoint", "stream", "finished", "none"].includes(m.source) ? m.source : "none";
  const num = (x) => (Number.isFinite(x) && x >= 0 ? x : 0);
  out.source = src;
  out.segments = num(m.segments);
  out.done = num(m.done);
  out.total = Number.isFinite(m.total) && m.total > 0 ? m.total : null;
  if (src === "none") return out;
  out.cells = (Array.isArray(m.cells) ? m.cells : []).slice(0, 1000).map((c) => Math.min(100, Math.max(0, Math.round(num(c)))));
  out.rows = (Array.isArray(m.rows) ? m.rows : []).slice(0, 64).map((r, i) => {
    const start = num(r?.start), end = Math.max(start, num(r?.end)), done = Math.min(num(r?.done), end - start);
    return { n: i + 1, start, end, done, pct: end > start ? Math.floor((done / (end - start)) * 100) : 100 };
  });
  return out;
}

// ---------------------------------------------------------------- v0.2: search, columns, toolbar, drop, notices

/** Search box: every word of `q` must appear in the name or the link (case-insensitive). */
export function matchesQuery(it, q) {
  const words = String(q ?? "").toLowerCase().split(/\s+/).filter(Boolean).slice(0, 8);
  if (!words.length) return true;
  const hay = `${it.name ?? ""}\n${it.url ?? ""}`.toLowerCase();
  return words.every((w) => hay.includes(w));
}

/** Table columns, in their default order. `min`/`def` widths in px; `name` is flexible by default. */
export const COLUMNS = [
  { key: "name", min: 120, def: null, fixed: true },
  { key: "size", min: 60, def: 84 },
  { key: "prog", min: 90, def: 140 },
  { key: "status", min: 70, def: 108 },
  { key: "speed", min: 60, def: 92 },
  { key: "eta", min: 56, def: 74 },
  { key: "url", min: 80, def: null },
];
const COL_MAX_W = 1200;

/**
 * Saved column layout (anything, e.g. JSON from localStorage) -> a complete, valid layout:
 * every known column exactly once, unknown keys dropped, widths clamped, the name column
 * always shown. Returns [{ key, on, w }] (w = null: default width).
 */
export function normalizeCols(saved) {
  const byKey = new Map(COLUMNS.map((c) => [c.key, c]));
  const out = [];
  const seen = new Set();
  for (const s of Array.isArray(saved) ? saved.slice(0, 32) : []) {
    const c = byKey.get(s?.key);
    if (!c || seen.has(c.key)) continue;
    seen.add(c.key);
    const w = Number.isFinite(s.w) ? Math.round(Math.min(COL_MAX_W, Math.max(c.min, s.w))) : null;
    out.push({ key: c.key, on: c.fixed ? true : s.on !== false, w });
  }
  for (const c of COLUMNS) if (!seen.has(c.key)) out.push({ key: c.key, on: true, w: null });
  return out;
}

/** CSS grid track list for the shown columns, in order. */
export function gridTemplate(cols) {
  const byKey = new Map(COLUMNS.map((c) => [c.key, c]));
  return cols.filter((c) => c.on).map((c) => {
    const d = byKey.get(c.key);
    if (c.w != null) return `${c.w}px`;
    if (d.def != null) return `${d.def}px`;
    return c.key === "name" ? "minmax(160px, 3fr)" : "minmax(120px, 2fr)";
  }).join(" ");
}

/** Move column `key` to the position of column `before` (or to the end if `before` is null). */
export function moveCol(cols, key, before) {
  const c = cols.find((x) => x.key === key);
  if (!c || key === before) return cols;
  const rest = cols.filter((x) => x.key !== key);
  const i = before == null ? rest.length : rest.findIndex((x) => x.key === before);
  rest.splice(i < 0 ? rest.length : i, 0, c);
  return rest;
}

/** Toolbar buttons that can be hidden (the "Add" button always stays). */
export const TOOLS = ["add_video", "resume", "pause", "pause_all", "remove", "clear_finished", "scheduler", "settings"];
/** Saved toolbar choice -> { hidden: Set, labels: bool }. */
export function normalizeToolbar(saved) {
  const hidden = new Set((Array.isArray(saved?.hidden) ? saved.hidden : []).filter((k) => TOOLS.includes(k)));
  return { hidden, labels: saved?.labels !== false };
}

/**
 * v0.2: "video" | "playlist" for a YouTube video or playlist PAGE, otherwise null. The same rule
 * as `eid_engine::links::video_page` (the backend refuses such a link as a file anyway); the
 * window uses it to open the video dialog instead of saving the page as a file.
 */
export function videoPage(link) {
  let u;
  try { u = new URL(String(link ?? "").trim()); } catch { return null; }
  if (!/^https?:$/.test(u.protocol) || u.username || u.password) return null;
  const host = u.hostname.toLowerCase().replace(/\.$/, "");
  const seg = u.pathname.split("/").filter(Boolean);
  const has = (k) => !!u.searchParams.get(k);
  if (host === "youtu.be") return seg.length === 1 ? "video" : null;
  const yt = ["youtube.com", "youtube-nocookie.com"].some((d) => ["", "www.", "m.", "music."].some((p) => host === p + d));
  if (!yt) return null;
  if (seg.length === 1 && seg[0] === "watch" && has("v")) return "video";
  if (seg.length === 1 && seg[0] === "playlist" && has("list")) return "playlist";
  if (seg.length === 2 && ["shorts", "live", "embed", "v"].includes(seg[0])) return "video";
  return null;
}

/**
 * v0.2 playlists: the playlist a YouTube link carries, if any. `watch?v=X&list=Y` and
 * `playlist?list=Y` give `{ listId, playlistUrl }` (canonical `playlist?list=` address, the form
 * yt-dlp reads as the whole list); `videoUrl` is set when the link names one video. Lists that
 * are personal or generated (Watch Later `WL`, Liked `LL`/`LM`, Mixes `RD…`) are not offered.
 * A bare `watch?v=` gives no playlist: it is never guessed (from a title or anything else).
 */
export function linkPlaylist(link) {
  let u;
  try { u = new URL(String(link ?? "").trim()); } catch { return null; }
  if (!/^https?:$/.test(u.protocol) || u.username || u.password) return null;
  const host = u.hostname.toLowerCase().replace(/\.$/, "");
  const yt = host === "youtu.be" || ["youtube.com", "youtube-nocookie.com"].some((d) => ["", "www.", "m.", "music."].some((p) => host === p + d));
  if (!yt) return null;
  const list = u.searchParams.get("list");
  const offer = !!list && /^[A-Za-z0-9_-]{2,64}$/.test(list) && !["WL", "LL", "LM"].includes(list) && !list.startsWith("RD");
  const kind = videoPage(link);
  const out = {};
  if (kind === "video") {
    const seg = u.pathname.split("/").filter(Boolean);
    const v = host === "youtu.be" ? seg[0] : seg[0] === "watch" ? u.searchParams.get("v") : seg[1];
    if (v && /^[A-Za-z0-9_-]{11}$/.test(v)) out.videoUrl = "https://www.youtube.com/watch?v=" + v;
  }
  if (offer) {
    out.listId = list;
    out.playlistUrl = "https://www.youtube.com/playlist?list=" + list;
  }
  return out.videoUrl || out.playlistUrl ? out : null;
}

/**
 * v0.2: a range of playlist items typed by the user: "1-10, 15, 20-25" (also "،" and "–").
 * `available` is the list of item numbers the playlist has. Returns `{ items: number[] }`
 * (sorted, only available numbers) or `{ error: "empty" | "syntax" | "none" }`.
 */
export function parseRange(text, available) {
  const have = new Set(available);
  const t = String(text ?? "").replace(/[،؛;]/g, ",").replace(/[–—]/g, "-").trim();
  if (!t) return { error: "empty" };
  const out = new Set();
  for (const part of t.split(",")) {
    const p = part.trim();
    if (!p) continue;
    const m = /^(\d{1,6})(?:\s*-\s*(\d{1,6}))?$/.exec(p);
    if (!m) return { error: "syntax" };
    let a = Number(m[1]);
    let b = m[2] === undefined ? a : Number(m[2]);
    if (a > b) [a, b] = [b, a];
    if (b - a > 100000) return { error: "syntax" };
    for (let i = a; i <= b; i++) if (have.has(i)) out.add(i);
  }
  if (!out.size) return { error: "none" };
  return { items: [...out].sort((x, y) => x - y) };
}

/**
 * Links in dropped or pasted text (text/uri-list or plain text): http(s) only, without
 * duplicates, at most `max`. Comment lines of a uri-list (#...) are skipped.
 */
export function linksFromText(text, max = 200) {
  const out = [];
  const seen = new Set();
  for (const line of String(text ?? "").split(/\r?\n/)) {
    if (line.startsWith("#")) continue;
    for (const m of line.matchAll(/https?:\/\/[^\s"'<>]+/gi)) {
      const u = m[0].replace(/[),.;\]]+$/, "");
      if (u.length > 4096 || seen.has(u)) continue;
      seen.add(u);
      out.push(u);
      if (out.length >= max) return out;
    }
  }
  return out;
}

/**
 * Items that just finished or failed: compare the store BEFORE merging a `list` answer with the
 * answer. Only items already known before (never the first list after start) are reported.
 */
export function finishedNow(store, resp) {
  const out = [];
  for (const it of Array.isArray(resp?.items) ? resp.items : []) {
    const old = store.get(it.id);
    if (!old || old.status === it.status) continue;
    if (it.status === "done" || it.status === "failed") out.push({ id: it.id, name: it.name, status: it.status });
  }
  return out;
}
