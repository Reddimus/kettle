#!/usr/bin/env python3
"""Headless behavioral coverage for the UI regression gate (stdlib only)."""

import contextlib
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    "capture_ui_states", Path(__file__).with_name("capture-ui-states.py")
)
capture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(capture)
ROOT = Path(__file__).resolve().parent.parent


class PathSafetyTests(unittest.TestCase):
    def symlink(self, alias, target):
        try:
            alias.symlink_to(target, target_is_directory=True)
        except (OSError, NotImplementedError):
            if os.name == "nt":
                self.skipTest("directory symlinks require Windows symlink privileges")
            raise

    def exercise(self, out, baseline, root, *, default_out=False):
        baseline.mkdir(parents=True, exist_ok=True)
        saved = baseline / "default.en.dark.default.png"
        saved.write_bytes(b"irreplaceable baseline bytes")
        sentinel = root / "unrelated"
        sentinel.write_bytes(b"keep")
        existed = out.exists()
        marker = out / ".output-sentinel"
        if existed:
            marker.write_bytes(b"keep output until validation succeeds")
        comparisons = []

        def shot(_smoke, _kettle, directory, *_args):
            result = directory / saved.name
            result.write_bytes(b"new capture bytes")
            return result

        def compare(actual, expected, *_args):
            comparisons.append((actual.resolve(), expected.resolve()))
            return "ok"

        argv = ["capture-ui-states.py", "--states", "default", "--compare", str(baseline)]
        if not default_out:
            argv += ["--out", str(out)]
        with patch.object(capture, "ROOT", root), patch("sys.argv", argv), \
                patch.object(capture, "load_live_smoke") as loader, \
                patch.object(capture, "capture_state", side_effect=shot) as shots, \
                patch.object(capture, "compare", side_effect=compare), \
                patch.object(capture.shutil, "rmtree", wraps=capture.shutil.rmtree) as delete, \
                contextlib.redirect_stdout(io.StringIO()):
            try:
                status = capture.main()
                diagnostic = ""
            except SystemExit as error:
                status = error.code
                diagnostic = str(error)
            except OSError as error:
                status = 1
                diagnostic = str(error)
        # Separate subtests keep all observable failures visible in the red run.
        for label, actual, expected in (
            ("baseline bytes", saved.read_bytes() if saved.exists() else None,
             b"irreplaceable baseline bytes"),
            ("helper loads", loader.call_count, 0),
            ("output deletions", delete.call_count, 0),
            ("output existence", out.exists(), existed),
            ("old output bytes", marker.read_bytes() if marker.exists() else None,
             b"keep output until validation succeeds" if existed else None),
            ("captures", shots.call_count, 0),
            ("comparisons", comparisons, []),
            ("unrelated bytes", sentinel.read_bytes(), b"keep"),
        ):
            with self.subTest(observation=label):
                self.assertEqual(actual, expected)
        with self.subTest(observation="clear rejection"):
            self.assertNotEqual(status, 0)
            self.assertIn("overlap", diagnostic)

    def test_rejects_equal_nested_and_symlink_aliases_before_side_effects(self):
        for kind in ("equal", "normalized", "out-ancestor", "baseline-ancestor",
                     "missing-out-child", "symlink-equal", "symlink-out-ancestor",
                     "symlink-baseline-ancestor", "symlink-parent", "compare-alias-equal",
                     "compare-alias-descendant", "default-relative"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory(dir=ROOT) as raw:
                root = Path(raw)
                baseline = root / "baseline"
                baseline.mkdir()
                out = baseline
                if kind == "normalized":
                    out = baseline / ".." / "baseline"
                elif kind in ("out-ancestor", "symlink-out-ancestor"):
                    out = root / "output"
                    baseline = out / "baselines"
                elif kind in ("baseline-ancestor", "symlink-baseline-ancestor"):
                    out = baseline / "output"
                    out.mkdir()
                elif kind == "missing-out-child":
                    out = baseline / "not-yet-created"
                elif kind == "default-relative":
                    baseline = root / "target" / "ui-states"
                    out = baseline
                if kind.startswith("symlink"):
                    out.mkdir(parents=True, exist_ok=True)
                    alias = root / "alias"
                    if kind == "symlink-parent":
                        self.symlink(alias, root)
                        out = alias / "baseline"
                    else:
                        self.symlink(alias, out)
                        out = alias
                elif kind.startswith("compare-alias"):
                    alias = root / "alias"
                    if kind == "compare-alias-descendant":
                        out = root / "output"
                        baseline = out / "baselines"
                    baseline.mkdir(parents=True, exist_ok=True)
                    self.symlink(alias, baseline)
                    baseline = alias
                with contextlib.chdir(root):
                    # The default output is absolute; the baseline spelling is relative.
                    if kind == "default-relative":
                        baseline = Path("target/ui-states")
                    self.exercise(out, baseline, root, default_out=kind == "default-relative")

    def test_rejects_final_output_symlink_without_overlap_before_side_effects(self):
        for use_compare in (False, True):
            for default_out in (False, True):
                with self.subTest(compare=use_compare, default_out=default_out), \
                        tempfile.TemporaryDirectory(dir=ROOT) as raw:
                    root = Path(raw)
                    target = root / "saved-output"
                    target.mkdir()
                    saved = target / "default.en.dark.default.png"
                    saved.write_bytes(b"saved target bytes\x00\xff")
                    nested = target / "nested" / "sentinel"
                    nested.parent.mkdir()
                    nested.write_bytes(b"nested target bytes\x00\xfe")
                    baseline = root / "disjoint-baseline"
                    baseline.mkdir()
                    baseline_file = baseline / saved.name
                    baseline_file.write_bytes(b"saved baseline bytes\x00\xfd")
                    out = root / "target" / "ui-states" if default_out else root / "shots"
                    out.parent.mkdir(parents=True, exist_ok=True)
                    self.symlink(out, target)
                    link_stat = out.lstat()
                    link_text = out.readlink()
                    target_stat = target.stat()
                    argv = ["capture-ui-states.py", "--states", "default"]
                    if not default_out:
                        argv += ["--out", str(out)]
                    if use_compare:
                        argv += ["--compare", str(baseline)]

                    def shot(_smoke, _kettle, directory, *_args):
                        path = directory / saved.name
                        path.write_bytes(b"new capture bytes")
                        return path

                    with patch.object(capture, "ROOT", root), patch("sys.argv", argv), \
                            patch.object(capture, "load_live_smoke") as loader, \
                            patch.object(capture, "capture_state", side_effect=shot) as shots, \
                            patch.object(capture, "compare", return_value="ok") as compare, \
                            patch.object(capture.shutil, "rmtree",
                                         wraps=capture.shutil.rmtree) as delete, \
                            contextlib.redirect_stdout(io.StringIO()):
                        try:
                            status = capture.main()
                            diagnostic = ""
                        except SystemExit as error:
                            status, diagnostic = error.code, str(error)
                        except OSError as error:
                            status, diagnostic = 1, str(error)
                    for label, actual, expected in (
                        ("target bytes", saved.read_bytes() if saved.exists() else None,
                         b"saved target bytes\x00\xff"),
                        ("nested target bytes", nested.read_bytes() if nested.exists() else None,
                         b"nested target bytes\x00\xfe"),
                        ("target identity", (target.stat().st_dev, target.stat().st_ino),
                         (target_stat.st_dev, target_stat.st_ino)),
                        ("symlink exists", out.is_symlink(), True),
                        ("symlink identity", (out.lstat().st_dev, out.lstat().st_ino),
                         (link_stat.st_dev, link_stat.st_ino)),
                        ("symlink destination", out.readlink(), link_text),
                        ("baseline bytes", baseline_file.read_bytes(),
                         b"saved baseline bytes\x00\xfd"),
                        ("helper loads", loader.call_count, 0),
                        ("output deletions", delete.call_count, 0),
                        ("captures", shots.call_count, 0),
                        ("comparisons", compare.call_count, 0),
                        ("output manifest", (target / "ui-states.json").exists(), False),
                    ):
                        with self.subTest(observation=label):
                            self.assertEqual(actual, expected)
                    with self.subTest(observation="clear rejection"):
                        self.assertNotEqual(status, 0)
                        self.assertIn("--out", diagnostic)
                        self.assertIn("symlink", diagnostic)

    def test_disjoint_sibling_with_shared_prefix_compares_without_blessing(self):
        with tempfile.TemporaryDirectory(dir=ROOT) as raw:
            root = Path(raw)
            out, baseline = root / "shots", root / "shots-saved"
            out.mkdir()
            (out / "stale").write_bytes(b"old output")
            baseline.mkdir()
            saved = baseline / "default.en.dark.default.png"
            saved.write_bytes(b"saved")
            def shot(_smoke, _kettle, directory, *_args):
                path = directory / saved.name
                path.write_bytes(b"different")
                return path
            with patch("sys.argv", ["capture-ui-states.py", "--out", str(out),
                                    "--compare", str(baseline), "--states", "default"]), \
                    patch.object(capture, "load_live_smoke"), \
                    patch.object(capture, "capture_state", side_effect=shot), \
                    patch.object(capture, "compare", return_value="no baseline") as compare, \
                    contextlib.redirect_stdout(io.StringIO()), \
                    contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(capture.main(), 1)
            self.assertFalse((out / "stale").exists())
            self.assertEqual(saved.read_bytes(), b"saved")
            compare.assert_called_once_with(out / saved.name, saved, 24, 0.5)
            self.assertEqual(json.loads((out / "ui-states.json").read_text())[0]["result"],
                             "no baseline")

    def test_existing_case_aliases_on_case_insensitive_filesystems(self):
        for kind in ("equal", "out-ancestor", "baseline-ancestor"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory(dir=ROOT) as raw:
                root = Path(raw)
                baseline = root / "baseline"
                baseline.mkdir()
                alias = root / "BASELINE"
                if not alias.exists():
                    self.skipTest("requires a case-insensitive filesystem")
                out = alias
                if kind == "out-ancestor":
                    baseline = baseline / "saved"
                elif kind == "baseline-ancestor":
                    out = alias / "output"
                    out.mkdir()
                self.exercise(out, baseline, root)


def geometry_fixture(name, scale=1, narrow=False):
    width = 640 if narrow else 1280
    def rect(x, y, w, h):
        return dict(x=x * scale, y=y * scale, width=w * scale, height=h * scale)
    content = rect(0, 28, width, 792)
    pane = dict(id=1, focused=True, rect=content.copy(), terminal=content.copy())
    modal = ("palette" if name.startswith("palette-") else
             "search" if name.startswith("search-") else
             name if name in ("settings", "theme-picker") else None)
    modal = "theme_picker" if modal == "theme-picker" else modal
    g = dict(surface=dict(width=width * scale, height=820 * scale), scale_factor=scale,
             content=content, panes=[pane], fullscreen=False,
             modals={key: key == modal for key in
                     ("palette", "search", "settings", "theme_picker")},
             tab_bar=dict(in_titlebar=True, height=28 * scale, y=0,
                          band=rect(78, 0, width - 78, 28),
                          new_tab=rect(width - 28, 0, 28, 28),
                          new_tab_menu=rect(width - 56, 0, 28, 28),
                          segments=[dict(rect=rect(78, 0, 100, 28), active=True)]))
    if name == "tabs":
        g["tab_bar"]["segments"] = [dict(rect=rect(78 + i * 100, 0, 100, 28),
                                         active=i == 2) for i in range(3)]
    if name == "splits":
        g["panes"] = [dict(id=i, focused=i == 2, rect=r, terminal=r.copy())
                      for i, r in enumerate((rect(0, 28, width / 2, 792),
                                             rect(width / 2, 28, width / 2, 396),
                                             rect(width / 2, 424, width / 2, 396)))]
    if modal in ("palette", "theme_picker"):
        g["picker"] = dict(rect=rect(40, 80, width - 80, 300),
                           rows_rect=rect(48, 130, width - 96, 240),
                           rows=[dict(rect=rect(48, 130, width - 96, 30),
                                      enabled=name != "palette-nomatch")])
    if modal == "settings":
        g["settings"] = dict(rect=rect(20, 40, width - 40, 700),
                             content_rect=rect(30, 100, width - 60, 600),
                             search_rect=rect(30, 50, width - 60, 30),
                             rows=[dict(rect=rect(30, 100, width - 60, 30))])
    if modal == "search":
        g["search"] = dict(rect=rect(0, 790, width, 30), has_match=name == "search-matches",
                           controls=[dict(name=key, rect=rect(i * 40, 790, 40, 30))
                                     for i, key in enumerate(("editor", "previous", "next", "wrap",
                                                              "case", "regex", "invert", "close"))])
        g["content"]["height"] -= 30 * scale
        g["panes"][0]["rect"]["height"] -= 30 * scale
        g["panes"][0]["terminal"]["height"] -= 30 * scale
    return g


class GeometryTests(unittest.TestCase):
    def run_fixture(self, name, geometry, raw):
        directory = Path(raw)
        out = directory / "output"
        events = []
        class Live:
            def __enter__(self):
                return self
            def __exit__(self, *_args):
                pass
            def json_ctl(self, method, params=None):
                events.append((method, params))
                return copy.deepcopy(geometry) if method == "ui_geometry" else {}
            def screenshot(self, _target):
                raise AssertionError("geometry-only must not capture PNGs")
        smoke = SimpleNamespace(LiveKettle=lambda *_args, **_kwargs: Live(),
                                live_shell_command=lambda *_args, **_kwargs: None)
        temporary_directory = tempfile.TemporaryDirectory
        with patch("sys.argv", ["capture-ui-states.py", "--out", str(out),
                                "--states", name, "--geometry-only"]), \
                patch.object(capture, "load_live_smoke", return_value=smoke), \
                patch.object(capture.time, "sleep"), \
                patch.object(capture.tempfile, "TemporaryDirectory",
                             side_effect=lambda **kwargs: temporary_directory(dir=directory, **kwargs)), \
                contextlib.redirect_stdout(io.StringIO()), \
                contextlib.redirect_stderr(io.StringIO()):
            status = capture.main()
        evidence = out / f"{name}.en.dark.default.geometry.json"
        self.assertEqual(json.loads(evidence.read_text()), geometry)
        self.assertEqual(list(out.glob("*.png")), [])
        self.assertEqual(sum(method == "ui_geometry" for method, _ in events), 1)
        return status, json.loads((out / "ui-states.json").read_text())

    def test_all_states_at_both_scales_and_widths(self):
        for name in capture.STATES:
            for scale in (1, 2):
                for narrow in (False, True):
                    with self.subTest(name=name, scale=scale, narrow=narrow), \
                            tempfile.TemporaryDirectory(dir=ROOT) as raw:
                        status, results = self.run_fixture(name, geometry_fixture(name, scale, narrow), raw)
                        self.assertEqual(status, 0)
                        self.assertEqual(results[0]["result"], "geometry ok")

    def test_bad_geometry_fails_and_keeps_diagnostics(self):
        cases = (
            ("default", lambda g: g["surface"].update(width=0)),
            ("default", lambda g: g.update(scale_factor=-1)),
            ("default", lambda g: g["panes"][0]["terminal"].update(width=2000)),
            ("default", lambda g: g["panes"][0].update(focused=False)),
            ("default", lambda g: g["tab_bar"]["band"].update(x=0)),
            ("default", lambda g: g["tab_bar"]["new_tab"].update(width=1)),
            ("default", lambda g: g["content"].update(y=0)),
            ("tabs", lambda g: g["tab_bar"]["segments"].pop()),
            ("tabs", lambda g: g["tab_bar"]["segments"][2]["rect"].update(x=-100000)),
            ("splits", lambda g: g["panes"].pop()),
            ("splits", lambda g: g["panes"][1].update(rect=g["panes"][0]["rect"].copy())),
            ("palette-empty", lambda g: g["modals"].update(palette=False)),
            ("palette-query", lambda g: g["picker"]["rect"].update(x=-10)),
            ("palette-empty", lambda g: g["picker"].update(rows=[])),
            ("palette-nomatch", lambda g: g["picker"]["rows"][0].update(enabled=True)),
            ("settings", lambda g: g["settings"]["search_rect"].update(y=800)),
            ("search-matches", lambda g: g["search"].update(has_match=False)),
            ("search-nomatch", lambda g: g["search"].update(has_match=True)),
            ("search-matches", lambda g: g["search"]["controls"].pop()),
            ("search-matches", lambda g: g["panes"][0]["rect"].update(height=792)),
            ("theme-picker", lambda g: g["picker"]["rows"][0]["rect"].update(width=2000)),
        )
        for index, (name, mutate) in enumerate(cases):
            with self.subTest(index=index, name=name), tempfile.TemporaryDirectory(dir=ROOT) as raw:
                geometry = geometry_fixture(name)
                mutate(geometry)
                status, results = self.run_fixture(name, geometry, raw)
                self.assertEqual(status, 1)
                self.assertIn("geometry:", results[0]["result"])

    def test_overflow_can_park_inactive_tabs(self):
        geometry = geometry_fixture("tabs", scale=2, narrow=True)
        geometry["tab_bar"]["segments"][0]["rect"]["x"] = -100000
        with tempfile.TemporaryDirectory(dir=ROOT) as raw:
            status, _ = self.run_fixture("tabs", geometry, raw)
            self.assertEqual(status, 0)


if __name__ == "__main__":
    unittest.main()
