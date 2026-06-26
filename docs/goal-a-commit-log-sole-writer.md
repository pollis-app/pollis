# Goal A — Commit-log sole-writer (read-only client on the log DB)

**Tracking:** #420 (split from #419 / Goal B). **Governing principle:**
`docs/backend-core-invariants.md` — *invalid states unrepresentable*.

This is the **coordination document** for the parallel implementation of #420.
It is the single source of truth: the verified write/read surface, the design,
the open decisions, the work breakdown with **file ownership** (so parallel
agents don't collide), and the ordering/hazards. Keep the **status board**
current as work lands.

---

## 0. Goal, in one paragraph

Move `mls_commit_log` + `mls_welcome` + `mls_group_info` into their **own Turso
database**. The Delivery Service (DS) holds a **read-write** token to it and is
the **only** writer; the desktop/mobile client holds a **read-only** token there
(reads direct, writes only via the DS). Turso tokens are **database-level, not
table-level** — that constraint is the entire reason these three tables must
live in a separate DB. Result: a client *physically cannot* write the commit log
around the DS, so gaps/forks require a non-contiguous write the sole writer
refuses → **epoch/commit slips become structurally impossible.**

Independent of Goal B (#419, the full ~61-site migration). Goal A delivers the
correctness guarantee on its own.

---

## 1. Verified surface (audited 2026-06-25, against current `main`)

All sites below are **remote** (libsql, `state.remote_db`). Local SQLite writes
are out of scope. Line numbers are anchors — re-verify at edit time.

### 1a. WRITE sites — 8 total (every one must route through the DS)

| # | Table | File:line | Op | Fn | Notes |
|---|---|---|---|---|---|
| W1 | `mls_commit_log` | `mls/delivery.rs:110` | INSERT | `direct_submit` | the commit row |
| W2 | `mls_group_info` | `mls/delivery.rs:135` | UPSERT | `direct_submit` | resulting-epoch GroupInfo |
| W3 | `mls_welcome` | `mls/delivery.rs:153` | INSERT | `direct_submit` | per-recipient Welcomes |
| W4 | `mls_group_info` | `mls/group_state.rs:78` | UPSERT | `publish_group_info` | **republish** (not in #420 ticket) |
| W5 | `mls_welcome` | `mls/welcomes.rs:121` | UPDATE `delivered=1` | `poll_mls_welcomes_inner` | **delivered-flag** (not in ticket) |
| W6 | `mls_welcome` | `state.rs:284` | UPDATE `delivered=0` | `load_user_db_with_key` | reset on DB load/re-enroll |
| W7 | `mls_welcome` | `state.rs:292` | UPDATE `delivered=0` | `load_user_db_with_key` | reset (all devices) |
| W8 | `mls_welcome` | `commands/device_enrollment.rs:795` | DELETE | `reset_identity_and_devices` | identity-reset cleanup (domain G) |

> **W1–W3** are the `direct_submit` path the ticket calls out. **W4–W8 are the
> scope gap** — they are non-`direct_submit` client writes to these tables and
> will fail under a read-only token. Each needs a DS endpoint (see §3 + Open
> Decision OD-1).

### 1b. READ sites — 6 total (repoint to the read-only log-DB connection)

| # | Table | File:line | Fn |
|---|---|---|---|
| R1 | `mls_commit_log` | `mls/reconcile.rs:48` | `our_commit_is_canonical` |
| R2 | `mls_commit_log` | `mls/group_state.rs:631` | `process_pending_commits_locked` |
| R3 | `mls_commit_log` | `commands/voice_e2ee.rs:284` | `catch_up_mls_group` |
| R4 | `mls_group_info` | `mls/group_state.rs:191` | `external_join_attempt` |
| R5 | `mls_group_info` | `commands/voice_e2ee.rs:183` | `published_group_epoch` |
| R6 | `mls_welcome` | `mls/welcomes.rs:90` | `poll_mls_welcomes_inner` |

Reads are **safe to repoint now**: the new `log_db` connection falls back to
`remote_db` when the log DB isn't configured (tests / pre-cutover), so repointing
is behaviorally inert until `LOG_DB_*` is set.

### 1c. Cross-DB FK references (must drop when tables move)

- `mls_commit_log.sender_id -> users(id) ON DELETE CASCADE`
- `mls_welcome.recipient_id -> users(id) ON DELETE CASCADE`
- `mls_group_info` — no FK.

In the log DB these become cross-database references → **omit the FKs**. The DS
already validates `sender_id`/membership server-side before writing.

---

## 2. The connection spine (dependency root)

Add a **second** remote connection to `pollis-core`, read-only against the log DB,
alongside the existing read-write `remote_db`.

- **`config.rs`** — add `log_db_url: Option<String>` + `log_db_token:
  Option<String>`, plumbed with the `option_env!(...).or_else(std::env::var).filter(non-empty)`
  pattern (mirror `pollis_delivery_url`). `for_test()` → `None`.
- **`state.rs`** — add `pub log_db: Arc<RemoteDb>`. In `AppState::new`, build it
  from the log-DB config **iff present**, else `Arc::clone(&remote_db)`
  (fallback). Add `log_db` param to `new_with_parts`.
- **`db/remote.rs`** — no new type needed; `RemoteDb::connect(url, token)` already
  fits. (Read-only is enforced by the *token*, not the client.)
- **`bridge.rs`** — add the two optional fields to `InitConfig` (mobile init),
  `#[serde(default)]` + `.filter(non-empty)`.

**Every callsite that builds `AppState`/`Config`** (must stay compiling):
`src-tauri/src/lib.rs` (`from_env` + `AppState::new` — no change, internal),
`pollis-core/src/bridge.rs`, `src-tauri/tests/flows/harness.rs` (passes
`new_with_parts` — needs the new arg), `src-tauri/src/test_harness.rs`.

---

## 3. The DS side

- **`pollis-delivery`** gains a log-DB connection. Cleanest: the DS's existing
  `TURSO_URL`/`TURSO_TOKEN` simply **point at the log DB** (it's becoming the MLS
  control-plane writer). During the dual-write transition (OD-2) it needs *both*
  the main DB (for backward-compat writes) and the log DB — so a second
  `Db`/env pair (`LOG_DB_URL`/`LOG_DB_TOKEN`) with fallback to the main one.
- **New endpoints** for W4–W8 (auth-required; actor == authenticated user;
  server-side membership/role checks):
  - `POST /v1/group-info` — republish GroupInfo (W4).
  - `POST /v1/welcomes/ack` — mark delivered (W5).
  - welcome delivered-reset (W6/W7) + welcome delete (W8) — fold into the
    device-registration / identity-reset endpoints, or a small
    `POST /v1/welcomes/reset` + `DELETE /v1/welcomes`. **See OD-1.**
- **Test harness** (`src-tauri/tests/flows/harness.rs`): the in-process DS uses a
  **custom `delivery_submit`** handler that calls `commit::submit_commit`
  directly and **bypasses `build_router`'s auth** (confirmed). To exercise the
  real path, either serve the real `build_router` or add the auth check to the
  custom handler. **WAL gotcha:** two libsql handles on one local file don't
  share writes promptly — in tests, give the DS and clients the *same* log-DB
  handle (Arc clone) or use the same shared test DB for both logical DBs.

---

## 4. Migration + tokens + env

- **`pollis-core/src/db/migrations/000007_commit_log_db.sql`** — `CREATE TABLE`
  the three tables (+ `idx_mls_commit_conv`, `idx_mls_welcome_recip`),
  **no FKs**. This migration is applied to the **log DB**, not the main DB.
- **Do NOT drop** the three tables from the main DB now — old shipped clients
  still read/write them (CLAUDE.md backward-compat rule). Drop is a later phase
  after old-version uptake ages out.
- **`scripts/db-apply.sh`** points at one DB via `TURSO_URL`/`TURSO_TOKEN`. Add a
  **second apply step** in `desktop-release.yml` with the log-DB admin URL/token.
- **Env vars** (consistent with `TURSO_*` naming):
  - `LOG_DB_URL` — log DB URL (client + DS).
  - `LOG_DB_TOKEN` — **read-only** token, embedded in client builds.
  - `LOG_DB_ADMIN_TOKEN` — **read-write** token, migrations + DS only. **Never**
    embed in client builds.
- Doppler `prd_prod` syncs → GH secrets; DS env on the VPS at `/root/ds.env`.

---

## 5. DECISIONS (resolved 2026-06-25)

### OD-1 — How do W4–W8 move behind the DS? → **DS endpoints for all 5.**
Add narrow, auth-required DS endpoints for every one of W4–W8 (full sole-writer
model). W8 (identity-reset welcome delete) is scoped minimally — it overlaps Goal
B's account-lifecycle domain but must move now or the RO token bricks it. See the
**wire contract (§5a)**.

### OD-2 — Cutover / cross-version continuity? → **Fresh start, no backfill.**
The DS points at an **empty** log DB and just starts writing. In-flight
conversations re-sync via MLS **external-join recovery** (#412 — GroupInfo is
published to the log DB on every commit, so a behind client rejoins at the
current epoch). **Consequence: S5 (dual-write + backfill) is CANCELLED.** No
main→log copy, no dual-write window. The only cutover work is ordering/runbook
(S7) + confirming external-join handles an initially-empty log DB.

> Accepted tradeoff: at cutover, active groups experience transient re-sync churn
> as clients external-join. Acceptable per CLAUDE.md bounded-history (no message
> sent while you were a member is lost — external-join catches MLS state up).

---

## 5a. Wire contract for the new DS write endpoints (S4a ↔ S4b agree on THIS)

**Auth (all endpoints, gated by `POLLIS_DS_REQUIRE_AUTH` exactly like `/v1/commits`):**
4 headers `X-Pollis-User` / `X-Pollis-Device` / `X-Pollis-Timestamp` /
`X-Pollis-Signature`; canonical message `{METHOD}\n{PATH}\n{TIMESTAMP}\n{lowercase
hex sha256(body)}`; Ed25519 over it with the device signer
(`commands::mls::device::load_or_create_device_signer`), pubkey =
`user_device.mls_signature_pub` (DS looks this up in the **main** DB, not the log
DB). Reuse `pollis_delivery::auth::verify_request` — it returns the authenticated
`user_id`. **The authenticated user must equal the actor/owner the write targets.**

Client seam pattern (all): mirror `submit_commit` — when
`config.pollis_delivery_url` is `Some` → signed POST to the DS; when `None` →
direct write (current behavior, for tests / pre-cutover). Body is JSON; binary
fields base64 (STANDARD).

| Write | Endpoint | Body | DS action | Authz |
|---|---|---|---|---|
| W4 | `POST /v1/group-info` | `{conversation_id, epoch, group_info(b64), updated_by_device_id}` | UPSERT `mls_group_info` … `ON CONFLICT(conversation_id) DO UPDATE … WHERE excluded.epoch > mls_group_info.epoch` (epoch-monotone, matches `group_state.rs:78`) | authed user is a current member of `conversation_id` |
| W5 | `POST /v1/welcomes/ack` | `{welcome_ids: [string]}` | `UPDATE mls_welcome SET delivered=1 WHERE id IN (…) AND recipient_id = :authed` | authed user owns each welcome (recipient_id) |
| W6/W7 | `POST /v1/welcomes/reset` | `{device_id: string \| null}` | `UPDATE mls_welcome SET delivered=0 WHERE recipient_id=:authed AND (:device_id IS NULL OR recipient_device_id=:device_id OR recipient_device_id IS NULL)` (device_id present → W6 device-scoped; null → W7 all) | recipient_id = authed |
| W8 | `POST /v1/welcomes/purge` | `{}` | `DELETE FROM mls_welcome WHERE recipient_id = :authed` | recipient_id = authed |

W1–W3 stay on the existing `POST /v1/commits` (already atomic). After W5 routes
through the DS, **R6** (`welcomes.rs:90` read) can finally move to `log_db` (the
function no longer needs a same-conn write).

---

## 6. Work breakdown & file ownership (no two streams share a file)

| Stream | Owns (files) | Depends on | Decision-gated? |
|---|---|---|---|
| **S1 — Spine + reads** | `pollis-core`: `config.rs`, `state.rs`, `db/remote.rs`, `bridge.rs`; repoint R1–R6 in `reconcile.rs`, `group_state.rs`, `voice_e2ee.rs`, `welcomes.rs` | — | No (fallback-inert) |
| **S2 — Migration + env** | `db/migrations/000007_commit_log_db.sql`, `.env.example` | — | No |
| **S3 — DS log-DB connection** | `pollis-delivery/*` (connection capability + route W1–W3 writes to log conn, fallback to main) | — | No (capability needed under any OD) |
| **S4a — DS write endpoints** | `pollis-delivery/*` — the 5 endpoints in §5a + shared auth extraction | S3 | builds to §5a |
| **S4b — Client signing + seams** | `pollis-core`: request-signing (4 headers) + DS-write helper; seams W4 (`group_state.rs`), W5 (`welcomes.rs`), W6/W7 (`state.rs`), W8 (`device_enrollment.rs`); repoint R6 | S1 | builds to §5a |
| ~~S5 — Dual-write + backfill~~ | **CANCELLED** by OD-2 (fresh start) | — | — |
| **S6 — Harness + tests** | `src-tauri/tests/flows/harness.rs`, `src-tauri/src/test_harness.rs` — real `build_router` auth, register new endpoints in in-process DS, separate log-DB handle, RO-write-fails acceptance test | S4a, S4b | — |
| **S7 — Release CI + runbook** | `.github/workflows/desktop-release.yml` (2nd migration apply + RO-token build inject), infra runbook | S2 | infra/human |

**Phase 1 (done):** S1, S2, S3.
**Phase 2a (parallel, NOW):** S4a, S4b, S7. **Phase 2b (after 2a):** S6.

---

## 7. Status board

| Stream | Status | Branch / PR | Notes |
|---|---|---|---|
| S1 Spine + reads | ✅ done (uncommitted) | working tree | `log_db` conn + fallback; R1–R5 repointed; R6 deferred (shared read+write conn). Only external `new_with_parts` caller (`flows/harness.rs`) updated. |
| S2 Migration + env | ✅ done (uncommitted) | working tree | 3 tables, no FKs, indexes incl. unique epoch; `.env.example` documents `LOG_DB_URL`/`LOG_DB_TOKEN`/`LOG_DB_ADMIN_TOKEN`. **Relocated** to `pollis-core/src/db/migrations-log/000001_commit_log_db.sql` (own dir/sequence) so `db-apply.sh` provisions the log DB with *only* these 3 tables — not the whole main baseline. Not in the `include_str!` list (never applied to main/local DB). |
| S3 DS log-DB connection | ✅ done (uncommitted) | working tree | `AppState.log_db` (+fallback); `build_router_with_log_db`; submit/commits handlers use log conn; **auth stays on main DB** (`user_device` lives there). `cargo check -p pollis-delivery` clean. |
| S4a DS write endpoints | ✅ done (uncommitted) | working tree | `pollis-delivery/src/writes.rs`: `POST /v1/group-info`, `/v1/welcomes/ack`, `/v1/welcomes/reset`, `/v1/welcomes/purge`; shared `gate()` auth; writes on `log_db`, authz on `db`. (is_member bug fixed by S6.) |
| S4b Client signing + seams | ✅ done (uncommitted) | working tree | `ds_client.rs::ds_post` signs the 4 `X-Pollis-*` headers (= #419 Step 1); `http_submit` now signs too; **W4/W5/W6/W7/W8 all routed via DS**, R6 moved to `log_db`. |
| S8 W6/W7 reroute | ✅ done (uncommitted) | working tree | `load_user_db_with_key` now **returns** `mls_was_empty` (captured before `ensure_device_cert` pollutes `mls_kv`); new `welcomes::reset_welcome_delivery` seam (`POST /v1/welcomes/reset`, direct fallback); `set_pin`/`unlock` call it **after** `ensure_device_cert` so the signed reset authenticates. Verified end-to-end: temp probe showed `DS reset OK 200`; full suite **32 passed / 0 failed** with auth on. |
| ~~S5 Dual-write + backfill~~ | ❎ cancelled (OD-2) | | fresh start, no backfill |
| S6 Harness + tests | ✅ done (uncommitted) | working tree | In-process DS now serves ALL 5 routes (`/v1/commits` + W4–W8) with **auth always enforced** on the shared `RemoteDb` handle; reuses real `pollis_delivery::writes::*` + `auth::verify_request` (approach **b**: pure conn-level fns extracted in `writes.rs`). Commit handler keeps the #411 lost-response fault injection + adds the sig check. Harness flips `accounts.json` `last_active_user` to the acting client before each dispatch so signed DS writes attribute to the right user (multi-user-in-one-process). New `security::ds_rejects_unsigned_or_invalid_writes` acceptance test. **Fixed an S4a bug:** `writes::is_member` missed group-keyed MLS conversations (`group_member.group_id = conversation_id`) — every group GroupInfo republish 403'd. Full flows suite: **32 passed / 0 failed** with auth on. |
| S7 Release CI + runbook | ✅ done (code) / ⏳ infra pending | working tree | `desktop-release.yml`: 2nd `db-apply.sh` step (log DB, `MIGRATIONS_DIR=migrations-log`, admin token) + RO-token injected into all 3 build jobs (admin token never in builds). `docs/goal-a-deploy-runbook.md` written. **Infra/human steps remain** (create Turso log DBs, mint tokens, Doppler→GH secrets, point DS at log DB, redeploy DS, ship release, flip RO token LAST). |
| S9 Transparency reader repoint (#422) | ✅ done (uncommitted) | working tree | **Audit blind spot:** the `verifiable-log` builder/serve read `mls_commit_log` directly (source of verify.pollis.com's *group* transparency log) — cutover would freeze it. Fix: `source::connect_with_token` (per-DB token); builder reads `mls_commit_log` from the **log DB**, `account_key_log` from **main**; `serve live` repointed; `docker-entrypoint.sh` + `transparency-publish.yml` get `LOG_DB_URL`/`LOG_DB_AUTH_TOKEN` (fallback to main when unset). Makes the guarantee *stronger* — attests the DS-only-written copy. `cargo test -p verifiable-log-builder -p verifiable-log-serve` 47/0. **Must ship before prod cutover.** |

### Provisioned (dev)
- **`pollis-log-dev`** Turso DB created (group `pollis-dev`), migrated to v1 (only the 3 tables). RO + admin tokens minted (in session scratch). DS not yet repointed (Phase 2 paused pending S9 review).

### Remaining before #420 is fully shipped
- **All code is done** (S1–S9). All 8 writes (W1–W8) route through the DS; the transparency builder/serve read the commit log from the log DB.
- **Infra cutover** per `docs/goal-a-deploy-runbook.md` (human/CI): create Turso log DBs, mint RO/RW tokens, Doppler→GH secrets (incl. `LOG_DB_URL`/`LOG_DB_AUTH_TOKEN` for the transparency publisher), point DS at the log DB + redeploy, repoint the transparency publisher, ship a desktop release, flip the client RO token **last**.
- Optional: turn `POLLIS_DS_REQUIRE_AUTH=true` in dev DS now that the harness proves signing works end-to-end.

---

## 8. Ordering & hazards

1. **RO token last.** Don't ship the client's read-only log-DB token until the DS
   writes the log DB *and* all 8 writes route through it — else every write bricks.
2. **DS before client reads.** The client must not read the log DB before the DS
   has populated it (backfill + dual-write), or in-flight conversations read empty.
3. **RO vs admin token.** Embed only `LOG_DB_TOKEN` (read-only) in client builds;
   `LOG_DB_ADMIN_TOKEN` (read-write) is migrations + DS only.
4. **Additive only.** Create tables in the log DB; never drop from the main DB in
   this phase.
5. **Test WAL sharing.** Same log-DB handle for DS + clients in the harness.
6. **Transparency publisher before/at cutover (#422).** The `verifiable-log`
   builder/serve must be pointed at the log DB (`LOG_DB_URL`/`LOG_DB_AUTH_TOKEN`,
   read token) before the DS starts writing commits there — else verify.pollis.com's
   group/commit transparency log silently freezes at the cutover point.
   `account_key_log` stays on the main DB. (Code: S9, done.)
