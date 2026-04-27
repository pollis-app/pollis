# Tauri Commands

All backend calls from the frontend use `invoke("command_name", { args })`. Commands are registered in `src-tauri/src/lib.rs` and implemented in `src-tauri/src/commands/`.

## auth (`commands/auth.rs`)
- `initialize_identity(user_id)` — ensure MLS credentials + KPs, poll welcomes. Requires the local DB to be open (post-`set_pin` / `unlock`).
- `get_identity()` — check if MLS identity exists locally
- `request_otp(email)` — send OTP code to email
- `verify_otp(email, code)` → `AuthResult` — verify OTP, register the device. Does **not** open the local DB; `set_pin` (signup) or `unlock` (resume) does.
- `get_session()` → `AuthResult | null` — rebuild profile from `accounts.json`. Does not open the local DB.
- `logout(delete_data)` — clear session, optionally delete local data
- `delete_account(user_id)` — delete account from Turso + local
- `wipe_local_data()` — delete all local databases and keystore entries

## pin (`commands/pin.rs`)
PIN is cryptographically load-bearing — see `pin-design.md`.
- `set_pin(old_pin?, new_pin)` — initial-set sources from `AppState.unlock` (canonical) or the legacy plaintext keystore slots (upgrade fallback). Wraps both keys, deletes the legacy slots, opens the local DB via `load_user_db_with_key`, publishes the device cert.
- `unlock(user_id, pin)` → `UnlockOutcome` — verify PIN, populate `AppState.unlock`, open the local DB, migrate away any #195-vintage legacy slots, publish the device cert.
- `lock()` — drop `AppState.unlock` and close the local DB. Until the next `unlock`, every DB-touching command fails with "Not signed in".
- `get_unlock_state()` → `{ last_active_user, is_unlocked, pin_set }` — frontend uses this to route between pin-entry, pin-create, and the main app.

## user (`commands/user.rs`)
- `get_user_profile(user_id)` → `User`
- `update_user_profile(user_id, username?, email?, phone?)` → `User`
- `search_user_by_username(query)` → `User[]`
- `get_preferences(user_id)` → JSON string
- `save_preferences(user_id, preferences_json)`
- `upload_avatar(user_id, file_data, file_name, content_type)` → URL
- `get_avatar_url(user_id)` → URL

## groups (`commands/groups.rs`)
- `list_user_groups(user_id)` → `Group[]`
- `list_user_groups_with_channels(user_id)` → `GroupWithChannels[]`
- `list_group_channels(group_id)` → `Channel[]`
- `create_group(name, description?, owner_id)` → `Group`
- `create_channel(group_id, name, description?, channel_type?)`
- `send_group_invite(group_id, inviter_id, invitee_identifier)`
- `get_pending_invites(user_id)` → `PendingInvite[]`
- `accept_group_invite(invite_id, user_id)`
- `decline_group_invite(invite_id, user_id)`
- `request_to_join_group(group_id, user_id)`
- `approve_join_request(request_id, approver_id)`
- `reject_join_request(request_id, approver_id)`
- `remove_member_from_group(group_id, user_id, actor_id)`
- `leave_group(group_id, user_id)`
- `update_member_role(group_id, target_user_id, new_role, actor_id)`
- `list_group_members(group_id)` → `Member[]`
- `search_groups(query)` → `Group[]`

## messages (`commands/messages.rs`)
- `send_message(conversation_id, sender_id, content, reply_to_id?, sender_username?)` → `Message`
- `get_channel_messages(user_id, channel_id, limit, cursor?)` → `MessagePage`
- `get_dm_messages(user_id, dm_channel_id, limit, cursor?)` → `MessagePage`
- `edit_message(message_id, conversation_id, sender_id, new_content)`
- `delete_message(message_id, user_id)` — hard-deletes the envelope on Turso + the sender's local row. If the message had attachments (`_att` in the plaintext JSON payload), each `content_hash` is reference-counted against the sender's other non-deleted local messages; unreferenced ones have their `attachment_object` row + R2 object removed (best-effort, logged on failure). Cross-user references are invisible because attachment metadata lives inside the MLS-encrypted payload — convergent encryption means another member re-uploading the same file simply re-registers the dedup row.
- `search_messages(user_id, query, conversation_id?)` → `Message[]`

## dm (`commands/dm.rs`)
- `create_dm_channel(creator_id, member_ids)` → `DmChannel` — seeds creator's `accepted_at` as now, other members' as NULL (pending request). Rejects with `"message request pending"` if a block exists in either direction with any proposed member.
- `list_dm_channels(user_id)` → `DmChannel[]` — only channels where the caller has accepted (`accepted_at IS NOT NULL`) and neither party has blocked the other.
- `list_dm_requests(user_id)` → `DmChannel[]` — channels where the caller's `accepted_at IS NULL` and no block exists with the other participant(s).
- `accept_dm_request(dm_channel_id, user_id)` — idempotent; flips the caller's `accepted_at` to now. The conversation then appears in `list_dm_channels`.
- `get_dm_channel(dm_channel_id)` → `DmChannel`
- `add_user_to_dm_channel(dm_channel_id, user_id, added_by)`
- `remove_user_from_dm_channel(dm_channel_id, user_id, requester_id)`
- `leave_dm_channel(dm_channel_id, user_id)` — if the last member leaves, channel + envelopes are cleaned up.

## blocks (`commands/blocks.rs`)
- `block_user(blocker_id, blocked_id)` — idempotent. Inserts `user_block` row and resets `accepted_at = NULL` on the blocker's `dm_channel_member` rows for every DM shared with the blocked user (so after unblock those conversations reappear as requests).
- `unblock_user(blocker_id, blocked_id)` — deletes the `user_block` row. DM history becomes visible again and un-accepted channels surface in `list_dm_requests`.
- `list_blocked_users(user_id)` → `BlockedUser[]`
- Enforced in: `create_dm_channel`, `send_message` (DM only — group-channel sends are not gated), `send_group_invite`. All three return the identical string `"message request pending"` so the sender cannot infer which gate rejected them.

## mls (`commands/mls.rs`)
- `reconcile_group_mls(conversation_id, actor_user_id)`
- `process_pending_commits(conversation_id, user_id)`
- `poll_mls_welcomes(user_id)`
- `generate_mls_key_package(user_id)` → JSON

## device_enrollment (`commands/device_enrollment.rs`)
Every path that produces a fresh `account_id_key` (signup, approval, Secret-Key recovery, identity reset) hands the bytes to `AppState.unlock` — never to the keystore unwrapped. The frontend then routes to pin-create; `set_pin` wraps under the user's PIN and opens the local DB.
- `start_device_enrollment(user_id)` → `EnrollmentHandle`
- `poll_enrollment_status(request_id)` → `EnrollmentStatus`. On `Approved`, populates `AppState.unlock`; defers cert / KP / external-join to `finalize_device_enrollment`.
- `approve_device_enrollment(request_id, user_id, verification_code)`
- `reject_device_enrollment(request_id, user_id)`
- `list_pending_enrollment_requests(user_id)` → `PendingEnrollmentRequest[]`
- `recover_with_secret_key(user_id, secret_key)` — same handoff pattern as the approval path.
- `reset_identity_and_recover(user_id, email)` — soft recovery; `reset_identity` populates `AppState.unlock` with the new keypair before this command's local-DB cleanup runs.
- `finalize_device_enrollment(user_id)` — call after `set_pin` completes. Publishes the device cert + a fresh MLS key package, then external-joins every existing group / DM the device isn't in yet. Idempotent for fresh signup.
- `list_user_devices(user_id)` → `DeviceInfo[]`
- `reset_identity(user_id)` → new secret key

## livekit (`commands/livekit.rs`)
- `get_livekit_token(room_id, user_id, username)` → token string
- `subscribe_realtime(on_event: Channel)`
- `connect_rooms(room_ids, user_id, username)`

## voice (`commands/voice.rs`)
- `prepare_voice_connection(channel_id, user_id, display_name)` — best-effort warmup fired on user "intent" (hover, route entry). Mints + caches a LiveKit token and runs a one-shot HTTPS probe to warm DNS / TLS / connection pool. Idempotent; safe to call eagerly. Consumed by the next `join_voice_channel` for the same channel + identity.
- `join_voice_channel(channel_id, user_id, display_name, input_device, output_device, audio_processing)` — connect to LiveKit and publish the local mic. `audio_processing` is the `ApmConfig` struct (AGC + NS + AEC settings) — see [Audio Processing](./audio-processing.md). Consumes a fresh warmup if present and runs `Room::connect` + cpal mic init concurrently to minimise cold-start latency.
- `leave_voice_channel()`
- `toggle_voice_mute()`
- `set_voice_input_device(device_name)` / `set_voice_output_device(device_name)` — switch device mid-call. Input switch rebuilds APM if the new device's sample rate differs.
- `set_voice_audio_processing(config)` — push live APM config (AGC target, NS level, AEC on/off) without rejoining. Internal echo / noise / AGC state is preserved; only the changed submodule re-initialises.
- `subscribe_voice_events(on_event: Channel)`
- `list_audio_devices()` → `AudioDevice[]`
- `get_last_join_timings()` — debug: most recent `JoinTimings` record (jwt, room connect, mic init, first publish, total).

## r2 (`commands/r2.rs`)
- `upload_file(data, key, content_type)` → URL
- `download_file(key)` → bytes
- `presign_upload(key, content_type)` → presigned URL
- `upload_media(path, filename, content_type)` / `download_media(r2_key, content_hash)` — convergent-encryption media path; dedups via `attachment_object` on Turso.
- Internal: `delete_r2_object(state, r2_key)` — SigV4 DELETE used by `delete_message` to purge orphaned attachments. Treats 404 as success.

---
_Back to [index.md](./index.md)_
