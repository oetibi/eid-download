#!/usr/bin/env python3
"""Reproducible engine benchmark (not a test gate). Linux/Unix only (uses os.wait4).

Runs the real `eid get` binary (release) against the loopback `eid-benchserver` and reports, per
case: wall time, throughput, peak RSS of the `eid` process, CPU time (user+sys) and CPU seconds
per GiB. Every result file is verified by SHA-256 against the server's content hash; a mismatch
aborts the run (a fast wrong file is worth nothing).

Usage (from the repository root):
  cargo build --release -p eid-cli -p eid-testserver
  python3 scripts/bench/engine_bench.py --suite unthrottled --size 1073741824 --reps 3
  python3 scripts/bench/engine_bench.py --suite throttled --size 67108864 --throttle 4194304
  python3 scripts/bench/engine_bench.py --suite single --size 268435456

Options: --work DIR (where files are written; default ./target/bench-work, i.e. a real disk,
not tmpfs), --conns 1,2,4 (override the connection list), --extra "--ramp-ms 50" (passed to
`eid get` verbatim, for experiments), --json (one JSON object per line instead of a table).
Numbers depend on the machine; compare runs on the same machine, never quote as constants.
"""
import argparse
import hashlib
import json
import os
import shlex
import shutil
import statistics
import subprocess
import sys
import time

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
EID = os.path.join(ROOT, "target", "release", "eid")
SERVER = os.path.join(ROOT, "target", "release", "eid-benchserver")

SUITES = {
    "unthrottled": ("/big.bin", [1, 2, 4, 8, 16, 32]),
    "throttled": ("/throttled.bin", [1, 2, 4, 8, 16]),
    "single": ("/no-range.bin", [8]),
}


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while True:
            b = f.read(1 << 20)
            if not b:
                return h.hexdigest()
            h.update(b)


def start_server(size, throttle):
    p = subprocess.Popen(
        [SERVER, "--size", str(size), "--throttle-per-conn", str(throttle)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
    )
    url = sha = None
    while sha is None:
        line = p.stdout.readline()
        if not line:
            sys.exit("benchserver exited early")
        k, _, v = line.strip().partition(" ")
        if k == "URL":
            url = v
        elif k == "SHA256":
            sha = v
    return p, url, sha


def run_once(url, work, conns, extra):
    out = os.path.join(work, "run")
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(out)
    argv = [EID, "get", url, "-d", out, "-c", str(conns), "-q"] + extra
    t0 = time.monotonic()
    p = subprocess.Popen(argv, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    _, status, ru = os.wait4(p.pid, 0)
    wall = time.monotonic() - t0
    err = p.stderr.read().decode(errors="replace")
    p.stderr.close()
    code = os.waitstatus_to_exitcode(status)
    if code != 0:
        sys.exit(f"eid get failed ({code}): {err}")
    files = [f for f in os.listdir(out) if not f.startswith(".")]
    if len(files) != 1:
        sys.exit(f"expected exactly one result file, got {files}")
    path = os.path.join(out, files[0])
    return path, wall, ru


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--suite", choices=SUITES, required=True)
    ap.add_argument("--size", type=int, required=True)
    ap.add_argument("--throttle", type=int, default=4 * 1024 * 1024)
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--conns")
    ap.add_argument("--extra", default="")
    ap.add_argument("--work", default=os.path.join(ROOT, "target", "bench-work"))
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args()
    for b in (EID, SERVER):
        if not os.path.exists(b):
            sys.exit(f"missing {b}: cargo build --release -p eid-cli -p eid-testserver")
    path_part, conns_list = SUITES[a.suite]
    if a.conns:
        conns_list = [int(c) for c in a.conns.split(",")]
    extra = shlex.split(a.extra)
    os.makedirs(a.work, exist_ok=True)
    server, base, want = start_server(a.size, a.throttle)
    gib = a.size / (1 << 30)
    try:
        if not a.json:
            print(f"# suite={a.suite} size={a.size} throttle/conn={a.throttle if a.suite == 'throttled' else '-'} "
                  f"reps={a.reps} extra={a.extra!r} nproc={os.cpu_count()}")
            print("| conns | wall s (median) | min..max | MiB/s (median) | peak RSS MiB (max) | CPU s (median) | CPU s/GiB |")
            print("|---:|---:|---:|---:|---:|---:|---:|")
        for c in conns_list:
            walls, cpus, rss = [], [], []
            for _ in range(a.reps):
                path, wall, ru = run_once(base + path_part, a.work, c, extra)
                got = sha256_file(path)
                if got != want:
                    sys.exit(f"SHA-256 MISMATCH conns={c}: {got} != {want}")
                os.remove(path)
                walls.append(wall)
                cpus.append(ru.ru_utime + ru.ru_stime)
                rss.append(ru.ru_maxrss / 1024)  # Linux: KiB
            w = statistics.median(walls)
            cpu = statistics.median(cpus)
            row = {
                "suite": a.suite, "size": a.size, "conns": c, "reps": a.reps,
                "wall_median_s": round(w, 3), "wall_min_s": round(min(walls), 3),
                "wall_max_s": round(max(walls), 3),
                "mib_per_s": round(a.size / (1 << 20) / w, 1),
                "peak_rss_mib": round(max(rss), 1), "cpu_median_s": round(cpu, 3),
                "cpu_s_per_gib": round(cpu / gib, 3), "sha256_ok": True,
            }
            if a.json:
                print(json.dumps(row), flush=True)
            else:
                print(f"| {c} | {row['wall_median_s']} | {row['wall_min_s']}..{row['wall_max_s']} | "
                      f"{row['mib_per_s']} | {row['peak_rss_mib']} | {row['cpu_median_s']} | "
                      f"{row['cpu_s_per_gib']} |", flush=True)
    finally:
        server.stdin.close()
        server.wait(timeout=10)


if __name__ == "__main__":
    main()
