# MLS Implementation Progress

Unified MLS (RFC 9420) for groups AND DMs. Every conversation (group channel or DM)
maps to one MLS group — single protocol, same code path.
`conversation_id` (ULID) is also the MLS `group_id`.

Crates: `openmls = "0.8"`, `openmls_rust_crypto = "0.5"`, `openmls_basic_credential = "0.5"`
Ciphersuite: `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`
Credential: `BasicCredential(user_id.as_bytes())`

---

## Phase 1 — Infrastructure  ✅ DONE

- [x] Add `openmls`, `openmls_rust_crypto`, `openmls_basic_credential` to `Cargo.toml`
- [x] `src-tauri/src/db/migrations/000003_mls.sql` — remote tables (run against Turso)
- [x] `local_schema.sql` — add `mls_kv` table; bump `LOCAL_SCHEMA_VERSION` to `"5"`
- [x] `src/signal/mls_storage.rs` — `MlsStore<'a>` with KV helpers (trait impl in Phase 2)
- [x] `src/signal/mod.rs` — expose `pub mod mls_storage`

**Scope naming convention for Phase 2 StorageProvider impl:**
```
"kp"            key = hash_ref bytes          → key packages
"psk"           key = psk_id bytes            → pre-shared keys
"sig_kp"        key = public_key bytes        → signature key pairs
"enc_kp"        key = public_key bytes        → HPKE encryption key pairs
"epoch_kp"      key = (group_id, epoch, leaf) → per-epoch encryption key pairs
"group_ctx"     key = group_id bytes          → GroupContext
"group_ts"      key = group_id bytes          → TreeSync
"group_state"   key = group_id bytes          → GroupState enum
"group_cfg"     key = group_id bytes          → MlsGroupJoinConfig
"group_itx"     key = group_id bytes          → InterimTranscriptHash
"group_ctag"    key = group_id bytes          → ConfirmationTag
"group_prop"    key = (group_id, proposal_ref)→ queued Proposals
"group_ln"      key = group_id bytes          → own LeafNode(s)
"group_aad"     key = group_id bytes          → application AAD
"group_init"    key = group_id bytes          → InitSecret
"group_epoch"   key = group_id bytes          → EpochSecrets
"group_msg"     key = group_id bytes          → MessageSecrets
"group_export"  key = group_id bytes          → ExportSecret
```

---

## Phase 2 — StorageProvider + Key Package Lifecycle  ✅ DONE

- [x] Add `openmls_traits = "0.5"` to `Cargo.toml` (direct dep needed alongside `openmls = "0.8"`)
- [x] `impl openmls_traits::storage::StorageProvider<1> for MlsStore<'_>` in `mls_storage.rs`
      — full 34-method impl mirroring openmls_memory_storage; key layout = label+key+VERSION
      — list ops (own_leaf_nodes, proposals) store Vec<Vec<u8>> JSON arrays under same table
      — `cargo check` clean (only "unused" warnings, expected)

- [x] `src/commands/mls.rs` — new command file with:
  - `generate_mls_key_package` Tauri command (local gen + persist via MlsStore)
  - `publish_mls_key_package` Tauri command (INSERT OR IGNORE remote)
  - `fetch_mls_key_package` Tauri command (UPDATE...RETURNING atomic claim)
  - `ensure_mls_key_package` pub helper (used by `initialize_identity`)
  - `validate_key_package` pub helper (Phase 3 will use this on welcome)
  - `PollisProvider<'a>` — `OpenMlsProvider` combining `RustCrypto` + `MlsStore`
- [x] Register three commands in `src/lib.rs`
- [x] `initialize_identity` calls `ensure_mls_key_package` (non-fatal on error)
- [x] `cargo check` clean

---

## Phase 3 — Group / DM Creation  ✅ DONE

- [x] `create_mls_group(conversation_id, creator_user_id)` Tauri command
  - `init_mls_group` inner helper: `MlsGroup::new_with_group_id` with `use_ratchet_tree_extension(true)`
  - Group state auto-persisted via `PollisProvider` + `MlsStore`
- [x] `process_welcome(welcome_bytes)` Tauri command
  - `apply_welcome` inner helper: `Welcome::tls_deserialize` → `StagedWelcome::new_from_welcome` → `into_group`
  - `MlsGroupJoinConfig::default()`, ratchet_tree = `None` (embedded in Welcome via extension)
- [x] `poll_mls_welcomes(user_id)` Tauri command — drains `mls_welcome WHERE delivered=0`, applies each, marks `delivered=1`
- [x] `create_channel` gains `creator_id` param; calls `init_mls_group` (non-fatal) after INSERT
- [x] `create_dm_channel` calls `init_mls_group` (non-fatal) after INSERT
- [x] Three new commands registered in `lib.rs`
- [x] Frontend: `creatorId` added to `create_channel` invoke in `CreateChannel.tsx`, `useGroups.ts`, `services/api.ts`

---

## Phase 4 — Member Changes  ✅ DONE

- [x] `load_group_with_signer(provider, conversation_id)` private helper
      — `MlsGroup::load` + `SignatureKeyPair::read` from leaf node key + resolves any pending commit
- [x] `add_member_mls(conversation_id, target_user_id, actor_user_id)` Tauri command
  - Atomically claims target's KeyPackage from `mls_key_package`
  - Validates KeyPackage identity matches `target_user_id`
  - Calls `MlsGroup::add_members` → `(commit_msg, welcome_msg, _)`
  - Serialises commit + welcome as `MlsMessageOut` TLS bytes
  - Calls `merge_pending_commit` → epoch advances locally
  - Posts commit to `mls_commit_log`; posts Welcome to `mls_welcome` for target
- [x] `remove_member_mls(conversation_id, target_user_id, actor_user_id)` Tauri command
  - Looks up target's `LeafNodeIndex` via `member_leaf_index(&BasicCredential)`
  - Calls `MlsGroup::remove_members` → commit, merges locally, posts to `mls_commit_log`
  - Forward secrecy: remaining members advance epoch on apply (next poll)
- [x] `process_pending_commits(conversation_id)` Tauri command
  - Reads current epoch from local MlsGroup
  - Fetches `mls_commit_log WHERE epoch >= current_epoch ORDER BY epoch, seq`
  - For each: validates `row_epoch == current_epoch` (gap detection)
  - Deserialises `MlsMessageIn`, calls `try_into_protocol_message()`,
    `process_message` → `StagedCommitMessage(c)` → `merge_staged_commit`
  - Welcome bytes stored as `MlsMessageOut` TLS; `apply_welcome` uses `extract()` → `MlsMessageBodyIn::Welcome`
- [x] `apply_welcome` updated to deserialise `MlsMessageIn` and extract inner `Welcome` via `extract()`
- [x] Six commands registered in `lib.rs`

---

## Phase 5 — Message Encryption  ✅ DONE

- [x] `try_mls_encrypt(conn, conversation_id, plaintext) -> Option<Vec<u8>>` pub helper in `mls.rs`
  - Loads `MlsGroup` via `load_group_with_signer`, calls `create_message`, TLS-serializes `MlsMessageOut`
- [x] `try_mls_decrypt(conn, conversation_id, ciphertext) -> Option<Vec<u8>>` pub helper in `mls.rs`
  - `MlsGroup::load` → `tls_deserialize` → `try_into_protocol_message` → `process_message`
  - Extracts `ApplicationMessage::into_bytes()` from `ProcessedMessageContent`
- [x] `send_message` updated:
  - Tries MLS encrypt first; on success stores hex-prefixed `"mls:<hex>"` in `message_envelope.ciphertext`
  - Falls back to Signal sender-key if no MLS group exists (non-MLS conversations)
  - Skips `sender_key_dist` distribution when MLS is active
- [x] `get_channel_messages` updated:
  - Detects `"mls:"` prefix → hex-decodes → `try_mls_decrypt`; otherwise Signal `try_decrypt_message`
  - Caches decrypted plaintext in local `message` table as before
- [x] `get_dm_messages` updated: same MLS-first / Signal-fallback decrypt logic
- [x] `cargo check` clean

---

## Phase 6 — Cleanup  ✅ DONE

- [x] Delete `src/signal/session.rs`, `group.rs`, `x3dh.rs`, `ratchet.rs`, `crypto.rs`, `identity.rs`
- [x] `src/signal/mod.rs` — now only `pub mod mls_storage;`
- [x] Delete `src/commands/signal.rs`
- [x] `src/commands/messages.rs` — removed all Signal imports, helpers, and distribution logic; MLS-only
- [x] `src/commands/dm.rs` — removed Signal sender-key distribution; MLS-only
- [x] `src/commands/auth.rs` — `initialize_identity` calls only `ensure_mls_key_package`;
      removed `upload_initial_keys`, X25519/Ed25519 key generation, `get_identity` returns None
- [x] `src/commands/mod.rs` — removed `pub mod signal;`
- [x] `src/lib.rs` — removed three Signal commands from `invoke_handler`
- [x] `src/db/migrations/000004_drop_signal.sql` — DROP TABLE for `sender_key_dist`, `x3dh_init`,
      `signed_prekey`, `one_time_prekey` (run against Turso manually)
- [x] `cargo check` clean

---

## Remote DB Changes (run against Turso manually)

Migration `000003_mls.sql`:
- `mls_key_package` — published KeyPackages, claimed one-at-a-time (replaces `one_time_prekey`)
- `mls_commit_log` — AUTOINCREMENT seq linearises concurrent commits (replaces sender key dist)
- `mls_welcome` — Welcome messages to new members (replaces `x3dh_init`)

## Local DB Changes

`local_schema.sql` v5:
- Add `mls_kv(scope TEXT, key BLOB, value BLOB, PRIMARY KEY(scope, key))` for StorageProvider
