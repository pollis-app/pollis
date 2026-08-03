# Backend Commands

All backend calls from the frontend use `invoke("command_name", { args })` (imported from `frontend/src/bridge`, which routes to Tauri's `invoke`). Dispatch happens in `src-tauri/src/commands/`: a thin `#[tauri::command]` shim per command — registered in `src-tauri/src/lib.rs`'s `invoke_handler!` — forwards the JSON-shaped call into `pollis-core/src/commands/`. The implementations live in `pollis-core` so a CLI / TUI / mobile binding (uniffi) can call them without any shell-runtime dependency. Edit `pollis-core`, not the shims.

The path in each section header below points at the implementation in `pollis-core`. The `#[tauri::command]` shim under `src-tauri/src/commands/` with the same module name re-exports the types and forwards each command verbatim.

> ### ⚠️ This is a PARTIAL listing — read this before trusting it
>
> **Last verified: 2026-08-03 (#714), against `src-tauri/src/lib.rs` at `d13c906`.**
>
> At that point the app registered **167** commands in `tauri::generate_handler![…]`. The prose sections below describe roughly **half** of them; the rest (voice calls/ringing, camera, screenshare, tray, terminal, reactions, safety numbers, retention/eviction, media permissions, transparency self-audit, sound effects, update gating, clipboard, and several groups/messages commands) have **no prose here at all**. Nothing below has been re-verified line by line beyond the corrections noted in this pass, so treat an entry's *arguments and return shape* as a hint, not a contract.
>
> **The authoritative list is the code, not this file.** To get it:
>
> ```bash
> # every registered command name, in module order
> sed -n '/generate_handler!\[/,/^\s*\]) *$/p' src-tauri/src/lib.rs
> ```
>
> Appendix A at the bottom of this page is a mechanically extracted snapshot of that list as of the date above — names only, no invented descriptions. If a command appears there but not in the prose, read its `pollis-core` implementation; do not assume this page's silence means it does not exist.
>
> When you add or change a command, update the relevant section here **and** Appendix A in the same PR (per `CLAUDE.md`: update the wiki alongside the code).

## auth (`commands/auth.rs`)
- `initialize_identity(user_id)` — ensure MLS credentials + KPs, poll welcomes. Requires the local DB to be open (post-`set_pin` / `unlock`).
- `get_identity()` — check if MLS identity exists locally
- `request_otp(email)` — send OTP code to email
- `verify_otp(email, code)` → `AuthResult` — verify OTP, register the device. Does **not** open the local DB; `set_pin` (signup) or `unlock` (resume) does.
- `get_session()` → `AuthResult | null` — rebuild profile from `accounts.json`. Does not open the local DB.
- `get_device_id()` → `string | null` — this device's stable `device_id` (or null pre-registration). Used to build the per-device voice identity (`voice-{user_id}:{device_id}`) so a client can tell which voice participant is itself (#140). **Not a registered Tauri command** as of 2026-08-03 (#714) — it lives in `pollis-core/src/commands/auth.rs` and is called internally; `invoke("get_device_id")` will fail.
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
- ~~`upload_avatar(user_id, file_data, file_name, content_type)`~~ / ~~`get_avatar_url(user_id)`~~ — **neither exists** anywhere in `pollis-core` or `src-tauri` as of 2026-08-03 (#714). Struck out rather than replaced with a guess: `avatar_url` is a field on the user profile returned by `get_user_profile`; whatever writes it is not one of these two commands. Trace it from `frontend/src/hooks/queries/useUserProfile.ts` before documenting a replacement.

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
- `request_group_access(group_id, requester_id)` — creates a pending join request. (Documented here as `request_to_join_group` until #714; that name has never existed in the tree.)
- `approve_join_request(request_id, approver_id)`
- `reject_join_request(request_id, approver_id)`
- `remove_member_from_group(group_id, user_id, actor_id)`
- `leave_group(group_id, user_id)`
- `set_member_role(group_id, user_id, role, requester_id)` — promote/demote; requester must be an admin, `role` is `'admin' | 'member'`. (Was documented as `update_member_role`; corrected in #714.)
- `get_group_members(group_id)` → `GroupMember[]` — (was documented as `list_group_members`; corrected in #714.)
- `search_group_by_slug(slug)` → `Group` — finds the group whose name derives to `slug`; **errors** if there is no match (it is not a list-returning search). (Was documented as `search_groups(query)` → `Group[]`; corrected in #714.)
- Also registered but undocumented here: `update_group`, `delete_group`, `update_channel`, `delete_channel`, `get_group_join_requests`, `get_my_join_request`. See Appendix A.

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

## mls (`commands/mls/`)
Only three MLS commands are registered (`src-tauri/src/commands/mls.rs`):
- `process_pending_commits(conversation_id, user_id)`
- `poll_mls_welcomes(user_id)`
- `catch_up_all_mls_groups(user_id)`

Corrected 2026-08-03 (#714):
- ~~`reconcile_group_mls(conversation_id, actor_user_id)`~~ — **not a command**. Reconcile is internal: `pollis_core::commands::mls::reconcile_group_mls_impl`, driven by the sweep in `commands/mls/sweep.rs` and by `auth.rs`. It has no shim and no `generate_handler!` entry.
- ~~`generate_mls_key_package(user_id)`~~ — **does not exist** anywhere in the tree under that name.

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
- `is_current_device_registered(user_id)` → `bool`. The authoritative re-check behind the `device_revoked` inbox nudge: the nudge is per-user so it reaches every device, and only the one that answers `false` signs out (a forged nudge can't evict a valid device). `false` requires an ACTIVE row — revocation is a tombstone, so this filters `revoked_at IS NULL` (#685). Returns `true` when the device id isn't known yet, so early boot never self-logs-out.
- `reset_identity(user_id)` → new secret key. **Not a registered Tauri command** as of 2026-08-03 (#714): it is `pollis_core::commands::account_identity::reset_identity`, reached from the registered `reset_identity_and_recover`.
- Also registered in this module but undocumented here: `list_security_events`. See Appendix A.

## livekit (`commands/livekit.rs`)
- Tokens are minted by the DS now (#393) — no on-device signer. `get_livekit_token` and friends call `ds_livekit_token` (`POST /v1/livekit/token`); server-side fan-out/roster go through `ds_livekit_send_data` / `ds_livekit_participants`. The client holds no LiveKit API secret.
- `get_livekit_token(room_id, user_id, username)` → token string (identity/name derived server-side; the args are ignored)
- `subscribe_realtime(on_event: Channel)`
- `connect_rooms(room_ids, user_id, username)`

## voice (`commands/voice.rs`)
**Participant identity** is per-device: `voice-{user_id}:{device_id}` (device_id from `AppState.device_id`; falls back to legacy `voice-{user_id}` pre-login). This lets two devices of the same user coexist in one room instead of colliding on the SFU (#140) — mirrors the realtime/inbox scheme in `livekit/realtime.rs`. Parse the user back out with `voice::user_id_from_voice_identity` (Rust) / `voice/identity.ts` (frontend). The voice event loop **does not play back** audio tracks whose parsed user_id is the local user's (self-hear mute) but still shows them as participants. `RoomEvent::Disconnected` now fully tears down the room + mic stream via `release_voice_resources` (previously leaked).
- `prepare_voice_connection(channel_id, user_id, display_name)` — best-effort warmup fired on user "intent" (hover, route entry). Pre-fetches + caches the DS-minted voice token (identity derived server-side, so it matches the join) and runs a one-shot HTTPS probe to warm DNS / TLS / connection pool. Idempotent; safe to call eagerly. Consumed by the next `join_voice_channel` for the same channel + identity.
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
- ~~`presign_upload(key, content_type)`~~ — **does not exist** under that name as of 2026-08-03 (#714). Presigning is server-side: the DS secrets broker mints URLs via `POST /v1/r2/presign` (see `delete_r2_object` below and `docs/secrets-broker.md`).
- `get_media_url(r2_key, content_hash, content_type)` → URL — builds the loopback media-server URL (`http://127.0.0.1:<port>/<token>/<hash>`) for a cached item; errors if the server isn't started or there is no active unlock. See `pollis-core/src/media_server.rs`.
- `upload_media(path, filename, content_type)` / `download_media(r2_key, content_hash)` — convergent-encryption media path; dedups via `attachment_object` on Turso.
- Internal: `delete_r2_object(state, r2_key)` — DS-presigned DELETE (via `presign_r2`) used by `delete_message` to purge orphaned attachments. Treats 404 as success. The client holds no R2 credentials — every get/put/delete is presigned by the DS secrets broker (`POST /v1/r2/presign`, #393).

## overlay (`commands/overlay.rs`)
Runtime application of the closed-overlay relay mode (design `docs/relay-overlay-design.md` §14). Off-by-default; when off no shim runs and every network path is byte-for-byte the direct path.
- `get_overlay_mode()` → `"off" | "prefer" | "strict"` — the CURRENT live mode (the running shim's mode, or `off` when no shim).
- `set_overlay_mode(mode)` — parse + APPLY LIVE. Off→non-off builds the `RealRelayFactory`, starts the loopback SOCKS5 shim, and reconnects both remote DBs through it (`RemoteDb::set_overlay_shim`) so reqwest control-plane calls AND libsql route through the relay; non-off→Off tears the shim down and reconnects direct; Prefer↔Strict flips the shim's policy mode live (no restart, no DB reconnect). Idempotent — safe to call on every app start / login (the UI calls it after loading the saved preference; boot calls the same `apply_overlay_mode` with `POLLIS_OVERLAY`). On failure to bring the overlay up it rolls back to the previous working state and errors — never a half-routed app; Strict degrades (surfaced error) rather than silently going direct. Config: `POLLIS_OVERLAY` (mode), `POLLIS_OVERLAY_RELAY` (comma-sep relay endpoints — a POOL: `RealRelayFactory` tries them in health order and fails over to the first success, marking a failed relay dead for a cooldown, fail-open if all dead, single-hop), `POLLIS_OVERLAY_RELAY_CERT` (path or base64 DER of the pinned relay QUIC leaf). Persisting the choice is the settings-UI slice's job.
- **Frontend surface:** Preferences → "Network privacy (relay)" (`PreferencesPage.tsx`) — an Off/Prefer/Strict `radiogroup`. `overlay_mode` lives in `PreferencesData` (`usePreferences.ts`, synced blob, default `off`); selecting a mode persists it (`save_preferences`) and applies it live (`set_overlay_mode`); on login/restart the saved mode is re-applied once, guarded by a `get_overlay_mode` diff so an unchanged mode never reconnects the DBs. An apply error (e.g. Strict with no relay) surfaces inline and the control reflects the real resulting mode from `get_overlay_mode` — never a silent direct.

---

## Appendix A — full registered-command inventory (names only)

Mechanically extracted from `tauri::generate_handler![…]` in `src-tauri/src/lib.rs` at `d13c906` on **2026-08-03** (#714). **167 commands.** Grouped by the `commands::<module>::` path used at the registration site; **names only — no descriptions are given here because they were not verified.** A name in this list that has no prose above is real and callable; read its implementation in `pollis-core/src/commands/` before using it.

Regenerate with:

```bash
sed -n '/generate_handler!\[/,/^\s*\]) *$/p' src-tauri/src/lib.rs
```

- **`(lib.rs, no module)`** — `hide_window`, `read_clipboard_files`, `read_clipboard_image_to_temp`
- **`auth`** — `delete_account`, `dev_login`, `get_identity`, `get_session`, `initialize_identity`, `is_current_device_registered`, `list_known_accounts`, `list_user_devices`, `logout`, `request_email_change_otp`, `request_otp`, `revoke_device`, `verify_email_change`, `verify_otp`, `wipe_local_data`
- **`blocks`** — `block_user`, `list_blocked_users`, `unblock_user`
- **`camera`** — `list_video_devices`, `start_camera`, `start_camera_preview`, `stop_camera`, `stop_camera_preview`, `subscribe_camera_events`
- **`device_enrollment`** — `approve_device_enrollment`, `finalize_device_enrollment`, `list_pending_enrollment_requests`, `list_security_events`, `poll_enrollment_status`, `recover_with_secret_key`, `reject_device_enrollment`, `reset_identity_and_recover`, `start_device_enrollment`
- **`dm`** — `accept_dm_request`, `add_user_to_dm_channel`, `create_dm_channel`, `get_dm_channel`, `leave_dm_channel`, `list_dm_channels`, `list_dm_requests`, `remove_user_from_dm_channel`
- **`groups`** — `accept_group_invite`, `approve_join_request`, `create_channel`, `create_group`, `decline_group_invite`, `delete_channel`, `delete_group`, `get_group_join_requests`, `get_group_members`, `get_my_join_request`, `get_pending_invites`, `leave_group`, `list_group_channels`, `list_user_groups`, `list_user_groups_with_channels`, `reject_join_request`, `remove_member_from_group`, `request_group_access`, `search_group_by_slug`, `send_group_invite`, `set_member_role`, `update_channel`, `update_group`
- **`install_kind`** — `detect_managed_install`
- **`livekit`** — `cancel_call`, `connect_rooms`, `get_livekit_token`, `get_livekit_url`, `get_livekit_view_token`, `list_voice_participants`, `list_voice_room_counts`, `publish_ping`, `publish_typing`, `publish_voice_presence`, `start_call`, `subscribe_realtime`
- **`media_permissions`** — `get_media_permission_status`, `open_privacy_settings`, `revoke_media_permissions`, `set_revoke_media_on_exit`
- **`messages`** — `add_reaction`, `delete_message`, `edit_message`, `get_channel_messages`, `get_dm_messages`, `get_message_retention`, `get_reactions`, `ingest_channel_envelopes`, `ingest_dm_envelopes`, `list_channel_previews`, `list_messages`, `list_messages_by_sender`, `read_channel_messages`, `read_dm_messages`, `remove_reaction`, `run_message_eviction`, `search_messages`, `send_message`, `set_message_retention`
- **`mls`** — `catch_up_all_mls_groups`, `poll_mls_welcomes`, `process_pending_commits`
- **`overlay`** — `get_overlay_mode`, `set_overlay_mode`
- **`pin`** — `get_unlock_state`, `lock`, `set_pin`, `unlock`
- **`r2`** — `download_file`, `download_media`, `get_media_url`, `upload_file`, `upload_media`
- **`safety`** — `get_safety_number`, `list_peer_verifications`, `set_contact_verified`
- **`screenshare`** — `cancel_screen_share_picker`, `enumerate_screen_sources`, `screenshare_ws_url`, `start_screen_share`, `stop_screen_share`, `subscribe_screen_share_events`, `subscribe_screen_share_frames`
- **`sfx`** — `play_sfx`, `start_ring`, `stop_ring`
- **`terminal`** — `terminal_ack`, `terminal_close`, `terminal_open`, `terminal_resize`, `terminal_write`
- **`transparency`** — `audit_peer_account_key`, `self_audit_account_key`, `verify_own_build`
- **`tray`** — `tray_set_close_to_tray`, `tray_set_enabled`, `tray_set_unread`, `tray_set_voice_state`
- **`update`** — `is_update_required`, `mark_update_required`
- **`user`** — `get_preferences`, `get_user_profile`, `save_preferences`, `search_user_by_username`, `update_user_profile`
- **`voice`** — `get_last_join_timings`, `join_voice_channel`, `leave_voice_channel`, `list_audio_devices`, `prepare_voice_connection`, `set_remote_user_volume`, `set_voice_audio_processing`, `set_voice_input_device`, `set_voice_output_device`, `subscribe_voice_events`, `toggle_voice_mute`
- **`voice_test`** — `play_test_tone`, `record_and_play_back`, `set_mic_test_monitor`, `start_mic_test`, `stop_mic_test`, `stop_test_playback`, `subscribe_voice_test_events`

If this appendix and `src-tauri/src/lib.rs` disagree, `lib.rs` is right and this file is stale.

---
_Back to [index.md](./index.md)_
