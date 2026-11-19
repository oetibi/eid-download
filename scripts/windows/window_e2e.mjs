#!/usr/bin/env node
// v0.2 end-to-end test of the REAL desktop window on Windows, driven through its buttons
// (owner's real-Windows report, 2026-11-19). The WebView2 inside EidDownload.exe is reached
// over the Chrome DevTools Protocol (WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port,
// a test-only switch of WebView2 itself; the app has no such option), so every step below is a
// click or a typed value in the real window, checked against the real result (files, SHA-256,
// the app's own list, the registry). Throw-away profile, own IPC endpoint, local test server.
//
// Part A - the owner's situation: EidDownload.exe ALONE in "%TEMP%\Temp1_<zip>.zip\..." (what
// Explorer does when the app is opened from inside the ZIP):
//   1. the startup bar says "opened from inside a ZIP" and "eid-native-host.exe is missing";
//   2. Add -> a real file from the test server -> done, SHA-256 exact;
//   3. Add -> a YouTube link -> the Video window opens instead (no file queued), its probe runs
//      through the app's own worker and answers "tools missing" (before: os error 2);
//   4. Add -> a link that answers with a web page -> fails with the web-page message, no file;
//   5. a throttled download: select the row, toolbar Pause -> paused, Resume -> done, SHA exact;
//   6. Settings -> Browser extension -> Connect is refused with the "extract the ZIP" message.
//   --online (real-video.yml only: real internet): video tools installed from the window after
//   the agreement, a real PeerTube video probed and downloaded at 360p from the window.
// Part B - an extracted copy (app + eid-native-host + eid side by side):
//   7. no startup bar; Settings -> Connect -> Chrome/Edge/Firefox "connected to this copy";
//      the HKCU keys point at manifests whose program is this copy's eid-native-host.exe;
//   8. that eid-native-host.exe, spoken to exactly as a browser does (Native Messaging framing),
//      reaches the open window: ping answers "gui";
//   9. Disconnect removes the keys.
// Usage: node scripts/windows/window_e2e.mjs [--bin target\release] [--out window-e2e] [--online]
import { spawn, execFileSync } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const arg = (k, d) => { const i = process.argv.indexOf(k); return i > 0 ? process.argv[i + 1] : d; };
const BIN = path.resolve(arg("--bin", "target\\release"));
const OUT = path.resolve(arg("--out", "window-e2e"));
const ONLINE = process.argv.includes("--online");
const PEERTUBE = "https://framatube.org/w/kkGMgK9ZtnKfYAgnEtQxbv"; // "What is PeerTube?", CC BY-SA
fs.mkdirSync(OUT, { recursive: true });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const ROOT = fs.mkdtempSync(path.join(os.tmpdir(), "eid-window-e2e-"));
const PROFILE = path.join(ROOT, "profile");
for (const d of ["AppData\\Roaming", "AppData\\Local", "Downloads"]) fs.mkdirSync(path.join(PROFILE, d), { recursive: true });
const DOWNLOADS = path.join(PROFILE, "Downloads");
const ENDPOINT = "win" + randomBytes(4).toString("hex");
const ENV = { ...process.env, USERPROFILE: PROFILE, APPDATA: path.join(PROFILE, "AppData\\Roaming"), LOCALAPPDATA: path.join(PROFILE, "AppData\\Local") };
const procs = [];
let failed = 0;
const results = [];

function log(...a) { console.log(...a); }
async function check(name, fn) {
  log(`...  ${name}`);
  try { await fn(); results.push({ name, ok: true }); log(`ok   ${name}`); }
  catch (e) { failed++; results.push({ name, ok: false, error: String(e?.stack ?? e) }); log(`FAIL ${name}\n     ${e?.stack ?? e}`); }
}
const assert = (c, m) => { if (!c) throw new Error(m); };
async function until(fn, ms, what) {
  const t0 = Date.now();
  let last;
  while (Date.now() - t0 < ms) {
    // one attempt may never answer (a DevTools call to a busy page): it is bounded too
    const left = Math.max(1000, ms - (Date.now() - t0));
    try {
      last = await Promise.race([fn(), sleep(Math.min(left, 30000)).then(() => { throw new Error("no answer within 30 s"); })]);
      if (last) return last;
    } catch (e) { if (e?.fatal) throw e; last = e; }
    await sleep(250);
  }
  throw new Error(`timed out after ${ms} ms: ${what} (last: ${last?.message ?? JSON.stringify(last)})`);
}
const sha = (f) => createHash("sha256").update(fs.readFileSync(f)).digest("hex");

function eid(...a) {
  try { return execFileSync(path.join(BIN, "eid.exe"), [...a, "--endpoint", ENDPOINT], { env: ENV, encoding: "utf8", windowsHide: true }); }
  catch (e) { return `ERROR ${e.status}: ${e.stdout ?? ""}${e.stderr ?? ""}`; }
}
/** `eid list` line (`#id status link  (error)`) whose link contains `needle`: { id, status, line }. */
function item(needle) {
  const line = eid("list").split(/\r?\n/).find((l) => l.includes(needle));
  const m = line && /^#(\d+)\s+(\w+)/.exec(line.trim());
  return m ? { id: m[1], status: m[2], line } : null;
}

// ---------------------------------------------------------------- the test server
async function testServer() {
  const p = spawn(path.join(BIN, "eid-testserver.exe"), [], { windowsHide: true, stdio: ["ignore", "pipe", "inherit"] });
  procs.push(p);
  let buf = "";
  p.stdout.on("data", (d) => { buf += d; });
  return until(() => /http:\/\/\S+/.exec(buf)?.[0], 30000, "test server URL");
}

// ---------------------------------------------------------------- the app and its WebView2
let port = 9300 + Math.floor(Math.random() * 400);
// The app (Tauri/wry) passes its own WebView2 browser arguments, which take the place of the
// WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS variable (first CI run: no DevTools port opened). The
// WebView2 per-user policy "AdditionalBrowserArguments" (value name = the exe file name) is
// applied over what the app asks for; it is set only for this test and removed at the end.
const POLICIES = ["HKCU", "HKLM"].map((h) => `${h}\\Software\\Policies\\Microsoft\\Edge\\WebView2\\AdditionalBrowserArguments`);
const reg = (...a) => execFileSync("reg", a, { encoding: "utf8", windowsHide: true, stdio: ["ignore", "pipe", "ignore"] });
const policyBefore = POLICIES.map((k) => { try { return /REG_SZ\s+(.+)/.exec(reg("query", k, "/v", "EidDownload.exe"))?.[1].trim() ?? null; } catch { return null; } });
function setPolicy(p) {
  // HKLM needs administrator rights (the CI runner has them); HKCU alone is tried otherwise
  for (const k of POLICIES) { try { reg("add", k, "/v", "EidDownload.exe", "/t", "REG_SZ", "/d", `--remote-debugging-port=${p}`, "/f"); } catch (e) { log(`     (policy ${k}: ${e.message.split("\n")[0]})`); } }
}
function restorePolicy() {
  POLICIES.forEach((k, i) => {
    try {
      if (policyBefore[i]) reg("add", k, "/v", "EidDownload.exe", "/t", "REG_SZ", "/d", policyBefore[i], "/f");
      else reg("delete", k, "/v", "EidDownload.exe", "/f");
    } catch {}
  });
}
/** The command lines of the running WebView2 browser processes (diagnosis). */
function webviewCommandLines() {
  try {
    return execFileSync("powershell", ["-NoProfile", "-Command", "Get-CimInstance Win32_Process -Filter \"Name='msedgewebview2.exe'\" | Where-Object { $_.CommandLine -notmatch '--type=' } | ForEach-Object { $_.CommandLine }"], { encoding: "utf8", windowsHide: true }).trim();
  } catch (e) { return `(${e.message})`; }
}
async function startApp(exe, extraArgs = ["--endpoint", ENDPOINT]) {
  port++;
  setPolicy(port);
  const env = { ...ENV, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}` };
  const p = spawn(exe, extraArgs, { env, stdio: ["ignore", "ignore", "pipe"] });
  procs.push(p);
  let stderr = "";
  p.stderr.on("data", (d) => { stderr += d; });
  const target = await until(async () => {
    if (p.exitCode !== null) throw Object.assign(new Error(`the app exited with code ${p.exitCode}: ${stderr.slice(-2000)}`), { fatal: true });
    const r = await fetch(`http://127.0.0.1:${port}/json/list`);
    const list = await r.json();
    return list.find((t) => t.type === "page" && /tauri|localhost/.test(t.url));
  }, 90000, "the app's WebView2 page over CDP").catch((e) => {
    // say what is known: is the app alive, has WebView2 written its DevTools port anywhere?
    const found = [];
    const walk = (d, depth = 0) => { if (depth > 6) return; for (const x of fs.readdirSync(d, { withFileTypes: true })) { const q = path.join(d, x.name); if (x.isDirectory()) { try { walk(q, depth + 1); } catch {} } else if (x.name === "DevToolsActivePort") found.push(`${q}: ${fs.readFileSync(q, "utf8").split(/\r?\n/)[0]}`); } };
    for (const d of [path.join(PROFILE, "AppData"), process.env.LOCALAPPDATA && path.join(process.env.LOCALAPPDATA, "com.eid.download")]) { try { if (d) walk(d); } catch {} }
    throw new Error(`${e.message}; app ${p.exitCode === null ? "still running" : "exited " + p.exitCode}; DevToolsActivePort: ${found.join(", ") || "none"}; WebView2 browser command lines: ${webviewCommandLines() || "none"}; stderr: ${stderr.slice(-1000)}`);
  });
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
  let seq = 0;
  const pending = new Map();
  const errors = [];
  ws.onmessage = (m) => {
    const d = JSON.parse(m.data);
    if (d.id && pending.has(d.id)) { pending.get(d.id)(d); pending.delete(d.id); }
    if (d.method === "Runtime.exceptionThrown") errors.push(d.params.exceptionDetails.exception?.description ?? d.params.exceptionDetails.text);
  };
  const send = (method, params = {}) => new Promise((res, rej) => {
    const id = ++seq;
    const t = setTimeout(() => { pending.delete(id); rej(new Error(`DevTools ${method}: no answer within 60 s`)); }, 60000);
    pending.set(id, (d) => { clearTimeout(t); res(d); });
    ws.send(JSON.stringify({ id, method, params }));
  });
  await send("Runtime.enable");
  await send("Page.enable");
  const ev = async (expr) => {
    const r = await send("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true });
    if (r.result?.exceptionDetails) throw new Error(`${expr}\n=> ${r.result.exceptionDetails.exception?.description ?? r.result.exceptionDetails.text}`);
    return r.result?.result?.value;
  };
  const shot = async (name) => {
    const r = await send("Page.captureScreenshot", { format: "png" });
    fs.writeFileSync(path.join(OUT, name), Buffer.from(r.result.data, "base64"));
    log(`     screenshot ${name}`);
  };
  await until(() => ev("!!document.querySelector('#viewport') && document.readyState === 'complete'"), 30000, "window loaded");
  return { p, ev, shot, errors, close: () => ws.close() };
}
const js = (s) => JSON.stringify(s);
/** Type `value` into an input the way a user does (value + input event). */
const typeIn = (sel, value) => `(() => { const i = document.querySelector(${js(sel)}); i.value = ${js(value)}; i.dispatchEvent(new Event('input', { bubbles: true })); })()`;
const click = (sel) => `document.querySelector(${js(sel)}).click()`;

async function addLink(w, link, name) {
  await w.ev(click("#btn-add"));
  await until(() => w.ev("document.querySelector('#dlg-add').open"), 5000, "add dialog");
  await w.ev(typeIn("#in-links", link));
  await w.ev(typeIn("#in-name", name ?? ""));
  await w.ev(click("#btn-start"));
}

async function stopApp(w) {
  w?.close();
  eid("stop");
  if (w && w.p.exitCode === null) await until(() => w.p.exitCode !== null, 30000, "app exit after eid stop");
}

// ---------------------------------------------------------------- run
const base = await testServer();
log(`test server: ${base}`);
const expected = createHash("sha256").update(Buffer.from(await (await fetch(`${base}/demo.bin`)).arrayBuffer())).digest("hex");
let w = null;
try {
  // ===== Part A: the app alone in Explorer's "opened from inside the ZIP" folder
  const zipDir = path.join(ROOT, "Temp1_EidDownload-windows-x64.zip", "EidDownload-windows-x64");
  fs.mkdirSync(zipDir, { recursive: true });
  const lone = path.join(zipDir, "EidDownload.exe");
  fs.copyFileSync(path.join(BIN, "EidDownload.exe"), lone);
  assert(fs.readdirSync(zipDir).length === 1, "only the app in its folder");
  w = await startApp(lone);
  log(`window up (CDP port ${port}), app alone in ${zipDir}`);

  await check("1. startup bar: opened from inside a ZIP, eid-native-host.exe missing", async () => {
    await until(() => w.ev("!document.querySelector('#checkbar').hidden"), 10000, "check bar");
    const codes = await w.ev("[...document.querySelectorAll('#check-list li')].map((l) => l.dataset.code)");
    assert(JSON.stringify(codes) === '["from_zip","no_native_host"]', JSON.stringify(codes));
    await w.shot("A1-zip-warning.png");
    await w.ev(click("#check-close"));
  });

  await check("2. Add -> a real file -> done, SHA-256 exact", async () => {
    await addLink(w, `${base}/demo.bin`, "w-demo.bin");
    await until(() => item("/demo.bin")?.status === "done", 120000, "demo.bin done");
    assert(sha(path.join(DOWNLOADS, "w-demo.bin")) === expected, "SHA-256");
  });

  await check("3. Add -> YouTube link -> Video window; its probe runs through the app itself (no os error 2)", async () => {
    const yt = "https://www.youtube.com/watch?v=aqz-KE-bpKQ";
    await addLink(w, yt);
    await until(() => w.ev("document.querySelector('#dlg-video').open && !document.querySelector('#dlg-add').open"), 5000, "video window instead");
    assert((await w.ev("document.querySelector('#in-vurl').value")) === yt, "link carried over");
    await until(() => w.ev("!document.querySelector('#v-tools').hidden"), 60000, "'tools missing' box (the probe ran)");
    const text = await w.ev("document.querySelector('#dlg-video').innerText");
    assert(!/os error|cannot find the file|cannot start/i.test(text), text);
    await w.shot("A3-youtube-to-video-window.png");
    if (!ONLINE) await w.ev(click("#v-cancel"));
    assert(!item("youtube"), "the YouTube page reached the file list: " + eid("list"));
  });

  if (ONLINE) {
    await check("online: tools installed from the window after the agreement, a real video downloaded at 360p", async () => {
      await w.ev(click("#v-install"));
      await until(() => w.ev("document.querySelector('#dlg-consent').open && document.querySelector('#c-text').textContent.length > 20"), 30000, "agreement");
      await w.ev("(() => { const c = document.querySelector('#c-agree'); c.checked = true; c.dispatchEvent(new Event('change')); })()");
      await w.ev(click("#c-install"));
      // the tool store: <LOCALAPPDATA>\EidDownload\tools\current.json names the installed set
      const tools = path.join(PROFILE, "AppData\\Local\\EidDownload\\tools");
      const status = () => w.ev("document.querySelector('#dlg-video').innerText").catch((e) => String(e));
      try {
        await until(() => fs.existsSync(path.join(tools, "current.json")), 900000, "tools installed (current.json)");
      } catch (e) { throw new Error(`${e.message}; the window says: ${await status()}`); }
      const all = [];
      const walk = (d) => { for (const x of fs.readdirSync(d, { withFileTypes: true })) { const q = path.join(d, x.name); if (x.isDirectory()) walk(q); else all.push(path.relative(tools, q)); } };
      walk(tools);
      log(`     tools: ${all.join(", ")}`);
      for (const f of ["yt-dlp.exe", "ffmpeg.exe", "deno.exe"]) assert(all.some((x) => x.endsWith(f)), `${f} missing: ${all}`);
      await w.ev(typeIn("#in-vurl", PEERTUBE));
      await w.ev(click("#btn-probe"));
      await until(() => w.ev("!document.querySelector('#v-result').hidden"), 180000, "real probe result");
      await w.ev("(() => { const q = document.querySelector('#v-quality'); q.value = '360p'; q.dispatchEvent(new Event('change')); })()");
      await w.shot("A-online-probe.png");
      await w.ev(click("#v-start"));
      const mp4 = await until(() => fs.readdirSync(DOWNLOADS).find((f) => f.endsWith(".mp4") && fs.statSync(path.join(DOWNLOADS, f)).size > 100000), 900000, "the video file");
      log(`     video: ${mp4} (${fs.statSync(path.join(DOWNLOADS, mp4)).size} bytes)`);
      await w.shot("A-online-done.png");
    });
  }

  await check("4. Add -> a link that answers with a web page -> refused with the message, no file", async () => {
    await addLink(w, `${base}/watch`);
    const it = await until(() => { const i = item("/watch"); return i && i.status === "failed" ? i : null; }, 60000, "the page link fails");
    assert(/web page/i.test(it.line) || /web page/i.test(eid("list")), it.line);
    assert(!fs.existsSync(path.join(DOWNLOADS, "watch")), "a file 'watch' was written");
  });

  await check("5. toolbar Pause and Resume on a selected download; SHA-256 exact", async () => {
    await addLink(w, `${base}/slow.bin`, "w-slow.bin");
    const it = await until(() => { const i = item("/slow.bin"); return i && i.status === "running" ? i : null; }, 60000, "slow.bin running");
    await sleep(3000);
    // the downloads list (after a video the window shows its video jobs)
    await w.ev(click("[data-view=all]"));
    await until(() => w.ev(`!!document.querySelector('.tr[data-id="${it.id}"]')`), 10000, "its row");
    await w.ev(`document.querySelector('.tr[data-id="${it.id}"]').dispatchEvent(new MouseEvent('click', { bubbles: true }))`);
    await until(() => w.ev("!document.querySelector('.tool[data-cmd=pause]').disabled"), 5000, "Pause enabled for the selection");
    await w.ev(click(".tool[data-cmd=pause]"));
    await until(() => item("/slow.bin")?.status === "paused", 30000, "paused");
    await w.shot("A5-paused.png");
    await until(() => w.ev("!document.querySelector('.tool[data-cmd=resume]').disabled"), 5000, "Resume enabled");
    await w.ev(click(".tool[data-cmd=resume]"));
    await until(() => item("/slow.bin")?.status === "done", 240000, "done after resume");
    assert(sha(path.join(DOWNLOADS, "w-slow.bin")) === expected, "SHA-256 after pause/resume");
  });

  await check("6. Settings -> Connect the browsers is refused here with the 'extract the ZIP' message", async () => {
    await w.ev(click("[data-cmd=settings]"));
    await w.ev(click("#dlg-settings [data-tab=desktop]"));
    await until(() => w.ev("document.querySelectorAll('#browser-list li').length === 3"), 10000, "browser list");
    await w.ev(click("#browser-connect"));
    await until(() => w.ev("!document.querySelector('#browser-error').hidden"), 10000, "refusal");
    const t = await w.ev("document.querySelector('#browser-error').textContent");
    assert(/ZIP/.test(t), t);
    await w.shot("A6-connect-refused.png");
    await w.ev("document.querySelector('#dlg-settings').close()");
  });
  assert(w.errors.length === 0, "script errors in the window: " + w.errors.join(" | "));
  await stopApp(w);
  w = null;

  // ===== Part B: an extracted copy
  const dir = path.join(ROOT, "Eid Download portable");
  fs.mkdirSync(dir, { recursive: true });
  for (const f of ["EidDownload.exe", "eid-native-host.exe", "eid.exe"]) fs.copyFileSync(path.join(BIN, f), path.join(dir, f));
  w = await startApp(path.join(dir, "EidDownload.exe"));
  const KEYS = { chrome: "HKCU\\Software\\Google\\Chrome\\NativeMessagingHosts\\com.eid.download", edge: "HKCU\\Software\\Microsoft\\Edge\\NativeMessagingHosts\\com.eid.download", firefox: "HKCU\\Software\\Mozilla\\NativeMessagingHosts\\com.eid.download" };
  const regValue = (k) => { try { return /REG_SZ\s+(.+)/.exec(execFileSync("reg", ["query", k, "/ve"], { encoding: "utf8", windowsHide: true }))?.[1].trim() ?? null; } catch { return null; } };
  const saved = Object.fromEntries(Object.entries(KEYS).map(([b, k]) => [b, regValue(k)]));

  await check("7. extracted copy: no startup bar; Connect -> all three browsers point at this copy", async () => {
    await sleep(3000);
    assert(await w.ev("document.querySelector('#checkbar').hidden"), "startup bar shown for an extracted copy");
    await w.ev(click("[data-cmd=settings]"));
    await w.ev(click("#dlg-settings [data-tab=desktop]"));
    await w.ev(click("#browser-connect"));
    await until(() => w.ev("[...document.querySelectorAll('#browser-list li')].every((l) => l.dataset.state === 'this')"), 10000, "connected");
    await w.shot("B7-connected.png");
    for (const [b, k] of Object.entries(KEYS)) {
      const m = regValue(k);
      assert(m && fs.existsSync(m), `${b}: key ${k} = ${m}`);
      const host = JSON.parse(fs.readFileSync(m, "utf8")).path;
      assert(path.resolve(host).toLowerCase() === path.join(dir, "eid-native-host.exe").toLowerCase(), `${b}: ${host}`);
    }
  });

  await check("8. the registered eid-native-host.exe, spoken to as a browser does, reaches the window", async () => {
    const m = regValue(KEYS.chrome);
    const host = JSON.parse(fs.readFileSync(m, "utf8")).path;
    const p = spawn(host, ["chrome-extension://ambokeecedpgicighhcpdfjphdnphgaa/"], { env: { ...ENV, EID_IPC_ENDPOINT: ENDPOINT }, windowsHide: true });
    procs.push(p);
    const body = Buffer.from(JSON.stringify({ v: 1, id: "ext-e2e1", command: { cmd: "ping" } }));
    const len = Buffer.alloc(4); len.writeUInt32LE(body.length);
    let out = Buffer.alloc(0);
    p.stdout.on("data", (d) => { out = Buffer.concat([out, d]); });
    p.stdin.write(Buffer.concat([len, body]));
    const resp = await until(() => { if (out.length < 4) return null; const n = out.readUInt32LE(0); return out.length >= 4 + n ? JSON.parse(out.subarray(4, 4 + n).toString("utf8")) : null; }, 30000, "native host answer");
    p.stdin.end();
    log(`     native host answered: ${JSON.stringify(resp)}`);
    assert(resp.ok === true && resp.result?.app === "gui", JSON.stringify(resp));
  });

  await check("9. Disconnect removes this copy's registration", async () => {
    await w.ev(click("#browser-disconnect"));
    await until(() => w.ev("[...document.querySelectorAll('#browser-list li')].every((l) => l.dataset.state === 'none')"), 10000, "disconnected");
    for (const k of Object.values(KEYS)) assert(regValue(k) === null, `${k} still there`);
  });
  // put back what the runner had before (normally nothing)
  for (const [b, v] of Object.entries(saved)) if (v) execFileSync("reg", ["add", KEYS[b], "/ve", "/d", v, "/f"], { windowsHide: true });
  assert(w.errors.length === 0, "script errors in the window: " + w.errors.join(" | "));
  await stopApp(w);
  w = null;
} catch (e) {
  failed++;
  log(`FAIL (setup) ${e?.stack ?? e}`);
} finally {
  try { await w?.shot?.("final.png"); } catch {}
  if (w) await stopApp(w).catch(() => {});
  for (const p of procs) { try { if (p.exitCode === null) p.kill(); } catch {} }
  restorePolicy();
  fs.writeFileSync(path.join(OUT, "window-e2e.json"), JSON.stringify(results, null, 1));
  await sleep(1000);
  try { fs.rmSync(ROOT, { recursive: true, force: true }); } catch {}
}
log(failed ? `WINDOW E2E: ${failed} check(s) FAILED` : `WINDOW E2E: all ${results.length} checks passed`);
process.exit(failed ? 1 : 0);
