// Build output and native-host manifest tests. Run: node --test extension/test/
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import zlib from "node:zlib";
import { build, manifest, chromeIdFromKey, CHROME_KEY, CHROME_ID, FIREFOX_ID, icon } from "../../scripts/ext/build.mjs";
import { hostManifest } from "../../scripts/ext/host-manifest.mjs";

test("pinned Chrome/Edge id matches the public key", () => {
  assert.equal(chromeIdFromKey(CHROME_KEY), CHROME_ID);
  assert.match(CHROME_ID, /^[a-p]{32}$/);
});

test("manifests: MV3, least privilege, per-browser background", () => {
  const c = manifest("chrome");
  const f = manifest("firefox");
  for (const m of [c, f]) {
    assert.equal(m.manifest_version, 3);
    assert.deepEqual(m.permissions, ["nativeMessaging", "storage"]);
    assert.deepEqual(m.content_scripts[0].js, ["lib/core.js", "content.js"]);
    assert.equal(m.default_locale, "en");
  }
  // Chrome/Edge/Brave grant content-script matches themselves: no host permissions asked.
  // Firefox MV3 grants only declared host permissions, so it declares exactly the content
  // script's matches (no more than the content script already needs; owner's report: no button
  // on Firefox 160).
  assert.equal(c.host_permissions, undefined);
  assert.deepEqual(f.host_permissions, f.content_scripts[0].matches);
  assert.equal(c.background.service_worker, "background.js");
  assert.equal(c.key, CHROME_KEY);
  assert.equal(c.browser_specific_settings, undefined);
  assert.deepEqual(f.background.scripts, ["lib/core.js", "background.js"]);
  assert.equal(f.background.service_worker, undefined);
  assert.equal(f.browser_specific_settings.gecko.id, FIREFOX_ID);
  assert.equal(f.key, undefined);
  assert.throws(() => manifest("safari"));
});

test("build writes both targets with every referenced file", () => {
  const out = fs.mkdtempSync(path.join(os.tmpdir(), "eid-ext-build-"));
  try {
    const dirs = build(out);
    for (const target of ["chrome", "firefox"]) {
      const dir = dirs[target];
      const m = JSON.parse(fs.readFileSync(path.join(dir, "manifest.json"), "utf8"));
      const refs = [
        ...Object.values(m.icons),
        ...m.content_scripts.flatMap((c) => c.js),
        m.options_ui.page,
        ...(m.background.scripts || [m.background.service_worker]),
        "_locales/en/messages.json",
        "_locales/ar/messages.json",
        "options.js",
        "options.css",
      ];
      for (const r of refs) assert.ok(fs.statSync(path.join(dir, r)).isFile(), `${target}: ${r}`);
      // the service worker imports core.js by a relative path that must exist
      assert.match(fs.readFileSync(path.join(dir, "background.js"), "utf8"), /importScripts\("lib\/core\.js"\)/);
    }
  } finally {
    fs.rmSync(out, { recursive: true, force: true });
  }
});

test("icons are valid PNGs of the right size", () => {
  for (const sz of [16, 32, 48, 128]) {
    const b = icon(sz);
    assert.deepEqual([...b.subarray(0, 8)], [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    assert.equal(b.toString("ascii", 12, 16), "IHDR");
    assert.equal(b.readUInt32BE(16), sz);
    assert.equal(b.readUInt32BE(20), sz);
    const idatLen = b.readUInt32BE(33);
    assert.equal(b.toString("ascii", 37, 41), "IDAT");
    const raw = zlib.inflateSync(b.subarray(41, 41 + idatLen));
    assert.equal(raw.length, (sz * 4 + 1) * sz);
  }
});

test("host manifest: same fields as eid-platform native_host.rs, ids validated", () => {
  const exe = "C:\\Program Files\\Eid Download\\eid-native-host.exe";
  const c = hostManifest("chrome", exe);
  assert.deepEqual(c, {
    name: "com.eid.download",
    description: "Eid Download native host",
    path: exe,
    type: "stdio",
    allowed_origins: [`chrome-extension://${CHROME_ID}/`],
  });
  assert.deepEqual(hostManifest("edge", exe).allowed_origins, c.allowed_origins);
  const f = hostManifest("firefox", "/opt/eid/eid-native-host");
  assert.deepEqual(f.allowed_extensions, [FIREFOX_ID]);
  assert.equal(f.allowed_origins, undefined);
  assert.throws(() => hostManifest("chrome", "eid-native-host.exe"), /absolute/);
  assert.throws(() => hostManifest("chrome", "C:\\a\nb.exe"), /control/);
  assert.throws(() => hostManifest("chrome", exe, { chromeIds: [] }), /id/);
  assert.throws(() => hostManifest("chrome", exe, { chromeIds: ["*"] }), /id/);
  assert.throws(() => hostManifest("firefox", exe, { firefoxIds: ["a b"] }), /id/);
  assert.throws(() => hostManifest("opera", exe), /browser/);
});
