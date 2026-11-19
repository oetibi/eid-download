// Unit tests of the window's pure logic (node --test, no browser).
import test from "node:test";
import assert from "node:assert/strict";
import { mergeList, categoryOf, inView, buildView, clickSelect, safeQualities, fmtSize, fmtClock, jobFraction, isToolsMissing, pollDelay, segmentView, matchesQuery, normalizeCols, gridTemplate, moveCol, COLUMNS, normalizeToolbar, linksFromText, finishedNow, videoPage } from "../logic.js";

const it = (id, o = {}) => ({ id, status: "pending", name: `f${id}.zip`, url: "", folder: "D", attempts: 0, error_kind: null, error: null, rate: null, ...o });

test("mergeList: full replaces, delta updates and removes, old shape is full", () => {
  const s = new Map();
  let r = mergeList(s, { rev: 1, full: true, items: [it(1), it(2)], removed: [] });
  assert.equal(r.rev, 1); assert.ok(r.changed); assert.deepEqual([...s.keys()], [1, 2]);
  r = mergeList(s, { rev: 2, full: false, items: [it(2, { status: "running" })], removed: [1] });
  assert.ok(r.changed); assert.deepEqual([...s.keys()], [2]); assert.equal(s.get(2).status, "running");
  r = mergeList(s, { rev: 2, full: false, items: [], removed: [] });
  assert.equal(r.changed, false);
  r = mergeList(s, { items: [it(7)] }); // v0.0.1 answer: no rev, full list
  assert.equal(r.rev, null); assert.deepEqual([...s.keys()], [7]);
});

test("categories and sidebar views", () => {
  assert.equal(categoryOf("a.MP4"), "video");
  assert.equal(categoryOf("x.unknownext"), "other");
  assert.ok(inView(it(1, { status: "running" }), "active"));
  assert.ok(!inView(it(1, { status: "done" }), "unfinished"));
  assert.ok(inView(it(1, { folder: "C:\\x" }), "folder:C:\\x"));
});

test("sorting is stable newest-first on ties; selection with ctrl/shift", () => {
  const s = new Map([[1, it(1, { name: "b" })], [2, it(2, { name: "a" })], [3, it(3, { name: "a" })]]);
  assert.deepEqual(buildView(s, "all", "name", 1).map((x) => x.id), [3, 2, 1]);
  let r = clickSelect(new Set(), null, [3, 2, 1], 3);
  r = clickSelect(r.selected, r.anchor, [3, 2, 1], 1, { shift: true });
  assert.deepEqual([...r.selected].sort(), [1, 2, 3]);
  r = clickSelect(r.selected, r.anchor, [3, 2, 1], 2, { ctrl: true });
  assert.deepEqual([...r.selected].sort(), [1, 3]);
});

test("quality policy never offers more than 2160p", () => {
  assert.deepEqual(safeQualities(["4320p", "2880p", "2160p", "best", "audio", "junk"]), ["2160p", "best", "audio"]);
});

test("formatting and job progress", () => {
  const U = ["B", "KB", "MB", "GB", "TB"];
  assert.equal(fmtSize(0, U), "0 B"); assert.equal(fmtSize(1536, U), "1.50 KB"); assert.equal(fmtSize(null, U), "");
  assert.equal(fmtClock(59), "0:59"); assert.equal(fmtClock(3661), "1:01:01");
  assert.equal(jobFraction({ state: "running", kind: "video", bytes: 50, total_bytes: 100 }), 0.5);
  // a playlist's bytes are the current item's: total progress comes from the items
  const pl = { state: "running", kind: "playlist", bytes: 90, total_bytes: 100, total_items: 2,
    items: [{ state: "done", progress: 1 }, { state: "running", progress: 0.5 }, { state: "skipped", progress: 0 }] };
  assert.equal(jobFraction(pl), 0.75);
  assert.equal(jobFraction({ state: "done" }), 1);
});

test("misc", () => {
  assert.ok(isToolsMissing("TOOLS_MISSING: x")); assert.ok(!isToolsMissing("other"));
  assert.equal(pollDelay({ hidden: true }), null);
  assert.ok(pollDelay({ busy: true }) < pollDelay({ busy: false }));
});

test("segment map: shapes from the backend, clamped and never trusted", () => {
  const v = segmentView({ source: "checkpoint", total: 1000, done: 700, segments: 2, cells: [100, 48.6, -3, 250, "x"], rows: [{ start: 0, end: 600, done: 600 }, { start: 600, end: 1000, done: 100 }] });
  assert.equal(v.source, "checkpoint"); assert.equal(v.segments, 2); assert.equal(v.total, 1000);
  assert.deepEqual(v.cells, [100, 49, 0, 100, 0]);
  assert.deepEqual(v.rows.map((r) => [r.n, r.pct]), [[1, 100], [2, 25]]);
  // a row claiming more saved bytes than its length is clamped
  assert.equal(segmentView({ source: "checkpoint", cells: [], rows: [{ start: 10, end: 20, done: 99 }] }).rows[0].pct, 100);
  // unknown source, null, garbage: nothing drawn
  for (const bad of [null, undefined, 7, "x", { source: "evil", cells: [100] }, { source: "none", cells: [100] }]) {
    const e = segmentView(bad);
    assert.equal(e.source, "none"); assert.deepEqual(e.cells, []); assert.deepEqual(e.rows, []);
  }
  // huge answers are capped
  assert.equal(segmentView({ source: "stream", cells: Array(5000).fill(100) }).cells.length, 1000);
  assert.equal(segmentView({ source: "checkpoint", cells: [], rows: Array(500).fill({ start: 0, end: 1, done: 1 }) }).rows.length, 64);
});

test("search: every word in name or link, case-insensitive; empty matches all", () => {
  const a = it(1, { name: "Ubuntu 24.04 Desktop.iso", url: "https://releases.example.com/u.iso" });
  assert.ok(matchesQuery(a, ""));
  assert.ok(matchesQuery(a, "  ubuntu  ISO "));
  assert.ok(matchesQuery(a, "releases desktop"));
  assert.ok(!matchesQuery(a, "ubuntu fedora"));
  assert.ok(matchesQuery(it(2, { name: "تقرير نهائي.pdf" }), "نهائي"));
  const s = new Map([[1, a], [2, it(2, { name: "x.zip" })]]);
  assert.deepEqual(buildView(s, "all", "id", -1, "ubuntu").map((x) => x.id), [1]);
});

test("columns: saved layout is repaired, widths clamped, name always shown, order kept", () => {
  const d = normalizeCols(null);
  assert.deepEqual(d.map((c) => c.key), COLUMNS.map((c) => c.key));
  const n = normalizeCols([{ key: "url", on: false, w: 5 }, { key: "evil" }, { key: "name", on: false, w: 99999 }, { key: "url" }, "x", null]);
  assert.deepEqual(n.slice(0, 2), [{ key: "url", on: false, w: 80 }, { key: "name", on: true, w: 1200 }]);
  assert.equal(n.length, COLUMNS.length);
  assert.equal(new Set(n.map((c) => c.key)).size, COLUMNS.length);
  assert.equal(gridTemplate(n).split(" ").length >= 6, true);
  assert.ok(!gridTemplate(n).includes("NaN"));
  const m = moveCol(d, "url", "name");
  assert.deepEqual(m.slice(0, 2).map((c) => c.key), ["url", "name"]);
  assert.deepEqual(moveCol(d, "name", null).at(-1).key, "name");
  assert.equal(moveCol(d, "nope", "name"), d);
});

test("toolbar: only known buttons can be hidden; labels on by default", () => {
  const t = normalizeToolbar({ hidden: ["remove", "add", "evil"], labels: false });
  assert.deepEqual([...t.hidden], ["remove"]); assert.equal(t.labels, false);
  assert.equal(normalizeToolbar("junk").labels, true);
});

test("dropped text: http(s) links only, deduplicated, uri-list comments skipped, bounded", () => {
  const txt = "# comment http://no.example\r\nhttps://a.example/x.zip\nsee (https://b.example/y.iso), and https://a.example/x.zip\nftp://c/z javascript:alert(1)\n";
  assert.deepEqual(linksFromText(txt), ["https://a.example/x.zip", "https://b.example/y.iso"]);
  assert.equal(linksFromText(Array(500).fill("https://h/p").map((u, i) => u + i).join("\n")).length, 200);
  assert.deepEqual(linksFromText(null), []);
});

test("notices: only items that changed to done or failed, never on the first list", () => {
  const s = new Map([[1, it(1, { status: "running" })], [2, it(2, { status: "done" })], [3, it(3, { status: "running" })]]);
  const r = finishedNow(s, { items: [it(1, { status: "done" }), it(2, { status: "done" }), it(3, { status: "failed" }), it(4, { status: "done" })] });
  assert.deepEqual(r.map((x) => [x.id, x.status]), [[1, "done"], [3, "failed"]]);
  assert.deepEqual(finishedNow(new Map(), { items: [it(1, { status: "done" })] }), []);
});

test("video pages: the same rule as eid_engine::links::video_page", () => {
  for (const [l, want] of [
    ["https://www.youtube.com/watch?v=dQw4w9WgXcQ", "video"], ["http://youtube.com/watch?v=abc&list=PL1&t=10s", "video"],
    ["https://m.youtube.com/watch?v=abc", "video"], ["https://music.youtube.com/watch?v=abc", "video"],
    ["https://WWW.YOUTUBE.COM./watch?v=abc", "video"], ["https://youtu.be/abc?si=x", "video"],
    ["https://www.youtube.com/shorts/abc", "video"], ["https://www.youtube.com/live/abc", "video"],
    ["https://www.youtube-nocookie.com/embed/abc", "video"], ["https://www.youtube.com/playlist?list=PL123", "playlist"],
    ["  https://www.youtube.com/watch?v=abc  ", "video"],
    ["https://www.youtube.com/", null], ["https://www.youtube.com/watch", null], ["https://www.youtube.com/watch?v=", null],
    ["https://www.youtube.com/playlist", null], ["https://www.youtube.com/@channel", null], ["https://www.youtube.com/shorts/", null],
    ["https://youtu.be/", null], ["https://youtu.be/a/b", null], ["https://notyoutube.com/watch?v=abc", null],
    ["https://youtube.com.evil.example/watch?v=abc", null], ["https://evil.example/www.youtube.com/watch?v=abc", null],
    ["https://u:p@www.youtube.com/watch?v=abc", null], ["ftp://www.youtube.com/watch?v=abc", null],
    ["javascript:alert(1)//youtube.com/watch?v=a", null], ["https://example.com/video.mp4", null], ["not a link", null], [null, null],
  ]) assert.equal(videoPage(l), want, String(l));
});
