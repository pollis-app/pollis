# Pollis Architecture

A privacy-first desktop messaging app combining MLS-based end-to-end encryption with Slack-style group features.

For deeper, file-anchored documentation see `.codesight/wiki/index.md`. For the auditor-facing protocol/threat model see `docs/security-whitepaper.md`.

---

## Stack

| Layer | Tech |
|---|---|
| Desktop shell | Tauri 2 (Rust backend + WebKit-based webview frontend) |
| Frontend | React + TypeScript, Vite, TailwindCSS, TanStack Router (memory history), TanStack Query, Zustand (UI state only) |
| Backend | Rust, Tauri commands invoked via `invoke()` from the frontend |
| End-to-end encryption | MLS (RFC 9420) via OpenMLS 0.8 |
| Remote DB | Turso (libSQL) via `libsql` 0.6, native Hrana/HTTP2 protocol over TLS |
| Local DB | SQLite via `rusqlite` 0.31 with bundled SQLCipher; per-user file `pollis_{user_id}.db` |
| Auth | Email OTP (Resend) + 4-digit per-user local PIN unlocking PIN-wrapped key blobs in the OS keystore |
| Object storage | Cloudflare R2 (S3-compatible) via AWS SigV4, with convergent encryption for attachments |
| Real-time signalling + voice | LiveKit (Rust crate), JWT-signed room tokens |
| Audio capture | `cpal` mic → optional RNNoise → WebRTC APM (AGC2 + NS + HPF + AEC) → LiveKit publish (all in Rust — no JS media path) |
| Secrets | Doppler → GitHub Actions; local dev uses `.env.development` |

The marketing site under `website/` is static HTML on Cloudflare Pages and is not part of the desktop app.

---

## Core Principles

1. **End-to-end encrypted messaging.** All message content is MLS-encrypted on the device before it leaves; the server never sees plaintext.
2. **Zero-knowledge server.** Turso stores ciphertext envelopes, public MLS material, and metadata. It cannot read messages or recover any private key.
3. **Direct to Turso.** The Tauri backend opens a libSQL connection directly to Turso — there is no intermediate API server. All business logic runs in the Tauri Rust process.
4. **Local-first secrets.** Private keys, MLS group state, and decrypted plaintext only exist on the user's device. Disk copies are protected by SQLCipher (local DB) and Argon2id-derived AEAD wrapping (keystore).
5. **Bounded but reliable history.** Members joining at MLS epoch N cannot decrypt messages from epoch < N (an MLS property), and new devices for an existing user start empty (no Megolm-style key backup). Within those limits, every message that was sent while a member was a member must be deliverable and decryptable on every device that user owns. See `CLAUDE.md` § "Messages must work" for the product principle.

---

## Network architecture

```
┌────────────────────────────┐
│ Tauri desktop app          │
│  ├─ React webview (UI)     │
│  └─ Rust backend           │ ◀──── invoke() commands ─────┐
└──────┬─────────────────────┘                              │
       │                                                    │
       │ direct libSQL/Hrana over TLS                       │ no HTTP server in the middle —
       ▼                                                    │ business logic runs inside
┌────────────────┐   ┌──────────────┐   ┌────────────────┐  │ the Tauri process
│ Turso          │   │ Cloudflare R2│   │ LiveKit (SFU)  │  │
│ (metadata,     │   │ (encrypted   │   │ (signalling +  │ ─┘
│ ciphertext,    │   │ attachments, │   │ voice plaintext│
│ MLS commit log,│   │ avatars)     │   │ at SFU)        │
│ welcomes,      │   └──────────────┘   └────────────────┘
│ GroupInfo)     │
└────────────────┘
```

There is **no HTTP/gRPC backend between the desktop app and Turso.** All CRUD, MLS group operations, R2 uploads, LiveKit token minting, and Resend OTP requests run inside the Tauri Rust process and are exposed to React via Tauri commands.

WebRTC is not available in Tauri's WebKitGTK webview on Linux, so voice (and any future video) is implemented in Rust using the `livekit` crate. JS-based media APIs are not an option on our target platforms.

---

## Data storage model

| Store | Stores | Never stores |
|---|---|---|
| **Turso** (remote) | `users`, `groups`, `channels`, `group_member`, `dm_channel*`, `group_invite`, `group_join_request`, `user_block`, `message_envelope` (MLS ciphertext), `mls_key_package`, `mls_commit_log`, `mls_welcome`, `mls_group_info`, `user_device` (incl. cross-signing `device_cert`), `account_recovery` (wrapped account-identity key), `device_enrollment_request`, `security_event`, `attachment_object` (content-hash → R2 key) | Message plaintext, private keys |
| **Local SQLite (SQLCipher)** | Decrypted message plaintext (`message.content`), MLS group state (`mls_kv`), preferences cache, UI state | User profiles, groups, channels (fetched from Turso) |
| **OS Keystore** (Keychain / Secret Service / Credential Manager) | `device_id_{uid}`, `db_key_wrapped_{uid}` (SQLCipher key, AEAD-wrapped under PIN-derived KEK), `account_id_key_wrapped_{uid}` (Ed25519 account-identity private, same wrapping), `pin_meta_{uid}` (Argon2 params + verifier blob + attempt counter) | The unwrapped DB key or account-identity key (after PIN setup) |

The local DB file is encrypted under a 32-byte random key sourced from the OS keystore, which itself only exists on disk as ciphertext under a key derived from the user's PIN via Argon2id.

For the full schema with column-by-column annotations see `.codesight/wiki/database.md`.

---

## Identity model

Pollis carries three nested identities. They are kept distinct on purpose.

1. **Account identity** — one long-lived Ed25519 keypair per user. Public half is in `users.account_id_pub`. Private half is on each enrolled device as ciphertext under the PIN-derived KEK, and on the server as a single `account_recovery` blob wrapped under a key derived from a user-held *Secret Key* (a 30-character Crockford base32 string with 150 bits of entropy, shown to the user once).
2. **Device identity** — a stable ULID `device_id` per device, plus a stable per-device MLS Ed25519 signing keypair. The device's MLS signing public is **cross-signed** by the account-identity private key, producing a `device_cert` stored in `user_device`. Other clients verify these certs before accepting a device into an MLS group.
3. **MLS leaf identity** — every device's `BasicCredential` carries `{user_id}:{device_id}` UTF-8 in its serialized content. One credential covers every KeyPackage and every leaf node that device produces.

### Authentication & unlock

Email OTP via Resend proves control of an email address. It is **not** the device unlock factor.

The unlock factor is a 4-digit PIN, local to the device. The PIN is fed through Argon2id (m=64 MiB, t=3, p=1, output 32 bytes) to derive a KEK; the KEK uses XChaCha20-Poly1305 to wrap the SQLCipher key and the account-identity Ed25519 private. 10 wrong attempts wipe the wrapped blobs and force re-enrollment via Secret Key recovery. The PIN never leaves the device.

`accounts.json` records "who has signed in on this device" with crash-safe atomic writes (tempfile + fsync + rename) and loud parse-failure handling.

### Multi-device enrollment

A new device for an existing user can be enrolled two ways:

- **Approval path.** New device generates an ephemeral X25519 keypair and a 6-digit verification code, posts a request row, and fires a LiveKit inbox event. An already-enrolled sibling device confirms the matching code (constant-time compared) and AEAD-wraps the account-identity key under ECDH(approver_priv, requester_pub) → HKDF-SHA256 → AES-256-GCM. The new device unwraps with its in-memory ephemeral private key.
- **Secret Key recovery.** New device unwraps the server-stored `account_recovery` blob using the user-typed Secret Key (HKDF-SHA256 → AES-256-GCM).

Either path ends with the new device populating `AppState.unlock`, the user setting a PIN (which writes the PIN-wrapped slots), the device publishing its own `device_cert` and KeyPackages, and external-joining every existing MLS group via the published `GroupInfo`.

---

## Encryption (MLS)

- **Specification:** RFC 9420.
- **Library:** `openmls` 0.8 + `openmls_rust_crypto` 0.5, with a Pollis-defined `StorageProvider` backed by the local SQLCipher `mls_kv` table.
- **Cipher suite:** `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` (suite 1, MTI for MLS 1.0). HPKE per RFC 9180.
- **Group topology:** one MLS group per Pollis Group (shared by all its channels); one MLS group per DM channel.
- **Membership changes** flow through `reconcile_group_mls_impl` in `src-tauri/src/commands/mls.rs`. It diffs the desired roster vs. the actual MLS tree and emits a single combined commit with `Add` + `Remove` proposals. The commit is *staged* locally, persisted to Turso (`mls_commit_log` + per-recipient `mls_welcome` rows) on a fresh libSQL connection, and only then merged locally — this ordering is the invariant that prevents "local epoch ahead of remote" split-brain.
- **External commits** (RFC 9420 §11.2.1) handle new-device joins without requiring a sibling Welcome: the device fetches the latest `GroupInfo` from `mls_group_info` and externally commits into the group.
- **Cross-signing verification** runs on inbound commits that add devices: receivers fetch the added device's `device_cert` from `user_device` and verify against the user's `account_id_pub`. Verification is currently advisory (warn-and-proceed) — the security whitepaper documents this gap.

For the full key-material taxonomy, KDF/AEAD parameters, and attack-surface analysis see `docs/security-whitepaper.md`.

---

## Frontend data flow

```
React component
  → invoke("command_name", { args })
    → Rust Tauri command (src-tauri/src/commands/*.rs)
      → Turso (remote, metadata + ciphertext) and/or
        SQLCipher local (secrets, MLS state)
    ← Result<T>
  ← TanStack Query cache
```

**TanStack Query is the source of truth** for remote data. Components read through hooks in `frontend/src/hooks/queries/`. **Zustand** holds only UI state — selected group/channel, transient session data, current user reference. There is no parallel client-side store for remote data.

Routing uses TanStack Router with **memory history** (no browser URL bar in a desktop app). `AppShell` is the root route; key routes are documented in `.codesight/wiki/overview.md`.

---

## Tauri commands (selected)

Registered in `src-tauri/src/lib.rs`, implemented under `src-tauri/src/commands/`.

| Module | Commands |
|---|---|
| `auth` | `request_otp`, `verify_otp`, `get_session`, `dev_login` (debug only), `initialize_identity`, `logout` |
| `pin` | `set_pin`, `unlock`, `lock`, `get_unlock_state` |
| `account_identity` | (internal) `generate_account_identity`, `reset_identity`, `verify_device_cert` |
| `device_enrollment` | `start_device_enrollment`, `poll_enrollment_status`, `list_pending_enrollment_requests`, `approve_device_enrollment`, `reject_device_enrollment`, `recover_with_secret_key`, `reset_identity_and_recover`, `finalize_device_enrollment`, `list_security_events` |
| `user` | `get_user_profile`, `update_user_profile`, `search_user_by_username` |
| `groups` | `list_user_groups`, `list_group_channels`, `create_group`, `create_channel`, `invite_to_group`, `approve_join_request`, `remove_member_from_group`, `leave_group` |
| `dm` | `create_dm_channel`, `list_dm_channels`, `list_dm_requests`, `add_user_to_dm_channel`, `accept_dm_request` |
| `messages` | `list_messages`, `send_message`, `poll_pending_messages`, `edit_message`, `delete_message`, `add_reaction`, `remove_reaction` |
| `mls` | `create_mls_group`, `process_welcome`, `poll_mls_welcomes`, `process_pending_commits`, `reconcile_group_mls`, `generate_mls_key_package`, `publish_mls_key_package`, `fetch_mls_key_package` |
| `livekit` | `get_livekit_token`, `list_voice_participants`, `list_voice_room_counts` |
| `voice` | `join_voice_channel`, `leave_voice_channel`, `set_voice_audio_processing`, … (see `.codesight/wiki/audio-processing.md`) |
| `r2` | `upload_file`, `download_file`, `upload_media`, `download_media` |
| `blocks` | `block_user`, `unblock_user`, `list_blocks` |

For the full reference see `.codesight/wiki/commands.md`.

---

## Project structure

```
src-tauri/                # Rust backend
  src/
    commands/             # Tauri command handlers
    db/                   # libSQL (remote) + SQLCipher (local) + migrations
    config.rs             # Env-var config (Turso, R2, LiveKit, Resend)
    accounts.rs           # accounts.json (atomic, crash-safe)
    keystore.rs           # OS keystore abstraction (+ in-memory impl for tests)
    state.rs              # AppState shared across commands
    realtime.rs           # LiveKit room manager + event dispatch
    signal/               # Legacy Signal-protocol vestige (mls_storage backend; the rest is removed)
    lib.rs                # App setup, command registration

frontend/                 # React app
  src/
    components/           # UI components (auth, layout, message, voice, ui/, …)
    pages/                # Route pages
    hooks/queries/        # TanStack Query hooks
    services/             # invoke() wrappers, R2 upload helpers
    stores/               # Zustand (UI state only)
    types/                # TypeScript types — kept aligned with Rust structs
    router.tsx, main.tsx  # TanStack Router setup, app entry

website/                  # Static marketing HTML on Cloudflare Pages (not part of the app)
.codesight/wiki/          # Authoritative deep-dive docs
docs/security-whitepaper.md   # Auditor-facing protocol/threat model
```

---

## Security model summary

| | Trusted | Untrusted |
|---|---|---|
| | User's device, OS keystore, the SQLCipher local DB, the Tauri binary at install, the user-held Secret Key, the user-held PIN | Network, Turso, Cloudflare R2, LiveKit, Resend, server operators |

What the server can see: user metadata, social graph, encrypted message envelopes (size, sender, timestamp), MLS commit/welcome timing, public keys.

What the server cannot see: message plaintext, MLS application secrets, attachment plaintext, account-identity private keys, the PIN, the Secret Key, the SQLCipher key.

Voice traffic is **not** end-to-end encrypted: LiveKit acts as an SFU and sees plaintext audio. This is consistent with Slack Huddles, Discord, Microsoft Teams, and Google Meet, and is the largest deviation between Pollis' messaging-side and media-side cryptographic guarantees. See `docs/security-whitepaper.md` § 10 for details and § 13 for the full list of known gaps.

---

## Where to go next

- `.codesight/wiki/index.md` — full doc tree (database, MLS, commands, UI components, testing harness, audio pipeline, PIN design, notifications, Windows signing).
- `docs/security-whitepaper.md` — auditor-facing protocol/threat model and standards references.
- `CLAUDE.md` — operating principles and constraints (what to build, what not to build).
