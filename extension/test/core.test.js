// Unit tests for the pure logic. Run: node --test extension/test/
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const C = require("../src/lib/core.js");

const V = "dQw4w9WgXcQ"; // an 11-char id shape
const WATCH = "https://www.youtube.com/watch?v=" + V;

test("classifyUrl: YouTube watch, short link, shorts, embed, live", () => {
  for (const u of [
    WATCH,
    "https://youtube.com/watch?v=" + V + "&t=42s&feature=share",
    "https://m.youtube.com/watch?v=" + V,
    "https://music.youtube.com/watch?v=" + V,
    "https://youtu.be/" + V + "?si=tracking",
    "https://www.youtube.com/live/" + V,
  ]) {
    const c = C.classifyUrl(u);
    assert.equal(c.kind, "youtube", u);
    assert.equal(c.type, "watch", u);
    assert.equal(c.videoId, V, u);
    assert.equal(c.videoUrl, WATCH, u); // canonical, tracking parameters dropped
    assert.equal(c.playlistUrl, undefined, u);
  }
  const s = C.classifyUrl("https://www.youtube.com/shorts/" + V);
  assert.deepEqual([s.type, s.videoUrl], ["shorts", WATCH]);
  for (const u of [
    "https://www.youtube.com/embed/" + V + "?autoplay=1",
    "https://www.youtube-nocookie.com/embed/" + V,
  ]) {
    const e = C.classifyUrl(u);
    assert.deepEqual([e.type, e.videoUrl], ["embed", WATCH], u);
  }
});

test("classifyUrl: playlists (offered and not offered)", () => {
  const L = "PLx0sYbCqOb8TBPRdmBHs5Iftvv9TPboYG";
  const w = C.classifyUrl(WATCH + "&list=" + L + "&index=3");
  assert.equal(w.type, "watch");
  assert.equal(w.videoUrl, WATCH);
  assert.equal(w.playlistUrl, "https://www.youtube.com/playlist?list=" + L);
  const p = C.classifyUrl("https://www.youtube.com/playlist?list=" + L);
  assert.equal(p.type, "playlist");
  assert.equal(p.videoUrl, undefined);
  assert.equal(p.playlistId, L);
  assert.equal(C.classifyUrl("https://www.youtube.com/embed/" + V + "?list=" + L).playlistId, L);
  // Watch Later, Liked, Liked music and Mixes are private/personal/endless: not offered
  for (const bad of ["WL", "LL", "LM", "RD" + V, "RDMM", "", "a", "x".repeat(65), "PL<script>"]) {
    const c = C.classifyUrl(WATCH + "&list=" + encodeURIComponent(bad));
    assert.equal(c.playlistUrl, undefined, bad);
    assert.equal(c.videoUrl, WATCH, bad);
  }
  assert.equal(C.classifyUrl("https://www.youtube.com/playlist?list=WL").type, "other");
  // a Short never offers its playlist
  assert.equal(C.classifyUrl("https://www.youtube.com/shorts/" + V + "?list=" + L).playlistUrl, undefined);
});

test("classifyUrl: non-video YouTube pages, other sites, junk", () => {
  for (const u of [
    "https://www.youtube.com/",
    "https://www.youtube.com/results?search_query=x",
    "https://www.youtube.com/@channel",
    "https://www.youtube.com/watch?v=short",
    "https://www.youtube.com/watch?v=" + V + "x",
    "https://www.youtube.com/embed/",
    "https://www.youtube.com/shorts/../" + V,
  ]) {
    const c = C.classifyUrl(u);
    assert.equal(c.kind, "youtube", u);
    assert.equal(c.type, "other", u);
    assert.equal(c.videoUrl, undefined, u);
  }
  assert.deepEqual(C.classifyUrl("https://example.com/v.html"), { kind: "web" });
  assert.deepEqual(C.classifyUrl("https://notyoutube.com/watch?v=" + V), { kind: "web" });
  assert.deepEqual(C.classifyUrl("https://youtube.com.evil.example/watch?v=" + V), { kind: "web" });
  for (const u of [
    "file:///C:/x.mp4",
    "chrome://settings",
    "javascript:alert(1)",
    "about:blank",
    "https://user:pass@www.youtube.com/watch?v=" + V,
    "not a url",
    "",
    null,
    42,
    "https://h/" + "a".repeat(C.MAX_URL_LEN),
  ]) {
    assert.deepEqual(C.classifyUrl(u), { kind: "unsupported" }, String(u).slice(0, 40));
  }
});

test("filterQualities: policy order, 2160p cap, no 8K, no unknowns, no duplicates", () => {
  assert.deepEqual(
    C.filterQualities(["audio", "720p", "4320p", "2160p", "best", "720p", "8k", "2880p", "144p", 1080, null, "1080p"]),
    ["2160p", "1080p", "720p", "best", "audio"],
  );
  assert.deepEqual(C.filterQualities(["4320p", "2880p", "8K", "8640p"]), []);
  assert.deepEqual(C.filterQualities("1080p"), []);
  assert.deepEqual(C.filterQualities(undefined), []);
  for (const q of C.ALL_QUALITIES) {
    const h = C.heightOf(q);
    assert.ok(h === null || h <= C.MAX_HEIGHT, q);
  }
  assert.equal(C.MAX_HEIGHT, 2160);
  assert.deepEqual(C.ALL_QUALITIES, ["2160p", "1440p", "1080p", "720p", "480p", "360p", "best", "audio"]);
});

test("normalizeProbe: validates shape, filters qualities, cleans text, bounds items", () => {
  assert.equal(C.normalizeProbe(null), null);
  assert.equal(C.normalizeProbe({ kind: "channel" }), null);
  assert.equal(C.normalizeProbe("video"), null);
  const v = C.normalizeProbe({
    kind: "video",
    title: "  Hello\u202e\u0000 world\n ",
    uploader: 7,
    duration: -1,
    qualities: ["4320p", "1080p", "best", "audio"],
    extra: "ignored",
  });
  assert.deepEqual(v, { kind: "video", title: "Hello world", qualities: ["1080p", "best", "audio"] });
  const long = C.normalizeProbe({ kind: "video", title: "x".repeat(1000), qualities: [] });
  assert.ok(long.title.length <= 300);
  const p = C.normalizeProbe({
    kind: "playlist",
    title: "L",
    qualities: ["720p", "best"],
    items: [
      { index: 2, id: "b", title: "Two", duration: 61 },
      { index: 1, id: "a", title: "One" },
      { index: 2, id: "dup", title: "dup" },
      { index: 0, title: "zero" },
      { index: 1.5, title: "frac" },
      { index: "3", title: "str" },
      null,
      { index: 4, title: { evil: 1 }, duration: Infinity },
    ],
  });
  assert.deepEqual(p.items, [
    { index: 1, title: "One" },
    { index: 2, title: "Two", duration: 61 },
    { index: 4, title: "" },
  ]);
  const many = Array.from({ length: C.MAX_ITEMS + 50 }, (_, i) => ({ index: i + 1, title: "t" }));
  assert.equal(C.normalizeProbe({ kind: "playlist", qualities: [], items: many }).items.length, C.MAX_ITEMS);
  assert.deepEqual(C.normalizeProbe({ kind: "playlist", qualities: [] }).items, []);
});

test("buildRequest: exact wire shapes for every browser command", () => {
  const id = "ext-0123456789abcdef01234567";
  assert.deepEqual(C.buildRequest({ cmd: "ping" }, id), { v: 1, id, command: { cmd: "ping" } });
  assert.deepEqual(C.buildRequest({ cmd: "probe", url: WATCH, junk: 1 }, id), {
    v: 1,
    id,
    command: { cmd: "probe", url: WATCH },
  });
  assert.deepEqual(C.buildRequest({ cmd: "add", url: "https://cdn.example/v.mp4" }, id).command, {
    cmd: "add",
    url: "https://cdn.example/v.mp4",
  });
  assert.deepEqual(C.buildRequest({ cmd: "add_video", url: WATCH, quality: "1440p" }, id).command, {
    cmd: "add_video",
    url: WATCH,
    quality: "1440p",
  });
  const pl = "https://www.youtube.com/playlist?list=PLabc";
  assert.deepEqual(C.buildRequest({ cmd: "add_playlist", url: pl, quality: "audio" }, id).command, {
    cmd: "add_playlist",
    url: pl,
    quality: "audio",
  });
  assert.deepEqual(
    C.buildRequest({ cmd: "add_playlist", url: pl, quality: "best", items: [5, 1, 3] }, id).command.items,
    [1, 3, 5],
  );
});

test("buildRequest: rejects everything outside the browser allow-list and policy", () => {
  const id = "ext-1";
  const bad = [
    [{ cmd: "shutdown" }, "bad_action"],
    [{ cmd: "list" }, "bad_action"],
    [{ cmd: "show" }, "bad_action"],
    [{ cmd: "pause", item: 1 }, "bad_action"],
    [{ cmd: "resume", item: 1 }, "bad_action"],
    [null, "bad_action"],
    [{ cmd: "probe", url: "javascript:alert(1)" }, "bad_url"],
    [{ cmd: "probe", url: "blob:https://www.youtube.com/123" }, "bad_url"],
    [{ cmd: "probe", url: "https://u:p@h/" }, "bad_url"],
    [{ cmd: "probe", url: "https://h/" + "a".repeat(9000) }, "bad_url"],
    [{ cmd: "probe" }, "bad_url"],
    [{ cmd: "add", url: "data:video/mp4;base64,AAAA" }, "bad_url"],
    [{ cmd: "add_video", url: WATCH, quality: "4320p" }, "bad_quality"],
    [{ cmd: "add_video", url: WATCH, quality: "8k" }, "bad_quality"],
    [{ cmd: "add_video", url: WATCH, quality: "2160" }, "bad_quality"],
    [{ cmd: "add_video", url: WATCH }, "bad_quality"],
    [{ cmd: "add_playlist", url: WATCH, quality: "best", items: [] }, "bad_items"],
    [{ cmd: "add_playlist", url: WATCH, quality: "best", items: [0] }, "bad_items"],
    [{ cmd: "add_playlist", url: WATCH, quality: "best", items: [1, 1] }, "bad_items"],
    [{ cmd: "add_playlist", url: WATCH, quality: "best", items: [1.5] }, "bad_items"],
    [{ cmd: "add_playlist", url: WATCH, quality: "best", items: ["1"] }, "bad_items"],
    [{ cmd: "add_playlist", url: WATCH, quality: "best", items: [2 ** 32] }, "bad_items"],
    [{ cmd: "add_playlist", url: WATCH, quality: "best", items: "1,2" }, "bad_items"],
    [
      { cmd: "add_playlist", url: WATCH, quality: "best", items: Array.from({ length: 5001 }, (_, i) => i + 1) },
      "bad_items",
    ],
  ];
  for (const [action, code] of bad) {
    assert.throws(() => C.buildRequest(action, id), new RegExp("^Error: " + code + "$"), JSON.stringify(action)?.slice(0, 80));
  }
  for (const badId of ["", "a b", "../x", "x".repeat(65), null]) {
    assert.throws(() => C.buildRequest({ cmd: "ping" }, badId), /bad_id/);
  }
});

test("newRequestId: fresh, valid for proto.rs, unique", () => {
  const seen = new Set();
  for (let i = 0; i < 1000; i++) {
    const id = C.newRequestId();
    assert.match(id, /^ext-[0-9a-f]{24}$/);
    assert.ok(id.length <= 64);
    seen.add(id);
  }
  assert.equal(seen.size, 1000);
  assert.equal(C.newRequestId((b) => b.fill(255)), "ext-" + "ff".repeat(12));
});

test("classifyError / parseResponse: stable codes, raw text never needed", () => {
  const cases = {
    "Specified native messaging host not found.": "not_installed",
    "No such native application com.eid.download": "not_installed",
    "Access to the specified native messaging host is forbidden.": "not_allowed",
    "Eid Download is not running": "app_not_running",
    "cannot reach Eid Download": "app_not_running",
    "lost the connection to Eid Download": "app_not_running",
    "command not allowed from the browser": "app_too_old",
    "malformed request": "app_too_old",
    "TOOLS_MISSING: yt-dlp is not installed": "tools_missing",
    "video not found": "failed",
    "Native host has exited.": "failed",
    "": "failed",
  };
  for (const [m, code] of Object.entries(cases)) assert.equal(C.classifyError(m), code, m);
  assert.equal(C.classifyError(undefined), "failed");
  assert.deepEqual(C.parseResponse({ v: 1, id: "a", ok: true, result: { job: 3 } }, "a"), { ok: true, result: { job: 3 } });
  assert.deepEqual(C.parseResponse({ v: 1, id: "a", ok: false, error: "TOOLS_MISSING: x" }, "a"), {
    ok: false,
    code: "tools_missing",
  });
  // the host answers a frame it could not parse with id ""
  assert.deepEqual(C.parseResponse({ v: 1, id: "", ok: false, error: "malformed request" }, "a"), {
    ok: false,
    code: "app_too_old",
  });
  assert.equal(C.parseResponse({ v: 1, id: "other", ok: true, result: {} }, "a").ok, false);
  assert.equal(C.parseResponse({ v: 2, id: "a", ok: true }, "a").ok, false);
  assert.equal(C.parseResponse(undefined, "a").ok, false);
});

test("pendingDelay: probe polling protocol", () => {
  assert.equal(C.pendingDelay({ pending: true, retry_ms: 500 }), 500);
  assert.equal(C.pendingDelay({ pending: true, retry_ms: 1 }), 200);
  assert.equal(C.pendingDelay({ pending: true, retry_ms: 1e9 }), 5000);
  assert.equal(C.pendingDelay({ pending: true, retry_ms: "x" }), 500);
  assert.equal(C.pendingDelay({ pending: true }), 500);
  assert.equal(C.pendingDelay({ kind: "video", qualities: [] }), null);
  assert.equal(C.pendingDelay({ pending: "true" }), null);
  assert.equal(C.pendingDelay(null), null);
});

test("settings: defaults, clamping, unknown values, site list", () => {
  assert.deepEqual(C.normalizeSettings(undefined), { ...C.DEFAULT_SETTINGS, excludedSites: [] });
  const s = C.normalizeSettings({
    enabled: "yes",
    position: "bottom-left", // would cover the player controls: refused
    size: "huge",
    customSize: 999,
    color: "neon",
    customColor: "red;background:url(x)",
    shape: "star",
    label: "text",
    opacity: 0,
    visibility: "never",
    excludedSites: ["https://www.Example.com/path", "*.foo.org", "bar.net.", "", "a..b", "x y", 3, "example.com", "www.example.com"],
  });
  assert.equal(s.enabled, true);
  assert.equal(s.position, "top-right");
  assert.equal(s.size, "medium");
  assert.equal(s.customSize, 64);
  assert.equal(s.color, "teal");
  assert.equal(s.customColor, "#0f766e");
  assert.equal(s.shape, "rounded");
  assert.equal(s.label, "icon-text");
  assert.equal(s.opacity, 0.3);
  assert.equal(s.visibility, "hover");
  assert.deepEqual(s.excludedSites, ["www.example.com", "foo.org", "bar.net", "example.com"]);
  assert.equal(C.normalizeSettings({ customSize: 1 }).customSize, 20);
  assert.equal(C.normalizeSettings({ customColor: "#ABCDEF" }).customColor, "#abcdef");
});

test("isExcluded: exact host and sub-domains only", () => {
  const st = { excludedSites: ["example.com"] };
  assert.equal(C.isExcluded("example.com", st), true);
  assert.equal(C.isExcluded("www.example.com", st), true);
  assert.equal(C.isExcluded("WWW.EXAMPLE.COM.", st), true);
  assert.equal(C.isExcluded("notexample.com", st), false);
  assert.equal(C.isExcluded("example.com.evil.net", st), false);
  assert.equal(C.isExcluded("", st), false);
  assert.equal(C.isExcluded("example.com", {}), false);
});

test("buttonStyle and placeButton: sizes, shapes, never over the bottom controls", () => {
  assert.equal(C.buttonStyle({ size: "small" }).px, 28);
  assert.equal(C.buttonStyle({ size: "large" }).px, 42);
  assert.equal(C.buttonStyle({ size: "custom", customSize: 50 }).px, 50);
  assert.equal(C.buttonStyle({ shape: "pill", size: "large" }).radius, 21);
  assert.equal(C.buttonStyle({ shape: "square" }).radius, 3);
  assert.equal(C.buttonStyle({ color: "custom", customColor: "#123456" }).color, "#123456");
  assert.equal(C.buttonStyle({ label: "icon" }).iconOnly, true);
  const rect = { left: 100, top: 50, width: 640, height: 360 };
  const watch = { kind: "youtube", type: "watch", videoId: "x" };
  const r = C.placeButton(rect, { position: "top-right" }, watch, 150, 34);
  assert.deepEqual(r, { top: 62, left: 100 + 640 - 10 - 150 });
  const l = C.placeButton(rect, { position: "top-left" }, watch, 150, 34);
  assert.equal(l.left, 110);
  // embeds and Shorts keep clear of their top-bar controls
  assert.equal(C.placeButton(rect, {}, { kind: "youtube", type: "embed" }, 150, 34).top, 110);
  assert.equal(C.placeButton(rect, {}, { kind: "youtube", type: "shorts" }, 150, 34).top, 114);
  // the button always stays in the top 60% of the player (bottom = playback controls)
  for (const h of [120, 150, 200, 360, 1080]) {
    const p = C.placeButton({ left: 0, top: 0, width: 400, height: h }, {}, { kind: "youtube", type: "embed" }, 150, 42);
    if (p) assert.ok(p.top + 42 <= h * 0.6, String(h));
  }
  assert.equal(C.placeButton({ left: 0, top: 0, width: 150, height: 100 }, {}, watch, 40, 34), null);
});

test("eligibility: DRM/EME never, blob/MSE only on YouTube, ads and tiny players skipped", () => {
  const yt = { kind: "youtube", type: "watch", videoId: V };
  const web = { kind: "web" };
  const big = { width: 640, height: 360 };
  assert.equal(C.eligibility({ ...big, src: "blob:https://www.youtube.com/x" }, yt), "youtube");
  assert.equal(C.eligibility({ ...big, src: "blob:https://www.youtube.com/x", mediaKeys: true }, yt), "drm");
  assert.equal(C.eligibility({ ...big, src: "blob:x", encrypted: true }, yt), "drm");
  assert.equal(C.eligibility({ ...big, src: "https://cdn.example/a.mp4", encrypted: true }, web), "drm");
  assert.equal(C.eligibility({ ...big, src: "blob:x", isAd: true }, yt), "ad");
  assert.equal(C.eligibility({ width: 120, height: 90, src: "blob:x" }, yt), "small");
  assert.equal(C.eligibility({ ...big, src: "blob:x" }, { kind: "youtube", type: "other" }), "none");
  assert.equal(C.eligibility({ ...big, src: "https://cdn.example/a.mp4" }, web), "file");
  for (const src of ["blob:https://site/x", "", "data:video/mp4;base64,AA", "https://cdn/x.m3u8", "https://cdn/x.mpd?a=1", "mediastream:1"]) {
    assert.equal(C.eligibility({ ...big, src }, web), "no_source", src);
  }
  assert.equal(C.eligibility({ ...big, src: "https://x/a.mp4" }, { kind: "unsupported" }), "none");
});

test("locales: en and ar have exactly the same keys, placeholders and no empty text", () => {
  const dir = path.join(__dirname, "..", "src", "_locales");
  const en = JSON.parse(fs.readFileSync(path.join(dir, "en", "messages.json"), "utf8"));
  const ar = JSON.parse(fs.readFileSync(path.join(dir, "ar", "messages.json"), "utf8"));
  assert.deepEqual(Object.keys(ar).sort(), Object.keys(en).sort());
  for (const k of Object.keys(en)) {
    for (const loc of [en, ar]) assert.ok(loc[k].message && loc[k].message.trim(), k);
    assert.deepEqual(Object.keys(ar[k].placeholders || {}).sort(), Object.keys(en[k].placeholders || {}).sort(), k);
    for (const ph of Object.keys(en[k].placeholders || {})) {
      const re = new RegExp("\\$" + ph + "\\$", "i");
      assert.match(en[k].message, re, k);
      assert.match(ar[k].message, re, k);
    }
  }
  // every key used by the code exists
  const srcDir = path.join(__dirname, "..", "src");
  const used = new Set();
  for (const f of ["content.js", "options.js", "options.html"]) {
    const s = fs.readFileSync(path.join(srcDir, f), "utf8");
    for (const m of s.matchAll(/\bt\("([A-Za-z0-9]+)"/g)) used.add(m[1]);
    for (const m of s.matchAll(/data-i18n="([A-Za-z0-9]+)"/g)) used.add(m[1]);
    for (const m of s.matchAll(/:\s*"((?:err|q|color|opt)[A-Z0-9][A-Za-z0-9]*)"/g)) used.add(m[1]);
  }
  assert.ok(used.size > 30, "found " + used.size + " keys");
  for (const k of used) assert.ok(en[k], "missing locale key " + k);
});

test("no innerHTML / eval / remote code in the extension sources", () => {
  const srcDir = path.join(__dirname, "..", "src");
  for (const f of ["content.js", "background.js", "options.js", "lib/core.js"]) {
    const s = fs.readFileSync(path.join(srcDir, f), "utf8");
    for (const bad of [/\.innerHTML\s*=/, /\.outerHTML\s*=/, /insertAdjacentHTML/, /\beval\s*\(/, /new Function\s*\(/, /document\.write/, /https?:\/\/(?!www\.youtube\.com\/(watch|playlist)|www\.w3\.org)/]) {
      assert.doesNotMatch(s.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, ""), bad, f + " " + bad);
    }
  }
});
