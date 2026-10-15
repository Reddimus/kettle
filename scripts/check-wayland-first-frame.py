#!/usr/bin/env python3
"""Check initial buffer order for Wayland test windows with client decorations."""
import argparse
from dataclasses import dataclass, field
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


def trace_lines(path, limit=16 * 1024 * 1024):
    flags = os.O_RDONLY | getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_BINARY", 0)
    with os.fdopen(os.open(path, flags), "rb") as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise ValueError("trace must be a regular file")
        remaining = limit
        while line := stream.readline(remaining + 1):
            if len(line) > remaining:
                raise ValueError("trace exceeds byte limit; capture a shorter fixture (at most 16 MiB)")
            remaining -= len(line)
            yield line.decode("utf-8", errors="replace")


@dataclass
class Surface:
    identifier: int
    buffer: bool = False
    committed_buffer: bool = False
    first_commit: int | None = None
    destroyed: int | None = None
    children: list = field(default_factory=list)
    decorations_at_first_map: set[int] = field(default_factory=set)


def inspect(lines, minimum=1):
    surfaces, xdg, roots = {}, {}, []

    def surface(identifier):
        if identifier not in surfaces:
            surfaces[identifier] = Surface(identifier)
        return surfaces[identifier]

    for index, line in enumerate(lines, 1):
        if not re.match(r"^\[\s*[\d.]+\]\s*(?:\{[^}]*\})?\s*->\s+", line):
            continue
        if match := re.search(r"\.create_surface\(new id wl_surface[#@](\d+)\)", line):
            identifier = int(match[1])
            surfaces[identifier] = Surface(identifier)
        elif match := re.search(r"\.get_xdg_surface\(new id xdg_surface[#@](\d+), wl_surface[#@](\d+)\)", line):
            xdg[int(match[1])] = surface(int(match[2]))
        elif match := re.search(r"xdg_surface[#@](\d+)\.get_toplevel\(", line):
            if int(match[1]) in xdg:
                roots.append(xdg[int(match[1])])
        elif match := re.search(r"\.get_subsurface\(new id wl_subsurface[#@]\d+, wl_surface[#@](\d+), wl_surface[#@](\d+)\)", line):
            surface(int(match[2])).children.append(surface(int(match[1])))
        elif match := re.search(r"wl_surface[#@](\d+)\.attach\(([^,)]*)", line):
            surface(int(match[1])).buffer = bool(re.fullmatch(r"wl_buffer[#@]\d+", match[2]))
        elif match := re.search(r"wl_surface[#@](\d+)\.commit\(\)", line):
            current = surface(int(match[1]))
            current.committed_buffer = current.buffer
            if current.buffer and current.first_commit is None:
                current.first_commit = index
                current.decorations_at_first_map = {
                    id(child) for child in current.children if child.committed_buffer
                }
        elif match := re.search(r"wl_surface[#@](\d+)\.destroy\(\)", line):
            current = surfaces.pop(int(match[1]), None)
            if current:
                current.destroyed = index

    reports = []
    for root in roots:
        if root.first_commit is None:
            continue
        children = [child for child in root.children
                    if child.destroyed is None or child.destroyed > root.first_commit]
        if not children:
            continue
        late = [child.identifier for child in children
                if id(child) not in root.decorations_at_first_map]
        reports.append({"surface": root.identifier, "first_buffer_commit_line": root.first_commit,
                        "decoration_surfaces": len(children), "late_decorations": late})
    return {"pass": len(reports) >= minimum and all(not row["late_decorations"] for row in reports),
            "minimum_windows": minimum, "decorated_windows": reports}


def fixture(root=1, child=2, early=False, child_commit=True):
    prefix = [f"wl_compositor#3.create_surface(new id wl_surface#{root})",
              f"wl_compositor#3.create_surface(new id wl_surface#{child})",
              f"xdg_wm_base#4.get_xdg_surface(new id xdg_surface#5, wl_surface#{root})",
              "xdg_surface#5.get_toplevel(new id xdg_toplevel#6)",
              f"wl_subcompositor#7.get_subsurface(new id wl_subsurface#8, wl_surface#{child}, wl_surface#{root})",
              f"wl_surface#{root}.commit()"]
    parent = [f"wl_surface#{root}.attach(wl_buffer#9, 0, 0)", f"wl_surface#{root}.commit()"]
    decoration = [f"wl_surface#{child}.attach(wl_buffer#10, 0, 0)"]
    if child_commit:
        decoration.append(f"wl_surface#{child}.commit()")
    requests = prefix + (parent + decoration if early else decoration + parent)
    return [f"[{i}.000] -> {request}\n" for i, request in enumerate(requests)]


class TraceTests(unittest.TestCase):
    def test_trace_limit_counts_bytes_including_multibyte_text(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "trace.log"
            content = "abc\n\N{LATIN SMALL LETTER E WITH ACUTE}\n".encode()
            path.write_bytes(content)
            self.assertEqual("".join(trace_lines(path, len(content))).encode(), content)
            with self.assertRaisesRegex(ValueError, "byte limit"):
                list(trace_lines(path, len(content) - 1))

    def test_stale_size_cannot_extend_read_budget(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "trace.log"
            path.write_bytes(b"x" * 9)
            metadata = os.stat_result((stat.S_IFREG, 0, 0, 1, 0, 0, 0, 0, 0, 0))
            with patch.object(os, "fstat", return_value=metadata), \
                    patch.object(Path, "stat", return_value=metadata):
                with self.assertRaisesRegex(ValueError, "byte limit"):
                    list(trace_lines(path, 8))

    @unittest.skipUnless(hasattr(os, "mkfifo"), "named pipes are unavailable on this platform")
    def test_unwritten_pipe_is_rejected_without_waiting_for_a_writer(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "trace.pipe"
            os.mkfifo(path)
            child = subprocess.Popen([sys.executable, __file__, str(path)],
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                _, error = child.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.communicate()
                self.fail(f"owned pipe-reader process {child.pid} blocked")
            self.assertEqual(child.returncode, 2)
            self.assertIn(b"regular file", error)

    def test_decorations_precede_first_client_buffer(self):
        self.assertTrue(inspect(fixture())["pass"])

    def test_inline_first_paint_is_rejected(self):
        result = inspect(fixture(early=True))
        self.assertFalse(result["pass"])
        self.assertEqual(result["decorated_windows"][0]["late_decorations"], [2])

    def test_attach_without_commit_and_empty_parent_commit_do_not_count(self):
        self.assertFalse(inspect(fixture(child_commit=False))["pass"])

    def test_every_window_and_minimum_evidence_are_checked(self):
        self.assertFalse(inspect(fixture() + fixture(11, 12, early=True), 2)["pass"])
        self.assertFalse(inspect(fixture(), 2)["pass"])
        self.assertFalse(inspect([])["pass"])

    def test_a_decoration_unmapped_before_first_parent_commit_is_rejected(self):
        trace = fixture()
        trace[-2:-2] = ["[90.000] -> wl_surface#2.attach(nil, 0, 0)\n",
                         "[91.000] -> wl_surface#2.commit()\n"]
        self.assertFalse(inspect(trace)["pass"])

    def test_uncommitted_detach_does_not_hide_a_committed_decoration(self):
        trace = fixture()
        trace[-2:-2] = ["[90.000] -> wl_surface#2.attach(nil, 0, 0)\n"]
        self.assertTrue(inspect(trace)["pass"])

    def test_reused_surface_identifiers_have_independent_lifetimes(self):
        trace = fixture() + ["[99.000] -> wl_surface#1.destroy()\n",
                             "[100.000] -> wl_surface#2.destroy()\n"] + fixture(early=True)
        result = inspect(trace, 2)
        self.assertEqual(len(result["decorated_windows"]), 2)
        self.assertFalse(result["pass"])
        self.assertEqual(result["decorated_windows"][0]["late_decorations"], [])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trace", help="WAYLAND_DEBUG=client log, or self-test")
    parser.add_argument("--minimum-windows", type=int, default=1)
    args = parser.parse_args()
    if args.trace == "self-test":
        unittest.main(argv=[__file__])
        return
    if args.minimum_windows < 1:
        parser.error("--minimum-windows must be positive")
    try:
        result = inspect(trace_lines(Path(args.trace)), args.minimum_windows)
    except (OSError, ValueError) as error:
        parser.error(str(error))
    print(json.dumps(result, indent=2))
    raise SystemExit(0 if result["pass"] else 1)


if __name__ == "__main__":
    main()
