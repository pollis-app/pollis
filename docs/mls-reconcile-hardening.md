# MLS Reconcile Hardening — Runbook / Handoff Spec

> Self-contained spec to make Pollis's MLS group state **deterministic and
> bulletproof** regardless of which users act, their network state, the order
> things happen, or how long passes between actions. Written so another
> session/machine can execute it without re-discovering context. Governed by
> [`backend-core-invariants.md`](backend-core-invariants.md) ("invalid states
> are unrepresentable"). Tracked in **#397**.

## Goal (the property we must guarantee)

For any conversation, across any number of clients/devices, **any** network
ordering, **any** interleaving of actions, and **any** delay between them:

1. There is exactly **one** canonical MLS commit history (a gapless, linear
   chain of epochs). No forks, ever.
2. Every current member-device can **reach the head epoch** by replaying that
   chain (or recovering to it), no matter how far behind it starts.
3. Reconcile (membership add/remove, device add/remove, key rotation) is a
   **pure function of observable server state** — running it from any client,
   any number of times, in any order, converges to the same result.

Non-goal here (explicitly deferred — do NOT touch): message delivery,
`conversation_watermark`, the 30-day envelope TTL, envelope GC / envelopes
piling up. We may redesign delivery later. Crypto + commit-log correctness
only. No new features — `pollis-core` fixes only.

## What is already done (Phase 0, in `main`)

- `adfe518` — **commit log is append-only in code**: removed the
  `DELETE FROM mls_commit_log` "revoked self-add" path (it deleted canonical
  commits and wedged laggards: ELECTRON epoch 11). Revoked adds are now
  *applied* (stay on the canonical branch); reconcile evicts the device via an
  append-only remove. Plus **auto-heal**: on an epoch gap,
  `process_pending_commits` drops the stale local group so the existing
  external-join recovery rejoins at the head epoch instead of stopping forever.
- All 29 `flows` integration tests pass with this.

## Code map (where the logic lives)

- `pollis-core/src/commands/mls/group_state.rs`
  - `process_pending_commits_locked` — fetch `mls_commit_log` rows `epoch >=
    local`, verify added-device certs, apply in order, advance epoch. Gap →
    drop local group → external-join recovery (post-loop). **The hot path.**
  - `external_join_group_inner` / `external_join_attempt` — rejoin via external
    commit against published `mls_group_info`, CAS the target epoch
    (`ON CONFLICT(conv,epoch) DO NOTHING`), discard + retry on lost race.
  - `forget_local_mls_group`, `verify_added_devices` (→ `VerifyOutcome`).
- `pollis-core/src/commands/mls/reconcile.rs` — `reconcile_group_mls_*`: roster
  diff (group_member / dm_channel_member + pending invitees), TOFU-pin
  `account_id_pub`, claim KPs for devices not in the tree, drop leaves whose
  `user_device` row is revoked, stage commit, **post to Turso FIRST then merge
  locally** (remote failure must not advance local epoch).
- `pollis-core/src/commands/mls/sweep.rs` — `catch_up_all_mls_groups`: per-group
  `process_pending_commits` on launch/reconnect.
- Tables: `mls_commit_log` (`UNIQUE(conversation_id, epoch)` = the CAS that
  serializes commits — **present and verified in prod**), `mls_group_info`
  (external-join source), `mls_welcome`, `mls_key_package`, `user_device`
  (`revoked_at` tombstone).

## Invariants to enforce (and where)

### INV-1 — commit log is gapless, append-only, immutable (DB-enforced)
Code discipline (Phase 0) is not enough; make it physical:
- `BEFORE INSERT` trigger: reject `NEW.epoch > COALESCE(MAX(epoch),-1)+1` per
  conversation. (CAS-compatible: a stale committer inserts at `epoch <= max`
  and is absorbed by the `UNIQUE` conflict; only forward gaps abort.)
- `BEFORE DELETE` trigger: abort all deletes.
- `BEFORE UPDATE` trigger: abort changes to `epoch` / `commit_data` /
  `conversation_id`.
- Ships as migration `000007_mls_commit_log_immutable.sql` (prod is at v5, v6
  pending — **000007 is the correct next number, no collision**). Also add to
  `POST_BASELINE_MIGRATIONS` in `pollis-core/src/db/mod.rs` so the test harness
  gets it. → a gap/delete/rewrite becomes physically impossible.

### INV-2 — verify-on-apply chain (type + protocol)
MLS already chains epochs (confirmed transcript hash / confirmation tag);
`process_message` rejects a commit that doesn't extend the local head. Make the
reliance explicit and tested. Optionally persist the prior-epoch link and
assert it on insert as defense in depth.

### INV-3 — reconcile is deterministic & idempotent
- Pure function of `(roster, user_device, mls tree, key packages)` → a single
  staged commit. Same inputs, same output; re-running is a no-op once the tree
  matches the roster.
- All mutations serialized per conversation by `state.mls_group_lock` (held for
  the whole reconcile) — already the case; keep it.
- Remote-first, merge-second ordering (already present) so a remote failure
  never advances local epoch (no split-brain).
- Lost-CAS-race → discard local staged commit, re-process the winner, retry
  from the new head. Bounded retries.

### INV-4 — recovery is total
Every way a device can fall off the canonical branch resolves to "external-join
to head" (not "stop"): epoch gap (Phase 0 ✓), eviction (✓), apply-fail/fork
(✓). Audit that there is **no** remaining `break`-without-recovery in
`process_pending_commits_locked` (the `AbsentRetry` self-add defer is the one
intentional non-recovery — see edge cases).

## Edge-case matrix (each needs a test that creates the bad state and proves it can't persist)

| Case | Required outcome |
|---|---|
| Two members commit at the same epoch (race) | UNIQUE CAS: one wins, the loser discards + re-applies winner. No fork. |
| Stale committer (behind head) commits | Inserts at `epoch <= head` → conflict → catches up. No gap. |
| Revoked device self-adds (malicious) | Applied (append-only), then evicted by reconcile remove. Brief presence, never a wedge. Full close = creds server (below). |
| Device legit at commit, revoked later; a laggard re-evaluates | Laggard applies the canonical commit; later remove evicts. No delete, no wedge. (Phase 0 ✓ — keep the regression test.) |
| Member 300 commits / N years behind comes back | Replays the full gapless chain to head, or external-joins to head. Reaches current epoch. |
| Welcome never delivered before device acts | Device external-joins from `mls_group_info` (no Welcome needed). |
| Out-of-order commit arrival | Apply strictly in `epoch ASC, seq ASC`; gap → recover. |
| `mls_group_info` stale/absent during external-join | Retry with backoff; if absent, stay out cleanly (no squat). |
| Concurrent reconcile on two devices | `mls_group_lock` + CAS serialize; one wins, other re-syncs. |

## Test plan (the doctrine: prove the invalid state can't persist)

- Extend `src-tauri/tests/flows/` with multi-client scenarios for every row
  above, driven through the real command path (no mocks).
- **Adversarial ordering harness**: a helper that applies a generated sequence
  of (client, action) steps in randomized/interleaved order and asserts
  convergence + head-reachability for every client at the end. Seeded so
  failures reproduce. This is what catches "any order, any timing."
- Address the known harness limit (one local DB per `user_id` → true
  intra-user multi-device is under-tested): give each device its own
  `InMemoryKeystore` + local DB keyed by `(user_id, device_id)`.
- Every fix lands with a test that *constructs the invalid state and asserts it
  cannot persist* — happy-path coverage is not invariant coverage.

## Verification (use what we built)

- `cargo test --features test-harness --test flows` — the integration gate.
- Wire/confirm a CI gh action runs the flows suite on PRs to `main` (the
  release path is `desktop-release.yml`; the test gate should be its own action
  on PR). `verifier-release.yml` + the `verifiable-log*` crates are the
  account-key transparency auditor — orthogonal, but the same "verifiable,
  append-only log" discipline applies to `mls_commit_log`.

## Conditional: authorized-creds API server (the security close)

Strictly required only to fully close **malicious/revoked clients writing bad
commits** (a custom binary holding the shared Turso write token can post forks /
revoked self-adds; INV-1 stops gaps/forks structurally, but a malicious *valid*
write is only stopped at the source). NOT required for determinism under honest
clients. Build it if/when we want that close (the user OK'd ignoring the
"no server" invariant for this):

- **What:** a tiny, fast credential broker. Holds the real secrets; clients
  never do. Endpoints: `POST /otp/request` + `/otp/verify` (server calls
  Resend), `GET /db/token` (mint **per-user, short-TTL, scoped** Turso creds —
  blast radius shrinks from "whole DB forever" to "this user, minutes";
  revoked device → no token → cannot write), `GET /livekit/token`,
  `GET /r2/presign`.
- **Where:** `api.pollis.com`. Deployed alongside but unrelated to the rtc
  server. Cloudflare-native + tiny + Dockerized:
  - Recommended: **Cloudflare Workers** (+ Durable Objects for OTP state, or
    Workers KV) — smallest/fastest, no container needed; OR
  - If a Docker artifact is mandated: a minimal Rust (axum) image deployed to a
    Cloudflare **Container**, same code shape as `verifiable-log-serve`.
- This also resolves the mobile `EXPO_PUBLIC_*` secret-inlining issue (the
  Turso/LiveKit/Resend secrets currently ship in the JS bundle).

## Infra facts to remember (discovered, easy to trip on)

- **Two DBs:** `dev` = `libsql://dev-actuallydan…` (group `pollis-dev`, old
  1–18 migration lineage, messy — disposable) and `prod` =
  `libsql://prod-actuallydan…` (group `pollis-prod`, clean `0–5`, `push_token`
  pending). `pollis-test` group has `test` + `pollis-test-actuallydan`.
- **Both `.env.development` AND `.env.production` point at the `dev` DB**, and
  `.env.production`'s `TURSO_TOKEN` is **malformed** (invalid `data_insert`
  permission claim → Turso rejects it). Fix: point `.env.production` at
  `prod-actuallydan` with a valid token (or rely on Doppler/GH secrets, which
  already correctly drive `db-apply` against real prod).
- Clients (mobile/desktop) currently use the **dev** DB via `.env`. The dev DB
  is missing `account_key_log` and carries the old lineage; **prod is the
  correct one.** Repoint clients at prod when you care about real data.
- Prod DB writes / migrations: use the **turso CLI** (`turso db shell prod
  "…"`) — it's authed; the `.env.production` token is not usable.
- Release pipeline: `.github/workflows/desktop-release.yml` runs
  `scripts/db-apply.sh` on tag push (Tauri). `electron-release.yml` is disabled.
  `db-apply` already applied `0–5` to real prod; `push_token` deploys next
  release.
- **Next migration number is `000007`** (prod head is 5, 6 pending).

## Execution order

1. **INV-1** migration `000007` (commit-log immutability triggers) + add to
   `POST_BASELINE_MIGRATIONS` + the apply-then-mutate / laggard regression test.
2. **INV-4** audit + **INV-3** determinism/idempotency review + tests.
3. **Adversarial ordering harness** + the full edge-case matrix as tests.
4. **INV-2** explicit verify-on-apply (+ optional persisted chain link).
5. Confirm CI gh action runs the flows gate on PRs.
6. (Conditional) the `api.pollis.com` creds broker.
