# CLAUDE.md

Pollis — privacy-first E2EE desktop messenger (MLS, Slack-style channels). Tauri 2 shell + React renderer + reusable Rust core (`pollis-core`); Turso (libSQL) remote DB; `pollis-delivery` (axum) Delivery Service. Servers never see message plaintext.

**Docs:** `.codesight/wiki/` — schemas, MLS flows, testing, full command reference (start at `index.md`; **update the relevant article alongside code changes, without discussion**). `docs/deployments.md` — what ships where + CI pipelines. `ARCHITECTURE.md` — details.

## Commands

```bash
pnpm dev                # Tauri app  (dev:frontend = browser-only; cli = pollis-tui terminal client)
pnpm build:tauri        # bundle current platform  (:linux | :macos | :windows)
cargo test --features test-harness --test flows   # integration suite — real dispatch path, shared Turso from .env.test
                                                  # headless: -p pollis --no-default-features --features test-harness
cargo test -p pollis-delivery                     # DS endpoint tests
```

Secrets: Doppler → GH Actions; locally `.env.development` (scripts source it automatically; see `docs/run-it-yourself.md`).

## Architecture

- **Command path:** renderer → `invoke()` via `frontend/src/bridge` → thin `#[tauri::command]` shims (`src-tauri/src/commands/`) → real implementations in `pollis-core/src/commands/`. **Edit pollis-core, not the shims.** New command = impl + shim + register in `src-tauri/src/lib.rs` `invoke_handler!` + `src-tauri/src/test_harness.rs` if integration-tested. Command inventory: `.codesight/wiki/commands.md`.
- **Remote reads are direct to Turso** (`state.remote_db`, SELECT only). **Remote writes go through the DS only** — typed `POST /v1/...` on `pollis-delivery` (api.pollis.com / api-dev), called via `pollis-core/src/commands/mls/ds_client.rs`: `ds_post*` = device-signed Ed25519 `X-Pollis-*` headers; `ds_post_session*` = OTP-session bearer for bootstrap/pre-enrollment writes; `ds_post_plain` = the OTP endpoints themselves. Never add a client-side remote INSERT/UPDATE/DELETE — extend a DS endpoint.
- **Storage:** Turso = public metadata + encrypted envelopes (never plaintext or private keys). Local SQLite (`state.local_db`) = message ciphertext + MLS state (never users/groups/channels tables — those are remote-only). OS keystore = identity key pair + session token.
- **Frontend data:** React Query hooks (`frontend/src/hooks/queries/`) are the source of truth for remote data. MobX singletons (`frontend/src/stores/`, `makeAutoObservable`) hold UI state only — selection, current user; read fields inside `observer()` components; non-React code uses `autorun`/`reaction`.
- **Workspace:** `pollis-core` (backend; mobile via uniffi), `src-tauri` (shipping shell — Electron is history), `pollis-delivery` (DS), `pollis-tui` (terminal client), `pollis-capture-{linux,macos,proto}` (camera/screen helper subprocesses), `verifiable-log*` (transparency log), `frontend/`, `website/` (static HTML, Cloudflare Pages).
- **Perf & media are Rust-first.** All real-time media runs end-to-end in Rust (`commands/voice/`, capture helpers) — one code path for desktop + mobile, no V8 GC stutter on large buffers. The renderer's WebRTC is intentionally unused; IPC channels carry UI events only, never media. Cached/encrypted media bytes are served over the loopback HTTP server (`media_server.rs`). New perf-sensitive features: Rust `invoke()` command (CRUD-shaped) or loopback endpoint (byte-stream-shaped); JS only if the Rust path can't work.
- **Security model:** trusted = device, local DB, signed app binary, OS keystore. Untrusted = network, Turso, DS, server operators (they see metadata, never content or keys).

## Product principles

- **Invalid states unrepresentable** — governs all pollis-core / remote-schema / MLS / delivery / retention work. Enforce at the lowest layer: DB constraint > Rust type > protocol chokepoint > code discipline (last resort, and only with a test encoding the invariant). Such changes ship with a test proving the invalid state can't be created; "happy path works" is not coverage. Full failure taxonomy: `docs/backend-core-invariants.md`.
- **Messages must work; history is bounded, not flaky.** Exactly two acceptable losses: (1) messages sent before you joined the MLS tree, (2) a new device starts empty (no key backup — never add Megolm-style backup unless explicitly asked). Everything else — delivery to every current member, decryption, offline/online cycles, reconnects, DM-request accepts — must work. Given "simpler but drops messages" vs "more complex but delivers," pick the one that delivers.

## Rules

- **NEVER commit on local `main`** — always create a `fix/*` or `feature/*` branch first. Absolute.
- Always `pnpm`, never `npm`. Prefer editing existing files over creating new ones.
- No Claude attribution in commits (no `Co-Authored-By`). Single-line commit messages and terse PR descriptions unless the scope is large.
- Renderer imports `invoke`/`Channel`/window/dialog/etc. only from `frontend/src/bridge` — never `@tauri-apps/*` directly, never `fetch()` to a local server.
- Keep TypeScript types in sync with Rust structs.
- **Remote schema changes** = numbered migrations in `pollis-core/src/db/migrations/`. Never edit `000000_baseline.sql`; never `INSERT INTO schema_migrations` (the runner records it). Dev: apply by hand to your dev Turso. Prod: the desktop-release workflow applies them; failure aborts the release. Migrations must be additive and backward-compatible with the shipped app (safe: CREATE TABLE, nullable/DEFAULT ADD COLUMN, CREATE INDEX, already-satisfied CHECKs; DROP/RENAME/tightening require a multi-release dance).
- **NO MODALS** — absolute; sole exception is the Cmd+K search menu. Confirmation/input flows replace the chat input bar (edit/delete bar pattern in `MainContent`) or navigate to a page. No fixed-position overlays, backdrops, or dialog elements.
- Use existing `frontend/src/components/ui/` components (`Button`, `Switch`, `TextInput`, …) — never hand-rolled styled equivalents.
- New static pages register in three places: `frontend/src/router.tsx`, `PAGE_RESULTS` in `SearchPanel.tsx`, and the Sidebar nav. (Parameterized routes are exempt.)
- No neon/glow effects — solid borders and backgrounds only. No periodic polling (`setInterval` keepalives) — event-driven or `with_retry`.
- For design decisions, reference how Slack/Discord/Linear solve the same problem — don't reinvent solved problems.
- **After any merge that lands a real feature or fix, run the post-merge release checklist** (`docs/deployments.md` → "Post-merge release checklist"): walk the change's blast radius (shared crates like `pollis-core`/`verifiable-log*` fan out to desktop, CLI, DS, verifier, transparency log), decide redeploy/defer/N-A for each downstream output, and for anything deployed **confirm the new build is live** (e.g. DS `/version` SHA), not merely that a workflow fired. Don't rebuild everything every time — but never leave a downstream target silently stale.

## Style

- `if` statements always use braces, even for single-line bodies.
- Comments go above the line, never inline. Reusable components live in their own files.
- **Tailwind-first, token-backed:** design tokens are CSS variables in `frontend/src/index.css`, surfaced as semantic utilities in `tailwind.config.js` (`bg-bg`, `bg-surface[-raised|-high]`, `text-fg/dim/muted/accent`, `border-line[-strong]`, `hover:bg-hover`, `h-bar`). Use the utilities — no `[var(--c-…)]` arbitrary classes or inline token styles; if a utility is missing, add it to the theme. Inline `style` only for runtime-computed values; co-located CSS files (or `@layer components`) for pseudo-elements, keyframes, and complex selectors.
- Sizes that should track the user's font setting are in `rem` — never a `px` arbitrary class for a scalable dimension; `px` only for intentional non-scaling (1px hairlines). No per-file px→rem helpers. Bundle size is not a concern (Tailwind JIT) — choose for consistency.
