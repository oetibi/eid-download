// Store packages: valid ZIPs, deterministic, Chrome package without `key`, Firefox keeps its gecko id.
import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import zlib from "node:zlib";
import { packageAll } from "../../scripts/ext/package.mjs";

function readZip(buf) {
  const end = buf.lastIndexOf(Buffer.from([0x50, 0x4b, 0x05, 0x06]));
  assert.ok(end >= 0, "end of central directory");
  const n = buf.readUInt16LE(end + 10);
  let p = buf.readUInt32LE(end + 16);
  const out = new Map();
  for (let i = 0; i < n; i++) {
    assert.equal(buf.readUInt32LE(p), 0x02014b50);
    const crc = buf.readUInt32LE(p + 16), csize = buf.readUInt32LE(p + 20), nlen = buf.readUInt16LE(p + 28);
    const name = buf.toString("utf8", p + 46, p + 46 + nlen);
    const lo = buf.readUInt32LE(p + 42);
    assert.equal(buf.readUInt32LE(lo), 0x04034b50);
    const start = lo + 30 + buf.readUInt16LE(lo + 26) + buf.readUInt16LE(lo + 28);
    const data = zlib.inflateRawSync(buf.subarray(start, start + csize));
    assert.equal(zlib.crc32(data) >>> 0, crc, `crc of ${name}`);
    out.set(name, data);
    p += 46 + nlen + buf.readUInt16LE(p + 30) + buf.readUInt16LE(p + 32);
  }
  return out;
}

test("store packages are valid, deterministic and store-ready", () => {
  const a = fs.mkdtempSync(path.join(os.tmpdir(), "eid-pkg-a-"));
  const b = fs.mkdtempSync(path.join(os.tmpdir(), "eid-pkg-b-"));
  try {
    const s1 = packageAll(a), s2 = packageAll(b);
    assert.deepEqual(s1, s2, "same source, same bytes");
    const chrome = readZip(fs.readFileSync(path.join(a, "eid-download-chrome-0.1.0.zip")));
    const firefox = readZip(fs.readFileSync(path.join(a, "eid-download-firefox-0.1.0.zip")));
    for (const z of [chrome, firefox]) {
      for (const f of ["manifest.json", "background.js", "content.js", "lib/core.js", "icons/128.png", "_locales/ar/messages.json", "_locales/en/messages.json"]) assert.ok(z.has(f), f);
      for (const name of z.keys()) assert.ok(!name.startsWith("/") && !name.includes(".."), name);
    }
    const mc = JSON.parse(chrome.get("manifest.json"));
    const mf = JSON.parse(firefox.get("manifest.json"));
    assert.equal(mc.key, undefined, "stores refuse the key field");
    assert.equal(mf.browser_specific_settings.gecko.id, "eid-download@eid-download");
    assert.deepEqual(mc.permissions.sort(), ["nativeMessaging", "storage"]);
    const side = readZip(fs.readFileSync(path.join(a, "eid-download-chrome-sideload-0.1.0.zip")));
    assert.ok(JSON.parse(side.get("manifest.json")).key, "sideload build keeps the pinned key");
  } finally {
    fs.rmSync(a, { recursive: true, force: true });
    fs.rmSync(b, { recursive: true, force: true });
  }
});
