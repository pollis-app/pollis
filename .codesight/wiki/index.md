# pollis — Documentation Tree

Start here. Navigate to the article you need.

## Articles

- [Overview](./overview.md) — Architecture, stack, project structure
- [Database](./database.md) — Remote (Turso) and local (SQLite) schemas with all columns
- [MLS](./mls.md) — Message Layer Security: encryption, group membership, multi-device
- [Tauri Commands](./commands.md) — Rust backend command reference
- [UI Components](./ui.md) — React component inventory
- [Libraries](./libraries.md) — Frontend hooks, services, utilities
- [Testing](./testing.md) — Integration harness for multi-client end-to-end tests
- [Windows Signing](./windows-signing.md) — Azure Artifact Signing setup for Windows installer signing

## Quick Reference

| Layer | Tech | Location |
|-------|------|----------|
| Frontend | React, TypeScript, Vite, TailwindCSS | `frontend/src/` |
| Backend | Rust, Tauri 2 | `src-tauri/src/` |
| Remote DB | Turso (libSQL) | `src-tauri/src/db/migrations/000000_baseline.sql` + `000*.sql` |
| Local DB | SQLite (rusqlite) | `src-tauri/src/db/local_schema.sql` |
| Encryption | OpenMLS (RFC 9420) | `src-tauri/src/commands/mls.rs` |
| Media | LiveKit (Rust crate) | `src-tauri/src/commands/voice.rs`, `livekit.rs` |
| Storage | Cloudflare R2 | `src-tauri/src/commands/r2.rs` |
| Auth | Email OTP + OS keystore | `src-tauri/src/commands/auth.rs` |
| Secrets | Doppler → GitHub Actions | `.env.development` for local dev |
