# Schema

### users_new
- id: text (pk)
- email: text (required)
- username: text (required)
- phone: text
- identity_key: text
- avatar_url: text

### mls_key_package
- ref_hash: text (pk)

### mls_commit_log
- seq: integer (pk)
- conversation_id: text (required, fk)
- epoch: integer (required)
- commit_data: bytes (required)

### mls_welcome
- id: text (pk)
- recipient_id: text (required, fk)
- welcome_data: bytes (required)

### conversation_watermark
- conversation_id: text (required, fk)
- user_id: text (required, fk)
- device_id: text (required, fk)
- last_fetched_at: text (required)

### voice_presence
- user_id: text (required, fk)
- group_id: text (required, fk)
- channel_id: text (required, fk)
- display_name: text (required)
- joined_at: text (required)

### attachment_object
- content_hash: text (pk)
- r2_key: text (required)

### user_device
- device_id: text (pk, fk)
- user_id: text (required, fk)
- device_name: text
- last_seen: text (required)

### account_recovery
- user_id: text (pk, fk)
- identity_version: integer (required)
- salt: bytes (required)
- nonce: bytes (required)
- wrapped_key: bytes (required)

### mls_group_info
- conversation_id: text (pk, fk)
- epoch: integer (required)
- group_info: bytes (required)
- updated_by_device_id: text (required, fk)

### device_enrollment_request
- id: text (pk)
- user_id: text (required, fk)
- new_device_id: text (required, fk)
- new_device_ephemeral_pub: bytes (required)
- verification_code: text (required)
- wrapped_account_key: bytes
- status: text (required)
- expires_at: text (required)
- approved_by_device_id: text (fk)

### security_event
- id: text (pk)
- user_id: text (required, fk)
- kind: text (required)
- device_id: text (fk)
- metadata: text

### kv
- value: text (required)

### identity_key
- id: integer (pk)
- public_key: bytes (required)

### message
- id: text (pk)
- conversation_id: text (required, fk)
- sender_id: text (required, fk)
- ciphertext: bytes (required)
- content: text
- reply_to_id: text (fk)
- sent_at: text (required)
- received_at: text (required)
- delivered: integer (required)
- edited_at: text

### dm_conversation
- id: text (pk)
- peer_user_id: text (required, fk)

### preferences
- preferences: text (required)

### ui_state
- value: text (required)

### mls_kv
- scope: text (required)
- value: bytes (required)

### users
- id: text (pk)
- email: text (required)
- username: text
- phone: text
- identity_key: text
- avatar_url: text

### groups
- id: text (pk)
- name: text (required)
- description: text
- icon_url: text
- owner_id: text (required, fk)

### group_member
- group_id: text (required, fk)
- user_id: text (required, fk)
- role: text (required)
- joined_at: text (required)

### channels
- id: text (pk)
- group_id: text (required, fk)
- name: text (required)
- description: text
- channel_type: text (required)

### message_envelope
- id: text (pk)
- conversation_id: text (required, fk)
- sender_id: text (required, fk)
- ciphertext: text (required)
- reply_to_id: text (fk)
- sent_at: text (required)
- delivered: integer (required)

### dm_channel
- id: text (pk)
- created_by: text (required)

### dm_channel_member
- dm_channel_id: text (required, fk)
- user_id: text (required, fk)
- added_by: text (required)
- added_at: text (required)

### group_invite
- id: text (pk)
- group_id: text (required, fk)
- inviter_id: text (required, fk)
- invitee_id: text (required, fk)

### group_join_request
- id: text (pk)
- group_id: text (required, fk)
- requester_id: text (required, fk)
- reviewed_by: text (fk)
- reviewed_at: text
- status: text (required)

### user_preferences
- user_id: text (pk, fk)
- preferences: text (required)

### message_reaction
- id: text (pk)
- message_id: text (required, fk)
- user_id: text (required, fk)
- emoji: text (required)
