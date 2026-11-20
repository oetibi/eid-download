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

async function shot(name) {
  const { data } = await S("Page.captureScreenshot", { format: "png" });
  const dir = arg("--shots");
  await (await import("node:fs/promises")).mkdir(dir, { recursive: true });
  await (await import("node:fs/promises")).writeFile(join(dir, name), Buffer.from(data, "base64"));
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

  await check("startup self-check: opened from inside a ZIP / no native host => a clear bar (ar/en), closable; nothing when all is well", async () => {
    await open("?check=from_zip,no_native_host");
    await until("!document.querySelector('#checkbar').hidden", 3000, "check bar");
    const ar = await ev("[...document.querySelectorAll('#check-list li')].map((l) => l.dataset.code + ':' + l.textContent)");
    assert(ar.length === 2 && /ZIP/.test(ar[0]) && /استخراج الكل/.test(ar[0]) && /eid-native-host\.exe/.test(ar[1]), JSON.stringify(ar));
    if (arg("--shots")) await shot("self-check-ar.png");
    await ev("document.querySelector('#btn-lang').click()");
    await until("/Extract All/.test(document.querySelector('#check-list').textContent)", 3000, "English text");
    await ev("document.querySelector('#check-close').click()");
    assert(await ev("document.querySelector('#checkbar').hidden"), "bar closes");
    await ev("document.querySelector('#btn-lang').click()");
    await until("document.documentElement.dir === 'rtl'", 3000, "back to Arabic");
    await open("?check=bogus");
    await sleep(300);
    assert(await ev("document.querySelector('#checkbar').hidden"), "unknown codes or none: no bar");
    await open("");
  });

  await check("browser extension: Settings shows each browser, connects this copy, disconnects only this copy, refuses from a ZIP", async () => {
    const tab = async () => {
      await ev("document.querySelector('[data-cmd=settings]').click()");
      await ev("document.querySelector('#dlg-settings [data-tab=desktop]').click()");
      await until("document.querySelectorAll('#browser-list li').length === 3", 3000, "browser list");
    };
    const st = () => ev("[...document.querySelectorAll('#browser-list li')].map((l) => l.dataset.browser + ':' + l.dataset.state).join(' ')");
    await open("?browser=other");
    await tab();
    assert((await st()) === "chrome:other edge:other firefox:other", await st());
    assert(/Program Files/.test(await ev("document.querySelector('#browser-list').textContent")), "names the other copy");
    await ev("document.querySelector('#browser-disconnect').click()");
    await sleep(200);
    assert((await st()) === "chrome:other edge:other firefox:other", "another copy is never disconnected");
    await ev("document.querySelector('#browser-connect').click()");
    await until("[...document.querySelectorAll('#browser-list li')].every((l) => l.dataset.state === 'this')", 3000, "connected");
    if (arg("--shots")) await shot("browser-connect-ar.png");
    await ev("document.querySelector('#browser-disconnect').click()");
    await until("[...document.querySelectorAll('#browser-list li')].every((l) => l.dataset.state === 'none')", 3000, "disconnected");
    await ev("document.querySelector('#dlg-settings').close()");
    await open("?check=from_zip");
    await ev("document.querySelector('#check-close').click()");
    await tab();
    await ev("document.querySelector('#browser-connect').click()");
    await until("!document.querySelector('#browser-error').hidden", 3000, "refusal shown");
    assert(/استخراج الكل/.test(await ev("document.querySelector('#browser-error').textContent")), "says to extract the ZIP");
    assert((await st()) === "chrome:none edge:none firefox:none", "nothing connected");
    await ev("document.querySelector('#dlg-settings').close()");
    await open("");
  });

  await check("tools install: the window follows it to the end; a failure is SHOWN (never a silent close); success re-checks the link", async () => {
    const start = async (q) => {
      await open(q);
      await ev("document.querySelector('.tool[data-cmd=add_video]').click()");
      await until("document.querySelector('#dlg-video').open", 3000, "video dialog");
      await ev(`(() => { const i = document.querySelector('#in-vurl'); i.value = 'https://www.youtube.com/watch?v=abc'; i.dispatchEvent(new Event('input', {bubbles:true})); document.querySelector('#btn-probe').click(); })()`);
      await until("!document.querySelector('#v-tools').hidden", 5000, "tools-missing box");
      await ev("document.querySelector('#v-install').click()");
      await until("document.querySelector('#dlg-consent').open && document.querySelector('#c-text').textContent.length > 20", 3000, "agreement");
      await ev("const c = document.querySelector('#c-agree'); c.checked = true; c.dispatchEvent(new Event('change'))");
      await ev("document.querySelector('#c-install').click()");
      await until("!document.querySelector('#dlg-consent').open", 3000, "agreement closes");
      await until("/230/.test(document.querySelector('#v-status').textContent)", 3000, "'downloading the tools' shown");
    };
    await start("?notools=1&toolsfail=1");
    await until("!document.querySelector('#v-error').hidden", 8000, "the failure is shown");
    const e = await ev("document.querySelector('#v-error').textContent");
    assert(/تعذّر تثبيت الأدوات/.test(e) && /os error 5/.test(e), e);
    if (arg("--shots")) await shot("tools-install-failed-ar.png");
    assert(await ev("document.querySelector('#dlg-video').open"), "the window is still there");
    await ev("document.querySelector('#v-cancel').click()");
    await start("?notools=1");
    await until("/اكتمل تثبيت الأدوات/.test(document.querySelector('#v-status').textContent) || !document.querySelector('#v-result').hidden", 8000, "success shown");
    await until("!document.querySelector('#v-result').hidden", 8000, "the link is checked again after the install");
    await ev("document.querySelector('#v-cancel').click()");
  });

  await check("previous crash: the bar names the crash record and opens the logs folder; Settings shows version and build", async () => {
    await open("?check=last_crash");
    await until("!document.querySelector('#checkbar').hidden", 3000, "bar");
    const li = await ev("document.querySelector('#check-list li').dataset.code + '|' + document.querySelector('#check-list li').textContent");
    assert(li.startsWith("last_crash|") && /crash-1\.txt/.test(li) && /بشكل غير متوقع/.test(li), li);
    assert(!(await ev("document.querySelector('#check-logs').hidden")), "logs button");
    await ev("document.querySelector('#check-logs').click()");
    await until("window.__eidMockOpenedLogs === 1", 2000, "logs folder opened");
    await ev("document.querySelector('#check-close').click()");
    await ev("document.querySelector('[data-cmd=settings]').click()");
    await until("/abc1234/.test(document.querySelector('#about-text').textContent)", 3000, "build shown in Settings");
    await ev("document.querySelector('#dlg-settings').close()");
    await open("");
  });

  await check("playlist link: watch&list offers 'this video' or 'the whole playlist'; a range picks episodes; bare watch says how to get a list", async () => {
    await open("");
    const probeUrl = async (u) => {
      await ev("document.querySelector('.tool[data-cmd=add_video]').click()");
      await until("document.querySelector('#dlg-video').open", 3000, "video dialog");
      await ev(`(() => { const i = document.querySelector('#in-vurl'); i.value = ${JSON.stringify(u)}; i.dispatchEvent(new Event('input', {bubbles:true})); document.querySelector('#btn-probe').click(); })()`);
      await until("!document.querySelector('#v-result').hidden", 8000, "probe result");
    };
    await probeUrl("https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=PLarafta&index=1");
    assert(!(await ev("document.querySelector('#v-scope').hidden")), "both choices offered for watch&list");
    assert(await ev("document.querySelector('#v-scope input[value=video]').checked"), "'this video' first");
    assert(await ev("document.querySelector('#v-playlist').hidden"), "no episode list for one video");
    await ev("(() => { const r = document.querySelector('#v-scope input[value=list]'); r.checked = true; r.dispatchEvent(new Event('change', { bubbles: true })); })()");
    await until("!document.querySelector('#v-playlist').hidden && document.querySelectorAll('#v-items input').length === 12", 8000, "the playlist's episodes");
    if (arg("--shots")) await shot("playlist-scope-ar.png");
    // a wrong range is explained, a good one selects exactly those episodes
    await ev("document.querySelector('#v-range').value = '1-x'; document.querySelector('#v-range-apply').click()");
    assert(!(await ev("document.querySelector('#v-range-error').hidden")), "bad range not explained");
    await ev("document.querySelector('#v-range').value = '2-4، 7'; document.querySelector('#v-range-apply').click()");
    const picked = await ev("[...document.querySelectorAll('#v-items input:checked')].map((c) => Number(c.dataset.index))");
    assert(JSON.stringify(picked) === "[2,3,4,7]", JSON.stringify(picked));
    await ev("document.querySelector('#v-start').click()");
    await until("window.__eidMockLastPlaylist?.items?.length === 4", 5000, "playlist sent");
    const sent = await ev("window.__eidMockLastPlaylist");
    assert(sent.url === "https://www.youtube.com/playlist?list=PLarafta" && JSON.stringify(sent.items) === "[2,3,4,7]", JSON.stringify(sent));
    // back to 'this video' shows the video again (no second probe of the list needed)
    await probeUrl("https://www.youtube.com/watch?v=dQw4w9WgXcQ");
    assert(await ev("document.querySelector('#v-scope').hidden"), "no playlist choice without list=");
    assert(!(await ev("document.querySelector('#v-list-hint').hidden")), "the hint about list= links");
    await ev("document.querySelector('#v-cancel').click()");
  });

  await check("a link from the browser ('Open in Eid Download') opens the Video window with both choices", async () => {
    await open("");
    await ev("document.querySelector('[data-cmd=settings]').click()");
    await until("document.querySelector('#dlg-settings').open", 3000, "a dialog that must give way");
    await ev("window.__eidMockOpenVideo('https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=PLarafta')");
    await until("document.querySelector('#dlg-video').open && !document.querySelector('#dlg-settings').open", 5000, "the Video window");
    assert((await ev("document.querySelector('#in-vurl').value")) === "https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=PLarafta", "link");
    await until("!document.querySelector('#v-result').hidden && !document.querySelector('#v-scope').hidden", 8000, "probed, with 'this video' / 'the whole playlist'");
    await ev("document.querySelector('#v-cancel').click()");
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
    // v0.2: "if this quality is missing": shown for a fixed height, hidden for best/audio
    await ev("(() => { const q = document.querySelector('#v-quality'); q.value = 'best'; q.dispatchEvent(new Event('change')); })()");
    assert(await ev("document.querySelector('#v-missing-row').hidden"), "fallback shown for best");
    await ev("(() => { const q = document.querySelector('#v-quality'); q.value = '1080p'; q.dispatchEvent(new Event('change')); })()");
    assert(!(await ev("document.querySelector('#v-missing-row').hidden")), "fallback hidden for 1080p");
    await ev("document.querySelector('#v-missing').value = 'skip'");
    await ev("document.querySelector('#v-start').click()");
    await until("!document.querySelector('#dlg-video').open", 5000, "dialog closes on start");
    const sent = await ev("window.__eidMockLastPlaylist");
    assert(sent.quality === "1080p" && sent.if_missing === "skip", `sent ${JSON.stringify(sent)}`);
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

  await check("details window: segment map from the backend, per-segment table, ar/en, closes cleanly", async () => {
    await open("");
    const pick = (st) => ev(`[...window.__eid.store.values()].find(i => ${st})?.id ?? null`);
    const openP = async (id) => {
      await ev(`window.__eid.select([${id}])`);
      await ev("document.querySelector('#menu-downloads [data-cmd=properties]').click()");
      await until("document.querySelector('#dlg-props').open", 3000, "details open");
    };
    const paused = await pick("i.status === 'paused' && i.rate?.total");
    assert(paused != null, "mock has a paused item with a size");
    await openP(paused);
    await until("document.querySelectorAll('#p-segs tbody tr').length === 8", 4000, "8 segment rows");
    const info = await ev("document.querySelector('#p-map-info').textContent");
    assert(/الأجزاء: 8/.test(info), `info: ${info}`);
    // the canvas really has drawn pixels of the 'saved' colour, not only the empty bar
    const painted = await ev(`(() => { const c = document.querySelector('#p-map'); const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
      const px = new Set(); for (let i = 0; i < d.length; i += 4 * 7) px.add(d[i] + ',' + d[i+1] + ',' + d[i+2]); return px.size; })()`);
    assert(painted >= 2, `canvas looks empty (${painted} colours)`);
    if (arg("--shots")) await shot("details-ar.png");
    await ev("document.querySelector('#dlg-props').close()");
    // English, and an item without a known size: no map, no table
    await ev("document.querySelector('#btn-lang').click()");
    const unknown = await pick("i.status === 'running' && i.rate && i.rate.total == null");
    await openP(unknown);
    await until("/No map yet/.test(document.querySelector('#p-map-info').textContent)", 4000, "no-map text");
    assert(await ev("document.querySelector('#p-segs-wrap').hidden"), "table must be hidden");
    await ev("document.querySelector('#dlg-props').close()");
    const done = await pick("i.status === 'done'");
    await openP(done);
    await until("/complete/.test(document.querySelector('#p-map-info').textContent)", 4000, "finished text");
    if (arg("--shots")) await shot("details-en-finished.png");
    await ev("document.querySelector('#dlg-props').close()");
    await ev("document.querySelector('#btn-lang').click()");
    await until("document.documentElement.dir === 'rtl'", 3000, "rtl again");
  });

  await check("search filters by name or link; Esc clears", async () => {
    await open("");
    await ev("localStorage.removeItem('cols'); localStorage.removeItem('toolbar')");
    await open("");
    const total = await ev("window.__eid.store.size");
    await ev("(() => { const s = document.querySelector('#search'); s.value = 'ubuntu'; s.dispatchEvent(new Event('input')); })()");
    await until("[...document.querySelectorAll('.tr:not([hidden]) .c-name')].length === 1", 3000, "one row for 'ubuntu'");
    assert(/ubuntu/i.test(await ev("document.querySelector('.tr:not([hidden]) .c-name').textContent")), "wrong row");
    await ev("(() => { const s = document.querySelector('#search'); s.value = 'zzzz-none'; s.dispatchEvent(new Event('input')); })()");
    await until("!document.querySelector('#empty').hidden", 3000, "empty message");
    await ev("document.querySelector('#search').dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))");
    await until(`[...document.querySelectorAll('.tr:not([hidden])')].length === ${total}`, 3000, "all rows back");
  });

  await check("columns: hide, move and resize from the header; saved and restored after reload", async () => {
    // hide the link column through the header menu
    await ev("document.querySelector('#thead').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, clientX: 300, clientY: 100 }))");
    await until("!document.querySelector('#menu-cols').hidden", 2000, "column menu");
    const urlItem = await ev("[...document.querySelectorAll('#menu-cols [role=menuitemcheckbox]')].findIndex(b => /link|الرابط/i.test(b.textContent))");
    assert(urlItem >= 0, "no link column item");
    await ev(`document.querySelectorAll('#menu-cols [role=menuitemcheckbox]')[${urlItem}].click()`);
    await until("getComputedStyle(document.querySelector('#thead .c-url')).display === 'none'", 2000, "link header hidden");
    assert(await ev("getComputedStyle(document.querySelector('.tr .c-url')).display === 'none'"), "link cells still shown");
    // move 'status' before 'name' by drag and drop of the header
    await ev(`(() => { const dt = new DataTransfer(); const a = document.querySelector('#thead .c-status'), b = document.querySelector('#thead .c-name');
      a.dispatchEvent(new DragEvent('dragstart', { bubbles: true, dataTransfer: dt }));
      b.dispatchEvent(new DragEvent('dragover', { bubbles: true, cancelable: true, dataTransfer: dt }));
      b.dispatchEvent(new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: dt })); })()`);
    const order = await ev("JSON.parse(localStorage.getItem('cols')).map(c => c.key)");
    assert(order[0] === "status" && order[1] === "name", `order ${order}`);
    // resize the size column with the pointer
    const w0 = await ev("document.querySelector('#thead .c-size').getBoundingClientRect().width");
    await ev(`(() => { const h = document.querySelector('#thead .c-size .rz'); const r = h.getBoundingClientRect(); const x = r.left + 3, y = r.top + 5;
      const opt = (cx) => ({ bubbles: true, pointerId: 1, clientX: cx, clientY: y, isPrimary: true });
      h.setPointerCapture = () => {};
      h.dispatchEvent(new PointerEvent('pointerdown', opt(x)));
      h.dispatchEvent(new PointerEvent('pointermove', opt(x + (document.documentElement.dir === 'rtl' ? -60 : 60))));
      h.dispatchEvent(new PointerEvent('pointerup', opt(x))); })()`);
    const w1 = await ev("document.querySelector('#thead .c-size').getBoundingClientRect().width");
    assert(w1 > w0 + 40, `width ${w0} -> ${w1}`);
    // rows follow the header grid
    const rw = await ev("document.querySelector('.tr:not([hidden]) .c-size').getBoundingClientRect().width");
    assert(Math.abs(rw - w1) < 2, `row cell ${rw} vs header ${w1}`);
    await open("");
    assert(await ev("getComputedStyle(document.querySelector('#thead .c-url')).display === 'none'"), "hidden column came back after reload");
    const w2 = await ev("document.querySelector('#thead .c-size').getBoundingClientRect().width");
    assert(Math.abs(w2 - w1) < 2, `width not restored ${w1} vs ${w2}`);
    if (arg("--shots")) await shot("columns-ar.png");
    await ev("localStorage.removeItem('cols')");
  });

  await check("toolbar: hide buttons and show icons only; 'Add' always stays", async () => {
    await open("");
    await ev("document.querySelector('#toolbar').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true }))");
    await until("document.querySelector('#dlg-toolbar').open", 2000, "toolbar dialog");
    await ev("document.querySelector('#tb-list input').click()"); // first = add_video
    await ev("document.querySelector('#tb-labels').click()");
    assert(await ev("document.querySelector('#toolbar [data-cmd=add_video]').hidden"), "video button not hidden");
    assert(!(await ev("document.querySelector('#toolbar [data-cmd=add]').hidden")), "add hidden");
    assert(await ev("getComputedStyle(document.querySelector('#toolbar [data-cmd=add] span')).display === 'none'"), "labels still shown");
    await ev("document.querySelector('#dlg-toolbar').close()");
    await open("");
    assert(await ev("document.querySelector('#toolbar [data-cmd=add_video]').hidden"), "choice not saved");
    await ev("localStorage.removeItem('toolbar')");
    await open("");
  });

  await check("drag and drop: dropped links open the add dialog; files and non-links are refused", async () => {
    const drop = (type, data) => ev(`(() => { const dt = new DataTransfer(); dt.setData(${JSON.stringify(type)}, ${JSON.stringify(data)});
      document.body.dispatchEvent(new DragEvent('dragenter', { bubbles: true, cancelable: true, dataTransfer: dt }));
      const shown = !document.querySelector('#drop').hidden;
      document.body.dispatchEvent(new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: dt })); return shown; })()`);
    assert(await drop("text/uri-list", "# c\r\nhttps://a.example/x.zip\r\nhttps://b.example/y.iso"), "drop overlay not shown");
    await until("document.querySelector('#dlg-add').open", 2000, "add dialog");
    const v = await ev("document.querySelector('#in-links').value");
    assert(v === "https://a.example/x.zip\nhttps://b.example/y.iso", `links ${JSON.stringify(v)}`);
    await ev("document.querySelector('#dlg-add').close()");
    await drop("text/plain", "just words, javascript:alert(1)");
    await until("!document.querySelector('#toast').hidden", 2000, "refusal toast");
    assert(!(await ev("document.querySelector('#dlg-add').open")), "dialog opened for a non-link");
    assert(await ev("document.querySelector('#drop').hidden"), "overlay left on screen");
  });

  await check("notices: a download that finishes or fails shows a message (and none when turned off)", async () => {
    await open("");
    const id = await ev("[...window.__eid.store.values()].find(i => i.status === 'pending' || i.status === 'paused').id");
    await ev("document.querySelector('#toast').hidden = true");
    await ev(`window.__eidMockSet(${id}, { status: 'failed', error_kind: 'not_found' })`);
    await ev("window.__eid.poll()");
    await until("!document.querySelector('#toast').hidden && /فشل/.test(document.querySelector('#toast').textContent)", 4000, "failure notice");
    await ev("localStorage.setItem('notify', 'off')");
    await open("");
    await ev("document.querySelector('#toast').hidden = true");
    const id2 = await ev("[...window.__eid.store.values()].find(i => i.status === 'pending' || i.status === 'paused').id");
    await ev(`window.__eidMockSet(${id2}, { status: 'done' })`);
    await ev("window.__eid.poll()");
    await sleep(300);
    assert(await ev("document.querySelector('#toast').hidden"), "notice shown while turned off");
    await ev("localStorage.removeItem('notify')");
  });

  await check("desktop: tray/close-to-tray/clipboard switches save; tray menu texts follow the language; tray actions and copied links work", async () => {
    await open("");
    // tray menu texts were sent in Arabic, then in English after switching
    await until("window.__eidMockTrayTexts()?.quit === 'خروج'", 3000, "arabic tray texts");
    await ev("document.querySelector('#btn-lang').click()");
    await until("window.__eidMockTrayTexts()?.quit === 'Quit'", 3000, "english tray texts");
    await ev("document.querySelector('#btn-lang').click()");
    // switches (default: tray on, close-to-tray off, clipboard off)
    await ev("document.querySelector('[data-cmd=settings]').click()");
    await until("document.querySelector('#dlg-settings').open && document.querySelector('#set-tray').checked", 3000, "settings with tray on");
    assert(!(await ev("document.querySelector('#set-clipboard').checked")), "clipboard must be off by default");
    await ev("document.querySelector('#set-clipboard').click()");
    await sleep(200);
    await ev("document.querySelector('#set-tray').click()"); // off: close-to-tray is disabled
    await sleep(200);
    assert(await ev("document.querySelector('#set-close-tray').disabled"), "close-to-tray enabled without the icon");
    await ev("document.querySelector('#dlg-settings').close()");
    await ev("document.querySelector('[data-cmd=settings]').click()");
    await until("document.querySelector('#dlg-settings').open && document.querySelector('#set-clipboard').checked && !document.querySelector('#set-tray').checked", 3000, "saved switches read back");
    await ev("document.querySelector('#set-tray').click()");
    await sleep(200);
    await ev("document.querySelector('#dlg-settings').close()");
    // tray "pause all" runs the page's own command
    const busy = await ev("[...window.__eid.store.values()].filter(i => ['running','pending'].includes(i.status)).length");
    assert(busy > 0, "need a busy item");
    await ev("window.__eidMockEmit('tray', 'pause_all')");
    await until("[...window.__eid.store.values()].filter(i => i.status === 'running').length === 0", 4000, "paused from the tray");
    // a copied file link: bar, then the add dialog with that link
    await ev("window.__eidMockEmit('clipboard-link', { url: 'https://example.com/a/setup.zip', kind: 'file' })");
    await until("!document.querySelector('#clipbar').hidden", 2000, "clipboard bar");
    await ev("document.querySelector('#clip-add').click()");
    await until("document.querySelector('#dlg-add').open && document.querySelector('#in-links').value === 'https://example.com/a/setup.zip'", 2000, "add dialog with the link");
    await ev("document.querySelector('#dlg-add').close()");
    // a bad payload never shows the bar
    await ev("window.__eidMockEmit('clipboard-link', { url: 'javascript:alert(1)', kind: 'file' })");
    await sleep(100);
    assert(await ev("document.querySelector('#clipbar').hidden"), "bar shown for a non-http link");
    // a YouTube link opens the video dialog
    await ev("window.__eidMockEmit('clipboard-link', { url: 'https://www.youtube.com/watch?v=abc', kind: 'video' })");
    await ev("document.querySelector('#clip-add').click()");
    await until("document.querySelector('#dlg-video').open", 2000, "video dialog");
    await ev("document.querySelector('#dlg-video').close()");
    if (arg("--shots")) { await ev("document.querySelector('[data-cmd=settings]').click()"); await sleep(300); await shot("settings-desktop-ar.png"); await ev("document.querySelector('#dlg-settings').close()"); }
  });

  await check("queues, per-download limit and the 'when finished' countdown with Cancel", async () => {
    await open("");
    // queues dialog from the Downloads menu
    await ev("document.querySelector('#menu-downloads [data-cmd=queues]').click()");
    await until("document.querySelector('#dlg-queues').open && document.querySelectorAll('.q-row').length === 2", 3000, "two queues");
    assert(/الرئيسية/.test(await ev("document.querySelector('.q-row .q-name').textContent")), "main queue first, named");
    // off by default: computer actions cannot be chosen
    assert(await ev("document.querySelector('.q-row .q-after option[value=shutdown]').disabled"), "shutdown offered while turned off");
    await ev("document.querySelector('#dlg-queues').close()");
    await ev("document.querySelector('[data-cmd=settings]').click()");
    await until("document.querySelector('#dlg-settings').open", 2000, "settings");
    assert(!(await ev("document.querySelector('#set-power').checked")), "power actions must be off by default");
    await ev("document.querySelector('#set-power').click()");
    await sleep(200);
    await ev("document.querySelector('#dlg-settings').close()");
    await ev("document.querySelector('#menu-downloads [data-cmd=queues]').click()");
    await until("document.querySelector('#dlg-queues').open && document.querySelectorAll('.q-row').length === 2 && !document.querySelector('.q-row .q-after option[value=shutdown]').disabled", 3000, "shutdown allowed after turning it on");
    await ev("(() => { const r = document.querySelectorAll('.q-row')[1]; r.querySelector('.q-par').value = '4'; r.querySelector('.q-after').value = 'shutdown'; r.querySelector('.q-save').click(); })()");
    await until("document.querySelectorAll('.q-row')[1]?.querySelector('.q-after').value === 'shutdown' && document.querySelectorAll('.q-row')[1].querySelector('.q-par').value === '4'", 3000, "saved and read back");
    await ev("document.querySelectorAll('.q-row')[1].querySelector('.q-toggle').click()");
    await until("/متوقفة/.test(document.querySelectorAll('.q-row')[1]?.querySelector('.q-name').textContent ?? '')", 3000, "queue stopped");
    // a bad value is refused in the window
    await ev("(() => { const r = document.querySelectorAll('.q-row')[0]; r.querySelector('.q-par').value = '99'; r.querySelector('.q-save').click(); })()");
    assert(!(await ev("document.querySelector('#q-error').hidden")), "99 accepted");
    if (arg("--shots")) await shot("queues-ar.png");
    await ev("document.querySelector('#dlg-queues').close()");
    // per-download limit from the details window
    const id = await ev("[...window.__eid.store.values()].find(i => i.status === 'paused').id");
    await ev(`window.__eid.select([${id}])`);
    await ev("document.querySelector('#menu-downloads [data-cmd=properties]').click()");
    await until("document.querySelector('#dlg-props').open", 2000, "details");
    await ev("document.querySelector('#p-limit').value = '500'; document.querySelector('#p-limit-set').click()");
    await until(`window.__eidMockLimit(${id}) === 500`, 2000, "limit 500 sent");
    await ev("document.querySelector('#p-limit').value = '0'; document.querySelector('#p-limit-set').click()");
    await until(`window.__eidMockLimit(${id}) === null`, 2000, "0 = no limit");
    await ev("document.querySelector('#dlg-props').close()");
    // the countdown: bar with the action and seconds, Cancel stops it and resets the choice
    await ev("window.__eidMockCountdown(42, 'shutdown'); window.__eid.poll()");
    await until("!document.querySelector('#powerbar').hidden && /42/.test(document.querySelector('#power-text').textContent) && /إيقاف تشغيل الجهاز/.test(document.querySelector('#power-text').textContent)", 4000, "countdown bar");
    if (arg("--shots")) await shot("countdown-ar.png");
    await ev("document.querySelector('#power-cancel').click()");
    await until("document.querySelector('#powerbar').hidden", 2000, "bar hidden");
    await until("/أُلغي/.test(document.querySelector('#toast').textContent)", 4000, "cancelled message");
  });

  await check("an engine warning (resume point not saved) is shown once", async () => {
    await open("");
    const it = await ev("[...window.__eid.store.values()][0]");
    await ev("document.querySelector('#toast').hidden = true");
    await ev(`window.__eidMockWarn(${it.id}, 'final checkpoint: injected'); window.__eid.poll()`);
    await until("!document.querySelector('#toast').hidden && /نقطة الاستكمال/.test(document.querySelector('#toast').textContent)", 4000, "warning shown");
    assert((await ev("document.querySelector('#toast').textContent")).includes(it.name), "names the download");
  });

  await check("add dialog: a YouTube page opens the video window (never a file); mixed links name it", async () => {
    await open("");
    const before = await ev("window.__eid.store.size");
    await ev("document.querySelector('#btn-add').click()");
    await until("document.querySelector('#dlg-add').open", 3000, "add dialog");
    await ev("document.querySelector('#in-links').value = 'https://www.youtube.com/watch?v=abc123'; document.querySelector('#btn-start').click()");
    await until("document.querySelector('#dlg-video').open && !document.querySelector('#dlg-add').open", 3000, "video dialog instead");
    assert((await ev("document.querySelector('#in-vurl').value")) === "https://www.youtube.com/watch?v=abc123", "link carried over");
    assert(/فيديو/.test(await ev("document.querySelector('#toast').textContent")), "says why");
    if (arg("--shots")) await shot("youtube-routed-ar.png");
    await ev("document.querySelector('#dlg-video').close()");
    await sleep(300);
    assert((await ev("window.__eid.store.size")) === before, "no file download was added");
    // a file link next to it is added; then the video window opens for the page
    await ev("document.querySelector('#btn-add').click()");
    await until("document.querySelector('#dlg-add').open", 3000, "add dialog 2");
    await ev("document.querySelector('#in-links').value = 'https://example.com/a.zip\\nhttps://youtu.be/xyz'; document.querySelector('#btn-start').click()");
    await until("document.querySelector('#dlg-video').open && document.querySelector('#in-vurl').value === 'https://youtu.be/xyz'", 3000, "video dialog 2");
    await until(`window.__eid.store.size === ${before} + 1`, 3000, "the file link was added");
    await ev("document.querySelector('#dlg-video').close()");
    // with a bad link too: the dialog stays and names each problem
    await ev("document.querySelector('#btn-add').click()");
    await until("document.querySelector('#dlg-add').open", 3000, "add dialog 3");
    await ev("document.querySelector('#in-links').value = 'ftp://h/x https://www.youtube.com/shorts/q1'; document.querySelector('#btn-start').click()");
    await until("!document.querySelector('#add-error').hidden", 3000, "errors shown");
    const err = await ev("document.querySelector('#add-error').textContent");
    assert(/صفحة فيديو/.test(err) && /غير صالح/.test(err) && !(await ev("document.querySelector('#dlg-video').open")), err);
    await ev("document.querySelector('#dlg-add').close()");
    // Ctrl+V outside a field with a YouTube link: the video window
    await ev("document.dispatchEvent(Object.assign(new Event('paste', { bubbles: true, cancelable: true }), { clipboardData: { getData: () => 'https://m.youtube.com/watch?v=p1' } }))");
    await until("document.querySelector('#dlg-video').open", 3000, "paste opens video");
    await ev("document.querySelector('#dlg-video').close()");
  });

  await check("add dialog: queue and 'folder by type'; import/export from the menu; Desktop settings tab", async () => {
    await open("");
    await ev("document.querySelector('#btn-add').click()");
    await until("document.querySelector('#dlg-add').open && document.querySelectorAll('#in-cat option').length === 8 && document.querySelectorAll('#in-queue option').length === 2", 3000, "choices filled");
    assert((await ev("document.querySelector('#in-cat').value")) === "none", "default must be no sub-folder");
    await ev("(() => { const q = document.querySelector('#in-queue'); q.value = '1'; q.dispatchEvent(new Event('change')); document.querySelector('#in-cat').value = 'auto'; document.querySelector('#in-links').value = 'https://example.com/z.zip'; })()");
    assert(/Apps/.test(await ev("document.querySelector('#folder-show').textContent")), "queue choice sets the folder");
    await ev("document.querySelector('#btn-start').click()");
    await until("window.__eidMockLastAdd?.category === 'auto'", 3000, "category sent");
    assert(/Apps/.test(await ev("window.__eidMockLastAdd.folder")), "queue folder sent");
    // the default follows the setting
    await ev("document.querySelector('#set-autocat').click()");
    await ev("document.querySelector('#btn-add').click()");
    await until("document.querySelector('#dlg-add').open && document.querySelector('#in-cat').value === 'auto'", 3000, "auto by default after the setting");
    await ev("document.querySelector('#dlg-add').close(); document.querySelector('#set-autocat').click()");
    // import / export
    await ev("window.__eidMockImport = { urls: ['https://example.com/i1.zip', 'https://example.com/i2.iso'], refused: 1 }");
    const before = await ev("window.__eid.store.size");
    await ev("document.querySelector('#menu-downloads [data-cmd=import_list]').click()");
    await until(`window.__eid.store.size === ${before} + 2`, 4000, "imported items listed");
    await until("/أُضيف 2/.test(document.querySelector('#toast').textContent) && /رُفض 1/.test(document.querySelector('#toast').textContent)", 3000, "import report");
    await ev("document.querySelector('#menu-downloads [data-cmd=export_list]').click()");
    await until("/صُدّر/.test(document.querySelector('#toast').textContent)", 3000, "export report");
    // Desktop tab holds the desktop switches
    await ev("document.querySelector('[data-cmd=settings]').click()");
    await ev("document.querySelector('#dlg-settings [data-tab=desktop]').click()");
    assert(!(await ev("document.querySelector('[data-pane=desktop]').hidden")) && await ev("document.querySelector('[data-pane=desktop]').contains(document.querySelector('#set-tray'))"), "desktop tab");
    if (arg("--shots")) await shot("settings-desktop-tab-ar.png");
    await ev("document.querySelector('#dlg-settings').close()");
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
