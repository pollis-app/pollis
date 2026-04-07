# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Pollis is a privacy-first desktop messaging app with end-to-end encryption using Signal Protocol. Built with Tauri 2 (Rust + React), it combines Signal's security with Slack's group messaging features. The server never sees message plaintext.

**Stack**: Tauri 2, React/TypeScript, Rust, Turso (libSQL), Signal Protocol

**Key Architecture**: Tauri Rust backend connects **directly** to Turso (1 hop) for all CRUD. No separate backend server. All operations go through Tauri commands invoked from the React frontend.

## Development Commands

### Setup
```bash
pnpm install              # Install JS dependencies
```

`.env.development` is loaded automatically in dev builds via `dotenvy::from_filename(".env.development")` in `src-tauri/src/lib.rs`. No manual sourcing needed.

### Running
```bash
pnpm dev                  # Run Tauri desktop app (Rust + React)
pnpm dev:frontend         # Run frontend in browser only (no Tauri commands)
```

### Building
```bash
pnpm build                # Build for current platform
pnpm build:linux          # Linux amd64
pnpm build:macos          # Universal macOS binary
pnpm build:windows        # Windows amd64
```

### Secrets Management

Secrets are managed via **Doppler**, which syncs to GitHub Actions secrets automatically. For local development, create a `.env.development` file manually or use Doppler CLI (`doppler run -- pnpm dev`).

## Architecture

### Network Architecture

**Tauri Rust backend → Turso (DIRECT libsql connection)**
- 1 network hop — simple and fast
- Rust backend has same DB access as any server

**No separate gRPC/HTTP server** — that has been removed. All backend logic runs in the Tauri process.

**Tauri handles directly:**
- User profile CRUD
- Groups and channels CRUD
- Reading/writing to Turso
- R2 uploads/downloads
- Signal protocol operations
- Auth (email OTP + session in OS keystore)

### Data Storage Model

**Remote Database (Turso)** — public metadata:
- Users, groups, channels, membership
- Public keys for Signal Protocol key exchange
- Encrypted message envelopes (for offline delivery)
- **Never stores**: message plaintext, private keys

**Local Database (SQLite via rusqlite)** — secrets:
- Encrypted messages (ciphertext, nonce)
- Signal protocol session state
- **Never stores**: user profiles, groups, channels (fetched from remote)

**OS Keystore (keyring crate)**:
- Ed25519 identity key pair
- Session token

### Frontend Data Fetching

All backend calls use `invoke()` from `@tauri-apps/api/core`, wrapped in React Query hooks:

```typescript
// React Query hooks in frontend/src/hooks/queries/
useUserProfile()                    // invoke("get_user_profile")
useUserGroups()                     // invoke("list_user_groups")
useGroupChannels(groupId)           // invoke("list_group_channels", { groupId })
useChannelMessages(channelId)       // invoke("list_messages", { channelId })
useSendMessage()                    // invoke("send_message", ...)
```

**React Query is the source of truth** for remote data — don't duplicate in Zustand.

**Zustand store**: Only holds UI state (selected group/channel), current user reference, temporary session data.

### Tauri Commands

Implemented in `src-tauri/src/commands/`, registered in `src-tauri/src/lib.rs`:

- **auth**: `initialize_identity`, `get_identity`, `request_otp`, `verify_otp`, `get_session`, `logout`
- **user**: `get_user_profile`, `update_user_profile`, `search_user_by_username`
- **groups**: `list_user_groups`, `list_group_channels`, `create_group`, `create_channel`, `invite_to_group`
- **messages**: `list_messages`, `send_message`, `poll_pending_messages`
- **signal**: `get_prekey_bundle`, `rotate_signed_prekey`, `replenish_one_time_prekeys`
- **livekit**: `get_livekit_token`
- **r2**: `upload_file`, `download_file`

### Project Structure

```
src-tauri/              # Rust backend (Tauri)
  src/
    commands/           # Tauri command handlers
    config.rs           # Config from env vars
    db.rs               # Turso + local SQLite
    keystore.rs         # OS keystore (keyring)
    signal/             # Signal protocol
    state.rs            # AppState
    lib.rs              # App setup, command registration
frontend/               # React app (Vite, TypeScript, TailwindCSS)
  src/
    hooks/queries/      # React Query hooks
    types/              # TypeScript types
    components/         # React components
    pages/              # Route pages
website/                # Next.js marketing site (Vercel)
```

## Security Model

**Trusted**: User's device, local database (encrypted at rest), Tauri app code, OS keystore

**Untrusted**: Network, Turso database, server operators

**Turso can see**: User metadata, group membership, message metadata (sender, timestamp, size), connection patterns

**Turso cannot see**: Message content (encrypted), private keys (never leave device)

## Key Files

- `src-tauri/src/lib.rs` — Tauri app entry point, command registration
- `src-tauri/src/commands/` — All Tauri command implementations
- `src-tauri/src/state.rs` — AppState shared across commands
- `frontend/src/main.tsx` — React app entry point
- `frontend/src/hooks/queries/` — React Query hooks
- `ARCHITECTURE.md` — Detailed architecture documentation

## Important Notes

- **Tauri backend connects DIRECTLY to Turso** — no server middleman for CRUD
- **All backend calls from frontend use `invoke()`** — never fetch() to a local server
- **React Query is the source of truth** for remote data — don't duplicate in Zustand
- **Local DB should NOT have users/groups/channels tables** — those come from remote Turso
- **TypeScript types should match Rust structs** — keep them synchronized
- **`remote_schema.sql` is frozen** — do not modify it. All schema changes go in numbered migration files in `src-tauri/src/db/migrations/` (e.g. `000002_my_change.sql`) and are run by hand against Turso. Every migration file must end with an `INSERT INTO schema_migrations (version, description) VALUES (N, '...');` row matching its number
- **Prefer editing existing files** over creating new ones
- **Always use `pnpm`** not `npm`
- **Never add Claude as a co-author on commits** — do not include `Co-Authored-By:` trailers or any Claude attribution in commit messages
- **Never remove `data-testid` attributes** from JSX/HTML — they are used by Playwright E2E tests (`pnpm test:e2e`)
- **Never reinvent UI components** — always use existing components from `frontend/src/components/ui/`. Toggles/switches use `Switch`, buttons use `Button`, text inputs use `TextInput`, etc. Do not build custom styled `<button>` or `<input>` elements when a ui/ component already exists.

## Coding Style

### If statements always use braces
```typescript
// BAD
if (!currentUser) return;

// GOOD
if (!currentUser) {
  return;
}
```

### Component file organisation

Reusable components live in their own files. Only keep a component co-located with its parent if it is exclusively a child of that parent and will never be used elsewhere (e.g. a `ListItem` used only by `List`).

### Comments go above their relevant line, not inline
```typescript
// BAD
checkStatus(); // Verify with backend

// GOOD
// Verify with backend
checkStatus();
```
