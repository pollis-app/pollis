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

A fourth list is enforced for a different reason — containment rather than
reproducibility. Whatever a BUILD job writes to `$GITHUB_ENV` becomes an ordinary
environment variable for every later step of that job: every crate's `build.rs` and
proc-macro, pnpm's lifecycle scripts, and every third-party action. Every `option_env!`
input is a public endpoint or verification key, so exporting the recipe is harmless; a
credential exported alongside it is one a poisoned dependency can read and exfiltrate.
The release build jobs did exactly that — the account-wide R2 write key and the LiveKit
API secret sat in every build job's environment with no consumer in the job. So:

    4. every `$GITHUB_ENV` export in a build job of desktop-release.yml or
       cli-release.yml must name either an `option_env!` key or one of the known
       toolchain flags (SOURCE_DATE_EPOCH, RUSTFLAGS, ...), and no build-job export
       may interpolate a `secrets.*` value under a name outside the recipe;
    5. in EVERY job of those two workflows, an export that interpolates `secrets.X`
       must have X in the recipe — credentials the release/publish jobs need reach
       the `aws s3` steps as step-scoped `env:`, never through `$GITHUB_ENV`.

Exit 0 = a third party given the published recipe compiles the same inputs we did, and
         no release build job carries a credential the binary cannot even absorb.
Exit 1 = a report of the drift.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CONFIG = ROOT / "pollis-core" / "src" / "config.rs"
RELEASE = ROOT / ".github" / "workflows" / "desktop-release.yml"
CLI_RELEASE = ROOT / ".github" / "workflows" / "cli-release.yml"
REBUILD = ROOT / ".github" / "workflows" / "rebuild-verify.yml"

# The reproducible unit is the Linux AppImage, so only that job's recipe matters.
RELEASE_JOB = "build-linux"

# Every job that compiles client code in the two release workflows — the jobs whose
# environment is visible to build scripts and dependencies. Listed explicitly (and
# cross-checked against every `build-*` job header below) so a new build job cannot
# appear without being placed under this check.
BUILD_JOBS: dict[Path, tuple[str, ...]] = {
    RELEASE: ("build-macos", "build-windows", "build-capture-helper", "build-linux"),
    CLI_RELEASE: ("build-cli-linux", "build-cli-macos", "build-cli-windows"),
}

# The only non-recipe names a build job may write to $GITHUB_ENV: the determinism
# flags (#504) and the Windows signtool paths resolved on the runner. None of them
# carries a secret — and check 5 refuses an export that smuggles one under these names.
TOOLCHAIN_EXPORTS = frozenset(
    {
        "SOURCE_DATE_EPOCH",
        "RUSTFLAGS",
        "CFLAGS",
        "CXXFLAGS",
        "SIGNTOOL_PATH",
        "SIGNING_DLIB_PATH",
        "SIGN_METADATA_PATH",
    }
)

# One `$GITHUB_ENV` write, wherever it appears: the exported name (or None when the
# line's shape is not one this script understands) and whether it interpolates a
# repository secret.
Export = tuple[str, str | None, bool]

failures: list[str] = []


def fail(msg: str) -> None:
    failures.append(msg)


def baked_by_client() -> set[str]:
    """(1) Every env var the client absorbs at compile time."""
    return set(re.findall(r'option_env!\("([A-Z_0-9]+)"\)', CONFIG.read_text()))


def job_block(text: str, job: str) -> str:
    """The YAML body of one job, from its header to the next top-level job.

    The terminator is "the next job header OR end of file" — without the latter the
    LAST job in a file never matches, which is silent and looks like the job is
    missing rather than like a parsing bug.
    """
    m = re.search(rf"^  {re.escape(job)}:$(.*?)(?=^  [a-z][a-z0-9_-]*:$|\Z)", text, re.S | re.M)
    if not m:
        fail(f"could not locate the `{job}` job to read its build recipe")
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


def github_env_exports(block: str) -> list[Export]:
    """Every `$GITHUB_ENV` write in a job body, as (line, exported name, uses a secret).

    Understands the two shapes the release workflows use — `echo "KEY=..." >> $GITHUB_ENV`
    (bash, quoted or not) and `"KEY=..." | Out-File -FilePath $env:GITHUB_ENV` (pwsh) —
    plus the heredoc opener `echo "KEY<<EOF"`. Comment lines are skipped. A line that
    mentions GITHUB_ENV in any other shape yields name=None so the caller fails it:
    an export this script cannot read is one it cannot vouch for.
    """
    out: list[Export] = []
    for raw in block.splitlines():
        line = raw.strip()
        if "GITHUB_ENV" not in line or line.startswith("#"):
            continue
        m = re.search(r'"?([A-Z][A-Z0-9_]*)(?:=|<<)', line)
        out.append((line, m.group(1) if m else None, "secrets." in line))
    return out


def job_headers(text: str) -> list[str]:
    """Every top-level job id in a workflow file."""
    return re.findall(r"^  ([a-z][a-z0-9_-]*):$", text, re.M)


def check_build_job_exports(client: set[str]) -> None:
    """(4) + (5): no credential in a build job's environment, none via $GITHUB_ENV anywhere."""
    allowed = client | TOOLCHAIN_EXPORTS
    for path, jobs in BUILD_JOBS.items():
        text = path.read_text()

        # A `build-*` job this list does not know is a job this check does not cover.
        for job in job_headers(text):
            if job.startswith("build-") and job not in jobs:
                fail(
                    f"{path.name}: job `{job}` compiles client code but is not listed in "
                    f"BUILD_JOBS in {Path(__file__).name} — add it so its $GITHUB_ENV exports "
                    f"are checked against the option_env! recipe."
                )

        for job in jobs:
            block = job_block(text, job)
            for line, name, uses_secret in github_env_exports(block):
                if name is None:
                    fail(
                        f"{path.name} `{job}`: cannot read what this line exports to "
                        f"$GITHUB_ENV — rewrite it as `echo \"KEY=...\" >> $GITHUB_ENV` so "
                        f"the export can be checked: {line}"
                    )
                    continue
                if name not in allowed:
                    fail(
                        f"{path.name} `{job}` exports {name} to $GITHUB_ENV, which "
                        f"{CONFIG.name} never reads via option_env! — the binary cannot "
                        f"absorb it, but every build.rs, proc-macro, pnpm lifecycle script "
                        f"and third-party action in the job can read it. Delete the export; "
                        f"if a publish step needs the value, pass it as step-scoped `env:` on "
                        f"that step, straight from `secrets.*`."
                    )
                elif uses_secret and name not in client:
                    fail(
                        f"{path.name} `{job}` writes a `secrets.*` value into $GITHUB_ENV under "
                        f"the toolchain name {name} — a secret is a secret whatever it is "
                        f"called, and nothing outside the option_env! recipe belongs in a build "
                        f"job's environment: {line}"
                    )

        # (5) Release/publish/provenance jobs too: a credential those jobs legitimately use
        # still has no business in $GITHUB_ENV, where the artifact-download, gh-release and
        # attest/cosign actions inherit it. Step-scoped `env:` is the only sanctioned shape.
        for job in job_headers(text):
            if job in jobs:
                continue
            for line, name, uses_secret in github_env_exports(job_block(text, job)):
                if uses_secret and name not in client:
                    fail(
                        f"{path.name} `{job}` exports the secret {name or '<unreadable>'} to "
                        f"$GITHUB_ENV, making it visible to every later step in the job. Pass "
                        f"it as step-scoped `env:` on exactly the step that uses it: {line}"
                    )


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

    # C/C++ paths remapped too, or only the Rust half is path-independent.
    for name, text in (("release", RELEASE.read_text()), ("rebuilder", REBUILD.read_text())):
        for job in (REPRODUCIBLE_JOBS if name == "release" else ("rebuild-linux", "rebuild-capture-helper")):
            block = job_block(text, job)
            if not block:
                continue
            if "--remap-path-prefix" in block and "-ffile-prefix-map" not in block:
                fail(
                    f"the `{job}` job ({name}) remaps Rust paths but not C/C++ ones — "
                    f"`--remap-path-prefix` is a rustc flag and does not reach code compiled "
                    f"through cc-rs, so the binary embeds the absolute build path and only "
                    f"someone building at that same path can reproduce it. Add "
                    f"`-ffile-prefix-map` to CFLAGS/CXXFLAGS."
                )

    # (4) + (5) — nothing but the recipe in a build job's environment; no secret via
    # $GITHUB_ENV anywhere in the release workflows.
    check_build_job_exports(client)

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
            "reproduce the shipped bytes, or a release job's environment carries more "
            "than the recipe:\n",
            file=sys.stderr,
        )
        for f in failures:
            print(f"  ✗ {f}", file=sys.stderr)
        print(
            f"\n{len(failures)} problem(s). See docs/reproducible-builds-residuals.md.",
            file=sys.stderr,
        )
        return 1

    n_build_jobs = sum(len(j) for j in BUILD_JOBS.values())
    print(
        f"Build recipe OK — {len(release)} value(s) baked by the release are all in the "
        f"rebuilder's recipe ({len(recipe)} entries), every recipe entry is read by the client, "
        f"and none of the {n_build_jobs} release build jobs exports anything but the recipe."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
