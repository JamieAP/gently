#!/usr/bin/env python3
"""Fail-closed RustSec advisory gate for Cargo.lock with reviewed, expiring exceptions.

Runs `cargo audit --json` against the committed lockfile and fails on every
finding: vulnerabilities and every warning kind (unmaintained, unsound, yanked,
notices). A finding passes only when the repository's audit-allowlist.json
records its RustSec ID and package with a justification and an unexpired review
date. Database fetch errors, unreadable or inconsistent reports, malformed or
expired entries, and findings without a RustSec ID (such as yanked crates) fail.
"""

import argparse
from datetime import date, datetime, timezone
import json
from pathlib import Path
import re
import subprocess
import sys

ALLOWLIST = "audit-allowlist.json"
LOCKFILE = "Cargo.lock"
ENTRY_KEYS = {"id", "package", "justification", "expires"}
RUSTSEC = re.compile(r"RUSTSEC-\d{4}-\d{4}")
MAX_REVIEW_DAYS = 90
MIN_JUSTIFICATION = 20


class AuditFailure(Exception):
    pass


def parse_allowlist(text, today):
    """Return ({(id, package): entry}, [problems]) for one allowlist document."""
    try:
        document = json.loads(text)
    except ValueError:
        return {}, ["allowlist is not valid JSON"]
    if not isinstance(document, dict) or set(document) != {"advisories"} or not isinstance(document["advisories"], list):
        return {}, ['allowlist must be an object with only an "advisories" list']
    entries, problems = {}, []
    for index, entry in enumerate(document["advisories"]):
        where = f"allowlist entry {index}"
        if not isinstance(entry, dict) or set(entry) != ENTRY_KEYS:
            problems.append(f"{where} must have exactly: {', '.join(sorted(ENTRY_KEYS))}")
            continue
        if not isinstance(entry["id"], str) or not RUSTSEC.fullmatch(entry["id"]):
            problems.append(f"{where} id must be a RustSec advisory ID (RUSTSEC-YYYY-NNNN)")
            continue
        where = f"allowlist entry {entry['id']}"
        if not isinstance(entry["package"], str) or not entry["package"].strip():
            problems.append(f"{where} must name the affected crate")
            continue
        if not isinstance(entry["justification"], str) or len(entry["justification"].strip()) < MIN_JUSTIFICATION:
            problems.append(f"{where} needs a reviewed justification")
            continue
        try:
            expires = date.fromisoformat(entry["expires"]) if isinstance(entry["expires"], str) else None
        except ValueError:
            expires = None
        if expires is None:
            problems.append(f"{where} expires must be an ISO date (YYYY-MM-DD)")
            continue
        if expires < today:
            problems.append(f"{where} expired on {expires.isoformat()}; fix the dependency or review it again")
            continue
        if (expires - today).days > MAX_REVIEW_DAYS:
            problems.append(f"{where} expires more than {MAX_REVIEW_DAYS} days ahead; reviews must be renewed")
            continue
        key = (entry["id"], entry["package"])
        if key in entries:
            problems.append(f"{where} is listed twice for {entry['package']}")
            continue
        entries[key] = entry
    return entries, problems


def finding(kind, item):
    """Normalize one cargo-audit vulnerability or warning entry."""
    if not isinstance(item, dict) or not isinstance(item.get("package"), dict):
        raise AuditFailure(f"cargo audit returned an unreadable {kind} entry")
    package = item["package"]
    advisory = item.get("advisory")
    if advisory is not None and not isinstance(advisory, dict):
        raise AuditFailure(f"cargo audit returned an unreadable {kind} advisory")
    advisory = advisory or {}
    ident = advisory.get("id") if isinstance(advisory.get("id"), str) else None
    return {
        "id": ident if ident and RUSTSEC.fullmatch(ident) else None,
        "kind": kind,
        "package": package.get("name") if isinstance(package.get("name"), str) else "?",
        "version": package.get("version") if isinstance(package.get("version"), str) else "?",
        "title": advisory.get("title").strip() if isinstance(advisory.get("title"), str) else "",
        "url": advisory.get("url") if isinstance(advisory.get("url"), str) else "",
    }


def findings(report):
    """Return (findings, summary) from a `cargo audit --json` report."""
    if not isinstance(report, dict):
        raise AuditFailure("cargo audit did not return a report (advisory database unavailable?)")
    database, lockfile = report.get("database"), report.get("lockfile")
    vulnerabilities, warnings = report.get("vulnerabilities"), report.get("warnings")
    if (not isinstance(database, dict) or type(database.get("advisory-count")) is not int
            or not isinstance(lockfile, dict) or type(lockfile.get("dependency-count")) is not int
            or not isinstance(vulnerabilities, dict) or not isinstance(vulnerabilities.get("list"), list)
            or type(vulnerabilities.get("count")) is not int or type(vulnerabilities.get("found")) is not bool
            or not isinstance(warnings, dict) or any(not isinstance(v, list) for v in warnings.values())):
        raise AuditFailure("cargo audit returned an unsupported report format")
    if (vulnerabilities["count"] != len(vulnerabilities["list"])
            or vulnerabilities["found"] != bool(vulnerabilities["list"])):
        raise AuditFailure("cargo audit totals do not match its vulnerabilities")
    found = [finding("vulnerability", item) for item in vulnerabilities["list"]]
    for kind in sorted(warnings):
        found.extend(finding(kind, item) for item in warnings[kind])
    commit = database.get("last-commit") if isinstance(database.get("last-commit"), str) else "unknown"
    summary = {
        "advisories": database["advisory-count"],
        "database": commit[:12],
        "updated": database.get("last-updated") if isinstance(database.get("last-updated"), str) else "unknown",
        "dependencies": lockfile["dependency-count"],
    }
    return sorted(found, key=lambda f: (f["kind"] != "vulnerability", f["package"], f["id"] or "")), summary


def evaluate(report, allowlist_text, today):
    """Return (passed, lines) without performing any I/O."""
    entries, problems = parse_allowlist(allowlist_text, today)
    lines = [f"{LOCKFILE}: {problem}" for problem in problems]
    try:
        found, summary = findings(report)
    except AuditFailure as failure:
        return False, lines + [f"{LOCKFILE}: {failure}"]
    failing, allowed = [], []
    for item in found:
        entry = entries.get((item["id"], item["package"])) if item["id"] else None
        (allowed if entry is not None else failing).append((item, entry))
    kinds = {}
    for item in found:
        kinds[item["kind"]] = kinds.get(item["kind"], 0) + 1
    breakdown = ", ".join(f"{kind} {count}" for kind, count in sorted(kinds.items())) or "none"
    lines.insert(0, f"{LOCKFILE}: {len(found)} findings ({breakdown}) in {summary['dependencies']} locked crates "
                    f"against {summary['advisories']} RustSec advisories (database {summary['database']}, "
                    f"updated {summary['updated']}); {len(failing)} failing, {len(allowed)} reviewed exceptions")
    for item, _ in failing:
        lines.append(f"{LOCKFILE}: FAIL {item['id'] or 'unidentified finding'} {item['package']} {item['version']} "
                     f"({item['kind']}) {item['title']} {item['url']}".rstrip())
    for item, entry in allowed:
        lines.append(f"{LOCKFILE}: reviewed {item['id']} {item['package']} {item['version']} ({item['kind']}) "
                     f"until {entry['expires']} (justification in {ALLOWLIST})")
    used = {(item["id"], item["package"]) for item in found}
    for key in sorted(set(entries) - used):
        lines.append(f"{LOCKFILE}: notice: allowlist entry {key[0]} for {key[1]} no longer matches; remove it")
    return not failing and not problems, lines


def cargo_audit(root, executable):
    try:
        result = subprocess.run([executable, "audit", "--json", "--file", str(root / LOCKFILE)], cwd=root,
                                capture_output=True, text=True, check=False)
    except OSError:
        return None
    try:
        return json.loads(result.stdout)
    except ValueError:
        return None


def main(argv=None, audit=cargo_audit, today=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("root", nargs="?", default=".", type=Path, help="Cargo workspace root (default: .)")
    parser.add_argument("--cargo-audit", default="cargo-audit", help="cargo-audit executable (default: cargo-audit)")
    parser.add_argument("--report", type=Path,
                        help="evaluate a saved `cargo audit --json` report instead of fetching the advisory database")
    args = parser.parse_args(argv)
    today = today or datetime.now(timezone.utc).date()
    try:
        allowlist = (args.root / ALLOWLIST).read_text(encoding="utf-8")
    except OSError:
        allowlist = None
    if allowlist is None:
        passed, lines = False, [f"{LOCKFILE}: {ALLOWLIST} is missing; an empty list is {{\"advisories\": []}}"]
    else:
        if args.report is not None:
            try:
                report = json.loads(args.report.read_text(encoding="utf-8"))
            except (OSError, ValueError):
                report = None
        else:
            report = audit(args.root, args.cargo_audit)
        passed, lines = evaluate(report, allowlist, today)
    for line in lines:
        print(line, file=sys.stdout if passed else sys.stderr)
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
