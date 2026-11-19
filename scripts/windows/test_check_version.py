"""Tests for check_version.py. Run: python3 -m unittest discover -s scripts/windows"""
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import check_version as cv  # noqa: E402


class CheckVersion(unittest.TestCase):
    def test_the_repository_agrees_with_itself(self):
        w, t, l = cv.versions()
        self.assertEqual(cv.check(w, t, l), [])
        self.assertIn("eid-engine", l)
        self.assertEqual(cv.check(w, t, l, f"v{w}"), [])

    def test_mismatches_are_reported(self):
        ok = {"eid-engine": "0.1.0", "eid-gui": "0.1.0"}
        self.assertEqual(cv.check("0.1.0", "0.1.0", ok, "v0.1.0"), [])
        self.assertEqual(len(cv.check("0.1.0", "0.0.1", ok)), 1)
        self.assertEqual(len(cv.check("0.1.0", "0.1.0", {**ok, "eid-cli": "0.0.1"})), 1)
        self.assertEqual(len(cv.check("0.1.0", "0.1.0", ok, "v0.1.1")), 1)
        self.assertEqual(len(cv.check("0.1.0", "0.1.0", ok, "0.1.0")), 1)
        self.assertEqual(len(cv.check("0.1.0", "0.1.0", {})), 1)


if __name__ == "__main__":
    unittest.main()
