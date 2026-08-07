#!/usr/bin/env python3
"""check-ds-config-chain.py — prove DS config actually reaches the container (#760).

A config value must traverse five links to affect the running Delivery Service:

    1. Doppler                                      (source of truth)
    2. KEYS in .github/scripts/sync-ds-secrets.sh   (what is pushed to the Secrets Store)
    3. secrets_store_secrets / vars in wrangler.*   (what the WORKER can see)
    4. SECRET_KEYS / envVars in worker/index.ts     (what the CONTAINER is given)
    5. the DS reads the OS env var                  (effect)

Each link is a separate hand-maintained list. Three of the five were missing for
POLLIS_DS_METRICS_TOKEN and it took three PRs to notice, because a broken link is
invisible until the one before it is fixed: the deploy goes green, the sync log says
the secret was created, and the value simply has no effect.

Link 4 is the trap — a wrangler `var` binds to the WORKER, not the container, and a
Secrets Store binding reaches the container only if worker/index.ts forwards it.

This checks links 2-5 against pollis-delivery/ds-config-manifest.json, in BOTH
directions. A declared key missing from a link fails; an env var the DS reads that
nothing declares also fails. Link 1 (Doppler) cannot be checked without a token —
the deploy's own sync step is what surfaces that one.

Exit 0 = every declared key is wired end to end. Exit 1 = a report of what is broken.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "pollis-delivery" / "ds-config-manifest.json"
SYNC = ROOT / ".github" / "scripts" / "sync-ds-secrets.sh"
WORKER = ROOT / "pollis-delivery" / "worker" / "index.ts"
WRANGLER = {
    "dev": ROOT / "pollis-delivery" / "wrangler.dev.jsonc",
    "prod": ROOT / "pollis-delivery" / "wrangler.prod.jsonc",
}
DS_SRC = ROOT / "pollis-delivery" / "src"

failures: list[str] = []


def fail(msg: str) -> None:
    failures.append(msg)


def strip_jsonc(text: str) -> str:
    """Drop //-comments. Only ever applied to our own wrangler configs, whose string
    values contain no `//` (the image ref uses no scheme), so a line-oriented strip is
    sufficient and avoids depending on a JSON5 parser."""
    return re.sub(r"^\s*//.*$", "", text, flags=re.MULTILINE)


def load_manifest() -> dict:
    return json.loads(strip_jsonc(MANIFEST.read_text()))


def sync_keys() -> set[str]:
    """KEYS=( ... ) plus the conditional DEV_OTP append (link 2)."""
    text = SYNC.read_text()
    m = re.search(r"^KEYS=\((.*?)\)", text, re.DOTALL | re.MULTILINE)
    if not m:
        fail(f"{SYNC.name}: could not find a `KEYS=( ... )` block to read")
        return set()
    keys = set(m.group(1).split())
    keys |= set(re.findall(r"KEYS\+=\(([A-Z_0-9 ]+)\)", text)[0].split()) if re.findall(
        r"KEYS\+=\(([A-Z_0-9 ]+)\)", text
    ) else set()
    return keys


def wrangler_bindings(env: str) -> tuple[set[str], set[str]]:
    """(secrets_store binding names, vars names) for one environment (link 3)."""
    cfg = json.loads(strip_jsonc(WRANGLER[env].read_text()))
    secrets = {b["binding"] for b in cfg.get("secrets_store_secrets", [])}
    variables = {k for k in cfg.get("vars", {}) if not k.startswith("$")}
    return secrets, variables


def worker_secret_keys() -> set[str]:
    """SECRET_KEYS = [ ... ] as const (link 4, secrets half)."""
    m = re.search(r"const SECRET_KEYS = \[(.*?)\] as const", WORKER.read_text(), re.DOTALL)
    if not m:
        fail(f"{WORKER.name}: could not find `const SECRET_KEYS = [ ... ] as const`")
        return set()
    return set(re.findall(r'"([A-Z_0-9]+)"', m.group(1)))


def worker_forwarded_vars() -> set[str]:
    """Names the worker copies into the container's `envVars` (link 4, vars half).

    Matches both the plain `KEY: this.env.KEY ?? ...` form and the conditional-spread
    form used for optional values. Keyed on what appears as a PROPERTY of envVars, since
    merely reading `this.env.X` in the worker does not deliver X to the container — that
    distinction is the entire #720 bug."""
    text = WORKER.read_text()
    m = re.search(r"envVars = \{(.*?)\n  \};", text, re.DOTALL)
    if not m:
        fail(f"{WORKER.name}: could not find the `envVars = {{ ... }};` block")
        return set()
    body = m.group(1)
    return set(re.findall(r"^\s*([A-Z_0-9]+):", body, re.MULTILINE)) | set(
        re.findall(r"\{\s*([A-Z_0-9]+):\s*this\.env\.", body)
    )


def ds_read_vars() -> set[str]:
    """Every env var the DS actually reads (link 5)."""
    found: set[str] = set()
    for rs in DS_SRC.rglob("*.rs"):
        found |= set(re.findall(r'(?:env::)?var\("([A-Z_0-9]+)"\)', rs.read_text()))
    return found


def main() -> int:
    man = load_manifest()
    sec_both = set(man["secrets"]["both"])
    sec_dev = set(man["secrets"]["dev_only"])
    all_secrets = sec_both | sec_dev
    container_vars = set(man["container_vars"]["keys"])
    worker_vars = set(man["worker_vars"]["keys"])
    aliases = man["aliases"]["keys"]
    code_defaults = set(man["code_default_only"]["keys"])

    declared = all_secrets | container_vars | worker_vars | set(aliases) | code_defaults

    # ── Link 2: the sync script pushes every declared secret ────────────────
    sk = sync_keys()
    for k in sorted(all_secrets - sk):
        fail(f"link 2: {k} is declared a secret but is NOT in KEYS in {SYNC.name} — it will never be pushed to the Secrets Store")
    for k in sorted(sk - all_secrets):
        fail(f"link 2: {SYNC.name} pushes {k}, which the manifest does not declare — add it to the manifest or stop syncing it")

    # ── Link 3: each environment binds what it should ───────────────────────
    for env in ("dev", "prod"):
        bound, variables = wrangler_bindings(env)
        expected = sec_both | (sec_dev if env == "dev" else set())
        for k in sorted(expected - bound):
            fail(f"link 3 [{env}]: {k} has no `secrets_store_secrets` binding in wrangler.{env}.jsonc — the Worker cannot see it")
        for k in sorted(bound - expected):
            fail(f"link 3 [{env}]: wrangler.{env}.jsonc binds {k}, which the manifest does not expect in {env}")
        for k in sorted(container_vars - variables):
            fail(f"link 3 [{env}]: container var {k} is missing from `vars` in wrangler.{env}.jsonc")
        for k in sorted(worker_vars - variables):
            fail(f"link 3 [{env}]: worker var {k} is missing from `vars` in wrangler.{env}.jsonc")
        undeclared = variables - container_vars - worker_vars
        for k in sorted(undeclared):
            fail(f"link 3 [{env}]: wrangler.{env}.jsonc sets var {k}, which the manifest does not declare")

    # ── Link 4: the worker hands them to the container ──────────────────────
    wsk = worker_secret_keys()
    for k in sorted(all_secrets - wsk):
        fail(f"link 4: {k} is not in SECRET_KEYS in {WORKER.name} — it is bound to the Worker but NEVER injected into the container")
    for k in sorted(wsk - all_secrets):
        fail(f"link 4: {WORKER.name} lists {k} in SECRET_KEYS, which the manifest does not declare")

    fwd = worker_forwarded_vars()
    for k in sorted(container_vars - fwd):
        fail(f"link 4: container var {k} is set in wrangler `vars` but NOT forwarded via envVars in {WORKER.name} — a var binds to the WORKER, so the DS will silently use its compiled-in default (this is the #720 bug)")
    for k in sorted(worker_vars & fwd):
        fail(f"link 4: {k} is declared worker-only but IS forwarded to the container in {WORKER.name} — declare it a container var or stop forwarding it")

    # ── Link 5: the DS reads what we deliver, and we declare what it reads ──
    read = ds_read_vars()
    for k in sorted(read - declared):
        fail(f"link 5: the DS reads {k} but nothing declares it — add it to ds-config-manifest.json (as a container var if it must be settable, or code_default_only if the default is the policy)")
    for k in sorted(declared - read - worker_vars):
        fail(f"link 5: {k} is declared and delivered but the DS never reads it — dead config; remove it or start reading it")

    for alias, target in sorted(aliases.items()):
        if target not in read:
            fail(f"link 5: alias {alias} falls back to {target}, which the DS does not read")

    if failures:
        print("DS config chain BROKEN — a value can be correct in Doppler and still never reach the container:\n", file=sys.stderr)
        for f in failures:
            print(f"  ✗ {f}", file=sys.stderr)
        print(f"\n{len(failures)} problem(s). See pollis-delivery/ds-config-manifest.json.", file=sys.stderr)
        return 1

    print(
        f"DS config chain OK — {len(all_secrets)} secret(s), {len(container_vars)} container var(s), "
        f"{len(worker_vars)} worker var(s), {len(code_defaults)} code-default-only, "
        f"{len(aliases)} alias(es) verified across links 2-5."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
