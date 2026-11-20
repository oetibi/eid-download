#!/usr/bin/env node
// Stand-in for eid-native-host in the browser end-to-end tests (scripts/ext/browser_e2e.mjs):
// speaks Native Messaging (4-byte little-endian length + JSON) on stdin/stdout exactly like the
// real host, answers the app's commands with fixed data, and appends every request it receives
// to the file named by EID_STUB_LOG (one JSON line each), so the test can check what the
// extension really sent. It never touches the network. The real host is tested in Rust
// (crates/eid-native-host) and with the real app on Windows (scripts/windows/window_e2e.mjs).
import { appendFileSync } from "node:fs";

const LOG = process.env.EID_STUB_LOG;
const QUALITIES = ["2160p", "1080p", "720p", "360p", "best", "audio"];
let buf = Buffer.alloc(0);
let jobs = 0;

function reply(id, ok, payload) {
  const body = Buffer.from(JSON.stringify(ok ? { v: 1, id, ok: true, result: payload } : { v: 1, id, ok: false, error: payload }));
  const len = Buffer.alloc(4);
  len.writeUInt32LE(body.length);
  process.stdout.write(Buffer.concat([len, body]));
}

function answer(req) {
  const c = req.command ?? {};
  if (LOG) appendFileSync(LOG, JSON.stringify(c) + "\n");
  switch (c.cmd) {
    case "ping":
      return reply(req.id, true, { pong: true, app: "gui", version: "0.2.0" });
    case "probe": {
      const u = new URL(c.url);
      const list = u.searchParams.get("list");
      if (u.pathname === "/playlist" && list) {
        if (list === "PLgone") return reply(req.id, false, "unsupported url: this playlist does not exist or is private");
        const items = Array.from({ length: 120 }, (_, i) => ({ index: i + 1, title: `Test video ${i + 1}`, duration: 1500 + i }));
        return reply(req.id, true, { kind: "playlist", title: "Test playlist", items, qualities: QUALITIES });
      }
      const v = u.searchParams.get("v");
      return reply(req.id, true, { kind: "video", title: `Video ${v}`, qualities: QUALITIES, duration: 120 });
    }
    case "add":
    case "add_video":
    case "add_playlist":
    case "open_video":
      return reply(req.id, true, { job: ++jobs });
    default:
      return reply(req.id, false, "command not allowed from the browser");
  }
}

process.stdin.on("data", (d) => {
  buf = Buffer.concat([buf, d]);
  while (buf.length >= 4) {
    const n = buf.readUInt32LE(0);
    if (buf.length < 4 + n) break;
    const msg = JSON.parse(buf.subarray(4, 4 + n).toString("utf8"));
    buf = buf.subarray(4 + n);
    answer(msg);
  }
});
process.stdin.on("end", () => process.exit(0));
