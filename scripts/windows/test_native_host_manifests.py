"""Tests for native_host_manifests.py. Run: python3 -m unittest discover -s scripts/windows"""
import json
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import native_host_manifests as nhm  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parents[2]
C1 = "abcdefghijklmnopabcdefghijklmnop"
C2 = "ponmlkjihgfedcbaponmlkjihgfedcba"


def cfg(**kw):
    c = {"placeholder": False, "host_name": "com.eid.download",
         "chrome": [C1], "edge": [C2], "firefox": ["eid@example.org"]}
    c.update(kw)
    return c


class Build(unittest.TestCase):
    def test_three_manifests_with_relative_path_and_exact_origins(self):
        out = nhm.build(cfg())
        self.assertEqual(sorted(out), ["com.eid.download.brave.json",
                                       "com.eid.download.chrome.json",
                                       "com.eid.download.edge.json",
                                       "com.eid.download.firefox.json"])
        self.assertEqual(out["com.eid.download.brave.json"]["allowed_origins"],
                         [f"chrome-extension://{C1}/"])
        ch = out["com.eid.download.chrome.json"]
        self.assertEqual(ch["path"], "eid-native-host.exe")
        self.assertEqual(ch["type"], "stdio")
        self.assertEqual(ch["name"], "com.eid.download")
        self.assertEqual(ch["allowed_origins"], [f"chrome-extension://{C1}/"])
        self.assertNotIn("allowed_extensions", ch)
        # Edge: its own store ID plus the Chrome Web Store ID
        self.assertEqual(out["com.eid.download.edge.json"]["allowed_origins"],
                         [f"chrome-extension://{C2}/", f"chrome-extension://{C1}/"])
        ff = out["com.eid.download.firefox.json"]
        self.assertEqual(ff["allowed_extensions"], ["eid@example.org"])
        self.assertNotIn("allowed_origins", ff)

    def test_empty_list_skips_that_browser_and_never_writes_an_empty_allow_list(self):
        out = nhm.build(cfg(chrome=[], edge=[]))
        self.assertEqual(list(out), ["com.eid.download.firefox.json"])
        self.assertEqual(nhm.build(cfg(chrome=[], edge=[], firefox=[])), {})
        for m in nhm.build(cfg()).values():
            self.assertTrue(m.get("allowed_origins") or m.get("allowed_extensions"))

    def test_bad_ids_and_names_are_refused(self):
        bad = [
            dict(chrome=["A" * 32]), dict(chrome=["q" * 32]), dict(chrome=["a" * 31]),
            dict(edge=["a" * 33]), dict(chrome=["a" * 31 + "/"]), dict(chrome=["a" * 32 + "\n"]),
            dict(firefox=["eid@example.org\n"]), dict(host_name="com.eid\n"),
            dict(firefox=["evil\"},{\"x"]), dict(firefox=["a b"]), dict(firefox=[""]),
            dict(firefox=["x" * 129]), dict(firefox="eid@example.org"),
            dict(chrome=[C1, C1]), dict(host_name="Com.Eid"), dict(host_name="com..eid"),
            dict(host_name=".com"), dict(host_name="com.eid\\x"), dict(host_name=""),
            dict(placeholder="yes"), dict(extra=1),
        ]
        for b in bad:
            with self.subTest(b=b), self.assertRaises(nhm.Invalid):
                nhm.build(cfg(**b))

    def test_release_refuses_placeholders_but_a_dev_build_accepts_them(self):
        with self.assertRaises(nhm.Invalid):
            nhm.build(cfg(placeholder=True), release=True)
        self.assertEqual(len(nhm.build(cfg(placeholder=True))), 4)
        self.assertEqual(len(nhm.build(cfg(), release=True)), 4)

    def test_the_committed_config_is_valid(self):
        c = json.loads((ROOT / "extension/native-host-ids.json").read_text(encoding="utf-8"))
        out = nhm.build(c)
        self.assertEqual(c["host_name"], "com.eid.download")
        self.assertTrue(out)


class Cli(unittest.TestCase):
    def test_writes_files_and_fails_without_writing_on_bad_input(self):
        with tempfile.TemporaryDirectory() as d:
            d = pathlib.Path(d)
            good = d / "ids.json"
            good.write_text(json.dumps(cfg()), encoding="utf-8")
            self.assertEqual(nhm.main([str(good), str(d / "out")]), 0)
            m = json.loads((d / "out/com.eid.download.chrome.json").read_text(encoding="utf-8"))
            self.assertEqual(m["allowed_origins"], [f"chrome-extension://{C1}/"])
            bad = d / "bad.json"
            bad.write_text(json.dumps(cfg(chrome=["nope"])), encoding="utf-8")
            self.assertEqual(nhm.main([str(bad), str(d / "out2")]), 1)
            self.assertFalse((d / "out2").exists())
            ph = d / "ph.json"
            ph.write_text(json.dumps(cfg(placeholder=True)), encoding="utf-8")
            self.assertEqual(nhm.main([str(ph), str(d / "out3"), "--release"]), 1)
            self.assertFalse((d / "out3").exists())
            (d / "broken.json").write_text("{", encoding="utf-8")
            self.assertEqual(nhm.main([str(d / "broken.json"), str(d / "out4")]), 1)


if __name__ == "__main__":
    unittest.main()
