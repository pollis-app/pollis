# Tauri Commands

All backend calls from the frontend use `invoke("command_name", { args })`. Commands are registered in `src-tauri/src/lib.rs` and implemented in `src-tauri/src/commands/`.

## auth (`commands/auth.rs`)
- `initialize_identity(user_id)` — ensure MLS credentials + KPs, poll welcomes
- `get_identity()` — check if MLS identity exists locally
- `request_otp(email)` — send OTP code to email
- `verify_otp(email, code)` → `AuthResult` — verify OTP, create session
- `get_session()` → `AuthResult | null` — check for existing session
- `logout(delete_data)` — clear session, optionally delete local data
- `delete_account(user_id)` — delete account from Turso + local
- `wipe_local_data()` — delete all local databases and keystore entries

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
- `delete_message(message_id, conversation_id, sender_id)`
- `search_messages(user_id, query, conversation_id?)` → `Message[]`

## dm (`commands/dm.rs`)
- `create_dm_channel(creator_id, peer_id)` → `DmChannel`
- `list_dm_conversations(user_id)` → `DmConversation[]`

## mls (`commands/mls.rs`)
- `reconcile_group_mls(conversation_id, actor_user_id)`
- `process_pending_commits(conversation_id, user_id)`
- `poll_mls_welcomes(user_id)`
- `generate_mls_key_package(user_id)` → JSON

## device_enrollment (`commands/device_enrollment.rs`)
- `start_device_enrollment(user_id)` → `EnrollmentHandle`
- `poll_enrollment_status(request_id)` → `EnrollmentStatus`
- `approve_device_enrollment(request_id, user_id, verification_code)`
- `reject_device_enrollment(request_id, user_id)`
- `list_pending_enrollment_requests(user_id)` → `PendingEnrollmentRequest[]`
- `recover_with_secret_key(user_id, secret_key)`
- `list_user_devices(user_id)` → `DeviceInfo[]`
- `reset_identity(user_id)` → new secret key

## livekit (`commands/livekit.rs`)
- `get_livekit_token(room_id, user_id, username)` → token string
- `subscribe_realtime(on_event: Channel)`
- `connect_rooms(room_ids, user_id, username)`

## voice (`commands/voice.rs`)
- `join_voice(channel_id, group_id, user_id, display_name)`
- `leave_voice(channel_id)`
- `set_voice_muted(muted)`
- `set_voice_deafened(deafened)`
- `switch_audio_device(kind, device_name)`
- `list_audio_devices()` → `AudioDevice[]`

## r2 (`commands/r2.rs`)
- `upload_file(data, key, content_type)` → URL
- `download_file(key)` → bytes
- `presign_upload(key, content_type)` → presigned URL

---
_Back to [index.md](./index.md)_
