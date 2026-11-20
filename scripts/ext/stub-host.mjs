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
// v0.2: what the app's probe answers now (eid_media::choice): exact format pairs with details
const V_QUALITIES = ["4320p", "2160p", "1080p", "720p", "360p", "best", "audio", "m4a"];
const OPTIONS = [
  { quality: "4320p", height: 4320, format: "402+140", container: "mp4", fps: 60, codec: "AV1", hdr: null, audio: true, size: 603000000 },
  { quality: "2160p", height: 2160, format: "337+140", container: "mp4", fps: 60, codec: "VP9", hdr: "HDR10", audio: true, size: null },
  { quality: "1080p", height: 1080, format: "299+140", container: "mp4", fps: 60, codec: "H.264", hdr: null, audio: true, size: 52400000 },
  { quality: "720p", height: 720, format: "298+140", container: "mp4", fps: 60, codec: "H.264", hdr: null, audio: true, size: 26200000 },
  { quality: "360p", height: 360, format: "18", container: "mp4", fps: 30, codec: "H.264", hdr: null, audio: true, size: 9000000 },
];
const AUDIO_OPTIONS = [
  { quality: "m4a", original: true, format: "140", codec: "AAC", kbps: 129, size: 3000000 },
  { quality: "audio", original: false, format: null, codec: "MP3", kbps: 129, size: null },
];
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
      // an app from before v0.2 (no details): v=OLDAPPOLDAP
      if (v === "OLDAPPOLDAP") return reply(req.id, true, { kind: "video", title: `Video ${v}`, qualities: QUALITIES, duration: 120 });
      return reply(req.id, true, { kind: "video", title: `Video ${v}`, qualities: V_QUALITIES, options: OPTIONS, audio_options: AUDIO_OPTIONS, duration: 120 });
    }
    case "open_video":
      // the headless background server has no window (eid serve): list=PLnowindow
      if (/[?&]list=PLnowindow/.test(c.url ?? "")) return reply(req.id, false, "no window: this is the background server; open Eid Download and use Video");
      return reply(req.id, true, { opened: true });
    case "add":
    case "add_video":
    case "add_playlist":
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
