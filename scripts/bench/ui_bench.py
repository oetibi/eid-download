#!/usr/bin/env python3
"""UI poll-cost benchmark in headless Chrome (not a test gate; Blink = the engine of WebView2).

Serves the repository on a loopback-only static server (127.0.0.1, ephemeral port, for the
duration of the run), opens scripts/bench/ui_bench.html for each list size, and prints the time
one `refresh()` of crates/eid-gui/ui/app.js takes (fake IPC + render, then + forced layout).
The real app polls every 700 ms, so cost/700 ms is the share of one core the list costs.

Usage (repository root):  python3 scripts/bench/ui_bench.py [--sizes 10,100,500,1000,2000]
                          [--running 8] [--polls 30] [--chrome google-chrome]
"""
import argparse
import functools
import http.server
import json
import os
import re
import subprocess
import threading

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))


class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *a):
        pass


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sizes", default="10,100,500,1000,2000")
    ap.add_argument("--running", type=int, default=8)
    ap.add_argument("--polls", type=int, default=30)
    ap.add_argument("--chrome", default="google-chrome")
    a = ap.parse_args()
    handler = functools.partial(Quiet, directory=ROOT)
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    port = srv.server_address[1]
    print("| items | running | first load ms | poll ms (median) | poll ms (p95) | poll+layout ms (median) | poll+layout ms (p95) | JSON bytes/poll |")
    print("|---:|---:|---:|---:|---:|---:|---:|---:|")
    try:
        for n in [int(x) for x in a.sizes.split(",")]:
            url = f"http://127.0.0.1:{port}/scripts/bench/ui_bench.html?n={n}&running={min(a.running, n)}&polls={a.polls}"
            out = subprocess.run(
                [a.chrome, "--headless=new", "--no-sandbox", "--disable-gpu",
                 "--window-size=1200,900", "--virtual-time-budget=120000", "--dump-dom", url],
                capture_output=True, text=True, timeout=120,
            ).stdout
            m = re.search(r"RESULT (\{.*?\})", out)
            if not m:
                raise SystemExit(f"no result for n={n}:\n{out[-2000:]}")
            r = json.loads(m.group(1))
            assert r["rows"] == n, r
            print(f"| {n} | {r['running']} | {r['first_load_ms']} | {r['poll_script_median_ms']} | "
                  f"{r['poll_script_p95_ms']} | {r['poll_total_median_ms']} | {r['poll_total_p95_ms']} | "
                  f"{r['payload_bytes']} |", flush=True)
    finally:
        srv.shutdown()


if __name__ == "__main__":
    main()
