#!/usr/bin/env python3
"""One version everywhere: [workspace.package] version in Cargo.toml == crates/eid-gui/tauri.conf.json
"version" == every workspace crate in Cargo.lock, and (if given) == the tag without its "v".
Usage: check_version.py [--tag v0.1.0]. Exit 1 with the mismatch otherwise."""
import argparse
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]


def versions(root=ROOT):
    cargo = (root / "Cargo.toml").read_text(encoding="utf-8")
    m = re.search(r'^\[workspace\.package\][^\[]*?^version\s*=\s*"([^"]+)"', cargo, re.M | re.S)
    if not m:
        raise ValueError("no [workspace.package] version in Cargo.toml")
    tauri = json.loads((root / "crates/eid-gui/tauri.conf.json").read_text(encoding="utf-8"))
    lock = (root / "Cargo.lock").read_text(encoding="utf-8")
    local = {}
    for blk in lock.split("[[package]]"):
        n = re.search(r'^name = "([^"]+)"', blk, re.M)
        v = re.search(r'^version = "([^"]+)"', blk, re.M)
        if n and v and n.group(1).startswith("eid-") and "\nsource = " not in blk:
            local[n.group(1)] = v.group(1)
    return m.group(1), tauri.get("version"), local


def check(workspace, tauri, lock, tag=None):
    errs = []
    if tauri != workspace:
        errs.append(f"tauri.conf.json version {tauri} != Cargo.toml {workspace}")
    if not lock:
        errs.append("no eid-* crates in Cargo.lock")
    for n, v in sorted(lock.items()):
        if v != workspace:
            errs.append(f"Cargo.lock {n} {v} != Cargo.toml {workspace} (run `cargo update -w`)")
    if tag is not None and tag != f"v{workspace}":
        errs.append(f"tag {tag} != v{workspace}")
    return errs


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--tag")
    a = ap.parse_args(argv)
    w, t, l = versions()
    errs = check(w, t, l, a.tag)
    for e in errs:
        print(f"check_version: {e}", file=sys.stderr)
    if not errs:
        print(f"version {w}: Cargo.toml, Cargo.lock ({len(l)} crates), tauri.conf.json"
              + (f", tag {a.tag}" if a.tag else "") + " agree")
    return 1 if errs else 0


if __name__ == "__main__":
    sys.exit(main())
