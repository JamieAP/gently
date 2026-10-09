"""RustSec advisory gate tests; every audit report and advisory is synthetic."""
from datetime import date, timedelta
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("rust-audit.py")
SPEC = importlib.util.spec_from_file_location("rust_audit", SCRIPT)
audit = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(audit)

TODAY = date(2026, 10, 9)
RUSTSEC = "RUSTSEC-2026-0001"
OTHER = "RUSTSEC-2026-0002"
REASON = "Synthetic reviewed exception: the vulnerable path is unreachable."


def entry(ident, package):
    advisory = None if ident is None else {"id": ident, "package": package, "title": f" Synthetic {package} issue",
                                           "url": f"https://rustsec.org/advisories/{ident}"}
    return {"advisory": advisory, "package": {"name": package, "version": "1.0.0"}}


def report(vulnerabilities=(), **warnings):
    """Build a cargo-audit JSON report; findings are (id, crate) pairs."""
    listed = [entry(ident, package) for ident, package in vulnerabilities]
    return {
        "database": {"advisory-count": 1296, "last-commit": "a" * 40, "last-updated": "2026-10-09T00:00:00+00:00"},
        "lockfile": {"dependency-count": 408},
        "settings": {},
        "vulnerabilities": {"found": bool(listed), "count": len(listed), "list": listed},
        "warnings": {kind: [entry(ident, package) for ident, package in items] for kind, items in warnings.items()},
    }


def allowlist(*entries):
    return json.dumps({"advisories": [
        {"id": ident, "package": package, "justification": REASON, "expires": expires}
        for ident, package, expires in entries
    ]})


EMPTY = allowlist()
SOON = (TODAY + timedelta(days=30)).isoformat()


class RustAdvisoryGateTests(unittest.TestCase):
    def test_clean_report_passes_and_records_the_database(self):
        passed, lines = audit.evaluate(report(), EMPTY, TODAY)
        self.assertTrue(passed)
        self.assertIn("0 findings (none) in 408 locked crates against 1296 RustSec advisories", lines[0])
        self.assertIn("database aaaaaaaaaaaa", lines[0])

    def test_unlisted_vulnerability_fails(self):
        passed, lines = audit.evaluate(report([(RUSTSEC, "rustls")]), EMPTY, TODAY)
        self.assertFalse(passed)
        self.assertTrue(any(line.startswith(f"Cargo.lock: FAIL {RUSTSEC} rustls 1.0.0 (vulnerability)") for line in lines))

    def test_every_warning_kind_fails_unless_reviewed(self):
        for kind in ("unmaintained", "unsound", "notice"):
            with self.subTest(kind=kind):
                passed, lines = audit.evaluate(report(**{kind: [(RUSTSEC, "anyhow")]}), EMPTY, TODAY)
                self.assertFalse(passed)
                self.assertTrue(any(f"({kind})" in line and "FAIL" in line for line in lines))

    def test_yanked_crates_have_no_id_and_cannot_be_excepted(self):
        passed, lines = audit.evaluate(report(yanked=[(None, "left-pad")]),
                                       allowlist((RUSTSEC, "left-pad", SOON)), TODAY)
        self.assertFalse(passed)
        self.assertTrue(any("FAIL unidentified finding left-pad" in line for line in lines))

    def test_reviewed_unexpired_exception_passes(self):
        passed, lines = audit.evaluate(report([(RUSTSEC, "quinn-proto")]),
                                       allowlist((RUSTSEC, "quinn-proto", SOON)), TODAY)
        self.assertTrue(passed)
        self.assertTrue(any(line.startswith(f"Cargo.lock: reviewed {RUSTSEC} quinn-proto") for line in lines))

    def test_exception_does_not_cover_other_crates_or_advisories(self):
        listed = allowlist((RUSTSEC, "rustls", SOON))
        self.assertFalse(audit.evaluate(report([(RUSTSEC, "quinn-proto")]), listed, TODAY)[0])
        self.assertFalse(audit.evaluate(report([(OTHER, "rustls")]), listed, TODAY)[0])

    def test_expired_exception_fails_even_when_it_matches(self):
        expired = (TODAY - timedelta(days=1)).isoformat()
        passed, lines = audit.evaluate(report([(RUSTSEC, "rustls")]), allowlist((RUSTSEC, "rustls", expired)), TODAY)
        self.assertFalse(passed)
        self.assertTrue(any("expired on" in line for line in lines))

    def test_exception_review_window_is_bounded(self):
        late = (TODAY + timedelta(days=91)).isoformat()
        passed, lines = audit.evaluate(report([(RUSTSEC, "rustls")]), allowlist((RUSTSEC, "rustls", late)), TODAY)
        self.assertFalse(passed)
        self.assertTrue(any("more than 90 days" in line for line in lines))

    def test_stale_exception_is_a_notice(self):
        passed, lines = audit.evaluate(report(), allowlist((RUSTSEC, "rustls", SOON)), TODAY)
        self.assertTrue(passed)
        self.assertTrue(any("no longer matches; remove it" in line for line in lines))

    def test_malformed_allowlists_fail_closed(self):
        cases = [
            "not json",
            json.dumps([]),
            json.dumps({"advisories": [], "extra": 1}),
            json.dumps({"advisories": [{"id": RUSTSEC, "package": "rustls", "expires": SOON}]}),
            json.dumps({"advisories": [{"id": "GHSA-2222-3333-4444", "package": "rustls",
                                        "justification": REASON, "expires": SOON}]}),
            json.dumps({"advisories": [{"id": RUSTSEC, "package": " ", "justification": REASON, "expires": SOON}]}),
            json.dumps({"advisories": [{"id": RUSTSEC, "package": "rustls", "justification": "short", "expires": SOON}]}),
            json.dumps({"advisories": [{"id": RUSTSEC, "package": "rustls", "justification": REASON, "expires": "soon"}]}),
            allowlist((RUSTSEC, "rustls", SOON), (RUSTSEC, "rustls", SOON)),
        ]
        for text in cases:
            with self.subTest(text=text):
                self.assertFalse(audit.evaluate(report(), text, TODAY)[0])

    def test_unreadable_and_inconsistent_reports_fail_closed(self):
        broken = report([(RUSTSEC, "rustls")])
        broken["vulnerabilities"]["count"] = 0
        unflagged = report([(RUSTSEC, "rustls")])
        unflagged["vulnerabilities"]["found"] = False
        no_package = report()
        no_package["warnings"] = {"unsound": [{"advisory": None}]}
        for value in (None, [], {}, {"database": {}}, broken, unflagged, no_package):
            with self.subTest(value=value):
                passed, lines = audit.evaluate(value, EMPTY, TODAY)
                self.assertFalse(passed)
                self.assertTrue(lines)

    def test_main_requires_an_allowlist_and_runs_the_scanner(self):
        calls = []

        def scanner(root, executable):
            calls.append((root, executable))
            return report()

        with tempfile.TemporaryDirectory() as tmp, contextlib.redirect_stderr(io.StringIO()) as err:
            root = Path(tmp)
            self.assertEqual(audit.main([str(root)], audit=scanner, today=TODAY), 1)
            self.assertIn("audit-allowlist.json is missing", err.getvalue())
            self.assertEqual(calls, [])
            (root / "audit-allowlist.json").write_text(EMPTY, encoding="utf-8")
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(audit.main([str(root), "--cargo-audit", "/opt/cargo-audit"],
                                            audit=scanner, today=TODAY), 0)
            self.assertEqual(calls, [(root, "/opt/cargo-audit")])

    def test_cli_evaluates_a_saved_report_without_fetching(self):
        def scanner(root, executable):
            raise AssertionError("a saved report must not run the scanner")

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "audit-allowlist.json").write_text(EMPTY, encoding="utf-8")
            saved = root / "report.json"
            saved.write_text(json.dumps(report([(RUSTSEC, "rustls")])), encoding="utf-8")
            with contextlib.redirect_stderr(io.StringIO()) as err:
                self.assertEqual(audit.main([str(root), "--report", str(saved)], audit=scanner, today=TODAY), 1)
            self.assertIn(f"FAIL {RUSTSEC} rustls", err.getvalue())
            saved.write_text("{", encoding="utf-8")
            with contextlib.redirect_stderr(io.StringIO()) as err:
                self.assertEqual(audit.main([str(root), "--report", str(saved)], audit=scanner, today=TODAY), 1)
            self.assertIn("did not return a report", err.getvalue())


if __name__ == "__main__":
    unittest.main()
