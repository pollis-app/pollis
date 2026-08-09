#!/usr/bin/env python3
"""check-build-recipe.py — keep the reproducer's recipe equal to what the release bakes.

A Linux release is only bit-reproducible by a third party if the rebuilder compiles with
EXACTLY the values the release compiled with. Every one of those values reaches the binary
through `option_env!` in `pollis-core/src/config.rs`, which means an environment variable
present at release time and absent at rebuild time silently changes the bytes — and the
rebuild fails with a hash mismatch that looks like tampering.

That is not hypothetical. `POLLIS_OVERLAY_DIRECTORY_URL` and `POLLIS_OVERLAY_DIRECTORY_KEY`
were added to the release build and never to the rebuilder's recipe, so no rebuild could
have matched while the relay directory was live. Both of the rebuilder's runs failed on
exactly this, and the docs went on describing independent Linux reproduction as a shipped,
working property.

Three lists have to agree, and all three are hand-maintained in different files:

    1. `option_env!(...)` in pollis-core/src/config.rs    what the binary can absorb
    2. the release build's exports in desktop-release.yml what a release actually bakes
    3. the recipe list in rebuild-verify.yml              what a reproducer compiles with

The invariant this enforces is (2) ⊆ (3) ⊆ (1):

  * anything the RELEASE bakes must be in the rebuilder's recipe, or reproduction is
    impossible — this is the failure above;
  * anything in the recipe must actually be read by the client, or the recipe is
    advertising an input that does nothing and misleads whoever audits it.

Note (3) ⊄ (2) is fine and expected: the recipe may list a var the release leaves unset
(e.g. an optional log-DB token), because the rebuilder skips unset vars so the compile sees
`None` either way.

Exit 0 = a third party given the published recipe compiles the same inputs we did.
Exit 1 = a report of the drift.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CONFIG = ROOT / "pollis-core" / "src" / "config.rs"
RELEASE = ROOT / ".github" / "workflows" / "desktop-release.yml"
REBUILD = ROOT / ".github" / "workflows" / "rebuild-verify.yml"

# The reproducible unit is the Linux AppImage, so only that job's recipe matters.
RELEASE_JOB = "build-linux"

failures: list[str] = []


def fail(msg: str) -> None:
    failures.append(msg)


def baked_by_client() -> set[str]:
    """(1) Every env var the client absorbs at compile time."""
    return set(re.findall(r'option_env!\("([A-Z_0-9]+)"\)', CONFIG.read_text()))


def job_block(text: str, job: str) -> str:
    """The YAML body of one job, from its header to the next top-level job."""
    m = re.search(rf"^  {re.escape(job)}:$(.*?)(?=^  [a-z][a-z0-9_-]*:$)", text, re.S | re.M)
    if not m:
        fail(f"{RELEASE.name}: could not locate the `{job}` job to read its build recipe")
        return ""
    return m.group(1)


def baked_by_release() -> set[str]:
    """(2) What the Linux release job writes into the build environment.

    Matches the `echo "KEY=${{ secrets.X }}" >> $GITHUB_ENV` form the release uses. Only
    keys the client actually reads are relevant, so the caller intersects with (1) — this
    job also exports plenty of things that never reach the binary (signing material,
    toolchain paths).
    """
    block = job_block(RELEASE.read_text(), RELEASE_JOB)
    return set(re.findall(r'echo\s+"([A-Z_0-9]+)=', block))


def rebuild_recipe() -> set[str]:
    """(3) The recipe the independent rebuilder compiles with.

    Read from the `for key in ... ; do` loop rather than the `env:` mapping: the loop is
    what actually reaches `$GITHUB_ENV`, and an entry present in one but not the other is
    precisely the kind of drift this script exists to catch. Both are checked against each
    other below.
    """
    text = REBUILD.read_text()
    m = re.search(r"for key in ([A-Z_0-9\s\\]+?);\s*do", text)
    if not m:
        fail(f"{REBUILD.name}: could not find the `for key in ... ; do` recipe loop")
        return set()
    return set(m.group(1).replace("\\", " ").split())


def rebuild_env_mapping() -> set[str]:
    """The `env:` keys on the rebuilder's recipe step — must equal the loop."""
    text = REBUILD.read_text()
    m = re.search(r"- name: Load build recipe.*?\n\s+env:\n(.*?)\n\s+run:", text, re.S)
    if not m:
        fail(f"{REBUILD.name}: could not find the recipe step's `env:` block")
        return set()
    return set(re.findall(r"^\s+([A-Z_0-9]+):", m.group(1), re.M))


# Jobs that produce the bit-reproducible Linux payload. Both must build cold.
REPRODUCIBLE_JOBS = ("build-linux", "build-capture-helper")


def cached_reproducible_jobs(text: str) -> list[str]:
    """Reproducible-payload jobs that restore a build cache.

    A restored `target/` lets cargo reuse artifacts compiled under earlier conditions,
    so the output stops being a pure function of the source. This is what made v1.8.3
    unreproducible, and it is invisible in a diff — the job still looks correct. It
    cannot be fixed by caching the reproducer too: a third party starts cold by
    definition, so a build that reproduces only from our cache reproduces for nobody.
    """
    hits = []
    for job in REPRODUCIBLE_JOBS:
        block = job_block(text, job)
        if "Swatinem/rust-cache" in block or "actions/cache" in block:
            hits.append(job)
    return hits


def main() -> int:
    client = baked_by_client()
    if not client:
        fail(f"{CONFIG.name}: found no `option_env!` keys at all — has the recipe moved?")

    release = baked_by_release() & client
    recipe = rebuild_recipe()
    mapping = rebuild_env_mapping()

    # (2) ⊆ (3) — the one that breaks reproduction.
    for k in sorted(release - recipe):
        fail(
            f"{k} is baked into the {RELEASE_JOB} release build but is NOT in the rebuilder's "
            f"recipe — an independent rebuild compiles without it and CANNOT reproduce the "
            f"shipped bytes. Add it to both the `env:` block and the `for key in` loop in "
            f"{REBUILD.name}, and publish its value as a repository variable."
        )

    # (3) ⊆ (1) — a recipe entry the client never reads is a misleading input.
    for k in sorted(recipe - client):
        fail(
            f"{REBUILD.name} passes {k}, which {CONFIG.name} never reads via option_env! — "
            f"it cannot affect the binary, so publishing it as part of the recipe misleads "
            f"anyone auditing the reproduction."
        )

    # No build cache on the reproducible path.
    for job in cached_reproducible_jobs(RELEASE.read_text()):
        fail(
            f"the `{job}` release job restores a build cache — a reproducible payload must "
            f"be built from a clean tree, or its bytes depend on our cache state and no "
            f"third party (who always starts cold) can reproduce them. Remove the cache step."
        )

    # The step's own two lists must agree, or the recipe silently drops an entry.
    for k in sorted(mapping - recipe):
        fail(f"{REBUILD.name}: {k} is in the recipe step's `env:` but missing from the `for key in` loop — it is never exported")
    for k in sorted(recipe - mapping):
        fail(f"{REBUILD.name}: {k} is in the `for key in` loop but has no `env:` entry — it is always empty")

    if failures:
        print(
            "BUILD RECIPE DRIFT — an independent rebuild of the Linux payload cannot "
            "reproduce the shipped bytes:\n",
            file=sys.stderr,
        )
        for f in failures:
            print(f"  ✗ {f}", file=sys.stderr)
        print(
            f"\n{len(failures)} problem(s). See docs/reproducible-builds-residuals.md.",
            file=sys.stderr,
        )
        return 1

    print(
        f"Build recipe OK — {len(release)} value(s) baked by the release are all in the "
        f"rebuilder's recipe ({len(recipe)} entries), and every recipe entry is read by the client."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
