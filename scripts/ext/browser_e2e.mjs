#!/usr/bin/env node
// End-to-end test of the browser extension in REAL browsers: Chrome / Edge (Chromium, driven over
// the DevTools protocol) and Firefox (driven over WebDriver BiDi through geckodriver). Zero npm
// dependencies (Node >= 22: fetch + WebSocket).
//
// Owner's report (2026-11-20, Firefox 160 on Windows): the extension loaded, "Test connection"
// said "Eid Download answered", but the "Download this video" button never appeared over
// YouTube videos. This test proves the button in each browser, on stand-in pages served AT the
// real YouTube addresses (the browser's own request interception answers them: nothing is sent
// to YouTube), and the whole chain to a stand-in native host (scripts/ext/stub-host.mjs, which
// records what the extension sent):
//   1. watch?v=… : hovering the player shows the button at the top-right of the player;
//   2. its menu asks the host (probe) and lists the qualities; a choice sends add_video with the
//      canonical link;
//   3. YouTube's in-page navigation (history.pushState + yt-navigate-finish, no reload) to a
//      watch?v=…&list=… video: the button follows, the menu shows the playlist (title, 120
//      episodes), "choose" lists them, and a selection sends add_playlist with those items;
//   4. in-page navigation to the home page hides the button; Back (popstate) brings it back;
//   5. the options page: "always" + "top-left" + Save are applied to the page without hover.
//
//   node scripts/ext/browser_e2e.mjs --browser chrome|edge|brave|opera|vivaldi|firefox [--bin PATH]
//        [--nm-key HKCU\\...\\NativeMessagingHosts]   (Windows: the ONE registry key the stand-in
//        host is registered under; default: that browser's known key)
//        [--checks 1,2]   (only these checks; the others are printed as NOT RUN, never as passed)
import { spawn, execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { zip } from "./package.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const arg = (k, d) => { const i = process.argv.indexOf(k); return i > 0 ? process.argv[i + 1] : d; };
const BROWSER = arg("--browser", "chrome");
const CHROMIUM = ["chrome", "edge", "brave", "opera", "vivaldi"];
if (BROWSER !== "firefox" && !CHROMIUM.includes(BROWSER)) throw new Error(`unknown --browser ${BROWSER}`);
const ONLY = arg("--checks") ? new Set(arg("--checks").split(",").map((x) => x.trim())) : null;
// Windows: where each browser reads its Native Messaging hosts (HKCU). Opera and Vivaldi have no
// documented key: CI tries candidates one at a time with --nm-key (see ci-windows.yml).
const NM_KEYS = {
  firefox: "HKCU\\Software\\Mozilla\\NativeMessagingHosts",
  chrome: "HKCU\\Software\\Google\\Chrome\\NativeMessagingHosts",
  edge: "HKCU\\Software\\Microsoft\\Edge\\NativeMessagingHosts",
  brave: "HKCU\\Software\\BraveSoftware\\Brave-Browser\\NativeMessagingHosts",
};
const IS_WIN = process.platform === "win32";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const CHROME_ID = "ambokeecedpgicighhcpdfjphdnphgaa";
const FIREFOX_ID = "eid-download@eid-download";
const TMP = fs.mkdtempSync(path.join(os.tmpdir(), "eid-browser-e2e-"));
const STUB_LOG = path.join(TMP, "host-requests.jsonl");
const OUT = path.resolve(arg("--out", path.join(ROOT, "browser-e2e")));
fs.mkdirSync(OUT, { recursive: true });

// ---------------------------------------------------------------- the extension build
execFileSync(process.execPath, [path.join(ROOT, "scripts", "ext", "build.mjs"), "--out", path.join(TMP, "ext")], { stdio: "ignore" });
const EXT_DIR = path.join(TMP, "ext", BROWSER === "firefox" ? "firefox" : "chrome");

// ---------------------------------------------------------------- the stand-in native host
function registerHost() {
  const stub = path.join(ROOT, "scripts", "ext", "stub-host.mjs");
  let exe;
  if (IS_WIN) {
    exe = path.join(TMP, "stub-host.bat");
    fs.writeFileSync(exe, `@echo off\r\n"${process.execPath}" "${stub}" %*\r\n`);
  } else {
    exe = path.join(TMP, "stub-host.sh");
    fs.writeFileSync(exe, `#!/bin/sh\nexec "${process.execPath}" "${stub}" "$@"\n`, { mode: 0o755 });
  }
  const m = { name: "com.eid.download", description: "Eid Download (test stand-in)", path: exe, type: "stdio" };
  if (BROWSER === "firefox") m.allowed_extensions = [FIREFOX_ID];
  else m.allowed_origins = [`chrome-extension://${CHROME_ID}/`];
  const manifest = path.join(TMP, "com.eid.download.json");
  fs.writeFileSync(manifest, JSON.stringify(m));
  const restore = [];
  if (IS_WIN) {
    const base = arg("--nm-key", NM_KEYS[BROWSER]);
    if (!base) throw new Error(`--nm-key is required for ${BROWSER} on Windows (no known key)`);
    const key = `${base}\\com.eid.download`;
    console.log(`     native host registered ONLY under ${key}`);
    let before = null;
    try { before = /REG_SZ\s+(.+)/.exec(execFileSync("reg", ["query", key, "/ve"], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }))?.[1].trim() ?? null; } catch {}
    execFileSync("reg", ["add", key, "/ve", "/d", manifest, "/f"], { stdio: "ignore" });
    restore.push(() => { try { if (before) execFileSync("reg", ["add", key, "/ve", "/d", before, "/f"], { stdio: "ignore" }); else execFileSync("reg", ["delete", key, "/f"], { stdio: "ignore" }); } catch {} });
  } else if (BROWSER === "firefox") {
    // Firefox reads ~/.mozilla/native-messaging-hosts: HOME is this test's own folder
    const d = path.join(TMP, "home", ".mozilla", "native-messaging-hosts");
    fs.mkdirSync(d, { recursive: true });
    fs.copyFileSync(manifest, path.join(d, "com.eid.download.json"));
  } else {
    // Chromium-based browsers on Linux read <user-data-dir>/NativeMessagingHosts
    const d = path.join(TMP, "profile", "NativeMessagingHosts");
    fs.mkdirSync(d, { recursive: true });
    fs.copyFileSync(manifest, path.join(d, "com.eid.download.json"));
  }
  return () => restore.forEach((f) => f());
}
const hostRequests = () => (fs.existsSync(STUB_LOG) ? fs.readFileSync(STUB_LOG, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l)) : []);

// ---------------------------------------------------------------- the stand-in YouTube page
// A YouTube-like watch page: #movie_player with a 854x480 <video>, and the in-page navigation
// YouTube uses (pushState + yt-navigate-finish on the document, no reload).
function ytPage(title) {
  return `<!doctype html><html><head><meta charset="utf-8"><title>${title} - YouTube</title></head>
<body style="margin:0;background:#0f0f0f;color:#fff;font:14px sans-serif">
<div id="masthead" style="height:56px">YouTube (stand-in)</div>
<div id="movie_player" class="html5-video-player" style="position:relative;width:854px;height:480px;margin:0 0 0 24px">
  <video class="html5-main-video" width="854" height="480" style="display:block;background:#000"></video>
</div>
<script>
  // YouTube's single-page navigation: the address changes, the page does not reload
  window.__ytGo = (url) => {
    history.pushState({}, "", url);
    document.title = (new URL(location.href).searchParams.get("v") || "Home") + " - YouTube";
    document.dispatchEvent(new CustomEvent("yt-navigate-finish", { bubbles: true }));
  };
  window.addEventListener("popstate", () => document.dispatchEvent(new CustomEvent("yt-navigate-finish", { bubbles: true })));
</script>
</body></html>`;
}

// ---------------------------------------------------------------- drivers
/**
 * Common driver interface:
 *   open(url)                full navigation (served by interception for www.youtube.com)
 *   evaluate(expr)           JSON-serialisable result of an expression in the page
 *   hover(x, y)              real pointer move
 *   overlay()                {buttons:[{tag,attrs,text,ref}], texts:[]} inside the closed shadow root
 *   click(ref)               el.click() on a node of the overlay
 *   options(fn)              run `fn` (a function source) in the extension's options page
 */
async function chromium(kind) {
  const DEFAULT_BIN = {
    chrome: IS_WIN ? "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe" : "google-chrome",
    edge: IS_WIN ? "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe" : "microsoft-edge",
    brave: IS_WIN ? "C:\\Program Files\\BraveSoftware\\Brave-Browser\\Application\\brave.exe" : "brave-browser",
    opera: "opera",
    vivaldi: "vivaldi",
  };
  const bin = arg("--bin", DEFAULT_BIN[kind]);
  const prof = path.join(TMP, "profile");
  fs.mkdirSync(prof, { recursive: true });
  const proc = spawn(bin, ["--headless=new", "--no-sandbox", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
    "--remote-debugging-pipe", "--enable-unsafe-extension-debugging", `--user-data-dir=${prof}`, "--window-size=1280,900", "about:blank"],
  { stdio: ["ignore", "ignore", "pipe", "pipe", "pipe"], env: { ...process.env, EID_STUB_LOG: STUB_LOG } });
  let err = ""; proc.stderr.on("data", (d) => { err += d; });
  const w = proc.stdio[3], r = proc.stdio[4];
  let seq = 0, buf = Buffer.alloc(0);
  const pending = new Map(), hooks = [];
  r.on("data", (d) => {
    buf = Buffer.concat([buf, d]);
    let i;
    while ((i = buf.indexOf(0)) >= 0) {
      const m = JSON.parse(buf.subarray(0, i).toString("utf8")); buf = buf.subarray(i + 1);
      if (m.id && pending.has(m.id)) { const p = pending.get(m.id); pending.delete(m.id); m.error ? p.rej(new Error(JSON.stringify(m.error))) : p.res(m.result); }
      else for (const h of hooks) h(m);
    }
  });
  const send = (method, params = {}, sessionId) => new Promise((res, rej) => { const id = ++seq; pending.set(id, { res, rej }); w.write(JSON.stringify({ id, method, params, sessionId }) + "\0"); });
  const { id } = await send("Extensions.loadUnpacked", { path: EXT_DIR });
  if (id !== CHROME_ID) throw new Error(`extension id ${id}`);
  const attach = async (url) => {
    const { targetId } = await send("Target.createTarget", { url: "about:blank" });
    const { sessionId } = await send("Target.attachToTarget", { targetId, flatten: true });
    const S = (m, p) => send(m, p, sessionId);
    S.targetId = targetId;
    await S("Page.enable"); await S("Runtime.enable"); await S("DOM.enable");
    if (url) await S("Page.navigate", { url });
    return S;
  };
  const S = await attach();
  // answer www.youtube.com from here (nothing goes to YouTube)
  hooks.push((m) => {
    if (m.method !== "Fetch.requestPaused") return;
    const u = new URL(m.params.request.url);
    const doc = m.params.resourceType === "Document";
    const body = doc ? ytPage(u.searchParams.get("v") || "Home") : "";
    send("Fetch.fulfillRequest", { requestId: m.params.requestId, responseCode: doc ? 200 : 404,
      responseHeaders: [{ name: "content-type", value: "text/html; charset=utf-8" }], body: Buffer.from(body).toString("base64") }, m.sessionId).catch(() => {});
  });
  await S("Fetch.enable", { patterns: [{ urlPattern: "https://www.youtube.com/*", requestStage: "Request" }] });
  const evaluate = async (expr, s = S) => {
    const res = await s("Runtime.evaluate", { expression: `(async () => JSON.stringify(await (${expr})))()`, returnByValue: true, awaitPromise: true });
    if (res.exceptionDetails) throw new Error(res.exceptionDetails.exception?.description ?? res.exceptionDetails.text);
    return res.result.value === undefined ? undefined : JSON.parse(res.result.value);
  };
  return {
    name: kind,
    async open(url) {
      await S("Page.navigate", { url });
      for (let i = 0; i < 50; i++) { await sleep(100); try { if ((await evaluate("document.readyState")) === "complete") break; } catch {} }
      await sleep(500);
    },
    evaluate,
    async hover(x, y) { await S("Input.dispatchMouseEvent", { type: "mouseMoved", x, y }); await sleep(400); },
    async overlay() {
      const { root } = await S("DOM.getDocument", { depth: -1, pierce: true });
      const out = { buttons: [], texts: [], host: false };
      const textOf = (n) => (n.nodeType === 3 ? n.nodeValue : "") + (n.children ?? []).map(textOf).join("");
      const walk = (n, inside) => {
        for (const s of n.shadowRoots ?? []) if (s.shadowRootType === "closed" && n.nodeName === "EID-DOWNLOAD-OVERLAY") { out.host = true; walk(s, true); }
        if (inside && n.nodeType === 1 && n.nodeName !== "STYLE") {
          const a = {}; const l = n.attributes ?? []; for (let i = 0; i < l.length; i += 2) a[l[i]] = l[i + 1];
          if (n.nodeName === "BUTTON" || n.nodeName === "INPUT") out.buttons.push({ tag: n.nodeName.toLowerCase(), attrs: a, text: textOf(n).trim(), ref: n.backendNodeId });
        }
        if (inside && n.nodeType === 3 && n.nodeValue.trim()) out.texts.push(n.nodeValue.trim());
        if (n.nodeName === "STYLE") return;
        for (const c of n.children ?? []) walk(c, inside);
      };
      walk(root, false);
      return out;
    },
    async click(ref) {
      const { object } = await S("DOM.resolveNode", { backendNodeId: ref });
      await S("Runtime.callFunctionOn", { objectId: object.objectId, functionDeclaration: "function () { this.click(); }" });
      await sleep(300);
    },
    async options(fnSource) {
      const O = await attach(`chrome-extension://${CHROME_ID}/options.html`);
      for (let i = 0; i < 50; i++) { await sleep(100); try { if ((await evaluate("document.readyState", O)) === "complete") break; } catch {} }
      await sleep(500);
      const v = await evaluate(`(${fnSource})()`, O);
      // back to the page tab: a background tab gets no animation frames (the button is drawn in one)
      await send("Target.closeTarget", { targetId: O.targetId });
      await send("Target.activateTarget", { targetId: S.targetId });
      return v;
    },
    async close() { try { proc.kill("SIGKILL"); } catch {} },
    stderr: () => err,
  };
}

async function firefox() {
  const gdDir = process.env.GECKOWEBDRIVER;
  const gd = arg("--geckodriver", gdDir ? path.join(gdDir, IS_WIN ? "geckodriver.exe" : "geckodriver") : "geckodriver");
  const port = 4444 + Math.floor(Math.random() * 1000);
  // MOZ_REMOTE_ALLOW_SYSTEM_ACCESS: lets the test open the options tab from the browser's own
  // context (run 44717763588); the page under test runs as an ordinary web page either way
  const env = { ...process.env, EID_STUB_LOG: STUB_LOG, MOZ_REMOTE_ALLOW_SYSTEM_ACCESS: "1" };
  if (!IS_WIN) env.HOME = path.join(TMP, "home");
  const proc = spawn(gd, ["--port", String(port)], { stdio: ["ignore", "pipe", "pipe"], env });
  let err = ""; proc.stderr.on("data", (d) => { err += d; }); proc.stdout.on("data", (d) => { err += d; });
  const http = async (method, p, body) => {
    const r = await fetch(`http://127.0.0.1:${port}${p}`, { method, headers: { "content-type": "application/json" }, body: body ? JSON.stringify(body) : undefined });
    const j = await r.json();
    if (j.value?.error) throw new Error(`${p}: ${j.value.error}: ${j.value.message}`);
    return j.value;
  };
  for (let i = 0; i < 100; i++) { try { await http("GET", "/status"); break; } catch { await sleep(200); } }
  const uuid = randomUUID();
  const prefs = {
    "extensions.webextensions.uuids": JSON.stringify({ [FIREFOX_ID]: uuid }),
    "dom.security.https_first": false,
    "browser.shell.checkDefaultBrowser": false,
  };
  const opts = { args: ["-headless"], prefs };
  if (arg("--bin")) opts.binary = arg("--bin");
  const sess = await http("POST", "/session", { capabilities: { alwaysMatch: { browserName: "firefox", webSocketUrl: true, acceptInsecureCerts: true, "moz:firefoxOptions": opts } } });
  const sid = sess.sessionId;
  console.log(`     Firefox ${sess.capabilities.browserVersion} (${sess.capabilities.platformName})`);
  // a temporary add-on, exactly like about:debugging > "Load Temporary Add-on"
  const xpi = path.join(TMP, "eid-download.xpi");
  const files = []; const walk = (d, b = d) => { for (const e of fs.readdirSync(d, { withFileTypes: true })) { const p = path.join(d, e.name); if (e.isDirectory()) walk(p, b); else files.push(path.relative(b, p).split(path.sep).join("/")); } };
  walk(EXT_DIR);
  fs.writeFileSync(xpi, zip(files.sort().map((n) => ({ name: n, data: fs.readFileSync(path.join(EXT_DIR, n)) }))));
  const addonId = await http("POST", `/session/${sid}/moz/addon/install`, { path: xpi, temporary: true });
  if (addonId !== FIREFOX_ID) throw new Error(`add-on id ${addonId}`);
  // WebDriver BiDi
  const ws = new WebSocket(sess.capabilities.webSocketUrl);
  await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
  let seq = 0; const pending = new Map(); const logs = [];
  const bidi = (method, params = {}) => new Promise((res, rej) => { const id = ++seq; pending.set(id, { res, rej }); ws.send(JSON.stringify({ id, method, params })); });
  ws.onmessage = (ev) => {
    const m = JSON.parse(ev.data);
    if (m.id && pending.has(m.id)) { const p = pending.get(m.id); pending.delete(m.id); m.type === "error" ? p.rej(new Error(`${m.error}: ${m.message}`)) : p.res(m.result); return; }
    if (m.method === "network.beforeRequestSent" && m.params.isBlocked) {
      const u = new URL(m.params.request.url);
      const doc = m.params.navigation != null || /^\/(watch|playlist|$)/.test(u.pathname);
      const body = doc ? ytPage(u.searchParams.get("v") || "Home") : "";
      bidi("network.provideResponse", { request: m.params.request.request, statusCode: doc ? 200 : 404, reasonPhrase: doc ? "OK" : "Not Found",
        headers: [{ name: "content-type", value: { type: "string", value: "text/html; charset=utf-8" } }], body: { type: "string", value: body } }).catch((e) => logs.push(String(e)));
    }
    if (m.method === "log.entryAdded") logs.push(`${m.params.level}: ${m.params.text}`);
  };
  const { contexts } = await bidi("browsingContext.getTree", {});
  let context = contexts[0].context;
  await bidi("session.subscribe", { events: ["network.beforeRequestSent", "log.entryAdded"] });
  await bidi("network.addIntercept", { phases: ["beforeRequestSent"], urlPatterns: [{ type: "pattern", protocol: "https", hostname: "www.youtube.com" }] });
  const evaluate = async (expr, ctx = context) => {
    const r = await bidi("script.evaluate", { expression: `(async () => JSON.stringify(await (${expr})))()`, target: { context: ctx }, awaitPromise: true });
    if (r.type === "exception") throw new Error(r.exceptionDetails.text);
    return r.result.value === undefined ? undefined : JSON.parse(r.result.value);
  };
  return {
    name: "firefox",
    logs,
    async open(url) { await bidi("browsingContext.navigate", { context, url, wait: "complete" }); await sleep(800); },
    evaluate,
    async hover(x, y) {
      await bidi("input.performActions", { context, actions: [{ type: "pointer", id: "mouse", parameters: { pointerType: "mouse" }, actions: [{ type: "pointerMove", x: Math.round(x), y: Math.round(y), duration: 0 }] }] });
      await sleep(400);
    },
    async overlay() {
      const r = await bidi("script.callFunction", { functionDeclaration: "function () { return document.querySelector('eid-download-overlay'); }", target: { context }, awaitPromise: false,
        serializationOptions: { maxDomDepth: 40, includeShadowTree: "all" } });
      const out = { buttons: [], texts: [], host: false };
      const host = r.result;
      if (!host || host.type !== "node") return out;
      out.host = true;
      const textOf = (n) => (n.value?.nodeType === 3 ? n.value.nodeValue : "") + (n.value?.children ?? []).map(textOf).join("");
      const walk = (n) => {
        const v = n.value ?? {};
        if (v.nodeType === 1 && v.localName === "style") return;
        if (v.nodeType === 1 && (v.localName === "button" || v.localName === "input")) out.buttons.push({ tag: v.localName, attrs: v.attributes ?? {}, text: textOf(n).trim(), ref: n.sharedId });
        if (v.nodeType === 3 && v.nodeValue?.trim()) out.texts.push(v.nodeValue.trim());
        for (const c of v.children ?? []) walk(c);
      };
      if (host.value?.shadowRoot) walk(host.value.shadowRoot);
      return out;
    },
    async click(ref) {
      await bidi("script.callFunction", { functionDeclaration: "function (el) { el.click(); }", arguments: [{ sharedId: ref }], target: { context }, awaitPromise: false });
      await sleep(300);
    },
    async options(fnSource) {
      // WebDriver BiDi refuses to navigate a tab to moz-extension: (run 44717375680), so the tab
      // is opened from the browser's own (chrome) context through Marionette, like a user would
      const url = `moz-extension://${uuid}/options.html`;
      await http("POST", `/session/${sid}/moz/context`, { context: "chrome" });
      try {
        await http("POST", `/session/${sid}/execute/sync`, { script: "const w = Services.wm.getMostRecentWindow('navigator:browser'); w.gBrowser.selectedTab = w.gBrowser.addTab(arguments[0], { triggeringPrincipal: Services.scriptSecurityManager.getSystemPrincipal() }); return true;", args: [url] });
      } finally {
        await http("POST", `/session/${sid}/moz/context`, { context: "content" });
      }
      const oc = await until(async () => {
        const { contexts: cs } = await bidi("browsingContext.getTree", {});
        return cs.find((c) => c.url === url)?.context;
      }, 10000, "the options tab");
      await sleep(1000);
      const v = await evaluate(`(${fnSource})()`, oc);
      await bidi("browsingContext.close", { context: oc });
      await bidi("browsingContext.activate", { context });
      return v;
    },
    async close() { try { await http("DELETE", `/session/${sid}`); } catch {} try { proc.kill(); } catch {} },
    stderr: () => err,
  };
}

// ---------------------------------------------------------------- the scenario
let failed = 0;
const results = [];
async function check(name, fn) {
  if (ONLY && !ONLY.has(name.split(".")[0])) { console.log(`NOT RUN ${name} (--checks)`); return; }
  console.log(`...  ${name}`);
  try { await fn(); results.push({ name, ok: true }); console.log(`ok   ${name}`); }
  catch (e) { failed++; results.push({ name, ok: false, error: String(e?.stack ?? e) }); console.log(`FAIL ${name}\n     ${e?.stack ?? e}`); }
}
const assert = (c, m) => { if (!c) throw new Error(m); };
async function until(fn, ms, what) {
  const t0 = Date.now(); let last;
  while (Date.now() - t0 < ms) { try { last = await fn(); if (last) return last; } catch (e) { last = e; } await sleep(200); }
  throw new Error(`timed out (${ms} ms): ${what}; last: ${last?.message ?? JSON.stringify(last)?.slice(0, 600)}`);
}

const restoreHost = registerHost();
let B = null;
try {
  B = BROWSER === "firefox" ? await firefox() : await chromium(BROWSER);
  const player = () => B.evaluate("(() => { const r = document.querySelector('video').getBoundingClientRect(); return { x: r.x, y: r.y, w: r.width, h: r.height }; })()");
  const hoverPlayer = async () => { const p = await player(); await B.hover(p.x + p.w / 2, p.y + p.h / 2); return p; };
  const mainButton = (o) => o.buttons.find((b) => /\bbtn\b/.test(b.attrs.class ?? ""));
  const shownButton = async () => { const o = await B.overlay(); const b = mainButton(o); return b && /\bshow\b/.test(b.attrs.class ?? "") ? { o, b } : null; };
  const leftOf = (b) => Number(/left:\s*(-?\d+)/.exec(b.attrs.style ?? "")?.[1]);

  await check("1. YouTube watch page: hovering the player shows the button at its top-right", async () => {
    await B.open("https://www.youtube.com/watch?v=AAAAAAAAAAA");
    const p = await hoverPlayer();
    const { b } = await until(shownButton, 8000, "the button over the player");
    const left = leftOf(b);
    assert(left > p.x + p.w / 2 && left < p.x + p.w, `button left ${left} is not at the top-right of the player ${JSON.stringify(p)}`);
  });

  await check("2. its menu asks the app (probe) and lists the qualities; 720p sends add_video with the canonical link", async () => {
    const { b } = await until(shownButton, 5000, "button");
    await B.click(b.ref);
    const q720 = await until(async () => (await B.overlay()).buttons.find((x) => x.attrs["data-q"] === "720p"), 15000, "the 720p choice (probe through the native host)");
    const offered = (await B.overlay()).buttons.map((x) => x.attrs["data-q"]).filter(Boolean);
    assert(!offered.some((q) => /4320|2880/.test(q)), `8K offered: ${offered}`);
    await B.click(q720.ref);
    await until(() => hoverRequests().some((r) => r.cmd === "add_video"), 10000, "add_video at the host");
    const add = hostRequests().find((r) => r.cmd === "add_video");
    assert(add.url === "https://www.youtube.com/watch?v=AAAAAAAAAAA" && add.quality === "720p", JSON.stringify(add));
    const probe = hostRequests().find((r) => r.cmd === "probe");
    assert(probe && probe.url === "https://www.youtube.com/watch?v=AAAAAAAAAAA", JSON.stringify(probe));
  });

  await check("3. in-page navigation to watch?v=…&list=…: the button follows; the menu shows the playlist (120 videos); a selection sends add_playlist with those items", async () => {
    await B.evaluate("(window.__ytGo('/watch?v=BBBBBBBBBBB&list=PLtestlist01'), true)");
    await B.hover(5, 5);
    await hoverPlayer();
    const { b } = await until(shownButton, 8000, "the button after in-page navigation");
    await B.click(b.ref); // closes the menu still open from check 2, if any
    if (!(await B.overlay()).buttons.some((x) => x.attrs["data-act"] === "choose" || x.attrs["data-q"])) await B.click(b.ref);
    const choose = await until(async () => (await B.overlay()).buttons.find((x) => x.attrs["data-act"] === "choose"), 15000, "the playlist section ('choose episodes')");
    const texts = (await B.overlay()).texts.join(" | ");
    assert(/Test playlist/.test(texts) && /120/.test(texts), `playlist title/count not shown: ${texts.slice(0, 400)}`);
    await B.click(choose.ref);
    const boxes = await until(async () => { const o = await B.overlay(); const c = o.buttons.filter((x) => x.attrs.type === "checkbox"); return c.length ? c : null; }, 5000, "the episode list");
    assert(boxes.length === 120, `${boxes.length} episodes listed`);
    const none = (await B.overlay()).buttons.find((x) => x.attrs["data-act"] === "none");
    await B.click(none.ref);
    for (const v of ["1", "2", "40"]) await B.click(boxes.find((x) => x.attrs.value === v).ref);
    const go = (await B.overlay()).buttons.find((x) => x.attrs["data-act"] === "download-selected");
    await B.click(go.ref);
    await until(() => hostRequests().some((r) => r.cmd === "add_playlist"), 10000, "add_playlist at the host");
    const add = hostRequests().find((r) => r.cmd === "add_playlist");
    assert(add.url === "https://www.youtube.com/playlist?list=PLtestlist01" && JSON.stringify(add.items) === "[1,2,40]", JSON.stringify(add));
  });

  await check("4. in-page navigation to the home page hides the button; Back (popstate) brings it back", async () => {
    const o0 = await B.overlay();
    const x = o0.buttons.find((b) => /\bx\b/.test(b.attrs.class ?? ""));
    if (x) await B.click(x.ref);
    await B.evaluate("(window.__ytGo('/'), true)");
    await hoverPlayer();
    await sleep(1500);
    assert(!(await shownButton()), "the button is still shown on the home page");
    await B.evaluate("(history.back(), true)");
    await sleep(500);
    await hoverPlayer();
    await until(shownButton, 8000, "the button after Back");
    assert(/[?&]v=BBBBBBBBBBB/.test(await B.evaluate("location.href")), "Back did not return to the watch page");
  });

  await check("5. options page: 'always' + 'top-left' + Save are applied on the page (no hover needed)", async () => {
    const saved = await B.options(`async () => {
      document.querySelector('input[name=visibility][value=always]').click();
      document.querySelector('input[name=position][value=top-left]').click();
      document.querySelector('#f button[type=submit]').click();
      await new Promise((r) => setTimeout(r, 800));
      const access = document.getElementById('access');
      return { msg: document.getElementById('msg').textContent, accessShown: access ? !access.hidden : null };
    }`);
    console.log(`     options page said: ${JSON.stringify(saved)}`);
    // the extension may run on web pages (else the options page says so and offers to allow it)
    assert(saved.accessShown === false, "the options page says the extension is NOT allowed on web pages");
    await B.open("https://www.youtube.com/watch?v=CCCCCCCCCCC");
    await B.hover(3, 3);
    const p = await player();
    const { b } = await until(shownButton, 8000, "the button without hover ('always')");
    const left = leftOf(b);
    assert(left >= p.x && left < p.x + p.w / 2, `button left ${left} is not at the top-left of the player ${JSON.stringify(p)}`);
  });

  await check("6. 'Open in Eid Download' sends the video WITH its playlist to the app's own Video window", async () => {
    await B.open("https://www.youtube.com/watch?v=DDDDDDDDDDD&list=PLtestlist01&index=2&si=tracker");
    await B.hover(3, 3);
    const { b } = await until(shownButton, 8000, "the button");
    await B.click(b.ref);
    const open = await until(async () => (await B.overlay()).buttons.find((x) => x.attrs["data-act"] === "open-app"), 8000, "'Open in Eid Download' in the menu");
    await B.click(open.ref);
    await until(() => hostRequests().some((r) => r.cmd === "open_video"), 10000, "open_video at the host");
    const r = hostRequests().find((x) => x.cmd === "open_video");
    assert(r.url === "https://www.youtube.com/watch?v=DDDDDDDDDDD&list=PLtestlist01", JSON.stringify(r));
    await until(async () => /Opened in Eid Download/.test((await B.overlay()).texts.join(" ")), 5000, "the confirmation in the menu");
  });
} catch (e) {
  failed++;
  console.log(`FAIL (setup) ${e?.stack ?? e}`);
} finally {
  if (B?.logs?.length) console.log("browser log:\n  " + B.logs.slice(-40).join("\n  "));
  if (failed && B) console.log("driver/browser output (tail):\n" + B.stderr().slice(-3000));
  console.log("host requests:", JSON.stringify(hostRequests()).slice(0, 1500));
  await B?.close();
  restoreHost();
  fs.writeFileSync(path.join(OUT, `browser-e2e-${BROWSER}.json`), JSON.stringify(results, null, 1));
}
function hoverRequests() { return hostRequests(); }
if (!failed && results.length === 0) { failed = 1; console.log("FAIL no check ran"); }
console.log(failed ? `BROWSER E2E (${BROWSER}): ${failed} check(s) FAILED` : `BROWSER E2E (${BROWSER}): all ${results.length} checks passed${ONLY ? ` (only checks ${[...ONLY].join(",")}; the others NOT RUN)` : ""}`);
process.exit(failed ? 1 : 0);
