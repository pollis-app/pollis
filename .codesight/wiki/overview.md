# Overview

## Stack

**Tauri 2** desktop app: a React/TypeScript renderer running in the OS WebView (WebKitGTK on Linux, WKWebView on macOS, WebView2 on Windows), with the Rust backend compiled into the Tauri host process. The renderer reaches the backend through the shell-agnostic bridge `frontend/src/bridge`, which routes to Tauri's `invoke`.

- **No backend server.** The Rust core (compiled into the Tauri host process) connects directly to Turso (1 network hop). All business logic runs in `pollis-core`; the renderer invokes commands via `invoke(cmd, args)` through the bridge.
- **Media stays in Rust.** Voice and screenshare run inside `pollis-core` via the `livekit` + `libwebrtc` crates, not in the renderer. Two reasons: (a) cross-platform parity — the same Rust pipeline is consumed by mobile through uniffi bindings; (b) predictable allocation — multi-MB media buffers passed through a JS heap produce visible GC stutter, while Rust's manual allocation does not. This also sidesteps WebKitGTK's lack of WebRTC on Linux — the Rust path is the only one that works on every target.

## Data Flow

```
React component
  → invoke("command_name", { args })            // from frontend/src/bridge
    → @tauri-apps/api/core invoke(...)          // bridge routes to Tauri
      → #[tauri::command] shim                   // src-tauri/src/commands/*.rs
        → pollis_core::commands::*              // pollis-core/src/commands/*.rs
          → Turso (remote, metadata) or SQLite (local, secrets)
        ← Result<T>
      ← Result<T>
    ← Result<T>
  ← React Query cache
```

The `#[tauri::command]` shims in `src-tauri/src/commands/<module>.rs` are mechanical — each is registered in `src-tauri/src/lib.rs`'s `invoke_handler!` and forwards the JSON-shaped call to the corresponding `pollis_core::commands::…` function. Real logic lives in the `pollis-core` workspace crate so other front-ends — a CLI, a TUI, mobile via uniffi — can consume it without any shell-runtime dependency. **Edit `pollis-core`, not the shims.**

**React Query** is the source of truth for remote data. **MobX** holds only UI state (current user ref, transient session data).

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
pollis-core/src/
  commands/          # Real command implementations (auth, groups, messages, mls, voice, ...)
  db/                # Turso + local SQLite connections + numbered migrations
  config.rs          # Env var config
  keystore.rs        # Device keystore: OS keychain, else encrypted file
  state.rs           # AppState shared across commands
  realtime.rs        # LiveKit room manager + event dispatch
  sink.rs            # EventSink trait (frontend-channel abstraction)
  signal/            # MLS storage backend
  util.rs            # Crate-wide primitives with no better home (now_unix)
  lib.rs             # uniffi exports for mobile bindings

src-tauri/src/       # Tauri desktop host — the shipping shell
  commands/          # Thin #[tauri::command] shims forwarding to pollis_core
  sink.rs            # ChannelSink adapter (Tauri's ipc::Channel → EventSink)
  test_harness.rs    # Multi-client integration harness (feature = "test-harness")
  lib.rs             # tauri::Builder, plugin setup, invoke_handler!, lifecycle
  main.rs            # Binary entry

frontend/src/
  bridge/            # Runtime-host bridge — invoke/Channel/window/dialog/fs/shell/app/updater route through Tauri's invoke
  components/        # React components (Auth/, Layout/, Message/, ui/, Voice/, ...)
  hooks/queries/     # React Query hooks (useGroups, useMessages, usePreferences, ...)
  pages/             # Route pages
  services/          # api.ts (invoke wrappers), r2-upload.ts
  stores/            # MobX class singletons (appStore.ts)
  types/             # TypeScript types

mobile/              # Standalone Expo/RN app (NOT a pnpm workspace member) —
                     #   consumes pollis-core via uniffi (modules/pollis-native)
  eas.json           # EAS build profiles (dev/preview/production). Linked to the
                     #   @pollis/mobile EAS project (projectId in app.json), which
                     #   is what lets a device mint a push token at all (#707).
                     #   fingerprint runtimeVersion lives in app.json, NOT here —
                     #   a profile carrying it makes EAS CLI reject the whole file
  modules/pollis-native/  # uniffi-bindgen-react-native turbo module (JSI bridge)

website/             # Static marketing site (Cloudflare Pages, not part of the app)
```

**Mobile build (#706):** the mobile app has no released output yet, but CI now
builds a real Android APK. The `android-build` job in
`.github/workflows/mobile-core-check.yml` runs the full chain on `ubuntu-latest`
— uniffi binding gen + a 3-ABI `cargo-ndk` cross-compile of `pollis-core`,
`expo prebuild`, then `gradlew :app:assembleRelease` — and uploads the APK
artifact. It uses `expo prebuild` + Gradle directly (never `eas build`, so the
job needs no account, token or queue) and signs with the throwaway debug
keystore; distribution signing is blocked (Play console / Apple account #723).
See `docs/deployments.md` and `mobile/CLAUDE.md`.

**Shared Rust primitives.** `pollis-core/src/util.rs` and
`pollis-delivery/src/util.rs` hold the crate-wide odds and ends — currently just
`now_unix()` (whole seconds since the epoch, `u64`; callers whose downstream
arithmetic is signed cast at the boundary). Put a helper there rather than
re-deriving it in a module: before #875 the DS carried six copies of that clock
in two return types and pollis-core three under two names. Lowercase hex is
`hex::encode` — every crate that needs it already depends on `hex`, and the four
hand-rolled `hex_lower` loops are gone. The two `util` modules are separate
because `pollis-core` and `pollis-delivery` deliberately do not depend on each
other; a genuinely shared home needs a crate below both, which does not exist yet.

## Storage Model

| Store | Contents | Never stores |
|-------|----------|--------------|
| **Turso** (remote) | Users, groups, channels, membership, public keys, encrypted message envelopes, MLS commit log, MLS welcomes, GroupInfo | Message plaintext, private keys |
| **SQLite** (local, per-user, encrypted) | Decrypted messages, MLS group state (`mls_kv`), preferences cache | User profiles, groups, channels (fetched from remote) |
| **Device keystore** | ML-DSA-44 account identity key pair (private half is its 32-byte seed, so the wrapped blob is the same size it was under Ed25519), session token, device ID, DB encryption key | Anything in plaintext — see below |

### The device keystore has two backends (#882)

`keystore.rs` picks one **at runtime**, once per process, and never switches
mid-session (a store split across two backends is worse than either):

| Backend | Used when | What guards the bytes |
|---|---|---|
| **OS keychain** | A credential store answers a read probe — macOS Keychain, Windows Credential Manager, Linux Secret Service. Preferred always. | The OS |
| **Encrypted file** | No credential store answers (a server over SSH), or a debug build (no dev-loop prompts), or mobile | AES-256-GCM under a machine-bound KEK (desktop) / an OS-held key (Android Keystore, iOS Keychain) |

There is no third backend. `BackendKind` has exactly two variants and neither
writes plaintext; before #882 the file backend was plaintext JSON and reachable
by turning one Cargo feature off.

**Desktop KEK:** `HKDF-SHA256(ikm = platform machine ID, salt = fresh 16 B per
write)`. The machine ID is `/etc/machine-id` (Linux), IOPlatformUUID (macOS) or
MachineGuid (Windows) — a 128-bit host value that lives *outside* the data
directory. `POLLIS_KEYSTORE_MACHINE_ID` overrides it for hosts that have none;
with no source at all the keystore **errors** rather than degrading.

**What that buys, precisely.** It defeats the file *leaving the machine*: a
home-dir backup, an `scp` of the data dir, a synced folder, a resold disk read
on other hardware. It does **not** defend against a local attacker running as
the same UID (they can derive the same KEK), nor against a full-disk image or
VM snapshot (`/etc/machine-id` is on the same disk and world-readable), nor
against forensic recovery of the pre-migration plaintext blocks.

It is one of **two** layers. The secrets in that file — `db_key_wrapped_{uid}`,
`account_id_key_wrapped_{uid}` — are already Argon2id + XChaCha20-Poly1305
ciphertext under a PIN-derived KEK (`commands/pin.rs`). The PIN is *not* mixed
into the file KEK: the file is read before the PIN exists (boot reads
`pin_meta_{uid}` to decide which screen to show), and the PIN's own layer is
already inside. Each layer covers the other's gap — a 4-digit PIN alone is
~40 minutes of offline sweep at the tuned Argon2id cost once the file is
copied, and machine binding alone stops nothing local.

A TPM/Secure Enclave would improve this (a sealed key is in no backup and no
disk image). Deliberately not integrated: `tss-esapi` is a heavy C dependency,
needs a TPM *and* a reachable resource manager, and needs three platform
integrations — a half-integration with a silent fallback would look stronger
than it is.

**Migration.** A pre-#882 plaintext file is still readable, and the next read or
write replaces it with ciphertext through the existing tempfile + fsync +
rename. Every interruption leaves either the complete old file or the complete
new one. A file that cannot be decrypted is **never** rewritten or backed away —
those bytes are someone's identity key.

## Security Model

**Trusted:** User's device, local database, the signed Tauri application binary (Tauri host + WebView renderer + `pollis-core`) at the installed version, and the device keystore — the OS keychain where one exists, otherwise the machine-bound encrypted file at the reduced strength described above.
**Untrusted:** Network, Turso, server operators.

## Network egress & the closed-overlay relay

Every outbound connection goes to a fixed, small set of first-party hosts: the Delivery Service, Cloudflare R2, and LiveKit. (Turso used to be on that list; since #987 only the DS dials it — the client holds no database credential and does not link `libsql`.) The optional **closed-overlay relay** (`pollis-relay` crate; design `docs/relay-overlay-design.md` §14) can route the metadata-sensitive **control plane** through a first-party relay so the services see a relay's IP instead of the user's. It is **off by default** and inert unless `POLLIS_OVERLAY` selects a non-off mode at runtime.

- **Config** (`config.rs`): `POLLIS_OVERLAY` = `off` (default) | `prefer` | `strict`; `POLLIS_OVERLAY_RELAY` = the relay endpoint. Unknown/empty → `off`.
- **Wiring** (`pollis-core/src/net/overlay.rs`): when a non-off mode is set, `AppState::new` starts a loopback SOCKS5 shim (`pollis_relay::OverlayShim`) once and stores the `OverlayHandle` on `AppState.overlay`. The routing policy sends DS/R2 **and LiveKit signaling** (#813 Phase E1) through the overlay; **LiveKit media stays direct** (§6.4). Signaling and media share a hostname, so that split is enforced by the *transport*, not by a host list: the shim is a SOCKS5 CONNECT (TCP-only) proxy that callers opt into, while RTP/ICE is UDP emitted by the Rust media stack's own sockets, which never dial it. Overlaid signaling hides the signaling connection's address from the SFU — it does **not** make a call IP-private, because ICE hands the SFU the client's address by design.
- **Seams:** control-plane HTTP goes through `net::overlay::http_client(state.overlay.as_ref())` (a `socks5h://` reqwest client) instead of `reqwest::Client::new()`; the inner TLS still terminates at the real service, so the relay only forwards opaque bytes. That is now the ONLY seam: #987 deleted `overlay_connector` and its `tower::Service<hyper::Uri>` wrapper, which existed for exactly one caller — libsql — and took `pollis-core`'s direct `hyper` 0.14 / `hyper-rustls` 0.25 / `tower-service` dependencies with them. **`http_client` caches and clones; nothing builds its own.** `reqwest::Client` is `Arc`-backed, so cloning shares the connection pool, but a freshly-built one starts with an EMPTY pool and silently pays a full DNS+TCP+TLS handshake per request — measured at 3-4.5s per DS POST on a mobile dev build. There are exactly three sanctioned factories (`pollis_relay::http` for the client, `pollis_delivery::util` for the server, and `net::overlay`'s re-export), and `no_crate_builds_its_own_reqwest_client_outside_the_seam` fails the build for anything else (#875 found four live sites in the DS and three in `pollis-core`). On the DS side the factory itself (`util::http_client`) is **private** since #913 — callers go through `util::http_post(Upstream, url)`, which cannot be called without naming an upstream and therefore a request timeout, so an outbound call with no deadline is not expressible.
- **Fleet-wide schedules jitter (#875, `pollis_relay::backoff`).** Anything a whole fleet runs on a repeating tick spreads itself, or every device hits the same host on the same instant. `jittered` is symmetric ±12.5% and is used for failure backoffs; `jittered_down` only ever shortens and is used where the CEILING is a safety margin rather than a sanity clamp — `net::peer::reachability` must refresh before the revocation evidence it forwards on goes stale, so overshooting spends a margin that was calculated. The two scheduled refresh loops (`net::overlay`'s directory refresh, `net::peer::reachability`) previously slept a value derived from the signed artifact's `expires_at` — **the same timestamp for every client** — so the fleet was permanently in lockstep, not just during outage recovery.
- **Off = unchanged:** with `POLLIS_OVERLAY` unset, `overlay` is `None` and `http_client(None)` is a plain client — byte-for-byte the pre-overlay behavior.
- **Left direct:** nothing on the client, since #987 moved push fan-out server-side (`pollis-delivery/src/push.rs`). Expo is a non-first-party host the overlay's allowlist could not carry, so this was a documented residual leak (§14.4) for as long as clients POSTed to `exp.host` themselves; the DS does it now, which also took `push_token` out of client visibility. The `ureq` transparency verifier is **no longer** direct: `build_agent` takes an optional SOCKS5 proxy and `transparency.rs` passes `socks5://<shim>` to the `verify_*_via` entry points, so the verify path routes through the overlay when on (§14.4 residual closed).
- **Modes:** `prefer` falls back to a direct dial if the overlay is unreachable; `strict` surfaces a degraded error rather than silently going direct (messages-must-work).
- **Relay auth (offline, no metadata-plane query).** The relay authenticates a connecting device with an **offline device-certificate chain** — no Turso query, no network call per connection — which is what keeps a relay node out of the metadata plane (design §11.1; `docs/relay-operations.md`). The client presents **both** device leaf signing keys — Ed25519 (`mls_signature_pub`) and ML-DSA-44 (`mls_signature_pub_pq`), since the v2 cert binds both — plus the cert chain (`account_id_pub`, `device_cert`, `identity_version`, `issued_at`); the relay verifies possession (handshake signature, ML-DSA-44 under the PQ leaf key) + membership (`verify_device_cert`) locally. The cert primitive lives in the shared **`pollis-device-cert`** crate (deps: `ml-dsa` + std), which `pollis-core` mints with and re-exports, and `pollis-relay` verifies with — one frozen format, no crate cycle. A device with no cert yet (pre-enrollment/OTP bootstrap) can't cert-auth and stays direct.
- **Deployable node** (`pollis-relay` bin): TOML config file (bind / allowlist / persisted QUIC identity / rate limits / `allow_extend`), generate-and-persist self-signed QUIC identity, graceful shutdown (drain on SIGTERM/SIGINT), and in-memory per-account / per-IP rate + concurrency limits (`Rejected(RateLimited)`). Stateless, disposable, rotatable; holds no Turso/DS credentials. `GET /version` reports the node's **build** identity (`sha`) and **protocol** identity (`protocol` = the newest ALPN it speaks, `pollis-relay/4`); the automated AWS pool's reconciler uses protocol identity for signed-directory membership (a wrong-generation node is excluded so no client can fail ALPN against it) and build identity to cycle nodes off a stale image, so an image roll converges the fleet without a mutable `:latest` tag (#703). See `docs/relay-operations.md` and `infra/relay-hydra/README.md`.
- **Multi-hop onion (wire v4, #813 Phase A, design §6.2).** A `Circuit` is an ordered `Vec<Hop>`; `n = 1` is the shipped v0 path, `n > 1` telescopes. Against each hop the client sends its device handshake plus `Extend(next)`; that relay QUIC-dials the next one (pinning the 32-byte cert fingerprint the frame carries), announces itself with a `Layer` frame, and splices — after which the client runs **one nested TLS 1.3 session per hop** (`pollis_relay::onion`, pinning that hop's leaf cert) straight through it. So each relay peels exactly one layer: hop 1 sees the client's address and hop 2's, a middle hop sees only its neighbours, and the **last hop** — always first-party — sees the destination and its predecessor, never the client. The destination allowlist is enforced wherever the `Connect` lands, which is by construction the last hop, so the closed-overlay guarantee holds at every path length. Versions negotiate purely in ALPN (a v4 relay still offers `pollis-relay/3`, so v4 clients and the shipped v3 fleet interoperate single-hop); `Extend`/`Layer` are refused below v4, so a v3 stream can't request forwarding. **What this does NOT hide:** every hop still verifies the device cert, so each learns *which account* built the circuit — the property delivered is network-address unlinkability, not anonymity.
- **Path & guard selection (#813 Phase B, `pollis-core/src/net/path.rs`, design §4.4/§6.2).** Which relays a circuit is made of is client policy. Target length is **2 hops** (a pinned guard + a first-party exit — three is Tor's answer to an *untrusted* exit, which we do not have), capped at 3, and the selector always **leaves a spare**: a pool of `n` builds at most `n-1` hops, so a 2-node pool stays single-hop rather than losing failover. **The last hop is first-party by construction** — `RelayPath`'s terminal field is a `FirstPartyEndpoint`, a newtype whose only constructor rejects a peer relay, so a path exiting through a peer does not typecheck; peer relays (Phase D1) are middle-hops-only. A path never repeats a node, and guard ≠ exit within a circuit. **Guards:** 2 pinned entry relays persisted in `overlay-guards.json`, 30-day lifetime, replaced when they leave the directory / are revoked / expire — **never** for being unreachable (evicting on failure is the guard-discovery attack).
- **Peer relays in the signed directory (#813 wave 3, `pollis-core/src/net/{directory,path,overlay}.rs` + `infra/relay-hydra/reconciler/peers.mjs`).** Consenting devices carry traffic as **middle hops**, and the signed directory is the only channel they may arrive through (an unsigned one would be a way to feed a target a hand-picked set of relays). The reconciler asks each node it advertises for the peers parked with it (`GET /peers` on the health port), aggregates them pool-wide, drops any the revocation list covers, and publishes a **second additive optional** field — `peers: [{cert_b64, parked_at[]}]`, `version` still `1`, key omitted when empty, so a peerless pool publishes pre-#813 bytes. Client side: a peer entry becomes a `RelayTier::Peer` endpoint whose address is a **rendezvous** (a first-party relay holding its parked link — a peer accepts no inbound connections), it is dropped if unpinnable or parked at no advertised relay, and it is re-checked against revocation at dial time. Selection: **at most one peer per path**, never the guard or the exit, and a peer *lengthens* a path (2 → 3 hops) rather than filling a first-party slot — the spare rule counts first-party relays only, so volunteers can never inflate the pool into losing failover.
- **One key, two artifacts — the cross-artifact guard (#875).** `POLLIS_OVERLAY_DIRECTORY_KEY` signs BOTH the directory and `revocations.json` (a second key would be a second thing to rotate), so a valid signature proves only *who signed*, never *which artifact* — an on-path attacker can serve one where the other is expected and both signatures check out. Each payload therefore names itself: `type: "pollis-relay-directory"` (`net::directory::DIRECTORY_TYPE`) and `type: "pollis-relay-revocations"` (`pollis_relay::REVOCATION_TYPE`), and each verifier rejects the other's name. The revocation verifier **requires** its `type`; the directory verifier is deliberately one-sided — it rejects a present-and-wrong `type` but ACCEPTS an absent one, which is what today's reconciler publishes. Making it required would couple a client release to a reconciler release with no safe ordering (a fleet-wide outage waiting on a deploy race) and buy nothing: absence is already unambiguous, because every other artifact this key signs does carry a `type`. What the guard buys is that the rejection is a decision rather than a side effect of which required fields the other artifacts happen to lack — one `#[serde(default)]` away from disappearing. `version` stays `1` on both, as with every other additive field. The JS mirror is `infra/relay-hydra/lib/directory-verify.mjs`.
- **Client-side revocation (#813 Phase B/C, `pollis-core/src/net/revocation.rs`).** The directory carries an additive `revocation: {seq, path, count}` anchor (`version` stays `1`); the client fetches the short-lived signed `revocations.json` on the same refresh tick and verifies it with `pollis_relay::verify_revocations`. **Fail-closed:** no list, a list expired *at use time*, a list whose `seq` is below the anchor, or any verification error all yield an **empty** relay pool — never the unfiltered one — so `Prefer` dials direct and `Strict` degrades. One carve-out for deploy order: while the anchor is absent or says `count == 0`, an unreachable list means "no revocations known"; a held revocation is always honoured regardless.

## Realtime

LiveKit rooms carry realtime events (new_message, membership_changed, voice_joined, etc.). The Rust event loop in `livekit.rs` receives data events and pushes them through the `EventSink` trait; `src-tauri/src/sink.rs`'s `ChannelSink` wraps a `tauri::ipc::Channel<E>` so the event rides Tauri's IPC channel to the renderer, which subscribes through the bridge's `channelOn(id, handler)`. MLS operations (process commits, poll welcomes) fire as needed.

Events are a **convenience for speed**, not a correctness requirement. All MLS state is also read from the DB on every message send/receive, so offline devices catch up when they next interact.

## Voice

Voice channels run entirely in Rust by design — cross-platform parity with mobile (same uniffi-exposed `pollis-core`) and predictable allocation under heavy media buffers. The capture pipeline is `cpal mic → optional RNNoise → WebRTC APM (AGC2 + NS + HPF + AEC) → LiveKit publish`; remote playback is `NativeAudioStream → per-track buffers → mixer task (10 ms tick) → single cpal output stream`, which is also where the AEC render reference is tapped. Settings (mic boost, AGC target, NS level, AEC, click suppression) live in user preferences and apply mid-call via `set_voice_audio_processing` without rejoining. See [audio-processing.md](./audio-processing.md) for the full pipeline, framing constraints, and tuning surface.

Voice is end-to-end encrypted: each frame is AES-GCM-encrypted by libwebrtc's `FrameCryptor` post-Opus, keyed by a 32-byte secret derived from the channel's MLS group (`MlsGroup::export_secret("pollis/voice/v1", epoch, 32)`). The LiveKit SFU forwards ciphertext only; the key rotates automatically on every MLS epoch advance. See `pollis-core/src/commands/voice_e2ee.rs` and the "End-to-end encryption" section of [audio-processing.md](./audio-processing.md#end-to-end-encryption).

---
_Back to [index.md](./index.md)_
