#!/usr/bin/env python3
"""check-pinned-log-key.py — the transparency log's pinned keys must agree everywhere.

The ML-DSA-44 public key the log signs signed-tree-heads with is hard-coded in
EIGHT places across Rust, JS, JSON, Markdown and two workflows, and until #945
nothing checked that the copies agreed. That is not a tidiness problem. #732 is
the worked example: production served an `artifacts.js` pinning a key the log had
stopped signing with, and the page could only ever render "the served key
DIFFERS" — a tamper alarm, shown to every visitor, caused entirely by one stale
copy. A rotation that updates seven of eight surfaces reproduces that exactly,
and the surface it hits reports an honest log as a compromised one.

So this makes `pollis-core/src/commands/transparency.rs` the single source of
truth and asserts every other copy matches it. It is deliberately offline and
dependency-free (stdlib only) so it can gate a pull request rather than only
observe a deployed site.

Three checks, in increasing order of what they catch:

  1. **Registered copies agree.** Each entry in COPIES below must carry the key
     in the form declared for it — full 2624-char hex, the 64-char truncation
     the Learn page displays, or the derived 16-char key id.
  2. **Derived ids are actually derived.** Every key id must equal the first
     eight bytes of SHA-256 over the key's raw bytes, so a hand-typed id cannot
     drift from the key it names.
  3. **No unregistered copy exists.** Every 2624-hex literal anywhere in the repo
     must be a key this script knows about, in a file this script checks. That is
     what stops the count silently becoming nine: adding a copy without adding it
     here fails, which is the only way a registry like this stays honest.

Exit 0 = every copy agrees. Exit 1 = a report of exactly which file disagrees.

Run: python3 ./scripts/check-pinned-log-key.py   (no arguments, no network)
Its own tests: ./scripts/test-pinned-log-key.sh
"""

from __future__ import annotations

import hashlib
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# The source of truth. Both key sets live here as Rust consts and everything
# else is a copy of one of them.
TRUTH = "pollis-core/src/commands/transparency.rs"

# An ML-DSA-44 public key is 1312 bytes = 2624 lowercase hex characters.
KEY_HEX_LEN = 2624
HEX_LITERAL = re.compile(r"\b[0-9a-f]{%d}\b" % KEY_HEX_LEN)

# How many characters of the key the Learn page shows before its ellipsis.
LEARN_PREFIX_LEN = 64

# ── The registry ────────────────────────────────────────────────────────────
#
# (path, which key, form). Adding a hard-coded copy anywhere means adding a row
# here; check 3 fails otherwise.
#
#   "full"     — the complete 2624-char hex string
#   "prefix"   — the first LEARN_PREFIX_LEN chars, for display
#   "key_id"   — the 16-char derived id only
FULL, PREFIX, KEY_ID = "full", "prefix", "key_id"
SIGNER, ROOT_KEY = "signer", "root"

COPIES: list[tuple[str, str, str]] = [
    # 1. The source of truth itself — listed so it is checked for
    #    self-consistency (its key_id must match its key) like everything else.
    (TRUTH, SIGNER, FULL),
    (TRUTH, SIGNER, KEY_ID),
    (TRUTH, ROOT_KEY, FULL),
    (TRUTH, ROOT_KEY, KEY_ID),
    # 2. The website's client-side verifier — the copy #732 made stale.
    ("website/artifacts.js", SIGNER, FULL),
    ("website/artifacts.js", SIGNER, KEY_ID),
    # 3. The Learn page shows a truncation, not the whole key.
    ("website/learn.html", SIGNER, PREFIX),
    # 4. The root-signed key set published at /v1/key-set.json.
    ("key-set.json", SIGNER, FULL),
    ("key-set.json", SIGNER, KEY_ID),
    ("key-set.json", ROOT_KEY, KEY_ID),
    # 5/6. The two documents a reader is most likely to check the key against.
    ("SECURITY.md", SIGNER, FULL),
    ("README.md", SIGNER, FULL),
    # 7. The independent rebuilder verifies release artifacts under this key.
    (".github/workflows/rebuild-verify.yml", SIGNER, FULL),
    # 8. The verifier release notes quote it for people checking by hand.
    (".github/workflows/verifier-release.yml", SIGNER, FULL),
    # 9. Prose naming the key by its id.
    ("website/assurance.html", SIGNER, KEY_ID),
]

# Files that may contain a 2624-hex literal without being a pinned-key copy.
# Nothing qualifies today; kept explicit so an exemption is a visible decision.
SWEEP_EXEMPT: set[str] = set()

# Directories the unregistered-copy sweep does not descend into.
SWEEP_SKIP_DIRS = {
    ".git", "target", "node_modules", "dist", "build", ".next",
    "Pods", ".venv", "venv", "__pycache__", ".pnpm-store",
}

# Only text we could plausibly hand-edit. A 2624-hex run inside a lockfile or a
# binary is not a pinned-key copy.
SWEEP_SUFFIXES = {
    ".rs", ".js", ".mjs", ".ts", ".tsx", ".html", ".json", ".md",
    ".yml", ".yaml", ".sh", ".py", ".toml", ".txt",
}


def fail(msg: str) -> None:
    print(f"::error::{msg}")


def read(rel: str) -> str:
    path = ROOT / rel
    if not path.is_file():
        raise FileNotFoundError(rel)
    return path.read_text(encoding="utf-8", errors="replace")


def key_id_for(key_hex: str) -> str:
    """The log's own key-id rule: first 8 bytes of SHA-256 over the key bytes."""
    return hashlib.sha256(bytes.fromhex(key_hex)).hexdigest()[:16]


def extract_truth() -> dict[str, str]:
    """Pull both pinned keys out of the Rust source, BY CONST NAME.

    Named lookup rather than "the first 2624-hex literal in the file", which is
    what `website-verify.yml` used to do: `PINNED_LOG_ROOT_KEYS` holds a literal
    of the identical shape, so reordering the two consts would have silently
    made that check assert the root key while claiming to assert the signer.
    """
    src = read(TRUTH)
    keys: dict[str, str] = {}
    for name, const in ((SIGNER, "PINNED_LOG_PUBLIC_KEYS"), (ROOT_KEY, "PINNED_LOG_ROOT_KEYS")):
        anchor = src.find(f"pub const {const}")
        if anchor < 0:
            raise LookupError(
                f"{TRUTH} has no `pub const {const}` — has the shape changed? "
                f"This script must be updated with it, not around it."
            )
        found = HEX_LITERAL.search(src, anchor)
        if not found:
            raise LookupError(f"no {KEY_HEX_LEN}-char hex key after `{const}` in {TRUTH}")
        keys[name] = found.group(0)
    if keys[SIGNER] == keys[ROOT_KEY]:
        raise LookupError(
            "the signer and root pinned keys are identical — the offline root "
            "must not be the online signer, or the whole point of #754 is lost"
        )
    return keys


def check_copies(keys: dict[str, str], ids: dict[str, str]) -> list[str]:
    problems = []
    for rel, which, form in COPIES:
        try:
            body = read(rel)
        except FileNotFoundError:
            problems.append(f"{rel}: registered as carrying the pinned {which} key, but the file is missing")
            continue

        if form == FULL:
            needle, label = keys[which], f"the full {which} key"
        elif form == PREFIX:
            needle, label = keys[which][:LEARN_PREFIX_LEN], f"the displayed {which}-key prefix"
        else:
            needle, label = ids[which], f"the {which} key id"

        if needle in body:
            continue

        problems.append(
            f"{rel}: does not carry {label} ({needle[:16]}…). "
            f"A surface pinning a key the log no longer signs with reports an "
            f"HONEST log as tampered (#732/#875) — fix the copy, do not relax this."
        )
    return problems


def check_ids(keys: dict[str, str], ids: dict[str, str]) -> list[str]:
    """Every key id in the source of truth must be derived from its own key."""
    problems = []
    src = read(TRUTH)
    for which, const in ((SIGNER, "PINNED_LOG_PUBLIC_KEYS"), (ROOT_KEY, "PINNED_LOG_ROOT_KEYS")):
        anchor = src.find(f"pub const {const}")
        declared = re.search(r'key_id:\s*"([0-9a-f]{16})"', src[anchor:])
        if not declared:
            problems.append(f"{TRUTH}: `{const}` declares no 16-char key_id")
            continue
        if declared.group(1) != ids[which]:
            problems.append(
                f"{TRUTH}: `{const}` declares key_id {declared.group(1)} but "
                f"SHA-256 of its key gives {ids[which]} — the id names a "
                f"different key from the one beside it"
            )
    return problems


def sweep_for_unregistered(keys: dict[str, str]) -> list[str]:
    """Every 2624-hex literal in the repo must be a known key in a known file."""
    registered = {rel for rel, _, _ in COPIES} | SWEEP_EXEMPT
    known = set(keys.values())
    problems = []

    for path in sorted(ROOT.rglob("*")):
        if not path.is_file() or path.suffix not in SWEEP_SUFFIXES:
            continue
        rel = path.relative_to(ROOT).as_posix()
        if any(part in SWEEP_SKIP_DIRS for part in path.relative_to(ROOT).parts):
            continue
        try:
            body = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        for literal in set(HEX_LITERAL.findall(body)):
            if literal not in known:
                problems.append(
                    f"{rel}: contains a {KEY_HEX_LEN}-char hex key that is neither "
                    f"pinned key ({literal[:16]}…). Either it is a stale copy from "
                    f"before a rotation, or a new key nobody pinned."
                )
            elif rel not in registered:
                problems.append(
                    f"{rel}: carries a pinned key but is not registered in "
                    f"scripts/check-pinned-log-key.py. Add it to COPIES so a "
                    f"future rotation is forced to update it too — an unregistered "
                    f"copy is exactly the ninth place #945 exists to prevent."
                )
    return problems


def main() -> int:
    try:
        keys = extract_truth()
    except (LookupError, FileNotFoundError) as e:
        fail(str(e))
        return 1

    ids = {which: key_id_for(key) for which, key in keys.items()}

    # `--print <what>` gives a shell caller the source-of-truth value without
    # re-implementing the extraction. `website-verify.yml` uses it to compare the
    # DEPLOYED site against the repo; it used to `grep -oE '"[0-9a-f]{2624}"' |
    # head -1`, which was one const reorder away from asserting the root key.
    if len(sys.argv) == 3 and sys.argv[1] == "--print":
        wanted = {
            "signer": keys[SIGNER],
            "root": keys[ROOT_KEY],
            "signer-id": ids[SIGNER],
            "root-id": ids[ROOT_KEY],
        }
        if sys.argv[2] not in wanted:
            fail(f"--print takes one of {', '.join(sorted(wanted))}")
            return 1
        print(wanted[sys.argv[2]])
        return 0
    if len(sys.argv) > 1:
        fail(f"usage: {Path(sys.argv[0]).name} [--print signer|root|signer-id|root-id]")
        return 1

    problems = check_ids(keys, ids) + check_copies(keys, ids) + sweep_for_unregistered(keys)

    if problems:
        for p in problems:
            fail(p)
        print(
            f"\n{len(problems)} pinned-key disagreement(s). The source of truth is "
            f"{TRUTH}; every other copy must match it."
        )
        return 1

    checked = len({rel for rel, _, _ in COPIES})
    print(
        f"OK — signer {ids[SIGNER]} ({keys[SIGNER][:16]}…) and root {ids[ROOT_KEY]} "
        f"({keys[ROOT_KEY][:16]}…) agree across all {len(COPIES)} pinned references "
        f"in {checked} files, and no unregistered copy exists."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
