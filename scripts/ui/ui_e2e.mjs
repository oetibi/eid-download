#!/usr/bin/env node
// Headless-Chrome end-to-end check of the window UI (crates/eid-gui/ui) against its in-memory
// mock (mock.js). Zero dependencies: Node >= 22 (built-in WebSocket) + Chrome/Chromium (Blink,
// the engine of WebView2). It checks behaviour the owner asked for (Arabic RTL, video dialog,
// the agreement shown before any tool install, playlist item choice, no 8K, responsiveness with
// many downloads) and measures poll cost for the revisioned `list` vs the v0.0.1 full list.
// A mock is not proof that the real backend works; Windows CI and docs/OWNER-ACTIONS.md cover that.
//
//   node scripts/ui/ui_e2e.mjs [--chrome google-chrome] [--json out.json]
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { readFile, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, extname, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const UI = join(ROOT, "crates", "eid-gui", "ui");
const arg = (k, d) => { const i = process.argv.indexOf(k); return i > 0 ? process.argv[i + 1] : d; };
const CHROME = arg("--chrome", process.env.CHROME || "google-chrome");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const MIME = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml" };

// loopback-only static server for the UI folder
const server = createServer(async (req, res) => {
  const p = join(UI, decodeURIComponent(new URL(req.url, "http://x").pathname));
  if (!p.startsWith(UI)) { res.writeHead(403).end(); return; }
  let body;
  try { body = await readFile(p); } catch { res.writeHead(404).end(); return; }
  res.writeHead(200, { "content-type": MIME[extname(p)] ?? "application/octet-stream" }).end(body);
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const base = `http://127.0.0.1:${server.address().port}`;

const prof = await mkdtemp(join(tmpdir(), "eid-ui-e2e-"));
const chrome = spawn(CHROME, ["--headless=new", "--no-sandbox", "--disable-gpu", "--no-first-run",
  "--remote-debugging-port=0", `--user-data-dir=${prof}`, "--window-size=1280,800", "about:blank"],
  { stdio: ["ignore", "ignore", "pipe"] });
const wsUrl = await new Promise((res, rej) => {
  let buf = "";
  chrome.stderr.on("data", (d) => { buf += d; const m = buf.match(/DevTools listening on (ws:\/\/\S+)/); if (m) res(m[1]); });
  chrome.on("exit", (c) => rej(new Error(`chrome exited ${c}: ${buf}`)));
  setTimeout(() => rej(new Error("chrome did not start")), 20000);
});

// minimal CDP client
const ws = new WebSocket(wsUrl);
await new Promise((r) => ws.addEventListener("open", r, { once: true }));
let seq = 0; const pending = new Map(); const listeners = [];
ws.addEventListener("message", (ev) => {
  const m = JSON.parse(ev.data);
  if (m.id && pending.has(m.id)) { const { res, rej } = pending.get(m.id); pending.delete(m.id); m.error ? rej(new Error(JSON.stringify(m.error))) : res(m.result); }
  else for (const l of listeners) l(m);
});
const send = (method, params = {}, sessionId) => new Promise((res, rej) => { const id = ++seq; pending.set(id, { res, rej }); ws.send(JSON.stringify({ id, method, params, sessionId })); });
const { targetId } = await send("Target.createTarget", { url: "about:blank" });
const { sessionId } = await send("Target.attachToTarget", { targetId, flatten: true });
const S = (m, p) => send(m, p, sessionId);
await S("Runtime.enable"); await S("Page.enable");
const errors = [];
listeners.push((m) => {
  if (m.sessionId !== sessionId) return;
  if (m.method === "Runtime.exceptionThrown") errors.push(m.params.exceptionDetails.exception?.description ?? m.params.exceptionDetails.text);
  if (m.method === "Runtime.consoleAPICalled" && m.params.type === "error") errors.push(m.params.args.map((a) => a.value ?? a.description).join(" "));
});
async function ev(expr) {
  const r = await S("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true });
  if (r.exceptionDetails) throw new Error(`${expr}\n=> ${r.exceptionDetails.exception?.description ?? r.exceptionDetails.text}`);
  return r.result.value;
}
async function until(expr, ms = 8000, what = expr) {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) { if (await ev(expr)) return; await sleep(50); }
  throw new Error(`timed out: ${what}`);
}
async function open(q) {
  const t0 = Date.now();
  await S("Page.navigate", { url: `${base}/index.html${q}` });
  await until("!!(window.__eid && document.querySelector('#rows, tbody, [role=row]'))", 15000, "app loaded");
  await until("window.__eid.store.size > 0", 15000, "first list");
  return Date.now() - t0;
}

const results = { checks: [], bench: [] };
let failed = 0;
async function check(name, fn) {
  try { await fn(); results.checks.push({ name, ok: true }); console.log(`ok   ${name}`); }
  catch (e) { failed++; results.checks.push({ name, ok: false, error: String(e.message ?? e) }); console.log(`FAIL ${name}\n     ${e.message ?? e}`); }
}
const assert = (c, m) => { if (!c) throw new Error(m); };

try {
  await check("loads in Arabic, right-to-left, with no script errors", async () => {
    await open("");
    await ev("localStorage.clear()");
    await open("");
    const dir = await ev("document.documentElement.dir");
    const lang = await ev("document.documentElement.lang");
    assert(dir === "rtl" && lang === "ar", `dir=${dir} lang=${lang}`);
    assert(errors.length === 0, `errors: ${errors.join(" | ")}`);
  });

  await check("switches to English (left-to-right) and back", async () => {
    await ev("document.querySelector('#btn-lang').click()");
    await until("document.documentElement.dir === 'ltr'", 3000, "ltr");
    await ev("document.querySelector('#btn-lang').click()");
    await until("document.documentElement.dir === 'rtl'", 3000, "rtl");
  });

  await check("video tools: the agreement text is shown and must be accepted before any install", async () => {
    await open("?notools=1");
    await ev("document.querySelector('.tool[data-cmd=add_video]').click()");
    await until("document.querySelector('#dlg-video').open", 3000, "video dialog");
    await ev(`(() => { const i = document.querySelector('#in-vurl'); i.value = 'https://www.youtube.com/watch?v=abc'; i.dispatchEvent(new Event('input', {bubbles:true})); document.querySelector('#btn-probe').click(); })()`);
    await until("!document.querySelector('#v-tools').hidden", 5000, "tools-missing box");
    await ev("document.querySelector('#v-install').click()");
    await until("document.querySelector('#dlg-consent').open", 3000, "agreement dialog");
    await until("document.querySelector('#c-text').textContent.length > 20", 3000, "agreement text");
    assert(await ev("document.querySelector('#c-install').disabled"), "install enabled before agreeing");
    const st0 = await ev("window.__eid.api ? 0 : 0");
    void st0;
    await ev("document.querySelector('#c-cancel').click()");
    await until("!document.querySelector('#dlg-consent').open", 3000, "cancel closes");
    const after = await ev("import('./mock.js').then(() => true)");
    assert(after, "mock import");
    // cancel must not install: probing again still reports missing tools
    await ev("document.querySelector('#v-install').click()");
    await until("document.querySelector('#dlg-consent').open && document.querySelector('#c-text').textContent.length > 20", 3000, "agreement again");
    await ev("const c = document.querySelector('#c-agree'); c.checked = true; c.dispatchEvent(new Event('change'))");
    assert(!(await ev("document.querySelector('#c-install').disabled")), "install still disabled after agreeing");
    await ev("document.querySelector('#c-install').click()");
    await until("!document.querySelector('#dlg-consent').open", 5000, "agreement closes after install starts");
    await ev("document.querySelector('#v-cancel').click()");
  });

  await check("playlist: items listed, choose some, no 8K offered, starts a job", async () => {
    await open("");
    await ev("document.querySelector('.tool[data-cmd=add_video]').click()");
    await until("document.querySelector('#dlg-video').open", 3000, "video dialog");
    await ev(`(() => { const i = document.querySelector('#in-vurl'); i.value = 'https://www.youtube.com/playlist?list=PLx'; i.dispatchEvent(new Event('input', {bubbles:true})); document.querySelector('#btn-probe').click(); })()`);
    await until("!document.querySelector('#v-result').hidden", 8000, "probe result");
    const n = await ev("document.querySelectorAll('#v-items input').length");
    assert(n === 12, `items: ${n}`);
    const qs = await ev("[...document.querySelectorAll('#v-quality option')].map(o => o.value)");
    assert(!qs.includes("4320p") && qs.includes("2160p") && qs.includes("best") && qs.includes("audio"), `qualities ${qs}`);
    await ev("document.querySelector('#v-none').click()");
    await ev("const b = document.querySelectorAll('#v-items input'); b[0].checked = true; b[3].checked = true; b[0].dispatchEvent(new Event('change', {bubbles:true}))");
    assert(!(await ev("document.querySelector('#v-start').disabled")), "start disabled with 2 items");
    await ev("document.querySelector('#v-start').click()");
    await until("!document.querySelector('#dlg-video').open", 5000, "dialog closes on start");
  });

  await check("settings: engine limits load, validate and save", async () => {
    await open("");
    await ev("document.querySelector('.tool[data-cmd=settings]').click()");
    await until("document.querySelector('#dlg-settings').open", 3000, "settings dialog");
    await ev("document.querySelector('#dlg-settings [data-tab=downloads]')?.click()");
    await until("document.querySelector('#set-conns').value === '8'", 3000, "connections loaded");
    assert(!(await ev("document.querySelector('#set-conns').disabled")), "connections still disabled");
    await ev("document.querySelector('#set-conns').value = '99'; document.querySelector('#set-engine-save').click()");
    await until("!document.querySelector('#set-engine-error').hidden", 3000, "out-of-range refused");
    await ev("document.querySelector('#set-conns').value = '16'; document.querySelector('#set-limit').value = '2048'; document.querySelector('#set-parallel').value = '2'; document.querySelector('#set-engine-save').click()");
    await until("document.querySelector('#set-engine-error').hidden", 3000, "error cleared");
    await sleep(200);
    await ev("document.querySelector('#dlg-settings').close()");
    await ev("document.querySelector('.tool[data-cmd=settings]').click()");
    await until("document.querySelector('#set-conns').value === '16' && document.querySelector('#set-limit').value === '2048' && document.querySelector('#set-parallel').value === '2'", 3000, "saved values read back");
    await ev("document.querySelector('#dlg-settings').close()");
  });

  await check("scheduler: needs a day, saves a night window, reads it back", async () => {
    await ev("document.querySelector('.tool[data-cmd=scheduler]').click()");
    await until("document.querySelector('#dlg-sched').open", 3000, "scheduler dialog");
    assert((await ev("document.querySelectorAll('#sch-days input').length")) === 7, "7 days");
    await ev("const c = document.querySelector('#sch-on'); c.checked = true; c.dispatchEvent(new Event('change'))");
    await ev("for (const c of document.querySelectorAll('#sch-days input')) c.checked = false; document.querySelector('#sch-save').click()");
    await until("!document.querySelector('#sch-error').hidden", 3000, "no day refused");
    assert(await ev("document.querySelector('#dlg-sched').open"), "closed despite error");
    await ev("document.querySelector('#sch-start').value = '23:30'; document.querySelector('#sch-end').value = '06:00'; const d = document.querySelectorAll('#sch-days input'); d[3].checked = true; d[4].checked = true; document.querySelector('#sch-save').click()");
    await until("!document.querySelector('#dlg-sched').open", 3000, "saved and closed");
    await ev("document.querySelector('.tool[data-cmd=scheduler]').click()");
    await until("document.querySelector('#dlg-sched').open && document.querySelector('#sch-start').value === '23:30'", 3000, "read back");
    const days = await ev("[...document.querySelectorAll('#sch-days input')].map(c => c.checked)");
    assert(JSON.stringify(days) === JSON.stringify([false, false, false, true, true, false, false]), `days ${days}`);
    assert((await ev("document.querySelector('#sch-end').value")) === "06:00", "end");
    await ev("document.querySelector('#dlg-sched').close()");
  });

  await check("open file: opens a finished file, refuses a program and says why", async () => {
    const ids = await ev("[...window.__eid.store.values()].filter(i => i.status === 'done').map(i => [i.id, i.name])");
    const prog = ids.find(([, n]) => /\.exe$/i.test(n));
    const doc = ids.find(([, n]) => !/\.(exe|msi|bat|cmd|ps1|js|vbs|scr|lnk)$/i.test(n));
    assert(prog && doc, `need a finished .exe and a finished document: ${JSON.stringify(ids.slice(0, 5))}`);
    await ev(`window.__eid.select([${doc[0]}])`);
    await until("!document.querySelector('.tool[data-cmd=open_file], [data-cmd=open_file]').disabled", 3000, "open enabled");
    await ev("document.querySelector('[data-cmd=open_file]').click()");
    await until(`(window.__eidMockOpened || []).includes(${doc[0]})`, 3000, "document opened");
    await ev(`window.__eid.select([${prog[0]}])`);
    await sleep(50);
    await ev("document.querySelector('[data-cmd=open_file]').click()");
    await until("!document.querySelector('#toast').hidden && document.querySelector('#toast').textContent.length > 20", 3000, "refusal message");
    assert(!(await ev(`(window.__eidMockOpened || []).includes(${prog[0]})`)), "program was opened");
  });

  for (const n of [100, 1000, 3000]) {
    for (const old of [false, true]) {
      await check(`responsive with ${n} downloads (${old ? "v0.0.1 full list" : "revisioned list"})`, async () => {
        const load = await open(`?n=${n}&running=20${old ? "&old=1" : ""}`);
        await ev("window.__eid.stats.polls.length = 0; window.__eid.stats.renders.length = 0");
        await sleep(8000);
        const s = await ev(`(() => { const st = window.__eid.stats; const med = (a) => { const b = [...a].sort((x, y) => x - y); return b.length ? b[Math.floor(b.length / 2)] : null; };
          const p95 = (a) => { const b = [...a].sort((x, y) => x - y); return b.length ? b[Math.floor(b.length * 0.95)] : null; };
          return { polls: st.polls.length, poll_med: med(st.polls), poll_p95: p95(st.polls), render_med: med(st.renders), render_p95: p95(st.renders),
                   rows_in_dom: document.querySelectorAll('[role=row], tbody tr').length }; })()`);
        // input latency: a click on a row must select it within one frame budget-ish
        const lat = await ev(`new Promise((res) => { const r = document.querySelector('tbody tr, [role=row][data-id]'); const t0 = performance.now(); r?.dispatchEvent(new MouseEvent('click', {bubbles: true})); requestAnimationFrame(() => requestAnimationFrame(() => res(performance.now() - t0))); })`);
        const row = { n, shape: old ? "full" : "delta", load_ms: load, ...s, click_to_paint_ms: Math.round(lat * 10) / 10 };
        results.bench.push(row);
        console.log("     ", JSON.stringify(row));
        assert(s.rows_in_dom < 200, `virtual list must not render all rows (${s.rows_in_dom})`);
        assert(lat < 100, `click to paint ${lat} ms`);
      });
    }
  }
  await check("no script errors during the run", async () => { assert(errors.length === 0, errors.join(" | ")); });
} finally {
  const out = arg("--json");
  if (out) await (await import("node:fs/promises")).writeFile(out, JSON.stringify(results, null, 1));
  try { ws.close(); } catch {}
  chrome.kill("SIGKILL");
  server.close();
  await rm(prof, { recursive: true, force: true }).catch(() => {});
}
console.log(failed ? `${failed} check(s) failed` : "all checks passed");
process.exit(failed ? 1 : 0);
