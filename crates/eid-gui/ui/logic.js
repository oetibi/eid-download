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
export function buildView(store, view, key, dir) {
  const out = [];
  for (const it of store.values()) if (inView(it, view)) out.push(it);
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
  if (job.total_bytes > 0 && job.bytes != null) return Math.min(1, job.bytes / job.total_bytes);
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
