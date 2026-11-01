#!/usr/bin/env python3
"""Run the esctest2 and ucs-detect conformance suites in a live Kettle window.

    check-conformance.py esctest --kettle PATH [--out DIR] [--include REGEX] [--update]
    check-conformance.py ucs --kettle PATH [--out DIR] [--grapheme-mode auto|off|on]

Both tools are fetched into target/conformance at pinned versions and never
vendored: esctest2 (GPL-2.0) by git commit, ucs-detect from PyPI into a
private virtual environment. Kettle opens at exactly 80x25 cells with
/bin/sh, private XDG directories and the control server on, through the
live-UI harness, and the suite runs inside its pane.

The adapter also removes xterm's window-resize precondition from its two
DECNCSM set/query tests. Kettle supports no-clear without program-controlled
window resizing; the behavior assertions and other option decorators stay.

Kettle does not answer DECRQCRA: a checksum reply would let any program that
reads the terminal's input probe the screen. esctest reads cells with it, so
the run replaces esctest's cell check with `kettle ctl read_cells`, read after
a cursor-position report proves the terminal has parsed everything written
before it, and keeps DECRQCRA's origin-mode rule by asking the terminal for
DECOM (DECRQM) and its margins (DECRQSS).

The esctest result is compared with conformance-esctest-expected.txt, the
tests Kettle is known to fail, grouped by reason. A test that newly fails, or
newly passes, fails the run; --update rewrites the list, keeping each known
test's reason and putting new failures under an unclassified heading.
"""

import argparse
from contextlib import ExitStack, contextmanager
import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import time

REPO = Path(__file__).resolve().parents[1]
WORK = REPO / "target" / "conformance"
ESCTEST_URL = "https://github.com/ThomasDickey/esctest2.git"
ESCTEST_COMMIT = "2798f12149a19c3295e9b4853ab2da4b2eff1b2b"
UCS_DETECT = "ucs-detect==2.3.8"
EXPECTED = Path(__file__).with_name("conformance-esctest-expected.txt")
UNCLASSIFIED = "unclassified: triage before committing"
CONFIG = (
    "restore-session = false\nupdate-policy = off\nrecord = off\n"
    "shell = /bin/sh\nlogin-shell = false\nshell-integration = false\n"
    "agent-server = full\nask-before-closing = never\ncursor-blink = false\n"
    "window-width = 80\nwindow-height = 25\n"
)


def run(argv, **kwargs):
    return subprocess.run(argv, check=True, **kwargs)


def fetch_esctest():
    checkout = WORK / "esctest2"
    if (checkout / ".git").is_dir():
        head = subprocess.run(["git", "-C", str(checkout), "rev-parse", "HEAD"],
                              capture_output=True, text=True).stdout.strip()
        if head == ESCTEST_COMMIT:
            return checkout / "esctest"
        raise SystemExit(f"{checkout} is at {head}, not {ESCTEST_COMMIT}; remove it")
    WORK.mkdir(parents=True, exist_ok=True)
    run(["git", "clone", "--quiet", ESCTEST_URL, str(checkout)],
        env={**os.environ, "GIT_TERMINAL_PROMPT": "0"})
    run(["git", "-C", str(checkout), "checkout", "--quiet", ESCTEST_COMMIT])
    return checkout / "esctest"


def fetch_ucs_detect():
    venv = WORK / "ucs-venv"
    tool = venv / "bin" / "ucs-detect"
    if not tool.exists():
        WORK.mkdir(parents=True, exist_ok=True)
        run([sys.executable, "-m", "venv", str(venv)])
        run([str(venv / "bin" / "pip"), "install", "--quiet", "--disable-pip-version-check",
             UCS_DETECT])
    return tool


def load_live():
    spec = importlib.util.spec_from_file_location(
        "conformance_live", Path(__file__).with_name("check-live-ui-smoke.py"))
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def in_kettle(kettle, out, label, command_for):
    """Open Kettle, type command_for(pid, env) into its pane, wait for it."""
    live = load_live()
    with tempfile.TemporaryDirectory(prefix="kettle-conformance-") as scratch:
        root = Path(scratch)
        config = root / "kettle.conf"
        config.write_text(CONFIG)
        env = {"SHELL": "/bin/sh", "ENV": None, "BASH_ENV": None, "PS1": "$ "}
        # Blessed must inspect the terminal's capabilities, not inherited color policy.
        for key in ("NO_COLOR", "FORCE_COLOR", "CLICOLOR", "CLICOLOR_FORCE"):
            env[key] = None
        # A relative WAYLAND_DISPLAY names a socket in XDG_RUNTIME_DIR, which
        # is about to become private: pass the compositor's absolute path.
        display = os.environ.get("WAYLAND_DISPLAY")
        if display and not os.path.isabs(display) and os.environ.get("XDG_RUNTIME_DIR"):
            env["WAYLAND_DISPLAY"] = os.path.join(os.environ["XDG_RUNTIME_DIR"], display)
        for key in ("CONFIG", "STATE", "DATA", "CACHE", "RUNTIME"):
            path = root / key.lower()
            path.mkdir(mode=0o700)
            env["XDG_RUNTIME_DIR" if key == "RUNTIME" else f"XDG_{key}_HOME"] = str(path)
        done = out / f"{label}.done"
        done.unlink(missing_ok=True)
        with live.LiveKettle(str(kettle), config, out / f"{label}-kettle.log",
                             extra_env=env) as app:
            time.sleep(1.0)
            grid = app.json_ctl("read_cells", {"limit": 1})
            if (grid.get("cols"), grid.get("rows")) != (80, 25):
                raise SystemExit(f"Kettle opened at {grid.get('cols')}x{grid.get('rows')}, not 80x25")
            ctl_env = {k: v for k, v in env.items() if v is not None}
            command = command_for(root, app.pid, ctl_env)
            app.json_ctl("send_text", {"text": f"{command}; echo $? > {shlex.quote(str(done))}\r"})
            deadline = time.monotonic() + 3 * 3600
            while not done.exists():
                if time.monotonic() > deadline:
                    raise SystemExit(f"{label} did not finish within three hours")
                time.sleep(2)
            time.sleep(0.5)
            app.screenshot(out / f"{label}-final.png")
            return int(done.read_text().strip() or "1")


def esctest(args, out):
    tests = fetch_esctest()
    log = out / "esctest.log"
    log.unlink(missing_ok=True)

    def command(root, pid, ctl_env):
        config = root / "esctest.json"
        config.write_text(json.dumps({"kettle": str(args.kettle), "pid": pid,
                                      "env": ctl_env, "esctest_dir": str(tests)}))
        extra = [f"--include={args.include}"] if args.include else []
        return " ".join(shlex.quote(part) for part in [
            sys.executable, "-I", str(Path(__file__).resolve()), "esctest-in-pane",
            str(config), "--", "--expected-terminal=xterm", f"--logfile={log}",
            # Since xterm 383, mode 45 follows wrapped lines and mode 1045
            # provides the older screen-wide reverse wrapping.
            "--timeout=1", "--xterm-reverse-wrap=383", *extra])

    status = in_kettle(args.kettle, out, "esctest", command)
    text = log.read_text(errors="replace") if log.exists() else ""
    failed = set(re.findall(r"^\*\*\* TEST (\w+\.\w+) FAILED", text, re.M))
    summary = re.search(r"^\*\*\* \d+ tests? passed.*$", text, re.M)
    if status != 0 or summary is None:
        print(f"esctest did not finish (exit {status}); see {out / 'esctest-final.png'}")
        return 1
    print("esctest:", summary.group(0))
    if args.include:
        print("--include ran a subset; the expected list is not compared")
        return 0
    expected = read_expected()
    new = sorted(failed - set(expected))
    fixed = sorted(set(expected) - failed)
    for name in new:
        print("  newly failing:", name)
    for name in fixed:
        print("  newly passing:", name)
    if args.update:
        write_expected({name: expected.get(name, UNCLASSIFIED) for name in failed})
        print(f"wrote {EXPECTED.relative_to(REPO)}")
        return 0
    if new or fixed:
        print("the failing set changed: fix the regression, or run with --update and "
              "classify each new entry")
        return 1
    print(f"matches {EXPECTED.relative_to(REPO)} ({len(expected)} known failures)")
    return 0


def read_expected():
    expected, reason = {}, UNCLASSIFIED
    for line in EXPECTED.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            reason = line[1:-1]
        else:
            expected[line] = reason
    return expected


def write_expected(expected):
    header = [line for line in EXPECTED.read_text().splitlines() if line.startswith("#")]
    groups = {}
    for name, reason in expected.items():
        groups.setdefault(reason, []).append(name)
    body = []
    for reason in sorted(groups, key=lambda r: (r == UNCLASSIFIED, r)):
        body += ["", f"[{reason}]", *sorted(groups[reason])]
    EXPECTED.write_text("\n".join(header + body) + "\n")


def ucs(args, out):
    tool = fetch_ucs_detect()
    results = out / "ucs-detect.yaml"
    results.unlink(missing_ok=True)
    version = subprocess.run(
        [str(args.kettle), "--version"], capture_output=True, text=True
    ).stdout.split()
    version = version[1] if len(version) > 1 else "unknown"

    def command(_root, _pid, _ctl_env):
        return " ".join(
            shlex.quote(part)
            for part in [
                str(tool.parent / "python"),
                "-I",
                str(Path(__file__).resolve()),
                "ucs-in-pane",
                args.grapheme_mode,
                str(out / "mode2027.json"),
                "--",
                f"--save-yaml={results}",
                "--set-software-name=Kettle",
                f"--set-software-version={version}",
                "--no-final-summary",
                *(["--all"] if args.ucs_all else []),
            ]
        )

    status = in_kettle(args.kettle, out, "ucs-detect", command)
    if status != 0 or not results.exists():
        print(f"ucs-detect did not finish (exit {status})")
        return 1
    print(f"ucs-detect results in {results}")
    summary = subprocess.run(
        [str(tool.parent / "python"), "-I", "-c", UCS_SUMMARY, str(results)],
        capture_output=True,
        text=True,
    )
    print(summary.stdout or summary.stderr)
    return status


def adapt_ucs_grapheme_mode(terminal_module, mode, record):
    """Select measurement mode without changing scores or capability reports."""
    if mode == "auto":
        return
    if mode not in {"on", "off"}:
        raise ValueError("unknown grapheme measurement mode")

    @contextmanager
    def fixed_mode(term, timeout=1.0):
        term.get_dec_mode(2027, timeout=timeout, force=True)
        change = term.dec_modes_enabled if mode == "on" else term.dec_modes_disabled
        with change(2027, timeout=timeout):
            response = term.get_dec_mode(2027, timeout=timeout, force=True)
            state = {"enabled": response.enabled, "supported": response.supported}
            record(state)
            if not response.supported or response.enabled != (mode == "on"):
                raise RuntimeError(
                    "mode 2027 did not enter the requested measurement state"
                )
            yield

    terminal_module.maybe_grapheme_clustering_mode = fixed_mode


def run_ucs_suite(suite, terminal_module, mode):
    """Hold the requested mode for language and table measurements alike."""
    if mode == "auto":
        return suite.main()
    original_init = suite.init_term
    with ExitStack() as contexts:

        def init_term(*args, **kwargs):
            term, writer = original_init(*args, **kwargs)
            contexts.enter_context(term.cbreak())
            contexts.enter_context(terminal_module.maybe_grapheme_clustering_mode(term))
            return term, writer

        suite.init_term = init_term
        try:
            return suite.main()
        finally:
            suite.init_term = original_init


def ucs_in_pane(mode, receipt, arguments):
    import importlib.metadata
    import ucs_detect
    from ucs_detect import terminal

    if importlib.metadata.version("ucs-detect") != UCS_DETECT.split("==")[1]:
        raise RuntimeError("ucs-detect version differs from the pinned adapter")
    states = []

    def record(state):
        states.append(state)
        Path(receipt).write_text(
            json.dumps({"requested": mode, "states": states}, indent=2) + "\n"
        )

    Path(receipt).write_text(json.dumps({"requested": mode, "states": states}) + "\n")
    adapt_ucs_grapheme_mode(terminal, mode, record)
    sys.argv = ["ucs-detect", *arguments]
    return run_ucs_suite(ucs_detect, terminal, mode)


UCS_SUMMARY = """
import sys, yaml
results = yaml.safe_load(open(sys.argv[1]))["test_results"]
for category, versions in sorted(results.items()):
    if category == "language_results":
        total = sum(v["n_total"] for v in versions.values())
        errors = sum(v["n_errors"] for v in versions.values())
        if total:
            print(f"{category:24} {100 * (total - errors) / total:6.2f}% of {total}")
        continue
    for version, score in (versions or {}).items():
        print(f"{category:24} {score['pct_success']:6.2f}% of {score['n_total']} (Unicode {version})")
"""


def adapt_esctest_options(escutil):
    """DECNCSM works independently of Kettle's window-size policy."""
    original = escutil.optionRequired
    no_clear_cases = {"test_DECRQM_DEC_DECNCSM", "test_DECSET_DECNCSM"}

    def option_required(terminal, option, allowPassWithoutOption=False):
        def decorate(function):
            if (terminal == "xterm" and option == "xtermWinopsEnabled"
                    and function.__name__ in no_clear_cases):
                # Retain every assertion, including failures. Do not enable
                # xterm's global geometry option or force assertions to pass.
                return function
            return original(terminal, option, allowPassWithoutOption)(function)
        return decorate

    escutil.optionRequired = option_required


def esctest_in_pane(config_path, esctest_args):
    """Inside Kettle's pane: run esctest with the out-of-band cell check."""
    config = json.loads(Path(config_path).read_text())
    sys.path.insert(0, config["esctest_dir"])
    import esccmd
    import escio
    import esclog
    import esctypes
    import escutil

    ctl_env = {**os.environ, **config["env"]}

    def ctl(method, params):
        done = subprocess.run(
            [config["kettle"], "ctl", "--pid", str(config["pid"]), method,
             "--json", json.dumps(params), "--raw"],
            env=ctl_env, stdin=subprocess.DEVNULL, capture_output=True, text=True,
            timeout=15)
        if done.returncode != 0:
            raise esctypes.InternalError(
                f"kettle ctl {method} failed: {(done.stderr or done.stdout)[:300]}")
        return json.loads(done.stdout)

    def read_grid():
        params = {"limit": 1536}
        result = ctl("read_cells", params)
        cells = list(result.get("cells", []))
        while result.get("next_cursor") is not None:
            result = ctl("read_cells", {**params, "cursor": result["next_cursor"],
                                        "snapshot": result.get("snapshot")})
            cells.extend(result.get("cells", []))
        return {(cell["col"] + 1, cell["row"] + 1): cell["ch"] for cell in cells}

    def decrqss(setting):
        esccmd.DECRQSS(setting)
        return escio.ReadDCS()

    def origin_offset():
        """The (dx, dy) DECRQCRA applies to its coordinates under DECOM."""
        esccmd.DECRQM(6, DEC=True)
        params = escio.ReadCSI("$y", "?")
        if len(params) < 2 or params[1] != 1:
            return 0, 0
        dx = dy = 0
        for setting in ("r", "s"):
            try:
                reply = decrqss(setting)
            except Exception as error:  # an unanswered query keeps the offset
                esclog.LogInfo(f"adapter: DECRQSS {setting} unavailable: {error}")
                continue
            if reply.startswith("1$r"):
                first = int(reply[3:].split(";")[0]) - 1
                if setting == "r":
                    dy = first
                else:
                    dx = first
        return dx, dy

    def assert_screen_chars_in_rect_equal(rect, expected_lines):
        escutil.gHaveAsserted = True
        escutil.AssertVTLevel(4, "checksum")
        if rect.height() != len(expected_lines):
            raise esctypes.InternalError(
                f"Height of rect ({rect.height()}) does not match number of expected "
                f"lines ({len(expected_lines)})")
        escutil.GetCursorPosition()  # barrier: the terminal parsed all prior output
        dx, dy = origin_offset()
        grid = read_grid()
        actual, expected, errors = [], [], []
        for point in rect.points():
            y = point.y() - rect.top()
            x = point.x() - rect.left()
            line = expected_lines[y]
            if rect.width() != len(line):
                raise esctypes.InternalError(
                    f"Width of rect ({rect.width()}) does not match expected line {y}")
            want = ord(line[x])
            cell = grid.get((point.x() + dx, point.y() + dy), " ")
            got = ord(cell[0]) if cell and cell != "\0" else 32
            # esctest's own xterm rule: an erased cell matches an empty one.
            if want == 0 and got == 32:
                got = 0
            while len(actual) <= y:
                actual.append("")
                expected.append("")
            actual[y] += "." if got == 0 else (chr(got) if got < 256 else "?")
            expected[y] += "." if want == 0 else (chr(want) if want < 256 else "?")
            if want != got:
                errors.append(f"At {point} expected '{escutil.PrintableChar(want)}' "
                              f"(0x{want:02x}) but got '{escutil.PrintableChar(got)}' "
                              f"(0x{got:02x})")
        if errors:
            escutil.Raise(esctypes.ChecksumException(errors, actual, expected))

    escutil.AssertScreenCharsInRectEqual = assert_screen_chars_in_rect_equal
    adapt_esctest_options(escutil)
    sys.argv = [str(Path(config["esctest_dir"]) / "esctest.py"), *esctest_args]
    # esctest.py ends with a bare main(): importing it runs the suite, after the
    # patch above so each test's `from escutil import ...` binds the adapter.
    import esctest  # noqa: F401


def main():
    if len(sys.argv) > 3 and sys.argv[1] == "ucs-in-pane":
        rest = sys.argv[4:]
        return ucs_in_pane(
            sys.argv[2], sys.argv[3], rest[1:] if rest[:1] == ["--"] else rest
        )
    if len(sys.argv) > 2 and sys.argv[1] == "esctest-in-pane":
        rest = sys.argv[3:]
        esctest_in_pane(sys.argv[2], rest[1:] if rest[:1] == ["--"] else rest)
        return 0
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("suite", choices=["esctest", "ucs"])
    parser.add_argument("--kettle", type=Path, required=True)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--include", help="esctest: run only tests matching this regex")
    parser.add_argument(
        "--ucs-all", action="store_true",
        help="ucs: measure all width-table entries, not only contested entries",
    )
    parser.add_argument(
        "--grapheme-mode",
        choices=["auto", "off", "on"],
        default="auto",
        help="ucs: retain suite negotiation, or query and hold mode 2027 off/on",
    )
    parser.add_argument(
        "--update",
        action="store_true",
        help="esctest: rewrite the expected-failure list",
    )
    args = parser.parse_args()
    if args.suite != "ucs" and (args.grapheme_mode != "auto" or args.ucs_all):
        parser.error("--grapheme-mode and --ucs-all apply only to ucs")
    args.kettle = args.kettle.resolve()
    # The pane starts in its own directory, so every path it gets is absolute.
    out = (
        args.out
        or REPO / "target" / "diagnostics" / time.strftime("conformance-%Y%m%d-%H%M%S")
    ).resolve()
    out.mkdir(parents=True, exist_ok=True)
    return esctest(args, out) if args.suite == "esctest" else ucs(args, out)


if __name__ == "__main__":
    sys.exit(main())
