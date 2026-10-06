"""npm advisory gate tests; every audit report and advisory is synthetic."""
from datetime import date, datetime, timedelta, timezone
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("npm-audit.py")
SPEC = importlib.util.spec_from_file_location("npm_audit", SCRIPT)
audit = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(audit)

TODAY = date(2026, 10, 6)
GHSA = "GHSA-2222-3333-4444"
OTHER = "GHSA-5555-6666-7777"
REASON = "Synthetic reviewed exception: the vulnerable path is unreachable."


def report(*advisories):
    """Build an npm audit v2 report; each advisory is (id, package, severity)."""
    vulnerabilities = {}
    for ident, package, severity in advisories:
        vulnerabilities[package] = {
            "name": package, "severity": severity, "isDirect": False, "range": "<1.0.1",
            "via": [{"source": 1, "name": package, "dependency": package, "title": f"Synthetic {package} issue",
                     "url": f"https://github.com/advisories/{ident}", "severity": severity, "range": "<1.0.1"}],
            "effects": ["parent"], "fixAvailable": False,
        }
        vulnerabilities["parent"] = {"name": "parent", "severity": severity, "isDirect": True,
                                     "via": [package], "effects": [], "range": "*", "fixAvailable": False}
    counts = {level: 0 for level in audit.LEVELS}
    for vulnerability in vulnerabilities.values():
        counts[vulnerability["severity"]] += 1
    counts["total"] = len(vulnerabilities)
    return {"auditReportVersion": 2, "vulnerabilities": vulnerabilities,
            "metadata": {"vulnerabilities": counts, "dependencies": {"total": 12}}}


def allowlist(*entries):
    return json.dumps({"advisories": [
        {"id": ident, "package": package, "justification": REASON, "expires": expires}
        for ident, package, expires in entries
    ]})


def run(data, allow=allowlist(), level="low"):
    return audit.evaluate("project", data, allow, level, TODAY)


class AdvisoryGateTests(unittest.TestCase):
    def test_clean_report_passes(self):
        passed, lines = run(report())
        self.assertTrue(passed, lines)
        self.assertIn("0 advisories", lines[0])

    def test_unlisted_advisory_at_level_fails(self):
        passed, lines = run(report((GHSA, "dep", "low")))
        self.assertFalse(passed)
        self.assertTrue(any(f"FAIL {GHSA} dep (low)" in line for line in lines), lines)

    def test_advisory_below_level_is_reported_without_failing(self):
        passed, lines = run(report((GHSA, "dep", "low")), level="moderate")
        self.assertTrue(passed, lines)
        self.assertTrue(any("below level" in line and GHSA in line for line in lines), lines)

    def test_info_is_below_the_default_low_level(self):
        self.assertTrue(run(report((GHSA, "dep", "info")))[0])

    def test_reviewed_unexpired_exception_passes(self):
        passed, lines = run(report((GHSA, "dep", "critical")), allowlist((GHSA, "dep", "2026-10-06")))
        self.assertTrue(passed, lines)
        self.assertTrue(any(f"reviewed {GHSA} dep (critical) until 2026-10-06" in line for line in lines), lines)

    def test_exception_does_not_cover_other_packages_or_advisories(self):
        allow = allowlist((GHSA, "dep", "2026-11-01"))
        self.assertFalse(run(report((GHSA, "other-dep", "high")), allow)[0])
        self.assertFalse(run(report((GHSA, "dep", "high"), (OTHER, "dep2", "high")), allow)[0])

    def test_expired_exception_fails_even_when_it_matches(self):
        passed, lines = run(report((GHSA, "dep", "high")), allowlist((GHSA, "dep", "2026-10-05")))
        self.assertFalse(passed)
        self.assertTrue(any("expired on 2026-10-05" in line for line in lines), lines)
        self.assertTrue(any(f"FAIL {GHSA}" in line for line in lines), lines)

    def test_expired_unused_exception_fails_a_clean_report(self):
        self.assertFalse(run(report(), allowlist((GHSA, "dep", "2026-01-01")))[0])

    def test_exception_review_window_is_bounded(self):
        allow = allowlist((GHSA, "dep", "2027-01-05"))
        passed, lines = run(report((GHSA, "dep", "high")), allow)
        self.assertFalse(passed)
        self.assertTrue(any("more than 90 days" in line for line in lines), lines)

    def test_stale_exception_is_a_notice(self):
        passed, lines = run(report(), allowlist((GHSA, "dep", "2026-10-20")))
        self.assertTrue(passed, lines)
        self.assertTrue(any("no longer matches" in line for line in lines), lines)

    def test_malformed_allowlists_fail_closed(self):
        valid = {"id": GHSA, "package": "dep", "justification": REASON, "expires": "2026-10-20"}
        for document in (
            "not json",
            json.dumps([]),
            json.dumps({"advisories": [], "extra": True}),
            json.dumps({"advisories": [{**valid, "note": "extra key"}]}),
            json.dumps({"advisories": [{**valid, "id": "CVE-2026-0001"}]}),
            json.dumps({"advisories": [{**valid, "package": " "}]}),
            json.dumps({"advisories": [{**valid, "justification": "ok"}]}),
            json.dumps({"advisories": [{**valid, "expires": "soon"}]}),
            json.dumps({"advisories": [{**valid, "expires": 20261020}]}),
            json.dumps({"advisories": [valid, valid]}),
        ):
            with self.subTest(document=document):
                self.assertFalse(run(report(), document)[0])

    def test_registry_errors_and_unreadable_reports_fail_closed(self):
        broken_totals = report((GHSA, "dep", "high"))
        broken_totals["vulnerabilities"] = {}
        for data in (
            None,
            [],
            {"error": {"code": "ENOAUDIT", "summary": "audit endpoint returned an error"}},
            {"auditReportVersion": 1, "advisories": {}},
            {"auditReportVersion": 2, "vulnerabilities": {"dep": {"via": "dep"}}, "metadata": {"vulnerabilities": {"total": 1}}},
            broken_totals,
        ):
            with self.subTest(data=data):
                self.assertFalse(run(data)[0])

    def test_unknown_severity_and_unidentified_advisories_fail_at_any_level(self):
        unknown = report((GHSA, "dep", "high"))
        unknown["vulnerabilities"]["dep"]["via"][0]["severity"] = "severe"
        self.assertFalse(run(unknown, allowlist(), "critical")[0])
        unidentified = report((GHSA, "dep", "high"))
        unidentified["vulnerabilities"]["dep"]["via"][0]["url"] = "https://example.invalid/advisory/1"
        passed, lines = run(unidentified, allowlist((GHSA, "dep", "2026-10-20")))
        self.assertFalse(passed)
        self.assertTrue(any("unidentified advisory" in line for line in lines), lines)

    def test_main_audits_every_project_and_requires_an_allowlist(self):
        with tempfile.TemporaryDirectory() as temp:
            clean, vulnerable, missing = (Path(temp) / name for name in ("clean", "vulnerable", "missing"))
            for project in (clean, vulnerable, missing):
                project.mkdir()
            for project in (clean, vulnerable):
                (project / audit.ALLOWLIST).write_text(allowlist(), encoding="utf-8")
            reports = {clean: report(), vulnerable: report((GHSA, "dep", "moderate")), missing: report()}
            out, err = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                self.assertEqual(audit.main([str(clean)], reports.get, TODAY), 0)
                self.assertEqual(audit.main([str(clean), str(vulnerable)], reports.get, TODAY), 1)
                self.assertEqual(audit.main([str(missing)], reports.get, TODAY), 1)
                self.assertEqual(audit.main(["--audit-level=high", str(vulnerable)], reports.get, TODAY), 0)
            self.assertIn("vulnerable: FAIL", err.getvalue())
            self.assertIn("is missing", err.getvalue())

    def test_cli_evaluates_a_saved_report_without_the_registry(self):
        with tempfile.TemporaryDirectory() as temp:
            project = Path(temp) / "project"
            project.mkdir()
            saved = Path(temp) / "audit.json"
            saved.write_text(json.dumps(report((GHSA, "dep", "high"))), encoding="utf-8")
            (project / audit.ALLOWLIST).write_text(allowlist(), encoding="utf-8")
            command = [sys.executable, str(SCRIPT), "--report", str(saved), str(project)]
            failed = subprocess.run(command, capture_output=True, text=True, check=False)
            self.assertEqual(failed.returncode, 1, failed.stderr)
            self.assertIn(f"FAIL {GHSA} dep (high)", failed.stderr)
            expires = (datetime.now(timezone.utc).date() + timedelta(days=30)).isoformat()
            (project / audit.ALLOWLIST).write_text(allowlist((GHSA, "dep", expires)), encoding="utf-8")
            passed = subprocess.run(command, capture_output=True, text=True, check=False)
            self.assertEqual(passed.returncode, 0, passed.stderr)
            self.assertIn("1 reviewed exceptions", passed.stdout)


if __name__ == "__main__":
    unittest.main()
