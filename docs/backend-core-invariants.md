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

The only history that is ever lost (the *bounded-history* caveats):
- **(a)** Messages sent **before you joined** the MLS tree (a cryptographic
  property of MLS).
- **(b)** A **brand-new device** for an existing user starts empty (no key
  backup / Megolm).
- **(c)** *(new in #720)* A device whose **every** copy has been silent for longer
  than `POLLIS_DS_WATERMARK_STALE_MONTHS` (**12 months**, configured) stops pinning
  envelope retention, so mail that was waiting only for that device may be
  collected before it returns. Unlike (a) and (b) this one is a **policy** choice,
  not a cryptographic property — it exists because retention with no bound at all
  is unbounded storage growth. It is keyed on the device's last *report*, never on
  the message's age, so it is not a TTL; reporting again makes the device live
  immediately. Rationale for 12: `docs/metadata-retention-policy.md` §1.

Everything else must be delivered. "Come back to a group you joined 4 years and
300 commits ago, with dozens of adds/removals since, and receive every message"
is the literal acceptance test — and note (c) does **not** weaken it, because that
test describes a device that comes *back*, reports, and is live again. (c) bites
only when a device is gone for over a year and mail was waiting solely on it.

## Failure taxonomy — invalid states that are *currently representable*

Each of these is a state the system can presently get into. The fix column is
the invariant that makes it unrepresentable.

| # | Invalid state | Source (today) | Evidence |
|---|---|---|---|
| F1 | ~~**Commit-log gap** (non-contiguous epoch)~~ — **schema-guarded (#691)** | `UNIQUE(conv, epoch)` had *no contiguity constraint*; inserts could skip. The `BEFORE INSERT` forward-gap trigger (migration 000005) now aborts `epoch > MAX(epoch)+1` | ELECTRON epochs 4, 5, **11** missing → `ants` wedged |
| F2 | ~~**Deletion/rewrite of an applied commit**~~ — **schema-guarded (#691)** | client deletes closed by the #420 read-only token split, and structurally since #987 (the client has no database credential and `pollis-core` does not link `libsql`); DS-side blocked by the `BEFORE UPDATE` immutability trigger + the `BEFORE DELETE` head-protection trigger (migration 000005). Interior-gap deletes remain a Rust-layer concern — see the I1 scope note. An interior gap that DOES occur is now read atomically with the head and reported to the client as `ConversationState::hole`, so its recovery cannot be triggered by a torn read (#987) | dan applied epoch 11, it was deleted, ants wedged |
| F3 | ~~**Message dropped before delivery**~~ — **FIXED** | was `sent_at < now-30d` **OR** all-devices-caught-up; the OR'd TTL deleted undelivered envelopes on its own. TTL arm removed — envelope GC is now gated on the all-member-devices watermark alone (`pollis-delivery/src/messages.rs`) | was direct data loss for any member absent > 30d. Regressions pinned in `messages::gc_sql_tests`, `pollis-delivery/tests/envelope_retention.rs` (Part E), `flows::messages` |
| F4 | **Fragile delivery accounting** | delivery tracked by `sent_at` vs per-device `conversation_watermark` timestamp — clock skew, equal timestamps, coarse acks | timestamp ≠ a reliable cursor (**still open** — the monotonic-seq cursor of I3 is not yet built; the F3 fix bounds retention by that timestamp watermark) |
| F5 | **Welcome dropped before delivery** | `mls_welcome.delivered` flag + delete paths; no retention floor | a new device can miss its only Welcome |
| F6 | ~~**Retention ignores absent members**~~ — **FIXED for envelopes** | the envelope floor is the MIN `last_fetched_at` over every current member device (revoked devices excluded #685, devices silent past N months excluded #720), no longer gated by a TTL | F3's TTL was the leak; it is gone. The #720 liveness bound adds a *third* accepted loss (a device dormant past the window) — see I3. Commit/Welcome floors (I4) tracked separately |
| F8 | **One id naming two conversations** — **unrepresentable (#880 + #948)** | `dm_channel`, `groups` and `channels` are three tables with three separate primary keys, so the same id fits in all three, and `is_member` ORs across all three on one id. #879 added the `conversation_id_taken` chokepoint; #880 added the `conversation` registry, claimed in the same transaction as the row it names, so a second claim cannot commit *through the DS*; #948 (migration 000017) added guard triggers so the database itself refuses a row the registry did not grant — a writer that skips the registry is now refused too | a DM named after a victim's group made `is_member(victim_group, attacker)` true — commit injection, GroupInfo/Welcome overwrite, a LiveKit token for their room. Pinned in `pollis-delivery/tests/conversation_namespace.rs`, including raw-SQL refusal with the DS bypassed |
| F9 | ~~**Envelope stored at an epoch the log has closed**~~ — **unrepresentable (#1041)** | the pre-op catch-up and the op are two steps; an envelope sealed at epoch *e* after a committer's catch-up but before its commit merged was undecryptable once it merged (`max_past_epochs = 0`) and marked handled — silent loss; one sealed after the commit landed was readable by nobody. The DS now keeps an envelope only if its asserted `(generation, epoch)` is the log head (409 `epoch_behind`), the committer sweeps the closing epoch before it merges, and refused senders re-seal — see I8 | the `ui_e2e.rs` flake on CI. Pinned in `pollis-delivery` `epoch_gate_tests` and `flows::dms::message_sealed_*` (both fail with the gate and sweep disabled) |
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
  issues an unscoped per-conversation delete *for a conversation that still
  exists* (the retention deletes are the two prefix/generation shapes; the only
  whole-conversation delete is `teardown::purge_conversation_log`, which runs
  exclusively on ids the same operation just destroyed, and account deletion
  never removes a departed member's commits from a conversation that survives —
  that is a documented exemption in `teardown.rs`, because doing so would gap the
  chain for every remaining member). The two boundary cases are pinned by
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
  configured window (`POLLIS_DS_WATERMARK_STALE_MONTHS`, set to 12 months) stops
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

### I7 — A conversation id names exactly one conversation
- One namespace, owned by the `conversation` table (migration `000016`, #880):
  one row per id carrying the one `kind` it names. Every DS create path claims
  the id in the **same transaction** as the row it names, so the registry's
  primary key refusing a second claim rolls the creation back whole. The registry
  is append-only — a claimed id is claimed forever — because the attack is reuse,
  and with `foreign_keys=OFF` in production a deleted conversation leaves its
  membership rows behind for the next claimant to inherit.
- Unrepresentable since migration `000017` (#948): `BEFORE INSERT` /
  `BEFORE UPDATE OF id` guard triggers on all three tables `RAISE(ABORT)`
  unless the registry holds exactly `(id, kind)`, so a writer that skips the
  registry — or names another kind's id — is refused by the database itself.
  Triggers, not a foreign key: a bare per-table FK is satisfied by the same
  parent row for two children, and FK enforcement is off in production anyway.
  The triggers validate rather than claim, so `claim_conversation_id` stays
  the sole writer and the migration deployed against the running DS unchanged.

### I8 — No stored envelope is sealed behind the commit log's head
- An application envelope is decryptable only by members still AT the epoch it
  was sealed at (`max_past_epochs = 0`). Once the log holds a commit closing
  that epoch, every member either has moved past it or will the moment it
  catches up — an envelope stored behind the head is one that *some* member is
  guaranteed to drop. That was #1041: the pre-op catch-up and the op it
  precedes are two steps, and an envelope sealed in between was lost silently
  by the committer (merged past it before fetching it) or by everyone (sealed
  at an epoch already closed).
- Unrepresentable at the DS (#1041): `/v1/messages/send|edit` store an
  envelope only if its asserted `(generation, epoch)` equals `head_epoch_in`
  of the owning group's log, else 409 `epoch_behind`. The main and log DBs
  are separate handles, so the write is pre-check → insert → re-check → delete
  (`apply_at_head`), which closes the "commit landed while inserting" gap from
  the server's side. The residual window — a commit landing *after* admission —
  is the committer's: it sweeps the closing epoch's envelopes
  (`sweep_before_merge`) before it merges, and nothing more can be admitted
  there once its commit is in the log. Senders that are refused catch up,
  re-seal, and re-post (`post_resealing`).
- *Result:* "an envelope no current member can decrypt" is unrepresentable on
  the wire; the accepted losses stay exactly the three in `CLAUDE.md`.

## Enforcement layers, summarized

| Invariant | DB constraint/trigger | Rust type | Protocol | Test |
|---|---|---|---|---|
| I1 gapless append-only log | ✅ triggers (#691, migration 000005) | append-only chain type | CAS insert | direct-SQL violation (`commit_log_triggers.rs`) |
| I2 verifiable chain | link column | typed `CommitChain` | verify-on-apply | tamper test |
| I3 cursor delivery | seq + cursor tables; retention via floor | monotonic `Cursor` | ingest advances cursor | absent-member catch-up |
| I4 retain-to-slowest | retention floor (no TTL) | — | GC reads floor | 300-behind catch-up |
| I5 historical membership | — | — | derive from tree | churn + catch-up |
| I6 one schema | `schema_migrations` integrity | — | single apply path | test==prod schema check |
| I7 one id, one conversation | `conversation` PK + `kind` CHECK, claimed in the creating txn (#880); guard triggers on all three tables (#948, migration 000017) | `ConversationKind` | `conversation_id_taken` → 403 | guard vs registry vs DB-trigger layers (`conversation_namespace.rs`) |
| I8 no envelope behind the head | — (two DB handles; insert-then-verify) | `Sealed { generation, epoch }` asserted from the ciphertext | `epoch_behind` → 409 + committer pre-merge sweep | `epoch_gate_tests` (DS) + `dms::message_sealed_*` (flows, fail with the gate/sweep off) |

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
