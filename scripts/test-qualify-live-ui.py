#!/usr/bin/env python3
"""Qualification must reject short, failed, stale or unclosed runs."""

import importlib.util
from contextlib import redirect_stdout
from io import StringIO
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from types import SimpleNamespace
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location(
    "qualification", Path(__file__).with_name("qualify-live-ui.py")
)
qualification = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qualification)


class QualificationReceipts(unittest.TestCase):
    def receipts(self):
        return [
            {
                "source": "a" * 40,
                "tree": "b" * 40,
                "allocation": f"job-{i}",
                "binary_sha256": "c" * 64,
                "helper_sha256": "d" * 64,
                "counts": dict.fromkeys(qualification.SCENARIOS, 100),
                "status": "PASS",
                "unchanged": True,
            }
            for i in range(3)
        ]

    def test_all_six_need_300_repetitions_across_three_allocations(self):
        result = qualification.summarize(self.receipts())
        self.assertEqual(result["counts"], dict.fromkeys(qualification.SCENARIOS, 300))
        self.assertEqual(result["allocations"], 3)
        with self.assertRaises(ValueError):
            qualification.summarize(self.receipts()[:2])
        for case in qualification.SCENARIOS:
            receipts = self.receipts()
            receipts[1]["counts"][case] -= 1
            with self.subTest(case=case), self.assertRaises(ValueError):
                qualification.summarize(receipts)

    def test_identity_failure_and_cleanup_cannot_be_averaged_away(self):
        for key, value in [
            ("allocation", "job-0"),
            ("source", "other"),
            ("tree", "other"),
            ("binary_sha256", "other"),
            ("helper_sha256", "other"),
            ("status", "FAIL"),
            ("unchanged", False),
        ]:
            receipts = self.receipts()
            receipts[1][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                qualification.summarize(receipts)

    def test_an_allocation_must_exercise_every_scenario(self):
        receipts = self.receipts()
        receipts[0]["counts"]["search-history"] = 300
        receipts[1]["counts"]["search-history"] = 0
        receipts[2]["counts"]["search-history"] = 0
        with self.assertRaises(ValueError):
            qualification.summarize(receipts)

    def test_allocation_counts_must_match_the_100_round_protocol(self):
        receipts = self.receipts()
        receipts[0]["counts"]["program-keys"] = 101
        with self.assertRaises(ValueError):
            qualification.summarize(receipts)


class CaseSupervision(unittest.TestCase):
    @unittest.skipUnless(os.name == "posix", "Unix wait events")
    def test_only_terminal_wait_events_are_exits(self):
        live = qualification.load_live()
        process = SimpleNamespace(pid=42, returncode=None)
        for code, expected in [
            (None, False),
            (os.CLD_STOPPED, False),
            (os.CLD_CONTINUED, False),
            (os.CLD_TRAPPED, False),
            (os.CLD_EXITED, True),
            (os.CLD_KILLED, True),
            (os.CLD_DUMPED, True),
        ]:
            event = None if code is None else SimpleNamespace(si_code=code)
            with (
                self.subTest(code=code),
                patch.object(live.os, "waitid", return_value=event),
            ):
                self.assertEqual(live.process_exited_without_reaping(process), expected)

    @unittest.skipUnless(os.name == "posix", "native Unix termination witness")
    def test_normal_teardown_witnesses_the_signal_to_a_live_stopped_root(self):
        live = qualification.load_live()
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            ready = root / "ready"
            target = subprocess.Popen(
                [
                    sys.executable,
                    "-c",
                    f"from pathlib import Path;import time;Path({str(ready)!r}).touch();time.sleep(60)",
                ],
                start_new_session=True,
            )
            decoy = subprocess.Popen(
                [sys.executable, "-c", "import time;time.sleep(60)"]
            )
            (root / "owners.json").write_text(
                json.dumps({"target": target.pid, "decoy": decoy.pid})
            )
            try:
                deadline = time.monotonic() + 5
                while not ready.exists():
                    self.assertLess(time.monotonic(), deadline)
                    time.sleep(0.01)
                os.kill(target.pid, signal.SIGSTOP)
                deadline = time.monotonic() + 5
                while True:
                    stopped = os.waitid(
                        os.P_PID,
                        target.pid,
                        os.WSTOPPED | os.WNOHANG | os.WNOWAIT,
                    )
                    if stopped is not None and stopped.si_code == os.CLD_STOPPED:
                        break
                    self.assertLess(time.monotonic(), deadline)
                    time.sleep(0.01)
                self.assertFalse(live.process_exited_without_reaping(target))
                receipt = live.terminate_owned_process_group(target, grace_s=0.05)
                self.assertEqual(receipt["pid"], target.pid)
                self.assertIn(
                    {"signal": -target.returncode, "root_alive": True},
                    receipt["signals"],
                )
                self.assertIsNone(decoy.poll())
            finally:
                if target.poll() is None:
                    target.kill()
                target.wait(timeout=5)
                decoy.kill()
                decoy.wait(timeout=5)

    @unittest.skipUnless(os.name == "posix", "native Unix termination witness")
    def test_already_exited_root_has_no_live_signal_witness(self):
        live = qualification.load_live()
        target = subprocess.Popen(
            [sys.executable, "-c", "raise SystemExit(7)"], start_new_session=True
        )
        try:
            deadline = time.monotonic() + 5
            while not live.process_exited_without_reaping(target):
                self.assertLess(time.monotonic(), deadline)
                time.sleep(0.01)
            receipt = live.terminate_owned_process_group(target, grace_s=0.05)
            self.assertEqual(target.returncode, 7)
            self.assertTrue(
                all(item["root_alive"] is False for item in receipt["signals"])
            )
        finally:
            if target.poll() is None:
                target.kill()
            target.wait(timeout=5)

    def invoke(self, receipt, returncode=0, waits=None):
        with tempfile.TemporaryDirectory() as scratch:
            out = Path(scratch) / "case"
            proc = Mock(
                pid=123,
                wait=Mock(side_effect=waits)
                if waits
                else Mock(return_value=returncode),
            )

            def start(*args, **kwargs):
                if receipt is not None:
                    (out / "case.json").write_text(
                        json.dumps(receipt), encoding="utf-8"
                    )
                return proc

            live = Mock()
            scope = live.LinuxSubreaperScope.acquire.return_value
            with (
                patch.object(qualification.subprocess, "Popen", side_effect=start),
                patch.object(qualification, "load_live", return_value=live),
                patch.object(
                    qualification,
                    "stop_owned_tree",
                    return_value={"closed": True, "processes": []},
                ) as stop,
            ):
                result = qualification.invoke_case(
                    "search-history", Path("binary"), out
                )
                if result["timed_out"]:
                    stop.assert_called_once_with(
                        live,
                        proc,
                        out,
                        scope if qualification.platform.system() == "Linux" else None,
                    )
            return result, proc

    def test_process_exit_zero_requires_case_and_cleanup_receipts(self):
        good = {"case": "search-history", "status": "PASS", "cleanup": True}
        self.assertTrue(self.invoke(good)[0]["passed"])
        for change in [{"case": "other"}, {"status": "FAIL"}, {"cleanup": False}]:
            receipt = {**good, **change}
            self.assertFalse(self.invoke(receipt)[0]["passed"])
        self.assertFalse(self.invoke(None)[0]["passed"])
        self.assertFalse(self.invoke(good, returncode=1)[0]["passed"])

    def test_timeout_closes_the_owned_tree_and_never_counts_as_pass(self):
        good = {"case": "search-history", "status": "PASS", "cleanup": True}
        result, proc = self.invoke(
            good, waits=[subprocess.TimeoutExpired("owned", 300), 0]
        )
        self.assertFalse(result["passed"])
        self.assertTrue(result["timed_out"])
        proc.send_signal.assert_not_called()
        proc.kill.assert_not_called()

    def test_parent_interrupt_closes_the_owned_tree_before_propagating(self):
        with tempfile.TemporaryDirectory() as scratch:
            out = Path(scratch) / "case"
            live = Mock()
            proc = Mock(pid=123, wait=Mock(side_effect=[KeyboardInterrupt(), -9]))
            with (
                patch.object(qualification, "load_live", return_value=live),
                patch.object(qualification.subprocess, "Popen", return_value=proc),
                patch.object(qualification, "stop_owned_tree") as stop,
                self.assertRaises(KeyboardInterrupt),
            ):
                qualification.invoke_case("search-history", Path("binary"), out)
            stop.assert_called_once()
            self.assertEqual(stop.call_args.args[:3], (live, proc, out))
            self.assertEqual(proc.wait.call_count, 2)

    @unittest.skipUnless(os.name == "posix", "native Unix ownership regression")
    def test_forced_timeout_closes_detached_descendants_and_preserves_decoy(self):
        self.exercise_forced_timeout()

    @unittest.skipUnless(os.name == "posix", "native Unix ownership regression")
    def test_forced_timeout_retries_transient_inventory_failure(self):
        self.exercise_forced_timeout(inventory_failure=True)

    def exercise_forced_timeout(self, inventory_failure=False):
        live = qualification.load_live()
        real_popen = subprocess.Popen
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            out = root / "case"
            fixture = root / "fixture.py"
            fixture.write_text(
                "import json,os,signal,subprocess,sys,time\n"
                "from pathlib import Path\n"
                "root=Path(sys.argv[1]); role=sys.argv[2]\n"
                "signal.signal(signal.SIGINT,signal.SIG_IGN)\n"
                "signal.signal(signal.SIGTERM,signal.SIG_IGN)\n"
                "(root/(role+'.pid')).write_text(str(os.getpid()))\n"
                "if role!='grandchild':\n"
                " child='child' if role=='supervisor' else 'grandchild'\n"
                " subprocess.Popen([sys.executable,__file__,str(root),child],start_new_session=True)\n"
                "while True: time.sleep(1)\n",
                encoding="utf-8",
            )
            decoy = real_popen([sys.executable, "-c", "import time;time.sleep(60)"])
            recorded = []

            def start(*args, **kwargs):
                proc = real_popen(
                    [sys.executable, str(fixture), str(out), "supervisor"], **kwargs
                )
                recorded.append(proc)
                deadline = time.monotonic() + 5
                while not (out / "grandchild.pid").exists():
                    if time.monotonic() >= deadline:
                        raise AssertionError("owned descendant did not start")
                    time.sleep(0.01)
                return proc

            try:

                def dispatch(command, *args, **kwargs):
                    if len(command) > 2 and command[2] == "case":
                        return start(command, *args, **kwargs)
                    return real_popen(command, *args, **kwargs)

                real_run = live.run
                injected = []

                def inventory(command, *args, **kwargs):
                    if (
                        inventory_failure
                        and not injected
                        and command == ["ps", "-axo", "pid=,ppid=,uid="]
                        and (out / "grandchild.pid").exists()
                    ):
                        injected.append(True)
                        raise OSError("injected transient process inventory failure")
                    return real_run(command, *args, **kwargs)

                with (
                    patch.object(
                        qualification.subprocess, "Popen", side_effect=dispatch
                    ),
                    patch.object(qualification, "load_live", return_value=live),
                    patch.object(live, "run", side_effect=inventory),
                ):
                    result = qualification.invoke_case(
                        "search-history", Path("binary"), out, timeout=0.05
                    )
                self.assertFalse(result["passed"])
                self.assertTrue(result["timed_out"])
                expected = {int(p.read_text()) for p in out.glob("*.pid")}
                self.assertEqual(len(expected), 3)
                for pid in expected:
                    handle = live._open_stable_process_if_present(pid, "test fixture")
                    if handle is not None:
                        try:
                            self.assertTrue(
                                (live._process_state(handle) or "Z").startswith("Z")
                            )
                        finally:
                            handle.close()
                self.assertTrue(result["forced_cleanup"]["closed"])
                self.assertNotIn("error", result["forced_cleanup"])
                self.assertEqual(
                    expected,
                    {p["pid"] for p in result["forced_cleanup"]["processes"]},
                )
                if inventory_failure:
                    self.assertEqual(injected, [True])
                    self.assertIn(
                        "injected transient",
                        result["forced_cleanup"]["inventory_errors"][0],
                    )
                self.assertIsNone(decoy.poll())
            finally:
                # Also close recorded fixtures if the regression fails against old code.
                for path in out.glob("*.pid"):
                    handle = live._open_stable_process_if_present(
                        int(path.read_text()), "recorded test fixture"
                    )
                    if handle is not None:
                        try:
                            handle.signal(signal.SIGKILL)
                        finally:
                            handle.close()
                for proc in recorded:
                    proc.wait(timeout=5)
                decoy.kill()
                decoy.wait(timeout=5)

    @unittest.skipUnless(sys.platform == "linux", "native Linux subreaper regression")
    def test_successful_supervisor_cannot_leave_an_orphan_and_qualify(self):
        real_popen = subprocess.Popen
        live = qualification.load_live()
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            out = root / "case"
            fixture = root / "fixture.py"
            fixture.write_text(
                "import json,subprocess,sys\nfrom pathlib import Path\n"
                "root=Path(sys.argv[1])\n"
                "child=subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)'],start_new_session=True)\n"
                "(root/'child.pid').write_text(str(child.pid))\n"
                "(root/'case.json').write_text(json.dumps({'case':'search-history','status':'PASS','cleanup':True}))\n",
                encoding="utf-8",
            )
            decoy = real_popen([sys.executable, "-c", "import time;time.sleep(60)"])

            def dispatch(command, *args, **kwargs):
                if len(command) > 2 and command[2] == "case":
                    return real_popen(
                        [sys.executable, str(fixture), str(out)], **kwargs
                    )
                return real_popen(command, *args, **kwargs)

            try:
                with patch.object(
                    qualification.subprocess, "Popen", side_effect=dispatch
                ):
                    result = qualification.invoke_case(
                        "search-history", Path("binary"), out, timeout=5
                    )
                self.assertFalse(result["passed"])
                self.assertEqual(result["exit"], 0)
                self.assertFalse(result["timed_out"])
                self.assertTrue(result["forced_cleanup"]["closed"])
                self.assertNotIn("error", result["forced_cleanup"])
                self.assertEqual(
                    [p["pid"] for p in result["forced_cleanup"]["processes"]],
                    [int((out / "child.pid").read_text())],
                )
                self.assertIsNone(decoy.poll())
            finally:
                if (out / "child.pid").exists():
                    handle = live._open_stable_process_if_present(
                        int((out / "child.pid").read_text()), "recorded test orphan"
                    )
                    if handle is not None:
                        try:
                            handle.signal(signal.SIGKILL)
                        finally:
                            handle.close()
                decoy.kill()
                decoy.wait(timeout=5)

    def test_case_observes_fixture_exit_and_propagates_the_original_assertion(self):
        source = """class Process:
    pid = 42
    closed = False
    def poll(self): return 0 if self.closed else None
class LiveKettle:
    def __init__(self): self.proc = Process()
    def __enter__(self): return self
    def __exit__(self, *args): self.proc.closed = True

def run_search_history(binary, out):
    with LiveKettle():
        raise AssertionError("real UI failure")
"""
        with tempfile.TemporaryDirectory() as scratch:
            out = Path(scratch)
            helper = out / "helper.py"
            helper.write_text(source, encoding="utf-8")
            with patch.object(qualification, "HELPER", helper):
                with self.assertRaisesRegex(AssertionError, "real UI failure"):
                    qualification.run_case("search-history", Path("binary"), out)
            receipt = json.loads((out / "case.json").read_text(encoding="utf-8"))
            self.assertEqual(receipt["status"], "FAIL")
            self.assertTrue(receipt["cleanup"])
            self.assertEqual(receipt["fixtures"][0]["pid"], 42)
            self.assertTrue(receipt["fixtures"][0]["closed"])
            helper.write_text(
                source.replace('raise AssertionError("real UI failure")', "pass"),
                encoding="utf-8",
            )
            with patch.object(qualification, "HELPER", helper):
                self.assertEqual(
                    qualification.run_case("search-history", Path("binary"), out), 0
                )
            helper.write_text(
                source.replace("self.proc.closed = True", "pass").replace(
                    'raise AssertionError("real UI failure")', "pass"
                ),
                encoding="utf-8",
            )
            with patch.object(qualification, "HELPER", helper):
                self.assertEqual(
                    qualification.run_case("search-history", Path("binary"), out), 1
                )

    def test_relative_output_artifacts_work_in_a_different_pty_cwd(self):
        source = """import subprocess,sys
from pathlib import Path
class Process:
    pid = 42
    closed = False
    def poll(self): return 0 if self.closed else None
class LiveKettle:
    def __init__(self): self.proc = Process()
    def __enter__(self): return self
    def __exit__(self, *args): self.proc.closed = True
def run_program_keys(binary, out):
    artifact = out / "recorder-fixture.txt"
    artifact.write_text("fixture")
    foreign = out / "pty-cwd"
    foreign.mkdir()
    with LiveKettle():
        subprocess.run([sys.executable, "-c",
            "from pathlib import Path;import sys;assert Path(sys.argv[1]).read_text() == 'fixture'",
            str(artifact)], cwd=foreign, check=True, capture_output=True)
"""
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            out = root / "case"
            out.mkdir()
            helper = root / "helper.py"
            helper.write_text(source, encoding="utf-8")
            relative = Path(os.path.relpath(out, Path.cwd()))
            with patch.object(qualification, "HELPER", helper):
                self.assertEqual(
                    qualification.run_case("program-keys", Path("binary"), relative),
                    0,
                )
            receipt = json.loads((out / "case.json").read_text(encoding="utf-8"))
            self.assertEqual(receipt["status"], "PASS")
            self.assertTrue(receipt["cleanup"])

    def test_a_crashed_fixture_cannot_qualify_just_because_it_closed(self):
        source = """class Process:
    pid = 42
    closed = False
    def poll(self): return -6 if self.closed else None
class LiveKettle:
    def __init__(self): self.proc = Process()
    def __enter__(self): return self
    def __exit__(self, *args): self.proc.closed = True
def run_search_history(binary, out):
    with LiveKettle(): pass
"""
        with tempfile.TemporaryDirectory() as scratch:
            out = Path(scratch)
            helper = out / "helper.py"
            helper.write_text(source, encoding="utf-8")
            with patch.object(qualification, "HELPER", helper):
                self.assertEqual(
                    qualification.run_case("search-history", Path("binary"), out), 1
                )
            receipt = json.loads((out / "case.json").read_text(encoding="utf-8"))
            self.assertTrue(receipt["cleanup"])
            self.assertEqual(receipt["status"], "FAIL")
            self.assertEqual(receipt["fixtures"][0]["exit"], -6)

    def test_only_a_witnessed_normal_fixture_signal_can_qualify(self):
        source = """class Process:
    pid = 42
    closed = False
    def poll(self): return -9 if self.closed else None
class LiveKettle:
    def __init__(self): self.proc = Process()
    def __enter__(self): return self
    def __exit__(self, *args):
        self._termination_receipt = WITNESS
        self.proc.closed = True
def run_search_history(binary, out):
    with LiveKettle(): pass
"""
        for witness, expected in [
            ({"pid": 42, "signals": [{"signal": 9, "root_alive": True}]}, 0),
            ({"pid": 43, "signals": [{"signal": 9, "root_alive": True}]}, 1),
            ({"pid": 42, "signals": [{"signal": 9, "root_alive": False}]}, 1),
            ({"pid": 42, "signals": [{"signal": 15, "root_alive": True}]}, 1),
            ({"pid": 42, "signals": []}, 1),
            (None, 1),
        ]:
            with (
                self.subTest(witness=witness),
                tempfile.TemporaryDirectory() as scratch,
            ):
                out = Path(scratch)
                helper = out / "helper.py"
                helper.write_text("WITNESS = " + repr(witness) + "\n" + source)
                with patch.object(qualification, "HELPER", helper):
                    self.assertEqual(
                        qualification.run_case("search-history", Path("binary"), out),
                        expected,
                    )


class CampaignAdmission(unittest.TestCase):
    def test_initial_identity_failure_keeps_a_failed_zero_count_receipt(self):
        with tempfile.TemporaryDirectory() as scratch:
            binary = (Path(scratch) / "binary").resolve()
            binary.touch()
            out = Path(scratch) / "out"
            with (
                patch.object(
                    qualification, "identity", side_effect=ValueError("dirty checkout")
                ),
                patch.object(qualification, "invoke_case") as invoke,
            ):
                self.assertEqual(
                    qualification.campaign(binary, out, "allocation", {}), 1
                )
            invoke.assert_not_called()
            receipt = json.loads((out / "summary.json").read_text(encoding="utf-8"))
            self.assertEqual(receipt["status"], "FAIL")
            self.assertEqual(sum(receipt["counts"].values()), 0)
            self.assertIn("dirty checkout", receipt["admission_error"])

    def test_invalid_manifest_keeps_a_failed_zero_count_receipt(self):
        for kind in ("missing", "malformed", "directory"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as scratch:
                root = Path(scratch)
                manifest = root / "identity.json"
                if kind == "malformed":
                    manifest.write_text("{invalid", encoding="utf-8")
                elif kind == "directory":
                    manifest.mkdir()
                out = root / "out"
                with (
                    patch(
                        "sys.argv",
                        [
                            "qualify",
                            "run",
                            "--kettle",
                            str(root / "binary"),
                            "--out",
                            str(out),
                            "--allocation",
                            "allocation",
                            "--identity",
                            str(manifest),
                        ],
                    ),
                    patch.object(qualification, "invoke_case") as invoke,
                ):
                    self.assertEqual(qualification.main(), 1)
                invoke.assert_not_called()
                receipt = json.loads((out / "summary.json").read_text(encoding="utf-8"))
                self.assertEqual(receipt["status"], "FAIL")
                self.assertFalse(receipt["admitted"])
                self.assertEqual(sum(receipt["counts"].values()), 0)
                self.assertTrue(receipt["admission_error"])

    def test_aggregate_missing_receipt_produces_a_failed_result(self):
        with tempfile.TemporaryDirectory() as scratch:
            out = Path(scratch) / "result.json"
            with (
                patch(
                    "sys.argv",
                    [
                        "qualify",
                        "summarize",
                        str(Path(scratch) / "missing.json"),
                        "--out",
                        str(out),
                    ],
                ),
                redirect_stdout(StringIO()),
            ):
                self.assertEqual(qualification.main(), 1)
            self.assertEqual(
                json.loads(out.read_text(encoding="utf-8"))["status"], "FAIL"
            )

    def test_source_revision_change_and_dirty_checkout_fail_finalization(self):
        for final in [{"source": "changed"}, ValueError("checkout changed")]:
            with self.subTest(final=final), tempfile.TemporaryDirectory() as scratch:
                binary = (Path(scratch) / "binary").resolve()
                binary.touch()
                out = Path(scratch) / "out"
                candidate = {
                    "source": "source",
                    "tree": "tree",
                    "binary_sha256": "binary",
                    "helper_sha256": "helper",
                }
                last = {**candidate, **final} if isinstance(final, dict) else final
                with (
                    patch.object(
                        qualification, "identity", side_effect=[candidate, last]
                    ),
                    patch.object(
                        qualification,
                        "digest",
                        side_effect=lambda p: "binary" if p == binary else "helper",
                    ),
                    patch.object(
                        qualification, "invoke_case", return_value={"passed": True}
                    ),
                    redirect_stdout(StringIO()),
                ):
                    code = qualification.campaign(binary, out, "allocation", candidate)
                self.assertEqual(code, 1)
                receipt = json.loads((out / "summary.json").read_text(encoding="utf-8"))
                self.assertEqual(receipt["status"], "FAIL")
                self.assertFalse(receipt["unchanged"])

    def campaign(self, result, changed=False, mismatch=False):
        candidate = {
            "source": "source",
            "tree": "tree",
            "binary_sha256": "binary",
            "helper_sha256": "helper",
        }
        with tempfile.TemporaryDirectory() as scratch:
            binary = (Path(scratch) / "binary").resolve()
            binary.touch()
            out = Path(scratch) / "out"
            expected = dict(candidate)
            if mismatch:
                expected["binary_sha256"] = "other"

            def digest(path):
                return (
                    "changed" if changed else ("binary" if path == binary else "helper")
                )

            with (
                patch.object(
                    qualification,
                    "identity",
                    side_effect=[
                        candidate,
                        {**candidate, "binary_sha256": "changed"}
                        if changed
                        else candidate,
                    ],
                ),
                patch.object(qualification, "digest", side_effect=digest),
                patch.object(
                    qualification, "invoke_case", return_value=result
                ) as invoke,
                redirect_stdout(StringIO()),
            ):
                if mismatch:
                    self.assertEqual(
                        qualification.campaign(binary, out, "allocation", expected), 1
                    )
                    self.assertEqual(
                        json.loads((out / "summary.json").read_text(encoding="utf-8"))[
                            "status"
                        ],
                        "FAIL",
                    )
                    invoke.assert_not_called()
                    return None, None, None
                code = qualification.campaign(binary, out, "allocation", expected)
            return (
                code,
                json.loads((out / "summary.json").read_text(encoding="utf-8")),
                invoke.call_count,
            )

    def test_complete_campaign_runs_every_scenario_100_times(self):
        code, receipt, calls = self.campaign({"passed": True})
        self.assertEqual(code, 0)
        self.assertEqual(receipt["status"], "PASS")
        self.assertEqual(receipt["counts"], dict.fromkeys(qualification.SCENARIOS, 100))
        self.assertEqual(calls, 600)

    def test_first_failure_stops_without_retries_or_an_earned_pass(self):
        code, receipt, calls = self.campaign({"passed": False})
        self.assertEqual(code, 1)
        self.assertEqual(receipt["status"], "FAIL")
        self.assertTrue(receipt["unchanged"])
        self.assertEqual(calls, 1)
        self.assertEqual(sum(receipt["counts"].values()), 0)

    def test_changed_candidate_and_mismatched_artifact_fail(self):
        code, receipt, _ = self.campaign({"passed": True}, changed=True)
        self.assertEqual(code, 1)
        self.assertFalse(receipt["unchanged"])
        self.campaign({"passed": True}, mismatch=True)


if __name__ == "__main__":
    unittest.main()
