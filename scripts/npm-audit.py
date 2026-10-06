#!/usr/bin/env python3
"""Fail-closed npm advisory gate with reviewed, expiring exceptions.

`npm audit` has no ignore list. This wrapper audits each project's committed
lockfile and fails on every advisory at or above the audit level unless that
project's audit-allowlist.json records it with a justification and an unexpired
review date. Registry errors, unreadable reports, malformed or expired entries
and advisories without a GitHub advisory ID fail too.
"""

import argparse
from datetime import date, datetime, timezone
import json
from pathlib import Path
import re
import subprocess
import sys

LEVELS = ("info", "low", "moderate", "high", "critical")
ALLOWLIST = "audit-allowlist.json"
ENTRY_KEYS = {"id", "package", "justification", "expires"}
GHSA = re.compile(r"GHSA(?:-[23456789cfghjmpqrvwx]{4}){3}")
MAX_REVIEW_DAYS = 90
MIN_JUSTIFICATION = 20


class AuditFailure(Exception):
    pass


def rank(severity):
    # An unknown severity is treated as the most severe: fail closed.
    return LEVELS.index(severity) if severity in LEVELS else len(LEVELS)


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
        if not isinstance(entry["id"], str) or not GHSA.fullmatch(entry["id"]):
            problems.append(f"{where} id must be a GitHub advisory ID (GHSA-xxxx-xxxx-xxxx); npm reports no CVE IDs")
            continue
        where = f"allowlist entry {entry['id']}"
        if not isinstance(entry["package"], str) or not entry["package"].strip():
            problems.append(f"{where} must name the affected package")
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


def advisories(report):
    """Return the distinct advisories in an `npm audit --json` (v2) report."""
    if not isinstance(report, dict):
        raise AuditFailure("npm audit did not return a report")
    if "error" in report:
        raise AuditFailure("npm audit returned an error (registry or audit endpoint unavailable?)")
    vulnerabilities = report.get("vulnerabilities")
    counts = (report.get("metadata") or {}).get("vulnerabilities")
    if report.get("auditReportVersion") != 2 or not isinstance(vulnerabilities, dict) or not isinstance(counts, dict):
        raise AuditFailure("npm audit returned an unsupported report format")
    found = {}
    for package, vulnerability in vulnerabilities.items():
        via = vulnerability.get("via") if isinstance(vulnerability, dict) else None
        if not isinstance(via, list):
            raise AuditFailure(f"npm audit returned an unreadable entry for {package}")
        for source in via:
            if isinstance(source, str):
                continue  # A dependency path; the advisory is listed under that package.
            if not isinstance(source, dict):
                raise AuditFailure(f"npm audit returned an unreadable advisory for {package}")
            url = source.get("url") if isinstance(source.get("url"), str) else ""
            match = GHSA.fullmatch(url.rstrip("/").rsplit("/", 1)[-1])
            advisory = {
                "id": match.group(0) if match else None,
                "package": source.get("name") if isinstance(source.get("name"), str) else package,
                "severity": source.get("severity"),
                "title": source.get("title") if isinstance(source.get("title"), str) else "",
                "url": url,
            }
            found.setdefault((advisory["id"] or url or str(source.get("source")), advisory["package"]), advisory)
    total = counts.get("total")
    if not isinstance(total, int) or (total > 0) != bool(found):
        raise AuditFailure("npm audit totals do not match its advisories")
    return sorted(found.values(), key=lambda a: (-rank(a["severity"]), a["package"], a["id"] or ""))


def evaluate(name, report, allowlist_text, level, today):
    """Return (passed, lines) for one project without performing any I/O."""
    entries, problems = parse_allowlist(allowlist_text, today)
    lines = [f"{name}: {problem}" for problem in problems]
    try:
        found = advisories(report)
    except AuditFailure as failure:
        return False, lines + [f"{name}: {failure}"]
    counts = report["metadata"]["vulnerabilities"]
    dependencies = (report["metadata"].get("dependencies") or {}).get("total", "?")
    failing, allowed, below = [], [], []
    for advisory in found:
        entry = entries.get((advisory["id"], advisory["package"]))
        if rank(advisory["severity"]) < rank(level):
            below.append(advisory)
        elif entry is not None:
            allowed.append((advisory, entry))
        else:
            failing.append(advisory)
    used = {(advisory["id"], advisory["package"]) for advisory in found}
    severities = ", ".join(f"{severity} {counts.get(severity, 0)}" for severity in LEVELS)
    lines.insert(0, f"{name}: {len(found)} advisories across {counts.get('total', 0)} vulnerable packages "
                    f"({severities}) in {dependencies} locked dependencies; {len(failing)} failing at "
                    f"--audit-level={level}, {len(allowed)} reviewed exceptions, {len(below)} below level")
    for advisory in failing:
        lines.append(f"{name}: FAIL {advisory['id'] or 'unidentified advisory'} {advisory['package']} "
                     f"({advisory['severity']}) {advisory['title']} {advisory['url']}".rstrip())
    for advisory, entry in allowed:
        lines.append(f"{name}: reviewed {advisory['id']} {advisory['package']} ({advisory['severity']}) "
                     f"until {entry['expires']} (justification in {ALLOWLIST})")
    for advisory in below:
        lines.append(f"{name}: below level {advisory['id'] or 'unidentified advisory'} {advisory['package']} "
                     f"({advisory['severity']})")
    for key in sorted(set(entries) - used):
        lines.append(f"{name}: notice: allowlist entry {key[0]} for {key[1]} no longer matches; remove it")
    return not failing and not problems, lines


def npm_audit(project):
    result = subprocess.run(["npm", "audit", "--json", "--package-lock-only"], cwd=project,
                            capture_output=True, text=True, check=False)
    try:
        return json.loads(result.stdout)
    except ValueError:
        return None


def main(argv=None, audit=npm_audit, today=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("projects", nargs="*", default=["."], help="npm project directories (default: .)")
    parser.add_argument("--audit-level", choices=LEVELS, default="low")
    parser.add_argument("--report", type=Path,
                        help="evaluate a saved `npm audit --json` report for one project instead of querying the registry")
    args = parser.parse_args(argv)
    if args.report is not None and len(args.projects) != 1:
        parser.error("--report applies to exactly one project")
    today = today or datetime.now(timezone.utc).date()
    passed = True
    for project in map(Path, args.projects):
        name = project.resolve().name
        try:
            allowlist = (project / ALLOWLIST).read_text(encoding="utf-8")
        except OSError:
            allowlist = None
        if allowlist is None:
            ok, lines = False, [f"{name}: {ALLOWLIST} is missing; an empty list is {{\"advisories\": []}}"]
        else:
            if args.report is not None:
                try:
                    report = json.loads(args.report.read_text(encoding="utf-8"))
                except (OSError, ValueError):
                    report = None
            else:
                report = audit(project)
            ok, lines = evaluate(name, report, allowlist, args.audit_level, today)
        for line in lines:
            print(line, file=sys.stdout if ok else sys.stderr)
        passed = passed and ok
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
