# MLS (Message Layer Security)

All group encryption uses MLS (RFC 9420) via the `openmls` crate. One MLS group per Pollis group (all channels in a group share it). DM channels each have their own MLS group.

Source: `pollis-core/src/commands/mls.rs`

## Core Concepts

- **Epoch**: monotonically increasing version number of the group tree. Every commit advances the epoch by 1.
- **Commit**: an MLS operation that changes the group tree (add/remove members). Serialized to `mls_commit_log`.
- **Welcome**: an MLS message that lets a new member join at a specific epoch. Serialized to `mls_welcome`.
- **GroupInfo**: a snapshot of the group tree at a specific epoch. Stored in `mls_group_info`. Used for external-join.
- **KeyPackage**: a one-time-use cryptographic token published by each device. Consumed when the device is added to a group. Each device publishes ONE pool, in `CS_PQ`. #454 P2 published TWO disjoint pools — classic and post-quantum hybrid — so that a peer on either suite could always claim one; #669 retired the classic pool with the classic suite. `ensure_mls_key_package` rotates the pool on login and `replenish_key_packages` tops it up, both through the one `PollisProvider`, signing each leaf with the suite's scheme (ML-DSA-44).
- **External Join**: a device adds itself to a group using published GroupInfo, without needing a Welcome from an existing member.
- **Ciphersuite**: fixed per group at creation — RFC 9420 does not permit changing it in place (see **Suite generations** below for how a group moves anyway). Pollis runs exactly one: `CS_PQ` (`MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44`, code point `0x0052`, KEM = X-Wing = X25519 + ML-KEM-768, signature = ML-DSA-44). #454 introduced it as a *second* suite beside the classic `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` (`0x0001`) so a fleet mid-upgrade could still add each other; #668 moved it in place from `0x004D`, which was post-quantum in *confidentiality* only (same KEM, Ed25519 leaves); #669 deleted the classic suite — and with it the code that chose between the two — so `CS_HYBRID` was renamed `CS_PQ` and is now the sole suite. The point is provisional (`draft-ietf-mls-pq-ciphersuites` has renumbered once already) and moving it again is a wire break for every group on the suite — treat it as a migration, see `docs/pq-hybrid-mls-design.md` §7. Every new group is born on `CS_PQ` (`init_mls_group`); there is no selection step. The suite is still an explicit argument to the only two functions that mint suite-bound material — `create_mls_group_in_suite` and `build_key_package_in_suite` (#454 P1b) — so a code-point change stays a one-constant edit; one backend (`PollisProvider`, RustCrypto) serves it, so there is no suite→provider routing. Signing is still a function of the suite (`provider::signature_scheme`) rather than a constant, because a stored group persisted under an older code point must be read under *its* scheme; `CS_PQ` leaves sign ML-DSA-44. A device holds ONE signing key **per scheme** (`load_or_create_device_signer`, scheme-scoped) — published as `user_device.mls_signature_pub` (Ed25519) and `mls_signature_pub_pq` (ML-DSA-44), and both bound by the single v2 `device_cert` so no leaf is ever uncertified. A key package's suite is still stored in `mls_key_package.ciphersuite` and claims still narrow on it: the DS cannot read the suite out of the opaque blob, and the column is what keeps a package published under a retired code point from ever being served for a current group. `user_device.pq_capable` is **dead** since #669 — nothing reads or writes it (see [database.md](./database.md#user_device-migration-11--13--post-baseline-000011)).

## Key Functions

| Function | File | Purpose |
|----------|------|---------|
| `reconcile_group_mls_impl` | mls.rs | Declarative: diffs desired roster vs actual tree, commits adds/removes |
| `process_pending_commits_inner` | mls.rs | Processes inbound commits from commit log; external-joins if no local group |
| `external_join_group` | mls.rs | Self-service join using published GroupInfo |
| `poll_mls_welcomes_inner` | mls.rs | Fetches and applies pending Welcome messages |
| `apply_welcome` | mls.rs | Deserializes and applies a single Welcome |
| `publish_group_info` | mls.rs | Exports and stores current GroupInfo for external-join |
| `ensure_mls_key_package` | mls.rs | Rotates this device's pool: publishes 5 fresh `CS_PQ` KeyPackages |
| `init_mls_group` | mls.rs | Creates a new MLS group — always in `CS_PQ` (#669) |
| `migrate_to_current_suite_if_due` | mls/migrate.rs | Sweep-driven: stands up a `CS_PQ` **successor group** for any conversation whose stored suite is not `CS_PQ` |
| `self_update_if_due` | mls/self_update.rs | Sweep-driven leaf rotation for PCS — join + every 7 days ± jitter, ≤3 conversations per sweep (#666) |
| `has_local_group` | mls.rs | Checks if a local MLS group exists for a conversation |

## Reconcile Flow (the core operation)

`reconcile_group_mls_impl(state, conversation_id, actor_user_id)` is the **single function** that handles all group membership changes. It's called from:
- `create_group` (add creator's other devices)
- `send_group_invite` (pre-add invitee so Welcome is ready)
- `approve_join_request` (add requester)
- `remove_member_from_group` / `leave_group` (remove member)

Steps:
1. **Build roster** from `group_member` + `group_invite` (or `dm_channel_member`)
2. **TOFU-pin every roster peer's `account_id_pub`** via `batch_check_and_pin_account_keys` (one Turso query). First-seen keys are pinned silently; an existing pin that no longer matches the server flips `verified=0`, refreshes the pin in place, and emits a `KeyChanged` realtime event. The actor's own user id is excluded. This closes the historical group MITM hole — see `.codesight/wiki/safety.md`.
3. **Find devices** with unclaimed KeyPackages for roster users
4. **Peek at tree** to see who's already a member (avoids wasting KPs)
5. **Claim KPs** only for devices not in the tree
6. **Diff**: desired set vs actual tree → compute adds and removes
7. **Build and stage commit** with both add and remove proposals — do NOT `merge_pending_commit` yet
8. **Submit the commit bundle to the DS on a fresh connection**: commit + GroupInfo + Welcome(s) are one **atomic** `POST /v1/commits` — the DS writes all three in a single libsql transaction (see "MLS durability hardening" below), so a recipient never sees a commit with no matching Welcome
9. **On success**: `merge_pending_commit` locally → advance the local epoch
10. **On failure**: `clear_pending_commit` → leave local state at the prior epoch; caller can retry
11. **Publish GroupInfo** so external-join works
12. **Emit `RosterChanged` event** with the user-level diff (joined / left / device added / device removed). Local sink + room broadcast so existing members render inline timeline banners.

### Ordering invariant

The remote DB is the source of truth for MLS state. Staging the commit locally, writing it remotely on a **fresh** libsql connection, and only then merging locally means:
- A libsql stream eviction (hrana idle GC) mid-reconcile cannot advance the local epoch past a commit that never reached the server.
- A retry after a remote failure sees a clean local state rather than a doomed "local is ahead of remote" configuration.

See commit `83df6ef` for the rationale; breaking this ordering re-introduces the 9-user churn flake.

## How Other Devices Catch Up

When device A commits a membership change:
1. The commit is written to `mls_commit_log`
2. A `membership_changed` LiveKit event notifies online devices (convenience, not required). Like every realtime wake-up ping it carries **no sender/actor identity** — just the routing handle (see "Metadata-minimized signalling" below)
3. Other devices call `process_pending_commits_inner` which:
   - Fetches commits from `mls_commit_log` at `epoch >= local_epoch`
   - Applies them sequentially
   - If no local group exists → external-joins using published GroupInfo
   - If the group was evicted (user was kicked) → deletes it, then external-joins
   - Publishes updated GroupInfo after processing
   - Reports this device's now-current applied epoch to the DS (`ds_report_commit_since` → **device-signed** `POST /v1/commits/since`) so the server can compute the commit-log **retention floor** (#539, below). The report is authenticated (#681): it raises the floor, so it must be bound to the reporting device — reads (`GET /v1/commits/:id`) stay open, but recording a high-water does not

### Commit-log retention (I4, #539)

The DS (sole writer) prunes `mls_commit_log` below a **retention floor** so storage
and a long-offline member's catch-up cost stay bounded — event-driven on
commit-append and on a device's catch-up report, never on a timer. **Tier 1** keeps
the floor at/below the MIN applied epoch across all CURRENT member devices (guarded
by the SLOWEST member — zero loss; `NoLossForCurrentMember`, `specs/tla/Delivery.tla`
Spec B), applied only once the whole roster has reported (`mls_commit_since`).
**Tier 2** is a hard cap (`head − PRUNE_MAX_BEHIND_HEAD`) that bounds storage even
against a never-returning device; a member pruned past its epoch hits an epoch gap
on return (`invariants::classify` → `GapRecover`) and external-joins at head,
forfeiting only the pruned-gap messages (accepted loss #1). `may_rejoin` (I5) still
blocks a removed/revoked device from that rejoin. See
[database.md](./database.md#mls_commit_since-commit-log-db-migration-000003-539).

The epoch floor (`prune_floor`) is **clamped to `head − 1`** (#681): the reported
`since` values it derives Tier 1 from are untrusted, so a report above the real head
must never drive the floor at/above `head` and delete the whole live log — the head
commit (`epoch = head − 1`) is always retained, keeping the head reading true and the
group advanceable. This mirrors `closed_generation_floor`'s `.min(head_generation)`
(the live lineage is never retired wholesale). The report auth (above) and this clamp
are the two halves of #681: together an unauthenticated, per-conversation commit-log
wipe. Coverage: `prune_floor` clamp property + omit-clamp mutant (Kani, `commit.rs`);
`pollis-delivery/tests/retention.rs` (bogus high-water cannot wipe); the report
endpoint's signed/unsigned/revoked cases (`pollis-delivery/tests/auth.rs`); and the
E2E `forged_retention_high_water_cannot_wipe_the_live_log` (`flows/adversarial.rs`).

The bare `process_pending_commits_inner` / `_locked` variants are the raw commit replay used internally by the interleaved catch-up (via `process_pending_commits_inner_with_hook`). Callers that ADVANCE the epoch — the **commit-INITIATION** paths and the recovery converge alike — must NOT use the bare variant directly. The **commit-INITIATION** paths — send, edit, invite (add), remove — must NOT use the bare variant: advancing this device to head before its own op discards the ratchet keys for the current epoch (`max_past_epochs = 0`), so a current-epoch inbound message this device hasn't fetched yet would be **stranded** by its own commit (issue #440, the *committer strand*). They instead run the interleaved ingesting catch-up **before** advancing — see "Pre-op ingest-before-advance" below.

### Group-level interleaved catch-up (message-loss fix)

Every **catch-up** entry point instead routes through
`messages::catch_up_mls_group_interleaved(state, mls_group_id, user_id)`:
- `get_channel_messages` (opening a channel) and `get_dm_messages`
- the cold-launch/reconnect sweep `catch_up_all_mls_groups`
- the realtime `membership_changed` handler (`livekit/realtime.rs`)
- the `process_pending_commits` command (the app's manual "sync" shortcut)

**Why a group-level catch-up exists.** All channels in a group share ONE MLS
group (`mls_group_id == group_id`), but message ingest is per-conversation, and
`max_past_epochs = 0` (forward secrecy — the ratchet keys for an epoch are
discarded the instant the group advances past it). A per-channel or commit-only
catch-up advances the shared local group past an epoch at which *some* bound
conversation still holds an un-ingested message, and that message is then
**permanently undecryptable**. Three variants of the same root:
1. **cross-channel strand** — opening channel A advances the shared group past an
   epoch at which sibling channel B holds an un-ingested message;
2. **cold-launch sweep** — a bare commit-only replay advances every group to head
   before any message is ingested;
3. **realtime membership signal** — a bare commit-only replay on the membership
   event does the same.

**How it fixes them.** Given an `mls_group_id`, it enumerates *all* bound
conversations (a group's `channels`, or the single DM whose id IS the
`mls_group_id`), pulls each one's un-ingested envelopes past *that conversation's*
own watermark, indexes every envelope by its parsed MLS epoch across all
conversations, then drives the shared group's commit replay **once** — decrypting
the envelopes (from any conversation) sealed at each epoch via the `on_epoch` hook
in `process_pending_commits_locked_impl` **before** the next commit advances past
it. Each conversation's watermark advances independently over its own envelopes.
The replay still reaches head even with zero envelopes, so the cold-launch
"advance every group to head" guarantee is preserved. Steady state is cheap:
watermarks make repeat catch-ups return zero envelopes.

### Pre-op ingest-before-advance (committer strand, #440)

The group-level catch-up above closes the *fetch / sweep / realtime* variants,
but a fourth variant lives on the **commit-INITIATION** paths. When a client
performs its OWN operation it first catches up to head; if that catch-up is
commit-only, the client advances its epoch past a current-epoch inbound message
it hasn't ingested and loses it (`max_past_epochs = 0`). So every pre-op catch-up
runs the **interleaved ingesting** `catch_up_mls_group_interleaved` before the op
advances the epoch:

| Path | Call site | Lock held at the catch-up? |
| --- | --- | --- |
| Send message | `messages/send.rs` | No — swaps `process_pending_commits_inner` → interleaved catch-up |
| Edit message | `messages/edit_delete.rs` | No — same swap |
| Add member (invite) | `groups/invites.rs` (`send_group_invite`) | **Yes inside reconcile** — so the catch-up is HOISTED into the caller, before `reconcile_group_mls_impl` takes the per-conversation `mls_group_lock` |
| Remove member | `groups/membership.rs` (`remove_member_from_group`) | Same hoist, same reason |
| Voice / screenshare join | `voice_e2ee::derive_voice_key` | Already ingests (`ingest_*_envelopes_inner` → interleaved catch-up) — no change |

**Locking caveat.** `catch_up_mls_group_interleaved` internally calls
`process_pending_commits_inner_with_hook`, which acquires the per-conversation
`mls_group_lock`. It therefore MUST NOT be invoked while that lock is already
held. `reconcile_group_mls_impl` (the add/remove committer) holds the lock for
its whole body, so the invite/remove paths run the catch-up in their *caller*
BEFORE reconcile is entered. Send/edit hold no lock and swap in place.

**Recovery seam — lost-race converge (#4).** The reconcile-internal lost-race
converge was the LAST epoch-advancing path still using a bare commit-only replay
(`process_pending_commits_locked`). Applying the winner's commit — or rebuilding
via external-join if the converge forks — advances past the current epoch, so a
current-epoch inbound message not yet ingested would be stranded
(`max_past_epochs = 0`), exactly the strand-through-rebuild the marathon flagged
for a continuous member. It now runs the INTERLEAVED
`catch_up_mls_group_interleaved` instead, decrypting each epoch's messages before
advancing/rebuilding past it. Because that catch-up re-acquires the
`mls_group_lock`, reconcile drops its own guard first (it returns immediately
after the converge, so this is equivalent to reconcile finishing and a normal
catch-up running). This extends the ingest-before-advance invariant to every
advance path — fetch/sweep/realtime (group-level catch-up), send/edit/invite/
remove (pre-op hoist), and now the recovery converge.

### Recovery-path guards (revocation + membership lockout)

The external-join **recovery** paths in `process_pending_commits_locked_impl`
(no local group at start; group self-deleted during processing via eviction /
fork / epoch-gap) rebuild this device onto the published GroupInfo. Both are
gated by `may_rejoin_via_external_join`, which requires **two** things before it
lets a device rebuild:

1. `local_device_registered` — this device's `user_device` row still exists and
   is not revoked (fails **open** on error: a transient blip must never lock a
   legitimate device out of recovery);
2. `local_user_is_member` — the user is still a CURRENT member of the group
   (`group_member` / `dm_channel_member` / channel→group), mirroring the DS-side
   `writes::is_member`. Fails **closed** on error: this guards a membership
   *leak*, so when membership can't be confirmed we do NOT rebuild (never a
   permanent lockout — a real member recovers on the next pass).

**Why membership, not just revocation (fuzzer finding #2).** The DS
`/v1/commits` endpoint does NOT gate submissions on membership. A member who was
*removed* (their `group_member` row deleted) but whose device was NOT revoked
would pass the revocation gate, self-evict on catch-up, then external-join and
WIN its epoch on the CAS — climbing back into the tree and decrypting
post-removal traffic. The membership gate makes "a removed member rebuilds
itself" unrepresentable client-side. The `[Add(1), Remove(1), Add(2)]` shape is
the tightest repro (`removed_member_cannot_climb_back_via_external_join`): the
leak is only observable once a message is sent AFTER the climb-back.

The membership check uses `state.remote_db.conn()` directly (a separate
connection), NOT the `mls_group_lock` — safe to call from inside
`process_pending_commits_locked_impl`, which already holds that lock.

The `local_device_registered` gate here is the client half of the same
fail-closed logic the DS enforces server-side: the external-join **recovery** path
in `group_state.rs` treats a transient error as "cannot confirm membership → do
NOT rebuild" (a real member simply recovers on the next pass), so a blip can never
climb a removed device back into the tree (#430 P2).

## MLS durability hardening (#430)

A cluster of fixes that make membership state durable against dropped writes,
races, and duplicate deliveries. All are additive to the flows above.

- **Atomic DS commit bundle (P0).** `submit_commit` (`pollis-delivery/src/commit.rs`)
  writes the commit, its resulting-epoch GroupInfo, and any Welcomes inside **one**
  `IMMEDIATE` libsql transaction: all-commit-or-all-rollback. A partial write
  (commit lands, Welcome lost) used to be possible and recoverable only via the
  client's external-join fallback — the safety net was the exception path, not a
  guarantee. `IMMEDIATE` takes the write lock at BEGIN, so concurrent submitters
  still serialize exactly as the bare conditional INSERT did (one winner per epoch,
  no fork). The accept decision remains the single conditional
  `INSERT … SELECT … WHERE based_on = head … ON CONFLICT DO NOTHING`.
- **Eviction/remove reconcile backstop (P1).** The MLS post that evicts a removed
  member is best-effort; if dropped, the removed device is gone from the server
  roster but LINGERS in every remaining member's LOCAL tree (still a recipient of
  new-message seals) until some unrelated membership change re-runs reconcile — a
  forward-secrecy gap. The cold-launch/reconnect sweep now runs a backstop
  (`mls/sweep.rs`) after catching each group up: a cheap local-tree-vs-roster
  pre-check (`local_tree_has_stale_leaf`, two SELECTs + a local MLS load) and, only
  if a stale leaf remains, a `reconcile_group_mls_impl` retry that actually prunes
  it. Steady state (tree already matches roster) costs only the pre-check.
- **Welcome dedupe + idempotent resubmit (P2).** A `UNIQUE (conversation_id,
  recipient_id, recipient_device_id)` index on `mls_welcome` (commit-log-DB
  migration 000002) plus `ON CONFLICT … DO UPDATE` upserts in the submit bundle
  and the new `POST /v1/welcomes/resubmit` endpoint mean a re-sent Welcome
  refreshes the blob and re-arms delivery instead of stacking a duplicate row — so
  a retried commit bundle can never wedge on a dup. See
  [database.md](./database.md#mls_welcome-migration-3--11-now-on-the-commit-log-db).
- **`is_member` gate on `POST /v1/commits` (P2).** The commit-submit handler now
  verifies the authenticated committer is a **current** member of the conversation
  (`writes::is_member`) before accepting — mirroring `/v1/group-info`'s gate. This
  is the server half of the client-side membership gate above; together they make
  "a removed member climbs back via external-join" unrepresentable on both sides.
- **Authenticated retention report + head-clamped floor (#681).** The catch-up
  high-water report is a **device-signed** `POST /v1/commits/since`
  (`auth::verify_request_identity`, same four `X-Pollis-*` headers as
  `POST /v1/commits`, refuses a revoked device) instead of an unsigned `GET`
  side-effect — nobody can raise the floor on another device's behalf. Paired with
  `prune_floor`'s clamp to `head − 1`, this closes what was a remote,
  unauthenticated, per-conversation commit-log wipe. Reads stay open; an
  unauthenticated report is dropped, which only ever leaves the floor conservative
  (an unreported member disables Tier 1).

## Post-quantum suite: birth, migration, retirement (#454, #669)

### Which suite a new group gets

`CS_PQ`, always (`init_mls_group`). There is no decision to make and no code that
makes one.

**Why there used to be.** #454 shipped the PQ suite *alongside* the classic one and
`suite_for_new_group` (`mls/group_state.rs`) chose between them behind two gates: a
**roster gate** (every registered device on the desired roster, pending
`group_invite` rows included, had `pq_capable = 1`) and a deployment-wide **fleet
gate** (no unrevoked `user_device` seen within `FLEET_DORMANCY_DAYS` = 90 was still
classic-only). The roster gate alone was nearly vacuous — a group cannot see
*tomorrow's* invitees, and a PQ group that later has to admit a classic-only device
has only two options, both bad (skip it, or downgrade the group) — and the dormancy
window was what made "the whole fleet has updated" a reachable condition rather than
one hostage to a laptop in a drawer. Both gates failed toward availability.

**Why it dissolved.** #669 retired the classic suite outright: with one suite there
is no classic-only device to strand, so the gates have nothing left to protect
against. `suite_for_new_group`, `roster_is_fully_pq_capable`,
`fleet_is_fully_pq_capable`, `FLEET_DORMANCY_DAYS` and the `may_birth_hybrid`
predicate (with its I7 Kani harnesses) are all deleted, and `mark_pq_capable` with
them — nothing sets or reads `user_device.pq_capable` any more. The hard cutover was
available because the deployment has no active users; #669's own issue text assumed
it would have to wait out a 90-day fleet turnover, and that premise is what went
away, not the reasoning above.

### Suite generations — how an existing group moves

RFC 9420 has no "rekey to another suite" commit, so a migration is not an operation
on the group. `migrate_to_current_suite_if_due` (`mls/migrate.rs`,
≤`MAX_MIGRATIONS_PER_SWEEP` = 2 per sweep) stands up a **successor group** in
`CS_PQ`, moves the roster in by Welcome, and retires the predecessor. It fires for
any conversation whose stored suite is not `CS_PQ` — #454 wrote it as
"classic → hybrid", #669 generalised the trigger rather than deleting the path,
because `CS_PQ`'s code point is provisional and a renumber is exactly this
migration again with a different constant. In the steady state it is one local read
returning "nothing to do".

- Ordering key is the pair `(generation, epoch)`, compared **lexicographically** —
  the successor restarts at epoch 0, so a per-column max would read the migration
  as going backwards. Local `GroupId` is the conversation id verbatim at generation
  0 and `"{conversation_id}#g{generation}"` after; `mls_generation` tracks it.
- The whole thing is one **compare-and-swap**, enforced server-side: the DS accepts
  generation `N+1` at epoch 0 only when the submitter's `closes_epoch` names the
  head of generation `N` (`pollis_delivery::commit::accepts()`, Kani-proved). Any
  commit landing in the old lineage in between invalidates the migration rather
  than orphaning a commit on a lineage nobody will read again.
- Locally the successor is built under its own `GroupId`, so until the CAS resolves
  the device holds *both* lineages and a failure is a plain delete. Predecessor key
  material is dropped **last**, and only after the predecessor is drained to its
  head — it is the only thing that can still open pre-boundary envelopes, since
  `max_past_epochs = 0`.
- **No stranding**: before it creates anything, the migration claims a KeyPackage in
  the target suite for *every* roster device and aborts on the first miss — move
  everyone or nobody. A partial move would leave a device on the roster but never in
  the tree, unable to read its own conversation. #454 additionally gated on
  `pq_capable` (roster and fleet-wide); #669 dropped both reads, because the flag was
  only ever a *predictor* of this claim and the claim is the direct test.
- Forward-only: traffic sealed under the predecessor's suite before the boundary
  stays sealed under it. Nothing is re-encrypted.

Covered end-to-end by `src-tauri/tests/flows/pq_migration.rs` (suite-boundary
eviction of a stolen pre-migration leaf, dropped-Welcome recovery by external join)
and by the `model` fuzzer, which crosses the boundary mid-run and asserts no client
is stranded on a retired lineage.

### Self-update (#666)

`self_update_if_due` (`mls/self_update.rs`) rotates this device's own leaf: once on
join, then whenever its leaf in a conversation is older than `SELF_UPDATE_INTERVAL`
(7 days) plus a deterministic per-conversation jitter of up to `SELF_UPDATE_JITTER`
(2 days), capped at `MAX_SELF_UPDATES_PER_SWEEP` (3) per sweep. Launch-driven, not a
timer — consistent with the no-periodic-polling rule; a group whose members are all
offline does not heal until one of them opens the app.

Two things depend on it. **PCS**: without it, a compromised leaf stays readable
until membership happens to change. **Commit size**: unmerged leaves dominate a
`CS_PQ` UpdatePath (`kem_output` is 1120 B under X-Wing vs 32 B under the retired
classic suite's DHKEM), so without
self-update commit growth is linear in unmerged leaves — measured at 8→16 members,
+10.6 KB unmerged vs +2.4 KB merged.

## Multi-Device Enrollment

When a new device (deviceC) enrolls for an existing user:

1. **Approval path**: existing device approves → wraps `account_id_key` for deviceC
2. **DeviceC's `finalize_enrollment`**:
   - Publishes device cert (`ensure_device_cert`)
   - Publishes 5 `CS_PQ` KeyPackages (`ensure_mls_key_package`)
   - External-joins every group the user belongs to (`external_join_group`)
3. **Other devices** process deviceC's external-join commits on next read/send

The approver does NOT reconcile during approval (deviceC has no KPs yet at that point). DeviceC handles its own group joining via external-join.

## Voice Key Export

Voice channels reuse the same MLS group as the channel's messages. The shared per-room symmetric key is derived from the MLS exporter secret at the current epoch:

```text
voice_key = MlsGroup::export_secret(
    label   = "pollis/voice/v1",
    context = epoch.to_be_bytes(),
    length  = 32,
)
```

Both peers compute the same 32-byte key because both hold the same exporter secret at the same epoch. The key is handed to LiveKit's `FrameCryptor` (AES-128-GCM, libwebrtc-native) via `RoomOptions::encryption` so the SFU only ever sees ciphertext audio. On every commit merge in `process_pending_commits_inner` the key rotates without a reconnect — see `voice_e2ee::on_mls_epoch_changed` and [audio-processing.md](./audio-processing.md#end-to-end-encryption).

## Message Encrypt/Decrypt

**Send** (`send_message`):
1. Poll welcomes → interleaved ingesting catch-up (`catch_up_mls_group_interleaved`) to reach the current epoch while decrypting any current-epoch inbound message first, so this device's own send can't strand it (#440)
2. For a TEXT message, pad the plaintext to a size bucket (`messages::framing::pad`) so the ciphertext length no longer leaks the message length (metadata minimization, issue #331 v2, `docs/metadata-minimization-design.md` §4.1). Attachment envelopes are left unpadded. Then `try_mls_encrypt(local_db, group_id, plaintext)` → MLS ciphertext
3. Store ciphertext in `message_envelope` (remote) and `message` (local)

**Receive** (`get_channel_messages` / `get_dm_messages`):
1. Poll welcomes
2. `catch_up_mls_group_interleaved` — enumerate every bound conversation, fetch
   its un-ingested `message_envelope` rows from Turso, and replay commits ONCE
   for the shared group, decrypting each conversation's envelopes at the epoch
   they were sealed at (via `try_mls_decrypt`) *before* advancing past it
3. Strip size padding (`messages::framing::strip`) — a no-op for legacy/unpadded
   sends and attachment envelopes, detected by the framing version byte — then
   cache decrypted content in the local `message` table; advance each
   conversation's watermark independently
4. Read the requested conversation's page from the local `message` table

Decryption is interleaved with commit replay because `max_past_epochs = 0`: a
message must be decrypted while the local group is still AT its epoch (see
`envelope_epoch`, which parses an envelope's epoch without touching group state).

## Message deletion — "delete for everyone" (E2EE redaction)

Deleting your own message is **delete for everyone**: it is redacted on the
devices of members who *already fetched* it, not just hidden from pending
recipients. `delete_message` (`messages/edit_delete.rs`) has two paths:

- **Self-delete** (you deleting your own message) sends an **E2EE redaction
  control message**: an ordinary `type='message'` MLS envelope whose plaintext is
  a `0xF6` framing frame (`messages/framing.rs` `pad_redaction` / `classify`,
  a sibling of the `0xF5` text-padding frame) carrying the target message id,
  padded to the same size bucket so a redaction is length-indistinguishable from
  a short message and the **server never learns which message was redacted**. It
  rides the normal send path (`/v1/messages/send`) so it inherits MLS encryption,
  per-conversation watermarks, envelope GC, and offline delivery — **no DS route,
  schema, or migration change**. On ingest (`decrypt_and_persist_one`, the
  `message` branch) a recipient honors the redaction **only if its
  MLS-authenticated author (`cred_sender`) equals the target message's stored
  author** — so neither the server nor another member can redact a message they
  did not write (the invariant test is
  `flows/messages.rs::redaction_from_non_author_is_ignored`). Self-delete also
  removes the original envelope + any pending edit via `/v1/messages/delete` (so
  a not-yet-fetched member never receives it and the ciphertext does not linger
  at rest) and soft-deletes the sender's own row (`content=NULL, deleted_at`).

- **Admin-delete** (a group admin removing another member's message) keeps using
  the server-authorized plaintext `type='delete'` tombstone (`apply_delete_message`
  writes it; ingest applies it epoch-independently) — an admin generally cannot
  author an MLS message on another member's behalf, and moderation is legitimately
  server-side. Rejected for DMs.

The redaction is a **control message**: no visible `message` row is written for
it on either side.

## Credential Format

Each device's MLS credential is `{user_id}:{device_id}` encoded as a `BasicCredential`. Parsed by `parse_credential_user_id` and `parse_credential_device_id`.

## Sealed sender (#331)

Message attribution is taken from the **MLS credential inside the ciphertext**,
never from the server-writable `message_envelope.sender_id` column. On ingest
(`messages/ingest.rs`), `try_mls_decrypt` returns `(plaintext, cred_sender)` and
the local `message` row is written with `cred_sender` — the tuple's `sender_id` is
deliberately *not* read for attribution. This is **always on**: because the
sender is authenticated by MLS, a server that rewrites `sender_id` can neither
forge authorship nor learn it from that column.

On top of that, **envelope-sender blinding** makes the stored `sender_id`
non-identifying. This is **UNCONDITIONAL** (#607) — there is no flag: every
outbound `message`/redaction envelope is posted `sealed = 1` with a fixed sentinel
(`SEALED_SENDER_SENTINEL = "sealed"`) as the envelope's `sender_id` instead of the
real user id, so a Turso breach/subpoena of `message_envelope` reveals nothing
about who sent which message. There is no way to emit an unsealed envelope (the
no-opt-out invariant test in `flows/sealed_sender.rs`). The local `message` row
keeps the real `sender_id` (it's the author's own copy on a trusted device; the
send path writes self-attribution directly rather than re-deriving from the
credential). The DS drops the "send-as-yourself" equality check for these sends
but keeps the membership authz unchanged — it still verifies the *authenticated*
writer is a member (`apply_send_message` in `pollis-delivery/src/messages.rs`).
Edit envelopes (`type='edit'`) are the one exception: they keep the real
`sender_id` so the DS can membership-gate on the authenticated editor.

**Edit/delete authorization is now client-side (Solution A, #607).** Because the
stored `sender_id` is always the sentinel, the DS can no longer prove who authored
a message, so it stopped trying: `apply_edit_message` and the self-delete branch of
`apply_delete_message` gate on **membership only**; admin-delete still gates on the
re-derived **admin role**. Authorship is enforced **cryptographically on ingest**
instead — an edit or redaction is applied only when its MLS-authenticated author
(the credential inside the ciphertext) equals the target message's author
(`decrypt_and_persist_one` in `messages/ingest.rs`; invariant tests
`sealed_non_author_edit_is_ignored` / `redaction_from_non_author_is_ignored`). The
one accepted trade: a non-author member can remove a *not-yet-fetched* envelope
from Turso (an availability cost), but cannot forge a *delete appearance* on any
member who already holds the message — that still requires a valid redaction or an
admin tombstone. **Deploy ordering:** the DS must ship this authz change BEFORE any
sealing client, or edits/deletes 403 in prod (see `docs/deployments.md`).

**Honest scope (§2.1).** This is an **at-rest** defense. The DS authenticates
every write with an `X-Pollis-User` header and gates on membership, so a *live* DS
operator still sees the sender in real time. Closing that axis is v1.5
anonymous-membership (not shipped — tracked in #489). See
`docs/metadata-minimization-design.md` §2.

## Metadata-minimized signalling (#331 v2)

The LiveKit realtime wake-up pings (`new_message` / `edited_message` /
`deleted_message` / `membership_changed` / `roster_changed`) are only a **hint to
fetch**. LiveKit forwards them in cleartext, so they deliberately carry **no
sender/actor identity** — no `sender_id`, `sender_username`, `deleted_by`, or (for
`roster_changed`) the per-user `joined`/`left`/device-id lists. The recipient
re-derives the true sender from the MLS credential in the envelope it then fetches
(sealed sender, above). Payload builders and a test that fails if any identifying
field reappears live in `pollis-core/src/commands/livekit_signalling.rs`
(`docs/metadata-minimization-design.md` §5).

The reconciling client still emits the full `joined`/`left`/device-id diff to its
**own local sink** so it renders inline roster banners immediately; remote peers
re-derive the diff from the authenticated MLS commit + a member refetch. See
`.codesight/wiki/safety.md` → "Roster-change banners".

## GroupInfo Publishing

GroupInfo is published (upserted to `mls_group_info`) after:
- `init_mls_group` (epoch 0)
- `reconcile_group_mls_impl` (after every commit)
- `process_pending_commits_inner` (after applying commits)
- `external_join_group` (after self-joining)

The UPSERT only overwrites if the new epoch is strictly greater than the stored epoch.

---
_Back to [index.md](./index.md)_
