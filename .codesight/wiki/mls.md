# MLS (Message Layer Security)

All group encryption uses MLS (RFC 9420) via the `openmls` crate. One MLS group per Pollis group (all channels in a group share it). DM channels each have their own MLS group.

Source: `pollis-core/src/commands/mls.rs`

## Core Concepts

- **Epoch**: monotonically increasing version number of the group tree. Every commit advances the epoch by 1.
- **Commit**: an MLS operation that changes the group tree (add/remove members). Serialized to `mls_commit_log`.
- **Welcome**: an MLS message that lets a new member join at a specific epoch. Serialized to `mls_welcome`.
- **GroupInfo**: a snapshot of the group tree at a specific epoch. Stored in `mls_group_info`. Used for external-join.
- **KeyPackage**: a one-time-use cryptographic token published by each device. Consumed when the device is added to a group.
- **External Join**: a device adds itself to a group using published GroupInfo, without needing a Welcome from an existing member.

## Key Functions

| Function | File | Purpose |
|----------|------|---------|
| `reconcile_group_mls_impl` | mls.rs | Declarative: diffs desired roster vs actual tree, commits adds/removes |
| `process_pending_commits_inner` | mls.rs | Processes inbound commits from commit log; external-joins if no local group |
| `external_join_group` | mls.rs | Self-service join using published GroupInfo |
| `poll_mls_welcomes_inner` | mls.rs | Fetches and applies pending Welcome messages |
| `apply_welcome` | mls.rs | Deserializes and applies a single Welcome |
| `publish_group_info` | mls.rs | Exports and stores current GroupInfo for external-join |
| `ensure_mls_key_package` | mls.rs | Publishes 5 fresh KeyPackages for this device |
| `init_mls_group` | mls.rs | Creates a new MLS group (called from create_group/create_dm) |
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
8. **Write to Turso on a fresh connection**: commit to `mls_commit_log`, welcome(s) to `mls_welcome`
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
2. A `membership_changed` LiveKit event notifies online devices (convenience, not required)
3. Other devices call `process_pending_commits_inner` which:
   - Fetches commits from `mls_commit_log` at `epoch >= local_epoch`
   - Applies them sequentially
   - If no local group exists → external-joins using published GroupInfo
   - If the group was evicted (user was kicked) → deletes it, then external-joins
   - Publishes updated GroupInfo after processing

The bare `process_pending_commits_inner` / `_locked` variants are for the pure recovery/converge paths only (e.g. reconcile's lost-race convergence). The **commit-INITIATION** paths — send, edit, invite (add), remove — must NOT use the bare variant: advancing this device to head before its own op discards the ratchet keys for the current epoch (`max_past_epochs = 0`), so a current-epoch inbound message this device hasn't fetched yet would be **stranded** by its own commit (issue #440, the *committer strand*). They instead run the interleaved ingesting catch-up **before** advancing — see "Pre-op ingest-before-advance" below.

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
BEFORE reconcile is entered. Send/edit hold no lock and swap in place. The
reconcile-internal recovery replay (lost-race → `process_pending_commits_locked`)
is left untouched — it is a converge path, not a pre-op catch-up.

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

## Multi-Device Enrollment

When a new device (deviceC) enrolls for an existing user:

1. **Approval path**: existing device approves → wraps `account_id_key` for deviceC
2. **DeviceC's `finalize_enrollment`**:
   - Publishes device cert (`ensure_device_cert`)
   - Publishes 5 KeyPackages (`ensure_mls_key_package`)
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
2. `try_mls_encrypt(local_db, group_id, plaintext)` → MLS ciphertext
3. Store ciphertext in `message_envelope` (remote) and `message` (local)

**Receive** (`get_channel_messages` / `get_dm_messages`):
1. Poll welcomes
2. `catch_up_mls_group_interleaved` — enumerate every bound conversation, fetch
   its un-ingested `message_envelope` rows from Turso, and replay commits ONCE
   for the shared group, decrypting each conversation's envelopes at the epoch
   they were sealed at (via `try_mls_decrypt`) *before* advancing past it
3. Cache decrypted content in the local `message` table; advance each
   conversation's watermark independently
4. Read the requested conversation's page from the local `message` table

Decryption is interleaved with commit replay because `max_past_epochs = 0`: a
message must be decrypted while the local group is still AT its epoch (see
`envelope_epoch`, which parses an envelope's epoch without touching group state).

## Credential Format

Each device's MLS credential is `{user_id}:{device_id}` encoded as a `BasicCredential`. Parsed by `parse_credential_user_id` and `parse_credential_device_id`.

## GroupInfo Publishing

GroupInfo is published (upserted to `mls_group_info`) after:
- `init_mls_group` (epoch 0)
- `reconcile_group_mls_impl` (after every commit)
- `process_pending_commits_inner` (after applying commits)
- `external_join_group` (after self-joining)

The UPSERT only overwrites if the new epoch is strictly greater than the stored epoch.

---
_Back to [index.md](./index.md)_
