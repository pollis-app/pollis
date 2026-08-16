# Goal A — Commit-log DB deploy runbook

> **STATUS: COMPLETE.** This cutover shipped. Prod and dev both run a separate
> commit-log DB — `LOG_DB_URL` + `LOG_DB_ADMIN_TOKEN` are wired into
> `delivery-deploy-prod.yml` / `delivery-deploy-dev.yml`, which migrate the log DB
> on every deploy. The runbook is kept as the record of *how* it was done and as
> the procedure to re-run when standing up a new environment; it is no longer a
> list of pending work. Two sections that described the world before the cutover
> have been marked obsolete in place rather than deleted, because the reasoning in
> them still explains why the current shape is what it is.

**Scope:** the infra + human steps that took #420 (Goal A — commit-log
sole-writer) from "code merged" to "live in prod". Design and rationale live in
[`goal-a-commit-log-sole-writer.md`](./goal-a-commit-log-sole-writer.md); this
file is only the ordered operational checklist.

The end state: three MLS control-plane tables (`mls_commit_log`,
`mls_group_info`, `mls_welcome`) live in a **separate Turso DB**. The Delivery
Service (DS) holds a **read-write** token and is the **only** writer; clients
hold a **read-only** token and read direct / write only via the DS. Because
Turso tokens are database-level (not table-level), a client *physically cannot*
write the commit log around the DS — epoch/commit slips become structurally
impossible.

> **Cutover model (OD-2): fresh start, no backfill.** The DS points at an
> **empty** log DB and just starts writing. There is no main→log copy and no
> dual-write window. In-flight conversations re-sync via MLS external-join
> recovery (#412): GroupInfo is republished to the log DB on every commit, so a
> behind client rejoins at the current epoch. Accepted tradeoff: transient
> re-sync churn for active groups at cutover. No message sent while you were a
> member is lost.

---

## CODE vs INFRA/HUMAN

Everything below the line in each section is a **human action**. The CODE side
(merged via PR) is already covered by streams S1–S6 plus S7's CI wiring. The one CODE prerequisite this runbook depended on — the **000007 hazard** — was
resolved in #420 and is retained below only as history.

---

## ~~000007 hazard~~ — RESOLVED in #420, kept as history

> **Obsolete.** The fix below was applied: the log-DB schema no longer sits in the
> shared migration directory. It lives in its own series at
> `pollis-schema/migrations-log/`, starting at `000001_commit_log_db.sql`, with
> its own `schema_migrations` table in the log DB. There is no `000007` in
> `pollis-schema/migrations/` and no way for a log-DB migration to be applied
> to the main DB. Nothing here is actionable; it is preserved because it explains
> why the two migration series exist.

<details>
<summary>Original hazard text</summary>


`scripts/db-apply.sh` applies **every** `*.sql` in `MIGRATIONS_DIR` that the
target DB has not already recorded, then records each version into **that DB's
own** `schema_migrations`. `000007_commit_log_db.sql` currently sits in the
shared dir `pollis-schema/migrations/` alongside the whole main schema
(`000000_baseline` + `000001..000006`). That creates a two-way problem:

- **Log DB (empty) pointed at the shared dir → gets the ENTIRE main schema.**
  The baseline (000000) would create `users`, `groups`, `channels`, … and an
  **FK-bearing** `mls_commit_log`/`mls_welcome`. 000007's `CREATE TABLE IF NOT
  EXISTS` then no-ops, so the log DB ends up with exactly the FK-bearing tables
  the split exists to avoid. Wrong DB shape.
- **Main DB pointed at the shared dir → also applies 000007.** The
  `CREATE TABLE IF NOT EXISTS` is a harmless no-op (the tables already exist on
  main), but it still **records v7** in main's `schema_migrations` — misleading,
  since 000007 is a log-DB-only schema that should never appear in main's
  migration history.

**Recommended fix (CODE, owned by the migrations stream S2 — not done in S7):**
**move** `000007_commit_log_db.sql` out of the shared dir into a dedicated
log-DB migrations dir, renumbered as its own fresh sequence, e.g.:

```
pollis-schema/migrations-log/000001_commit_log_db.sql
```

This keeps the main schema out of the log DB (the log DB's dir contains *only*
the three no-FK tables + indexes) and keeps the log schema out of the main DB's
pending set (the main apply never sees it again → no misleading v7 on main).
The log DB gets its own independent `schema_migrations` sequence starting at 1.

The release workflow's commit-log apply step already points `MIGRATIONS_DIR` at
`pollis-schema/migrations-log`. **Until that dir exists the step safely
no-ops** ("No pending migrations") — so the workflow is correct now and becomes
functional the moment S2 lands the move. **Do not push a release tag expecting
the log DB to be provisioned until that move has merged.**

(Alternative, not recommended: leave 000007 in the shared dir and accept the
harmless-but-misleading v7 record on main + manually pre-create the log DB
schema by hand. Rejected — it pollutes the log DB with the full main schema as
described above. The dedicated-dir move is the clean answer.)

---

</details>

---

## 1. Turso — create DBs and mint tokens (per env)

Do this for **each** of prod / dev / test. Names: `pollis-log-prod`,
`pollis-log-dev`, `pollis-log-test`.

```bash
# Create the DB
turso db create pollis-log-prod

# URL (libsql://…) — this is LOG_DB_URL
turso db show pollis-log-prod --url

# READ-WRITE token → LOG_DB_ADMIN_TOKEN (DS + migrations only; NEVER in a client)
turso db tokens create pollis-log-prod

# READ-ONLY token → LOG_DB_TOKEN (embedded in client builds)
turso db tokens create pollis-log-prod --read-only
```

Repeat with `pollis-log-dev` / `pollis-log-test`.

> The read-only token is the load-bearing security control: it is what makes a
> client *unable* to write the log DB. Mint it with `--read-only` and never
> hand a client anything else.

**Apply the schema to each new DB.** Prod is applied automatically by the
release workflow's "commit-log DB" step (once the S2 dir move has landed). For
dev/test, apply by hand against the dedicated dir:

```bash
TURSO_URL='libsql://pollis-log-dev-….turso.io' \
TURSO_TOKEN='<LOG_DB_ADMIN_TOKEN for dev>' \
MIGRATIONS_DIR='pollis-schema/migrations-log' \
  ./scripts/db-apply.sh
```

---

## 2. Doppler — store secrets, sync to GH Actions

Per env config, store all three:

| Doppler key | Value | Consumed by |
|---|---|---|
| `LOG_DB_URL` | log DB URL | client builds + DS + migrations |
| `LOG_DB_TOKEN` | **read-only** token | client builds only |
| `LOG_DB_ADMIN_TOKEN` | **read-write** token | DS + migrations only |

- Put prod values in the `prd_prod` config (it syncs → **GitHub Actions
  secrets**). Put dev values in the dev config, test values in the test config.
- The `prd_prod` → GH Actions sync gives the release workflow exactly the three
  secrets it references:
  - `secrets.LOG_DB_URL`
  - `secrets.LOG_DB_TOKEN`        (read-only — injected into builds)
  - `secrets.LOG_DB_ADMIN_TOKEN`  (read-write — migrations apply step only)
- If you don't use the Doppler→GH sync, add those three as GH Actions repo
  secrets manually. The workflow already references all three; missing secrets
  serialize to empty strings, leaving the client/DS on the main-DB fallback
  (inert, not broken).

---

## 3. Point the DS at the log DB

> **Superseded (#515).** This step originally edited `/root/ds.env` on a VPS and
> poked Watchtower to recreate the container. The DS no longer runs on a VPS: it
> runs on **Cloudflare Containers** (a Worker + Durable Object fronting the same
> image), deployed by `delivery-deploy-prod.yml` / `delivery-deploy-dev.yml`. The
> `DEV_OTP` co-location warning below no longer applies either — prod and dev are
> separate Workers with separate `wrangler.*.jsonc` configs and separate secret
> stores, so a dev value cannot reach prod by sharing a file.

Secrets live in Doppler and are synced into Wrangler's Secrets Store by the deploy
workflow. The DS needs, in its own environment:

```ini
TURSO_URL / TURSO_TOKEN              # main DB — auth + user_device lookups
LOG_DB_URL / LOG_DB_ADMIN_TOKEN      # commit-log DB, READ-WRITE (sole writer)
```

The DS uses the **admin (read-write)** token because it is the sole writer. When
both `LOG_DB_URL` and `LOG_DB_ADMIN_TOKEN` are set it routes MLS control-plane
writes to the log DB; when unset it falls back to the main DB (see
`pollis-delivery/src/main.rs`). **Never** give the DS `LOG_DB_TOKEN` (the
read-only token) — that one is for client builds.

To apply: set the values in Doppler, then run the **Deploy Delivery (prod)** or
**Deploy Delivery (dev)** workflow. Each deploy also runs `db-apply.sh` against
the log DB, so the migration series in `migrations-log/` is applied before the new
image goes live. Confirm the rollout with `curl https://api.pollis.com/version`
and check the returned SHA is the commit you deployed — a workflow that merely
succeeded is not proof the new container is serving.

---

## 4. Local dev / test env files

- `.env.development` — add `LOG_DB_URL` + a token for the dev log DB. A single
  read-write token is fine locally (the read-only split only matters in prod).
- `.env.test` — add `LOG_DB_URL` + a token for the test log DB.
- Both are optional: with `LOG_DB_*` unset, `pollis-core` falls back to the main
  remote DB (`for_test()` → `None`), so existing tests keep passing unchanged.

---

## 5. ORDERING (critical — from §8 of the design doc)

The token flip is the last thing that happens. Do these strictly in order:

1. **Create the DBs and apply the migration to them** (sections 1–2). The log DB
   now has the three tables and is empty. Nothing reads or writes it yet.
2. **Point the DS at the log DB and redeploy it** (section 3) so the DS becomes
   the writer. Confirm all 8 writes (W1–W8) route through the DS — W1–W3 via
   `/v1/commits`, W4–W8 via their new endpoints — and land in the log DB. The DS
   holds the read-write token; clients still talk to the DS, not the log DB.
3. **Only THEN ship a client release that reads the log DB and holds the
   read-only token.** Push the release tag; the workflow injects `LOG_DB_TOKEN`
   (read-only) into the builds and runs the commit-log migration apply.
4. **The read-only token flip is the LAST step.** Never ship a client carrying
   `LOG_DB_TOKEN` before the DS is writing the log DB and *all 8* writes route
   through it — otherwise every client write to those tables hits a read-only
   token and bricks. Reads are inert before this point (the client falls back to
   the main DB until `LOG_DB_*` is compiled in), so there is no rush; correctness
   requires waiting.

> Why this order: a read-only client that reaches the log DB before the DS has
> populated it reads empty; a client that tries to *write* the log DB before its
> writes are routed through the DS gets a permission failure. Both are avoided by
> "DS writes first, client reads/holds-RO-token last." OD-2 means there is no
> backfill to wait on — step 2 is the DS simply starting to write the empty DB,
> and active groups re-sync via external-join at cutover.

---

## 6. Rollback

If the log DB misbehaves after cutover:

1. **Remove `LOG_DB_URL` + `LOG_DB_ADMIN_TOKEN` from the DS's Doppler config** and
   redeploy (Deploy Delivery workflow). With `LOG_DB_*` unset the DS falls back to
   writing the **main** DB's copies of the three tables (the fallback path in
   `pollis-delivery/src/main.rs`) — old shipped clients still read/write those,
   so the system is back to pre-Goal-A behavior.
2. **Revert the client release** — ship/point users at the previous build (or a
   new tag) that does not carry `LOG_DB_TOKEN`, so clients read the main DB
   again. Clients with the RO token compiled in fall back to the main remote DB
   when the log DB is unreachable, but pulling the release removes the variable
   entirely.

Because the main-DB copies of the three tables were **never dropped** in this
phase (additive-only), the fallback is always a live, populated target.
