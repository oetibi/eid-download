// Eid Download window. Talks to the Rust side through Tauri `invoke`; outside Tauri (a plain
// browser, for layout checks) it uses an in-memory mock. Untrusted text (file names, links,
// error details) is only ever set through textContent, never as HTML.
import { TEXT } from "./i18n.js";

const tauri = window.__TAURI__;
const api = tauri
  ? { call: (cmd, args) => tauri.core.invoke(cmd, args) }
  : (await import("./mock.js")).mockApi();

const $ = (s) => document.querySelector(s);
let lang = localStorage.getItem("lang") === "en" ? "en" : "ar";
let filter = "all";
let items = [];
let defaultDir = "";
let pickedFolder = null;
const rows = new Map();

const t = (k) => TEXT[lang][k] ?? k;
const errText = (k) => TEXT[lang].err[k] ?? null;

function applyLang() {
  document.documentElement.lang = lang;
  document.documentElement.dir = lang === "ar" ? "rtl" : "ltr";
  document.title = t("app_title");
  for (const el of document.querySelectorAll("[data-t]")) el.textContent = t(el.dataset.t);
  $("#in-links").placeholder = t("links_ph");
  showFolder();
  rows.forEach((r) => r.remove());
  rows.clear();
  render();
}

function size(n) {
  if (n == null) return "?";
  const u = [t("unit_b"), t("unit_kb"), t("unit_mb"), t("unit_gb")];
  let i = 0, v = n;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return `${v.toFixed(i === 0 ? 0 : v < 10 ? 2 : 1)} ${u[i]}`;
}
function duration(s) {
  if (s == null) return "";
  if (s < 60) return `${s} ${t("t_s")}`;
  if (s < 3600) return `${Math.floor(s / 60)} ${t("t_m")} ${s % 60} ${t("t_s")}`;
  return `${Math.floor(s / 3600)} ${t("t_h")} ${Math.floor((s % 3600) / 60)} ${t("t_m")}`;
}

let toastTimer;
function toast(msg) {
  const el = $("#toast");
  el.textContent = msg;
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (el.hidden = true), 4500);
}
function failure(e) {
  const raw = String(e?.message ?? e);
  const key = raw.replace(/^invalid input: /, "").replace(/ /g, "_");
  toast(errText(key) ?? errText(raw) ?? raw);
}

const ACTIVE = new Set(["pending", "waiting", "running", "paused"]);
function matches(it) {
  if (filter === "active") return ACTIVE.has(it.status);
  if (filter === "done") return it.status === "done";
  if (filter === "failed") return it.status === "failed";
  return true;
}

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
}

function makeRow(id) {
  const li = el("li", "item");
  li.dataset.id = id;
  const name = el("bdi", "name");
  const ops = el("div", "ops");
  const meta = el("div", "meta");
  const bar = el("div", "progress");
  bar.append(el("i"));
  const err = el("div", "err");
  li.append(name, ops, meta, bar, err);
  return li;
}

function button(label, action, id) {
  const b = el("button", null, label);
  b.type = "button";
  b.addEventListener("click", async () => {
    b.disabled = true;
    try {
      await action(id);
      await refresh();
    } catch (e) {
      failure(e);
    } finally {
      b.disabled = false;
    }
  });
  return b;
}

function opsFor(it) {
  const out = [];
  if (["running", "pending", "waiting"].includes(it.status)) out.push(button(t("pause"), (id) => api.call("pause", { id }), it.id));
  if (it.status === "paused") out.push(button(t("resume"), (id) => api.call("resume", { id }), it.id));
  if (it.status === "failed") out.push(button(t("retry"), (id) => api.call("resume", { id }), it.id));
  if (it.status === "done") out.push(button(t("reveal"), (id) => api.call("reveal", { id }), it.id));
  if (it.status !== "running") {
    out.push(button(t("remove"), async (id) => {
      await api.call("remove", { id });
      toast(t("removed"));
    }, it.id));
  }
  return out;
}

function fill(li, it) {
  li.className = `item s-${it.status}`;
  li.querySelector(".name").textContent = it.name;
  li.querySelector(".name").title = it.name;
  // operations change only with the status (keeps focus and clicks stable while polling)
  const ops = li.querySelector(".ops");
  if (ops.dataset.status !== it.status) {
    ops.dataset.status = it.status;
    ops.replaceChildren(...opsFor(it));
  }
  const meta = li.querySelector(".meta");
  const parts = [el("span", "badge", t(`st_${it.status}`))];
  const r = it.rate;
  if (r) {
    parts.push(el("span", null, r.total != null ? `${size(r.done)} ${t("of")} ${size(r.total)}` : `${size(r.done)} · ${t("unknown_size")}`));
    if (it.status === "running" && r.speed > 0) parts.push(el("span", null, `${size(r.speed)}${t("unit_s")}`));
    if (it.status === "running" && r.eta != null) parts.push(el("span", null, `${duration(r.eta)} ${t("left")}`));
  }
  if (it.attempts > 1 && it.status !== "done") parts.push(el("span", null, `${t("attempts")} ${it.attempts}`));
  const url = el("bdi", "url", it.url);
  url.dir = "ltr";
  url.title = `${it.url}\n${it.folder}`;
  parts.push(url);
  meta.replaceChildren(...parts);

  const bar = li.querySelector(".progress");
  const fillEl = bar.firstChild;
  const pct = r && r.total ? Math.min(100, (r.done / r.total) * 100) : it.status === "done" ? 100 : 0;
  bar.classList.toggle("indet", it.status === "running" && !(r && r.total));
  fillEl.style.width = `${pct.toFixed(1)}%`;
  bar.title = r && r.total ? `${pct.toFixed(1)}%` : "";

  const err = li.querySelector(".err");
  if (it.error_kind && it.status !== "done") {
    err.hidden = false;
    const small = el("small", null, `${t("details")}: ${it.error}`);
    small.dir = "auto";
    err.replaceChildren(document.createTextNode(errText(it.error_kind) ?? it.error), small);
  } else {
    err.hidden = true;
    err.replaceChildren();
  }
}

function render() {
  const counts = { all: items.length, active: 0, done: 0, failed: 0 };
  for (const it of items) {
    if (ACTIVE.has(it.status)) counts.active++;
    if (it.status === "done") counts.done++;
    if (it.status === "failed") counts.failed++;
  }
  for (const [k, v] of Object.entries(counts)) $(`[data-count="${k}"]`).textContent = v;
  const list = $("#list");
  const shown = items.filter(matches);
  const keep = new Set(shown.map((i) => i.id));
  for (const [id, li] of rows) if (!keep.has(id)) { li.remove(); rows.delete(id); }
  shown.forEach((it, idx) => {
    let li = rows.get(it.id);
    if (!li) { li = makeRow(it.id); rows.set(it.id, li); }
    fill(li, it);
    if (list.children[idx] !== li) list.insertBefore(li, list.children[idx] ?? null);
  });
  const empty = $("#empty");
  empty.hidden = shown.length > 0;
  empty.textContent = items.length === 0 ? t("empty") : t("empty_filter");
}

async function refresh() {
  try {
    const s = await api.call("list");
    items = s.items;
    defaultDir = s.default_dir;
    render();
  } catch (e) {
    console.error(e);
  }
}

function showFolder() {
  const f = $("#folder-show");
  f.textContent = pickedFolder ?? `${t("folder_default")} — ${defaultDir}`;
  f.title = pickedFolder ?? defaultDir;
  $("#btn-reset").hidden = !pickedFolder;
}

function openAdd(prefill) {
  $("#add-error").hidden = true;
  $("#in-links").value = prefill ?? "";
  $("#in-name").value = "";
  showFolder();
  $("#dlg-add").showModal();
  $("#in-links").focus();
}

async function submitAdd(ev) {
  ev.preventDefault();
  const errEl = $("#add-error");
  errEl.hidden = true;
  const start = $("#btn-start");
  start.disabled = true;
  try {
    const name = $("#in-name").value.trim();
    const r = await api.call("add", {
      links: $("#in-links").value,
      folder: pickedFolder,
      name: name || null,
    });
    if (r.refused.length) {
      const lines = r.refused.map(([u, k]) => `• ⁨${u}⁩ —${errText(k) ?? k}`);
      errEl.textContent = `${r.added.length ? t("added_n")(r.added.length) + "\n" : ""}${t("refused_n")(r.refused.length)}\n${lines.join("\n")}`;
      errEl.hidden = false;
      $("#in-links").value = r.refused.map(([u]) => u).join("\n");
    } else {
      $("#dlg-add").close();
      toast(t("added_n")(r.added.length));
    }
    await refresh();
  } catch (e) {
    const k = String(e?.message ?? e);
    errEl.textContent = errText(k) ?? k;
    errEl.hidden = false;
  } finally {
    start.disabled = false;
  }
}

$("#btn-add").addEventListener("click", () => openAdd());
$("#btn-cancel").addEventListener("click", () => $("#dlg-add").close());
$("#form-add").addEventListener("submit", submitAdd);
$("#btn-pick").addEventListener("click", async () => {
  try {
    const p = await api.call("pick_folder");
    if (p) { pickedFolder = p; showFolder(); }
  } catch (e) { failure(e); }
});
$("#btn-reset").addEventListener("click", () => { pickedFolder = null; showFolder(); });
$("#btn-clear").addEventListener("click", async () => {
  try {
    const n = await api.call("clear_finished");
    toast(t("cleared_n")(n));
    await refresh();
  } catch (e) { failure(e); }
});
$("#btn-lang").addEventListener("click", () => {
  lang = lang === "ar" ? "en" : "ar";
  localStorage.setItem("lang", lang);
  applyLang();
});
for (const b of document.querySelectorAll("[data-filter]")) {
  b.addEventListener("click", () => {
    filter = b.dataset.filter;
    document.querySelectorAll("[data-filter]").forEach((x) => x.classList.toggle("on", x === b));
    render();
  });
}
// Ctrl+V anywhere outside a text field: open "add" with the pasted link
document.addEventListener("paste", (e) => {
  if (e.target.closest?.("textarea, input") || $("#dlg-add").open) return;
  const text = e.clipboardData?.getData("text") ?? "";
  if (/^\s*https?:\/\//i.test(text)) { e.preventDefault(); openAdd(text.trim()); }
});

applyLang();
await refresh();
setInterval(refresh, 700);
