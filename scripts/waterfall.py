#!/usr/bin/env python3
"""Render a trace as an ASCII waterfall and check its structural integrity.

Usage:
    gently trace <trace_id> --json | python3 scripts/waterfall.py

Reads the span array from stdin (the `gently trace --json` shape) and prints a
depth-indented, time-proportional waterfall followed by integrity checks:
session-root presence, parent-link resolution, non-negative durations, and
child-within-parent temporal nesting. These checks describe the supplied spans
and do not establish complete capture or actual completion.
"""

import json, sys
spans = json.load(sys.stdin)
if not spans:
    print("no spans"); sys.exit(1)
by_id = {s["span_id"]: s for s in spans}
def n(x): return int(x) if x not in (None, "") else None
# Prefer the collector-derived display end from available observations. This
# is not proof of complete capture or completion. Fall back to the stored end
# when the collector omits the derived field.
def eff_end(s): return n(s.get("effective_end_unix_nano")) or n(s.get("end_unix_nano"))
t0 = min(n(s["start_unix_nano"]) for s in spans)
tmax = max((eff_end(s) or n(s["start_unix_nano"])) for s in spans)
total = max(tmax - t0, 1)
def dur_ms(s):
    e = eff_end(s)
    return ((e - n(s["start_unix_nano"])) / 1e6) if e else 0.0
def off_ms(s): return (n(s["start_unix_nano"]) - t0) / 1e6
kids, roots = {}, []
for s in spans:
    p = s.get("parent_span_id")
    (kids.setdefault(p, []).append(s) if (p and p in by_id) else roots.append(s))
W = 56
def bar(s):
    o = round(off_ms(s) * 1e6 / total * W)
    d = max(1, round(dur_ms(s) * 1e6 / total * W))
    o = min(o, W - 1); d = min(d, W - o)
    return " " * o + "█" * d
STATUS = {0: " ", 1: "✓", 2: "✗"}
print(f"{'dur':>9} {'st':>1}  span{'':<22}│{'timeline →':<{W}}│")
print("─" * 9 + " ─  " + "─" * 26 + "┼" + "─" * W + "┤")
def walk(s, depth):
    label = ("  " * depth) + s["name"]
    print(f"{dur_ms(s):8.1f}m {STATUS.get(s['status'],'?')}  {label:<26.26}│{bar(s):<{W}}│")
    for c in sorted(kids.get(s["span_id"], []), key=lambda x: n(x["start_unix_nano"])):
        walk(c, depth + 1)
for r in sorted(roots, key=lambda x: n(x["start_unix_nano"])):
    walk(r, 0)

# ---- integrity ----
print("\nINTEGRITY")
nonroot = [s for s in spans if s.get("parent_span_id")]
resolved = [s for s in nonroot if s["parent_span_id"] in by_id]
dangling = [s for s in nonroot if s["parent_span_id"] not in by_id]
neg = [s for s in spans if n(s.get("end_unix_nano")) and n(s["end_unix_nano"]) < n(s["start_unix_nano"])]
EPS = 2_000_000  # 2ms slack for clock granularity between separate hook processes
violations = []
for s in spans:
    p = by_id.get(s.get("parent_span_id"))
    if not p: continue
    cs, ce = n(s["start_unix_nano"]), eff_end(s) or n(s["start_unix_nano"])
    ps, pe = n(p["start_unix_nano"]), eff_end(p) or n(p["start_unix_nano"])
    # An unclosed/provisional parent (zero- or negative-width: end <= start, e.g.
    # a Codex session root with no SessionEnd, or a crashed Claude session) has no
    # meaningful upper bound - only check the lower bound against it.
    unclosed = pe <= ps
    if cs < ps - EPS or (not unclosed and ce > pe + EPS):
        violations.append((s["name"], p["name"]))
root = [s for s in spans if s["name"] == "session" and not s.get("parent_span_id")]
def ok(b): return "PASS ✓" if b else "FAIL ✗"
print(f"  spans total                 : {len(spans)}")
print(f"  session-root present        : {ok(bool(root))} ({len(root)} root)")
print(f"  parent links resolved       : {ok(not dangling)} ({len(resolved)}/{len(nonroot)} non-root spans; {len(dangling)} dangling)")
print(f"  no negative durations       : {ok(not neg)}")
print(f"  children nest within parent : {ok(not violations)} ({len(violations)} violations)")
for v in violations[:5]: print(f"      ! {v[0]} not within {v[1]}")
