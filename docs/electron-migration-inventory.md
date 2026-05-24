# Electron migration — Tauri surface inventory

Snapshot of every Tauri-coupled surface in the desktop app at the start of the
Electron port. All commands forward to `pollis_core::commands::*`; the Tauri
binary owns only shims, lifecycle, plugin wiring, and OS-window chrome. The
goal of porting is to replace the Tauri shim layer + frontend transport with an
Electron + napi-rs equivalent while leaving `pollis-core` untouched.

---

## 1. Tauri commands

All commands live in `src-tauri/src/commands/`. Each row's "core fn" column is
the function inside `pollis_core::commands::<module>` the shim forwards to.

Argument types use the Rust signature verbatim (minus the `state: State<'_,
Arc<AppState>>` parameter which is implicit on every command). `Result<T>` is
`crate::error::Result<T>` (a `Result<T, Error>` alias from
`pollis_core::error`).

### auth — `src-tauri/src/commands/auth.rs`

| Command | Args | Returns | Channel? | Core fn |
| --- | --- | --- | --- | --- |
| `initialize_identity` | `user_id: String` | `IdentityInfo` | no | `initialize_identity` |
| `get_identity` | — | `Option<IdentityInfo>` | no | `get_identity` |
| `request_otp` | `email: String` | `()` | no | `request_otp` |
| `verify_otp` | `email: String, code: String` | `UserProfile` | no | `verify_otp` |
| `request_email_change_otp` | `user_id: String, new_email: String` | `()` | no | `request_email_change_otp` |
| `verify_email_change` | `user_id: String, new_email: String, code: String` | `()` | no | `verify_email_change` |
| `dev_login` | `_email: String` | `UserProfile` | no | `dev_login` |
| `get_session` | — | `Option<UserProfile>` | no | `get_session` |
| `logout` | `delete_data: bool` | `()` | no | `logout` |
| `delete_account` | `user_id: String` | `()` | no | `delete_account` |
| `list_known_accounts` | — | `accounts::AccountsIndex` | no | `list_known_accounts` |
| `wipe_local_data` | — | `()` | no | `wipe_local_data` |
| `list_user_devices` | `user_id: String` | `Vec<serde_json::Value>` | no | `list_user_devices` |
| `revoke_device` | `user_id: String, device_id: String` | `()` | no | `revoke_device` |

### pin — `src-tauri/src/commands/pin.rs`

| Command | Args | Returns | Channel? | Core fn |
| --- | --- | --- | --- | --- |
| `set_pin` | `old_pin: Option<String>, new_pin: String` | `()` | no | `set_pin` |
| `unlock` | `user_id: String, pin: String` | `UnlockOutcome` | no | `unlock` |
| `lock` | — | `()` | no | `lock` |
| `get_unlock_state` | — | `UnlockStateSnapshot` | no | `get_unlock_state` |

### device_enrollment — `src-tauri/src/commands/device_enrollment.rs`

| Command | Args | Returns | Channel? | Core fn |
| --- | --- | --- | --- | --- |
| `start_device_enrollment` | `user_id: String` | `EnrollmentHandle` | no | `start_device_enrollment` |
| `poll_enrollment_status` | `request_id: String` | `EnrollmentStatus` | no | `poll_enrollment_status` |
| `list_pending_enrollment_requests` | `user_id: String` | `Vec<PendingEnrollmentRequest>` | no | `list_pending_enrollment_requests` |
| `approve_device_enrollment` | `request_id: String, verification_code: String` | `()` | no | `approve_device_enrollment` |
| `reject_device_enrollment` | `request_id: String` | `()` | no | `reject_device_enrollment` |
| `recover_with_secret_key` | `user_id: String, secret_key: String` | `()` | no | `recover_with_secret_key` |
| `reset_identity_and_recover` | `user_id: String, confirm_email: String` | `String` | no | `reset_identity_and_recover` |
| `finalize_device_enrollment` | `user_id: String` | `()` | no | `finalize_device_enrollment` |
| `list_security_events` | `user_id: String, limit: Option<i64>` | `Vec<SecurityEvent>` | no | `list_security_events` |

### safety — `src-tauri/src/commands/safety.rs`

| Command | Args | Returns | Channel? | Core fn |
| --- | --- | --- | --- | --- |
| `get_safety_number` | `my_user_id: String, peer_user_id: String` | `SafetyNumberInfo` | no | `get_safety_number` |
| `set_contact_verified` | `peer_user_id: String, verified: bool` | `()` | no | `set_contact_verified` |
| `list_peer_verifications` | — | `Vec<PeerVerificationEntry>` | no | `list_peer_verifications` |

### user — `src-tauri/src/commands/user.rs`

| Command | Args | Returns | Channel? | Core fn |
| --- | --- | --- | --- | --- |
| `get_user_profile` | `user_id: String` | `Option<UserProfile>` | no | `get_user_profile` |
| `update_user_profile` | `user_id: String, username: Option<String>, preferred_name: Option<String>, phone: Option<String>, avatar_url: Option<String>` | `()` | no | `update_user_profile` |
| `search_user_by_username` | `username: String` | `Option<UserProfile>` | no | `search_user_by_username` |
| `get_preferences` | `user_id: String` | `String` | no | `get_preferences` |
| `save_preferences` | `user_id: String, preferences_json: String` | `()` | no | `save_preferences` |

### groups — `src-tauri/src/commands/groups.rs`

| Command | Args | Returns | Channel? | Core fn |
| --- | --- | --- | --- | --- |
| `list_user_groups` | `user_id: String` | `Vec<Group>` | no | `list_user_groups` |
| `list_user_groups_with_channels` | `user_id: String` | `Vec<GroupWithChannels>` | no | `list_user_groups_with_channels` |
| `list_group_channels` | `group_id: String` | `Vec<Channel>` | no | `list_group_channels` |
| `create_group` | `name: String, description: Option<String>, owner_id: String, create_default_text_channel: Option<bool>, create_default_voice_channel: Option<bool>` | `Group` | no | `create_group` |
| `create_channel` | `group_id: String, name: String, description: Option<String>, channel_type: Option<String>, _creator_id: String` | `Channel` | no | `create_channel` |
| `send_group_invite` | `group_id: String, inviter_id: String, invitee_identifier: String` | `()` | no | `send_group_invite` |
| `get_pending_invites` | `user_id: String` | `Vec<PendingInvite>` | no | `get_pending_invites` |
| `accept_group_invite` | `invite_id: String, user_id: String` | `()` | no | `accept_group_invite` |
| `decline_group_invite` | `invite_id: String, user_id: String` | `()` | no | `decline_group_invite` |
| `request_group_access` | `group_id: String, requester_id: String` | `()` | no | `request_group_access` |
| `get_group_join_requests` | `group_id: String, requester_id: String` | `Vec<JoinRequest>` | no | `get_group_join_requests` |
| `get_my_join_request` | `group_id: String, requester_id: String` | `Option<JoinRequest>` | no | `get_my_join_request` |
| `approve_join_request` | `request_id: String, approver_id: String` | `()` | no | `approve_join_request` |
| `reject_join_request` | `request_id: String, approver_id: String` | `()` | no | `reject_join_request` |
| `update_group` | `group_id: String, requester_id: String, name: Option<String>, description: Option<String>, icon_url: Option<String>` | `Group` | no | `update_group` |
| `delete_group` | `group_id: String, requester_id: String` | `()` | no | `delete_group` |
| `get_group_members` | `group_id: String` | `Vec<GroupMember>` | no | `get_group_members` |
| `remove_member_from_group` | `group_id: String, user_id: String, requester_id: String` | `()` | no | `remove_member_from_group` |
| `leave_group` | `group_id: String, user_id: String` | `()` | no | `leave_group` |
| `update_channel` | `channel_id: String, requester_id: String, name: Option<String>, description: Option<String>` | `Channel` | no | `update_channel` |
| `delete_channel` | `channel_id: String, requester_id: String` | `()` | no | `delete_channel` |
| `set_member_role` | `group_id: String, user_id: String, role: String, requester_id: String` | `()` | no | `set_member_role` |
| `search_group_by_slug` | `slug: String` | `Group` | no | `search_group_by_slug` |

### dm — `src-tauri/src/commands/dm.rs`

| Command | Args | Returns | Channel? | Core fn |
| --- | --- | --- | --- | --- |
| `create_dm_channel` | `creator_id: String, member_ids: Vec<String>` | `DmChannel` | no | `create_dm_channel` |
| `list_dm_channels` | `user_id: String` | `Vec<DmChannel>` | no | `list_dm_channels` |
| `list_dm_requests` | `user_id: String` | `Vec<DmChannel>` | no | `list_dm_requests` |
| `accept_dm_request` | `dm_channel_id: String, user_id: String` | `()` | no | `accept_dm_request` |
| `get_dm_channel` | `dm_channel_id: String` | `DmChannel` | no | `get_dm_channel` |
| `add_user_to_dm_channel` | `dm_channel_id: String, user_id: String, added_by: String` | `()` | no | `add_user_to_dm_channel` |
| `remove_user_from_dm_channel` | `dm_channel_id: String, user_id: String, requester_id: String` | `()` | no | `remove_user_from_dm_channel` |
| `leave_dm_channel` | `dm_channel_id: String, user_id: String` | `()` | no | `leave_dm_channel` |

### blocks — `src-tauri/src/commands/blocks.rs`

| Command | Args | Returns | Channel? | Core fn |
| --- | --- | --- | --- | --- |
| `block_user` | `blocker_id: String, blocked_id: String` | `()` | no | `block_user` |
| `unblock_user` | `blocker_id: String, blocked_id: String` | `()` | no | `unblock_user` |
| `list_blocked_users` | `user_id: String` | `Vec<BlockedUser>` | no | `list_blocked_users` |

### messages — `src-tauri/src/commands/messages.rs`

| Command | Args | Returns | Channel? | Core fn |
| --- | --- | --- | --- | --- |
| `list_messages` | `conversation_id: String, limit: Option<i64>, before_id: Option<String>` | `Vec<Message>` | no | `list_messages` |
| `send_message` | `conversation_id: String, sender_id: String, content: String, reply_to_id: Option<String>, sender_username: Option<String>` | `Message` | no | `send_message` |
| `get_channel_messages` | `user_id: String, channel_id: String, limit: Option<i64>, cursor: Option<MessageCursor>` | `MessagePage` | no | `get_channel_messages` |
| `get_dm_messages` | `user_id: String, dm_channel_id: String, limit: Option<i64>, cursor: Option<MessageCursor>` | `MessagePage` | no | `get_dm_messages` |
| `read_channel_messages` | `channel_id: String, limit: Option<i64>, cursor: Option<MessageCursor>` | `MessagePage` | no | `read_channel_messages` |
| `read_dm_messages` | `dm_channel_id: String, limit: Option<i64>, cursor: Option<MessageCursor>` | `MessagePage` | no | `read_dm_messages` |
| `ingest_channel_envelopes` | `user_id: String, channel_id: String` | `()` | no | `ingest_channel_envelopes` |
| `ingest_dm_envelopes` | `user_id: String, dm_channel_id: String` | `()` | no | `ingest_dm_envelopes` |
| `list_messages_by_sender` | `sender_id: String` | `Vec<MessageWithContext>` | no | `list_messages_by_sender` |
| `list_channel_previews` | `user_id: String` | `Vec<ChannelPreview>` | no | `list_channel_previews` |
| `search_messages` | `query: String, limit: Option<i64>` | `Vec<SearchResult>` | no | `search_messages` |
| `add_reaction` | `message_id: String, user_id: String, emoji: String` | `()` | no | `add_reaction` |
| `remove_reaction` | `message_id: String, user_id: String, emoji: String` | `()` | no | `remove_reaction` |
| `get_reactions` | `message_id: String` | `Vec<Reaction>` | no | `get_reactions` |
| `delete_message` | `message_id: String, user_id: String` | `()` | no | `delete_message` |
| `edit_message` | `conversation_id: String, message_id: String, user_id: String, new_content: String` | `()` | no | `edit_message` |

### mls — `src-tauri/src/commands/mls.rs`

| Command | Args | Returns | Channel? | Core fn |
| --- | --- | --- | --- | --- |
| `generate_mls_key_package` | `user_id: String` | `serde_json::Value` | no | `generate_mls_key_package` |
| `publish_mls_key_package` | `user_id: String, ref_hex: String, key_package_bytes: Vec<u8>` | `()` | no | `publish_mls_key_package` |
| `fetch_mls_key_package` | `target_user_id: String` | `Option<Vec<u8>>` | no | `fetch_mls_key_package` |
| `create_mls_group` | `conversation_id: String, creator_user_id: String` | `()` | no | `create_mls_group` |
| `process_welcome` | `welcome_bytes: Vec<u8>` | `()` | no | `process_welcome` |
| `poll_mls_welcomes` | `user_id: String` | `()` | no | `poll_mls_welcomes` |
| `reconcile_group_mls` | `conversation_id: String, actor_user_id: String` | `()` | no | `reconcile_group_mls` |
| `process_pending_commits` | `conversation_id: String, user_id: String` | `()` | no | `process_pending_commits` |

### livekit — `src-tauri/src/commands/livekit.rs`

| Command | Args | Returns | Channel? | Core fn |
| --- | --- | --- | --- | --- |
| `get_livekit_token` | `room_name: String, identity: String, display_name: String` | `String` | no | `get_livekit_token` |
| `get_livekit_url` | — | `String` | no | `get_livekit_url` |
| `subscribe_realtime` | `on_event: Channel<RealtimeEvent>` | `()` | **yes** — `RealtimeEvent` | `subscribe_realtime` |
| `connect_rooms` | `room_ids: Vec<String>, user_id: String, username: String` | `()` | no | `connect_rooms` |
| `publish_ping` | `room_id: String, channel_id: Option<String>, conversation_id: Option<String>, sender_id: String, sender_username: Option<String>` | `()` | no | `publish_ping` |
| `publish_typing` | `room_id: String, channel_id: Option<String>, conversation_id: Option<String>, user_id: String, username: Option<String>, is_typing: bool` | `()` | no | `publish_typing` |
| `publish_voice_presence` | `group_id: String, channel_id: String, user_id: String, display_name: String, joined: bool` | `()` | no | `publish_voice_presence` |
| `list_voice_participants` | `channel_id: String` | `Vec<VoiceParticipantInfo>` | no | `list_voice_participants` |
| `list_voice_room_counts` | `channel_ids: Vec<String>` | `Vec<VoiceRoomCount>` | no | `list_voice_room_counts` |
| `start_call` | `callee_id: String, caller_id: String, caller_username: String` | `StartCallResult` | no | `start_call` |
| `cancel_call` | `other_user_id: String, call_id: String` | `()` | no | `cancel_call` |

### voice — `src-tauri/src/commands/voice.rs`

| Command | Args | Returns | Channel? | Core fn |
| --- | --- | --- | --- | --- |
| `subscribe_voice_events` | `on_event: Channel<VoiceEvent>` | `()` | **yes** — `VoiceEvent` | `subscribe_voice_events` |
| `list_audio_devices` | — | `Vec<AudioDevice>` | no | `list_audio_devices` |
| `prepare_voice_connection` | `channel_id: String, user_id: String, display_name: String` | `()` | no | `prepare_voice_connection` |
| `join_voice_channel` | `channel_id: String, user_id: String, display_name: String, input_device: Option<String>, output_device: Option<String>, audio_processing: voice_apm::ApmConfig, counterparty_user_id: Option<String>` | `()` | no | `join_voice_channel` |
| `leave_voice_channel` | — | `()` | no | `leave_voice_channel` |
| `toggle_voice_mute` | — | `bool` | no | `toggle_voice_mute` |
| `set_remote_user_volume` | `user_id: String, volume: f32` | `()` | no | `set_remote_user_volume` |
| `set_voice_input_device` | `device_name: String` | `()` | no | `set_voice_input_device` |
| `set_voice_output_device` | `device_name: String` | `()` | no | `set_voice_output_device` |
| `set_voice_audio_processing` | `config: voice_apm::ApmConfig` | `()` | no | `set_voice_audio_processing` |
| `get_last_join_timings` | — | `Option<JoinTimings>` | no | `get_last_join_timings` |

### voice_test — `src-tauri/src/commands/voice_test.rs`

| Command | Args | Returns | Channel? | Core fn |
| --- | --- | --- | --- | --- |
| `subscribe_voice_test_events` | `on_event: Channel<VoiceTestEvent>` | `()` | **yes** — `VoiceTestEvent` | `subscribe_voice_test_events` |
| `start_mic_test` | `input_device_id: String, output_device_id: String, monitor: bool` | `()` | no | `start_mic_test` |
| `set_mic_test_monitor` | `enabled: bool, output_device_id: String` | `()` | no | `set_mic_test_monitor` |
| `stop_mic_test` | — | `()` | no | `stop_mic_test` |
| `record_and_play_back` | `input_device_id: String, output_device_id: String, duration_ms: u32` | `()` | no | `record_and_play_back` |
| `play_test_tone` | `output_device_id: String, kind: String` | `()` | no | `play_test_tone` |
| `stop_test_playback` | — | `()` | no | `stop_test_playback` |

### screenshare — `src-tauri/src/commands/screenshare.rs`

| Command | Args | Returns | Channel? | Core fn |
| --- | --- | --- | --- | --- |
| `subscribe_screen_share_events` | `on_event: Channel<ScreenShareEvent>` | `()` | **yes** — `ScreenShareEvent` | `subscribe_screen_share_events` |
| `subscribe_screen_share_frames` | `on_frame: Channel<InvokeResponseBody>` | `()` | **yes** — `InvokeResponseBody::Raw` (binary BGRx/I420 frame packets — see Section 2) | `subscribe_screen_share_frames` |
| `enumerate_screen_sources` | — | `pollis_capture_proto::SourceList` | no | `enumerate_screen_sources` |
| `cancel_screen_share_picker` | — | `()` | no | `cancel_screen_share_picker` |
| `start_screen_share` | `selection: Option<pollis_capture_proto::Selection>` | `()` | no | `start_screen_share` |
| `stop_screen_share` | — | `()` | no | `stop_screen_share` |

### r2 — `src-tauri/src/commands/r2.rs`

| Command | Args | Returns | Channel? | Core fn |
| --- | --- | --- | --- | --- |
| `upload_file` | `key: String, data: Vec<u8>, content_type: String` | `UploadResult` | no | `upload_file` |
| `upload_media` | `path: String, filename: String, content_type: String` | `MediaUploadResult` | no | `upload_media` |
| `download_file` | `key: String` | `Vec<u8>` | no | `download_file` |
| `download_media` | `r2_key: String, content_hash: String` | `Vec<u8>` | no | `download_media` |
| `get_media_url` | `r2_key: String, content_hash: String, content_type: String` | `String` | no | `get_media_url` |

### sfx — `src-tauri/src/commands/sfx.rs`

| Command | Args | Returns | Channel? | Core fn |
| --- | --- | --- | --- | --- |
| `play_sfx` | `sound: &str` | `()` (sync) | no | `play_sfx` |
| `start_ring` | — | `()` (sync) | no | `start_ring` |
| `stop_ring` | — | `()` (sync) | no | `stop_ring` |

### terminal — `src-tauri/src/commands/terminal.rs`

| Command | Args | Returns | Channel? | Core fn |
| --- | --- | --- | --- | --- |
| `terminal_open` | `rows: u16, cols: u16, on_output: Channel<InvokeResponseBody>` | `String` (terminal id) | **yes** — `InvokeResponseBody::Raw` (raw PTY bytes) | `terminal_open` |
| `terminal_write` | (Tauri-specific) `request: tauri::ipc::Request<'_>` with raw body + `x-terminal-id` header | `()` | no | `terminal_write` |
| `terminal_resize` | `terminal_id: String, rows: u16, cols: u16` | `()` | no | `terminal_resize` |
| `terminal_close` | `terminal_id: String` | `()` | no | `terminal_close` |
| `terminal_ack` | `terminal_id: String, bytes: usize` | `()` | no | `terminal_ack` |

`terminal_write` is the **only** command that uses a raw IPC body request (not
JSON). Frontend sends the encoded keystroke `Uint8Array` as the body and the
terminal id in `x-terminal-id`. This avoids `Array.from(uint8) → JSON number
array → Vec<u8>` on every keypress. The Electron port needs an equivalent
binary-args channel here.

### update — `src-tauri/src/commands/update.rs`

| Command | Args | Returns | Channel? | Core fn |
| --- | --- | --- | --- | --- |
| `mark_update_required` | — | `()` | no | `mark_update_required` |
| `is_update_required` | — | `bool` | no | `is_update_required` |

### install_kind — `src-tauri/src/commands/install_kind.rs`

| Command | Args | Returns | Channel? | Core fn |
| --- | --- | --- | --- | --- |
| `detect_managed_install` | — | `Option<ManagedInstallInfo>` | no | (local — uses `tauri::utils::platform::bundle_type`) |

**Tauri-specific** — reads `tauri::utils::config::BundleType` to decide whether
the running binary is AUR-packaged. Port must replace `bundle_type()` with an
Electron equivalent (e.g. check `process.execPath` or pass a build-time flag),
because `tauri::utils` won't exist after the port.

### top-level shims in `src-tauri/src/lib.rs`

| Command | Args | Returns | Channel? | Notes |
| --- | --- | --- | --- | --- |
| `read_clipboard_files` | `app: tauri::AppHandle` | `Vec<String>` | no | macOS: shells out to `osascript` for NSPasteboard file URLs. Linux/Windows: reads `text/uri-list` via `tauri-plugin-clipboard-manager`. |
| `read_clipboard_image_to_temp` | `app: tauri::AppHandle` | `String` (temp file path) | no | Uses `tauri-plugin-clipboard-manager`'s `read_image()`, encodes to PNG via `image` crate, writes to `std::env::temp_dir()`. |
| `hide_window` | `window: tauri::Window` | `()` | no | macOS: `window.hide()`. Other: `window.close()`. |

---

## 2. Channel event types

Every `T` that flows through `tauri::ipc::Channel<T>` from section 1. All
serialize with `#[serde(tag = "type", rename_all = "snake_case")]` unless
noted. Frontend reads these as discriminated unions on `payload.type`.

### `RealtimeEvent` — `pollis-core/src/realtime.rs` (line 16)

```rust
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RealtimeEvent {
    NewMessage {
        channel_id: Option<String>,
        conversation_id: Option<String>,
        sender_id: String,
        sender_username: Option<String>,
    },
    DmCreated { conversation_id: String },
    MembershipChanged {
        conversation_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>, // "invite" | "approval" | None
    },
    VoiceJoined {
        channel_id: String,
        user_id: String,
        display_name: String,
    },
    VoiceLeft {
        channel_id: String,
        user_id: String,
    },
    MemberRoleChanged { group_id: String },
    EditedMessage {
        channel_id: Option<String>,
        conversation_id: Option<String>,
        message_id: String,
        sender_id: String,
    },
    DeletedMessage {
        channel_id: Option<String>,
        conversation_id: Option<String>,
        message_id: String,
        deleted_by: String,
    },
    EnrollmentRequested {
        request_id: String,
        new_device_id: String,
        verification_code: String,
    },
    RealtimeReconnected { room_id: String },
    CallInvite {
        call_id: String,
        room_name: String,
        caller_id: String,
        caller_username: String,
    },
    CallCanceled { call_id: String },
    Typing {
        channel_id: Option<String>,
        conversation_id: Option<String>,
        user_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        username: Option<String>,
        is_typing: bool,
    },
    PresenceChanged {
        user_id: String,
        room_id: String,
        present: bool,
    },
    KeyChanged {
        peer_user_id: String,
        peer_identity_version: i64,
    },
}
```

### `VoiceEvent` — `pollis-core/src/commands/voice/types.rs` (line 80)

```rust
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VoiceEvent {
    ParticipantJoined {
        identity: String,
        name: String,
        is_muted: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        avatar_url: Option<String>,
    },
    ParticipantLeft { identity: String },
    Muted { identity: String },
    Unmuted { identity: String },
    SpeakingStarted { identity: String },
    SpeakingStopped { identity: String },
    ConnectionQualityChanged {
        identity: String,
        quality: String, // "excellent" | "good" | "poor" | "lost"
    },
    Disconnected,
}
```

### `VoiceTestEvent` — `pollis-core/src/commands/voice_test.rs` (line 31)

```rust
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VoiceTestEvent {
    Frame { peak: f32, rms: f32 }, // both normalized 0.0..1.0
    RecordingStarted,
    RecordingFinished,
    PlaybackStarted,
    PlaybackFinished,
}
```

### `ScreenShareEvent` — `pollis-core/src/commands/screenshare.rs` (line 62)

```rust
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScreenShareEvent {
    LocalStarted { width: u32, height: u32 },
    LocalStopped,
    LocalError { message: String },
    LocalUnsupported { message: String },
    RemoteStarted {
        track_key: String,
        identity: String,
        width: u32,
        height: u32,
    },
    RemoteStopped { track_key: String },
}
```

### Screenshare frame channel — `Channel<InvokeResponseBody>` (raw bytes)

`subscribe_screen_share_frames` ships I420 frames as
`InvokeResponseBody::Raw(Vec<u8>)`. The payload is a packed binary frame
(header + Y/U/V planes) — NOT JSON. The Electron port must surface a
zero-copy binary IPC path here (e.g. `MessagePort` `transferList`, or `Buffer`
through a napi callback) so multi-MB frames don't go through JSON or get
copied through V8 heap.

### Terminal output channel — `Channel<InvokeResponseBody>` (raw bytes)

`terminal_open`'s `on_output` ships PTY stdout as
`InvokeResponseBody::Raw(Vec<u8>)`. Same binary-IPC requirement as
screenshare frames — keystroke latency is sensitive to copy overhead.

### Other types in command signatures (not via Channel)

These appear as `Args` or `Returns` above and are also re-exported from
`pollis_core::commands::*` via the shims' `pub use`. Listed here so the
napi-binding work has a one-stop map of what needs serde→napi conversion:

- `auth`: `IdentityInfo`, `UserProfile`, `accounts::AccountsIndex`
- `pin`: `UnlockOutcome`, `UnlockStateSnapshot`
- `device_enrollment`: `EnrollmentHandle`, `EnrollmentStatus`, `PendingEnrollmentRequest`, `SecurityEvent`
- `safety`: `SafetyNumberInfo`, `PeerVerificationEntry`
- `user`: `UserProfile`
- `groups`: `Group`, `GroupWithChannels`, `Channel`, `PendingInvite`, `JoinRequest`, `GroupMember`
- `dm`: `DmChannel`
- `blocks`: `BlockedUser`
- `messages`: `Message`, `MessagePage`, `MessageCursor`, `MessageWithContext`, `ChannelPreview`, `SearchResult`, `Reaction`
- `livekit`: `VoiceParticipantInfo`, `VoiceRoomCount`, `StartCallResult`
- `voice`: `AudioDevice`, `JoinTimings`, `voice_apm::ApmConfig`
- `screenshare`: `pollis_capture_proto::SourceList`, `pollis_capture_proto::Selection` (re-exported through `src-tauri/Cargo.toml` because they are part of the public command signature)
- `r2`: `UploadResult`, `MediaUploadResult`
- `install_kind`: `ManagedInstallInfo`, `ManagedInstallKind`

---

## 3. Frontend Tauri API usage

Distinct API surfaces under `@tauri-apps/api/*` and `@tauri-apps/plugin-*` in
`frontend/src/`. `event.listen`/`event.emit` are **not** used — all backend →
frontend messaging goes through `Channel<T>`.

| API | Files |
| --- | --- |
| `invoke` from `@tauri-apps/api/core` | `App.tsx`, `components/Layout/AppShell.tsx`, `components/Layout/MainContent.tsx`, `components/TerminalView.tsx`, `components/UpdateScreen.tsx`, `components/Voice/RemoteUserVolumeSlider.tsx`, `components/ui/ChatInput.tsx`, `hooks/queries/useBlocks.ts`, `hooks/queries/useGroups.ts`, `hooks/queries/useMessages.ts`, `hooks/queries/usePreferences.ts`, `hooks/queries/useReactions.ts`, `hooks/queries/useSearchMessages.ts`, `hooks/queries/useUserProfile.ts`, `hooks/queries/useVoiceParticipants.ts`, `hooks/useLiveKitRealtime.ts`, `hooks/useTypingPublisher.ts`, `hooks/useVoiceTest.ts`, `pages/Call.tsx`, `pages/CreateChannel.tsx`, `pages/CreateGroup.tsx`, `pages/DM.tsx`, `pages/Preferences.tsx`, `pages/SearchGroup.tsx`, `pages/Settings.tsx`, `pages/VoiceSettingsPage.tsx`, `screenshare/screenShareSession.ts`, `services/api.ts`, `services/r2-upload.ts`, `utils/notify.ts`, `utils/sfx.ts`, `utils/voiceWarmup.ts`, `voice/voiceBridge.ts`, `voice/VoiceSessionManager.ts` |
| `Channel` from `@tauri-apps/api/core` | `components/TerminalView.tsx`, `hooks/useLiveKitRealtime.ts`, `hooks/useVoiceTest.ts`, `screenshare/screenShareSession.ts`, `voice/VoiceSessionManager.ts` |
| `convertFileSrc` from `@tauri-apps/api/core` (dynamic import) | `pages/Settings.tsx` |
| `getCurrentWindow` from `@tauri-apps/api/window` (methods: `minimize`, `toggleMaximize`, `close`, `startDragging`, `onDragDropEvent`, `setBadgeCount`, `setIcon`, `center`, `setSize`, `setPosition`, `innerSize`, `outerPosition`, `scaleFactor`, `onResized`, `onMoved`) | `components/Layout/AppShell.tsx`, `components/Layout/TitleBar.tsx`, `hooks/useBadge.ts`, `hooks/useWindowState.ts` |
| `availableMonitors` from `@tauri-apps/api/window` | `hooks/useWindowState.ts` |
| `LogicalSize`, `LogicalPosition` from `@tauri-apps/api/dpi` | `hooks/useWindowState.ts` |
| `Image` from `@tauri-apps/api/image` | `hooks/useBadge.ts` |
| `getVersion` from `@tauri-apps/api/app` | `pages/Settings.tsx` |
| `tempDir` from `@tauri-apps/api/path` | `components/ui/ChatInput.tsx` |
| `check` from `@tauri-apps/plugin-updater` | `components/UpdateScreen.tsx`, `pages/Settings.tsx` |
| `relaunch`, `exit` from `@tauri-apps/plugin-process` | `components/UpdateScreen.tsx` (`relaunch`), `pages/Root.tsx` (`exit`) |
| `open` from `@tauri-apps/plugin-dialog` | `components/ui/ChatInput.tsx` |
| `save` (as `saveDialog`) from `@tauri-apps/plugin-dialog` | `components/Message/MessageItem.tsx` |
| `open` from `@tauri-apps/plugin-shell` | `components/Message/MediaLinkUnfurl.tsx`, `components/ui/LinkifiedText.tsx` |
| `writeFile` from `@tauri-apps/plugin-fs` | `components/Message/MessageItem.tsx`, `components/ui/ChatInput.tsx` |
| `readFile`, `stat` from `@tauri-apps/plugin-fs` | `components/ui/ChatInput.tsx` |
| `invoke('plugin:notification\|notify')` (raw plugin invoke — no JS import) | `utils/notify.ts` |
| `invoke('plugin:notification\|is_permission_granted')` (raw plugin invoke) | `pages/Preferences.tsx` |
| `invoke('plugin:notification\|request_permission')` (raw plugin invoke) | `pages/Preferences.tsx` |

Test-only stubs in `frontend/src/__mocks__/tauri-core.ts` and
`frontend/src/__mocks__/tauri-event.ts` — these get rewritten/removed by the
port, not migrated.

`navigator.clipboard.writeText` is used as a non-Tauri fallback in
`components/ManagedInstallScreen.tsx` and `components/Auth/SaveSecretKeyScreen.tsx`.

---

## 4. tauri-plugin-* deps

From `src-tauri/Cargo.toml` (lines 49-56) and `src-tauri/src/lib.rs` (init
calls at lines 292-299).

| Crate | `Cargo.toml` ver | Builder wiring (`lib.rs`) | Notes |
| --- | --- | --- | --- |
| `tauri-plugin-shell` | 2 | `.plugin(tauri_plugin_shell::init())` — line 292 | Frontend uses `shell.open` to open URLs in system browser. |
| `tauri-plugin-dialog` | 2 | `.plugin(tauri_plugin_dialog::init())` — line 293 | Frontend uses `dialog.open` (file picker) and `dialog.save` (save-as). |
| `tauri-plugin-fs` | 2 | `.plugin(tauri_plugin_fs::init())` — line 294 | Frontend uses `fs.readFile`, `fs.writeFile`, `fs.stat`. |
| `tauri-plugin-notification` | 2 | `.plugin(tauri_plugin_notification::init())` — line 295 | Used via raw `invoke('plugin:notification\|notify' / 'is_permission_granted' / 'request_permission')`. No JS-side import. |
| `tauri-plugin-os` | 2 | `.plugin(tauri_plugin_os::init())` — line 296 | Listed under permissions; no direct JS import was found (registered for future use / capability scope). |
| `tauri-plugin-process` | 2 | `.plugin(tauri_plugin_process::init())` — line 297 | `process.exit` (Root.tsx), `process.relaunch` (UpdateScreen.tsx). |
| `tauri-plugin-updater` | 2 | `.plugin(tauri_plugin_updater::Builder::new().build())` — line 298 | `updater.check` (UpdateScreen.tsx, Settings.tsx). Endpoints + minisign pubkey in `tauri.conf.json` `plugins.updater`. |
| `tauri-plugin-clipboard-manager` | 2 | `.plugin(tauri_plugin_clipboard_manager::init())` — line 299 | Used **server-side** by `read_clipboard_files` (`app.clipboard().read_text()`) and `read_clipboard_image_to_temp` (`app.clipboard().read_image()`) — see lib.rs L174, L202. No JS-side import. |

No tray, autostart, deep-link, single-instance, http, or global-shortcut Tauri
plugins are used. Global shortcuts are implemented in the renderer via
DOM-level keydown listeners (`useGlobalShortcut` hook), not through a Tauri
plugin.

---

## 5. tauri.conf.json highlights

`src-tauri/tauri.conf.json`.

| Setting | Value |
| --- | --- |
| App identifier | `com.pollis.app` |
| Product name | `Pollis` |
| Version | `1.0.43` |
| Deep-link schemes | **none** (no `plugins.deep-link` entry) |
| `frontendDist` | `../frontend/dist` |
| `devUrl` | `http://localhost:5173` |
| `beforeDevCommand` | `pnpm --filter frontend dev` |
| `beforeBuildCommand` | `pnpm --filter frontend build` |
| `withGlobalTauri` | `false` |
| Window label | `main` |
| Window default size | 650×480 |
| Window minimum size | 420×360 |
| Window decorations | `false` (custom title bar via `TitleBar.tsx`) |
| Window transparent | `false` |
| Window resizable | `true` |
| Window fullscreen | `false` |
| Window shadow | `true` |
| Security CSP | `null` (no CSP set) |
| `assetProtocol.enable` | `true` (scope empty) |
| Bundle targets | `"all"` (every target supported by Tauri for the platform) |
| Bundle icons | `icons/32x32.png`, `64x64.png`, `128x128.png`, `128x128@2x.png`, `icon.png`, `icon.ico`, `AppIcon.icns` |
| macOS signing identity | `Developer ID Application: Daniel Kral (9JF7WWYMU2)` |
| macOS minimum system version | `14.0` |
| macOS `infoPlist` | `Info.plist` |
| macOS `entitlements` | `Entitlements.plist` |
| Linux `deb.depends` | `libasound2`, `libpulse0`, `libdbus-1-3` |
| Linux `rpm.depends` | `libasound.so.2()(64bit)`, `libpulse.so.0()(64bit)`, `libdbus-1.so.3()(64bit)` |
| Linux `appimage.bundleMediaFramework` | `true` |
| `createUpdaterArtifacts` | `true` |
| Updater endpoints | `https://cdn.pollis.com/releases/update-{{bundle_type}}.json`, `https://cdn.pollis.com/releases/update.json` |
| Updater pubkey | (minisign base64 — `dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDUzREVFMzNCOEVFRjIxOEQK…`) |

### `Info.plist` entries

- `NSUserNotificationAlertStyle` = `alert`
- `NSMicrophoneUsageDescription` = "Pollis needs microphone access for voice channels"
- `NSScreenCaptureUsageDescription` = "Pollis needs screen recording access to share your screen in voice channels"

### `Entitlements.plist` entries

- `com.apple.security.device.audio-input` = `true`
- `com.apple.security.cs.allow-unsigned-executable-memory` = `true` (required by libwebrtc)
- `com.apple.security.cs.allow-jit` = `true` (required by libwebrtc)

No NSCameraUsageDescription is set — Pollis is audio-only currently. No Linux
`.desktop` file lives in `src-tauri/`; Tauri generates one at bundle time.

---

## 6. capabilities/

One file: `src-tauri/capabilities/default.json`.

| File | Windows | Permissions granted |
| --- | --- | --- |
| `default.json` | `main` | `core:default`, `core:window:allow-start-dragging`, `core:window:allow-minimize`, `core:window:allow-toggle-maximize`, `core:window:allow-close`, `core:window:allow-inner-size`, `core:window:allow-outer-position`, `core:window:allow-set-size`, `core:window:allow-set-position`, `core:window:allow-scale-factor`, `core:window:allow-is-focused`, `core:window:allow-set-badge-count`, `core:window:allow-set-icon`, `core:image:allow-from-bytes`, `shell:allow-open`, `dialog:default`, `fs:default`, `fs:allow-temp-write`, `notification:default`, `os:default`, `process:allow-exit`, `process:allow-restart`, `updater:default`, `clipboard-manager:allow-read-text` |

Platforms: `linux`, `macOS`, `windows`. No per-platform / per-window
restrictions beyond this single capability set.

---

## Notes for porting agents

- **Channel<T> is the only Rust→frontend event mechanism in use.** No
  `event.emit` / `event.listen` anywhere. Five Channels exist: realtime, voice,
  voice_test, screenshare_events, screenshare_frames, terminal_output. The
  napi binding must expose a callback-per-subscription API on each subscribe.
- **Two channels carry raw binary payloads, not JSON.**
  `subscribe_screen_share_frames` and `terminal_open`'s `on_output` use
  `InvokeResponseBody::Raw`. These are perf-critical paths — port them as
  zero-copy `Buffer` callbacks, not JSON IPC.
- **`terminal_write` also uses a raw IPC body** with the terminal id in an
  `x-terminal-id` header. Keystroke-rate path; needs equivalent binary-args
  IPC in Electron.
- **The on-disk media cache uses a Rust-side loopback HTTP server**, not the
  Tauri asset protocol, even though `assetProtocol.enable: true` is set in
  `tauri.conf.json`. The server is spawned in `lib.rs` setup and stored at
  `state.media_server_port`. Frontend embeds `http://127.0.0.1:<port>/<…>`
  URLs. Port this verbatim — do not move it to Electron's protocol handlers.
- **macOS, Linux, and Windows all need custom window chrome.** The Tauri
  window is `decorations: false` with a frontend title bar (`TitleBar.tsx`),
  rounded corners applied via AppKit (`apply_macos_rounded_corners`) and DWM
  (`apply_windows_rounded_corners`), and macOS close behavior overridden to
  hide-not-quit (`hide_on_close`). Electron equivalents: `frame: false` +
  `titleBarStyle: 'hiddenInset'` on macOS, manual DWM call on Windows.
- **`install_kind` reads `tauri::utils::config::BundleType`** to decide if the
  AUR owns the install. This won't exist post-port — pass install kind in at
  build time (env var or bundled JSON).
- **`read_clipboard_files` shells out to `osascript` on macOS** to read
  `public.file-url` from NSPasteboard. Linux/Windows paths use
  `tauri-plugin-clipboard-manager`'s `read_text()`. Electron's `clipboard`
  module exposes `readBuffer('public.file-url')` directly on macOS, so the
  osascript hack can probably be removed during the port.
- **Linux startup applies two env vars** (`WEBKIT_DISABLE_DMABUF_RENDERER=1`,
  `GST_AUDIO_SINK=pulsesink`) and one runtime call (`setrlimit RLIMIT_NOFILE`).
  Electron on Linux uses Chromium, not WebKitGTK — the WebKit-specific env
  vars become irrelevant. `setrlimit` is still needed because Chromium + cpal
  + libsql + reqwest hit the same FD pressure.
- **Linux WebKit also has `enable_webrtc` + `enable_media_stream` toggled on
  at runtime** (lib.rs L362-370). Chromium has WebRTC enabled by default, so
  this also goes away — and the long-standing "Linux webview has no WebRTC"
  caveat in CLAUDE.md becomes moot for the renderer. Voice/screen-share still
  stay in Rust for the reasons in CLAUDE.md (no GC, predictable buffers).
- **No deep-link, no tray, no autostart, no single-instance.** Easy port
  surface — nothing to migrate here.
- The integration test harness (`src-tauri/src/test_harness.rs`) drives real
  `#[tauri::command]`s through `tauri::test::get_ipc_response`. After porting,
  either keep a Tauri-shaped test harness compiled with the existing shims, or
  migrate the harness to call `pollis_core::commands::*` directly (cleaner —
  the shims add no logic).
