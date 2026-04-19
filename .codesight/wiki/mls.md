# MLS (Message Layer Security)

All group encryption uses MLS (RFC 9420) via the `openmls` crate. One MLS group per Pollis group (all channels in a group share it). DM channels each have their own MLS group.

Source: `src-tauri/src/commands/mls.rs`

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
2. **Find devices** with unclaimed KeyPackages for roster users
3. **Peek at tree** to see who's already a member (avoids wasting KPs)
4. **Claim KPs** only for devices not in the tree
5. **Diff**: desired set vs actual tree → compute adds and removes
6. **Build and stage commit** with both add and remove proposals — do NOT `merge_pending_commit` yet
7. **Write to Turso on a fresh connection**: commit to `mls_commit_log`, welcome(s) to `mls_welcome`
8. **On success**: `merge_pending_commit` locally → advance the local epoch
9. **On failure**: `clear_pending_commit` → leave local state at the prior epoch; caller can retry
10. **Publish GroupInfo** so external-join works

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

This runs automatically on every message send (`send_message`) and message read (`get_channel_messages`, `get_dm_messages`), plus when `membership_changed` events arrive.

## Multi-Device Enrollment

When a new device (deviceC) enrolls for an existing user:

1. **Approval path**: existing device approves → wraps `account_id_key` for deviceC
2. **DeviceC's `finalize_enrollment`**:
   - Publishes device cert (`ensure_device_cert`)
   - Publishes 5 KeyPackages (`ensure_mls_key_package`)
   - External-joins every group the user belongs to (`external_join_group`)
3. **Other devices** process deviceC's external-join commits on next read/send

The approver does NOT reconcile during approval (deviceC has no KPs yet at that point). DeviceC handles its own group joining via external-join.

## Message Encrypt/Decrypt

**Send** (`send_message`):
1. Poll welcomes → process pending commits (ensures current epoch)
2. `try_mls_encrypt(local_db, group_id, plaintext)` → MLS ciphertext
3. Store ciphertext in `message_envelope` (remote) and `message` (local)

**Receive** (`get_channel_messages` / `get_dm_messages`):
1. Poll welcomes → process pending commits
2. Fetch `message_envelope` rows from Turso
3. `try_mls_decrypt(local_db, group_id, ciphertext)` → plaintext
4. Cache decrypted content in local `message` table

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
