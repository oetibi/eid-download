#!/usr/bin/env node
// Build the Eid Download browser extension from one shared source:
//   extension/src  ->  extension/dist/chrome   (Chrome + Edge, MV3 service worker)
//                  ->  extension/dist/firefox  (Firefox, MV3 background script + gecko id)
// No npm dependencies. Usage: node scripts/ext/build.mjs [--out DIR]
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import zlib from "node:zlib";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const SRC = path.join(ROOT, "extension", "src");
const VERSION = "0.1.0";

/**
 * Public key that pins the Chrome/Edge extension id for unpacked installs (the native host
 * manifest's `allowed_origins` needs a stable id). Only the PUBLIC half is in the repository;
 * the private half was never written to disk. A Chrome Web Store / Edge Add-ons listing gets
 * its own store id, which must then be added to `allowed_origins` too (docs/EXTENSION.md).
 */
export const CHROME_KEY =
  "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAtea6+FT7iM7ZpkK9IZV2fQHuI7pPYxql6r/8hqfXOvAwx011qZQaJfB2Y4S3lB+oaISPyQ6/JiV49LCjeiDuxY5dWY3mzd6HD8UaZ7omigMDWvE8z0Ivf8pFKmZwv6uF41cSH53YO3vkyN9rwelI0SODCHV4s0RRuXg+nTeHOHkutxpWzn6Ki3Sc+BUuWDwO3qe1PNSowiVGpLVTWJS0Ngy2wQYncf3SWaL3EPgXG0KaZkGayH6GCXMW2RnPaNMoSbi4Y6gEHLj64LAv44FOEWTyqBCZfidkdvuqrDq0KpBdWu8+TjFGVXHRBd+LhFGv8fs2hVxAV/ol3/eGUucwKwIDAQAB";
export const CHROME_ID = "ambokeecedpgicighhcpdfjphdnphgaa";
export const FIREFOX_ID = "eid-download@eid-download";

/** Chrome's id rule: first 128 bits of SHA-256(DER public key), hex digits mapped 0-f -> a-p. */
export function chromeIdFromKey(b64) {
  const hex = createHash("sha256").update(Buffer.from(b64, "base64")).digest("hex").slice(0, 32);
  return [...hex].map((c) => String.fromCharCode(97 + parseInt(c, 16))).join("");
}

const CONTENT_MATCHES = ["https://*/*", "http://*/*"];

export function manifest(target) {
  const m = {
    manifest_version: 3,
    name: "__MSG_extName__",
    description: "__MSG_extDescription__",
    version: VERSION,
    default_locale: "en",
    icons: { 16: "icons/16.png", 32: "icons/32.png", 48: "icons/48.png", 128: "icons/128.png" },
    // least privilege: native messaging + settings. No tabs, cookies, webRequest, history.
    permissions: ["nativeMessaging", "storage"],
    content_scripts: [
      {
        matches: CONTENT_MATCHES,
        js: ["lib/core.js", "content.js"],
        run_at: "document_idle",
        all_frames: true,
      },
    ],
    options_ui: { page: "options.html", open_in_tab: true },
    action: { default_title: "__MSG_extName__", default_icon: { 16: "icons/16.png", 32: "icons/32.png" } },
  };
  if (target === "chrome") {
    m.key = CHROME_KEY;
    m.minimum_chrome_version = "116";
    m.background = { service_worker: "background.js" };
  } else if (target === "firefox") {
    m.background = { scripts: ["lib/core.js", "background.js"] };
    m.browser_specific_settings = {
      gecko: {
        id: FIREFOX_ID,
        strict_min_version: "128.0",
        // AMO data-collection declaration: the extension collects nothing
        data_collection_permissions: { required: ["none"] },
      },
    };
  } else {
    throw new Error("unknown target " + target);
  }
  return m;
}

// ---------------------------------------------------------------- icons (tiny PNG encoder)

function png(size, rgba) {
  const crc = (buf) => zlib.crc32(buf) >>> 0;
  const chunk = (type, data) => {
    const len = Buffer.alloc(4);
    len.writeUInt32BE(data.length);
    const td = Buffer.concat([Buffer.from(type, "ascii"), data]);
    const c = Buffer.alloc(4);
    c.writeUInt32BE(crc(td));
    return Buffer.concat([len, td, c]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // RGBA
  const raw = Buffer.alloc((size * 4 + 1) * size);
  for (let y = 0; y < size; y++) {
    raw[y * (size * 4 + 1)] = 0;
    rgba.copy(raw, y * (size * 4 + 1) + 1, y * size * 4, (y + 1) * size * 4);
  }
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", zlib.deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

function segDist(px, py, ax, ay, bx, by) {
  const dx = bx - ax;
  const dy = by - ay;
  const t = Math.max(0, Math.min(1, ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)));
  return Math.hypot(px - ax - t * dx, py - ay - t * dy);
}

/** Teal rounded square with a white "download" arrow (same path as the in-page button). */
export function icon(size) {
  const buf = Buffer.alloc(size * size * 4);
  const s = size / 24; // the button's 24-unit viewBox
  const r = 5 * s;
  const stroke = 2.4 * s;
  const segs = [
    [12, 4, 12, 14.5],
    [12, 14.5, 7.5, 10],
    [12, 14.5, 16.5, 10],
    [5, 17, 5, 19.5],
    [5, 19.5, 19, 19.5],
    [19, 19.5, 19, 17],
  ];
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const px = x + 0.5;
      const py = y + 0.5;
      // rounded-rect signed distance
      const qx = Math.abs(px - size / 2) - (size / 2 - r);
      const qy = Math.abs(py - size / 2) - (size / 2 - r);
      const d = Math.hypot(Math.max(qx, 0), Math.max(qy, 0)) + Math.min(Math.max(qx, qy), 0) - r;
      const bg = Math.max(0, Math.min(1, 0.5 - d));
      let dm = Infinity;
      for (const [ax, ay, bx, by] of segs) dm = Math.min(dm, segDist(px, py, ax * s, ay * s, bx * s, by * s));
      const fg = Math.max(0, Math.min(1, stroke / 2 + 0.5 - dm));
      const i = (y * size + x) * 4;
      // teal #0f766e blended with white by fg
      buf[i] = Math.round(15 + (255 - 15) * fg);
      buf[i + 1] = Math.round(118 + (255 - 118) * fg);
      buf[i + 2] = Math.round(110 + (255 - 110) * fg);
      buf[i + 3] = Math.round(255 * bg);
    }
  }
  return png(size, buf);
}

// ---------------------------------------------------------------- build

function copyTree(from, to) {
  fs.mkdirSync(to, { recursive: true });
  for (const e of fs.readdirSync(from, { withFileTypes: true })) {
    const a = path.join(from, e.name);
    const b = path.join(to, e.name);
    if (e.isDirectory()) copyTree(a, b);
    else if (e.isFile()) fs.copyFileSync(a, b);
  }
}

export function build(outRoot) {
  if (chromeIdFromKey(CHROME_KEY) !== CHROME_ID) throw new Error("CHROME_ID does not match CHROME_KEY");
  const out = {};
  for (const target of ["chrome", "firefox"]) {
    const dir = path.join(outRoot, target);
    // rebuild into a fresh directory (only ever our own build output)
    fs.rmSync(dir, { recursive: true, force: true });
    copyTree(SRC, dir);
    fs.mkdirSync(path.join(dir, "icons"), { recursive: true });
    for (const sz of [16, 32, 48, 128]) fs.writeFileSync(path.join(dir, "icons", `${sz}.png`), icon(sz));
    fs.writeFileSync(path.join(dir, "manifest.json"), JSON.stringify(manifest(target), null, 2) + "\n");
    out[target] = dir;
  }
  return out;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const i = process.argv.indexOf("--out");
  const outRoot = i > 0 ? path.resolve(process.argv[i + 1]) : path.join(ROOT, "extension", "dist");
  const out = build(outRoot);
  for (const [k, v] of Object.entries(out)) console.log(`${k}: ${v}`);
  console.log(`chrome/edge id: ${CHROME_ID}`);
  console.log(`firefox id:     ${FIREFOX_ID}`);
}
