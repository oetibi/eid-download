// Eid Download window. Talks to the Rust side through Tauri `invoke`; outside Tauri (a plain
// browser, for layout checks and benchmarks) it uses the in-memory mock in mock.js.
// Untrusted text (file names, links, titles, error details) is only ever set through
// textContent / attributes, never as HTML.
// Lightness: the download table is virtual (only the visible rows exist in the DOM), each cell
// is written only when its text changes, all DOM work is batched in requestAnimationFrame, and
// the list is polled fast only while something moves and not at all while the window is hidden.
import { TEXT } from "./i18n.js";
import {
  ACTIVE, BUSY, CATEGORIES, mergeList, buildView, inView, categoryOf, fraction, clickSelect,
  pollDelay, safeQualities, QUALITIES, optionFor, optionDetails, fmtSize, fmtClock, jobFraction, isToolsMissing, segmentView,
  COLUMNS, TOOLS, normalizeCols, gridTemplate, moveCol, normalizeToolbar, linksFromText, finishedNow, videoPage,
  linkPlaylist, parseRange,
} from "./logic.js";

const tauri = window.__TAURI__;
const usingMock = !tauri;
const api = tauri
  ? { call: (cmd, args) => tauri.core.invoke(cmd, args), on: (name, f) => tauri.event.listen(name, (e) => f(e.payload)) }
  : (await import("./mock.js")).mockApi(new URLSearchParams(location.search));

const $ = (s) => document.querySelector(s);
const $$ = (s) => document.querySelectorAll(s);
const ROW_H = 28; // keep in sync with .tr height in style.css
const OVERSCAN = 6;

// ---------------------------------------------------------------- settings (local only)
const settings = {
  lang: localStorage.getItem("lang") === "en" ? "en" : "ar",
  theme: ["light", "dark"].includes(localStorage.getItem("theme")) ? localStorage.getItem("theme") : "system",
  folder: localStorage.getItem("folder") || null,
  quality: localStorage.getItem("quality") || "best",
  sidebar: localStorage.getItem("sidebar") !== "off",
  notify: localStorage.getItem("notify") !== "off",
  missing: ["lower", "higher", "skip"].includes(localStorage.getItem("missing")) ? localStorage.getItem("missing") : "lower",
};
function saveSetting(k, v) {
  settings[k] = v;
  if (v == null) localStorage.removeItem(k); else localStorage.setItem(k, typeof v === "boolean" ? (v ? "on" : "off") : v);
}

const t = (k, ...a) => {
  const v = TEXT[settings.lang][k];
  return typeof v === "function" ? v(...a) : v ?? k;
};
const errText = (k) => TEXT[settings.lang].err[k] ?? null;
const units = () => [t("unit_b"), t("unit_kb"), t("unit_mb"), t("unit_gb"), t("unit_tb")];
const size = (n) => fmtSize(n, units());

// ---------------------------------------------------------------- state
const store = new Map(); // id -> item as sent by `list`
let rev = null; // last revision from `list` (null = backend without revisions)
let defaultDir = "";
let view = "all";
let sortKey = "id", sortDir = -1;
let viewItems = []; // items shown, sorted
let order = []; // their ids
let selected = new Set();
let anchor = null;
let cursor = null; // keyboard focus row id
let propsId = null;
let pickedFolder = null;
let query = "";
let jobs = [];
let jobsAvailable = true;
let jobsRetryAt = 0;
let firstRenderDone = false;
const stats = { polls: [], renders: [] }; // only read by the benchmark (mock mode)

// ---------------------------------------------------------------- small helpers
function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
}
function setText(node, v) {
  if (node._v !== v) { node._v = v; node.textContent = v; }
}
function setAttr(node, name, v) {
  const k = `_a_${name}`;
  if (node[k] !== v) { node[k] = v; if (v == null) node.removeAttribute(name); else node.setAttribute(name, v); }
}
let toastTimer;
function toast(msg) {
  const e = $("#toast");
  e.textContent = msg;
  e.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (e.hidden = true), 4500);
}
function errMessage(e) {
  const raw = String(e?.message ?? e);
  const key = raw.replace(/^invalid input: /, "").replace(/ /g, "_");
  if (/not found|unknown command|not allowed/i.test(raw) && /command/i.test(raw)) return errText("not_available");
  return errText(key) ?? errText(raw) ?? raw;
}
const failure = (e) => toast(errMessage(e));

// ---------------------------------------------------------------- theme & language
function applyTheme() {
  if (settings.theme === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = settings.theme;
}

function applyLang() {
  const d = document.documentElement;
  d.lang = settings.lang;
  d.dir = settings.lang === "ar" ? "rtl" : "ltr";
  document.title = t("app_title");
  sendTrayTexts();
  for (const n of $$("[data-t]")) n.textContent = t(n.dataset.t);
  $("#in-links").placeholder = t("links_ph");
  $("#in-vurl").placeholder = "https://www.youtube.com/watch?v=…";
  $("#search").placeholder = t("search_ph");
  $("#search").setAttribute("aria-label", t("search_ph"));
  for (const n of $$("fieldset.soon, fieldset.soon *")) n.title = t("soon_tip");
  for (const row of pool) row.reset();
  folderKey = "";
  jobEls.forEach((x) => x.li.remove());
  jobEls.clear();
  showFolder();
  invalidate(true);
}

// ---------------------------------------------------------------- polling
let pollTimer = null;
let polling = false;
let pollAgain = false;
let pollCount = 0;
let gotList = false;

let powerActive = false;
function anyBusy() {
  if (powerActive) return true;
  for (const it of store.values()) if (BUSY.has(it.status)) return true;
  return jobs.some((j) => j.state === "queued" || j.state === "running");
}

function schedulePoll() {
  clearTimeout(pollTimer);
  const d = pollDelay({ hidden: document.visibilityState === "hidden", busy: anyBusy() });
  if (d != null) pollTimer = setTimeout(poll, d);
}

async function poll() {
  if (polling) { pollAgain = true; return; }
  polling = true;
  pollAgain = false;
  try {
    await refresh();
    const wantJobs = view === "jobs" || jobs.some((j) => j.state === "queued" || j.state === "running") || pollCount % 5 === 0;
    if (wantJobs) await refreshJobs();
    pollCount++;
  } finally {
    polling = false;
    if (pollAgain) kick(); else schedulePoll();
  }
}

async function refresh() {
  const t0 = performance.now();
  try {
    const s = await api.call("list", rev != null ? { since: rev } : {});
    if (s?.default_dir) defaultDir = s.default_dir;
    const fin = gotList && settings.notify ? finishedNow(store, s) : [];
    const r = mergeList(store, s);
    if (fin.length) notify(fin);
    if (s && "power" in s) applyPower(s.power);
    // engine warnings (e.g. a resume point that could not be saved): shown once each
    if (Array.isArray(s?.warnings)) for (const w of s.warnings.slice(0, 5)) {
      if (Array.isArray(w) && typeof w[1] === "string") toast(t("w_checkpoint", store.get(w[0])?.name ?? `#${w[0]}`, w[1].slice(0, 300)));
    }
    // v0.2: "Open in Eid Download" from the browser: the Video window with that link (the
    // window was brought to the front by the app); the newest request wins
    if (Array.isArray(s?.open_video) && s.open_video.length) {
      const u = s.open_video[s.open_video.length - 1];
      if (typeof u === "string" && u.length <= 8192 && /^https?:\/\/\S+$/i.test(u)) openFromBrowser(u);
    }
    rev = r.rev;
    if (r.changed || !gotList) invalidate(true);
    gotList = true;
  } catch (e) {
    console.error(e);
  }
  if (usingMock) { stats.polls.push(performance.now() - t0); if (stats.polls.length > 500) stats.polls.shift(); }
}

/** Ask the next poll to come now (after a user action). */
function kick() {
  clearTimeout(pollTimer);
  pollTimer = setTimeout(poll, 0);
}

document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "hidden") clearTimeout(pollTimer);
  else kick();
});

// ---------------------------------------------------------------- render scheduling
let rafPending = false;
let dirtyData = false;
function invalidate(data) {
  if (data) dirtyData = true;
  if (!rafPending) { rafPending = true; requestAnimationFrame(frame); }
}

function frame() {
  rafPending = false;
  const t0 = performance.now();
  if (dirtyData) {
    dirtyData = false;
    recomputeView();
    renderSidebar();
    renderStatusBar();
    renderProps();
    if (view === "jobs") renderJobs();
    updateCommands();
  }
  renderRows();
  if (usingMock) { stats.renders.push(performance.now() - t0); if (stats.renders.length > 500) stats.renders.shift(); }
  if (!firstRenderDone && gotList) {
    firstRenderDone = true;
    performance.mark("eid-first-render");
  }
}

function recomputeView() {
  viewItems = view === "jobs" ? [] : buildView(store, view, sortKey, sortDir, query);
  order = viewItems.map((i) => i.id);
  for (const id of selected) if (!store.has(id)) selected.delete(id);
  $("#spacer").style.height = `${viewItems.length * ROW_H}px`;
  const empty = $("#empty");
  const isEmpty = viewItems.length === 0 && view !== "jobs";
  if (empty.hidden !== !isEmpty) empty.hidden = !isEmpty;
  if (isEmpty) setText(empty, store.size === 0 ? t("empty") : t("empty_filter"));
}

// ---------------------------------------------------------------- virtual table
const viewport = $("#viewport");
const pool = [];

class Row {
  constructor() {
    const r = el("div", "tr");
    r.setAttribute("role", "row");
    this.el = r;
    this.name = el("bdi", "c-name");
    this.name.dir = "auto";
    this.size = el("div", "c-size");
    this.prog = el("div", "c-prog");
    this.bar = el("div", "pbar");
    this.fill = el("i");
    this.bar.append(this.fill);
    this.pct = el("span", "pct");
    this.prog.append(this.bar, this.pct);
    this.status = el("div", "c-status");
    this.speed = el("div", "c-speed");
    this.eta = el("div", "c-eta");
    this.url = el("bdi", "c-url");
    this.url.dir = "ltr";
    r.append(this.name, this.size, this.prog, this.status, this.speed, this.eta, this.url);
    this.reset();
  }
  reset() {
    this.id = null; this.top = null; this.cls = null; this.frac = null;
    for (const c of [this.name, this.size, this.pct, this.status, this.speed, this.eta, this.url]) { c._v = undefined; c._a_title = undefined; }
  }
  bind(it, idx) {
    const top = idx * ROW_H;
    if (this.top !== top) { this.top = top; this.el.style.transform = `translateY(${top}px)`; }
    if (this.id !== it.id) { this.id = it.id; this.el.dataset.id = it.id; }
    const sel = selected.has(it.id);
    const cls = `tr s-${it.status}${sel ? " sel" : ""}${cursor === it.id ? " cur" : ""}`;
    if (this.cls !== cls) { this.cls = cls; this.el.className = cls; setAttr(this.el, "aria-selected", sel ? "true" : "false"); }
    setText(this.name, it.name);
    setAttr(this.name, "title", it.name);
    const r = it.rate;
    setText(this.size, r?.total != null ? size(r.total) : r ? `${size(r.done)} / ?` : "");
    const f = fraction(it);
    const fv = f == null ? 0 : Math.round(f * 1000) / 1000;
    if (this.frac !== fv) { this.frac = fv; this.fill.style.transform = `scaleX(${fv})`; }
    setText(this.pct, f == null ? "" : `${(f * 100).toFixed(1)}%`);
    setText(this.status, t(`st_${it.status}`));
    const errTip = it.error_kind && it.status !== "done" ? `${errText(it.error_kind) ?? it.error_kind}\n${t("details")}: ${it.error ?? ""}` : null;
    setAttr(this.status, "title", errTip);
    const running = it.status === "running";
    setText(this.speed, running && r?.speed > 0 ? `${size(r.speed)}${t("unit_s")}` : "");
    setText(this.eta, running && r?.eta != null ? fmtClock(r.eta) : "");
    setText(this.url, it.url);
    setAttr(this.url, "title", `${it.url}\n${it.folder}`);
    if (this.el.hidden) this.el.hidden = false;
  }
  hide() {
    if (!this.el.hidden) this.el.hidden = true;
    this.id = null;
  }
}

function ensurePool() {
  const need = Math.ceil((viewport.clientHeight || 600) / ROW_H) + OVERSCAN * 2;
  while (pool.length < need) {
    const r = new Row();
    pool.push(r);
    viewport.append(r.el);
  }
}

function renderRows() {
  ensurePool();
  const first = Math.max(0, Math.floor(viewport.scrollTop / ROW_H) - OVERSCAN);
  for (let k = 0; k < pool.length; k++) {
    const idx = first + k;
    const it = viewItems[idx];
    if (it) pool[k].bind(it, idx); else pool[k].hide();
  }
}

viewport.addEventListener("scroll", () => invalidate(false), { passive: true });
new ResizeObserver(() => invalidate(false)).observe(viewport);

function scrollToRow(id) {
  const idx = order.indexOf(id);
  if (idx < 0) return;
  const top = idx * ROW_H, h = viewport.clientHeight;
  if (top < viewport.scrollTop) viewport.scrollTop = top;
  else if (top + ROW_H > viewport.scrollTop + h) viewport.scrollTop = top + ROW_H - h;
}

function rowIdFrom(e) {
  const r = e.target.closest?.(".tr");
  return r && r.dataset.id ? Number(r.dataset.id) : null;
}

viewport.addEventListener("click", (e) => {
  const id = rowIdFrom(e);
  if (id == null) {
    if (!e.ctrlKey && !e.shiftKey) { selected = new Set(); invalidate(true); }
    return;
  }
  const s = clickSelect(selected, anchor, order, id, { ctrl: e.ctrlKey || e.metaKey, shift: e.shiftKey });
  selected = s.selected; anchor = s.anchor; cursor = id;
  viewport.focus({ preventScroll: true });
  invalidate(true);
});
viewport.addEventListener("dblclick", (e) => {
  const id = rowIdFrom(e);
  if (id != null) openProps(id);
});
viewport.addEventListener("contextmenu", (e) => {
  e.preventDefault();
  const id = rowIdFrom(e);
  if (id == null) return;
  if (!selected.has(id)) { selected = new Set([id]); anchor = id; cursor = id; invalidate(true); }
  updateCommands();
  showMenu($("#ctx"), e.clientX, e.clientY);
});

// sortable headers
for (const b of $$("#thead [data-sort]")) {
  b.addEventListener("click", () => {
    const k = b.dataset.sort;
    if (sortKey === k) sortDir = -sortDir;
    else { sortKey = k; sortDir = k === "name" || k === "url" || k === "status" ? 1 : -1; }
    for (const x of $$("#thead [data-sort]")) x.removeAttribute("aria-sort");
    b.setAttribute("aria-sort", sortDir === 1 ? "ascending" : "descending");
    invalidate(true);
  });
}

// ---------------------------------------------------------------- sidebar & status bar
let folderKey = "";
function renderSidebar() {
  const counts = { all: store.size, active: 0, unfinished: 0, finished: 0, failed: 0 };
  for (const c of CATEGORIES) counts[c] = 0;
  counts.other = 0;
  const folders = new Map();
  for (const it of store.values()) {
    if (BUSY.has(it.status)) counts.active++;
    if (it.status !== "done") counts.unfinished++; else counts.finished++;
    if (it.status === "failed") counts.failed++;
    counts[categoryOf(it.name)]++;
    folders.set(it.folder, (folders.get(it.folder) ?? 0) + 1);
  }
  counts.jobs = jobs.length;
  for (const b of $$("[data-count]")) setText(b, String(counts[b.dataset.count] ?? 0));
  // folders: rebuilt only when the set of folders changes
  const list = [...folders.keys()].sort().slice(0, 30);
  const key = list.join("\n");
  const ul = $("#tree-folders");
  if (key !== folderKey) {
    folderKey = key;
    ul.replaceChildren(...list.map((f) => {
      const li = el("li");
      const b = el("button");
      b.type = "button";
      b.dataset.view = `folder:${f}`;
      b.title = f;
      const name = el("bdi", null, f.split(/[\\/]/).filter(Boolean).pop() || f);
      name.dir = "auto";
      b.append(svgIcon("i-folder"), name, el("b"));
      b.classList.toggle("on", view === b.dataset.view);
      li.append(b);
      return li;
    }));
  }
  for (const b of ul.querySelectorAll("button")) setText(b.querySelector("b"), String(folders.get(b.dataset.view.slice(7)) ?? 0));
}

function svgIcon(id) {
  const ns = "http://www.w3.org/2000/svg";
  const s = document.createElementNS(ns, "svg");
  s.setAttribute("class", "ic");
  const u = document.createElementNS(ns, "use");
  u.setAttribute("href", `#${id}`);
  s.append(u);
  return s;
}

document.querySelector(".sidebar").addEventListener("click", (e) => {
  const b = e.target.closest("[data-view]");
  if (b) setView(b.dataset.view);
});

function setView(v) {
  view = v;
  for (const b of $$("[data-view]")) b.classList.toggle("on", b.dataset.view === v);
  const jobsView = v === "jobs";
  $("#table-view").hidden = jobsView;
  $("#jobs-view").hidden = !jobsView;
  viewport.scrollTop = 0;
  if (jobsView) { renderJobs(); refreshJobs().then(() => invalidate(true)); }
  invalidate(true);
}

function renderStatusBar() {
  let running = 0, speed = 0;
  for (const it of store.values()) {
    if (it.status === "running") { running++; speed += it.rate?.speed ?? 0; }
  }
  for (const j of jobs) if (j.state === "running") speed += j.speed ?? 0;
  setText($("#sb-active"), t("sb_active", running));
  setText($("#sb-speed"), t("sb_speed", `${size(speed)}${t("unit_s")}`));
  setText($("#sb-count"), t("sb_count", store.size));
  setText($("#sb-selected"), t("sb_selected", selected.size));
}

// ---------------------------------------------------------------- commands
const selItems = () => [...selected].map((id) => store.get(id)).filter(Boolean);
const SOON = new Set();

async function each(list, cmd) {
  const res = await Promise.allSettled(list.map((it) => api.call(cmd, { id: it.id })));
  const bad = res.find((r) => r.status === "rejected");
  if (bad) failure(bad.reason);
  kick();
}

const commands = {
  add: { run: () => openAdd() },
  add_video: { run: () => openVideo() },
  columns: { run: () => { const r = $("#thead").getBoundingClientRect(); showColsMenu(document.documentElement.dir === "rtl" ? r.right - 20 : r.left + 20, r.bottom); } },
  customize_toolbar: { run: () => openToolbarDialog() },
  queues: { run: () => openQueues() },
  import_list: { run: async () => {
    try {
      const r = await api.call("import_list", { folder: settings.folder ?? null });
      if (r) { toast(t("imp_done", r.added, r.refused, r.duplicates)); kick(); }
    } catch (e) { failure(e); }
  } },
  export_list: { run: async () => {
    try { const n = await api.call("export_list"); if (n != null) toast(t("exp_done", n)); } catch (e) { failure(e); }
  } },
  find: { run: () => { $("#search").focus(); $("#search").select(); } },
  settings: { run: () => openSettings() },
  resume: {
    on: () => selItems().some((i) => i.status === "paused" || i.status === "failed"),
    run: () => each(selItems().filter((i) => i.status === "paused" || i.status === "failed"), "resume"),
  },
  pause: {
    on: () => selItems().some((i) => BUSY.has(i.status)),
    run: () => each(selItems().filter((i) => BUSY.has(i.status)), "pause"),
  },
  toggle: {
    on: () => selItems().some((i) => i.status !== "done"),
    run: () => (commands.pause.on() ? commands.pause.run() : commands.resume.run()),
  },
  pause_all: {
    on: () => [...store.values()].some((i) => BUSY.has(i.status)),
    run: () => each([...store.values()].filter((i) => BUSY.has(i.status)), "pause"),
  },
  resume_all: {
    on: () => [...store.values()].some((i) => i.status === "paused"),
    run: () => each([...store.values()].filter((i) => i.status === "paused"), "resume"),
  },
  remove: {
    on: () => selItems().length > 0,
    run: async () => {
      const list = selItems();
      const can = list.filter((i) => i.status !== "running");
      const res = await Promise.allSettled(can.map((it) => api.call("remove", { id: it.id })));
      const ok = res.filter((r) => r.status === "fulfilled").length;
      const bad = res.find((r) => r.status === "rejected");
      const skipped = list.length - can.length;
      const msg = [ok === 1 && list.length === 1 ? t("removed") : ok ? t("removed_n", ok) : null, skipped ? t("skipped_running", skipped) : null, bad ? errMessage(bad.reason) : null].filter(Boolean);
      if (msg.length) toast(msg.join("\n"));
      kick();
    },
  },
  clear_finished: {
    on: () => [...store.values()].some((i) => i.status === "done"),
    run: async () => {
      try { toast(t("cleared_n", await api.call("clear_finished"))); } catch (e) { failure(e); }
      kick();
    },
  },
  select_all: {
    on: () => view !== "jobs" && order.length > 0,
    run: () => { selected = new Set(order); anchor = order[0] ?? null; invalidate(true); },
  },
  properties: { on: () => selected.size > 0, run: () => openProps(selItems()[0]?.id) },
  reveal: {
    on: () => selItems().length === 1 && selItems()[0].status === "done",
    run: async () => { try { await api.call("reveal", { id: selItems()[0].id }); } catch (e) { failure(e); } },
  },
  open_file: {
    on: () => selItems().length === 1 && selItems()[0].status === "done",
    run: () => openFile(selItems()[0].id),
  },
  scheduler: { run: () => openScheduler() },
  redownload: {
    on: () => selItems().some((i) => i.status === "failed"),
    run: () => each(selItems().filter((i) => i.status === "failed"), "resume"),
  },
  copy_link: {
    on: () => selItems().length > 0,
    run: () => copyText(selItems().map((i) => i.url).join("\n")),
  },
  toggle_sidebar: {
    run: () => { saveSetting("sidebar", !settings.sidebar); applySidebar(); },
  },
  view_jobs: { run: () => setView("jobs") },
  theme_system: { run: () => { saveSetting("theme", "system"); applyTheme(); updateCommands(); } },
  theme_light: { run: () => { saveSetting("theme", "light"); applyTheme(); updateCommands(); } },
  theme_dark: { run: () => { saveSetting("theme", "dark"); applyTheme(); updateCommands(); } },
  lang: { run: () => setLang(settings.lang === "ar" ? "en" : "ar") },
  shortcuts: { run: () => openInfo(t("c_shortcuts"), t("shortcuts_body")) },
  about: { run: () => openInfo(t("c_about"), t("about_body")) },
};

function updateCommands() {
  for (const n of $$("[data-cmd]")) {
    const c = commands[n.dataset.cmd];
    if (!c) continue;
    const on = c.on ? c.on() : true;
    if (n.disabled !== !on) n.disabled = !on;
    if (SOON.has(n.dataset.cmd)) setAttr(n, "title", t("soon_tip"));
    if (n.dataset.cmd.startsWith("theme_")) setAttr(n, "aria-checked", n.dataset.cmd === `theme_${settings.theme}` ? "true" : "false");
  }
}

function run(cmd) {
  const c = commands[cmd];
  if (!c || (c.on && !c.on())) return;
  closeMenus();
  Promise.resolve(c.run()).catch(failure);
}

document.addEventListener("click", (e) => {
  const n = e.target.closest?.("[data-cmd]");
  if (n && !n.disabled) { run(n.dataset.cmd); return; }
  if (!e.target.closest?.(".menu, [data-menu]")) closeMenus();
});

async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const ta = el("textarea");
    ta.value = text;
    document.body.append(ta);
    ta.select();
    document.execCommand("copy");
    ta.remove();
  }
  toast(t("copied"));
}

function applySidebar() {
  $("#sidebar").hidden = !settings.sidebar;
}

function setLang(l) {
  saveSetting("lang", l);
  applyLang();
  renderCheck();
}

// ---------------------------------------------------------------- menus
let openMenu = null;
function closeMenus() {
  for (const m of $$(".menu")) m.hidden = true;
  for (const b of $$("[data-menu]")) b.classList.remove("open");
  openMenu = null;
}
function showMenu(menu, x, y) {
  closeMenus();
  menu.hidden = false;
  const r = menu.getBoundingClientRect();
  const left = Math.max(4, Math.min(x - (document.documentElement.dir === "rtl" ? r.width : 0), innerWidth - r.width - 4));
  const top = Math.max(4, Math.min(y, innerHeight - r.height - 4));
  menu.style.left = `${left}px`;
  menu.style.top = `${top}px`;
  openMenu = menu;
  menu.querySelector("button:not(:disabled)")?.focus();
}
function openMenuFor(b) {
  updateCommands();
  const m = $(`#menu-${b.dataset.menu}`);
  const r = b.getBoundingClientRect();
  showMenu(m, document.documentElement.dir === "rtl" ? r.right : r.left, r.bottom);
  b.classList.add("open");
}
for (const b of $$("[data-menu]")) {
  b.addEventListener("click", () => {
    const m = $(`#menu-${b.dataset.menu}`);
    if (openMenu === m) closeMenus(); else openMenuFor(b);
  });
  b.addEventListener("mouseenter", () => {
    if (openMenu && !openMenu.classList.contains("ctx") && openMenu !== $(`#menu-${b.dataset.menu}`)) openMenuFor(b);
  });
}
// arrow keys inside an open menu
document.addEventListener("keydown", (e) => {
  if (!openMenu) return;
  const items = [...openMenu.querySelectorAll("button:not(:disabled)")];
  const i = items.indexOf(document.activeElement);
  if (e.key === "Escape") { e.preventDefault(); closeMenus(); viewport.focus(); }
  else if (e.key === "ArrowDown") { e.preventDefault(); items[(i + 1) % items.length]?.focus(); }
  else if (e.key === "ArrowUp") { e.preventDefault(); items[(i - 1 + items.length) % items.length]?.focus(); }
  else return;
  e.stopImmediatePropagation();
}, true);
window.addEventListener("blur", closeMenus);
window.addEventListener("resize", closeMenus);

// ---------------------------------------------------------------- keyboard
const inField = (e) => e.target.closest?.("input, textarea, select, [contenteditable]");
const anyDialog = () => document.querySelector("dialog[open]");

document.addEventListener("keydown", (e) => {
  const ctrl = e.ctrlKey || e.metaKey;
  if (anyDialog()) return;
  if (ctrl && e.shiftKey && (e.key === "N" || e.key === "n")) { e.preventDefault(); run("add_video"); return; }
  if (ctrl && (e.key === "n" || e.key === "N")) { e.preventDefault(); run("add"); return; }
  if (ctrl && e.key === ",") { e.preventDefault(); run("settings"); return; }
  if (ctrl && (e.key === "f" || e.key === "F")) { e.preventDefault(); run("find"); return; }
  if (e.key === "F1") { e.preventDefault(); run("shortcuts"); return; }
  if (inField(e) || view === "jobs") return;
  const onButton = e.target.closest?.("button");
  if (ctrl && (e.key === "a" || e.key === "A")) { e.preventDefault(); run("select_all"); return; }
  if (e.key === "Delete") { e.preventDefault(); run("remove"); return; }
  if (onButton) return; // Space/Enter on a button press that button
  if (e.key === " ") { e.preventDefault(); run("toggle"); return; }
  if (e.key === "Enter") { e.preventDefault(); run("properties"); return; }
  if ((e.shiftKey && e.key === "F10") || e.key === "ContextMenu") {
    e.preventDefault();
    if (!selected.size) return;
    const id = cursor ?? [...selected][0];
    scrollToRow(id);
    const vr = viewport.getBoundingClientRect();
    const y = vr.top + order.indexOf(id) * ROW_H - viewport.scrollTop + ROW_H;
    updateCommands();
    showMenu($("#ctx"), document.documentElement.dir === "rtl" ? vr.right - 40 : vr.left + 40, y);
    return;
  }
  const moves = { ArrowDown: 1, ArrowUp: -1, PageDown: 10, PageUp: -10, Home: -Infinity, End: Infinity };
  if (e.key in moves && order.length) {
    e.preventDefault();
    const cur = order.indexOf(cursor);
    let next = cur < 0 ? 0 : cur + moves[e.key];
    next = Math.max(0, Math.min(order.length - 1, Number.isFinite(next) ? next : next > 0 ? order.length - 1 : 0));
    const id = order[next];
    const s = clickSelect(selected, anchor, order, id, { shift: e.shiftKey, ctrl: false });
    selected = s.selected; anchor = e.shiftKey ? s.anchor : id; cursor = id;
    scrollToRow(id);
    invalidate(true);
  }
});

// Ctrl+V anywhere outside a text field: open "add" (or the video dialog for a video site link)
document.addEventListener("paste", (e) => {
  if (inField(e) || anyDialog()) return;
  const text = (e.clipboardData?.getData("text") ?? "").trim();
  if (!/^https?:\/\//i.test(text)) return;
  e.preventDefault();
  if (!/\s/.test(text) && videoPage(text)) openVideo(text);
  else openAdd(text);
});

// ---------------------------------------------------------------- add dialog (behaviour of v0.0.1 kept)
const effectiveFolder = () => pickedFolder ?? settings.folder ?? null;
function showFolder() {
  const f = $("#folder-show");
  const eff = effectiveFolder();
  f.textContent = eff ?? `${t("folder_default")} — ${defaultDir}`;
  f.title = eff ?? defaultDir;
  $("#btn-reset").hidden = !pickedFolder;
}

function openAdd(prefill) {
  $("#add-error").hidden = true;
  $("#in-links").value = prefill ?? "";
  $("#in-name").value = "";
  pickedFolder = null;
  showFolder();
  fillAddChoices();
  $("#dlg-add").showModal();
  $("#in-links").focus();
}

// v0.2: queue + "folder by type" choices of the add dialog (lists fetched when it opens)
const CAT_KEY = { Compressed: "v_compressed", Documents: "v_documents", Music: "v_music", Video: "v_video", Pictures: "v_pictures", Programs: "v_programs" };
let addQueues = [];
async function fillAddChoices() {
  const cat = $("#in-cat");
  let cats = [];
  try { cats = await api.call("categories"); } catch { cats = []; }
  const opt = (v, text) => { const o = el("option", null, text); o.value = v; return o; };
  cat.replaceChildren(opt("none", t("cat_none")), opt("auto", t("cat_auto")),
    ...cats.filter((c) => typeof c === "string").map((c) => opt(c, CAT_KEY[c] ? t(CAT_KEY[c]) : c)));
  cat.value = localStorage.getItem("autocat") === "on" ? "auto" : "none";
  const q = $("#in-queue");
  try { addQueues = await api.call("queues"); } catch { addQueues = []; }
  q.replaceChildren(...addQueues.map((x, i) => opt(String(i), x.name === "main" ? t("q_main") : queueLabel(x))));
  q.value = "0";
  q.disabled = addQueues.length < 2;
}
$("#in-queue").addEventListener("change", () => {
  const x = addQueues[Number($("#in-queue").value)];
  pickedFolder = x && x.name !== "main" ? x.dir : null;
  showFolder();
});

async function submitAdd(ev) {
  ev.preventDefault();
  const errEl = $("#add-error");
  errEl.hidden = true;
  const start = $("#btn-start");
  start.disabled = true;
  try {
    const name = $("#in-name").value.trim();
    const r = await api.call("add", { links: $("#in-links").value, folder: effectiveFolder(), name: name || null, category: $("#in-cat").value || "none" });
    // v0.2: YouTube pages are video downloads, never files: when they are the only links that
    // were not added, the video dialog opens with the first one
    const pages = r.refused.filter(([, k]) => k === "video_page");
    if (pages.length && pages.length === r.refused.length) {
      $("#dlg-add").close();
      if (r.added.length) toast(t("added_n", r.added.length));
      openVideo(pages[0][0]);
      toast(t("routed_video", pages.length));
    } else if (r.refused.length) {
      const lines = r.refused.map(([u, k]) => `• ⁨${u}⁩ — ${errText(k) ?? k}`);
      errEl.textContent = `${r.added.length ? t("added_n", r.added.length) + "\n" : ""}${t("refused_n", r.refused.length)}\n${lines.join("\n")}`;
      errEl.hidden = false;
      $("#in-links").value = r.refused.map(([u]) => u).join("\n");
    } else {
      $("#dlg-add").close();
      toast(t("added_n", r.added.length));
    }
    kick();
  } catch (e) {
    errEl.textContent = errMessage(e);
    errEl.hidden = false;
  } finally {
    start.disabled = false;
  }
}

$("#btn-cancel").addEventListener("click", () => $("#dlg-add").close());
$("#form-add").addEventListener("submit", submitAdd);
$("#btn-pick").addEventListener("click", async () => {
  try {
    const p = await api.call("pick_folder");
    if (p) { pickedFolder = p; showFolder(); }
  } catch (e) { failure(e); }
});
$("#btn-reset").addEventListener("click", () => { pickedFolder = null; showFolder(); });
$("#btn-lang").addEventListener("click", () => run("lang"));

// ---------------------------------------------------------------- properties dialog
function openProps(id) {
  if (id == null || !store.has(id)) return;
  propsId = id;
  renderProps();
  const lim = store.get(id)?.limit_kib;
  $("#p-limit").value = lim ? String(lim) : "0";
  $("#dlg-props").showModal();
  drawMap(segmentView(null));
  refreshMap();
}

// ---------------------------------------------------------------- segment map (details window)
// Asked for only while the details window is open: every second for a moving download, every
// 4 s otherwise. One request at a time; an answer for an item that is no longer shown is dropped.
let mapTimer = null;
let mapBusy = false;
async function refreshMap() {
  clearTimeout(mapTimer);
  mapTimer = null;
  const dlg = $("#dlg-props");
  if (propsId == null || !dlg.open || mapBusy) return;
  const id = propsId;
  mapBusy = true;
  let v;
  try { v = segmentView(await api.call("segments", { id })); } catch { v = segmentView(null); }
  mapBusy = false;
  if (propsId !== id || !dlg.open) return;
  drawMap(v);
  mapTimer = setTimeout(refreshMap, BUSY.has(store.get(id)?.status) ? 1000 : 4000);
}
function drawMap(v) {
  const c = $("#p-map");
  const dpr = window.devicePixelRatio || 1;
  const w = Math.max(1, c.clientWidth || 480), h = Math.max(1, c.clientHeight || 22);
  if (c.width !== Math.round(w * dpr) || c.height !== Math.round(h * dpr)) {
    c.width = Math.round(w * dpr); c.height = Math.round(h * dpr);
  }
  const ctx = c.getContext("2d");
  if (!ctx) return;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  const css = getComputedStyle(document.documentElement);
  const col = (k) => css.getPropertyValue(k).trim() || "#888";
  ctx.clearRect(0, 0, w, h);
  ctx.fillStyle = col("--bar");
  ctx.fillRect(0, 0, w, h);
  // bytes always run left to right (an offset scale), in both languages
  const n = v.cells.length;
  if (n) {
    const fill = col(v.source === "finished" ? "--ok" : "--run");
    const cw = w / n;
    for (let k = 0; k < n; k++) {
      const p = v.cells[k];
      if (!p) continue;
      ctx.globalAlpha = p >= 100 ? 1 : 0.25 + 0.6 * (p / 100);
      ctx.fillStyle = fill;
      ctx.fillRect(Math.floor(k * cw), 0, Math.ceil(cw), h);
    }
    ctx.globalAlpha = 1;
    // segment borders
    if (v.total && v.rows.length > 1) {
      ctx.fillStyle = col("--panel");
      for (const r of v.rows.slice(1)) ctx.fillRect(Math.round((r.start / v.total) * w), 0, 1, h);
    }
  }
  let info;
  if (v.source === "checkpoint") info = t("p_map_checkpoint", v.segments, size(v.done), size(v.total));
  else if (v.source === "stream") info = t("p_map_stream", size(v.done), size(v.total));
  else if (v.source === "finished") info = t("p_map_finished");
  else info = t("p_map_none");
  if (v.source === "checkpoint" && v.segments > v.rows.length) info += ` ${t("p_map_more", v.rows.length, v.segments)}`;
  setText($("#p-map-info"), info);
  setAttr(c, "aria-label", info);
  const wrap = $("#p-segs-wrap");
  wrap.hidden = v.rows.length === 0;
  const tb = $("#p-segs tbody");
  const rows = v.rows.map((r) => {
    const tr = el("tr");
    for (const x of [String(r.n), size(r.start), size(r.end), `${r.pct}%`]) { const td = el("td", null, x); td.dir = "auto"; tr.append(td); }
    if (r.pct >= 100) tr.className = "full";
    return tr;
  });
  tb.replaceChildren(...rows);
}
function renderProps() {
  const dlg = $("#dlg-props");
  if (propsId == null || !dlg.open) return;
  const it = store.get(propsId);
  if (!it) { dlg.close(); return; }
  const r = it.rate;
  setText($("#p-name"), it.name);
  setText($("#p-status"), t(`st_${it.status}`));
  const f = fraction(it);
  setText($("#p-done"), r ? (r.total != null ? `${size(r.done)} ${t("of")} ${size(r.total)}${f != null ? ` (${(f * 100).toFixed(1)}%)` : ""}` : `${size(r.done)} · ${t("unknown_size")}`) : "—");
  setText($("#p-speed"), it.status === "running" && r?.speed ? `${size(r.speed)}${t("unit_s")}` : "—");
  setText($("#p-eta"), it.status === "running" && r?.eta != null ? fmtClock(r.eta) : "—");
  setText($("#p-attempts"), String(it.attempts ?? 0));
  setText($("#p-url"), it.url);
  setText($("#p-folder"), it.folder);
  $("#p-bar").style.transform = `scaleX(${f ?? 0})`;
  $("#p-bar").parentElement.className = `pbar big s-${it.status}`;
  const pe = $("#p-error");
  const hasErr = !!(it.error_kind && it.status !== "done");
  pe.hidden = !hasErr;
  if (hasErr) setText(pe, `${errText(it.error_kind) ?? it.error_kind}\n${t("details")}: ${it.error ?? ""}`);
  const tg = $("#p-toggle");
  const label = BUSY.has(it.status) ? t("c_pause") : it.status === "failed" ? t("c_redownload") : t("c_resume");
  setText(tg, label);
  tg.disabled = it.status === "done";
  $("#p-reveal").disabled = it.status !== "done";
}
$("#p-toggle").addEventListener("click", async () => {
  const it = store.get(propsId);
  if (!it) return;
  try { await api.call(BUSY.has(it.status) ? "pause" : "resume", { id: it.id }); } catch (e) { failure(e); }
  kick();
});
$("#p-reveal").addEventListener("click", async () => {
  try { await api.call("reveal", { id: propsId }); } catch (e) { failure(e); }
});
$("#p-copy").addEventListener("click", () => { const it = store.get(propsId); if (it) copyText(it.url); });
$("#dlg-props").addEventListener("close", () => { propsId = null; clearTimeout(mapTimer); mapTimer = null; viewport.focus(); });

// ---------------------------------------------------------------- info dialog
function openInfo(title, body) {
  $("#info-title").textContent = title;
  const b = $("#info-body");
  if (Array.isArray(body)) {
    const dl = el("dl", "grid keys");
    for (const [k, v] of body) {
      const dt = el("dt");
      dt.append(el("kbd", null, k));
      dl.append(dt, el("dd", null, v));
    }
    b.replaceChildren(dl);
  } else {
    b.replaceChildren(el("p", null, body));
  }
  $("#dlg-info").showModal();
}

// ---------------------------------------------------------------- settings dialog
function qualityLabel(q) {
  return q === "best" ? t("q_best") : q === "audio" ? t("q_audio") : q === "m4a" ? t("q_m4a") : q === "2160p" ? t("q_2160p") : q === "4320p" ? t("q_4320p") : q;
}
/** v0.2: "1080p — 60 fps · H.264 · with audio · ≈ 52 MB" from the probe's own details. */
const OPT_WORDS = () => ({
  fps: (n) => t("vd_opt_fps", n), audio: t("vd_opt_audio"), noaudio: t("vd_opt_noaudio"),
  original: t("vd_opt_original"), converted: t("vd_opt_converted"), kbps: (n) => t("vd_opt_kbps", n), size: (s) => t("vd_opt_size", s),
});
function fillQualitySelect(sel, list, pick, result) {
  sel.replaceChildren(...list.map((q) => {
    const d = result ? optionDetails(optionFor(result, q), OPT_WORDS(), size) : [];
    const o = el("option", null, d.length ? `${qualityLabel(q)} — ${d.join(" · ")}` : qualityLabel(q));
    o.value = q;
    return o;
  }));
  sel.value = list.includes(pick) ? pick : list.includes("best") ? "best" : list[0] ?? "";
}
function showSettingsFolder() {
  const f = $("#set-folder");
  f.textContent = settings.folder ?? `${t("folder_default")} — ${defaultDir}`;
  f.title = settings.folder ?? defaultDir;
  $("#set-folder-reset").hidden = !settings.folder;
}
async function showTools(target, button) {
  try {
    const s = await api.call("tools_status");
    target.textContent = s.installed ? t("tools_ok", s.ytdlp_version, s.ffmpeg_version) : t("tools_no");
    if (button) button.hidden = !!s.installed;
    return s;
  } catch {
    target.textContent = t("tools_unknown");
    if (button) button.hidden = true;
    return null;
  }
}
function openSettings() {
  $("#set-missing").value = settings.missing;
  $("#set-lang").value = settings.lang;
  $("#set-theme").value = settings.theme;
  fillQualitySelect($("#set-quality"), QUALITIES, settings.quality);
  showSettingsFolder();
  showTools($("#set-tools"), $("#set-tools-install"));
  api.call("app_info").then((i) => setText($("#about-text"), t("about", i.version, i.commit, i.program))).catch(() => {});
  fillEngineFields();
  fillDesktopFields();
  $("#dlg-settings").showModal();
}
for (const b of $$("#dlg-settings [data-tab]")) {
  b.addEventListener("click", () => {
    for (const x of $$("#dlg-settings [data-tab]")) x.classList.toggle("on", x === b);
    for (const p of $$("#dlg-settings [data-pane]")) p.hidden = p.dataset.pane !== b.dataset.tab;
  });
}
$("#set-lang").addEventListener("change", (e) => {
  setLang(e.target.value);
  openSettingsTexts();
});
function openSettingsTexts() {
  fillQualitySelect($("#set-quality"), QUALITIES, settings.quality);
  $("#set-theme").value = settings.theme;
  showSettingsFolder();
  showTools($("#set-tools"), $("#set-tools-install"));
}
$("#set-theme").addEventListener("change", (e) => { saveSetting("theme", e.target.value); applyTheme(); updateCommands(); });
$("#set-quality").addEventListener("change", (e) => saveSetting("quality", e.target.value));
$("#set-missing").addEventListener("change", (e) => saveSetting("missing", e.target.value));
$("#set-folder-pick").addEventListener("click", async () => {
  try {
    const p = await api.call("pick_folder");
    if (p) { saveSetting("folder", p); showSettingsFolder(); showFolder(); }
  } catch (e) { failure(e); }
});
$("#set-folder-reset").addEventListener("click", () => { saveSetting("folder", null); showSettingsFolder(); showFolder(); });
/**
 * The tools are installed only after the user read the real agreement text (from the backend,
 * in the window's language) and ticked the box: `tools_install` is never called otherwise.
 * Resolves true when the install started.
 */
let consentDone = null;
async function askConsent() {
  const dlg = $("#dlg-consent");
  $("#c-agree").checked = false;
  $("#c-install").disabled = true;
  $("#c-error").hidden = true;
  $("#c-text").textContent = "…";
  dlg.showModal();
  try {
    $("#c-text").textContent = await api.call("tools_consent_text", { lang: settings.lang });
  } catch (e) {
    $("#c-text").textContent = "";
    $("#c-error").hidden = false;
    $("#c-error").textContent = errMessage(e);
  }
  return new Promise((res) => { consentDone = res; });
}
$("#c-agree").addEventListener("change", (e) => {
  $("#c-install").disabled = !e.target.checked || !$("#c-text").textContent || $("#c-text").textContent === "…";
});
$("#c-install").addEventListener("click", async () => {
  if (!$("#c-agree").checked) return;
  $("#c-install").disabled = true;
  try {
    await api.call("tools_install", { accept: true });
    $("#dlg-consent").close();
    consentDone?.(true);
  } catch (e) {
    $("#c-error").hidden = false;
    $("#c-error").textContent = errMessage(e);
    $("#c-install").disabled = false;
  }
});
$("#dlg-consent").addEventListener("close", () => { consentDone?.(false); consentDone = null; });
$("#set-tools-install").addEventListener("click", async () => {
  if (await askConsent()) watchInstall();
});

// ---------------------------------------------------------------- video / playlist dialog
let probe = null; // { url, result }
// v0.2: a link with list= : both probes, and which one the dialog shows
let scope = null; // { ctx: linkPlaylist(), video: {url,result}|null, list: {url,result}|null }
let probeSeq = 0;
let videoFolder = null;
const vStatus = (msg) => { const s = $("#v-status"); s.hidden = !msg; s.textContent = msg ?? ""; };
const vError = (msg) => { const s = $("#v-error"); s.hidden = !msg; s.textContent = msg ?? ""; };

/** v0.2: a link sent by the browser's button: any open dialog gives way to the Video window. */
function openFromBrowser(url) {
  for (const d of document.querySelectorAll("dialog[open]")) d.close();
  openVideo(url);
  toast(t("from_browser"));
}

function openVideo(prefill) {
  probe = null;
  probeSeq++;
  videoFolder = null;
  $("#in-vurl").value = prefill ?? "";
  $("#v-result").hidden = true;
  $("#v-tools").hidden = true;
  $("#v-install").disabled = false;
  vStatus(null); vError(null);
  updateVideoStart();
  $("#dlg-video").showModal();
  $("#in-vurl").focus();
  if (prefill) doProbe();
}

function showVideoFolder() {
  const f = $("#v-folder");
  const eff = videoFolder ?? settings.folder;
  f.textContent = eff ?? `${t("folder_default")} — ${defaultDir}`;
  f.title = eff ?? defaultDir;
}

function fmtDur(s) { return s != null && Number.isFinite(s) ? fmtClock(s) : ""; }

async function doProbe() {
  const url = $("#in-vurl").value.trim();
  vError(null);
  $("#v-tools").hidden = true;
  if (!/^https?:\/\/\S+$/i.test(url)) { vError(errText("bad_url")); return; }
  const seq = ++probeSeq;
  probe = null;
  scope = { ctx: linkPlaylist(url), video: null, list: null };
  $("#v-result").hidden = true;
  updateVideoStart();
  $("#btn-probe").disabled = true;
  vStatus(t("vd_probing"));
  try {
    const r = await api.call("video_probe", { url });
    if (seq !== probeSeq) return;
    probe = { url, result: r };
    if (r.kind === "playlist") scope.list = probe; else scope.video = probe;
    renderProbe();
  } catch (e) {
    if (seq !== probeSeq) return;
    if (isToolsMissing(e)) $("#v-tools").hidden = false;
    else vError(errMessage(String(e?.message ?? e).replace(/^[A-Z_]+:\s*/, "")));
  } finally {
    if (seq === probeSeq) { $("#btn-probe").disabled = false; vStatus(null); }
  }
}

function renderProbe() {
  const r = probe.result;
  $("#v-result").hidden = false;
  // v0.2: a video link that carries a playlist (watch?v=…&list=…): offer both; a bare video
  // link: say how to get a whole playlist (never guessed)
  const ctx = scope?.ctx;
  $("#v-scope").hidden = !(ctx?.playlistUrl && (scope.video || r.kind === "video"));
  if (!$("#v-scope").hidden) $(`#v-scope input[value=${r.kind === "playlist" ? "list" : "video"}]`).checked = true;
  $("#v-list-hint").hidden = !(r.kind === "video" && ctx?.videoUrl && !ctx.playlistUrl);
  $("#v-range").value = "";
  $("#v-range-error").hidden = true;
  setText($("#v-name"), r.title ?? "");
  const meta = [r.kind === "playlist" ? t("vd_kind_playlist") : t("vd_kind_video")];
  if (r.uploader) meta.push(`${t("vd_by")}: ${r.uploader}`);
  if (r.duration != null) meta.push(`${t("vd_duration")}: ${fmtDur(r.duration)}`);
  if (r.kind === "playlist") meta.push(t("vd_items", r.items?.length ?? 0));
  $("#v-meta").textContent = meta.join(" · ");
  fillQualitySelect($("#v-quality"), safeQualities(r.qualities), settings.quality, r.kind === "video" ? r : null);
  $("#v-missing").value = settings.missing;
  showMissingRow();
  const isList = r.kind === "playlist";
  $("#v-playlist").hidden = !isList;
  if (isList) {
    const ul = $("#v-items");
    const width = String(r.items?.length ?? 0).length < 3 ? 3 : String(r.items.length).length;
    ul.replaceChildren(...(r.items ?? []).map((x) => {
      const li = el("li");
      const lab = el("label", "check");
      const cb = el("input");
      cb.type = "checkbox"; cb.checked = true; cb.dataset.index = x.index;
      const title = el("bdi", "v-title", x.title ?? "");
      title.dir = "auto";
      lab.append(cb, el("span", "num", String(x.index).padStart(width, "0")), title, el("span", "dur", fmtDur(x.duration)));
      li.append(lab);
      return li;
    }));
    $("#v-folder-note").textContent = t("vd_folder_note", r.title ?? "");
  }
  showVideoFolder();
  updateVideoStart();
}

function pickedIndices() {
  return [...$$("#v-items input:checked")].map((c) => Number(c.dataset.index));
}
function updateVideoStart() {
  let ok = !!probe && $("#v-quality").value !== "";
  if (ok && probe.result.kind === "playlist") {
    const total = probe.result.items?.length ?? 0;
    const n = pickedIndices().length;
    $("#v-count").textContent = t("vd_count", n, total);
    ok = n > 0;
  }
  $("#v-start").disabled = !ok;
}

// v0.2: "this video only" / "the whole playlist"
$("#v-scope").addEventListener("change", async (e) => {
  if (!scope?.ctx?.playlistUrl) return;
  vError(null);
  if (e.target.value === "video" && scope.video) { probe = scope.video; renderProbe(); return; }
  if (e.target.value === "list") {
    if (scope.list) { probe = scope.list; renderProbe(); return; }
    const seq = ++probeSeq;
    const url = scope.ctx.playlistUrl;
    vStatus(t("vd_probing"));
    $("#v-start").disabled = true;
    try {
      const r = await api.call("video_probe", { url });
      if (seq !== probeSeq) return;
      scope.list = { url, result: r };
      probe = scope.list;
      renderProbe();
    } catch (err) {
      if (seq !== probeSeq) return;
      vError(errMessage(String(err?.message ?? err).replace(/^[A-Z_]+:\s*/, "")));
      $("#v-scope input[value=video]").checked = true;
    } finally {
      if (seq === probeSeq) vStatus(null);
      updateVideoStart();
    }
  }
});
// v0.2: select a range of episodes ("1-10, 15, 20-25")
function applyRange() {
  if (!probe || probe.result.kind !== "playlist") return;
  const boxes = [...document.querySelectorAll("#v-items input")];
  const r = parseRange($("#v-range").value, boxes.map((c) => Number(c.dataset.index)));
  const errEl = $("#v-range-error");
  if (r.error) { errEl.textContent = TEXT[settings.lang].vd_range_err[r.error]; errEl.hidden = false; return; }
  errEl.hidden = true;
  const want = new Set(r.items);
  for (const c of boxes) c.checked = want.has(Number(c.dataset.index));
  updateVideoStart();
}
$("#v-range-apply").addEventListener("click", applyRange);
$("#v-range").addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); applyRange(); } });
$("#btn-probe").addEventListener("click", doProbe);
$("#in-vurl").addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); doProbe(); } });
$("#in-vurl").addEventListener("input", () => {
  if (probe && $("#in-vurl").value.trim() !== probe.url) { probe = null; $("#v-result").hidden = true; updateVideoStart(); }
});
$("#v-items").addEventListener("change", updateVideoStart);
$("#v-quality").addEventListener("change", updateVideoStart);
$("#v-all").addEventListener("click", () => { for (const c of $$("#v-items input")) c.checked = true; updateVideoStart(); });
$("#v-none").addEventListener("click", () => { for (const c of $$("#v-items input")) c.checked = false; updateVideoStart(); });
$("#v-pick").addEventListener("click", async () => {
  try { const p = await api.call("pick_folder"); if (p) { videoFolder = p; showVideoFolder(); } } catch (e) { failure(e); }
});
$("#v-install").addEventListener("click", async () => {
  if (await askConsent()) watchInstall();
});
$("#v-cancel").addEventListener("click", () => { probeSeq++; $("#dlg-video").close(); });
$("#form-video").addEventListener("submit", async (e) => {
  e.preventDefault();
  if (!probe) return;
  const quality = $("#v-quality").value;
  if (!safeQualities([quality]).length) { vError(errText("bad_quality")); return; }
  const dir = videoFolder ?? settings.folder ?? null;
  $("#v-start").disabled = true;
  try {
    if (probe.result.kind === "playlist") {
      const idx = pickedIndices();
      if (!idx.length) { vError(t("vd_none_selected")); return; }
      const all = idx.length === (probe.result.items?.length ?? 0);
      // a playlist offers every fixed height: some items may not have the chosen one
      const if_missing = /^\d+p$/.test(quality) ? $("#v-missing").value : undefined;
      await api.call("playlist_add", all ? { url: probe.url, quality, dir, if_missing } : { url: probe.url, quality, items: idx, dir, if_missing });
    } else {
      // v0.2: the exact formats the line showed (the quality alone if the probe gave none)
      const format = optionFor(probe.result, quality)?.format ?? undefined;
      await api.call("video_add", format ? { url: probe.url, quality, dir, format } : { url: probe.url, quality, dir });
    }
    $("#dlg-video").close();
    toast(t("video_added"));
    jobsAvailable = true;
    setView("jobs");
  } catch (err) {
    vError(errMessage(err));
  } finally {
    updateVideoStart();
  }
});

// ---------------------------------------------------------------- video jobs view
const jobEls = new Map(); // job -> { li, parts, itemsOpen, itemEls }

async function refreshJobs() {
  if (!jobsAvailable && Date.now() < jobsRetryAt) return;
  try {
    const r = await api.call("video_jobs");
    jobs = Array.isArray(r) ? r : [];
    jobsAvailable = true;
  } catch {
    jobsAvailable = false;
    jobsRetryAt = Date.now() + 30000;
    jobs = [];
  }
  invalidate(true);
}

function jobButton(label, cmd, job) {
  const b = el("button", "small", label);
  b.type = "button";
  b.addEventListener("click", async () => {
    b.disabled = true;
    try { await api.call(cmd, { job }); } catch (e) { failure(e); }
    await refreshJobs();
    b.disabled = false;
  });
  return b;
}

function makeJob(j) {
  const li = el("li", "job");
  const title = el("bdi", "j-title");
  title.dir = "auto";
  const meta = el("div", "j-meta");
  const bar = el("div", "pbar");
  const fill = el("i");
  bar.append(fill);
  const nums = el("div", "j-nums");
  const ops = el("div", "j-ops");
  const items = el("ul", "j-items");
  items.hidden = true;
  li.append(title, ops, meta, bar, nums, items);
  const x = { li, title, meta, bar, fill, nums, ops, items, opsKey: "", itemEls: [] };
  jobEls.set(j.job, x);
  return x;
}

function renderJobs() {
  const ul = $("#jobs");
  const empty = $("#jobs-empty");
  empty.hidden = jobs.length > 0;
  if (!jobs.length) setText(empty, jobsAvailable ? t("jobs_empty") : t("jobs_unavailable"));
  const keep = new Set(jobs.map((j) => j.job));
  for (const [k, x] of jobEls) if (!keep.has(k)) { x.li.remove(); jobEls.delete(k); }
  jobs.forEach((j, i) => {
    const x = jobEls.get(j.job) ?? makeJob(j);
    if (ul.children[i] !== x.li) ul.insertBefore(x.li, ul.children[i] ?? null);
    x.li.className = `job js-${j.state}`;
    setText(x.title, j.title ?? "");
    const kind = j.kind === "playlist" ? t("vd_kind_playlist") : t("vd_kind_video");
    setText(x.meta, `${kind} · ${t(`js_${j.state}`)} · ${t("j_quality", qualityLabel(j.quality), j.got_height)} · ${t("j_items", j.done_items ?? 0, j.total_items ?? 0, j.failed_items ?? 0)}`);
    const f = jobFraction(j);
    x.fill.style.transform = `scaleX(${f})`;
    const bytes = j.total_bytes ? `${size(j.bytes ?? 0)} ${t("of")} ${size(j.total_bytes)}` : j.bytes ? size(j.bytes) : "";
    const speed = j.state === "running" && j.speed ? `${size(j.speed)}${t("unit_s")}` : "";
    setText(x.nums, [`${(f * 100).toFixed(1)}%`, bytes, speed].filter(Boolean).join(" · "));
    const opsKey = `${j.state}|${j.failed_items > 0}|${!!j.items?.length}|${settings.lang}`;
    if (x.opsKey !== opsKey) {
      x.opsKey = opsKey;
      const ops = [];
      if (j.state === "running" || j.state === "queued") ops.push(jobButton(t("c_pause"), "video_pause", j.job));
      if (j.state === "paused") ops.push(jobButton(t("c_resume"), "video_resume", j.job));
      if (j.failed_items > 0 || j.state === "failed") ops.push(jobButton(t("j_retry"), "video_retry", j.job));
      if (j.items?.length) {
        const b = el("button", "small ghost", t("j_show_items"));
        b.type = "button";
        b.addEventListener("click", () => { x.items.hidden = !x.items.hidden; renderJobs(); });
        ops.push(b);
      }
      if (j.state !== "running") ops.push(jobButton(t("j_remove"), "video_remove", j.job));
      x.ops.replaceChildren(...ops);
    }
    if (!x.items.hidden && j.items) renderJobItems(x, j.items);
  });
}

function renderJobItems(x, items) {
  while (x.itemEls.length < items.length) {
    const li = el("li", "j-item");
    const num = el("span", "num");
    const title = el("bdi", "v-title");
    title.dir = "auto";
    const bar = el("div", "pbar");
    const fill = el("i");
    bar.append(fill);
    const st = el("span", "j-st");
    li.append(num, title, bar, st);
    x.items.append(li);
    x.itemEls.push({ li, num, title, fill, st });
  }
  while (x.itemEls.length > items.length) x.itemEls.pop().li.remove();
  items.forEach((it, i) => {
    const e = x.itemEls[i];
    setText(e.num, String(it.index).padStart(3, "0"));
    setText(e.title, it.title ?? "");
    const p = it.state === "done" ? 1 : Math.min(1, Math.max(0, it.progress ?? 0));
    e.fill.style.transform = `scaleX(${p})`;
    const label = `${t(`js_${it.state}`)}${it.got_height ? ` · ${it.got_height}p` : ""}`;
    setText(e.st, it.state === "running" ? `${(p * 100).toFixed(0)}%` : label);
    setAttr(e.st, "title", it.error ?? null);
    const cls = `j-item js-${it.state}`;
    if (e.li.className !== cls) e.li.className = cls;
  });
}

// ---------------------------------------------------------------- start
applyTheme();
applySidebar();
applyLang();
// ---------------------------------------------------------------- open file, engine settings, scheduler
async function openFile(id) {
  try { await api.call("open_file", { id }); } catch (e) { failure(e); }
}

let engine = null; // last settings read from / saved to the app
async function loadEngine() {
  try { engine = await api.call("settings_get"); } catch (e) { failure(e); return null; }
  return engine;
}
async function saveEngine(next, errBox) {
  errBox.hidden = true;
  try {
    engine = await api.call("settings_set", { settings: next });
    toast(t("saved"));
    return true;
  } catch (e) {
    errBox.textContent = errMessage(e);
    errBox.hidden = false;
    return false;
  }
}
async function fillEngineFields() {
  const s = await loadEngine();
  if (!s) return;
  $("#set-parallel").value = s.max_parallel;
  $("#set-conns").value = s.connections;
  $("#set-limit").value = s.speed_limit_kib;
  $("#set-engine-error").hidden = true;
}
function intIn(input) {
  const v = Number(input.value);
  const min = Number(input.min), max = Number(input.max);
  return Number.isInteger(v) && v >= min && v <= max ? v : null;
}
$("#set-engine-save").addEventListener("click", async () => {
  const errBox = $("#set-engine-error");
  const p = intIn($("#set-parallel")), c = intIn($("#set-conns")), l = intIn($("#set-limit"));
  for (const [v, inp] of [[p, "#set-parallel"], [c, "#set-conns"], [l, "#set-limit"]]) {
    if (v === null) {
      errBox.textContent = `${$(`label[for="${inp.slice(1)}"]`).textContent}: ${$(inp).min}–${$(inp).max}`;
      errBox.hidden = false;
      $(inp).focus();
      return;
    }
  }
  const base = engine ?? (await loadEngine());
  if (!base) return;
  await saveEngine({ ...base, max_parallel: p, connections: c, speed_limit_kib: l }, errBox);
});

const hhmm = (m) => `${String(Math.floor(m / 60)).padStart(2, "0")}:${String(m % 60).padStart(2, "0")}`;
const mins = (v) => { const [h, m] = String(v).split(":").map(Number); return h * 60 + m; };
function schedDays() {
  const box = $("#sch-days");
  if (box.childElementCount) return box;
  t("sch_days").forEach((name, i) => {
    const l = el("label", "check");
    const c = el("input");
    c.type = "checkbox";
    c.dataset.bit = String(i);
    l.append(c, el("span", null, name));
    box.append(l);
  });
  return box;
}
function schedToggle() { $("#sch-body").classList.toggle("off", !$("#sch-on").checked); }
async function openScheduler() {
  const s = await loadEngine();
  if (!s) return;
  const box = schedDays();
  // day names follow the language at the moment the dialog opens
  $$("#sch-days span").forEach((n, i) => (n.textContent = t("sch_days")[i]));
  const w = s.schedule;
  $("#sch-on").checked = !!w;
  $("#sch-start").value = hhmm(w ? w.start_min : 120);
  $("#sch-end").value = hhmm(w ? w.end_min : 480);
  for (const c of box.querySelectorAll("input")) c.checked = w ? !!(w.weekdays & (1 << Number(c.dataset.bit))) : true;
  $("#sch-error").hidden = true;
  schedToggle();
  $("#dlg-sched").showModal();
}
$("#sch-on").addEventListener("change", schedToggle);
$("#sch-save").addEventListener("click", async () => {
  const errBox = $("#sch-error");
  const base = engine ?? (await loadEngine());
  if (!base) return;
  let schedule = null;
  if ($("#sch-on").checked) {
    let weekdays = 0;
    for (const c of $$("#sch-days input")) if (c.checked) weekdays |= 1 << Number(c.dataset.bit);
    const start_min = mins($("#sch-start").value), end_min = mins($("#sch-end").value);
    const bad = !weekdays ? t("sch_need_day") : !(start_min >= 0 && end_min >= 0) ? t("sch_same") : start_min === end_min ? t("sch_same") : null;
    if (bad) { errBox.textContent = bad; errBox.hidden = false; return; }
    schedule = { start_min, end_min, weekdays };
  }
  if (await saveEngine({ ...base, schedule }, errBox)) $("#dlg-sched").close();
});

if (usingMock) window.__eid = { stats, store, refresh, poll, invalidate, viewport, select: (ids) => { selected = new Set(ids); invalidate(true); updateCommands(); } };
await poll();


// ---------------------------------------------------------------- v0.2: notices
function notify(list) {
  const done = list.filter((x) => x.status === "done"), failed = list.filter((x) => x.status === "failed");
  if (list.length === 1) toast(done.length ? t("n_done", list[0].name) : t("n_failed", list[0].name));
  else toast(t("n_many", done.length, failed.length));
}

// ---------------------------------------------------------------- v0.2: search
let searchTimer;
$("#search").addEventListener("input", () => {
  clearTimeout(searchTimer);
  searchTimer = setTimeout(() => { query = $("#search").value; invalidate(true); }, 120);
});
$("#search").addEventListener("keydown", (e) => {
  if (e.key === "Escape") { e.preventDefault(); $("#search").value = ""; query = ""; invalidate(true); viewport.focus(); }
  else if (e.key === "Enter" || e.key === "ArrowDown") { e.preventDefault(); viewport.focus(); }
});

// ---------------------------------------------------------------- v0.2: columns (show/hide, order, width)
const COL_LABEL = { name: "col_name", size: "col_size", prog: "col_progress", status: "col_status", speed: "col_speed", eta: "col_eta", url: "col_url" };
let cols = normalizeCols(readJson("cols"));
function readJson(k) { try { return JSON.parse(localStorage.getItem(k) ?? "null"); } catch { return null; } }
const colStyle = document.head.appendChild(el("style"));
function applyCols() {
  const rules = cols.map((c, i) => `.thead > .c-${c.key}, .tr > .c-${c.key} { order: ${i};${c.on ? "" : " display: none;"} }`);
  colStyle.textContent = `${rules.join("\n")}\n.thead, .tr { grid-template-columns: ${gridTemplate(cols)}; }`;
}
function saveCols() { localStorage.setItem("cols", JSON.stringify(cols)); applyCols(); }
applyCols();

function showColsMenu(x, y) {
  const m = $("#menu-cols");
  const items = cols.map((c) => {
    const b = el("button");
    b.type = "button";
    b.setAttribute("role", "menuitemcheckbox");
    b.setAttribute("aria-checked", c.on ? "true" : "false");
    b.disabled = c.key === "name";
    b.append(el("span", "chk", c.on ? "✓" : ""), el("span", null, t(COL_LABEL[c.key])));
    b.addEventListener("click", () => { c.on = !c.on; saveCols(); closeMenus(); });
    return b;
  });
  const reset = el("button", null, t("cols_reset"));
  reset.type = "button";
  reset.setAttribute("role", "menuitem");
  reset.addEventListener("click", () => { cols = normalizeCols(null); saveCols(); closeMenus(); });
  const hint = el("p", "note", t("cols_hint"));
  m.replaceChildren(...items, el("hr"), reset, hint);
  showMenu(m, x, y);
}
$("#thead").addEventListener("contextmenu", (e) => { e.preventDefault(); showColsMenu(e.clientX, e.clientY); });

const COL_DND = "application/x-eid-column";
for (const b of $$("#thead > button")) {
  const key = [...b.classList].find((c) => c.startsWith("c-"))?.slice(2);
  if (!key) continue;
  b.dataset.col = key;
  b.draggable = true;
  b.addEventListener("dragstart", (e) => { e.dataTransfer.setData(COL_DND, key); e.dataTransfer.effectAllowed = "move"; });
  b.addEventListener("dragover", (e) => { if (e.dataTransfer.types.includes(COL_DND)) { e.preventDefault(); b.classList.add("dropto"); } });
  b.addEventListener("dragleave", () => b.classList.remove("dropto"));
  b.addEventListener("drop", (e) => {
    b.classList.remove("dropto");
    const from = e.dataTransfer.getData(COL_DND);
    if (!from) return;
    e.preventDefault();
    e.stopPropagation();
    cols = moveCol(cols, from, key);
    saveCols();
  });
  const h = el("span", "rz");
  h.setAttribute("aria-hidden", "true");
  b.append(h);
  h.addEventListener("click", (e) => e.stopPropagation());
  h.addEventListener("pointerdown", (e) => {
    e.preventDefault();
    e.stopPropagation();
    const c = cols.find((x) => x.key === key);
    const x0 = e.clientX, w0 = b.getBoundingClientRect().width;
    const sign = document.documentElement.dir === "rtl" ? -1 : 1;
    h.setPointerCapture(e.pointerId);
    const move = (ev) => { c.w = Math.round(w0 + sign * (ev.clientX - x0)); cols = normalizeCols(cols); applyCols(); };
    const up = () => { h.removeEventListener("pointermove", move); saveCols(); };
    h.addEventListener("pointermove", move);
    h.addEventListener("pointerup", up, { once: true });
    h.addEventListener("pointercancel", up, { once: true });
  });
  h.addEventListener("dblclick", (e) => { e.stopPropagation(); const c = cols.find((x) => x.key === key); c.w = null; saveCols(); });
}

// ---------------------------------------------------------------- v0.2: toolbar customization
let toolbar = normalizeToolbar(readJson("toolbar"));
function applyToolbar() {
  for (const b of $$("#toolbar .tool[data-cmd]")) b.hidden = toolbar.hidden.has(b.dataset.cmd);
  $("#toolbar").classList.toggle("icons-only", !toolbar.labels);
  // a separator with no visible button on one side is hidden too
  const kids = [...$("#toolbar").children];
  for (const [i, k] of kids.entries()) {
    if (!k.classList.contains("sep")) continue;
    const prev = kids.slice(0, i).reverse().find((x) => !x.classList.contains("sep"));
    const next = kids.slice(i + 1).find((x) => !x.classList.contains("sep"));
    k.hidden = !(prev && !prev.hidden && prev.classList.contains("tool")) || !(next && !next.hidden && next.classList.contains("tool"));
  }
}
function saveToolbar() { localStorage.setItem("toolbar", JSON.stringify({ hidden: [...toolbar.hidden], labels: toolbar.labels })); applyToolbar(); }
function openToolbarDialog() {
  const list = $("#tb-list");
  list.replaceChildren(...TOOLS.map((k) => {
    const src = $(`#toolbar .tool[data-cmd="${k}"] span`);
    const lab = el("label", "check");
    const cb = el("input");
    cb.type = "checkbox";
    cb.checked = !toolbar.hidden.has(k);
    cb.addEventListener("change", () => { if (cb.checked) toolbar.hidden.delete(k); else toolbar.hidden.add(k); saveToolbar(); });
    lab.append(cb, el("span", null, src?.textContent ?? k));
    return lab;
  }));
  $("#tb-labels").checked = toolbar.labels;
  $("#dlg-toolbar").showModal();
}
$("#tb-labels").addEventListener("change", (e) => { toolbar.labels = e.target.checked; saveToolbar(); });
$("#tb-reset").addEventListener("click", () => { toolbar = normalizeToolbar(null); saveToolbar(); openToolbarDialog(); });
$("#toolbar").addEventListener("contextmenu", (e) => { e.preventDefault(); openToolbarDialog(); });
applyToolbar();

// ---------------------------------------------------------------- v0.2: notify setting
$("#set-notify").checked = settings.notify;
$("#set-notify").addEventListener("change", (e) => { settings.notify = e.target.checked; saveSetting("notify", e.target.checked); });

// ---------------------------------------------------------------- v0.2: drag and drop of links
let dragDepth = 0;
const isLinkDrag = (e) => {
  const ty = [...(e.dataTransfer?.types ?? [])];
  return !ty.includes(COL_DND) && (ty.includes("text/uri-list") || ty.includes("text/plain") || ty.includes("Files"));
};
document.addEventListener("dragenter", (e) => {
  if (!isLinkDrag(e) || anyDialog()) return;
  e.preventDefault();
  dragDepth++;
  $("#drop").hidden = false;
});
document.addEventListener("dragover", (e) => { if (isLinkDrag(e) && !anyDialog()) { e.preventDefault(); e.dataTransfer.dropEffect = "copy"; } });
document.addEventListener("dragleave", () => { dragDepth = Math.max(0, dragDepth - 1); if (!dragDepth) $("#drop").hidden = true; });
document.addEventListener("drop", (e) => {
  if (!isLinkDrag(e) || anyDialog()) return;
  e.preventDefault();
  dragDepth = 0;
  $("#drop").hidden = true;
  const dt = e.dataTransfer;
  const links = linksFromText(dt.getData("text/uri-list") || dt.getData("text/plain"));
  if (!links.length) { toast(dt.types.includes("Files") ? t("drop_files") : t("drop_none")); return; }
  if (links.length === 1 && videoPage(links[0])) openVideo(links[0]);
  else openAdd(links.join("\n"));
});
// tooltips carry the button text (needed when only icons are shown), in the current language
$("#toolbar").addEventListener("mouseover", (e) => {
  const b = e.target.closest?.(".tool");
  if (b) setAttr(b, "title", b.querySelector("span")?.textContent ?? null);
});

// v0.2: "if this quality is missing" is asked for playlists and a fixed height only (a single
// video offers only the qualities it really has)
function showMissingRow() {
  const q = $("#v-quality").value;
  $("#v-missing-row").hidden = !(probe?.result?.kind === "playlist" && /^\d+p$/.test(q));
}
$("#v-quality").addEventListener("change", showMissingRow);

// ---------------------------------------------------------------- v0.2: tray, close to tray, clipboard
function sendTrayTexts() {
  api.call("tray_texts", { texts: { show: t("tray_show"), pause_all: t("c_pause_all"), resume_all: t("c_resume_all"), quit: t("tray_quit") } })
    .catch(() => {}); // an older backend without a tray: nothing to name
}
api.on?.("tray", (action) => { if (action === "pause_all" || action === "resume_all") run(action); });

// v0.2: browser extension connection (Settings > Desktop)
// v0.2: Brave and Opera on Windows read Chrome's registration (ci-windows, browsers-more)
const BR_NAME = { chrome: "Chrome · Brave · Opera", edge: "Edge", firefox: "Firefox" };
function showBrowsers(list) {
  const ul = $("#browser-list");
  ul.replaceChildren(...(Array.isArray(list) ? list : []).filter((s) => Object.hasOwn(BR_NAME, s?.browser)).map((s) => {
    const li = document.createElement("li");
    const st = ["this", "other", "none", "error"].includes(s.state) ? s.state : "error";
    li.className = st;
    li.dataset.browser = s.browser;
    li.dataset.state = st;
    li.textContent = t(`br_${st}`, BR_NAME[s.browser], String(s.host ?? ""));
    return li;
  }));
}
async function browserCall(cmd) {
  const err = $("#browser-error");
  err.hidden = true;
  try {
    const r = await api.call(cmd);
    showBrowsers(r);
    return true;
  } catch (e) {
    const k = String(e);
    err.textContent = k === "from_zip" ? t("chk_from_zip") : k === "no_native_host" ? t("chk_no_native_host") : errMessage(e);
    err.hidden = false;
    return false;
  }
}
$("#browser-connect").addEventListener("click", async () => { if (await browserCall("browser_connect")) toast(t("br_done")); });
$("#browser-disconnect").addEventListener("click", () => browserCall("browser_disconnect"));

async function fillDesktopFields() {
  browserCall("browser_status");
  const s = await loadEngine();
  if (!s) return;
  $("#set-tray").checked = s.tray !== false;
  $("#set-close-tray").checked = !!s.close_to_tray;
  $("#set-close-tray").disabled = s.tray === false;
  $("#set-clipboard").checked = !!s.clipboard;
  $("#set-power").checked = !!s.power_actions;
  $("#set-desktop-error").hidden = true;
}
for (const id of ["#set-tray", "#set-close-tray", "#set-clipboard", "#set-power"]) {
  $(id).addEventListener("change", async () => {
    const base = engine ?? (await loadEngine());
    if (!base) return;
    const tray = $("#set-tray").checked;
    const ok = await saveEngine({ ...base, tray, close_to_tray: tray && $("#set-close-tray").checked, clipboard: $("#set-clipboard").checked, power_actions: $("#set-power").checked }, $("#set-desktop-error"));
    if (!ok) await fillDesktopFields();
    $("#set-close-tray").disabled = !tray;
  });
}

let clipLink = null;
api.on?.("clipboard-link", (p) => {
  const url = typeof p?.url === "string" && p.url.length <= 2048 && /^https?:\/\//i.test(p.url) ? p.url : null;
  if (!url) return;
  clipLink = { url, kind: p.kind === "video" ? "video" : "file" };
  setText($("#clip-url"), url);
  setAttr($("#clip-url"), "title", url);
  $("#clipbar").hidden = false;
});
$("#clip-ignore").addEventListener("click", () => { $("#clipbar").hidden = true; clipLink = null; });
$("#clip-add").addEventListener("click", () => {
  if (!clipLink) return;
  const { url, kind } = clipLink;
  $("#clipbar").hidden = true;
  clipLink = null;
  if (kind === "video") openVideo(url); else openAdd(url);
});
sendTrayTexts();

// ---------------------------------------------------------------- v0.2: startup self-check
const CHECK_KEYS = { from_zip: "chk_from_zip", no_native_host: "chk_no_native_host", last_crash: "chk_last_crash" };
let checkCodes = []; // [{ code, path }]
function renderCheck() {
  const ul = $("#check-list");
  ul.replaceChildren(...checkCodes.map((c) => { const li = document.createElement("li"); li.dataset.code = c.code; li.textContent = t(CHECK_KEYS[c.code], String(c.path ?? "")); return li; }));
  $("#check-logs").hidden = !checkCodes.some((c) => c.code === "last_crash");
  $("#checkbar").hidden = checkCodes.length === 0;
}
(async () => {
  try {
    const r = await api.call("self_check");
    checkCodes = (Array.isArray(r) ? r : [])
      .map((c) => (typeof c === "string" ? { code: c } : c))
      .filter((c) => c && Object.hasOwn(CHECK_KEYS, c.code));
  } catch { checkCodes = []; }
  renderCheck();
})();
$("#check-logs").addEventListener("click", () => api.call("open_logs").catch(failure));
$("#about-logs").addEventListener("click", () => api.call("open_logs").catch(failure));

// v0.2: follow a started tool install to its end and SAY how it ended (before, a failure was
// never shown: the window only said "started")
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let installWatch = null;
function watchInstall() {
  if (installWatch) return installWatch;
  const show = (msg, isErr) => {
    if ($("#dlg-video").open) { if (isErr) { vStatus(null); vError(msg); } else { vError(null); vStatus(msg); } }
    const se = $("#set-tools-error");
    se.hidden = !isErr; se.textContent = isErr ? msg : "";
  };
  show(t("tools_installing"), false);
  installWatch = (async () => {
    const t0 = Date.now();
    let s = null;
    while (Date.now() - t0 < 60 * 60 * 1000) {
      await sleep(1000);
      try { s = await api.call("tools_status"); } catch { s = null; continue; }
      if (s && !s.installing) break;
    }
    installWatch = null;
    if (s?.installed && !s.error) {
      show(t("tools_done"), false);
      toast(t("tools_done"));
      if ($("#dlg-settings").open) showTools($("#set-tools"), $("#set-tools-install"));
      // the link typed before the install: check it again now
      if ($("#dlg-video").open && $("#in-vurl").value.trim()) doProbe();
    } else {
      const msg = t("tools_failed", errMessage(s?.error ?? "unknown"));
      show(msg, true);
      toast(msg);
    }
  })();
  return installWatch;
}
$("#check-close").addEventListener("click", () => { checkCodes = []; renderCheck(); });

// ---------------------------------------------------------------- v0.2: per-download speed limit
$("#p-limit-set").addEventListener("click", async () => {
  const id = propsId;
  const v = Number($("#p-limit").value);
  if (!Number.isInteger(v) || v < 0 || v > 12500000) { toast(`${t("p_limit")} 0–12500000`); return; }
  try { await api.call("item_limit", { id, limit_kib: v === 0 ? null : v }); toast(t("saved")); kick(); } catch (e) { failure(e); }
});

// ---------------------------------------------------------------- v0.2: "when finished" countdown
const AD_KEY = { none: "ad_none", exit: "ad_exit", sleep: "ad_sleep", hibernate: "ad_hibernate", shutdown: "ad_shutdown" };
function applyPower(p) {
  const c = Array.isArray(p?.countdown) && Number.isFinite(p.countdown[0]) && AD_KEY[p.countdown[1]] ? p.countdown : null;
  powerActive = !!c;
  const bar = $("#powerbar");
  if (c) {
    setText($("#power-text"), t("pw_count", t(AD_KEY[c[1]]), Math.max(0, Math.round(c[0]))));
    if (bar.hidden) { bar.hidden = false; $("#power-cancel").focus(); }
  } else if (!bar.hidden) bar.hidden = true;
  const last = typeof p?.last === "string" ? p.last : null;
  if (last === "cancelled") toast(t("pw_cancelled"));
  else if (last === "aborted") toast(t("pw_aborted"));
  else if (last?.startsWith("failed:") && AD_KEY[last.slice(7)]) toast(t("pw_failed", t(AD_KEY[last.slice(7)])));
}
$("#power-cancel").addEventListener("click", async () => {
  try { await api.call("cancel_countdown"); } catch (e) { failure(e); }
  $("#powerbar").hidden = true;
  powerActive = false;
  kick();
});

// ---------------------------------------------------------------- v0.2: queues dialog
const queueLabel = (q) => (q.name === "main" ? t("q_main") : q.dir.split(/[\\/]/).filter(Boolean).pop() || q.name);
async function openQueues() {
  await fillQueues();
  $("#dlg-queues").showModal();
}
async function fillQueues() {
  let list;
  try { list = await api.call("queues"); } catch (e) { failure(e); return; }
  const machineOk = !!(engine ?? (await loadEngine()))?.power_actions;
  $("#q-error").hidden = true;
  const tpl = $("#q-row");
  $("#q-list").replaceChildren(...list.map((q) => {
    const row = tpl.content.firstElementChild.cloneNode(true);
    for (const n of row.querySelectorAll("[data-t]")) n.textContent = t(n.dataset.t);
    row.dataset.queue = q.name;
    row.querySelector(".q-name").textContent = `${queueLabel(q)}${q.paused ? ` ${t("q_stopped")}` : ""}`;
    row.querySelector(".q-dir").textContent = q.dir;
    row.querySelector(".q-counts").textContent = t("q_counts", q.total, q.done, q.running, q.failed);
    row.querySelector(".q-par").value = q.max_parallel;
    // owner decision: computer actions only when allowed in Settings (off by default)
    for (const o of row.querySelectorAll(".q-after option")) {
      if (["sleep", "hibernate", "shutdown"].includes(o.value) && !machineOk) { o.disabled = true; o.textContent += ` ${t("q_power_off")}`; }
    }
    row.querySelector(".q-after").value = AD_KEY[q.after_done] ? q.after_done : "none";
    const tg = row.querySelector(".q-toggle");
    tg.textContent = q.paused ? t("q_start") : t("q_stop");
    const send = async (patch) => {
      try { await api.call("queue_set", { patch: { name: q.name, ...patch } }); await fillQueues(); kick(); }
      catch (e) { $("#q-error").textContent = errMessage(e); $("#q-error").hidden = false; }
    };
    row.querySelector(".q-save").addEventListener("click", () => {
      const n = Number(row.querySelector(".q-par").value);
      if (!Number.isInteger(n) || n < 1 || n > 8) { $("#q-error").textContent = `${t("s_parallel")}: 1–8`; $("#q-error").hidden = false; return; }
      send({ max_parallel: n, after_done: row.querySelector(".q-after").value });
    });
    tg.addEventListener("click", () => send({ paused: !q.paused }));
    return row;
  }));
}

// v0.2: default "folder by type" for new downloads
$("#set-autocat").checked = localStorage.getItem("autocat") === "on";
$("#set-autocat").addEventListener("change", (e) => localStorage.setItem("autocat", e.target.checked ? "on" : "off"));
