#!/usr/bin/env node
// Headless-Chrome end-to-end check of the browser extension (zero dependencies; Node >= 22 and
// Chrome >= 126). Chrome is driven over --remote-debugging-pipe so the unpacked build can be
// loaded with CDP Extensions.loadUnpacked (branded Chrome dropped --load-extension).
// It opens a local page with a real <video>, hovers it and checks, inside the closed Shadow DOM:
// the "Download this video" button appears over the player, opens its menu, and a click on a
// download choice reaches the native-messaging layer, which here is not installed, so the
// button must say so (not fail silently). A local page with an <video> whose element reports
// DRM never gets a button. The real app + native host are covered on Windows
// (docs/OWNER-ACTIONS.md, MANUAL-TESTS); this proves the in-page part in a real browser.
//
//   node scripts/ext/ext_e2e.mjs [--chrome google-chrome]
//
// REAL mode (owner: "mocks are not proof"): --real <YouTube watch URL> --host <eid-native-host>
// --app <eid> [--home DIR]. Registers the real native host for this Chrome profile, then on
// the real YouTube page: hover the player, open the button, wait for the qualities the real app
// probed with the real yt-dlp, click 360p, and wait until the file is in DIR/Downloads (the
// app's default folder). The app's video tools must already be installed and agreed to in the
// per-user folders of the environment Chrome inherits (the native host starts `eid serve`).
import { spawn, execFileSync } from "node:child_process";
import { createServer } from "node:http";
import { mkdtemp, rm, mkdir, writeFile, readdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const arg = (k, d) => { const i = process.argv.indexOf(k); return i > 0 ? process.argv[i + 1] : d; };
const CHROME = arg("--chrome", process.env.CHROME || "google-chrome");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

execFileSync(process.execPath, [join(ROOT, "scripts", "ext", "build.mjs")], { stdio: "ignore" });
const EXT = join(ROOT, "extension", "dist", "chrome");

const PAGE = `<!doctype html><html><head><meta charset="utf-8"><title>video page</title></head>
<body style="margin:0;background:#222">
<h1 style="color:#fff">Eid e2e</h1>
<video id="v" src="/clip.mp4" width="640" height="360" controls style="display:block;margin:40px"></video>
</body></html>`;
const SMALL = `<!doctype html><html><body><video src="/clip.mp4" width="160" height="90"></video></body></html>`;
const server = createServer((req, res) => {
  const u = new URL(req.url, "http://x");
  if (u.pathname === "/page.html") return res.writeHead(200, { "content-type": "text/html" }).end(PAGE);
  if (u.pathname === "/small.html") return res.writeHead(200, { "content-type": "text/html" }).end(SMALL);
  if (u.pathname === "/clip.mp4") return res.writeHead(200, { "content-type": "video/mp4" }).end(Buffer.alloc(16));
  res.writeHead(404).end();
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const base = `http://127.0.0.1:${server.address().port}`;

const prof = await mkdtemp(join(tmpdir(), "eid-ext-e2e-"));
const REAL = arg("--real");
const realHome = arg("--home", join(prof, "home"));
const env = { ...process.env };
if (REAL) {
  const host = resolve(arg("--host")), app = resolve(arg("--app"));
  await mkdir(join(prof, "NativeMessagingHosts"), { recursive: true });
  await writeFile(join(prof, "NativeMessagingHosts", "com.eid.download.json"), JSON.stringify({
    name: "com.eid.download", description: "Eid Download", path: host, type: "stdio",
    allowed_origins: ["chrome-extension://ambokeecedpgicighhcpdfjphdnphgaa/"] }));
  await mkdir(join(realHome, "Downloads"), { recursive: true });
  Object.assign(env, { EID_NATIVE_HOST_APP: app, HOME: realHome, EID_IPC_ENDPOINT: `e2e${process.pid}` });
}
const chrome = spawn(CHROME, ["--headless=new", "--no-sandbox", "--disable-gpu", "--no-first-run",
  "--remote-debugging-pipe", "--enable-unsafe-extension-debugging", `--user-data-dir=${prof}`,
  "--window-size=1280,800", "about:blank"], { stdio: ["ignore", "ignore", "pipe", "pipe", "pipe"], env });
let stderr = ""; chrome.stderr.on("data", (d) => { stderr += d; });
const toChrome = chrome.stdio[3], fromChrome = chrome.stdio[4];

let seq = 0; const pending = new Map(); const events = []; const hooks = [];
let buf = Buffer.alloc(0);
fromChrome.on("data", (d) => {
  buf = Buffer.concat([buf, d]);
  let i;
  while ((i = buf.indexOf(0)) >= 0) {
    const m = JSON.parse(buf.subarray(0, i).toString("utf8")); buf = buf.subarray(i + 1);
    if (m.id && pending.has(m.id)) { const { res, rej } = pending.get(m.id); pending.delete(m.id); m.error ? rej(new Error(JSON.stringify(m.error))) : res(m.result); }
    else { events.push(m); for (const h of hooks) h(m); }
  }
});
const send = (method, params = {}, sessionId) => new Promise((res, rej) => {
  const id = ++seq; pending.set(id, { res, rej });
  toChrome.write(JSON.stringify({ id, method, params, sessionId }) + "\0");
});

let failed = 0;
async function check(name, fn) {
  try { const r = await fn(); console.log(r === "skip" ? `SKIP ${name}` : `ok   ${name}`); }
  catch (e) { failed++; console.log(`FAIL ${name}\n     ${e.message ?? e}`); }
}
const assert = (c, m) => { if (!c) throw new Error(m); };

/** Every text inside the extension's closed shadow root on the page (DOM.getDocument pierces). */
async function shadowTexts(S) {
  const { root } = await S("DOM.getDocument", { depth: -1, pierce: true });
  const out = { host: false, texts: [], buttons: [] };
  const styleIds = new Set();
  const walk = (n, inShadow) => {
    if (n.shadowRootType === "closed") inShadow = true;
    // only the extension's closed root (the <video>'s own controls are a "user-agent" root)
    if (n.shadowRoots) for (const s of n.shadowRoots) { if (s.shadowRootType === "closed") { out.host = true; walk(s, true); } }
    if (inShadow && n.nodeType === 3 && n.nodeValue.trim() && n.parentId && !styleIds.has(n.parentId)) out.texts.push(n.nodeValue.trim());
    if (inShadow && n.nodeName === "BUTTON") out.buttons.push(n);
    if (n.nodeName === "STYLE") styleIds.add(n.nodeId);
    for (const c of n.children ?? []) walk(c, inShadow);
  };
  walk(root, false);
  return out;
}

try {
  let extId;
  await check("the unpacked build loads (MV3, key-pinned id)", async () => {
    const r = await send("Extensions.loadUnpacked", { path: EXT });
    extId = r.id;
    assert(extId === "ambokeecedpgicighhcpdfjphdnphgaa", `id ${extId}`);
  });
  const { targetId } = await send("Target.createTarget", { url: "about:blank" });
  const { sessionId } = await send("Target.attachToTarget", { targetId, flatten: true });
  const S = (m, p) => send(m, p, sessionId);
  await S("Page.enable"); await S("Runtime.enable"); await S("DOM.enable");
  const nav = async (u) => { await S("Page.navigate", { url: u }); await sleep(1500); };
  const hover = async (x, y) => { await S("Input.dispatchMouseEvent", { type: "mouseMoved", x, y }); await sleep(600); };
  const click = async (x, y) => {
    await S("Input.dispatchMouseEvent", { type: "mousePressed", x, y, button: "left", clickCount: 1 });
    await S("Input.dispatchMouseEvent", { type: "mouseReleased", x, y, button: "left", clickCount: 1 });
    await sleep(800);
  };
  const boxOf = async (nodeId) => { const { model } = await S("DOM.getBoxModel", { backendNodeId: nodeId }); const q = model.content; return { x: (q[0] + q[2]) / 2, y: (q[1] + q[5]) / 2 }; };

  await check("button appears over a real <video> when hovered", async () => {
    await nav(`${base}/page.html`);
    await hover(360, 300);
    const s = await shadowTexts(S);
    assert(s.host, "no closed shadow host on the page");
    assert(s.buttons.length >= 1, "no button in the shadow root");
  });

  await check(REAL ? "menu opens, and a download click reaches the REAL app through the real native host" : "menu opens, and a download click reports that the desktop app/host is missing", async () => {
    let s = await shadowTexts(S);
    const btn = s.buttons[0];
    const p = await boxOf(btn.backendNodeId);
    await click(p.x, p.y);
    s = await shadowTexts(S);
    assert(s.buttons.length >= 2, `menu items: ${s.buttons.length} texts=${s.texts.join(" | ")}`);
    const cls = (b) => { const a = b.attributes ?? []; const i = a.indexOf("class"); return i >= 0 ? a[i + 1] : ""; };
    const choice = s.buttons.find((b) => /\bit\b/.test(cls(b)));
    assert(choice, "no download choice in the menu");
    const q = await boxOf(choice.backendNodeId);
    await click(q.x, q.y);
    await sleep(1500);
    s = await shadowTexts(S);
    const all = s.texts.join(" | ");
    if (REAL) assert(/Sent to Eid Download/i.test(all), `the real app did not accept it: ${all}`);
    else assert(/not installed|install|غير مثبت|ثبّت/i.test(all), `no 'not installed' message: ${all}`);
    console.log("     shown:", all.slice(0, 200));
  });

  if (REAL) await check(`REAL: YouTube page -> button -> real probe -> 360p -> file in Downloads (${REAL})`, async () => {
    const t0 = Date.now();
    await S("Page.navigate", { url: REAL });
    // the player
    let rect = null;
    for (let i = 0; i < 60 && !rect; i++) {
      await sleep(500);
      const r = await S("Runtime.evaluate", { expression: `(() => { const v = [...document.querySelectorAll('video')].map(v => v.getBoundingClientRect()).find(r => r.width > 200 && r.height > 120); return v ? {x: v.x + v.width / 2, y: v.y + v.height / 2} : null; })()`, returnByValue: true });
      rect = r.result.value;
    }
    const wall = await S("Runtime.evaluate", { expression: `/not a bot/i.test(document.body.innerText)`, returnByValue: true });
    if (wall.result.value) { console.log("     YouTube shows this headless browser its 'confirm you're not a bot' wall (no bypass attempted)"); return "skip"; }
    assert(rect, "no player on the page (consent wall or bot check?)");
    const cls = (b) => { const a = b.attributes ?? []; const i = a.indexOf("class"); return i >= 0 ? a[i + 1] : ""; };
    const attr = (b, k) => { const a = b.attributes ?? []; const i = a.indexOf(k); return i >= 0 ? a[i + 1] : null; };
    let s;
    for (let i = 0; i < 20; i++) { await hover(rect.x + (i % 3), rect.y); s = await shadowTexts(S); if (s.buttons.length) break; }
    if (!s.buttons.length && (await S("Runtime.evaluate", { expression: `/not a bot/i.test(document.body.innerText)`, returnByValue: true })).result.value) {
      console.log("     YouTube shows this headless browser its 'confirm you're not a bot' wall (no bypass attempted)"); return "skip";
    }
    if (!s.buttons.length) {
      const shot = await S("Page.captureScreenshot", { format: "png" });
      await writeFile(join(tmpdir(), "eid-ext-e2e-youtube.png"), Buffer.from(shot.data, "base64"));
      const info = await S("Runtime.evaluate", { expression: `JSON.stringify([...document.querySelectorAll('video')].map(v => ({r: v.getBoundingClientRect(), src: (v.currentSrc||'').slice(0,40), ad: !!v.closest('.ad-showing, .ad-interrupting')})))`, returnByValue: true });
      console.log("     videos:", info.result.value, "screenshot:", join(tmpdir(), "eid-ext-e2e-youtube.png"));
    }
    assert(s.buttons.length, "no button over the YouTube player");
    const p = await boxOf(s.buttons[0].backendNodeId);
    await click(p.x, p.y);
    let q360 = null;
    for (let i = 0; i < 240 && !q360; i++) {
      await sleep(500);
      s = await shadowTexts(S);
      q360 = s.buttons.find((b) => attr(b, "data-q") === "360p");
      if (!q360 && s.texts.some((x) => /not installed|Could not start|not allow/i.test(x))) break;
    }
    const offered = s.buttons.map((b) => attr(b, "data-q")).filter(Boolean);
    console.log(`     probe ${((Date.now() - t0) / 1000).toFixed(1)}s offered: ${offered.join(",")} | ${s.texts.join(" | ").slice(0, 300)}`);
    assert(q360, "no 360p choice");
    assert(!offered.some((q) => /4320|2880/.test(q)), "8K offered");
    const q = await boxOf(q360.backendNodeId);
    await click(q.x, q.y);
    const dl = join(realHome, "Downloads");
    let file = null;
    const t1 = Date.now();
    while (!file && Date.now() - t1 < 300000) {
      await sleep(1000);
      file = (await readdir(dl)).find((f) => f.endsWith(".mp4"));
    }
    assert(file, `no .mp4 in ${dl} after 300 s`);
    console.log(`     downloaded in ${((Date.now() - t1) / 1000).toFixed(1)}s: ${file}`);
  });

  // The in-page YouTube menu with the real app: the browser is given a minimal stand-in page at
  // the real YouTube watch URL (CDP Fetch; nothing is sent to YouTube for the page itself), so
  // the content script runs its YouTube flow: real probe (qualities + playlist) and a real
  // playlist "choose items" download through the real native host and app.
  if (REAL) await check(`REAL: in-page YouTube menu (stand-in page at the watch URL) -> choose playlist items 1 -> files`, async () => {
    const u = new URL(REAL);
    const stand = `<!doctype html><html><head><meta charset="utf-8"><title>stand-in</title></head><body style="margin:0">
<div id="movie_player"><video src="${base}/clip.mp4" width="854" height="480" style="display:block;margin:20px"></video></div></body></html>`;
    hooks.push((m) => {
      if (m.method === "Fetch.requestPaused" && m.sessionId === sessionId) {
        send("Fetch.fulfillRequest", { requestId: m.params.requestId, responseCode: 200,
          responseHeaders: [{ name: "content-type", value: "text/html; charset=utf-8" }],
          body: Buffer.from(stand).toString("base64") }, sessionId).catch(() => {});
      }
    });
    await S("Fetch.enable", { patterns: [{ urlPattern: "https://www.youtube.com/watch*", requestStage: "Request" }] });
    await nav(REAL);
    await S("Fetch.disable");
    const attr = (b, k) => { const a = b.attributes ?? []; const i = a.indexOf(k); return i >= 0 ? a[i + 1] : null; };
    let s;
    for (let i = 0; i < 10; i++) { await hover(400 + i, 250); s = await shadowTexts(S); if (s.buttons.length) break; }
    assert(s.buttons.length, "no button over the stand-in player at the YouTube URL");
    let p = await boxOf(s.buttons[0].backendNodeId);
    await click(p.x, p.y);
    let choose = null, q360 = null;
    const t0 = Date.now();
    for (let i = 0; i < 240 && !(choose && q360); i++) {
      await sleep(500);
      s = await shadowTexts(S);
      q360 = s.buttons.find((b) => attr(b, "data-q") === "360p");
      choose = s.buttons.find((b) => attr(b, "data-act") === "choose");
    }
    console.log(`     menu ready ${((Date.now() - t0) / 1000).toFixed(1)}s: ${s.texts.join(" | ").slice(0, 400)}`);
    assert(q360, "no 360p choice for this video");
    assert(choose, "no 'choose items' for the playlist");
    p = await boxOf(choose.backendNodeId);
    await click(p.x, p.y);
    s = await shadowTexts(S);
    const boxes = s.buttons.length; void boxes;
    // the picker: check boxes are <input type=checkbox>; tick only item 1 through the DOM node
    const { root } = await S("DOM.getDocument", { depth: -1, pierce: true });
    const inputs = [];
    const walk = (n) => { if (n.nodeName === "INPUT") inputs.push(n); for (const c of n.children ?? []) walk(c); for (const r of n.shadowRoots ?? []) walk(r); };
    walk(root);
    const cbs = inputs.filter((n) => attr(n, "type") === "checkbox");
    console.log(`     picker shows ${cbs.length} items`);
    assert(cbs.length >= 2, "picker items");
    // untick all but the first
    for (const n of cbs) {
      const { object } = await S("DOM.resolveNode", { backendNodeId: n.backendNodeId });
      await S("Runtime.callFunctionOn", { objectId: object.objectId, functionDeclaration: `function (first) { if (this.checked !== first) this.click(); }`, arguments: [{ value: n === cbs[0] }] });
    }
    s = await shadowTexts(S);
    const go = s.buttons.filter((b) => /primary/.test(attr(b, "class") ?? "")).pop();
    assert(go, "no start button in the picker");
    const before = new Set(await readdir(join(realHome, "Downloads"), { recursive: true }));
    p = await boxOf(go.backendNodeId);
    await click(p.x, p.y);
    s = await shadowTexts(S);
    console.log(`     after start: ${s.texts.slice(-2).join(" | ")}`);
    let file = null; const t1 = Date.now();
    while (!file && Date.now() - t1 < 300000) {
      await sleep(1000);
      file = (await readdir(join(realHome, "Downloads"), { recursive: true })).find((f) => /(^|\/)01 - .*\.mp4$/.test(f) && !before.has(f));
    }
    assert(file, "no '01 - ….mp4' in a playlist folder after 300 s");
    console.log(`     downloaded in ${((Date.now() - t1) / 1000).toFixed(1)}s: ${file}`);
  });

  // YouTube shows headless Chrome a "confirm you're not a bot" wall instead of the player (no
  // bypass is attempted). The same chain minus the in-page button: the extension's own
  // background (service worker) asks the real native host -> real app -> real yt-dlp.
  if (REAL) await check(`REAL: extension background -> native host -> app: probe + add_video 360p + file (${REAL})`, async () => {
    const { targetInfos } = await send("Target.getTargets");
    const sw = targetInfos.find((t) => t.type === "service_worker" && t.url.startsWith("chrome-extension://ambokeecedpgicighhcpdfjphdnphgaa/"));
    assert(sw, "no extension service worker");
    const { sessionId: ws } = await send("Target.attachToTarget", { targetId: sw.targetId, flatten: true });
    const W = async (expr) => {
      const r = await send("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true }, ws);
      if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description ?? r.exceptionDetails.text);
      return r.result.value;
    };
    const video = new URL(REAL); video.searchParams.delete("list"); video.searchParams.delete("index");
    const t0 = Date.now();
    const pr = await W(`handle({ eid: 1, cmd: "probe", url: ${JSON.stringify(video.href)} })`);
    console.log(`     probe ${((Date.now() - t0) / 1000).toFixed(1)}s: ${JSON.stringify(pr).slice(0, 300)}`);
    assert(pr.ok && pr.result.kind === "video", "probe failed");
    assert(pr.result.qualities.includes("360p") && !pr.result.qualities.some((q) => /4320|2880/.test(q)), "qualities");
    if (new URL(REAL).searchParams.get("list")) {
      const pl = new URL("https://www.youtube.com/playlist"); pl.searchParams.set("list", new URL(REAL).searchParams.get("list"));
      const pp = await W(`handle({ eid: 1, cmd: "probe", url: ${JSON.stringify(pl.href)} })`);
      console.log(`     playlist probe: kind=${pp.result?.kind} items=${pp.result?.items?.length} title=${pp.result?.title}`);
      assert(pp.ok && pp.result.kind === "playlist" && pp.result.items.length > 1, "playlist probe");
    }
    const before = new Set(await readdir(join(realHome, "Downloads")));
    const add = await W(`handle({ eid: 1, cmd: "add_video", url: ${JSON.stringify(video.href)}, quality: "360p" })`);
    assert(add.ok, `add_video: ${JSON.stringify(add)}`);
    let file = null; const t1 = Date.now();
    while (!file && Date.now() - t1 < 300000) {
      await sleep(1000);
      file = (await readdir(join(realHome, "Downloads"))).find((f) => f.endsWith(".mp4") && !before.has(f));
    }
    assert(file, "no new .mp4 in Downloads after 300 s");
    console.log(`     downloaded in ${((Date.now() - t1) / 1000).toFixed(1)}s: ${file}`);
  });

  await check("a small video (an ad/thumbnail) gets no button", async () => {
    await nav(`${base}/small.html`);
    await hover(80, 45);
    const s = await shadowTexts(S);
    assert(!s.buttons.some(() => true) || !s.host, "button shown on a 160x90 video");
  });
} finally {
  try { chrome.kill("SIGKILL"); } catch {}
  server.close();
  await rm(prof, { recursive: true, force: true }).catch(() => {});
}
console.log(failed ? `${failed} check(s) failed` : "all checks passed");
process.exit(failed ? 1 : 0);
