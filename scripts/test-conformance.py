#!/usr/bin/env python3
"""Verify the esctest feature adapter without launching a terminal."""

import importlib.util
from pathlib import Path
from types import SimpleNamespace
import unittest

spec = importlib.util.spec_from_file_location(
    "conformance_check", Path(__file__).with_name("check-conformance.py")
)
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)


class NoClearOptions(unittest.TestCase):
    def setUp(self):
        self.wrapped = []

        def option_required(terminal, option, allowPassWithoutOption=False):
            def decorate(function):
                self.wrapped.append((terminal, option, allowPassWithoutOption, function))
                def wrapper():
                    raise RuntimeError("original option decorator")
                return wrapper
            return decorate

        self.utility = SimpleNamespace(optionRequired=option_required)
        checker.adapt_esctest_options(self.utility)

    def decorate(self, name, terminal="xterm", option="xtermWinopsEnabled", fail=False):
        def function():
            if fail:
                raise AssertionError("terminal behavior failed")
            return 17
        function.__name__ = name
        return self.utility.optionRequired(terminal, option)(function)

    def test_only_the_two_no_clear_cases_drop_the_geometry_precondition(self):
        for name in ["test_DECRQM_DEC_DECNCSM", "test_DECSET_DECNCSM"]:
            self.assertEqual(self.decorate(name)(), 17)
        self.assertEqual(self.wrapped, [])

    def test_no_clear_assertion_failures_still_fail(self):
        for name in ["test_DECRQM_DEC_DECNCSM", "test_DECSET_DECNCSM"]:
            with self.assertRaisesRegex(AssertionError, "terminal behavior failed"):
                self.decorate(name, fail=True)()

    def test_window_and_other_terminal_preconditions_are_preserved(self):
        for name, terminal, option in [
            ("test_XtermWinops_ResizeChars_BothParameters", "xterm", "xtermWinopsEnabled"),
            ("test_DECSET_DECNCSM", "iTerm2", "xtermWinopsEnabled"),
            ("test_DECSET_DECNCSM", "xterm", "anotherOption"),
        ]:
            with self.assertRaisesRegex(RuntimeError, "original option decorator"):
                self.decorate(name, terminal, option)()
        self.assertEqual(len(self.wrapped), 3)

    def test_existing_decorator_options_are_forwarded(self):
        def unrelated():
            return 17
        self.utility.optionRequired("xterm", "anotherOption", True)(unrelated)
        self.assertEqual(self.wrapped[0][:3], ("xterm", "anotherOption", True))


if __name__ == "__main__":
    unittest.main()
