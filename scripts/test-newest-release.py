#!/usr/bin/env python3
"""Tests for scripts/newest-release.py."""

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SPEC = importlib.util.spec_from_file_location("newest_release", os.path.join(HERE, "newest-release.py"))
newest_release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(newest_release)


def release(id, tag, draft=False, prerelease=False):
    return {"id": id, "tag_name": tag, "draft": draft, "prerelease": prerelease}


class NewestRelease(unittest.TestCase):
    def test_orders_by_version_not_by_listing_or_text(self):
        pages = [[release(1, "v5.9.0"), release(2, "v5.10.0")], [release(3, "v5.7.1")]]
        self.assertEqual(newest_release.newest(pages), 2)

    def test_skips_drafts_prereleases_and_other_tags(self):
        pages = [[
            release(1, "v5.6.0"),
            release(2, "v5.8.0", draft=True),
            release(3, "v5.9.0", prerelease=True),
            release(4, "v6.0.0-rc1"),
            release(5, "nightly"),
            release(6, "v05.7.0"),
        ]]
        self.assertEqual(newest_release.newest(pages), 1)

    def test_an_older_tag_published_last_does_not_win(self):
        # The run for v5.7.0 finishing after v5.7.1 sees both published.
        pages = [[release(11, "v5.7.0"), release(12, "v5.7.1")]]
        self.assertEqual(newest_release.newest(pages), 12)

    def test_no_qualifying_release_fails_without_output(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "releases.json")
            for pages in ([[release(1, "v1.0.0", draft=True)]], {"not": "pages"}):
                with open(path, "w", encoding="utf-8") as handle:
                    json.dump(pages, handle)
                result = subprocess.run(
                    [sys.executable, os.path.join(HERE, "newest-release.py"), path],
                    capture_output=True, text=True, check=False,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")


    def test_require_waits_for_the_run_s_own_release(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "releases.json")
            script = os.path.join(HERE, "newest-release.py")
            # Just published, but the list still shows it as a draft: the
            # previous release is newest, and promoting it would be wrong.
            with open(path, "w", encoding="utf-8") as handle:
                json.dump([[release(1, "v5.6.0"), release(2, "v5.7.0", draft=True)]], handle)
            waiting = subprocess.run([sys.executable, script, "--require", "v5.7.0", path],
                                     capture_output=True, text=True, check=False)
            self.assertEqual(waiting.returncode, 3)
            self.assertEqual(waiting.stdout, "")
            with open(path, "w", encoding="utf-8") as handle:
                json.dump([[release(1, "v5.6.0"), release(2, "v5.7.0")]], handle)
            listed = subprocess.run([sys.executable, script, "--require", "v5.7.0", path],
                                    capture_output=True, text=True, check=True)
            self.assertEqual(listed.stdout.strip(), "2")

if __name__ == "__main__":
    unittest.main()
