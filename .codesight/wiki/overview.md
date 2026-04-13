# Overview

## Stack

**Tauri 2** desktop app: Rust backend + React/TypeScript frontend in a webview.

- **No backend server.** The Tauri Rust process connects directly to Turso (1 network hop). All business logic runs in the Tauri process via `invoke()` commands.
- **No WebRTC in webview.** Tauri's Linux webview (WebKitGTK) lacks WebRTC. All media (voice, video) runs in Rust via the `livekit` crate. Never suggest using JS-based media APIs.

## Data Flow

```
React component
  → invoke("command_name", { args })
    → Rust Tauri command (src-tauri/src/commands/*.rs)
      → Turso (remote, metadata) or SQLite (local, secrets)
    ← Result<T>
  ← React Query cache
```

**React Query** is the source of truth for remote data. **Zustand** holds only UI state (current user ref, transient session data).

## Frontend Routing

**TanStack Router** with **memory history** (no browser URL bar in a desktop app). Routes defined in `frontend/src/router.tsx`. `AppShell` is the root route component (sidebar + `<Outlet />`).

Key routes:
- `/` — Root (home)
- `/groups/$groupId` — Group landing
- `/groups/$groupId/channels/$channelId` — Text channel
- `/groups/$groupId/voice/$channelId` — Voice channel
- `/groups/$groupId/members`, `/groups/$groupId/invite`, `/groups/$groupId/leave` — Group management
- `/dms/$conversationId` — DM conversation
- `/preferences`, `/settings`, `/security` — User settings
- `/invites`, `/join-requests` — Pending invites and join requests
- `/search` — Global search

Navigation uses `useNavigate()` from TanStack Router. Pages use `useParams()` for route params (`$groupId`, `$channelId`, etc.).

## Project Structure

```
src-tauri/src/
  commands/          # Tauri command handlers (auth, groups, messages, mls, ...)
  db.rs              # Turso + local SQLite connections
  db/migrations/     # remote_schema.sql (frozen) + numbered migrations
  config.rs          # Env var config
  keystore.rs        # OS keystore (keyring crate)
  state.rs           # AppState shared across commands
  lib.rs             # App setup, command registration

frontend/src/
  components/        # React components (Auth/, Layout/, Message/, ui/, Voice/, ...)
  hooks/queries/     # React Query hooks (useGroups, useMessages, usePreferences, ...)
  pages/             # Route pages
  services/          # api.ts (invoke wrappers), r2-upload.ts
  stores/            # Zustand (appStore.ts)
  types/             # TypeScript types

website/             # Static marketing site (Cloudflare Pages, not part of the app)
```

## Storage Model

| Store | Contents | Never stores |
|-------|----------|--------------|
| **Turso** (remote) | Users, groups, channels, membership, public keys, encrypted message envelopes, MLS commit log, MLS welcomes, GroupInfo | Message plaintext, private keys |
| **SQLite** (local, per-user, encrypted) | Decrypted messages, MLS group state (`mls_kv`), preferences cache | User profiles, groups, channels (fetched from remote) |
| **OS Keystore** | Ed25519 identity key pair, session token, device ID, DB encryption key | |

## Security Model

**Trusted:** User's device, local database, Tauri app code, OS keystore.
**Untrusted:** Network, Turso, server operators.

## Realtime

LiveKit rooms carry realtime events (new_message, membership_changed, voice_joined, etc.). The Rust event loop in `livekit.rs` receives data events, dispatches them to the frontend via a typed `tauri::ipc::Channel`, and triggers MLS operations (process commits, poll welcomes) as needed.

Events are a **convenience for speed**, not a correctness requirement. All MLS state is also read from the DB on every message send/receive, so offline devices catch up when they next interact.

---
_Back to [index.md](./index.md)_
