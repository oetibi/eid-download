#!/usr/bin/env python3
"""Run `cargo test --workspace [--release]` and verify every name in required_tests.txt
executed and passed (not ignored, not missing). Exit 1 otherwise."""
import re, subprocess, sys, pathlib
root = pathlib.Path(__file__).resolve().parent.parent
import platform

# Windows consoles default to cp1252: printing the failure marker must never crash the checker
# (CI run 43387076026). A failed check must stay visible AND exit non-zero.
for _s in (sys.stdout, sys.stderr):
    try:
        _s.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass
_win = platform.system() == "Windows"
req = []
not_tested = []
for l in (root / "scripts/required_tests.txt").read_text().splitlines():
    l = l.strip()
    if not l or l.startswith("#"):
        continue
    if l.startswith("unix:"):          # genuinely Unix-only test (symlinks, rustls backend, SIGINT)
        if _win:
            not_tested.append(l[5:])
            continue
        l = l[5:]
    req.append(l)
extra = sys.argv[1:]
p = subprocess.run(["cargo", "test", "--workspace", "--no-fail-fast", *extra, "--", "--test-threads=4"],
                   cwd=root, capture_output=True, text=True)
out = p.stdout + p.stderr
sys.stdout.write(out[-4000:])
for m in re.finditer(r"^---- .*? stdout ----\n(.*?)(?=^---- |^failures:)", out, re.M | re.S):
    sys.stdout.write("\nFAILURE DETAIL: " + m.group(0)[:3000])
results = {}
for m in re.finditer(r"^test (\S+) \.\.\. (ok|FAILED|ignored)", out, re.M):
    results[m.group(1)] = m.group(2)
bad = []
for name in req:
    hits = [k for k in results if k == name or k.endswith("::" + name)]
    if not hits: bad.append((name, "NOT RUN"))
    elif any(results[h] != "ok" for h in hits): bad.append((name, ",".join(results[h] for h in hits)))
if not_tested:
    print("\nNOT TESTED ON THIS PLATFORM (Unix-only required tests; they do NOT count as passed):")
    for n in not_tested: print("  -", n)
print(f"\nrequired: {len(req)}  ok: {len(req)-len(bad)}  bad: {len(bad)}")
for n, why in bad: print("  ✗", n, why)
sys.exit(1 if (bad or p.returncode != 0) else 0)
