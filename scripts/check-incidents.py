#!/usr/bin/env python3
"""check-incidents.py — keep the public incident record honest (#877).

`website/incidents.json` is rendered on pollis.com/status and is the only place a
user can find out whether something went wrong. A record like that fails in two
directions, and both are worse than having no page at all:

  * malformed — the page silently renders nothing, so an incident that WAS
    written down is published as "no incidents";
  * overdue — an incident closes and the postmortem it owes never appears, so
    the page shows a resolved outage with no explanation and the deadline in
    docs/incidents/README.md §3 quietly becomes a suggestion.

This asserts both, offline, on every PR that touches the record, the schema or
the scripts around them. Exit 0 = the record is publishable.
"""

from __future__ import annotations

import json
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# The record under test. Overridable by a single positional argument so
# `test-incidents-check.sh` can point it at a deliberately broken copy — a
# validator nobody has watched go red is a validator nobody should cite.
INCIDENTS = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "website" / "incidents.json"
HISTORY = ROOT / "website" / "status-history.json"
INCIDENT_DIR = ROOT / "docs" / "incidents"

SEVERITIES = {"sev1", "sev2", "sev3", "sev4"}
STATUSES = {"investigating", "identified", "monitoring", "resolved"}
REQUIRED = [
    "id",
    "title",
    "severity",
    "status",
    "correctness_impact",
    "components",
    "started_at",
    "resolved_at",
    "user_impact",
    "summary",
    "postmortem",
]
# docs/incidents/README.md §3. A postmortem is owed for anything a user lost
# something to, and the clock is enforced here rather than remembered.
POSTMORTEM_DEADLINE_DAYS = 14

failures: list[str] = []


def fail(msg: str) -> None:
    failures.append(msg)


def parse_ts(value: str, where: str) -> datetime | None:
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except (ValueError, AttributeError):
        fail(f"{where}: {value!r} is not an RFC 3339 timestamp")
        return None


def main() -> int:
    try:
        record = json.loads(INCIDENTS.read_text())
    except FileNotFoundError:
        print(f"error: {INCIDENTS} is missing — /status renders it", file=sys.stderr)
        return 1
    except json.JSONDecodeError as e:
        print(f"error: {INCIDENTS} is not valid JSON — the page would render nothing: {e}", file=sys.stderr)
        return 1

    if not record.get("record_started_at"):
        fail("incidents.json has no record_started_at — an empty list would read as "
             "'nothing has ever gone wrong' rather than 'the record starts here'")

    # Components must name a real probed target, or the status page draws an
    # incident against a service it does not show.
    known_components: set[str] = set()
    if HISTORY.exists():
        try:
            known_components = {t["id"] for t in json.loads(HISTORY.read_text()).get("targets", [])}
        except (json.JSONDecodeError, KeyError, TypeError):
            fail("status-history.json is unreadable, so component ids cannot be checked")

    incidents = record.get("incidents")
    if not isinstance(incidents, list):
        print("error: incidents.json has no `incidents` array", file=sys.stderr)
        return 1

    now = datetime.now(timezone.utc)
    seen: set[str] = set()

    for i, inc in enumerate(incidents):
        where = f"incident[{i}]"
        if not isinstance(inc, dict):
            fail(f"{where}: not an object")
            continue
        where = f"incident {inc.get('id', f'[{i}]')!r}"

        for field in REQUIRED:
            if field not in inc:
                fail(f"{where}: missing required field `{field}` (docs/incidents/README.md §5)")
        if any(f not in inc for f in REQUIRED):
            continue

        if inc["id"] in seen:
            fail(f"{where}: duplicate id")
        seen.add(inc["id"])

        parts = str(inc["id"]).split("-", 3)
        if len(parts) < 4 or not parse_ts(f"{'-'.join(parts[:3])}T00:00:00Z", f"{where} id"):
            fail(f"{where}: id must be YYYY-MM-DD-slug")

        if inc["severity"] not in SEVERITIES:
            fail(f"{where}: severity {inc['severity']!r} is not one of {sorted(SEVERITIES)}")
        if inc["status"] not in STATUSES:
            fail(f"{where}: status {inc['status']!r} is not one of {sorted(STATUSES)}")
        if not isinstance(inc["correctness_impact"], bool):
            fail(f"{where}: correctness_impact must be true or false — README §2 makes it a "
                 "question every incident answers, so it may not be omitted or left null")

        if not isinstance(inc["components"], list) or not inc["components"]:
            fail(f"{where}: components must be a non-empty list")
        elif known_components:
            unknown = [c for c in inc["components"] if c not in known_components]
            if unknown:
                fail(f"{where}: components {unknown} are not probed targets "
                     f"({sorted(known_components)}) — the status page cannot draw them")

        started = parse_ts(inc["started_at"], f"{where} started_at")
        resolved = None
        if inc["resolved_at"] is not None:
            resolved = parse_ts(inc["resolved_at"], f"{where} resolved_at")
            if started and resolved and resolved < started:
                fail(f"{where}: resolved_at is before started_at")

        if inc["status"] == "resolved" and inc["resolved_at"] is None:
            fail(f"{where}: status is resolved but resolved_at is null")
        if inc["status"] != "resolved" and inc["resolved_at"] is not None:
            fail(f"{where}: resolved_at is set but status is {inc['status']!r}")

        for field in ("title", "user_impact", "summary"):
            if not str(inc[field] or "").strip():
                fail(f"{where}: `{field}` is empty — a blank entry publishes an outage with no account of it")

        owes = inc["severity"] in {"sev1", "sev2"} or inc["correctness_impact"]
        pm = inc["postmortem"]
        if pm is not None:
            if not (ROOT / pm).exists():
                fail(f"{where}: postmortem {pm!r} does not exist")
            elif not str(pm).startswith("docs/incidents/"):
                fail(f"{where}: postmortem must live under docs/incidents/")
        elif owes and resolved is not None and now - resolved > timedelta(days=POSTMORTEM_DEADLINE_DAYS):
            overdue = (now - resolved).days - POSTMORTEM_DEADLINE_DAYS
            fail(f"{where}: a {inc['severity']}"
                 f"{' with correctness impact' if inc['correctness_impact'] else ''} resolved "
                 f"{(now - resolved).days} days ago still has no postmortem — "
                 f"{overdue} day(s) past the {POSTMORTEM_DEADLINE_DAYS}-day deadline "
                 f"(docs/incidents/README.md §3). Copy docs/incidents/TEMPLATE.md.")

    # The format is only enforceable while the documents describing it exist.
    for required_doc in (INCIDENT_DIR / "README.md", INCIDENT_DIR / "TEMPLATE.md"):
        if not required_doc.exists():
            fail(f"{required_doc.relative_to(ROOT)} is missing — the record has no written format")

    if failures:
        print("PUBLIC INCIDENT RECORD IS NOT PUBLISHABLE:\n", file=sys.stderr)
        for msg in failures:
            print(f"  ✗ {msg}", file=sys.stderr)
        print(f"\n{len(failures)} problem(s).", file=sys.stderr)
        return 1

    print(f"Incident record OK — {len(incidents)} incident(s), record open since "
          f"{record.get('record_started_at')}.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
