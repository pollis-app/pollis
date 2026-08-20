# Pollis Architecture

A privacy-first desktop messaging app combining MLS-based end-to-end encryption with Slack-style group features.

For deeper, file-anchored documentation see `.codesight/wiki/index.md`. For the auditor-facing protocol/threat model see `docs/security-whitepaper.md`.

---

## Stack

| Layer | Tech |
|---|---|
| Desktop shell | Tauri 2 (Rust host + system WebView renderer) |
| Frontend | React 19 + TypeScript, Vite, TailwindCSS, TanStack Router (memory history), TanStack Query, MobX (UI state only) |
| Backend | Rust split into `pollis-core` (reusable crate; also exposed to mobile via uniffi) and `src-tauri` (the Tauri host), invoked from the renderer via `invoke(cmd, args)` |
| End-to-end encryption | MLS (RFC 9420) via OpenMLS 0.8 for messages and files, ciphersuite `MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44` (post-quantum; the classical suite was retired in #669); AES-128-GCM frame-level encryption via libwebrtc's `FrameCryptor` for voice, keyed from a 32-byte MLS exporter secret |
| Remote DB | Turso (libSQL). Reached **only by `pollis-delivery`** — since #987 the client has no database credential and does not link `libsql` at all; every remote read and write is a signed `POST /v1/…` to the DS |
| Local DB | SQLite via `rusqlite` 0.37 with SQLCipher; per-user file `pollis_{user_id}.db`, encrypted at rest on every platform. Windows links SQLCipher from the `pollis-sqlcipher` crate, whose crypto provider is RustCrypto rather than OpenSSL — OpenSSL cannot be linked beside libwebrtc's BoringSSL under MSVC (whitepaper §7.0, #992) |
| Auth | Email OTP (Resend) + 4-digit per-user local PIN unlocking PIN-wrapped key blobs in the OS keystore |
| Object storage | Cloudflare R2, with convergent encryption for attachments. The client holds **no** R2 credentials (#506) — uploads use a short-lived presigned URL minted by the Delivery Service |
| Real-time signalling + voice | LiveKit (Rust crate). The client holds no LiveKit API key (#393/#506) — room tokens are minted by the Delivery Service |
| Audio capture | `cpal` mic → optional RNNoise → WebRTC APM (AGC2 + NS + HPF + AEC) → LiveKit publish (all in Rust — the renderer never touches media) |
| Packaging + auto-update | Tauri bundler (DMG + app, NSIS, AppImage + deb + rpm) and the Tauri updater plugin reading `update-{{bundle_type}}.json` manifests from `cdn.pollis.com`; OS code signature (Apple Developer ID + notarization, Azure Trusted Signing) is the trust root |
| Secrets | Doppler → GitHub Actions; local dev uses `.env.development` |

The marketing site under `website/` is static HTML on Cloudflare Pages and is not part of the desktop app.

---

## Core Principles

1. **End-to-end encrypted messaging, files, and voice.** All message content is MLS-encrypted on the device before it leaves; the server never sees plaintext. Files are convergent-encrypted with the key delivered inside the MLS-encrypted message. Voice frames are AES-128-GCM-encrypted by libwebrtc's `FrameCryptor`, keyed by the channel's MLS-exporter secret, so the LiveKit SFU forwards ciphertext only.
2. **Zero-knowledge servers.** Turso stores ciphertext envelopes, public MLS material, and metadata; the Delivery Service handles the writes. Neither can read messages or recover any private key — both are untrusted parties in the security model below.
3. **Everything remote goes through the Delivery Service.** The Rust backend runs inside the Tauri host process (`src-tauri` over `pollis-core`) and talks to exactly one backend: `pollis-delivery` (`api.pollis.com`), an axum service in this repo. It is the single writer that serializes MLS commits (#419/#420), and since #987 it is also the single reader — the client holds no database credential and `pollis-core` does not depend on `libsql`, so "the client cannot reach the database" is a fact about the dependency graph rather than a rule about the token's scope. It brokers the remaining credentials too: R2 presigned uploads and LiveKit room tokens. Crypto stays client-side; the DS sees ciphertext and metadata, never plaintext or a private key.
4. **Local-first secrets.** Private keys, MLS group state, and decrypted plaintext only exist on the user's device. Disk copies are protected by SQLCipher (local DB, every platform — see the Local DB row above) and Argon2id-derived AEAD wrapping (keystore). Where no OS keychain exists, the keystore file itself is AES-256-GCM ciphertext under a machine-bound key (#882) — never plaintext.
5. **Bounded but reliable history.** Members joining at MLS epoch N cannot decrypt messages from epoch < N (an MLS property), and new devices for an existing user start empty (no Megolm-style key backup). Within those limits, every message that was sent while a member was a member must be deliverable and decryptable on every device that user owns. See `CLAUDE.md` § "Messages must work" for the product principle.

---

## Network architecture

```
┌──────────────────────────────────────────────────────┐
│ Tauri desktop app                                    │
│  ├─ Renderer (system WebView): React UI              │
│  │     invoke(cmd, args)                             │
│  │     ipc::Channel<…> for streamed events           │
│  └─ Tauri host process (Rust)                        │
│        src-tauri shims → pollis-core                 │
│        · all MLS crypto, all plaintext               │
└───┬──────────────────────────────────────────┬───────┘
    │                                          │
    │                                          │ READS AND WRITES:
    │                                          │ typed HTTPS POST /v1/… ,
    │                                          │ ML-DSA-44 device-signed
    │                                          │ headers (#987)
    │                                          ▼
┌────────────────┐   reads +        ┌──────────────────────────┐
│ Turso (libSQL) │ ◀──writes────────│ pollis-delivery (axum)   │
│ metadata,      │   (DS only)      │ api.pollis.com           │
│ ciphertext     │                  │ · single writer: commits │
│ envelopes,     │                  │   serialize per group    │
│ MLS commit log,│                  │ · single reader: the     │
│ welcomes,      │                  │   client has no token    │
│ GroupInfo      │                  │ · mints R2 presigned URL │
└────────────────┘                  │ · mints LiveKit token    │
                                    │ · Resend OTP, push fan-  │
                                    │   out, retention sweeps  │
                                    └────┬──────────┬──────────┘
    ┌──────────────┐   ┌────────────────┐│          │
    │ Cloudflare R2│◀──┤ LiveKit (SFU)  ││          │
    │ encrypted    │   │ forwards AES-  │◀┘          ▼
    │ attachments, │   │ GCM ciphertext │      ┌───────────┐
    │ avatars      │   │ frames only    │      │ Resend,   │
    └──────────────┘   └────────────────┘      │ APNs/FCM  │
          ▲ presigned PUT/GET from the client  └───────────┘
```

**The client has no database credential (#987).** It used to open a libSQL
connection to Turso with a read-only token and issue `SELECT`s; the write side
was already the DS, so "don't write from the client" was enforced by the token's
scope. The read side had no such enforcement available — a read-only token is
still a WHOLE-DATABASE token, which is the residual #917 named and could not
close. #987 closed it by removing the connection: `pollis-core` does not link
`libsql`, so a client-side query is a compile error, not a policy violation.

Every remote read and every remote mutation is a typed `POST /v1/…` call to
`pollis-delivery` (`api.pollis.com`, `api-dev.pollis.com`), made through
`pollis-core/src/commands/mls/ds_client.rs`:

| Helper | Authenticated by | Used for |
|---|---|---|
| `ds_post` / `ds_post*` | ML-DSA-44 device signature in `X-Pollis-*` headers | everything a signed-in device does, reads included |
| `ds_post_session*` | OTP-session bearer token | bootstrap and pre-enrolment writes, before a device key exists |
| `ds_post_plain` | nothing, or a capability | the OTP endpoints, the pre-unlock account probe, and the enrollment poll (gated on possession of a 128-bit `request_id`) |

Reads are POST, not GET, for a specific reason: the canonical signing message
covers the path WITHOUT its query string, so a signed `GET …?since=N` would carry
unauthenticated parameters — the #681 shape, where an unauthenticated
`GET /v1/commits` was a remote commit-log wipe. Putting the query in a JSON body
makes the signature's `sha256(body)` binding cover the values themselves.

The DS exists for three reasons, in order of importance:

1. **MLS needs a serialization point.** Two devices committing at epoch *N* both produce a
   valid epoch *N+1*; whoever writes second must be rejected, not merged. The DS is the
   single writer that decides (#419/#420) — a property no amount of client-side care can
   provide.
2. **The client should not hold shared credentials.** R2 keys and the LiveKit API secret
   used to ship inside the binary, where anyone could extract them (#393/#506). The DS holds
   them and hands back short-lived presigned URLs and room tokens instead.
3. **Server-side policy.** Retention sweeps, push fan-out, and rate limiting need to run
   where the client cannot bypass them.

What did **not** move is the crypto. Group state, key material, and plaintext never leave
the device; the DS validates structure and authorization over ciphertext it cannot read, so
it stays in the *untrusted* column of the security model below.

**IPC shape.** The renderer calls `invoke(cmd, args)` through the shell-agnostic wrapper at `frontend/src/bridge/invoke.ts`, which routes to Tauri's `invoke`. The host (`src-tauri/src/lib.rs`) registers one `#[tauri::command]` shim per command in its `invoke_handler!`; each shim forwards the call into `pollis_core::commands::*`. Streaming events flow the other direction over Tauri's `ipc::Channel`: `pollis-core` pushes envelopes through the `EventSink` abstraction, `src-tauri`'s `ChannelSink` adapter wraps a `tauri::ipc::Channel`, and the renderer subscribes via the bridge's `Channel`/`listen` helpers. This is the path used for voice frames, screenshare frames, realtime events, and terminal output.

Media stays in Rust by design. The renderer's Chromium does have WebRTC available, but voice and screenshare run inside `pollis-core` for two reasons: (a) **cross-platform parity** — the same media pipeline is consumed by mobile through uniffi, so one code path covers desktop and mobile, and (b) **predictable allocation** — multi-MB media buffers passed through the V8 heap produce visible GC stutter, while Rust's manual allocation does not. JS-based media APIs are reserved for the future when a small preview or thumbnail needs it.

---

## Data storage model

| Store | Stores | Never stores |
|---|---|---|
| **Turso** (remote) | Identity and devices (`users`, `user_device` with its cross-signing `device_cert` and PQ signature key, `account_recovery` holding the *wrapped* account-identity key, `account_key_log`); the social graph (groups, channels, DM membership, invites, blocks); delivery state (`message_envelope` — MLS ciphertext, reactions, per-device `conversation_watermark`, push tokens); public MLS material (`mls_key_package`, `mls_commit_log`, `mls_welcome`, `mls_group_info`); and `attachment_object`/`attachment_ref` mapping content hashes to R2 keys | Message plaintext, private keys |
| **Local SQLite (SQLCipher, every platform)** | Decrypted message plaintext (`message.content`), MLS group state (`mls_kv`), preferences cache, UI state | User profiles, groups, channels (fetched from the DS) |
| **Device keystore** — the OS keychain (Keychain / Secret Service / Credential Manager) where one exists, otherwise a machine-bound encrypted file (#882) | `device_id_{uid}`, `db_key_wrapped_{uid}` (SQLCipher key, AEAD-wrapped under PIN-derived KEK), `account_id_key_wrapped_{uid}` (Ed25519 account-identity private, same wrapping), `pin_meta_{uid}` (Argon2 params + verifier blob + attempt counter) | The unwrapped DB key or account-identity key (after PIN setup) |

Turso is two databases, not one. The main DB holds user/device metadata and message
envelopes; a separate **commit-log DB** (`LOG_DB_URL`) holds the MLS control plane —
`mls_commit_log`, `mls_group_info`, `mls_welcome` — with the Delivery Service as its sole
writer. Splitting them means the tables whose ordering the whole protocol depends on have
exactly one writer and their own migration series (`pollis-schema/migrations-log/`).

The local DB file is encrypted under a 32-byte random key sourced from the device keystore, which itself only exists on disk as ciphertext under a key derived from the user's PIN via Argon2id.

The keystore backend is chosen at runtime, once per process: the OS keychain wherever one answers, otherwise a file whose bytes are AES-256-GCM ciphertext under `HKDF-SHA256(platform machine ID, fresh per-write salt)`. Plaintext is not a reachable state in any build. The machine binding is what makes a *copied* keystore useless — a backup, an `scp`, a resold disk — and it is honestly weaker than a keychain: it does not stop a local attacker running as the same UID, nor a full-disk image, since the machine ID sits on the same disk. See `.codesight/wiki/overview.md` for the full statement of what it does and does not defend against.

For the full schema with column-by-column annotations see `.codesight/wiki/database.md`.

---

## Identity model

Pollis carries three nested identities. They are kept distinct on purpose.

1. **Account identity** — one long-lived **ML-DSA-44** keypair per user (FIPS 204; moved from Ed25519 in #668). Public half is in `users.account_id_pub` — 1312 bytes, and its signatures are 2420. The private half is canonically a 32-byte seed, exactly like the Ed25519 key it replaced, which is why every wrap, recovery, and device-transfer path kept working unchanged. That seed lives on each enrolled device as ciphertext under the PIN-derived KEK, and on the server as a single `account_recovery` blob wrapped under a key derived from a user-held *Secret Key* (a 30-character Crockford base32 string carrying 150 bits of entropy, shown to the user once).
2. **Device identity** — a stable ULID `device_id` per device, plus a stable MLS signing keypair **per signature scheme** (Ed25519 and ML-DSA-44), since the scheme is a function of the ciphersuite. One v2 `device_cert` binds *both* leaf keys at once, signed by the account-identity private key and stored in `user_device`. Clients verify that cert before accepting a device into an MLS group, and the Delivery Service re-verifies it at publish time.
3. **MLS leaf identity** — every device's `BasicCredential` carries `{user_id}:{device_id}` UTF-8 in its serialized content. One credential covers every KeyPackage and every leaf node that device produces.

### Authentication & unlock

Email OTP via Resend proves control of an email address. It is **not** the device unlock factor.

The unlock factor is a 4-digit PIN, local to the device. The PIN is fed through Argon2id (m=64 MiB, t=3, p=1, output 32 bytes) to derive a KEK; the KEK uses XChaCha20-Poly1305 to wrap the SQLCipher key and the account-identity Ed25519 private. 10 wrong attempts wipe the wrapped blobs and force re-enrollment via Secret Key recovery. The PIN never leaves the device.

`accounts.json` records "who has signed in on this device" with crash-safe atomic writes (tempfile + fsync + rename) and loud parse-failure handling.

### Multi-device enrollment

A new device for an existing user can be enrolled two ways:

- **Approval path.** The new device generates an ephemeral X25519 keypair and posts a request row, then fires a LiveKit inbox event. The verification code is **derived from the ephemeral public key**, not generated beside it: `HKDF-SHA256(ephemeral_pub, info="pollis-enrollment-sas-v1")` → 8 characters over a 32-symbol Crockford-style alphabet, 40 bits. Deriving it is the point (#793) — the approver recomputes the expected code from the key it *fetched*, so a swapped key produces a different code and the mismatch is visible to the user. A code merely stored alongside the key would still match after substitution. The approver then AEAD-wraps the account-identity seed under ECDH(approver_priv, requester_pub) → HKDF-SHA256, with both public keys bound into the KDF info, → AES-256-GCM. The new device unwraps with its in-memory ephemeral private key.
- **Secret Key recovery.** New device unwraps the server-stored `account_recovery` blob using the user-typed Secret Key (HKDF-SHA256 → AES-256-GCM).

Either path ends with the new device populating `AppState.unlock`, the user setting a PIN (which writes the PIN-wrapped slots), the device publishing its own `device_cert` and KeyPackages, and external-joining every existing MLS group via the published `GroupInfo`.

---

## Encryption (MLS)

- **Specification:** RFC 9420.
- **Library:** `openmls` 0.8 — since #668 pinned by a workspace-wide `[patch.crates-io]` to an exact upstream `main` revision, because the `draft-ietf-mls-pq-ciphersuites` feature carrying the ML-DSA suite is in no release — with a Pollis-defined `StorageProvider` backed by the local SQLCipher `mls_kv` table. **One crypto provider serves everything** (`pollis-core/src/commands/mls/provider.rs`): `PollisProvider` = `openmls_rust_crypto` 0.5. #454 instead **routed** the provider by ciphersuite, because its hybrid suite (X-Wing / `0x004D`) was implemented only by `openmls_libcrux_crypto`, whose AES-GCM decryption has an unpatched non-constant-time tag check (RUSTSEC-2026-0211) — so classic AES-GCM traffic had to be kept off it. #668 dissolved that by moving the PQ suite to a code point RustCrypto implements, and libcrux left the dependency graph; #669 then retired the classic suite, so there is no second suite to route in any case. Pinned by the `mls_backend_is_rustcrypto` test.
- **Cipher suite (one).** `CS_PQ` = `MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44` (code point `0x0052`), whose KEM is X-Wing = X25519 + ML-KEM-768 (FIPS 203), so a recorded session stays sealed unless *both* are broken, and whose leaves sign ML-DSA-44 (FIPS 204). HPKE per RFC 9180. #454 ran it as a second suite beside the classic `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` (`0x0001`) so that a fleet mid-upgrade could still add each other; #668 moved it in place from `0x004D` (same X-Wing KEM, Ed25519 → ML-DSA-44 leaves); #669 deleted the classic suite and renamed `CS_HYBRID` to `CS_PQ`. The suite is still an explicit argument to the two functions that create suite-bound material — `create_mls_group_in_suite` and `build_key_package_in_suite` — rather than a hidden constant, because `0x0052` is provisional and a renumber must stay a one-constant edit. Each device publishes ONE KeyPackage pool; the DS still stores its suite in `mls_key_package.ciphersuite` so a claim narrows on it and a package from a retired code point is never served. `user_device.pq_capable` is dead since #669 — retired in place, read by nothing. Signing is a function of the suite (`provider::signature_scheme`), so a device keeps one signing key **per scheme** — Ed25519 and ML-DSA-44, both bound by a single v2 `device_cert`.
- **Which suite a group runs.** `CS_PQ`, always — `init_mls_group` takes no decision. #454 chose per group via `suite_for_new_group`, behind a roster gate (every device on the desired roster `pq_capable`) *and* a deployment-wide fleet gate (no unrevoked `user_device` seen within 90 days still classic-only), failing toward availability. #669 deleted both gates with the classic suite: with one suite there is no device the choice could strand.
- **Suite migration** (#454 P4) does not change a running group's suite, which RFC 9420 forbids. `migrate_to_current_suite_if_due` stands up a **successor group** in `CS_PQ` for any conversation whose stored suite is not `CS_PQ`, moves the roster by Welcome, and retires the predecessor. It survives the classic suite's retirement because `0x0052` is a provisional code point: a renumber is this same migration with a different constant. Ordering is the monotone key `(conversation_id, generation, epoch)`; local `GroupId` is the conversation id verbatim at generation 0 and `"{conversation_id}#g{generation}"` after, tracked in the `mls_generation` table. The DS accepts generation N+1 at epoch 0 only when its `closes_epoch` names the head of generation N (`pollis_delivery::commit::accepts()`, Kani-proved) — a compare-and-swap that makes two competing successors unrepresentable. No member is stranded: a KeyPackage in the target suite is claimed for every roster device before anything is created, and the first miss aborts the whole migration. Forward-only: pre-crossing traffic stays sealed under the predecessor's suite.
- **Self-update / PCS** (#666): each device rotates its own leaf on join and, sweep-driven, whenever its leaf in a conversation is older than `SELF_UPDATE_INTERVAL` (7 days) plus a deterministic per-conversation jitter of up to 2 days, capped at `MAX_SELF_UPDATES_PER_SWEEP` (3) per sweep. Launch-driven by design — no background timer. This is also what keeps commit size logarithmic rather than linear: unmerged leaves are the expensive part of a hybrid UpdatePath (hybrid 8→16 members costs +10.6 KB unmerged vs +2.4 KB merged).
- **Group topology:** one MLS group per Pollis Group (shared by all its channels); one MLS group per DM channel.
- **Membership changes** flow through `reconcile_group_mls_impl` in `pollis-core/src/commands/mls/reconcile.rs`. It diffs the desired roster against the actual MLS tree and emits a single combined commit carrying `Add` + `Remove` proposals. The commit is *staged* locally, submitted to the Delivery Service via `POST /v1/commits` (commit, `GroupInfo`, and the per-recipient welcomes in one request — `mls/delivery.rs`), and only merged locally once the DS accepts it. That ordering is the invariant preventing "local epoch ahead of remote" split-brain, and the DS is what makes it enforceable: a second device committing from the same epoch gets `409 Conflict` (`SubmitResult::LostRace`) and retries against the new epoch rather than forking the group.
- **External commits** (RFC 9420 §11.2.1) handle new-device joins without requiring a sibling Welcome: the device fetches the latest `GroupInfo` from `mls_group_info` and externally commits into the group.
- **Cross-signing verification** runs at two points. The Delivery Service verifies a cert against the account's stored `account_id_pub` before accepting the publish (`pollis-delivery/src/cert.rs` — the format itself lives in the shared `pollis-device-cert` crate, so client and servers cannot drift). Receivers verify again on inbound commits that add devices (`verify_added_devices`). A failing cert does **not** cause the commit to be dropped: a commit that won the epoch CAS is canonical and immutable, and deleting one forks the group — that was a real production wedge. Instead the commit is applied to stay on the single canonical branch, and the uncertified device is evicted the MLS-native way by the next reconcile, one epoch later. Verification failure is thus enforced by eviction rather than rejection, bounded to one epoch of exposure.
- **Account-key TOFU** runs on every group reconcile and every DM message ingest. `batch_check_and_pin_account_keys` in `pollis-core/src/commands/safety.rs` bulk-fetches every roster peer's `account_id_pub` (`POST /v1/read/account-keys` since #987 — one batched request per reconcile, not one per contact), pins first-seen values locally (`contact_verification` table), and emits a `KeyChanged` realtime event on mismatch. This closes the historical group MITM hole — previously only the DM path detected Turso-side key swaps; groups inherited the gap. The pin is per-USER (not per-conversation), so verifying a peer once propagates a shield badge to every surface where they appear.
- **Roster-change banners.** A non-empty reconcile commit emits a `RosterChanged` realtime event with the per-user diff (joined / left / device added / device removed). The reconciler emits locally + broadcasts to the conversation's LiveKit room so already-connected peers render the inline timeline banner without refetching. See `pollis-core/src/commands/mls/reconcile.rs` and `frontend/src/stores/rosterChangeStore.ts`.

For the full key-material taxonomy, KDF/AEAD parameters, and attack-surface analysis see `docs/security-whitepaper.md`.

---

## Frontend data flow

```
React component
  → invoke("command_name", { args })            // from frontend/src/bridge
    → tauri invoke(cmd, args)                   // bridge → @tauri-apps/api/core
      → #[tauri::command] shim                  // src-tauri/src/commands/*.rs
        → pollis_core::commands::*              // pollis-core/src/commands/*.rs
          → pollis-delivery  (POST /v1/… — every remote read AND write)
          → SQLCipher local  (plaintext, MLS state, secrets)
        ← Result<T>
      ← Result<T>
    ← Result<T>
  ← TanStack Query cache
```

The renderer never imports `@tauri-apps/*` directly. It imports `invoke` / `Channel` / window / dialog / fs / shell / app / updater from `frontend/src/bridge`, a thin runtime-host bridge that routes to Tauri's `invoke`. Real business logic lives in `pollis-core`, which has no shell-runtime dependency and is also consumed by uniffi-generated mobile bindings.

**TanStack Query is the source of truth** for remote data. Components read through hooks in `frontend/src/hooks/queries/`. **MobX** holds only UI state — selected group/channel, transient session data, current user reference. There is no parallel client-side store for remote data. The stores in `frontend/src/stores/` are MobX class singletons; components read them inside `observer()` wrappers.

Routing uses TanStack Router with **memory history** (no browser URL bar in a desktop app). `AppShell` is the root route; key routes are documented in `.codesight/wiki/overview.md`.

---

## Backend commands

Roughly 170 `#[tauri::command]`s are registered in `src-tauri/src/lib.rs`'s `invoke_handler!`,
grouped by module under `pollis-core/src/commands/`: `auth`, `pin`, `account_identity`,
`device_enrollment`, `user`, `groups`, `dm`, `messages`, `mls`, `livekit`, `voice`, `r2`,
`blocks`, `transparency`, and the media/capture helpers.

**The inventory is deliberately not repeated here.** It changes every release and a copy in
this file is a copy that goes stale — which is exactly how this document came to name four
commands that no longer exist (#803). `invoke_handler!` is the only complete list, and
`.codesight/wiki/commands.md` is the annotated reference. Note that not every public function
in `commands/` is a registered command: `reconcile_group_mls_impl`, `create_mls_group`, and
the key-package helpers are internal and reached only from other commands.

---

## Project structure

The workspace splits the backend into `pollis-core` — reusable, with no shell-runtime
dependency — and the shells that consume it. That split is what lets the desktop host, a
TUI, and mobile-via-uniffi share one copy of the command/state/db/MLS code.

| Crate | Role |
|---|---|
| `pollis-core` | The backend. Commands, `AppState`, both DB layers, MLS, media, uniffi exports for mobile |
| `src-tauri` | The shipping desktop host: `#[tauri::command]` shims, `ChannelSink`, the integration test harness |
| `pollis-delivery` | The Delivery Service (axum). Every remote write, plus credential brokering and retention sweeps |
| `pollis-tui` | Terminal client — a second consumer of `pollis-core`, and a cheap check that the crate stays shell-agnostic |
| `pollis-capture-linux`, `pollis-capture-macos`, `pollis-capture-proto` | Camera/screen capture helper subprocesses and their shared wire protocol |
| `verifiable-log`, `verifiable-log-builder`, `verifiable-log-serve` | The transparency log: tree/statement types, the CI-side builder, and the read path |
| `pollis-relay` | Overlay relay node (see `docs/relay-overlay-design.md`) |
| `pollis-device-cert` | Device cross-signing certificate types |
| `pollis-schema` | The remote schema as data: `migrations/` (main DB) + `migrations-log/` (commit-log DB) and the constants that embed them. Dependency-free, so `pollis-delivery` — which must not depend on `pollis-core` — builds its test databases from the same definitions (#875) |
| `pollis-sqlcipher` | The SQLCipher the **Windows** build links, and only Windows: the stock amalgamation (`vendor/sqlite3.c`, unpatched) compiled with `-DSQLCIPHER_CRYPTO_LIBTOMCRYPT` against a ~20-function header whose primitives are RustCrypto, so no OpenSSL lands beside libwebrtc's BoringSSL (#992). Its C is built on every target anyway, so its known-answer and at-rest tests run on the ordinary Linux gate |

Outside the Rust workspace: `frontend/` (React renderer — the renderer reaches the host only
through `frontend/src/bridge`, never `@tauri-apps/*` directly) and `website/` (static
marketing HTML on Cloudflare Pages, not part of the app).

Within `pollis-core`, the pieces worth knowing before reading the code: `commands/` (the real
implementations), `db/` (libSQL remote + SQLCipher local + the numbered migrations), `state.rs`
(`AppState`), `sink.rs` (the `EventSink` trait that keeps `pollis-core` unaware of Tauri),
`net/` (overlay/relay transport), `media_server.rs` (the loopback HTTP server serving decrypted
media bytes to the WebView), and `signal/` — which is now only `mls_storage.rs`, the MLS state
backend; the Signal protocol itself is long gone and the directory name is a fossil.

---

## Security model summary

| | Trusted | Untrusted |
|---|---|---|
| | User's device, the device keystore (the OS keychain, or — where none exists — a machine-bound encrypted file, which is weaker: see `.codesight/wiki/overview.md`), the SQLCipher local DB, the signed Tauri binary (host + WebView renderer + `pollis-core`) at the installed version, the user-held Secret Key, the user-held PIN | Network, Turso, **`pollis-delivery` (the Delivery Service)**, Cloudflare R2, LiveKit, Resend, server operators |

The Delivery Service is ours and it is still untrusted. It authenticates devices, orders MLS
commits, and brokers credentials — all over material it cannot read. A fully compromised DS
can censor, delay, or reorder delivery and can observe the same metadata Turso does; it cannot
decrypt a message, forge a commit that members' clients will accept, or recover a private key.

Binary integrity at install and at every auto-update rests on the OS-native code signature: Apple Developer ID + notarization (Gatekeeper checks both at launch and at install) on macOS, Azure Trusted Signing (Authenticode) on Windows. The Tauri updater verifies the same signature before invoking the installer, so a tampered download cannot replace the running binary.

What the server can see: user metadata, social graph, encrypted message envelopes (size, sender, timestamp), MLS commit/welcome timing, public keys.

What the server cannot see: message plaintext, MLS application secrets, attachment plaintext, account-identity private keys, the PIN, the Secret Key, the SQLCipher key.

Voice traffic is end-to-end encrypted at the frame level: each audio frame is AES-128-GCM-encrypted by libwebrtc's `FrameCryptor` post-Opus / pre-SRTP. The keying is two steps, and conflating them causes confusion: MLS exports a **32-byte** secret per epoch (`MlsGroup::export_secret("pollis/voice/v1", epoch, 32)`), which is then fed to LiveKit's `KeyProvider::with_shared_key` and PBKDF2-derived into the **128-bit** AES-GCM frame key — defaults matched to `livekit-client` JS so peers interoperate across SDKs. Every MLS epoch advance re-derives and rotates the key on the live provider (`on_mls_epoch_changed`); libwebrtc's 16-key ring keeps the previous key available for frames still in flight. LiveKit acts as an SFU but forwards ciphertext only — it never sees plaintext audio. The same shape Discord ships in their 2024 DAVE protocol. See `docs/security-whitepaper.md` § 10.2 for the full design and `pollis-core/src/commands/voice_e2ee.rs` for the implementation.

---

## Where to go next

- `.codesight/wiki/index.md` — full doc tree (database, MLS, commands, UI components, testing harness, audio pipeline, PIN design, notifications, Windows signing).
- `docs/security-whitepaper.md` — auditor-facing protocol/threat model and standards references.
- `CLAUDE.md` — operating principles and constraints (what to build, what not to build).
- `docs/deployments.md` — what ships where, and the CI pipelines that put it there.
- `src-tauri/src/lib.rs` (`invoke_handler!`) and `pollis-schema/migrations/` — the two
  authoritative inventories. This document deliberately links to them rather than copying
  them, because copies go stale silently and a stale architecture doc is worse than none.
