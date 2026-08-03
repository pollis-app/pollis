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
| F1 | ~~**Commit-log gap** (non-contiguous epoch)~~ — **schema-guarded (#691)** | `UNIQUE(conv, epoch)` had *no contiguity constraint*; inserts could skip. The `BEFORE INSERT` forward-gap trigger (migration 000005) now aborts `epoch > MAX(epoch)+1` | ELECTRON epochs 4, 5, **11** missing → `ants` wedged |
| F2 | ~~**Deletion/rewrite of an applied commit**~~ — **schema-guarded (#691)** | client deletes closed by the #420 read-only token split; DS-side now blocked by the `BEFORE UPDATE` immutability trigger + the `BEFORE DELETE` head-protection trigger (migration 000005). Interior-gap deletes remain a Rust-layer concern — see the I1 scope note | dan applied epoch 11, it was deleted, ants wedged |
| F3 | ~~**Message dropped before delivery**~~ — **FIXED** | was `sent_at < now-30d` **OR** all-devices-caught-up; the OR'd TTL deleted undelivered envelopes on its own. TTL arm removed — envelope GC is now gated on the all-member-devices watermark alone (`pollis-delivery/src/messages.rs`) | was direct data loss for any member absent > 30d. Regressions pinned in `messages::gc_sql_tests`, `pollis-delivery/tests/envelope_retention.rs` (Part E), `flows::messages` |
| F4 | **Fragile delivery accounting** | delivery tracked by `sent_at` vs per-device `conversation_watermark` timestamp — clock skew, equal timestamps, coarse acks | timestamp ≠ a reliable cursor (**still open** — the monotonic-seq cursor of I3 is not yet built; the F3 fix bounds retention by that timestamp watermark) |
| F5 | **Welcome dropped before delivery** | `mls_welcome.delivered` flag + delete paths; no retention floor | a new device can miss its only Welcome |
| F6 | ~~**Retention ignores absent members**~~ — **FIXED for envelopes** | the envelope floor is the MIN `last_fetched_at` over every current member device (revoked devices excluded #685, devices silent past N months excluded #720), no longer gated by a TTL | F3's TTL was the leak; it is gone. The #720 liveness bound adds a *third* accepted loss (a device dormant past the window) — see I3. Commit/Welcome floors (I4) tracked separately |
| F7 | **Schema divergence: test vs prod** | two apply paths — test harness uses `POST_BASELINE_MIGRATIONS` on a *fresh* DB; prod uses `db-apply.sh` (version-tracked) on the *long-lived* DB. Version numbers collide with the old lineage | prod missing `000005_account_key_log`, `000006_push_token` while all tests pass |

## Target invariants & where they're enforced

### I1 — The commit log is a gapless, append-only, single-writer-per-epoch chain
- `UNIQUE(conversation_id, generation, epoch)` (have it; `generation` added by
  #454 P4, every pre-existing row generation 0) → one writer per epoch, per
  suite lineage.
- **`BEFORE INSERT` trigger**: reject `epoch > MAX(epoch)+1` for the
  conversation → can't skip ahead (kills F1). Compatible with the existing
  CAS (`ON CONFLICT(conv,epoch) DO NOTHING`): a stale committer inserts at
  `epoch ≤ max` and is absorbed by the conflict; only forward gaps abort.
- **`BEFORE DELETE` trigger**: reject deleting the **live head**
  (`MAX(epoch)` of `MAX(generation)`) **while any earlier commit of the same
  conversation still exists** → the ELECTRON deletion that wedges every member
  (head regressed out from under members who had applied it) is unrepresentable
  (kills the catastrophic half of F2). Deleting the head when it is the *last*
  row is allowed — an empty chain is trivially gapless (see the scope note).
- **`BEFORE UPDATE` trigger**: abort changes to
  `conversation_id`/`generation`/`epoch`/`commit_data` → immutable history
  (kills the rewrite half of F2).
- *Result:* a forward gap, a rewritten commit, or a head deleted while earlier
  commits remain is **physically unrepresentable.**
- **Shipped:** commit-log-DB migration `000005_mls_commit_log_triggers.sql`
  (#691), with direct-SQL violation tests in
  `pollis-delivery/tests/commit_log_triggers.rs`.
- **Scope note (honest, from #691).** The doc originally specified a
  `BEFORE DELETE` that "aborts all deletes". That is now **infeasible**: the I4
  retention prune (`delete_commits_below` / `delete_generations_below`, #539)
  legitimately deletes a *prefix* / whole *closed lineage*, and it post-dates
  this section. The DELETE trigger therefore guards the head — invariant under
  both prune shapes, hence order-independent and provably free of false
  positives — rather than aborting outright. Preventing an *interior* gap on
  delete is not expressible in a per-row SQLite `BEFORE DELETE` trigger without
  false-positiving on the intermediate state of a legitimate bulk prefix prune
  (a partly-truncated prefix is indistinguishable from a mid-chain hole), so
  interior contiguity stays enforced by the CAS + the clamped retention floor in
  Rust. The schema layer guarantees the three that actually wedge a group:
  no forward gap, no rewrite, no head-regression-while-members-hold-it.
- **Full-conversation erasure IS allowed (corrected in #691 review — an earlier
  draft of this section wrongly claimed the head guard made erasure impossible).**
  The guard fires only when the head is deleted *while an earlier commit of the
  same conversation still exists* — the ELECTRON shape. It does **not** fire on
  deleting the head when it is the last row, so a conversation can be emptied:
  prune bottom-up (each step the lowest epoch; the final row is head-and-only-row →
  allowed), or issue a bare `DELETE FROM mls_commit_log` (SQLite visits rows in
  ascending `seq`/rowid order, and the head has the highest `seq` in its
  conversation, so it is deleted last, after its siblings are gone). This is not
  hypothetical: the test-harness `wipe_remote` reset issues exactly that bare
  `DELETE`, and the *first* draft of the guard took down the entire flows suite
  (81/82 scenarios) because it forbade the wipe. A future delete-account /
  erase-conversation path therefore needs **no** bypass primitive — it just
  deletes, and the guard stays out of the way. The stated cost: buggy code that
  sweeps an entire *live* conversation's rows is no longer stopped at the schema
  layer, which is acceptable — it is whole-conversation removal, a different
  operation from I1's head-regression failure mode, and nothing above the DB
  issues an unscoped per-conversation delete (the only production deletes are the
  two prefix/generation retention shapes; account deletion in `account.rs` never
  touches `mls_commit_log`). The two boundary cases are pinned by
  `bulk_wipe_empties_the_table` / `delete_head_when_only_row_is_allowed` in
  `commit_log_triggers.rs`.

### I2 — Commits are a verifiable chain
- MLS already chains epochs via the confirmed transcript hash / confirmation
  tag. Persist the prior-epoch link alongside each commit and verify linkage on
  apply (belt-and-suspenders over I1). A commit that doesn't extend the head is
  rejected, not stored.

### I3 — Delivery is a monotonic per-(member-device) cursor; retention is bounded by the slowest *live* member, never a TTL
- Replace `sent_at` watermarks with a **monotonic per-conversation message
  sequence** + a **per-(user,device) cursor** ("consumed up to seq K").
- A message row is retained until **every current member-device's cursor has
  passed it**. **No TTL** ever deletes an unconsumed message (kills F3, F4, F6).
- A member who *leaves* releases their hold (they're no longer current); a
  *new* member starts at their join epoch (caveat (a) preserved).
- **Device-liveness bound (#720).** Retention is additionally bounded by device
  *liveness*: a member device that has not **reported** a watermark within a
  configured window (`POLLIS_DS_WATERMARK_STALE_MONTHS`, default 6 months) stops
  pinning and is excluded from the floor, mirroring the revoked-device exclusion
  (#685). The bound is on the device's own **last report**
  (`conversation_watermark.reported_at`, a server-stamped wall-clock time),
  **never on the message's age or the cursor value** — that distinction is what
  keeps it from being the F3 TTL: a device that reported yesterday pins every
  envelope below its cursor, however ancient. Coming back and reporting makes the
  device live again immediately (reversible; it does not revoke the device or
  remove it from any roster). Enforced in the SQL predicate
  (`pollis-delivery/src/messages.rs` `CLEANUP_*` `?2` arm), NOT the shared roster
  macro, so envelope-GC and commit-log rosters stay in parity (I5) while envelope
  retention carries this extra, envelope-only bound.
- *Result:* "message deleted before a current *live* member received it" is
  unrepresentable. The one thing this adds is a **third accepted loss** (beyond
  pre-join history and a new empty device): a device dormant past the window that
  later returns may find gaps — a deliberate storage/correctness trade held behind
  a conservative window whose *value* is the owner's product call
  (`docs/metadata-retention-policy.md` §1).

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
| I1 gapless append-only log | ✅ triggers (#691, migration 000005) | append-only chain type | CAS insert | direct-SQL violation (`commit_log_triggers.rs`) |
| I2 verifiable chain | link column | typed `CommitChain` | verify-on-apply | tamper test |
| I3 cursor delivery | seq + cursor tables; retention via floor | monotonic `Cursor` | ingest advances cursor | absent-member catch-up |
| I4 retain-to-slowest | retention floor (no TTL) | — | GC reads floor | 300-behind catch-up |
| I5 historical membership | — | — | derive from tree | churn + catch-up |
| I6 one schema | `schema_migrations` integrity | — | single apply path | test==prod schema check |

## Roadmap (phased)

> Tracked in **[#397](https://github.com/actuallydan/pollis/issues/397)**.


- **Phase 0 (done):** stop the bleeding — append-only in code (`adfe518`: no
  more commit-log deletes) + auto-heal wedged members via external-join.
- **Phase 1 (done, #691):** I1 — DB triggers making the commit log
  gapless/immutable and protecting the live head *against a regression while
  earlier commits remain* (migration `000005`, log DB) + the direct-SQL violation
  tests (`pollis-delivery/tests/commit_log_triggers.rs`). "Append-only" is enforced as
  head-protection rather than abort-all-deletes, because the I4 retention prune
  legitimately truncates a prefix — see the I1 scope note above.
- **Phase 2:** I3 — **partially done.** The 30-day TTL is deleted and envelope
  retention is bound to the slowest member-device (kills F3, and F6 for
  envelopes). The delivery cursor model itself (monotonic per-conversation seq +
  per-device cursor, replacing the `sent_at` timestamp watermark) is **still
  outstanding** — F4 remains open.
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
