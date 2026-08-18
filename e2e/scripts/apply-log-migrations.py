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
import sqlite3
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
    """Split a migration file into individual statements.

    Trigger-aware. A naive `split(";")` is WRONG here and was: a
    `CREATE TRIGGER ... BEGIN <body>; <body>; END;` body contains semicolons, so
    splitting on them shreds every trigger in `000005_mls_commit_log_triggers.sql`
    into unparseable fragments — the server answers `unexpected end of input`,
    and the orphaned `END` parses as `COMMIT`, hence
    `cannot commit - no transaction is active`.

    So `;` only terminates a statement when we are NOT inside a `BEGIN ... END`
    block. `sqlite3.complete_statement` in the self-check below is what proves
    the split is right rather than merely plausible.
    """
    lines = [ln for ln in sql.splitlines() if not ln.strip().startswith("--")]
    buf: list[str] = []
    depth = 0
    for ln in lines:
        stripped = ln.strip()
        upper = stripped.upper()
        buf.append(ln)
        # `BEGIN` opening a trigger body. Bare `BEGIN`/`BEGIN` + comment only —
        # never `BEGIN TRANSACTION`, which these files do not use.
        if upper == "BEGIN" or upper.startswith("BEGIN "):
            depth += 1
            continue
        if depth:
            if upper.startswith("END;") or upper == "END":
                depth -= 1
                if depth == 0 and stripped.endswith(";"):
                    stmt = "\n".join(buf).strip().rstrip(";").strip()
                    if stmt:
                        yield stmt
                    buf = []
            continue
        if stripped.endswith(";"):
            stmt = "\n".join(buf).strip().rstrip(";").strip()
            if stmt:
                yield stmt
            buf = []
    tail = "\n".join(buf).strip().rstrip(";").strip()
    if tail:
        yield tail


# A `CREATE TRIGGER` whose body carries its own `;` terminators — the exact
# shape the previous naive `split(";")` shredded. Kept as an executable fixture
# so the splitter cannot silently regress the next time someone adds a trigger:
# under the naive split this parses as three broken fragments, under a correct
# one it is a single statement.
SELF_TEST_SQL = """
-- a comment; with a semicolon
CREATE TABLE t (a INTEGER);

CREATE TRIGGER t_guard
BEFORE INSERT ON t
FOR EACH ROW
BEGIN
    SELECT RAISE(ABORT, 'nope') WHERE NEW.a < 0;
    SELECT RAISE(ABORT, 'nope either') WHERE NEW.a > 10;
END;

CREATE INDEX t_a ON t(a);
"""


def check_complete(stmts, where):
    """Every produced statement must be a COMPLETE SQL statement.

    This is the guard that turns a bad split from a confusing server-side
    `unexpected end of input` into a local, named failure — and it is what makes
    the trigger handling in `statements` verified rather than assumed.
    """
    bad = [st for st in stmts if not sqlite3.complete_statement(st + ";")]
    if bad:
        print(
            f"[apply-log-migrations] FAILED: {len(bad)} incomplete statement(s) "
            f"from {where} — the splitter is wrong (a CREATE TRIGGER body's "
            f"internal `;` is the usual cause). First: {bad[0][:120]!r}",
            file=sys.stderr,
        )
        sys.exit(1)


def self_test():
    got = list(statements(SELF_TEST_SQL))
    check_complete(got, "the self-test fixture")
    triggers = [st for st in got if "CREATE TRIGGER" in st.upper()]
    assert len(got) == 3, f"expected 3 statements, got {len(got)}: {got}"
    assert len(triggers) == 1, f"trigger body must stay one statement, got {triggers}"
    assert "END" in triggers[0].upper(), "trigger statement must include its END"
    print("[apply-log-migrations] self-test ok (3 statements, trigger intact)")


def main():
    if "--self-test" in sys.argv:
        self_test()
        return
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
    collected = []
    for path in paths:
        with open(path) as f:
            collected.extend(statements(f.read()))
    check_complete(collected, MIG_DIR)
    reqs = [{"type": "execute", "stmt": {"sql": stmt}} for stmt in collected]
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
