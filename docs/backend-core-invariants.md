# Backend Core Invariants — "Invalid States Are Unrepresentable"

> **This is the governing principle for all backend-core development** (`pollis-core`,
> the remote schema, the MLS/delivery/retention layers). Read it before changing
> anything that touches message delivery, MLS group state, or retention.

## The principle

Every piece of backend-core state that affects **message delivery** or **group
membership** must be modeled so that an invalid configuration **cannot be
expressed**. We enforce invariants at the *lowest possible layer*, in this order
of preference:

1. **Schema / DB constraints & triggers** — the state physically can't exist.
2. **Rust type system** — the invalid value can't be constructed.
3. **Protocol logic** — a single, audited chokepoint.
4. **Code discipline** — last resort, and only when backed by a test that
   encodes the invariant.

A correctness property defended only by "every caller remembers to do X" is a
bug waiting to happen. If we find ourselves relying on discipline, that's a
signal we modeled the state wrong.

## The guarantee we are engineering for

> A member added to a conversation at epoch *N* must — no matter how long they
> are away, how many epochs pass, or how many members are added/removed in the
> meantime — be able to:
> 1. **Reach the current epoch** by replaying every commit *N → current* (the
>    commit chain has no gaps, ever), and
> 2. **Receive and decrypt every message** sent in any epoch they were a member.

The only history that is ever lost (the *bounded-history* caveats, unchanged):
- **(a)** Messages sent **before you joined** the MLS tree (a cryptographic
  property of MLS).
- **(b)** A **brand-new device** for an existing user starts empty (no key
  backup / Megolm).

Everything else must be delivered. "Come back to a group you joined 4 years and
300 commits ago, with dozens of adds/removals since, and receive every message"
is the literal acceptance test.

## Failure taxonomy — invalid states that are *currently representable*

Each of these is a state the system can presently get into. The fix column is
the invariant that makes it unrepresentable.

| # | Invalid state | Source (today) | Evidence |
|---|---|---|---|
| F1 | **Commit-log gap** (non-contiguous epoch) | `mls_commit_log` has `UNIQUE(conv, epoch)` but *no contiguity constraint*; inserts can skip | ELECTRON epochs 4, 5, **11** missing → `ants` wedged |
| F2 | **Deletion of an applied commit** | `process_pending_commits` used to `DELETE` a "revoked self-add" row (fixed in `adfe518`, but not *forbidden*) | dan applied epoch 11, it was deleted, ants wedged |
| F3 | **Message dropped before delivery** | `ingest.rs` cleanup: `sent_at < now-30d` **OR** all-devices-caught-up. The **30-day TTL drops undelivered messages** | direct data loss for any member absent > 30d |
| F4 | **Fragile delivery accounting** | delivery tracked by `sent_at` vs per-device `conversation_watermark` timestamp — clock skew, equal timestamps, coarse acks | timestamp ≠ a reliable cursor |
| F5 | **Welcome dropped before delivery** | `mls_welcome.delivered` flag + delete paths; no retention floor | a new device can miss its only Welcome |
| F6 | **Retention ignores absent members** | floor is computed from a snapshot and gated by a TTL, not the *true* slowest member-device cursor | F3's TTL is the leak |
| F7 | **Schema divergence: test vs prod** | two apply paths — test harness uses `POST_BASELINE_MIGRATIONS` on a *fresh* DB; prod uses `db-apply.sh` (version-tracked) on the *long-lived* DB. Version numbers collide with the old lineage | prod missing `000005_account_key_log`, `000006_push_token` while all tests pass |

## Target invariants & where they're enforced

### I1 — The commit log is a gapless, append-only, single-writer-per-epoch chain
- `UNIQUE(conversation_id, epoch)` (have it) → one writer per epoch.
- **`BEFORE INSERT` trigger**: reject `epoch > MAX(epoch)+1` for the
  conversation → can't skip ahead (kills F1). Compatible with the existing
  CAS (`ON CONFLICT(conv,epoch) DO NOTHING`): a stale committer inserts at
  `epoch ≤ max` and is absorbed by the conflict; only forward gaps abort.
- **`BEFORE DELETE` trigger**: abort all deletes → append-only (kills F2).
- **`BEFORE UPDATE` trigger**: abort changes to `epoch`/`commit_data` →
  immutable history.
- *Result:* a gap or a deleted/edited commit is **physically unrepresentable.**

### I2 — Commits are a verifiable chain
- MLS already chains epochs via the confirmed transcript hash / confirmation
  tag. Persist the prior-epoch link alongside each commit and verify linkage on
  apply (belt-and-suspenders over I1). A commit that doesn't extend the head is
  rejected, not stored.

### I3 — Delivery is a monotonic per-(member-device) cursor; retention is bounded by the slowest member, never a TTL
- Replace `sent_at` watermarks with a **monotonic per-conversation message
  sequence** + a **per-(user,device) cursor** ("consumed up to seq K").
- A message row is retained until **every current member-device's cursor has
  passed it**. **No TTL** ever deletes an unconsumed message (kills F3, F4, F6).
- A member who *leaves* releases their hold (they're no longer current); a
  *new* member starts at their join epoch (caveat (a) preserved).
- *Result:* "message deleted before a current member received it" is
  unrepresentable.

### I4 — Commits and Welcomes are retained until every member-device has consumed them
- Same floor as I3 applied to `mls_commit_log` and `mls_welcome`: never prune
  below the slowest member-device's applied epoch / delivered cursor (kills
  F5). Commit-log pruning is already disabled; this *formalizes* the floor so
  it can't be reintroduced unsafely.
- *Result:* "a returning member can't reach an epoch / get their Welcome" is
  unrepresentable.

### I5 — Historical membership is derivable, not guessed
- Retention and "who must receive epoch E" derive from the MLS tree /
  membership log, not from a mutable current-roster snapshot. The set of
  member-devices holding the retention floor is always exactly the current
  members behind the cursor.

### I6 — One schema, one apply path
- Eliminate the test/prod divergence (F7): a single source of truth for the
  schema and a single, version-correct apply path used by **both** the test
  harness and prod. New migrations are numbered to continue past the live DB's
  max version (see root `CLAUDE.md`), and ordering/contiguity of `schema_migrations`
  is enforced. A migration that the test path applies but the prod path skips
  must be impossible.

## Enforcement layers, summarized

| Invariant | DB constraint/trigger | Rust type | Protocol | Test |
|---|---|---|---|---|
| I1 gapless append-only log | ✅ primary | append-only chain type | CAS insert | multi-client gap/laggard |
| I2 verifiable chain | link column | typed `CommitChain` | verify-on-apply | tamper test |
| I3 cursor delivery | seq + cursor tables; retention via floor | monotonic `Cursor` | ingest advances cursor | absent-member catch-up |
| I4 retain-to-slowest | retention floor (no TTL) | — | GC reads floor | 300-behind catch-up |
| I5 historical membership | — | — | derive from tree | churn + catch-up |
| I6 one schema | `schema_migrations` integrity | — | single apply path | test==prod schema check |

## Roadmap (phased)

- **Phase 0 (done):** stop the bleeding — append-only in code (`adfe518`: no
  more commit-log deletes) + auto-heal wedged members via external-join.
- **Phase 1:** I1 — DB triggers making the commit log gapless/append-only/
  immutable. + the regression test that was missing (laggard + apply-then-mutate).
- **Phase 2:** I3 — delivery cursor model (monotonic seq + per-device cursor),
  delete the 30-day TTL, retention bound to the slowest member-device.
- **Phase 3:** I4/I5 — retention floor for commits + welcomes; historical
  membership derivation; the "300 commits behind, 4 years later" acceptance test.
- **Phase 4:** I6 — collapse the dual schema-apply paths into one; fix the
  migration-version collision; assert test schema == prod schema in CI.
- **Phase 5:** I2 + types — model the chain and cursors as append-only Rust
  types; property/multi-client tests per invariant.

## Test doctrine (why "all green" kept lying to us)

The suite tested the *happy replay* of an append-only, consistent log. It never
modeled the operations that *break* the invariant: a row deleted after another
member applied it; a laggard reading a mutated log; the prod schema diverging
from the test schema. **Every invariant above must have a test that tries to
create the invalid state and asserts it can't** — not just a test that the valid
path works. Coverage of the happy path is not coverage of the invariant.
