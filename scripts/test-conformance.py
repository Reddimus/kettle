#!/usr/bin/env python3
"""Verify the esctest feature adapter without launching a terminal."""

import importlib.util
from pathlib import Path
from types import SimpleNamespace
import unittest
import tempfile
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "conformance_check", Path(__file__).with_name("check-conformance.py")
)
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)


class EsctestColorIsolation(unittest.TestCase):
    def test_resets_special_resources_between_cases_without_catching_assertions(self):
        events = []
        commands = SimpleNamespace(
            note=events.append, ResetSpecialColor=lambda: events.append("special reset")
        )
        source = """import esccmd
def reset():
    esccmd.note("original reset")
def main():
    reset()
    esccmd.note("first test")
    reset()
    esccmd.note("second test")
    raise AssertionError("real failure")
main()
"""
        with tempfile.TemporaryDirectory() as scratch:
            script = Path(scratch) / "esctest.py"
            script.write_text(source, encoding="utf-8")
            with patch.dict("sys.modules", {"esccmd": commands}):
                with self.assertRaisesRegex(AssertionError, "real failure"):
                    checker.run_esctest_suite(script, commands)
        self.assertEqual(
            events,
            [
                "original reset",
                "special reset",
                "first test",
                "original reset",
                "special reset",
                "second test",
            ],
        )

    def test_refuses_an_unexpected_upstream_entrypoint(self):
        with tempfile.TemporaryDirectory() as scratch:
            script = Path(scratch) / "esctest.py"
            script.write_text("main(1)\n", encoding="utf-8")
            with self.assertRaisesRegex(RuntimeError, "entrypoint"):
                checker.run_esctest_suite(script, SimpleNamespace())


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


class GraphemeModes(unittest.TestCase):
    def fixture(self, enabled=False, supported=True):
        from contextlib import contextmanager

        states = []
        term = SimpleNamespace(enabled=enabled, supported=supported)

        def query(mode, timeout, force):
            self.assertEqual(mode, 2027)
            self.assertTrue(force)
            return SimpleNamespace(enabled=term.enabled, supported=term.supported)

        @contextmanager
        def change(want, mode, timeout):
            before = term.enabled
            if term.supported:
                term.enabled = want
            try:
                yield
            finally:
                term.enabled = before

        term.get_dec_mode = query
        term.dec_modes_enabled = lambda mode, timeout: change(True, mode, timeout)
        term.dec_modes_disabled = lambda mode, timeout: change(False, mode, timeout)
        module = SimpleNamespace(maybe_grapheme_clustering_mode=object())
        return term, module, states

    def test_fixed_modes_are_observed_and_restored(self):
        for mode, before, want in [("off", True, False), ("on", False, True)]:
            term, module, states = self.fixture(before)
            checker.adapt_ucs_grapheme_mode(module, mode, states.append)
            with module.maybe_grapheme_clustering_mode(term):
                self.assertEqual(term.enabled, want)
            self.assertEqual(term.enabled, before)
            self.assertEqual(states, [{"enabled": want, "supported": True}])

    def test_measurement_failures_propagate_and_restore_the_mode(self):
        term, module, states = self.fixture()
        checker.adapt_ucs_grapheme_mode(module, "on", states.append)
        with self.assertRaisesRegex(RuntimeError, "measurement failed"):
            with module.maybe_grapheme_clustering_mode(term):
                raise RuntimeError("measurement failed")
        self.assertFalse(term.enabled)

    def test_unsupported_on_is_not_counted_as_enabled(self):
        term, module, states = self.fixture(supported=False)
        checker.adapt_ucs_grapheme_mode(module, "on", states.append)
        with self.assertRaisesRegex(RuntimeError, "mode 2027"):
            with module.maybe_grapheme_clustering_mode(term):
                self.fail("measurement ran without mode")
        self.assertEqual(states, [{"enabled": False, "supported": False}])

    def test_unsupported_off_is_not_counted_as_confirmed(self):
        term, module, states = self.fixture(supported=False)
        checker.adapt_ucs_grapheme_mode(module, "off", states.append)
        with self.assertRaisesRegex(RuntimeError, "mode 2027"):
            with module.maybe_grapheme_clustering_mode(term):
                self.fail("measurement ran without a confirmed mode")
        self.assertEqual(states, [{"enabled": False, "supported": False}])

    def test_whole_suite_holds_the_mode_outside_table_contexts(self):
        from contextlib import nullcontext

        for mode, before, want in [("on", False, True), ("off", True, False)]:
            for fail in [False, True]:
                term, module, states = self.fixture(before)
                term.cbreak = nullcontext
                checker.adapt_ucs_grapheme_mode(module, mode, states.append)
                init = lambda stream: (term, "writer")
                suite = SimpleNamespace(init_term=init)

                def main():
                    actual, writer = suite.init_term("stream")
                    self.assertIs(actual, term)
                    self.assertEqual(writer, "writer")
                    self.assertEqual(actual.enabled, want)
                    if fail:
                        raise RuntimeError("language measurement failed")
                    return 17

                suite.main = main
                if fail:
                    with self.assertRaisesRegex(RuntimeError, "language measurement"):
                        checker.run_ucs_suite(suite, module, mode)
                else:
                    self.assertEqual(checker.run_ucs_suite(suite, module, mode), 17)
                self.assertEqual(term.enabled, before)
                self.assertIs(suite.init_term, init)
                self.assertEqual(states, [{"enabled": want, "supported": True}])

    def test_auto_does_not_wrap_suite_initialization(self):
        init = object()
        suite = SimpleNamespace(init_term=init, main=lambda: 17)
        self.assertEqual(checker.run_ucs_suite(suite, None, "auto"), 17)
        self.assertIs(suite.init_term, init)

    def test_auto_leaves_the_pinned_suite_context_untouched(self):
        term, module, states = self.fixture()
        original = module.maybe_grapheme_clustering_mode
        checker.adapt_ucs_grapheme_mode(module, "auto", states.append)
        self.assertIs(module.maybe_grapheme_clustering_mode, original)


class FixtureEnvironment(unittest.TestCase):
    def test_color_overrides_are_removed_only_from_the_child(self):
        import os
        import tempfile
        from unittest.mock import patch

        class Entered(Exception):
            pass

        captured = {}

        class FakeLive:
            def __init__(self, *args, extra_env):
                captured.update(extra_env)

            def __enter__(self):
                raise Entered

            def __exit__(self, *args):
                pass

        flags = {
            name: "1"
            for name in ["NO_COLOR", "FORCE_COLOR", "CLICOLOR", "CLICOLOR_FORCE"]
        }
        with tempfile.TemporaryDirectory() as out, patch.dict(os.environ, flags):
            with patch.object(
                checker, "load_live", return_value=SimpleNamespace(LiveKettle=FakeLive)
            ):
                with self.assertRaises(Entered):
                    checker.in_kettle(Path("kettle"), Path(out), "fixture", None)
            for name in flags:
                self.assertIsNone(captured[name])
                self.assertEqual(os.environ[name], "1")


if __name__ == "__main__":
    unittest.main()
