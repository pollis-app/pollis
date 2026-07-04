# pollis — Documentation Tree

Start here. Navigate to the article you need.

## Articles

- [Overview](./overview.md) — Architecture, stack, project structure
- [Database](./database.md) — Remote (Turso) and local (SQLite) schemas with all columns
- [MLS](./mls.md) — Message Layer Security: encryption, group membership, multi-device
- [Backend Commands](./commands.md) — Rust backend command reference
- [UI Components](./ui.md) — React component inventory
- [Libraries](./libraries.md) — Frontend hooks, services, utilities
- [Testing](./testing.md) — Integration harness for multi-client end-to-end tests
- [Safety & Verification](./safety.md) — Signal-style safety numbers, TOFU pinning, group MITM defence (refs #277)
- [Windows Signing](./windows-signing.md) — Azure Artifact Signing setup for Windows installer signing
- [PIN Design](./pin-design.md) — Local PIN unlock: KDF/AEAD choices, blob format, lifecycle, threat model
- [Notifications](./notifications.md) — Sound, OS notification, badge, and alert dispatcher (`notify()` + category table)
- [Audio Processing](./audio-processing.md) — Mic-side AGC + NS + AEC pipeline (WebRTC APM), playback mixer, AEC render reference, build deps
- [Screen-Capture Helper Split](./capture-split.md) — Per-platform capture subprocess + shared socket protocol; Linux two-backend session-type routing (#281), macOS SCK isolation (#283)
- [Authorized-Secrets Broker](./secrets-broker.md) — Server-side LiveKit token minting + R2 SigV4 presign so API secrets never ship in the client bundle (#393)
- [Media Permissions](./media-permissions.md) — OS camera/mic/screen access: live status, revoke-on-quit, manual revoke; honest per-OS behavior (#443)

## Quick Reference

| Layer | Tech | Location |
|-------|------|----------|
| Frontend | React, TypeScript, Vite, TailwindCSS | `frontend/src/` |
| Desktop shell | Tauri 2 (Rust host + system WebView renderer) | `src-tauri/src/` |
| Backend (logic) | Rust workspace crate `pollis-core` | `pollis-core/src/` |
| Backend (host binding) | Tauri host — `#[tauri::command]` shims, plugins, `invoke_handler!`, lifecycle | `src-tauri/src/` |
| Remote DB | Turso (libSQL) | `pollis-core/src/db/migrations/000000_baseline.sql` + `000*.sql` |
| Local DB | SQLite (rusqlite) | `pollis-core/src/db/local_schema.sql` |
| Encryption | OpenMLS (RFC 9420) | `pollis-core/src/commands/mls.rs` |
| Media | LiveKit (Rust crate) | `pollis-core/src/commands/voice.rs`, `livekit.rs` |
| Storage | Cloudflare R2 | `pollis-core/src/commands/r2.rs` |
| Auth | Email OTP + OS keystore | `pollis-core/src/commands/auth.rs` |
| Secrets | Doppler → GitHub Actions | `.env.development` for local dev |
