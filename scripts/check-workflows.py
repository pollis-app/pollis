#!/usr/bin/env python3
"""check-workflows.py — catch workflow files GitHub cannot parse.

An unparseable workflow does not announce itself. GitHub creates a run with ZERO jobs
and a `failure` conclusion, lists the workflow under its file path instead of its
`name:`, and every trigger it declares silently stops working. Nothing in the UI says
"this file is invalid" where anyone would look.

That happened here: a second top-level `env:` block was added to rebuild-verify.yml, so
the workflow stopped parsing and the auto-verify trigger it had just gained never fired.
The pre-merge check missed it because `yaml.safe_load` accepts duplicate keys under
last-one-wins, while GitHub rejects the document. A validation that is more permissive
than the thing it validates provides false confidence, which is worse than no check.

So this parses every workflow the way GitHub does — duplicates are an error — and
asserts the structural bits that make a workflow do anything at all.

Exit 0 = every workflow parses and declares a name and at least one trigger.
Exit 1 = a report of what GitHub would reject.
"""

from __future__ import annotations

import sys
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS = ROOT / ".github" / "workflows"

failures: list[str] = []


class StrictLoader(yaml.SafeLoader):
    """SafeLoader that refuses duplicate mapping keys, as GitHub's parser does."""


def _no_duplicate_keys(loader, node, deep=False):
    seen = set()
    for key_node, _ in node.value:
        key = loader.construct_object(key_node, deep=deep)
        if key in seen:
            raise yaml.constructor.ConstructorError(
                None, None, f"duplicate key {key!r}", key_node.start_mark
            )
        seen.add(key)
    return yaml.constructor.SafeConstructor.construct_mapping(loader, node, deep)


StrictLoader.add_constructor(
    yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, _no_duplicate_keys
)


def main() -> int:
    files = sorted(WORKFLOWS.glob("*.yml")) + sorted(WORKFLOWS.glob("*.yaml"))
    if not files:
        print(f"no workflows found under {WORKFLOWS}", file=sys.stderr)
        return 1

    for f in files:
        try:
            doc = yaml.load(f.read_text(), Loader=StrictLoader)
        except yaml.YAMLError as e:
            failures.append(f"{f.name}: GitHub would REJECT this file — {e}")
            continue

        if not isinstance(doc, dict):
            failures.append(f"{f.name}: top level is not a mapping")
            continue

        # A missing `name:` is what makes a broken workflow show up as a bare path.
        if not doc.get("name"):
            failures.append(f"{f.name}: no top-level `name:` — it will list under its file path")

        # PyYAML parses the bare `on:` key as the boolean True.
        triggers = doc.get("on", doc.get(True))
        if not triggers:
            failures.append(f"{f.name}: no `on:` triggers — this workflow can never run")

        if "jobs" not in doc or not doc["jobs"]:
            failures.append(f"{f.name}: no `jobs:` — nothing would execute")

    if failures:
        print("WORKFLOW FILES GITHUB CANNOT USE:\n", file=sys.stderr)
        for msg in failures:
            print(f"  ✗ {msg}", file=sys.stderr)
        print(
            f"\n{len(failures)} problem(s). An invalid workflow fails silently — a run "
            f"with zero jobs — so this must be caught here.",
            file=sys.stderr,
        )
        return 1

    print(f"Workflows OK — {len(files)} file(s) parse strictly, all name a trigger and jobs.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
