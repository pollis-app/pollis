"""Regenerate the import pages' preview fixtures from real `pollis-import` output.

    python3 e2e/fixtures/gen_import_previews.py      (from the repo root)

Each fixture is what `import_preview` returns for a new group: the plan and
invite sheet of a `pollis-import/tests/fixtures` export, with the fixture's
location replaced by a plausible user path. The Slack one carries a second
plan with archived channels included (`preview_archived`).
"""

import json
import os
import subprocess
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
FIXTURES = os.path.join(ROOT, "pollis-import", "tests", "fixtures")


def cli(*args):
    out = subprocess.run(
        ["cargo", "run", "-q", "-p", "pollis-import", "--", "plan", *args],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return out


def relocate(text, real, shown):
    return text.replace(real, shown)


def preview(source, export, shown, extra=()):
    real = os.path.join(FIXTURES, export)
    flag = ["--slack"] if source == "slack" else []
    plan = json.loads(relocate(cli(*flag, real, "--json", *extra), real, shown))
    sheet = cli(*flag, real, "--sheet", *extra)
    return {
        "source": source,
        "path": shown,
        "plan": plan,
        "invite_sheet": sheet,
        "target": None,
        "candidates": [],
        "include_archived": "--include-archived" in extra,
    }


def write(name, data):
    path = os.path.join(ROOT, "e2e", "fixtures", name)
    with open(path, "w") as f:
        json.dump(data, f, indent=1, ensure_ascii=False)
        f.write("\n")
    print(f"wrote {path}", file=sys.stderr)


write("discord-import-preview.json", preview("discord", "dragons-den", "/Users/alice/Exports/dragons-den"))
slack_path = "/Users/alice/Downloads/Northwind Studio Slack export"
slack = preview("slack", "northwind", slack_path)
slack["preview_archived"] = preview("slack", "northwind", slack_path, ["--include-archived"])
write("slack-import-preview.json", slack)
