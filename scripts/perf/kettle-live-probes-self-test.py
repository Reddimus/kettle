#!/usr/bin/env python3
"""GUI-free regression tests for the live scrollback probe."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name("kettle-live-probes.py")
SPEC = importlib.util.spec_from_file_location("kettle_live_probes", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"cannot load {SCRIPT}")
PROBES = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROBES)


class FakeLive:
    def __init__(self, no_op: str | None = None, page_down_offset: int = 0) -> None:
        self.offset = 0
        self.no_op = no_op
        self.page_down_offset = page_down_offset

    def ctl(self, method, *, params, timeout):
        assert method == "perform_action"
        action = params["action"]
        if action == self.no_op:
            return
        if action == "scroll_page_up":
            self.offset += 24
        elif action == "scroll_page_down":
            self.offset = self.page_down_offset
        elif action == "scroll_to_bottom":
            self.offset = 0
        else:
            raise AssertionError(action)

    def json_ctl(self, method, *, timeout):
        assert method == "read_screen"
        return {"display_offset": self.offset}


class ScrollbackProbeTests(unittest.TestCase):
    def probe(self, live):
        with patch.object(PROBES, "make_scrollback"):
            return PROBES.scrollback_probe(live, 2)

    def test_page_down_no_op_is_rejected(self):
        with self.assertRaisesRegex(SystemExit, "scroll_page_down"):
            self.probe(FakeLive(no_op="scroll_page_down"))

    def test_page_down_must_restore_bottom(self):
        with self.assertRaisesRegex(SystemExit, "scroll_page_down"):
            self.probe(FakeLive(page_down_offset=1))

    def test_scroll_to_bottom_no_op_is_rejected(self):
        with self.assertRaisesRegex(SystemExit, "scroll_to_bottom"):
            self.probe(FakeLive(no_op="scroll_to_bottom"))

    def test_working_navigation_records_each_cycle(self):
        result = self.probe(FakeLive())
        self.assertEqual(result["final_display_offset"], 0)
        self.assertEqual(len(result["page_up_samples_ms"]), 2)
        self.assertEqual(len(result["page_down_samples_ms"]), 2)


if __name__ == "__main__":
    unittest.main()
