// Eid Download background: the only part of the extension that talks to the native host.
// Chrome/Edge run it as an MV3 service worker, Firefox as an MV3 background script.
// Content scripts never reach the host directly: they send small, validated messages here.
"use strict";

if (typeof importScripts === "function" && typeof EidCore === "undefined") {
  importScripts("lib/core.js"); // Chrome/Edge service worker
}

const api = typeof browser !== "undefined" ? browser : chrome;
const Core = EidCore;

/** One native round trip. Resolves to {ok:true,result} or {ok:false,code}. */
function nativeCall(action) {
  let req;
  try {
    req = Core.buildRequest(action, Core.newRequestId());
  } catch (e) {
    return Promise.resolve({ ok: false, code: "failed", detail: String(e && e.message) });
  }
  // promise form: Chrome/Edge MV3 and Firefox both return a promise when no callback is given
  let p;
  try {
    p = api.runtime.sendNativeMessage(Core.HOST_NAME, req);
  } catch (e) {
    return Promise.resolve({ ok: false, code: Core.classifyError(e && e.message) });
  }
  return Promise.resolve(p).then(
    (resp) => Core.parseResponse(resp, req.id),
    (e) => ({ ok: false, code: Core.classifyError(e && e.message) }),
  );
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** `probe`, following the app's {pending, retry_ms} answers (fresh id per poll). */
async function probe(url) {
  const deadline = Date.now() + Core.PROBE_DEADLINE_MS;
  for (;;) {
    const r = await nativeCall({ cmd: "probe", url });
    if (!r.ok) return r;
    const wait = Core.pendingDelay(r.result);
    if (wait === null) {
      const p = Core.normalizeProbe(r.result);
      return p ? { ok: true, result: p } : { ok: false, code: "failed" };
    }
    if (Date.now() + wait > deadline) return { ok: false, code: "timeout" };
    await sleep(wait);
  }
}

/** Only our own content scripts (in a tab) and our own pages may ask. */
function trustedSender(sender) {
  if (!sender || sender.id !== api.runtime.id) return false;
  return true;
}

const ACTIONS = new Set(["ping", "probe", "add", "add_video", "add_playlist"]);

async function handle(msg) {
  if (!msg || typeof msg !== "object" || msg.eid !== 1 || !ACTIONS.has(msg.cmd)) {
    return { ok: false, code: "failed" };
  }
  if (msg.cmd === "probe") return probe(msg.url);
  const action = { cmd: msg.cmd };
  if (msg.url !== undefined) action.url = msg.url;
  if (msg.quality !== undefined) action.quality = msg.quality;
  if (msg.items !== undefined) action.items = msg.items;
  const r = await nativeCall(action);
  // results of add* are opaque to the page ({job}); never forward more than ok/code
  return r.ok ? { ok: true } : { ok: false, code: r.code };
}

api.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (!trustedSender(sender)) return false;
  handle(msg).then(sendResponse, () => sendResponse({ ok: false, code: "failed" }));
  return true; // async response
});

// the toolbar button opens the settings
if (api.action && api.action.onClicked) {
  api.action.onClicked.addListener(() => api.runtime.openOptionsPage());
}
