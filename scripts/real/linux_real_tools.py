#!/usr/bin/env python3
"""Linux test assets for the REAL video test (crates/eid-cli/tests/real_jobs.rs) on a developer
box. The product ships only the Windows tools (tools/manifest.json); this fetches the Linux
builds of the SAME pinned versions (yt-dlp, FFmpeg BtbN autobuild, Deno), repacks them under
the Windows file names the app expects, writes a test manifest with their real sizes and
SHA-256, and serves them on 127.0.0.1 (the only base the test hook accepts).

  python3 scripts/real/linux_real_tools.py WORKDIR [PORT]   # prints the env to export, serves
  EID_REAL_MANIFEST=... EID_REAL_TOOLS_BASE=... \\
    cargo test -p eid-cli --test real_jobs -- --ignored --nocapture --test-threads=1
"""
import hashlib, http.server, json, os, shutil, subprocess, sys, tarfile, urllib.request, zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


def sha(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def get(url, dst):
    if not os.path.exists(dst):
        print("fetch", url, flush=True)
        urllib.request.urlretrieve(url, dst + ".part")
        os.replace(dst + ".part", dst)


def main():
    work = os.path.abspath(sys.argv[1])
    port = int(sys.argv[2]) if len(sys.argv) > 2 else 47613
    dl, x, srv = (os.path.join(work, d) for d in ("dl", "x", "srv"))
    for d in (dl, x, srv):
        os.makedirs(d, exist_ok=True)
    m = json.load(open(os.path.join(ROOT, "tools", "manifest.json")))
    t = {t["name"]: t for t in m["tools"]}
    yv = t["yt-dlp"]["version"]
    ff_win = t["ffmpeg"]["url"]
    ff_lin = ff_win.replace("-win64-", "-linux64-").replace(".zip", ".tar.xz")
    dv = t["deno"]["url"].split("/download/")[1].split("/")[0]
    get(f"https://github.com/yt-dlp/yt-dlp/releases/download/{yv}/yt-dlp_linux", f"{dl}/yt-dlp_linux")
    get(ff_lin, f"{dl}/ff.tar.xz")
    get(f"https://github.com/denoland/deno/releases/download/{dv}/deno-x86_64-unknown-linux-gnu.zip", f"{dl}/deno.zip")
    with tarfile.open(f"{dl}/ff.tar.xz") as tf:
        tf.extractall(x, filter="data")
    with zipfile.ZipFile(f"{dl}/deno.zip") as z:
        z.extractall(f"{x}/deno")
    ffdir = next(d for d in os.listdir(x) if d.startswith("ffmpeg"))
    shutil.copy(f"{dl}/yt-dlp_linux", f"{srv}/yt-dlp.exe")
    with zipfile.ZipFile(f"{srv}/ffmpeg-linux.zip", "w", zipfile.ZIP_DEFLATED) as z:
        z.write(f"{x}/{ffdir}/bin/ffmpeg", "ff/bin/ffmpeg.exe")
        z.write(f"{x}/{ffdir}/bin/ffprobe", "ff/bin/ffprobe.exe")
    with zipfile.ZipFile(f"{srv}/deno-linux.zip", "w", zipfile.ZIP_DEFLATED) as z:
        z.write(f"{x}/deno/deno", "deno.exe")
    src = {
        "yt-dlp": ("yt-dlp.exe", [(None, "yt-dlp.exe", f"{srv}/yt-dlp.exe")]),
        "ffmpeg": ("ffmpeg-linux.zip", [("ff/bin/ffmpeg.exe", "ffmpeg.exe", f"{x}/{ffdir}/bin/ffmpeg"),
                                        ("ff/bin/ffprobe.exe", "ffprobe.exe", f"{x}/{ffdir}/bin/ffprobe")]),
        "deno": ("deno-linux.zip", [("deno.exe", "deno.exe", f"{x}/deno/deno")]),
    }
    for tool in m["tools"]:
        asset, files = src[tool["name"]]
        tool["url"] = tool["url"].rsplit("/", 1)[0] + "/" + asset
        tool["size"], tool["sha256"] = os.path.getsize(f"{srv}/{asset}"), sha(f"{srv}/{asset}")
        tool["files"] = [{"entry": e, "name": n, "size": os.path.getsize(p), "sha256": sha(p)} for e, n, p in files]
        tool["version"] += " [Linux test build]"
    m["id"] = "linux-real-test"
    mf = os.path.join(work, "manifest-linux.json")
    json.dump(m, open(mf, "w"), indent=1)
    print(f"export EID_REAL_MANIFEST={mf}\nexport EID_REAL_TOOLS_BASE=http://127.0.0.1:{port}", flush=True)
    os.chdir(srv)
    http.server.ThreadingHTTPServer(("127.0.0.1", port), http.server.SimpleHTTPRequestHandler).serve_forever()


if __name__ == "__main__":
    main()
