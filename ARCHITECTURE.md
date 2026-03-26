# Pollis Architecture

**Signal's End-to-End Encryption + Slack's Group Features**

## Stack

- **Desktop**: Tauri 2 (Rust backend + React/TypeScript frontend)
- **Auth**: Email OTP via `request_otp` / `verify_otp` Tauri commands; session persisted in OS keystore via `keyring` crate
- **Remote DB**: Turso (libSQL) via `libsql` crate — public metadata only
- **Local DB**: SQLite via `rusqlite` — encrypted messages and crypto state
- **Encryption**: Signal Protocol (Ed25519 identity keys, X3DH, Double Ratchet)
- **File storage**: Cloudflare R2 via `reqwest`
- **Config**: `dotenvy` loads `.env.development` in dev; production secrets managed via Doppler (synced to GH Actions secrets)

---

## Core Principles

1. **End-to-End Encryption**: All message content encrypted using Signal Protocol before leaving the device
2. **Zero-Knowledge Server**: Turso never stores message plaintext
3. **Direct to Turso**: Desktop app connects directly to Turso (1 hop) for all CRUD — no middleman server
4. **Local-Only for Secrets**: Messages and private keys never leave the device unencrypted

---

## Data Storage

### Remote Database (Turso)

Stores public metadata:
- Users (id, username, email, phone, avatar_url)
- Groups, channels, membership
- Public keys for X3DH key exchange (identity keys, signed prekeys, one-time prekeys)
- Message envelopes (encrypted, for offline delivery)

Never stores: message plaintext, private keys.

### Local Database (SQLite via rusqlite)

Stores secrets:
- Encrypted messages (ciphertext, nonce, metadata)
- Signal protocol session state
- Message queue (pending outgoing messages)

Never stores: user profiles, groups, channels — those are fetched from remote.

### OS Keystore (keyring crate)

- Ed25519 identity key pair
- Session token after OTP verification

---

## Tauri Commands

Registered in `src-tauri/src/lib.rs`, implemented in `src-tauri/src/commands/`:

| Module | Commands |
|---|---|
| auth | `initialize_identity`, `get_identity`, `request_otp`, `verify_otp`, `get_session`, `logout` |
| user | `get_user_profile`, `update_user_profile`, `search_user_by_username` |
| groups | `list_user_groups`, `list_group_channels`, `create_group`, `create_channel`, `invite_to_group` |
| messages | `list_messages`, `send_message`, `poll_pending_messages` |
| signal | `get_prekey_bundle`, `rotate_signed_prekey`, `replenish_one_time_prekeys` |
| livekit | `get_livekit_token` |
| r2 | `upload_file`, `download_file` |

---

## Frontend Data Flow

Frontend calls Tauri commands via `invoke()` from `@tauri-apps/api/core`. React Query wraps these calls for caching and state management.

```typescript
// All backend calls use invoke()
import { invoke } from "@tauri-apps/api/core";

// Wrapped in React Query hooks (frontend/src/hooks/queries/)
useUserProfile()         // invoke("get_user_profile")
useUserGroups()          // invoke("list_user_groups")
useGroupChannels(id)     // invoke("list_group_channels", { groupId })
useChannelMessages(id)   // invoke("list_messages", { channelId })
```

**Zustand** holds only UI state: selected group/channel, current user reference, temporary session data.

---

## Authentication Flow

```
User enters email
    ↓
invoke("request_otp") → Turso: send OTP email
    ↓
User enters OTP code
    ↓
invoke("verify_otp") → Rust: verify OTP, create session
    ↓
Session token stored in OS keystore (keyring)
    ↓
invoke("initialize_identity") → Ed25519 key pair in keystore
    ↓
App ready
```

---

## Message Encryption Flow

**Sending**:
1. User types message
2. Rust command encrypts with Signal protocol using session key
3. Store encrypted ciphertext in local SQLite
4. Write encrypted envelope to Turso for recipient delivery

**Key Exchange (X3DH)**:
1. Alice calls `get_prekey_bundle(bob_id)` → fetches Bob's public keys from Turso
2. Derives shared secret using X3DH
3. Creates session, encrypts first message
4. Both parties establish Double Ratchet session

---

## Project Structure

```
src-tauri/              # Rust backend
  src/
    commands/           # Tauri command handlers (auth, user, groups, messages, signal, r2, livekit)
    config.rs           # Config loaded from env vars
    db.rs               # Turso (libsql) + local SQLite setup
    keystore.rs         # OS keystore via keyring crate
    signal/             # Signal protocol implementation
    state.rs            # AppState (shared across commands)
    lib.rs              # Tauri setup, command registration
frontend/               # React app (Vite, TypeScript, TailwindCSS)
  src/
    hooks/queries/      # React Query hooks
    types/              # TypeScript types
    components/         # React components
    pages/              # Route pages
    services/           # Any non-Tauri service helpers
website/                # Next.js marketing site (Vercel)
```

---

## Security Model

**Trusted**: User's device, local SQLite (encrypted at rest), Tauri app code, OS keystore

**Untrusted**: Network, Turso database, server operators

**Turso can see**: User metadata, group membership, message metadata (sender, timestamp, size), connection patterns, encrypted ciphertext

**Turso cannot see**: Message plaintext, private keys (never leave device)

### Guarantees

- **End-to-End Encryption**: Only sender and recipients can decrypt
- **Forward Secrecy**: Compromised long-term keys don't decrypt old messages
- **Post-Compromise Security**: Compromised session keys eventually heal
- **Zero-Knowledge**: Database operators cannot read message content
