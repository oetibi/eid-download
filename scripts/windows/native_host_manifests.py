#!/usr/bin/env python3
"""Write the Native Messaging host manifests that the Windows installer ships.

Input: extension/native-host-ids.json (the one place the extension IDs live).
Output (into OUT_DIR): <host>.chrome.json, <host>.edge.json, <host>.firefox.json, <host>.brave.json, one per
browser whose ID list is not empty. The installer copies them next to eid-native-host.exe and
its NSIS hook (crates/eid-gui/installer/hooks.nsh) points the HKCU keys at the files that exist.

`path` in the manifests is the bare file name "eid-native-host.exe": on Windows, Chrome, Edge
and Firefox resolve a relative path against the directory of the manifest itself, so the
manifests do not depend on where the user installs the app.

The ID rules are the same as crates/eid-platform/src/native_host.rs (Manifest::validate).
`--release` refuses a file still marked "placeholder": true (used by tag builds).
Exit 1 with a message on any invalid input; nothing is written in that case.
"""
import argparse
import json
import pathlib
import re
import sys

EXE = "eid-native-host.exe"
DESCRIPTION = "Eid Download native messaging host"
_HOST = re.compile(r"[a-z0-9_.]{1,64}")
_CHROME = re.compile(r"[a-p]{32}")
_FIREFOX = re.compile(r"[A-Za-z0-9._@\-{}]{1,128}")


class Invalid(Exception):
    pass


def _ids(cfg, key, pattern):
    v = cfg.get(key, [])
    if not isinstance(v, list) or not all(isinstance(i, str) for i in v):
        raise Invalid(f"'{key}' must be a list of strings")
    for i in v:
        if not pattern.fullmatch(i):
            raise Invalid(f"invalid {key} extension id: {i!r}")
    if len(set(v)) != len(v):
        raise Invalid(f"duplicate id in '{key}'")
    return v


def build(cfg, release=False):
    """Return {file name: manifest dict}. Raises Invalid."""
    if not isinstance(cfg, dict):
        raise Invalid("the config must be a JSON object")
    placeholder = cfg.get("placeholder")
    if not isinstance(placeholder, bool):
        raise Invalid("'placeholder' must be true or false")
    if release and placeholder:
        raise Invalid("extension/native-host-ids.json still has placeholder IDs "
                      "(set the real IDs and \"placeholder\": false, or empty lists)")
    name = cfg.get("host_name")
    if not isinstance(name, str) or not _HOST.fullmatch(name) or name.startswith(".") \
            or name.endswith(".") or ".." in name:
        raise Invalid(f"invalid host_name: {name!r}")
    chrome = _ids(cfg, "chrome", _CHROME)
    edge = _ids(cfg, "edge", _CHROME)
    firefox = _ids(cfg, "firefox", _FIREFOX)
    unknown = set(cfg) - {"_comment", "placeholder", "host_name", "chrome", "edge", "firefox"}
    if unknown:
        raise Invalid(f"unknown keys: {sorted(unknown)}")

    def base():
        return {"name": name, "description": DESCRIPTION, "path": EXE, "type": "stdio"}

    out = {}
    if chrome:
        m = base()
        m["allowed_origins"] = [f"chrome-extension://{i}/" for i in chrome]
        out[f"{name}.chrome.json"] = m
    # Edge also runs extensions installed from the Chrome Web Store, with their Chrome ID
    edge_all = edge + [i for i in chrome if i not in edge]
    if edge_all:
        m = base()
        m["allowed_origins"] = [f"chrome-extension://{i}/" for i in edge_all]
        out[f"{name}.edge.json"] = m
    if firefox:
        m = base()
        m["allowed_extensions"] = list(firefox)
        out[f"{name}.firefox.json"] = m
    # v0.2: Brave runs the Chrome build (and Chrome Web Store installs) with the Chrome IDs,
    # but reads its own registry key
    if chrome:
        m = base()
        m["allowed_origins"] = [f"chrome-extension://{i}/" for i in chrome]
        out[f"{name}.brave.json"] = m
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("config", type=pathlib.Path)
    ap.add_argument("out_dir", type=pathlib.Path)
    ap.add_argument("--release", action="store_true",
                    help="refuse a config still marked placeholder")
    a = ap.parse_args(argv)
    try:
        cfg = json.loads(a.config.read_text(encoding="utf-8"))
        files = build(cfg, release=a.release)
    except (Invalid, ValueError, OSError) as e:
        print(f"native_host_manifests: {e}", file=sys.stderr)
        return 1
    a.out_dir.mkdir(parents=True, exist_ok=True)
    for f, m in files.items():
        (a.out_dir / f).write_text(json.dumps(m, indent=2) + "\n", encoding="utf-8")
        print(f"wrote {a.out_dir / f}")
    if not files:
        print("no extension IDs: no browser will be registered")
    if cfg.get("placeholder"):
        print("WARNING: placeholder extension IDs (not a release build)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
