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
    # `unix:` = genuinely Unix-only (symlinks, rustls backend, SIGINT); `windows:` = genuinely
    # Windows-only (pipe DACL, HKCU registry). On the other platform the name is reported as
    # NOT TESTED (it does not count as passed) but it is still required on its own platform, so
    # deleting or #[ignore]-ing it turns that platform's CI red.
    if l.startswith("unix:") or l.startswith("windows:"):
        prefix, name = l.split(":", 1)
        if (prefix == "unix") == _win:
            not_tested.append(prefix + ":" + name)
            continue
        l = name
    req.append(l)
extra = sys.argv[1:]
# streamed line by line (ci-windows run 44693133755 hung here for 40 minutes with nothing in the
# log because the output was captured until the end): a test that hangs now shows up as cargo's
# "has been running for over 60 seconds" line, and the step's timeout ends the job
p = subprocess.Popen(["cargo", "test", "--workspace", "--no-fail-fast", *extra, "--", "--test-threads=4"],
                     cwd=root, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                     text=True, encoding="utf-8", errors="replace")
lines = []
for line in p.stdout:
    lines.append(line)
    if line.startswith("test ") or "running for over" in line or line.startswith("     Running"):
        sys.stdout.write(line)
        sys.stdout.flush()
p.wait()
out = "".join(lines)
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
