#!/usr/bin/env python3
"""Qualification must reject short, failed, stale or unclosed runs."""

import importlib.util
from contextlib import redirect_stdout
from io import StringIO
import json
from pathlib import Path
import signal
import subprocess
import tempfile
import unittest
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

            with patch.object(qualification.subprocess, "Popen", side_effect=start):
                result = qualification.invoke_case(
                    "search-history", Path("binary"), out
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

    def test_timeout_interrupts_the_recorded_supervisor_and_never_counts_as_pass(self):
        good = {"case": "search-history", "status": "PASS", "cleanup": True}
        result, proc = self.invoke(
            good, waits=[subprocess.TimeoutExpired("owned", 300), 0]
        )
        self.assertFalse(result["passed"])
        self.assertTrue(result["timed_out"])
        proc.send_signal.assert_called_once_with(signal.SIGINT)
        proc.kill.assert_not_called()

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
            self.assertEqual(receipt["fixtures"], [{"pid": 42, "closed": True}])
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
