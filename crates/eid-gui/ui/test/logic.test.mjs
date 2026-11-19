// Unit tests of the window's pure logic (node --test, no browser).
import test from "node:test";
import assert from "node:assert/strict";
import { mergeList, categoryOf, inView, buildView, clickSelect, safeQualities, fmtSize, fmtClock, jobFraction, isToolsMissing, pollDelay, segmentView } from "../logic.js";

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
