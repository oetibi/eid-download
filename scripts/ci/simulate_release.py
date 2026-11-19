#!/usr/bin/env python3
"""Run the `release` job of .github/workflows/ci-windows.yml LOCALLY, step by step, against a
real `eid-windows-x64` artifact, without publishing anything. Catches release-only failures
before a tag is pushed (v0.1.0 and v0.1.1 each failed for the first time on their tag).

  gh run download <ci-windows run id> -n eid-windows-x64 -D /tmp/art
  python3 scripts/ci/simulate_release.py --artifact /tmp/art [--tag-ref v0.1.2]

Copies the repository's tracked files into a temporary directory and runs every `run:` step of
the job there with bash (`set -euo pipefail`, like GitHub's default shell), with
`${{ steps.tag.outputs.* }}` / `${{ github.token }}` filled in and `if:` conditions on
`steps.tag.outputs.dry` evaluated. `uses:` steps are skipped (download-artifact = the copy of
--artifact into rel/). Without --tag-ref it is the dry run; with --tag-ref the tag path is
followed, but every `gh release create` / `gh release download` is replaced by an echo:
nothing is ever published. Needs bash, node >= 22, sha256sum, git, gh (read-only call), PyYAML.
"""
import argparse, os, re, shutil, subprocess, sys, tempfile

import yaml

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--artifact", required=True)
    ap.add_argument("--tag-ref", help="simulate a push of this tag (still never publishes)")
    ap.add_argument("--keep", action="store_true", help="keep the work directory")
    a = ap.parse_args()
    wf = yaml.safe_load(open(os.path.join(ROOT, ".github/workflows/ci-windows.yml"), encoding="utf-8"))
    steps = wf["jobs"]["release"]["steps"]
    work = tempfile.mkdtemp(prefix="eid-release-sim-")
    files = subprocess.run(["git", "ls-files", "-z"], cwd=ROOT, capture_output=True, check=True).stdout.split(b"\0")
    for f in filter(None, files):
        src, dst = os.path.join(ROOT, f.decode()), os.path.join(work, f.decode())
        if os.path.isfile(src):
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            shutil.copy2(src, dst)
    subprocess.run(["git", "init", "-q"], cwd=work, check=True)
    subprocess.run(["git", "remote", "add", "origin", "https://github.com/oetibi/eid-download"], cwd=work, check=True)
    out_file = os.path.join(work, ".gh-output")
    env = dict(os.environ, GITHUB_REPOSITORY="oetibi/eid-download", GITHUB_OUTPUT=out_file,
               GITHUB_REF=f"refs/tags/{a.tag_ref}" if a.tag_ref else "refs/pull/0/merge",
               GITHUB_REF_NAME=a.tag_ref or "0/merge")
    outputs = {}
    ok = True
    for s in steps:
        name = s.get("name") or s.get("uses")
        cond = s.get("if", "")
        m = re.search(r"steps\.tag\.outputs\.dry == '(true|false)'", cond)
        if m and outputs.get("dry") != m.group(1):
            print(f"--- skip (if): {name}")
            continue
        if "uses" in s:
            if s["uses"].startswith("actions/download-artifact"):
                shutil.copytree(a.artifact, os.path.join(work, s["with"]["path"]), dirs_exist_ok=True)
                print(f"--- artifact -> {s['with']['path']}")
            else:
                print(f"--- skip (uses): {name}")
            continue
        sub = lambda t: re.sub(r"\$\{\{\s*steps\.tag\.outputs\.(\w+)\s*\}\}", lambda x: outputs.get(x.group(1), ""), t.replace("${{ github.token }}", os.environ.get("GH_TOKEN", "")))
        senv = dict(env, **{k: sub(str(v)) for k, v in (s.get("env") or {}).items()})
        script = s["run"]
        # never publish; "download back" = the files that would have been uploaded
        script = re.sub(r'gh release download "\$TAG" --repo "\$GITHUB_REPOSITORY"',
                        "cp ../rel/EidDownload-windows-x64.zip ../rel/EidDownload-*-windows-x64-setup.exe ../rel/eid-download-*.zip ../rel/SHA256SUMS.txt .", script)
        script = re.sub(r"\bgh release create\b", "echo SIMULATED: gh release create", script)
        if "gh release view" in script and a.tag_ref:
            script = script.replace("pre=$(gh release view", "pre=true; : $(gh release view")
        cwd = os.path.join(work, s.get("working-directory", "."))
        print(f"=== {name}", flush=True)
        r = subprocess.run(["bash", "-e", "-o", "pipefail", "-c", script], cwd=cwd, env=senv)
        if os.path.exists(out_file):
            for line in open(out_file):
                if "=" in line:
                    k, v = line.rstrip("\n").split("=", 1)
                    outputs[k] = v
        if r.returncode != 0:
            print(f"!!! FAILED: {name} (exit {r.returncode})")
            ok = False
            break
    print(("release job simulation PASSED" if ok else "release job simulation FAILED") + f" ({work})")
    if not a.keep and ok:
        shutil.rmtree(work)
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
