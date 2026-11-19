#!/usr/bin/env node
// Print the Native Messaging host manifest for one browser (same fields and rules as
// crates/eid-platform/src/native_host.rs `Manifest::json`).
//   node scripts/ext/host-manifest.mjs chrome|edge|firefox <absolute path to eid-native-host(.exe)>
//        [--chrome-id ID]... [--firefox-id ID]...
// Defaults to the pinned ids of the unpacked build (scripts/ext/build.mjs).
import path from "node:path";
import { fileURLToPath } from "node:url";
import { CHROME_ID, FIREFOX_ID } from "./build.mjs";

export const HOST_NAME = "com.eid.download";

const validChromeId = (id) => /^[a-p]{32}$/.test(id);
const validFirefoxId = (id) => /^[A-Za-z0-9._@{}-]{1,128}$/.test(id);

export function hostManifest(browser, exe, { chromeIds = [CHROME_ID], firefoxIds = [FIREFOX_ID] } = {}) {
  if (typeof exe !== "string" || !(path.isAbsolute(exe) || path.win32.isAbsolute(exe))) {
    throw new Error("the native host path must be absolute");
  }
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u001f\u007f]/.test(exe)) throw new Error("control character in the native host path");
  const m = { name: HOST_NAME, description: "Eid Download native host", path: exe, type: "stdio" };
  if (browser === "chrome" || browser === "edge") {
    if (!chromeIds.length || !chromeIds.every(validChromeId)) throw new Error("invalid Chrome/Edge extension id");
    m.allowed_origins = chromeIds.map((i) => `chrome-extension://${i}/`);
  } else if (browser === "firefox") {
    if (!firefoxIds.length || !firefoxIds.every(validFirefoxId)) throw new Error("invalid Firefox extension id");
    m.allowed_extensions = firefoxIds.slice();
  } else {
    throw new Error("browser must be chrome, edge or firefox");
  }
  return m;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [browser, exe, ...rest] = process.argv.slice(2);
  const chromeIds = [];
  const firefoxIds = [];
  for (let i = 0; i < rest.length; i += 2) {
    if (rest[i] === "--chrome-id") chromeIds.push(rest[i + 1]);
    else if (rest[i] === "--firefox-id") firefoxIds.push(rest[i + 1]);
    else {
      console.error("unknown argument " + rest[i]);
      process.exit(2);
    }
  }
  try {
    const m = hostManifest(browser, exe, {
      chromeIds: chromeIds.length ? chromeIds : [CHROME_ID],
      firefoxIds: firefoxIds.length ? firefoxIds : [FIREFOX_ID],
    });
    process.stdout.write(JSON.stringify(m, null, 2) + "\n");
  } catch (e) {
    console.error(e.message);
    console.error("usage: host-manifest.mjs chrome|edge|firefox <absolute exe path> [--chrome-id ID] [--firefox-id ID]");
    process.exit(2);
  }
}
