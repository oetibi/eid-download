#!/usr/bin/env node
// Store packages for the browser extension (owner step: uploading them, docs/OWNER-ACTIONS.md).
//   node scripts/ext/package.mjs [--out DIR]     (default extension/dist/store)
// Writes, from a fresh build of extension/src:
//   eid-download-chrome-<v>.zip   Chrome Web Store AND Edge Add-ons. The `key` field is removed:
//                                 the stores refuse it and give the listing its own id, which must
//                                 then be added to extension/native-host-ids.json.
//   eid-download-firefox-<v>.zip  addons.mozilla.org (gecko id kept: it is the AMO id).
//   eid-download-chrome-sideload-<v>.zip  NOT for a store: keeps `key`, so an unpacked install in
//                                 Chrome/Edge gets the pinned id the installer already registers.
//   SHA256SUMS.txt
// Plain, unminified sources, so no separate source archive is needed for AMO review.
// Deterministic: fixed timestamps and sorted entries, so the same source gives the same bytes.
// No npm dependencies (zip "deflate" via node:zlib).
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import zlib from "node:zlib";
import { fileURLToPath } from "node:url";
import { build } from "./build.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");

function files(dir, base = dir) {
  const out = [];
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) out.push(...files(p, base));
    else if (e.isFile()) out.push(path.relative(base, p).split(path.sep).join("/"));
  }
  return out.sort();
}

/** Minimal ZIP writer (deflate, no zip64; extension packages are small). */
export function zip(entries) {
  const DOS_TIME = 0, DOS_DATE = (2026 - 1980) << 9 | 1 << 5 | 1; // 2026-01-01 00:00
  const parts = [], central = [];
  let offset = 0;
  for (const { name, data } of entries) {
    const nameBuf = Buffer.from(name, "utf8");
    const comp = zlib.deflateRawSync(data, { level: 9 });
    const crc = zlib.crc32(data) >>> 0;
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0); local.writeUInt16LE(20, 4); local.writeUInt16LE(0x0800, 6);
    local.writeUInt16LE(8, 8); local.writeUInt16LE(DOS_TIME, 10); local.writeUInt16LE(DOS_DATE, 12);
    local.writeUInt32LE(crc, 14); local.writeUInt32LE(comp.length, 18); local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(nameBuf.length, 26); local.writeUInt16LE(0, 28);
    parts.push(local, nameBuf, comp);
    const c = Buffer.alloc(46);
    c.writeUInt32LE(0x02014b50, 0); c.writeUInt16LE(20, 4); c.writeUInt16LE(20, 6); c.writeUInt16LE(0x0800, 8);
    c.writeUInt16LE(8, 10); c.writeUInt16LE(DOS_TIME, 12); c.writeUInt16LE(DOS_DATE, 14);
    c.writeUInt32LE(crc, 16); c.writeUInt32LE(comp.length, 20); c.writeUInt32LE(data.length, 24);
    c.writeUInt16LE(nameBuf.length, 28); c.writeUInt32LE(offset, 42);
    central.push(c, nameBuf);
    offset += local.length + nameBuf.length + comp.length;
  }
  const cd = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0); end.writeUInt16LE(entries.length, 8); end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(cd.length, 12); end.writeUInt32LE(offset, 16);
  return Buffer.concat([...parts, cd, end]);
}

export function packageAll(outDir) {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "eid-ext-"));
  try {
    const built = build(tmp);
    fs.mkdirSync(outDir, { recursive: true });
    const sums = [];
    for (const target of ["chrome", "firefox", "chrome-sideload"]) {
      const dir = built[target === "chrome-sideload" ? "chrome" : target];
      const man = JSON.parse(fs.readFileSync(path.join(dir, "manifest.json"), "utf8"));
      if (target === "chrome") delete man.key;
      const entries = files(dir).map((name) => ({
        name,
        data: name === "manifest.json" ? Buffer.from(JSON.stringify(man, null, 2) + "\n") : fs.readFileSync(path.join(dir, name)),
      }));
      const file = `eid-download-${target}-${man.version}.zip`;
      const buf = zip(entries);
      fs.writeFileSync(path.join(outDir, file), buf);
      sums.push(`${createHash("sha256").update(buf).digest("hex")}  ${file}`);
    }
    fs.writeFileSync(path.join(outDir, "SHA256SUMS.txt"), sums.join("\n") + "\n");
    return sums;
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const i = process.argv.indexOf("--out");
  const out = i > 0 ? path.resolve(process.argv[i + 1]) : path.join(ROOT, "extension", "dist", "store");
  for (const l of packageAll(out)) console.log(l);
  console.log(`-> ${out}`);
}
