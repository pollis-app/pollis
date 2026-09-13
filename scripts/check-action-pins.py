#!/usr/bin/env python3
"""check-action-pins.py — every third-party action must be pinned to a full commit SHA.

A `uses: owner/action@v4` line hands whoever controls that tag the ability to run
arbitrary code in our workflows, with our secrets, on our runners — and a tag is a
mutable pointer. That is not hypothetical: the tj-actions/changed-files compromise
(CVE-2025-30066, March 2025) retagged every version tag of an action used by tens of
thousands of repositories to point at a commit that dumped runner memory, and the
secrets it exfiltrated were the release credentials of whoever ran it. The release
workflows here hold Apple/Windows signing identities, the R2 key that writes
`cdn.pollis.com`, and the transparency log's append credential.

A 40-hex commit SHA is content-addressed: the tag owner cannot move it, and GitHub
refuses a SHA that is not reachable from the action's repository. So this refuses
anything else. The shape enforced on every `uses:` in `.github/workflows/*` and in
every composite action under `.github/actions/`:

    uses: owner/repo[/path]@<40-hex sha>   # <the tag or branch this SHA was resolved from>

  * the ref must be a full SHA — a tag, a branch or a short SHA is a failure;
  * the trailing comment is mandatory, because a bare SHA tells a reviewer nothing
    and it is what dependabot's `github-actions` ecosystem rewrites when it bumps
    the pin, so the two stay in step;
  * `./path` (an action in this repository) is exempt — it ships in the same commit
    as the workflow that calls it and is reviewed the same way;
  * `docker://image` must carry an `@sha256:` digest for the same reason a tag does
    not count.

The other half of pinning is bumping it. A pin nobody moves leaves the workflows on
a known-vulnerable action forever, and the tempting fix for that is to unpin — so
this also asserts `.github/dependabot.yml` still claims the `github-actions`
ecosystem. Dependabot rewrites the SHA and its trailing comment together, which is
why the comment is required above.

Exit 0 = every action reference is immutable and something is bumping the pins.
Exit 1 = a report of each mutable reference, by file and line.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS = ROOT / ".github" / "workflows"
ACTIONS = ROOT / ".github" / "actions"
DEPENDABOT = ROOT / ".github" / "dependabot.yml"

# `- uses: X` or `uses: X`, with an optional quoted value and optional trailing comment.
USES = re.compile(r"""^\s*(?:-\s+)?uses:\s*(['"]?)(?P<ref>[^\s'"#]+)\1\s*(?P<comment>#.*)?$""")
FULL_SHA = re.compile(r"^[0-9a-f]{40}$")
DOCKER_DIGEST = re.compile(r"@sha256:[0-9a-f]{64}$")


def action_files() -> list[Path]:
    files = sorted(WORKFLOWS.glob("*.yml")) + sorted(WORKFLOWS.glob("*.yaml"))
    if ACTIONS.is_dir():
        files += sorted(ACTIONS.rglob("action.yml")) + sorted(ACTIONS.rglob("action.yaml"))
    return files


def check_line(ref: str, comment: str | None) -> str | None:
    """None if the reference is immutable, else the reason it is not."""
    if ref.startswith("./"):
        return None
    if ref.startswith("docker://"):
        if DOCKER_DIGEST.search(ref):
            return None
        return "docker image without an @sha256: digest"
    if "@" not in ref:
        return "no ref at all — resolves to the default branch"
    _, version = ref.rsplit("@", 1)
    if not FULL_SHA.match(version):
        return f"`@{version}` is a mutable tag/branch (or a short SHA), not a full commit SHA"
    if not comment or not comment.lstrip("#").strip():
        return "SHA-pinned but missing the `# <version>` comment dependabot keeps in step"
    return None


def dependabot_failures() -> list[str]:
    """The pins have to be bumped by something, or pinning turns into rotting."""
    if not DEPENDABOT.is_file():
        return [
            ".github/dependabot.yml is missing — nothing bumps the SHA pins, so they "
            "will sit on whatever version was current the day they were written"
        ]
    try:
        doc = yaml.safe_load(DEPENDABOT.read_text())
    except yaml.YAMLError as e:
        return [f".github/dependabot.yml does not parse — GitHub would ignore it: {e}"]
    if not isinstance(doc, dict):
        return [".github/dependabot.yml: top level is not a mapping"]
    updates = doc.get("updates") or []
    ecosystems = {
        u.get("package-ecosystem") for u in updates if isinstance(u, dict)
    }
    if "github-actions" not in ecosystems:
        return [
            ".github/dependabot.yml has no `github-actions` update entry — the action "
            "pins would never be bumped"
        ]
    return []


def main() -> int:
    failures: list[str] = dependabot_failures()
    files = action_files()
    if not files:
        print(f"no workflows found under {WORKFLOWS}", file=sys.stderr)
        return 1

    checked = 0
    for f in files:
        rel = f.relative_to(ROOT)
        for lineno, line in enumerate(f.read_text().splitlines(), start=1):
            m = USES.match(line)
            if not m:
                continue
            checked += 1
            reason = check_line(m.group("ref"), m.group("comment"))
            if reason:
                failures.append(f"{rel}:{lineno}: `{m.group('ref')}` — {reason}")

    if failures:
        print("MUTABLE ACTION REFERENCES (a retagged action runs with our secrets):\n", file=sys.stderr)
        for msg in failures:
            print(f"  ✗ {msg}", file=sys.stderr)
        print(
            f"\n{len(failures)} problem(s). Pin to the full commit SHA of the release you mean "
            f"(`git ls-remote --tags https://github.com/<owner>/<repo>` shows the peeled commit) "
            f"and keep the version as a trailing `# vX.Y.Z` comment.",
            file=sys.stderr,
        )
        return 1

    print(f"Action pins OK — {checked} `uses:` reference(s) across {len(files)} file(s) are immutable.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
