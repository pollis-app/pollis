#!/usr/bin/env python3
"""Apply the commit-log-DB migrations (`migrations-log/`) to the single libsql
DB used by the e2e backend fixture.

Why this exists: in production the MLS control-plane tables live on a SEPARATE
commit-log DB (`LOG_DB_URL`, issue #420). `start-backend.sh` runs the DS in
single-DB fallback (LOG_DB_* unset), so those tables SHARE the one libsql DB —
but `db-apply.sh` only applies the main-DB `migrations/` dir. The incremental
commit-log migrations (`mls_welcome` UNIQUE dedupe index #430, `mls_commit_since`
#539) are therefore never applied, so the DS's idempotent Welcome upsert fails
with "ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint" and
Welcomes never persist — cross-client MLS delivery silently breaks.

These migrations are all `CREATE ... IF NOT EXISTS` (idempotent), so they're
applied by statement directly over the Hrana pipeline API, bypassing
`schema_migrations` version tracking (whose versions collide with the main dir).
"""
import glob
import json
import os
import sys
import urllib.request

TURSO_URL = os.environ.get("TURSO_URL", "http://127.0.0.1:8080").replace("libsql://", "https://")
# `pollis-schema/migrations-log`, NOT `pollis-core/src/db/migrations-log`: the
# migrations moved into the `pollis-schema` crate in #920 and this path was not
# updated with them. The glob then matched nothing and this script "succeeded"
# having applied 0 statements, so the commit-log tables silently stayed at their
# baseline — reintroducing exactly the breakage the docstring above says this
# script exists to prevent. The emptiness guard in `main` is what makes that
# failure mode loud rather than silent if the path ever drifts again.
MIG_DIR = os.path.join(os.path.dirname(__file__), "..", "..", "pollis-schema", "migrations-log")


def statements(sql: str):
    # Strip `--` line comments, then split on `;`. The migration files are plain
    # DDL with no `;` inside string literals, so a naive split is correct here.
    lines = [ln for ln in sql.splitlines() if not ln.strip().startswith("--")]
    body = "\n".join(lines)
    for stmt in body.split(";"):
        if stmt.strip():
            yield stmt.strip()


def main():
    reqs = []
    paths = sorted(glob.glob(os.path.join(MIG_DIR, "*.sql")))
    if not paths:
        print(
            f"[apply-log-migrations] FAILED: no .sql files under {MIG_DIR} — "
            "the migrations moved and this path is stale. Applying zero "
            "commit-log migrations silently breaks cross-client MLS delivery, "
            "so this is an error, not a no-op.",
            file=sys.stderr,
        )
        sys.exit(1)
    for path in paths:
        with open(path) as f:
            for stmt in statements(f.read()):
                reqs.append({"type": "execute", "stmt": {"sql": stmt}})
    reqs.append({"type": "close"})

    data = json.dumps({"requests": reqs}).encode()
    req = urllib.request.Request(
        f"{TURSO_URL}/v2/pipeline", data=data, headers={"content-type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        out = json.load(resp)

    errors = [r for r in out.get("results", []) if r.get("type") == "error"]
    if errors:
        print(f"[apply-log-migrations] FAILED: {json.dumps(errors)}", file=sys.stderr)
        sys.exit(1)
    print(f"[apply-log-migrations] applied {len(reqs) - 1} statement(s) from migrations-log/", file=sys.stderr)


if __name__ == "__main__":
    main()
