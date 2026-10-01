# Pollis Security Whitepaper

**Audience:** independent security auditors evaluating the protocol design and the flows around it.
**Scope:** the desktop and mobile clients built from this repository (`pollis-core` behind a Tauri or React Native shell), the Delivery Service (`pollis-delivery`), the remote services (Turso, Cloudflare R2, LiveKit, Resend, Expo push), and the trust boundaries between them. Covers cryptographic protocol design, key custody, identity, group membership, and every path that moves plaintext or key material across a trust boundary. Web-app concerns (XSS, CSP, SOP) are out of scope.
**Status:** authoritative for cryptographic claims. `ARCHITECTURE.md` and `.codesight/wiki/` are authoritative for implementation detail; where they disagree with this document on a cryptographic claim, this document wins.

---

## 1. Trust Model

### 1.1 Boundaries

| Trusted | Untrusted |
|---|---|
| The user's device | The network, on every path |
| The device keystore: the OS keychain (Keychain / Secret Service / Credential Manager) where one exists, otherwise a weaker machine-bound encrypted file (§3.5) | Turso (libSQL), the remote relational database |
| The signed application binary at the version the user installed | Cloudflare R2, object storage for attachments |
| The local SQLCipher database, encrypted at rest on every platform; the crypto provider differs per platform (§7.0) | LiveKit, the SFU and realtime-event channel |
| The user-held Secret Key (shown once, stored offline) | Resend, which carries OTP email |
| The user-held PIN | Expo / APNs / FCM, which carry mobile push |
| `accounts.json`, the local index of accounts signed in on this device (§1.1.1) | The Delivery Service (`pollis-delivery`): the only party holding the database credential, and the broker for the Resend, R2, LiveKit and Expo credentials (§4, §8, §9.3, §10.1) |
| — | Anyone holding a copy of `accounts.json` or the keystore but not the PIN |

The operators of the Pollis services also build and ship the app. As with Signal Desktop or WhatsApp Desktop, the binary is trusted at install time, and from then on the protocol defends against the server side of the same operator. Two mechanisms back binary integrity.

**Platform code-signing.** Apple Developer ID plus notarization on macOS; Azure Trusted Signing on Windows (`.codesight/wiki/windows-signing.md`). The updater checks the OS-native signature on every downloaded installer before launch (Gatekeeper, Authenticode), so an installer tampered with in transit does not install.

**Binary transparency.** Code-signing proves only that the holder of Pollis's key produced some bytes. The binaries log makes the set of bytes Pollis has published for each release public and append-only, so a targeted per-user build is either logged permanently or visibly absent. Design: `docs/verifiable-builds-design.md`; residuals: `docs/reproducible-builds-residuals.md`.

- Each release's pre-signature payload hash and signed-artifact hash, plus the pinned build recipe, are appended to a third ML-DSA-44-signed Merkle tree at `https://verify.pollis.com/v1/binaries`, beside the commit-log and account-key trees (§6.9). It has its own STH context (`pollis-verifiable-log:sth:v2:binaries`), so its heads cannot be replayed as either other tree's.
- `pollis-verify release <tag>` checks that every artifact for a tag is included, trusting only the pinned log key. The Security page has an in-app "Verify this build" check that does the same for the running binary.
- **Linux AppImage payload: reproducible, and checked.** The toolchain is pinned, absolute build paths are remapped, and `SOURCE_DATE_EPOCH` comes from the tag commit. `.github/workflows/rebuild-verify.yml` rebuilds the payload from public source after every desktop release and asserts the hash matches the logged leaf, trusting only the pinned key. At `v1.8.4` the rebuild matched the logged payload `1a4213a1…` exactly.
  - Bound: reproduction needs the same build path, because `--remap-path-prefix` is a rustc flag and does not reach C/C++ built through `cc-rs`.
  - Every compile-time input (`option_env!` in `pollis-core/src/config.rs`) is a public endpoint URL or public verification key. The client bakes no credential, so a party with no secrets can reproduce the payload from the published recipe. `scripts/check-build-recipe.py` fails CI if the release job, the rebuilder and `config.rs` disagree on that recipe.
- **macOS and Windows: recomputable, not reproducible.** The logged payload digest is the shipped `.dmg` / `.exe` with per-signing material removed: on macOS the stapled notarization ticket, each Mach-O code signature and the `_CodeSignature` manifests; on Windows the Authenticode certificate table and the PE checksum, stripped from every executable in the installer (#750). Anyone with the public artifact can apply the same normalization and compare with the log. This is the usual reproducible-builds treatment of signatures (cf. F-Droid). It does not show that those bytes rebuild from source; that waits on a matching-platform reproducer and the compiler-determinism items in the residuals doc. The signing/notarization wrapper is non-reproducible by construction; it is logged and bound to the payload.
- **Independent anchor.** Every installer and updater bundle also carries a keyless SLSA v1 build-provenance attestation (`actions/attest-build-provenance`) and a cosign signature, both recorded in the public Rekor log and bound to Pollis's GitHub Actions OIDC identity. They are published on `cdn.pollis.com`, the attestation at the `provenance_uri` each binaries-log leaf records. `cosign verify-blob` or a SLSA verifier confirms the bytes came from the pinned release workflow with no Pollis-held key on the path. This proves provenance, not reproducibility.

#### 1.1.1 `accounts.json`

The one client-side store that is neither encrypted nor machine-bound.

**Contents, per account:** the opaque `user_id` (a 128-bit ULID), the `username` and `avatar_url` (which every peer already sees), and `last_seen`; plus one top-level `last_active_user`. Nothing else.

**Protection:** file permissions only. Since #1000 it is created through `pollis-core/src/private_fs.rs` (0600, in a 0700 directory) on unix; on Windows it inherits the profile directory's ACL. A same-UID attacker or a full-disk image reads it regardless, but both already have the keystore, which is worth far more.

**Why that is acceptable:** it holds no secret and no PII, an argument that does not depend on the mode. A reader learns that the machine has accounts and their opaque ids, not who owns them. The file exists for bootstrap: it is the only store readable before unlock (local DB closed, no signer, no session), and `get_session` / `get_unlock_state` need a `user_id` before any credential exists.

**The login email is not in it (#997).** It lives in the per-user keystore slot `login_email_{user_id}`, under the same protection as the account identity key and the DB key. Pre-unlock email → `user_id` resolution still works because the keystore is readable then (`device_id_{user_id}` is already read at that point). Masking and hashing were rejected: the resolution is a case-insensitive equality test, so masked addresses at a shared domain collide, and email is low-entropy enough that a hash adds confirmation-resistance, not secrecy.

An install older than #997 still has the address in its file. The first launch on a newer build writes it to the keystore and only then rewrites the file without it, and erases nothing unless every address was moved. An interrupted migration retries next launch. Nothing ever sets the field again.

Corrupt-index snapshots (`accounts.bad-<unix-ms>.json`) have the same contents and permissions and are pruned to the newest three on the next good write. A snapshot of a pre-migration file still carries the old address; the prune bounds how many such copies survive.

### 1.2 What the server can and cannot see

**The Delivery Service** is the only client-facing server for data (§8). It sees everything Turso stores, plus live request metadata: the authenticated user and device on every signed request (`X-Pollis-User` / `X-Pollis-Device`, `pollis-delivery/src/auth.rs`) and the client IP (unless the user has opted into the relay overlay, below).

**Turso** holds metadata in plaintext:
- user records (id, email, username, avatar URL);
- the social graph: group membership, DM membership, blocks;
- conversation metadata: timestamps, MLS commit and Welcome timing, key-package availability;
- device registration (cert blobs, `mls_signature_pub`, `mls_signature_pub_pq`), push tokens, and `security_event` rows.

Turso sees only the Delivery Service's connections. Since #987 no client connects to it (§8).

Turso cannot recover message plaintext, any private key, MLS group state or secrets, or attachment plaintext. The account identity key exists server-side only inside the `account_recovery` blob, wrapped under the user's Secret Key, which is never sent (§5.2). Attachments are encrypted on the device before upload (§9.1).

**Metadata minimization** (design and threat model: `docs/metadata-minimization-design.md`):

- **Sealed sender (v1).** The `message_envelope` row stores a non-identifying sentinel with `sealed = 1` instead of the sender. Recipients attribute each message from the MLS-authenticated `{user_id}:{device_id}` credential inside the ciphertext (§2.3, §6.6). A breach, subpoena or dump of `message_envelope` no longer shows who sent what. **This is at-rest only:** the DS still authenticates every send by the sender's device signature, so a live DS operator sees the sender. Closing that needs anonymous membership proofs (v1.5), which are not built.
- **Size padding (v2).** Text plaintext is padded to PADMÉ buckets (≈12% worst-case overhead, 256 B floor) inside the MLS ciphertext (`pollis-core/src/commands/messages/framing.rs`). Attachment blobs are not padded: their size is inherent to cross-user dedup (§9.1).
- **Signalling minimization (v2).** See LiveKit below.

Still visible, and irreducible for a store-and-forward server: which conversations exist, roughly how many members each has, and when they are active. Membership rows are keyed by `user_id`; per-conversation pseudonyms (v3) and timing batching (v4) are not built.

**Network address.** An opt-in relay overlay (`docs/relay-overlay-design.md` §13; #455, #813) hides the client IP from the first-party services. It is built but off by default, and release builds do not turn it on. Pollis claims no anonymity.

**Post-quantum.** Group traffic uses a hybrid X25519 + ML-KEM-768 KEM (#454; §6.1, §6.10), so a recording made today is not decryptable by a future quantum computer. Account identity keys, device certs, DS request auth, transparency-log tree heads and every MLS leaf sign with ML-DSA-44 (#668, #669). Groups created before the PQ suite ran classic until they migrated, and traffic sealed before that boundary stays classically sealed.

**LiveKit** sees realtime data-channel events (`new_message`, `membership_changed`, `enrollment_requested`, `typing`, voice presence). These are JSON signalling, not message content, and are not encrypted at the application layer.
- `new_message` is a bare conversation-routing ping with no sender; the recipient attributes the message from the decrypted MLS credential (§6.6).
- `typing` and `voice_joined` / `voice_left` carry no `user_id` / `username` / `display_name`; recipients attribute them from the publishing participant (#836).
- Room names are opaque per-conversation pseudonyms derived server-side (#828, `pollis-delivery/src/room_id.rs`), and participant identities are per-room encrypted handles with no username in the JWT (#836, `participant_id.rs`). The SFU cannot map a participant to an account or link one account across rooms. It can count participants in a room and recognise a returning participant within one room. 1:1 call rooms (`call-<ulid>`) are per-call.
- Voice audio is frame-encrypted with AES-128-GCM before it leaves the device (§10.2). LiveKit sees RTP routing metadata, not audio.

**R2** sees opaque AEAD ciphertext at content-hash-derived keys (§9.2). It never sees the plaintext or the AEAD key.

**Resend** sees an email address and a 6-digit OTP in plaintext while delivering the message.

**Push (mobile only).** The DS sends push notifications through Expo, which hands them to APNs/FCM (`pollis-delivery/src/push.rs`). Desktop registers no push token. Each push has a fixed title and body and a `data` blob containing only an opaque handle `h`: 128 random bits, minted fresh per recipient per notification (#1122, #1157). The app trades `h` for `{conversation_id, kind}` over its own signed channel (`POST /v1/push/resolve`), which answers only the user it was minted for. The DS keeps the handle mapping for `PUSH_HANDLE_TTL_DAYS` (7). Expo, Apple and Google therefore see the device token, the notification time, and an unlinkable handle, but not the conversation, sender or content.

---

## 2. Identity Layers

Pollis has three nested identities. The rest of the document depends on keeping them apart.

### 2.1 Account identity (per user)

A long-lived **ML-DSA-44** keypair (FIPS 204), generated on the device that completes signup (`pollis-core/src/commands/account_identity.rs::generate_account_identity_material`); it was Ed25519 until #668. The public half is published to `users.account_id_pub` (BLOB, 1312 bytes). The private half is stored as its 32-byte seed, the same size as the old Ed25519 private key, so every slot that holds, wraps or transports it (§3.2, §5.1, §5.2) kept its size. It exists in exactly two places:

1. On the user's enrolled devices, only as ciphertext in the keystore slot `account_id_key_wrapped_{user_id}` (§3).
2. On the server, only as ciphertext in `account_recovery`, wrapped under a key derived from the user's Secret Key, which the server never sees.

When `users.account_id_pub` rotates (`reset_identity`), `users.identity_version` increments. A device whose local private key no longer derives the published key is treated as orphaned and wiped on its next sign-in (`auth.rs::verify_otp`, `account_identity.rs::has_matching_local_account_identity`).

### 2.2 Device identity (per device per user)

Each device gets a stable ULID `device_id` on first sign-in (`auth.rs::register_device`), stored in the keystore at `device_id_{user_id}`. It also generates one stable MLS signing keypair **per signature scheme** (`pollis-core/src/commands/mls/device.rs`): ML-DSA-44 for the current suite's leaves and Ed25519 for the classic suite's. #669 retired the classic suite, so nothing mints an Ed25519 leaf any more. The Ed25519 key is still generated, published and certified, because the scheme decides which stored key verifies a leaf and a group persisted under an older code point must stay readable. A device that only ever ran a pre-#668 build has just the Ed25519 key, in the legacy unsuffixed `mls_kv` row, which is still read.

Public halves are stored locally in `mls_kv` and remotely in `user_device.mls_signature_pub` (Ed25519) and `user_device.mls_signature_pub_pq` (ML-DSA-44, nullable, migration `000011_device_pq_signature_pub.sql`).

The ML-DSA-44 key is also the device's DS request-auth key. Every DS request is signed with it and verified against `user_device.mls_signature_pub_pq` (`pollis-core/src/commands/mls/ds_client.rs`, `pollis-delivery/src/auth.rs`). The `X-Pollis-Signature` header is base64 of a 2420-byte signature, about 3228 characters.

Both device public keys are cross-signed by the account identity key in **one** signature, the `device_cert`: ML-DSA-44 over a domain-separated, length-prefixed payload binding `device_id`, both device public keys, the `identity_version` at issuance, and the issuance time (`pollis-device-cert/src/lib.rs::device_cert_signed_payload`, domain `pollis-device-cert-v2\0`; format in §5.3). Certifying both keys at once means no leaf key is ever in use uncertified. Other clients use the cert to decide whether to admit a leaf into an MLS group.

### 2.3 MLS leaf identity (per device per group)

Each device's signing key populates a `BasicCredential` (RFC 9420 §5.3) whose content is the UTF-8 string `{user_id}:{device_id}` (`pollis-core/src/commands/mls/provider.rs::make_credential`). One credential covers every KeyPackage and leaf node the device produces in any group, so one `device_cert` covers the device's whole MLS surface.

---

## 3. PIN-Wrapped Key Storage

The PIN is a device-local unlock factor, not a server credential. It never leaves the device and the server has no record of it.

### 3.1 KDF and AEAD choices

Source: `pollis-core/src/commands/pin.rs`.

- **PIN format:** 4 ASCII digits (`validate_pin`), about 13 bits.
- **KDF:** Argon2id (RFC 9106), `argon2` 0.5, version 0x13, `m_cost = 64 MiB`, `t_cost = 3`, `p_cost = 1`, 32-byte output. About 250 ms on a mid-range Apple-silicon or Ryzen 5 machine, above the OWASP 2024 minimum (m=19 MiB, t=2). Parameters are stored in the `pin_meta_{user_id}` blob, so they can change on any re-wrap without a migration.
- **Salt:** 16 bytes from `OsRng`, fresh per user per re-wrap.
- **AEAD:** XChaCha20-Poly1305 (`chacha20poly1305` 0.10) with 24-byte random nonces, chosen over AES-256-GCM because random 24-byte nonces remove nonce-reuse risk.

### 3.2 Wrapped material

Three slots sit under the PIN-derived KEK:

- `pin_meta_{user_id}`: the fixed plaintext `b"pollis-pin-ok\0\0\0"` under the KEK. A wrong PIN fails on this 16-byte blob, so rejecting it costs one Argon2 evaluation, not three.
- `db_key_wrapped_{user_id}`: 32 random bytes, the SQLCipher key for `pollis_{user_id}.db`.
- `account_id_key_wrapped_{user_id}`: the 32-byte ML-DSA-44 seed (§2.1).

`pin_meta` also stores `failed_attempts` (u32 BE) and `last_attempt_unix` (u64 BE) outside the AEAD. They are not secret: the relevant attacker already has keystore read access and can count attempts themselves.

### 3.3 Lockout

`MAX_FAILED_ATTEMPTS = 10`. The 10th wrong PIN deletes all three slots (`pin.rs::nuke_wrapped`). The SQLCipher file and its WAL/SHM are left on disk but are unreadable, because their key is gone. The server-side account is untouched. The device is now equivalent to a new one and must re-enrol by Secret Key (§5.2) or by another device's approval (§5.1).

There is no time-based backoff. Against offline brute force the defence is ~250 ms of Argon2id per guess plus the 10-attempt ceiling; for attempts through the UI, the ceiling is the rate limit.

### 3.4 Key custody at rest

After PIN setup, `db_key` and `account_id_key` exist on disk only inside AEAD ciphertext. In process they live in `Zeroizing<Vec<u8>>` (`AppState.unlock`) and are scrubbed on drop. `lock()` drops the unlock state and closes the SQLCipher handle, returning to "needs PIN" without signing out.

### 3.5 Where the wrapped blobs physically sit (#882)

The keystore backend is chosen at runtime, once per process, and fixed for that process (a store split across two backends is worse than either):

1. **OS keychain** (macOS Keychain, Windows Credential Manager, Linux Secret Service) wherever one answers a read probe. Always preferred.
2. **Machine-bound encrypted file** (`keystore.pks`) where none answers, typically a headless server reached over SSH. Before #882 the choice there was refusing to start or writing keys in the clear.

The file is AES-256-GCM under `HKDF-SHA256(ikm = machine ID, salt = fresh 16 bytes per write)`. The machine ID is `/etc/machine-id` or `/var/lib/dbus/machine-id` (Linux), IOPlatformUUID (macOS) or MachineGuid (Windows): a 128-bit host-unique value that does not travel with a copy of the data directory. HKDF rather than Argon2id because the input is already high-entropy; the memory-hard cost is on the PIN (§3.1). With no machine ID available the keystore errors and names `POLLIS_KEYSTORE_MACHINE_ID` instead of falling back to a constant.

**Defends against** the file leaving the machine: a home-directory backup, an `scp` of the data dir, a synced folder, a container image built over a live data dir, a resold disk read on other hardware.

**Does not defend against:**
- A local attacker with the **same UID**, who can derive whatever the process derives. Only the OS keychain raises that bar, which is why it stays preferred.
- A **full-disk image or VM snapshot**, which includes the world-readable `/etc/machine-id`.
- **Forensic recovery** of pre-migration plaintext blocks. The migration replaces the file atomically but cannot securely erase the old inode on a modern SSD or CoW filesystem.

The PIN layer and the machine layer cover each other's gaps. The PIN alone is weak against an exfiltrated file (10⁴ candidates, about 40 minutes single-core at ~250 ms each). The machine binding alone is weak against a same-UID attacker. Together the attacker needs both the host identity and the PIN.

The PIN is not mixed into the file KEK. It cannot be: the keystore is read before any PIN exists (boot reads `pin_meta_{uid}` to choose between "enter PIN" and "set PIN", and `device_id_{uid}`). It also need not be, because the secrets inside are already PIN-wrapped.

TPM / Secure Enclave sealing would improve the same-UID and disk-image cases, since a sealed key appears in no image. It is not used: `tss-esapi` is a heavy C dependency, needs a TPM and resource manager reachable by the user, and would take three platform integrations. A partial integration with silent fallback would advertise protection a deployment might not have.

Mobile uses the same file format, sealed under a key held by the Android Keystore or iOS Keychain.

### 3.6 Migrating an existing plaintext keystore

Installs older than #882 have a plaintext keystore. It stays readable (locking a user out of their identity key is unrecoverable), and the first read or write re-encrypts it in place via tempfile + fsync + rename. Encoding completes before the old file is touched, so an interruption leaves either the whole old file or the whole new one.

A file that fails to decrypt, or decrypts but fails to parse, is never rewritten, moved aside, or treated as empty: those bytes are someone's identity key. The keystore reports a stable error and the only way to start over is the user's explicit "wipe this computer". (Before #950 a parse failure renamed the file to a sidecar, which behaved like a wipe.)

#950 also renamed `dev-keystore.json` to `keystore.pks`. The rename runs as write-new, read-back, unlink-old, so an interruption leaves both files, never neither, and the next start finishes it.

### 3.7 Comparable systems

- **Signal Desktop** encrypts its SQLCipher store with a random key held in the OS keystore, with no user factor. Pollis adds the PIN: cloning the keystore without the PIN does not decrypt local data. This is closer to iOS message-cache protection.
- **1Password / Bitwarden** use Argon2id with similar parameters, but over a high-entropy master password. Pollis has a 4-digit PIN; the 10-attempt wipe is what closes that gap.
- **WhatsApp Desktop** keeps its database key on disk with no user factor, like Pollis before PIN. Legacy unwrapped slots are now read only to wrap them at `set_pin`.

---

## 4. Authentication Flow (OTP)

Source: `pollis-delivery/src/otp.rs` (the OTP machinery) and `pollis-core/src/commands/auth.rs::request_otp`, `verify_otp` (thin clients).

The OTP proves control of an email address and nothing else. It is not the unlock factor (the PIN) or the recovery factor (the Secret Key).

- Generated, stored, verified and emailed by the **DS**. The client calls `POST /v1/auth/request-otp` and `POST /v1/auth/verify-otp` and only ever handles a code the user typed.
- 6 digits from `OsRng` (`gen_range(0..1_000_000u32)`), zero-padded.
- Stored in DS memory as `SHA-256(salt ‖ code)` with a fresh 16-byte salt. TTL 10 minutes; deleted on first successful verification. Compared in constant time (`otp.rs::constant_time_eq`).
- Sent by the DS to `api.resend.com` using `RESEND_API_KEY` from the DS environment. The client holds no Resend key.
- **Throttling** (`OtpConfig`, all keyed on the mailbox, and surviving a fresh `request-otp` since #1088):
  - 30 s between sends to one address (`PrepareOutcome::Throttled`);
  - at most 3 codes per TTL window;
  - after 5 wrong codes the mailbox, not just the code, is locked for 15 minutes.
  - Codes accumulate rather than replace, so requesting a new code does not invalidate one the user is about to type.
  - A per-client-IP window (`pollis-delivery/src/ratelimit.rs`) covers the many-addresses case. See §11.1.
- **The counters are not durable (#1142).** The store is an in-process map, and the DS container scales to zero after `sleepAfter` of idle time, which drops every counter and lockout. This is safe only while `sleepAfter >= ttl_secs`: an attacker must go silent long enough to reset the counter, and that silence also expires the code they were guessing. `pollis-delivery/tests/otp_state_durability.rs` pins the inequality across the TypeScript and Rust sources.

OTP is used in two places:
1. **Signup** (no `users` row yet). `verify_otp` creates the row, generates the ML-DSA-44 account identity and a Secret Key, and seeds `AppState.unlock`. The PIN-create screen that follows is what persists that material, as ciphertext under the PIN KEK.
2. **Soft recovery** (`reset_identity_and_recover`, §11.5), which requires the OTP plus a constant-time match of the typed email against `users.email`.

Returning users on an enrolled device skip OTP and enter their PIN against `pin_meta_{user_id}`. Before the PIN gate, transient keystore read failures (macOS keychain hiccups, Secret Service races) sent returning users back to OTP on every cold start. Rationale: `.codesight/wiki/pin-design.md`.

---

## 5. Multi-Device Enrollment

A user with an existing `account_id_pub` adds a device in one of two ways. Both end with the new device holding the account identity private key, having published a `device_cert` and `KeyPackage`s, and having joined every existing MLS group by external commit. Both tiers (desktop and mobile) can be either side.

### 5.1 Approval path (in-band, sibling-device-mediated)

Source: `pollis-core/src/commands/device_enrollment.rs`.

1. The new device generates an **ephemeral X25519 keypair** (`x25519-dalek` 2.x). The private half lives only in memory (`AppState.enrollment_ephemeral_keys`); restarting the app forfeits the request.
2. It **derives** an 8-character verification code (SAS) from its ephemeral public key: `HKDF-SHA256(salt = "pollis-enrollment-sas-salt-v1" ‖ len‖request_id ‖ len‖user_id ‖ len‖created_at, ikm = ephemeral_pub, info = "pollis-enrollment-sas-v1")`, 5 bits per character onto the Crockford base32 alphabet, 40 bits. Lengths are 4-byte big-endian, so the encoding is injective. The code is not secret; the server can compute it. It is displayed on the new device.
3. It writes a `device_enrollment_request` row (ephemeral public key, code, `pending`, 10-minute TTL) through a session-gated DS endpoint (`/v1/auth/enrollment-request`), since it has no signing key yet. The **DS** then sends `enrollment_requested` to the user's inbox room so online siblings see it at once; an offline sibling picks it up on its next login.
4. The sibling shows an **empty input**. The user reads the code off the new device and types it. The sibling re-derives the code from the ephemeral public key it fetched itself and compares in constant time (#1096). It never shows or trusts the DS's stored copy.
5. The sibling generates its own ephemeral X25519 keypair and computes ECDH, refusing a non-contributory (low-order) result. The wrap key is `HKDF-SHA256(ikm = ECDH, info = "pollis-enrollment-wrap-v1" ‖ requester_pub ‖ approver_pub)`. AES-256-GCM (12-byte random nonce) wraps the 32-byte ML-DSA-44 seed. The blob is `approver_pub ‖ nonce ‖ ciphertext+tag`, 92 bytes. It is written through `POST /v1/enrollment/approve`, which flips the status to `approved` and is bound server-side to the signer's own account. A `security_event` of kind `device_enrolled` (`via=approval,approver={device_id}`) is recorded.
6. The new device's `poll_enrollment_status` sees `approved`, unwraps with its in-memory private key, and checks that the seed's public half equals the published `account_id_pub` (a successful AEAD open only says the blob was sealed to this device). The seed and a fresh `db_key` populate `AppState.unlock`; PIN-create follows.
7. `finalize_device_enrollment` publishes the new device's `device_cert` and 5 `KeyPackage`s, then external-joins every group and DM the user belongs to (§6.4).

This is one-shot ECDH-then-AEAD, not an authenticated key exchange: nothing signs the approver's ephemeral key with the long-term account key. Authentication rests on the human comparison:

- Because the code is a function of the ephemeral public key (#793), an attacker who can write Turso and swaps in their own key changes the code the approver derives, and the two screens disagree.
- Because the approver shows an empty input and compares against its own derivation (#1096), the comparison is actually performed. When the approver displayed the DS's stored code and submitted it on one tap, a DS that swapped both the key and the stored code passed every check.
- Finding a substitute keypair that yields the same code costs ~2^40 keygens. The per-request salt makes that work per request, inside the 10-minute TTL; unsalted (before the 2026-09-18 review), one precomputed table worked against every user forever. The 8-character width matters: 6 digits would have been ~2^20.

Residual: no long-term-key signature over the approver's ephemeral key, so authentication is only as good as the human comparison.

### 5.2 Secret Key recovery path (out-of-band)

Source: `device_enrollment.rs::recover_with_secret_key`, `account_identity.rs::unwrap_recovery_blob`.

The Secret Key is 30 Crockford base32 characters (no I/L/O/U), prefixed `A3-`, dashed every 5 characters: 150 bits.

- **KDF:** HKDF-SHA256, `info = b"pollis-account-key-wrap-v1"`, a per-user 32-byte `OsRng` salt chosen at signup. IKM is the normalized key body (case-folded, dashes and whitespace removed).
- **AEAD:** AES-256-GCM, 12-byte random nonce.
- **Stored row** (`account_recovery`): `salt` (32 B), `nonce` (12 B), `wrapped_key` (48 B: 32 B seed + 16 B tag).

There is no Argon2 here: a 150-bit random secret needs no stretching, which is why it is generated rather than chosen. HKDF derives a uniform 256-bit key from it with a domain-separating `info`.

The key is shown once at signup and once more on `reset_identity`. The app neither stores nor retransmits it. The shape matches 1Password's Secret Key and Apple's iCloud Recovery Key: the operator stores an encrypted backup it cannot open.

### 5.3 Device cross-signing

Cross-signing stops the server from inserting a rogue device into a user's MLS groups by writing a fake `user_device` row.

Source: `account_identity.rs::sign_device_cert`, over the canonical format in the dependency-free `pollis-device-cert` crate, which `pollis-delivery` also uses to re-verify at `POST /v1/auth/publish-device-cert`, so client and server cannot drift. Signed payload:

```
DEVICE_CERT_DOMAIN ("pollis-device-cert-v2\x00", 22 bytes)
|| u8(device_id_len)   || device_id (UTF-8)
|| u16(ed25519_pub_len, BE) || ed25519 device pub (32 bytes)
|| u16(mldsa_pub_len, BE)   || ML-DSA-44 device pub (1312 bytes)
|| u32(identity_version, BE)
|| u64(issued_at, BE)
```

Length prefixes rule out concatenation ambiguity. The NUL-terminated domain separator stops the account key's signature being reinterpreted under another format, and the `v2` bump stops a v1 cert (one device key) being read under the two-key layout. Key lengths take `u16` prefixes because an ML-DSA-44 public key is 1312 bytes; `device_id` keeps `u8` (ULIDs are 26 bytes). Signatures are ML-DSA-44, 2420 bytes.

One function enforces the cert on both sides of every add: `mls/device.rs::IdentityDirectory::leaf_verdict(user, device, leaf_signature_key, scheme)`. Its inputs (`account_id_pub`, and the device row's `device_cert`, `cert_issued_at`, `cert_identity_version`, `mls_signature_pub`, `mls_signature_pub_pq`, `revoked_at`) come from the DS, but the verdict is computed on the client, because the DS is outside the trust boundary. A leaf is `Certified` only if the device row is live, the cert verifies under the account key, **and the leaf's signature key is byte-equal to the certified key for the group's scheme**. The last clause stops a genuine cert for a real device vouching for an attacker's leaf that claims the same `user:device` credential.

1. **Committer.** `reconcile_group_mls_impl` (and suite migration, which reuses its staging) reads the roster's cert material in one `POST /v1/read/roster-identities` and pins it. The actor's own account key comes from the local keystore; a peer whose reported key differs from the local TOFU pin (§5.4) has its key dropped. A claimed KeyPackage becomes an Add only if its leaf is `Certified`. `KeyPackageIn::validate` alone only proves someone holds the leaf key, which a DS substituting a package at claim time can satisfy. A refused package is burnt and reported (`ReconcileOutcome::refused_uncertified`); nothing is committed. The same pass evicts any existing leaf that is positively uncertified (revoked, bad cert, or not the certified key) with a normal remove commit.
2. **Replaying members.** `process_pending_commits_inner` reads the added leaves from the staged commit's own Add proposals (`apply_one_commit` → `AddedLeaf`), never from the committer-written `added_user_id` / `added_device_ids` columns, which are only a hint the DS uses to prefetch cert rows. An uncertified leaf is logged as an `uncertified_mls_leaf` `security_event` (shown on the Security page) and triggers an immediate eviction reconcile. The commit is still merged: it won the epoch and is canonical, refusing it would strand the device behind the group, and a staged commit cannot be re-processed later.

For the audit: a KeyPackage whose leaf the claimed account did not certify is never added by a conforming committer. A leaf that a non-conforming (malicious or pre-fix) committer adds anyway is detected by every honest member from the commit itself, recorded, and evicted by the next honest reconcile. The residual is the window between that commit and the eviction commit, during which the rogue leaf holds the group's key schedule; it is bounded by one honest member's reconcile, which starts as soon as the leaf is seen. Both halves are tested in `src-tauri/tests/flows/cross_signing.rs`, where the DS hands the committer a forged KeyPackage for a real device.

### 5.4 Safety numbers and key pinning

Cross-signing protects the tree against rogue devices under an unchanged account key. It does not help if the server swaps the account key itself. Two mechanisms cover that:

- **TOFU pinning.** `batch_check_and_pin_account_keys` (`pollis-core/src/commands/safety.rs`) runs on every inbound DM message and every group reconcile, before roster devices are added. It pins first-seen `account_id_pub` values and emits a `KeyChanged` event on mismatch, which shows an inline banner in every conversation with that peer and clears their verified shield. The committer's leaf check (§5.3) uses the pin as its root.
- **Safety numbers.** A 60-digit number (twelve 5-digit blocks, SHA-512-derived from both parties' account keys, plus a QR payload) for out-of-band comparison. Verification is per user and applies everywhere that user appears.
- The account-key transparency log (§6.9) makes the full key history of every user publicly checkable, not just the keys this device has seen.

---

## 6. End-to-End Encryption (MLS)

### 6.1 Standard and library

- **Specification:** RFC 9420, Messaging Layer Security.
- **Implementation:** `openmls` 0.8, pinned through a workspace `[patch.crates-io]` to an exact upstream `main` revision, because the `draft-ietf-mls-pq-ciphersuites` feature with the ML-DSA suite is not in a release. The patch covers every `openmls_*` crate so the graph cannot split. Bumping it is protocol-visible: the draft renumbers provisional code points.
- **Storage:** `MlsStore` (`pollis-core/src/signal/mls_storage.rs`) implements `openmls_traits::storage::StorageProvider` over the local SQLCipher `mls_kv` table.
- **Crypto provider:** one, `openmls_rust_crypto` over RustCrypto AEAD/HKDF/HPKE (`pollis-core/src/commands/mls/provider.rs`). Its AES-GCM tag check is constant-time (`subtle`). The PQ suite used to be `0x004D`, which only `openmls_libcrux_crypto` implemented; that backend's AES-GCM decryption has an unpatched non-constant-time tag check (RUSTSEC-2026-0211), so the classic suite was routed away from it. Moving to `0x0052` (#668), which RustCrypto implements, removed libcrux from the graph along with six `deny.toml` advisory ignores (`-0211`, `-0209`, `-0210`, `-0124`, `-0075`, `-0073`). `mls_backend_is_rustcrypto` (`pollis-core/src/commands/mls/tests.rs`) pins the single backend.
- **Cipher suite.** One, and every group is on it. A group's suite is part of its `GroupContext`; there is no global switch and no negotiation.

  | | `CS_PQ` |
  |---|---|
  | Name | `MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44` |
  | Code point | `0x0052` (provisional) |
  | KEM | **X-Wing**: X25519 + ML-KEM-768 (FIPS 203) |
  | AEAD | ChaCha20-Poly1305 |
  | Hash / KDF | SHA-384 / HKDF-SHA384 |
  | Signature | **ML-DSA-44** (FIPS 204), scheme `0x0904` |

  X-Wing derives the shared secret from both encapsulations, so it is at least as strong classically as the DHKEM it replaced and also resists a quantum adversary. An attacker must break both.

- **Suite history.** #454 shipped this KEM alongside the RFC 9420 mandatory suite `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` (`0x0001`) so a mid-upgrade fleet could still add each other; §6.10 covers how a group chose. #668 then moved the PQ suite in place from `0x004D` to `0x0052`: same X-Wing KEM, but ML-DSA-44 signatures and therefore SHA-384. #669 retired the classic suite (`CS_CLASSIC` deleted, `CS_HYBRID` renamed `CS_PQ`). Traffic sealed under the classic suite stays sealed under it. Because `0x0052` is provisional and the draft has renumbered before, the suite remains a parameter of the functions that mint suite-bound material, and a renumber would be handled as a lineage migration (§6.10).

- **Why signatures are post-quantum too.** The #454 argument for leaving signatures classical holds for messages: a harvest-now adversary attacks confidentiality, and a future quantum computer cannot forge a signature that was already verified. It does not hold for material whose verifiability must outlast the moment:
  - an account identity key is checked by every device that admits a leaf for that user, for the life of the account;
  - a device cert is a standing claim, re-checked by every client that admits the device and by the DS at publish time, and its verification is pure (no clock), so it stays checkable as long as it is on file;
  - the transparency log (§6.9) exists so its signed history stays re-checkable indefinitely. An auditor in 2040 replaying it is checking signatures made today; a forgeable scheme would let a future operator rewrite the past.

  ML-DSA-44 now signs account identity keys, device certs, DS request auth, transparency-log tree heads and MLS leaves.

- **The cost is size.** An ML-DSA-44 public key is 1312 bytes (Ed25519: 32) and a signature 2420 bytes (Ed25519: 64). They sit on every leaf node, every KeyPackage, every device cert and every DS request: roughly 8× more authentication material, on top of the PQ encapsulations (§6.7). The `X-Pollis-Signature` header grows from 88 to ~3228 characters, within default limits on the path (hyper 16 KiB per header, Cloudflare 16 KiB total); no proxy in front of the DS may be configured below an 8 KiB header budget.

### 6.2 Group lifecycle

- **One MLS group per Pollis group.** All channels in a group share its MLS group; the channel ID is metadata on the application message.
- **One MLS group per DM.**
- **Group ID:** the Pollis conversation ID (a ULID).
- **Creation:** `init_mls_group` (`mls/group_state.rs`) seeds epoch 0 with `use_ratchet_tree_extension(true)`, so every Welcome carries the full ratchet tree.
- **Membership changes** go through one function, `mls/reconcile.rs::reconcile_group_mls_impl`. It builds the desired roster from `group_member` ∪ `group_invite` (groups) or `dm_channel_member` (DMs), compares it with the actual tree, claims KeyPackages for missing devices, and emits one commit with all `Add` and `Remove` proposals. Pending invitees are added at invite time, so the Welcome already exists when they accept.

### 6.3 Commit/Welcome ordering

The remote commit log is the source of truth for MLS state. Every commit (reconcile and self-update alike) is published through `reconcile.rs::publish_staged_commit`:

1. Build the commit and **stage** it locally as a pending commit; the local epoch does not advance.
2. Submit the commit, the resulting-epoch GroupInfo and the Welcomes for added members to the DS in one request (`POST /v1/commits`). The DS accepts it only if this `(conversation, generation, epoch)` is unclaimed, and writes all three atomically, so no Welcome can point at a losing branch.
3. **Won:** `merge_pending_commit` advances the local epoch.
4. **Lost race or transport error:** ambiguous, because the commit may have landed with the response lost. The client checks the canonical log (`our_commit_is_canonical`, Kani-proved decision core; #411). If its exact bytes are at this epoch it adopts the win. Otherwise it calls `clear_pending_commit`, stays at the prior epoch and converges on the winner.

The invariant is that the local group is never ahead of the log. Merging before the remote write succeeds would leave this device at an epoch no other member can reach: permanent split-brain.

### 6.4 External commit / new-device join

Source: `mls/group_state.rs::external_join_group`. A new device (§5) cannot wait for a Welcome from a sibling that may be offline, so it uses MLS external commit (RFC 9420 §11.2.1):

1. Fetch the conversation's latest GroupInfo (with its epoch) from the DS.
2. Build `MlsGroup::external_commit_builder` with that GroupInfo and the device's `BasicCredential`; the embedded ratchet tree is enough to issue a commit.
3. Publish through the same DS commit path at the GroupInfo's epoch. Existing members merge it on their next `process_pending_commits` pass.

The commit row carries `added_user_id` / `added_device_ids` for the joining device as the prefetch hint. Members verify the joining leaf exactly as for any add (§5.3): from the commit's own Add proposal, against the joiner's `device_cert`, with the same flag-and-evict response.

### 6.5 KeyPackage lifecycle

Each device keeps 5 KeyPackages published (`mls/key_packages.rs::ensure_mls_key_package`, `TARGET = 5`). They are single-use: the DS claims one atomically by setting `claimed = 1` in a single statement (`pollis-delivery/src/devices.rs`). The device tops up after processing each Welcome (`replenish_key_packages`, called from `poll_mls_welcomes_inner`).

The consumer validates each claimed package (`KeyPackageIn::validate(crypto, ProtocolVersion::Mls10)`): leaf-node signature against the credential key, cipher suite, protocol version. Tampering can make a package fail validation and waste a slot, not pass. Validation does not prove the account certified the leaf; §5.3 does that.

### 6.6 Application message encryption

`pollis-core/src/commands/messages/send.rs::send_message` is the single entry point:

1. Resolve the conversation (`/v1/conversations/catch-up`). For a DM with a block in either direction, store the message locally and stop (§11.4).
2. Apply pending Welcomes (`poll_mls_welcomes_inner`) and commits (`process_pending_commits_inner`); with no local group, external-join.
3. Pad the plaintext (§1.2) and encrypt it as an MLS `application_data` message (`try_mls_encrypt`). The stored form is `mls:` + hex of the TLS-serialised `MlsMessageOut`.
4. `POST /v1/messages/send` with the sealed-sender sentinel, `sealed = 1`, and the `(generation, epoch)` the envelope was sealed at. If a commit has landed since, the DS refuses (#1041) and the client catches up, re-seals at the new epoch and posts again (`messages/seal.rs::post_resealing`).
5. The DS fans out content-free push to the `push_to` audience (§1.2).
6. The client publishes a LiveKit `new_message` ping to wake online recipients. Non-fatal: offline recipients fetch on their next read.

The `mls:` prefix is a format marker kept from the MLS rollout. Decryption (`messages/read.rs::list_messages` → `try_mls_decrypt`) strips it, hex-decodes, and calls `MlsGroup::process_message`.

### 6.7 Forward secrecy and post-compromise security

Both come from MLS (RFC 9420 §15.4–§15.6).

- **Forward secrecy.** TreeKEM advances on every commit and rotates path secrets, so a leaf private key stolen at epoch N decrypts only epoch N. Every membership change commits; there is no heartbeat ratchet, but ordinary activity keeps epochs moving.
- **Post-compromise security.** A thief holding a leaf key at epoch N keeps access until the **victim's own** self-update commit. Someone else's commit re-keys its issuer's path and encrypts to the copath, which still contains the victim's leaf, so the thief rides through it. Pollis issues self-updates from two places (#666):
  - immediately after a device joins a group;
  - from the cold-launch/reconnect sweep, for any group in which the device has not committed for 7 days plus a deterministic per-conversation jitter of up to 2 days, at most 3 groups per sweep (`mls/self_update.rs`).

  Idle groups heal on the next launch of any member, not on a timer, because Pollis does no periodic polling. A group whose members all stay offline does not heal while they are gone. `a_stolen_leaf_is_locked_out_once_the_victim_rotates` (`src-tauri/tests/flows/adversarial.rs`) exfiltrates a real device's MLS state, shows it reading live traffic, shows a third party's commit failing to evict it, and asserts the victim's own rotation does.
- **Post-join rotation also keeps commits logarithmic.** A member added by someone else knows no secrets above its leaf, so its direct path is blank and it appears alone in a copath resolution: one HPKE ciphertext for that member in every commit, until it commits itself. On the PQ suite each such ciphertext is ~35× an X25519 one. `self_update_turns_linear_commit_growth_into_logarithmic` (`mls/tests.rs`) measures doubling a group from 8 to 16 members: +10.6 KB per commit with unmerged leaves, +2.4 KB with all leaves merged.

### 6.8 Bounded history (deliberate)

Messages sent before a member joined are not visible to that member. That is a property of MLS. New devices start empty; there is no Megolm-style key backup. `account_recovery` restores account identity only, never message history. Anyone expecting a backup blob that also seals historical message keys should note that none exists, by design.

### 6.9 Verifiable transparency logs (commit history + account keys)

Pollis publishes append-only, ML-DSA-44-signed Merkle trees (RFC 6962 / RFC 9162) at `https://verify.pollis.com` so anyone can check that the server has not rewritten history. Each tree has its own STH context (`pollis-verifiable-log:sth:v2` for commits, `…:sth:v2:account-keys`, `…:sth:v2:binaries` for §1.1), so a head for one cannot be replayed as another.

- **MLS commit log.** Every membership/key-change commit. Replay proves no fork (no two commits share `(conversation_id, generation, epoch)`), no epoch regression (`(generation, epoch)` increases lexicographically), and that a new generation opens only at epoch 0. This removes the server's ability to fork a conversation, roll an epoch back, or show different histories to different auditors. Detail: `docs/transparency.md`.
- **Account-key directory.** One leaf per identity-key version, `(user_id, identity_version, account_id_pub)`, written to the append-only `account_key_log` table in step with every `users.account_id_pub` change (signup, `reset_identity`). Replay proves each user's key history is append-only with strictly increasing `identity_version`. This backs the TOFU layer (§5.4): TOFU catches a swap only on the next message and only for keys this device has seen; the log exposes every user's whole history.

A verifier trusts only the log's ML-DSA-44 public key, the signed tree head and the Merkle proofs. `pollis-verify` checks the whole log (`remote`), one conversation (`group`), one user's key history (`account <user_id>`), or one release (`release <tag>`). After every publish, CI re-verifies the served tree and compares the new heads with the previous ones; a regression aborts the publish.

The client audits itself too. `self_audit_account_key` runs the same `verify_account` the CLI runs over this user's history and compares the latest version with the device's key; `audit_peer_account_key` does the same for a pinned peer. The log key is pinned in the client (`PINNED_LOG_PUBLIC_KEYS`, `pollis-core/src/commands/transparency.rs`), and `scripts/check-pinned-log-key.py` keeps the other copies in the repo and on the website in agreement (#945). A served key that matches no pin is a hard alarm, since any key can sign a self-consistent forged tree. An empty pin set can only withhold trust (status *unverified*, never `ok` or `alarm`).

Key history: #668 moved the STH signature to ML-DSA-44 but derived it from the old Ed25519 seed (a format change, not a rotation). #732 rotated to fresh material and re-signed all three trees; that key is pinned today. Clients now pin a key *set* whose entries carry an optional `not_after`, so the next rotation can overlap (#740). An offline root key (#754) is pinned as `PINNED_LOG_ROOT_KEYS` to vouch for signer keys via a root-signed key-set statement (`key-set.json`).

**Limits.**
1. **Daily publish lag.** The trees are rebuilt and signed once a day (`transparency-publish.yml`). A new signup or key rotation is invisible to auditors until then; the client reports `pending`, not an alarm.
2. **Client checks are advisory.** They alert; they never block a send.
3. **No private lookups (no VRF).** `user_id`s and account keys are enumerable. The keys are public by design, but the tree leaks the user set and rotation cadence. A CONIKS / Key-Transparency-style VRF layer is the upgrade path.
4. **One first-party log and auditor.** Pollis runs the only log. `pollis-verify` is released so anyone can run an independent auditor, but no third party is contractually watching.
5. **CI is in the publishing TCB.** The signing key is a GitHub Actions secret (`STH_SIGNING_KEY`) and trees are signed in CI, so a compromised Actions environment could sign a bad tree. The post-publish self-audit and equivocation check detect that after the fact; they cannot prevent it. Custody decision and rationale: `docs/sth-signing-key-custody.md`.
6. **#732's rotation had no overlap window.** Re-signing every head under a new key is byte-identical to what a rewriting operator would do. An auditor holding cached pre-rotation heads must re-pin from the announcement; the transition itself is not verifiable. The key set with `not_after` addresses future rotations.

### 6.10 Which suite a group runs, and how a group changes suite

A group's suite is fixed at creation and changes only by explicit migration. It is never negotiated at runtime.

**Birth.** Always `CS_PQ` (`init_mls_group`, `mls/group_state.rs`).

**What #454 required, and why it is gone.** While two suites coexisted, `suite_for_new_group` allowed a hybrid birth only if two gates held. The **roster gate** required every registered device of every desired-roster user to be `pq_capable`. The **fleet gate** required no unrevoked device seen within 90 days to be classic-only. Capability was measured, not self-declared: the DS set `pq_capable` in the same write that landed a device's hybrid KeyPackage pool. The fleet gate was needed because a new group's roster is just its creator, so a classic-only device invited later would have no hybrid KeyPackage and could never join. Both gates failed toward availability, and the decision (`may_birth_hybrid`) was proved under Kani.

#669 deleted all of it: `suite_for_new_group`, both predicates, the dormancy constant, `may_birth_hybrid` and its harnesses, and the DS's `mark_pq_capable`. `user_device.pq_capable` remains as a dead column because migrations must stay additive. The reasoning still holds for a mixed fleet; this deployment had no active users and therefore no old clients to protect.

**Migration (`mls/migrate.rs`).** MLS cannot change a group's suite in place, so a group moves by creating a **successor group** and moving the roster across by Welcome. `migrate_to_current_suite_if_due` fires for any conversation whose stored suite is not `CS_PQ`. It is kept because `0x0052` is provisional: a renumber is this same migration with a different constant.
- `(conversation_id, generation)` names a lineage; generation 0 is the original group. The DS and the transparency log both order by `(conversation_id, generation, epoch)`.
- The DS accepts generation *N+1* at epoch 0 only if the submitter names the head of generation *N* in `closes_epoch` (`pollis_delivery::commit::accepts()`, Kani-proved), so a lineage is succeeded exactly once and never forked.
- **No member is stranded.** Before creating anything, the migration claims a target-suite KeyPackage for every roster device and aborts on the first miss: everyone moves or nobody does.
- The successor starts at epoch 0 with key material not derivable from the predecessor's, so a leaf stolen before the boundary is evicted at it. Members keep history they already decrypted. A member offline across the boundary drains the old lineage to its head before adopting the successor (`max_past_epochs = 0` makes that order necessary). A member whose successor Welcome is lost external-joins the successor. Each case is a `flows` scenario (`src-tauri/tests/flows/pq_migration.rs`), and the model-based fuzzer crosses the boundary under generated churn, offline periods and DS faults.

**Scope.** Traffic sealed before a group migrated stays under X25519. Migration is forward-only and cannot retract a recording an adversary already holds.

---

## 7. Local Encrypted Storage (SQLCipher)

Source: `pollis-core/src/db/local.rs`.

- **Library:** `rusqlite` 0.37 linking SQLCipher 4 (via `bundled-sqlcipher` features on Linux, macOS and mobile; via the `pollis-sqlcipher` crate on Windows, §7.0). Page-level AES-256-CBC with per-page HMAC-SHA512.
- **Key:** `PRAGMA key = "x'{hex}'"` with the 32-byte raw key, which bypasses SQLCipher's PBKDF2. Appropriate because the key is 32 CSPRNG bytes, not a passphrase.
- **Path:** `pollis_{user_id}.db` under the platform data dir (Linux `~/.local/share/pollis`, macOS `~/Library/Application Support/com.pollis.app`, Windows `%APPDATA%\pollis`; mobile passes `POLLIS_DATA_DIR`). `journal_mode=WAL`, `foreign_keys=ON`.
- **Schema version:** on a `LOCAL_SCHEMA_VERSION` mismatch the DB and its sidecars are destroyed and recreated. This triggers only on a missing version row, a version mismatch, or `NotADatabase` (wrong key); any other error surfaces rather than deleting the database.

### 7.0 Where SQLCipher's crypto comes from, per platform (#992)

The on-disk format is identical everywhere; the library supplying AES and HMAC is not.

| Platform | rusqlite feature | SQLCipher crypto provider |
|---|---|---|
| Linux | `bundled-sqlcipher` | System OpenSSL, linked as a **shared** object |
| macOS | `bundled-sqlcipher` | CommonCrypto (`-DSQLCIPHER_CRYPTO_CC`) |
| iOS / Android | `bundled-sqlcipher-vendored-openssl` | Statically vendored OpenSSL |
| Windows | `sqlcipher` (linked) + the `pollis-sqlcipher` crate | `-DSQLCIPHER_CRYPTO_LIBTOMCRYPT`, backed by RustCrypto (`aes`, `sha1`, `sha2`, `hmac`, `pbkdf2`) |

All four produce the same SQLCipher 4 ciphertext; the provider supplies primitives, not format.

**Why Windows differs.** Windows has no shared system libcrypto, so `bundled-sqlcipher` links static OpenSSL, and MSVC pulls whole object files from a static archive. SQLCipher built against OpenSSL 3 headers drags in OpenSSL's provider core and with it much of X.509/PEM/RSA/EC, most of which the BoringSSL that `libwebrtc-sys` links also defines; MSVC treats the duplicate symbols as fatal. Pointing SQLCipher at BoringSSL is also wrong: BoringSSL changed `PKCS5_PBKDF2_HMAC` and related lengths from `int` to `size_t`. `pollis-sqlcipher` compiles the stock SQLCipher amalgamation against a small header implemented by RustCrypto crates already in the graph, so no second libcrypto enters the image.

**Windows builds before #992 stored the database in the clear.** Until #988 the graph also contained `libsql`, whose bundled sqlite3 won every `sqlite3_*` symbol; SQLCipher was never linked and SQLite silently ignored `PRAGMA key`. #988 removed `libsql`, #991 pinned the state, #992 fixed it. Because a plaintext file cannot be opened once the codec is real, `LocalDb::open_at` detects the unencrypted `SQLite format 3\0` header, overwrites the file and its `-wal`/`-shm`/`-journal` sidecars, and recreates it empty. This is the sanctioned "a new device starts empty" loss, chosen over an `ATTACH … KEY` + `sqlcipher_export` migration because there was no Windows user base and a half-finished conversion leaves plaintext on disk.

**The disposal path resists path games (#1000).** It resolves the path once and does every later step on the handle. The open is `O_NOFOLLOW` (`FILE_FLAG_OPEN_REPARSE_POINT` on Windows) and must yield a regular file, so a symlink at `pollis_{user_id}.db` is declined, not followed. On unix the unlink is guarded by a device+inode comparison so it cannot remove a file swapped in afterwards. SQLite's Windows VFS opens without `FILE_SHARE_DELETE`, so another process's connection can make the unlink fail after the overwrite; the file is truncated to zero through the handle first, so that case leaves an empty file (which opens as a fresh encrypted DB) rather than aborting sign-in. The schema-mismatch wipe uses the same shredder, including sidecars; it previously removed only the main file and could leave a plaintext `-wal` behind.

**Tests.**
- `sqlcipher_is_the_sqlite_we_actually_linked`: `PRAGMA cipher_version` must answer on every platform, no `cfg` exception.
- `the_local_database_file_is_encrypted_at_rest`: writes a message via `LocalDb::open_for_user`, reads the file's raw bytes, and requires that the body is absent, the plaintext SQLite header is absent, and neither an unkeyed nor a wrong-keyed connection can read it.
- `the_crypto_provider_is_the_one_this_platform_documents` (#998): `PRAGMA cipher_provider` must match the table above. The table records a build outcome: `libsqlite3-sys` picks CommonCrypto on Apple only when it finds no OpenSSL, so an `OPENSSL_DIR` (or `OPENSSL_LIB_DIR` + `OPENSSL_INCLUDE_DIR`) on a build machine silently moves macOS to OpenSSL.
- The Windows provider has known-answer tests against RFC 6070, RFC 4231 and NIST SP 800-38A vectors.

**Where they run (#998).** Linux on `ubuntu-24.04` in `.github/workflows/mls-tests.yml`; Windows on `windows-latest` in `.github/workflows/windows-link.yml`; macOS on `macos-latest` (Apple Silicon, matching the shipped `aarch64-apple-darwin`) in `mls-tests.yml`'s `macos-at-rest` job. All are path-filtered to Rust changes and report through each workflow's always-reporting gate. The macOS job builds `pollis-core` with `--no-default-features`, dropping the media stack; `cargo tree -i libsqlite3-sys` is identical with and without `media`, so the linked sqlite3 is the same. Before #998 no macOS runner executed any test.

**Test-harness caveat.** The `flows` integration harness runs the DS in-process and so links `libsql`'s sqlite3 beside SQLCipher, which wins the symbols there. Harness clients run on unencrypted databases, so a green flows run says nothing about at-rest encryption. `src-tauri/tests/flows/linked_sqlite.rs` asserts this, and fails if `libsql` ever leaves that binary. Shipped binaries contain one sqlite3.

### 7.1 What's local-only

- Decrypted message plaintext (`message.content`).
- MLS group state (`mls_kv`: epoch state, ratchet tree, leaf private keys, signature keypairs, KeyPackage private halves).
- Per-device MLS signing public keys (`mls_kv` scope `PollisDeviceSigPub`, one row per signature scheme).
- UI and preference caches.

### 7.2 Plaintext at rest outside the database (#1000)

SQLCipher covers the message store. #1000 closed five other places the app wrote plaintext:

1. **Pasted and dropped files never touch disk.** They arrive in the WebView as `File` objects with no path. The renderer used to write each to the OS temp directory as `pollis-<timestamp>-<original filename>` and never delete it. Bytes now cross IPC into an in-memory registry (`pollis-core/src/commands/staging.rs`); the renderer holds an opaque id, `upload_media_staged` reads and releases on success, and `lock` / `logout` / `wipe_local_data` release everything. Clipboard images come back as PNG bytes instead of a `pollis-paste-<nanos>.png` file.
2. **The loopback media server forbids caching.** `serve_media` sent decrypted media with no cache directives, which RFC 9111 §3 lets WebKitGTK, WKWebView and WebView2 store on disk. Every response, including 206 range responses, now carries `Cache-Control: no-store, no-cache, must-revalidate, max-age=0` and `Pragma: no-cache`.
3. **The media-cache wipe hits the right directory.** The cache is AES-256-GCM under the session `db_key`. `logout`, `set_pin` and `unlock` resolved the cache directory from ambient state that named the wrong user at that moment, so they emptied `media-cache/_anon/`. The wipe now takes an explicit `CacheScope`. `wipe_local_data` also clears the shell's `app_data_dir()/media-cache`, which differs from `db::local::dirs_path()` on Linux and Windows.
4. **Everything the app creates is owner-only on unix.** One helper (`pollis-core/src/private_fs.rs`) creates files 0600 and directories 0700, passing the mode to `open(2)`/`mkdir(2)` so a file is never briefly world-readable, and re-applies it through the handle to tighten files left by older builds. That covers `accounts.json`, `keystore.pks`, `overlay-guards.json`, the media cache and `pollis-tui.log`. SQLite opens the database and sidecars itself, so `open_at` tightens all three afterwards. "Save attachment as…" (`downloads.rs`) writes through the helper, and a source-scan guard bans `std::fs::write` in the export module, `downloads.rs` and `commands/r2.rs`. **Windows is a deliberate no-op:** files under `%APPDATA%` inherit the profile ACL (owner, SYSTEM, Administrators), and a hand-rolled DACL could only be worse.
5. **The terminal client's log masks the account email.** `pollis-tui` redirects stderr into `pollis-tui.log` (the OS temp dir when `POLLIS_DATA_DIR` is unset), and `commands/auth.rs` logged the email on `request_otp` and `verify_otp`. The log is now 0600 and those lines mask the local part (`pollis_core::util::mask_email`, same shape as `pollis-delivery/src/redact.rs`). This was not a general logging review.

None of this changes §1: a same-UID attacker is still out of scope. It protects against other local users on a shared machine, and against copies of temp directories or WebView caches that outlive the session.

### 7.3 What's deliberately not local

User profiles, group/channel metadata, membership and blocks live on Turso and are fetched through the DS at read time. A stolen device with the SQLCipher key therefore cannot enumerate the social graph: it holds no database credential (§8), and the DS answers only for the authenticated signer.

---

## 8. Remote Database Access (there is none from the client)

Source: `pollis-core/src/commands/ds_reads.rs`, `pollis-core/src/commands/mls/ds_reads.rs`, `pollis-delivery/src/{reads,directory,account_reads}.rs`.

**Since #987 the client cannot reach the database.** Neither `pollis-core` nor `pollis` (the installed binary's crate) links a database driver, and no database URL or token is compiled in. `pollis` has an optional `libsql` reachable only through its `test-harness` feature, which the flows harness uses for a process-local "remote" and no release build enables; `pollis-core/tests/no_client_side_remote_reads.rs` fails if a driver appears in either graph. Every remote read is a typed POST to the DS (`/v1/read/…`, `/v1/directory/…`, `/v1/mls/…` and a few others) on the same ML-DSA-44 device-signed transport as writes (`ds_client.rs`), and only the DS holds the database credential.

### 8.1 What this replaced, and why

The client used to open a libSQL connection with a read-only token baked into the binary and issue `SELECT`s directly (102 call sites in 34 files). Read-only, but not scoped: the token was whole-database, so a user bypassing the app's guards could `SELECT * FROM group_member` and read any group's roster (#917). #393 shortened the token's life (a DS-minted short-TTL token on unlock) but not its scope. #987 removed the connection, the token, and the minting endpoint (`POST /v1/turso/token`).

### 8.2 Properties of the read path

- **POST, never GET.** The signed message is `{METHOD}\n{PATH}\n{TIMESTAMP}\n{hex sha256(body)}`, and PATH excludes the query string, so query parameters on a GET would be unauthenticated. That was the #681 bug: an unauthenticated `GET /v1/commits` could wipe a commit log. Read parameters go in the signed JSON body.
- **The server decides what a caller may read,** using the same predicate as for writes (`pollis_schema::authz::GROUP_ROLE_SQL`, shared by both crates). With auth on, `user_id` and `device_id` come from the verified signature; body-supplied values are used only when auth is off.
- **The MLS control plane is one snapshot.** `POST /v1/mls/conversation-state` returns GroupInfo, the pending-Welcome flag, lineage heads, the commit batch and both membership gates from one read transaction, and refuses to serve a non-contiguous commit batch. Both are correctness requirements: a torn GroupInfo/Welcome pair strands a device permanently, and a gap in a batch makes the client delete its MLS state. Tests: `pollis-delivery/tests/conversation_snapshot.rs`.
- **Three reads have no device signature, because no device credential exists yet:**
  - `POST /v1/auth/account-probe` runs before unlock and answers `{ exists, has_identity, identity_version }` for a ULID the caller read from its own `accounts.json`. This rests on the id being unguessable and the file being non-sensitive (§1.1.1). It has its own per-IP `probe` rate-limit tier.
  - `POST /v1/read/enrollment` is gated on knowing the enrollment `request_id`. A session cannot gate it: the DS session TTL and enrollment TTL are both 600 s and the session is minted first, so a session-gated poll would always 401 before the request expired. The payload is sealed to an ephemeral X25519 key that the id does not confer.
  - `POST /v1/read/recovery-blob` requires the OTP session and records a security event per fetch.

### 8.3 What an attacker gets from a built binary

Nothing that reads the database; there is no token to extract. The strongest position left is a device signing key stolen from a compromised machine's keystore, which authenticates as that device: the DS answers only for that device's account, and MLS still protects plaintext.

Signal Desktop holds a per-account auth token issued at registration. Pollis holds a per-device signing key and no database credential.

---

## 9. Object Storage (Cloudflare R2)

Source: `pollis-core/src/commands/r2.rs`.

### 9.1 Convergent encryption (attachments)

- **Content hash:** SHA-256(plaintext), the dedup anchor and KDF input.
- **Key/nonce:** HKDF-SHA256 over the content hash, no salt; `info = b"pollis-att-key"` for the 32-byte AES-256-GCM key, `info = b"pollis-att-nonce"` for a 12-byte base nonce.
- **AEAD:** AES-256-GCM over 4 MiB chunks. Chunk nonce = base nonce XOR the little-endian u32 chunk index in the first 4 bytes, so large files stream without buffering and nonces are unique without state.
- **Object key:** `media/{content_hash}.enc`. The original filename is no longer part of the key (#762); objects written under the older `media/{hash}/{filename}.enc` form still resolve because each object's key is stored in `attachment_object.r2_key`. Same plaintext → same object, so cross-user dedup is automatic.

### 9.2 Visibility on R2

R2 sees ciphertext, the object key (which contains the content hash), the size and the upload time. It never sees the AEAD key.

This is the shape of MEGA's and Tresorit's convergent schemes. The accepted trade-off is **confirmation-of-file**: someone holding a candidate plaintext can hash it and check whether that object exists. Pollis accepts this for cross-user dedup. Per-conversation key wrapping would remove it at the cost of dedup.

### 9.3 R2 transport

**The client holds no R2 credentials.** It requests a short-lived presigned URL from the DS (`POST /v1/r2/presign`, `r2.rs::presign_r2`) and then does a plain HTTPS `PUT`/`GET`/`DELETE`. SigV4 signing happens in the DS, which holds `R2_ACCESS_KEY_ID` / `R2_SECRET_ACCESS_KEY` (`pollis-delivery/src/broker.rs`). Only `R2_S3_ENDPOINT` and `R2_PUBLIC_URL`, both public, are compiled into the client.

A presigned URL authorises one operation on one key for a short window, so a binary yields no ability to list, overwrite or delete the bucket. For `emoji/…` uploads the DS signs `content-length` into the URL, so R2 itself rejects a body of any other size (#848).

`upload_media` reads files from disk by path inside `pollis-core`, so large attachments do not cross the IPC boundary. Bytes with no path (paste, drop) go through `upload_media_staged` (§7.2 item 1).

### 9.4 Avatars and group icons

These use `upload_file` / `download_file` and are **not** encrypted. Anyone with the R2 URL can fetch them. This is intentional: anyone who can see the user or group already sees the avatar. Worth flagging: whoever obtains a `users.avatar_url` or `groups.icon_url` can fetch the image without authentication. Dedup does not apply on this path.

---

## 10. Real-Time Media (LiveKit)

Source: `pollis-core/src/commands/livekit/`, `voice/`, `voice_e2ee.rs`, `voice_key_ring.rs`.

### 10.1 Authentication

LiveKit uses room-scoped HS256 JWTs **minted by the DS** (`pollis-delivery/src/broker.rs`): 15-minute participant tokens (`LIVEKIT_TOKEN_TTL_SECS`) and 5-minute admin tokens for `RoomService`.

- The client holds no LiveKit credential. `LIVEKIT_API_KEY` / `LIVEKIT_API_SECRET` are DS environment; only `LIVEKIT_URL` is compiled in. The client calls `POST /v1/livekit/token` (`mls/ds_client.rs::ds_livekit_token`).
- The token's identity is derived server-side from the verified request signature, so a client cannot mint a token as another user or device, and a reverse-engineered binary cannot mint one at all. The DS decides which room a caller may join and maps it to the room pseudonym (§1.2).
- Server-side sends (inbox nudges, notifications to rooms the caller has not joined) go through `POST /v1/livekit/send-data`; the DS restricts which event kinds a client may ask it to publish (`broker.rs`).

### 10.2 Voice frame-level E2EE

LiveKit is an SFU. DTLS-SRTP (RFC 5763/5764) protects each peer-to-SFU hop but terminates at the SFU, so in a stock deployment the SFU hears plaintext audio, as with Slack Huddles, Teams and Meet.

Pollis adds per-frame encryption after Opus and before SRTP: **AES-128-GCM** via libwebrtc's native `FrameCryptor` (the machinery behind `livekit-client`'s `setupE2EE`). `voice_e2ee::build_e2ee_options` builds `E2eeOptions { encryption_type: EncryptionType::Gcm, key_provider }`, which `voice/lifecycle.rs::join_voice_channel` passes into `Room::connect`. RTP headers stay readable for routing; payloads are ciphertext.

**Key derivation** (`voice_e2ee.rs::derive_voice_key`):

```text
voice_key = MlsGroup::export_secret(
    label = "pollis/voice/v1",
    context = epoch.to_be_bytes(),
    length = 32,
)
```

The group is the one protecting the channel's text (group channels share their group's MLS group; DMs use the conversation id). Every current member holds the exporter secret and derives the same key without the server; non-members and the SFU cannot. LiveKit's key provider then PBKDF2-derives the 128-bit frame key from it.

**Key ring.** `FrameCryptor` keeps a 16-slot ring per participant. A sender encrypts with the slot its `key_index` names and writes that index into the frame trailer; a receiver decrypts with the slot the trailer names. In the SDK, `KeyProvider::set_shared_key(key, idx)` only stores a key (and rejects `idx >= 16`), and every cryptor starts on slot 0 and never advances on its own. Storing a new key therefore does not change what senders use.

**Rotation.** When the MLS epoch advances, `process_pending_commits_inner` calls `voice_e2ee::on_mls_epoch_changed`, which:
1. re-derives the key for the new epoch;
2. installs it in slot `voice_key_index(epoch) = epoch mod 16` (`voice_key_ring.rs`) and reads the slot back to confirm;
3. calls `FrameCryptor::set_key_index(slot)` on every local sender cryptor in the room.

Receivers need nothing: the trailer carries the slot. Later local publications (screen share, camera) are re-pointed on `RoomEvent::LocalTrackPublished`, and the join path installs the join-epoch key in its own slot. No reconnect is needed. The previous slot keeps its key for a 10-second grace period (`VOICE_KEY_SLOT_GRACE_SECS`) for in-flight frames and lagging peers, then is overwritten with random bytes so a member removed at that epoch cannot keep injecting frames under it. Removed members lose decryption because they lack the new exporter secret; added members gain it from the new epoch.

**Residual.** Epochs exactly 16 apart share a slot, so 16 commits inside one grace window would retire the older key early. The cost is a few dropped frames from a lagging peer, never plaintext. The slot arithmetic is unit-tested, and `src-tauri/tests/flows/voice.rs` asserts that a rotation installs the key in the slot senders point at and keeps the previous slot populated through the grace window.

**Defaults.** `KeyProviderOptions::default()` with `key_ring_size = VOICE_KEY_RING_SIZE = 16`: PBKDF2, `LKFrameEncryptionKey` salt, ratchet window 16. These match `livekit-client` JS so a future web peer deriving from the same MLS group can interoperate.

**No opt-out.** Voice E2EE is unconditional. Its cost is microseconds per frame, and an optional mode invites users to misjudge their threat model.

Mobile is LiveKit data-only by product decision; voice, video and screenshare are desktop features.

### 10.3 Audio pipeline (defensive context)

`cpal` capture in 10 ms i16 mono frames → optional RNNoise (`nnnoiseless`) → WebRTC AudioProcessing (AGC2, NS, HPF, AEC; `webrtc-audio-processing`) → LiveKit `NativeAudioSource.capture_frame` → SRTP. The whole pipeline runs in the Rust core; audio never enters the renderer.

### 10.4 Signalling channel

LiveKit data packets carry application events: `new_message` (a wake-up; the ciphertext comes from the DS), `membership_changed`, typing and voice presence, and `enrollment_requested`. The last is published by the DS and carries the verification code in cleartext. That is acceptable because the code is not secret and authenticates nothing on its own; it is the human comparison channel for §5.1. LiveKit operators see all of these events. They do not see message ciphertext, MLS state or private keys.

---

## 11. Rate Limiting, Block Enforcement, Abuse Surfaces

### 11.1 OTP request rate limiting

The DS throttles at two scopes:

- **Per mailbox** (`pollis-delivery/src/otp.rs`): 30 s resend throttle, 3 codes per window, and a 15-minute mailbox lockout after 5 wrong codes (§4).
- **Per client IP** (`pollis-delivery/src/ratelimit.rs`): fixed windows over the unauthenticated OTP endpoints, keyed on `CF-Connecting-IP`. This covers a client spraying requests across many addresses to email-bomb mailboxes or burn Resend quota, which the per-mailbox limit cannot.
- Resend's own reputation and per-key limits sit underneath both.

### 11.2 PIN attempt rate limiting

Local, per user: 10 attempts, then the wrapped keys are deleted. No backoff. See §3.3.

### 11.3 Enrollment verification code

8 Crockford base32 characters (40 bits), single-use within the request's 10-minute TTL, compared in constant time. It is derived from the new device's ephemeral public key (#793), salted with a length-prefixed `request_id ‖ user_id ‖ created_at` transcript (2026-09-18 review), and the approver compares against its own re-derivation (#1096). Details and attack cost: §5.1.

### 11.4 Block enforcement

`user_block` is directional (A blocking B does not mean B blocks A); enforcement checks both directions, at DM creation (re-checked by the DS) and at send (`messages/send.rs`, `blocks::any_blocked_either_way`).

DM blocks are deliberately asymmetric in what each side can observe:
- The **blocker** no longer sees the conversation; the DS's DM listing hides channels whose other member the caller has blocked.
- The **blockee** still sees it. Sending appears to succeed and the message is stored in their local `message` table, but it is not encrypted, not posted to the DS, and not announced on LiveKit. The blocker never receives it, and the blockee gets no signal that they are blocked.

This matches Signal and iMessage: a block is not a channel through which the blocker reveals anything.

Group-channel blocks are render-side only. The blocker's client hides blocked senders; their messages are still encrypted, stored and announced normally. MLS is unaware of blocks.

### 11.5 Identity reset (destructive)

`reset_identity_and_recover` (`device_enrollment.rs`) is the destructive recovery path. It requires:

- a verified OTP for `users.email`;
- a constant-time match between the typed `confirm_email` and `users.email`.

It then:

1. Generates a fresh ML-DSA-44 account identity, bumps `users.identity_version`, and replaces the `account_recovery` blob (`POST /v1/account/rotate-identity`, CAS-guarded on `account_key_log`).
2. Removes the user from every `group_member`, `dm_channel_member`, `mls_key_package`, `conversation_watermark` and `mls_welcome` row and orphans their other devices. If the user was sole admin it promotes a new admin and hands over `groups.owner_id`. Groups and DMs left empty are torn down table by table, because production Turso runs `foreign_keys=OFF` and `ON DELETE CASCADE` does not fire (`pollis-delivery/src/teardown.rs`). The `users` row survives: this is a reset, not a deletion.
3. Wipes the local SQLCipher DB and its WAL/SHM.
4. The DS writes a `security_event` of kind `identity_rotated` inside the rotation transaction, naming the authorising credential (`credential=session` here, `credential=signature` for a rotation from an enrolled device). The client cannot suppress it.

**With the OTP session as credential, steps 1 and 2 are one server-side transaction.** A pre-enrollment device has no signing key, so `rotate-identity` accepts a verified-OTP session, and the DS treats that credential as meaning a full reset: `apply_rotate_identity` (`pollis-delivery/src/account.rs`) runs the whole wipe (memberships, key packages, every device except the one the session was minted for, taken from the session record) in the same transaction as the rotation and the audit row. No server state exists in which a session-minted key sits on an account that still has its memberships and devices. The client's follow-up `reset-recover` and `welcomes/purge` calls are idempotent no-ops on this path. A device-signed `rotate-identity` is a plain rotation, and there the client's `reset-recover` does the wipe.

Every other device the user enrolled is cryptographically orphaned: its account private key no longer matches `account_id_pub`, so its `device_cert` no longer verifies and its leaves are not admitted to new commits. This requires only the user's email and a working OTP, which is the intended soft-recovery UX.

**Audit-relevant property:** an attacker who controls only the user's email can do this. Defences: the server-authored `identity_rotated` row in the user's Security page, and the attack's visibility (the attacker's new identity owns no conversations, and the user's other devices are locked out on next use). What email compromise cannot do is inherit the account: the session path can never produce a rotated key that keeps the victim's memberships or leaves the victim's devices enrolled. Tests: `pollis-delivery/tests/reset_session.rs`.

---

## 12. Key Material Summary

| Material | Algorithm | Where it lives | Where it does not live |
|---|---|---|---|
| Account identity private | ML-DSA-44 seed (32 B) | Keystore `account_id_key_wrapped_{uid}` under the PIN KEK (and, on the file backend, a second AES-256-GCM layer under a machine-bound KEK, §3.5); `AppState.unlock` (zeroizing) | Unwrapped on disk; any server as plaintext |
| Account identity public | ML-DSA-44 (1312 B) | `users.account_id_pub`; `account_key_log`; local `mls_kv` indirectly | — |
| Secret Key (recovery) | 150-bit Crockford base32 | The user's offline copy | Any Pollis system |
| Recovery wrap key | HKDF-SHA256 → 32 B | Derived on demand from Secret Key + per-user salt | Stored anywhere |
| Device MLS signing private (one per scheme) | ML-DSA-44 seed (32 B), used for `CS_PQ` leaves and DS request auth; Ed25519 (32 B), still minted so older code points stay readable | Local `mls_kv` (SQLCipher), keyed by scheme | Off-device |
| Device MLS signing public (one per scheme) | Ed25519 (32 B) / ML-DSA-44 (1312 B) | `user_device.mls_signature_pub` / `mls_signature_pub_pq`; local `mls_kv`; both bound by one v2 `device_cert` | — |
| Device cert | ML-DSA-44 signature (2420 B) by the account key over both device publics | `user_device.device_cert` | — |
| MLS leaf / commit / Welcome material | TreeKEM, RFC 9420 | Local `mls_kv` (SQLCipher) | — |
| MLS HPKE init private | X-Wing (X25519 + ML-KEM-768) decapsulation key | Local `mls_kv` (SQLCipher) | Off-device |
| Published KeyPackages | Public halves, `CS_PQ` | `mls_key_package`, suite-tagged, claimed once | Any private half |
| MLS application secrets | RFC 9420 | Ephemeral, per epoch | Persisted past their epoch |
| SQLCipher DB key | 32 random bytes | Keystore `db_key_wrapped_{uid}` under the PIN KEK (§3.5); `AppState.unlock` | Unwrapped on disk |
| PIN | 4 ASCII digits | The user's head | Disk or wire |
| PIN KEK | Argon2id → 32 B | Derived at unwrap time | Stored anywhere |
| OTP | 6 digits | DS memory, as a salted hash; 10-min TTL; attempt-capped | Disk; the client |
| Enrollment ephemeral private | X25519 (32 B) | `AppState.enrollment_ephemeral_keys` (memory) | Disk, server |
| Attachment key | HKDF-SHA256 over content hash → 32 B | Derived on demand | Persisted; sent to R2 |
| Voice frame key | MLS exporter (32 B) → PBKDF2 → AES-128-GCM | LiveKit key provider ring, in memory | Off-device; the SFU |
| Database credentials (`TURSO_TOKEN`, `LOG_DB_TOKEN`) | bearer | DS environment only (§8) | The client binary |
| `R2_ACCESS_KEY_ID` / `R2_SECRET_ACCESS_KEY` | SigV4 | DS environment only | The client binary |
| `LIVEKIT_API_KEY` / `LIVEKIT_API_SECRET` | JWT signing | DS environment only | The client binary |
| `RESEND_API_KEY`, `EXPO_TOKEN` | bearer | DS environment only | The client binary |
| Transparency-log signing key | ML-DSA-44 seed | GitHub Actions secret `STH_SIGNING_KEY` (§6.9 limit 5) | The client; the DS |

---

## 13. Known Gaps and Audit Focus Recommendations

Ordered by adversary cost, cheapest first. Item numbers are stable; closed items are kept so references stay valid.

1. **Voice E2EE has no end-to-end decryption test.** §10.2. Key derivation, rotation and ring state are covered by unit tests and `flows/voice.rs`; a manually triggered two-client e2e (`e2e-two-client-voice-channel.yml`) runs a real LiveKit SFU but asserts only the participant roster. An adversarial review found the rotation used to store the new key in a slot no sender used (and from epoch 16 in no slot at all), so a member removed mid-call kept decrypting until everyone left. That is fixed, but nothing yet runs real LiveKit with two clients and asserts that a removed member's `FrameCryptor` reports `MissingKey`/`DecryptionFailed` after removal. Build that before a third-party audit.
2. **A rogue leaf is evicted, not refused; the window is one honest reconcile.** §5.3, §6.4. Conforming committers refuse uncertified KeyPackages (closing server substitution at claim time), and every replaying member verifies added leaves from the commit's own proposals. A non-conforming committer (malicious member or pre-fix client) can still land a commit adding an uncertified leaf; honest members merge it, record `uncertified_mls_leaf`, and evict it. Until the eviction lands, the rogue leaf holds the key schedule. Closing this needs a quarantine-and-resync state machine. Mitigations: TOFU pins (`batch_check_and_pin_account_keys`, refs #277) run before adds and anchor the leaf check; the account-key log (#330, §6.9) makes key swaps publicly auditable (limits in item 10).
3. **Closed (#987): shared database token in the binary.** The client holds no database credential (§8).
4. **Closed: no server-side OTP rate limiting.** The DS throttles per mailbox and per IP (§11.1). The counters are in memory; see §4 for why a container restart does not widen the guess budget.
5. **Avatars and group icons are public R2 objects.** §9.4. Anyone who obtains an `avatar_url` / `icon_url` can fetch the image without authentication.
6. **PCS healing is launch-driven, not timer-driven.** §6.7. Self-updates fire on join and from the launch sweep (7 days + up to 2 days jitter, at most 3 groups per sweep). A group whose members all stay offline does not rotate, and a device back from a long absence needs several launches to work through many groups.
7. **No Megolm-style key backup, by design.** §6.8. New devices and pre-join history are not recoverable. Do not report this as a gap unless your requirements demand backup; the product principle (`CLAUDE.md`) accepts it.
8. **Soft recovery needs only OTP + email match.** §11.5. Email compromise allows destroying the user's identity, including the Secret-Key recovery blob, but not inheriting it: the session-authenticated rotation is atomically a full membership/device wipe. It is recorded as a DS-authored `identity_rotated` event. The destruction itself is not preventable with the current factors.
9. **Closed: OTP comparison.** The DS compares salted SHA-256 hashes in constant time (§4).
10. **Account-key transparency has residual limits.** §6.9. (a) Daily publish lag: between a swap and the next publish, only live TOFU covers it. (b) Client audits alert but do not block. (c) No VRF: the user set and rotation cadence are enumerable. (d) One first-party log, signed in GitHub Actions, so CI is in the TCB; `pollis-verify` lets anyone run an independent auditor. (e) #732's rotation had no overlap window, so it was indistinguishable from equivocation to anyone holding cached heads; clients now pin a key set with expiries (#740) and an offline root (#754) for future rotations. None of these reopen the swap attack; they bound how fast and by whom it is detected.
11. **Sealed sender is at-rest only; the live DS still sees the sender.** §1.2. Sealed sender, size padding and signalling minimization are shipped. Every send still authenticates with the sender's device signature, so the DS learns the sender in real time. Anonymous membership proofs (v1.5), per-conversation membership pseudonyms (v3) and timing batching (v4) are not built. Conversation existence, member counts and the `user_id`-keyed graph remain visible to the DS and Turso. IP hiding exists only as the opt-in relay overlay, off by default. Post-quantum confidentiality (#454) and authentication (#668) are claimed with the scope in §6.1 and §6.10: forward-only from each group's migration.

---

## 14. References

**Core standards**
- RFC 9420: *The Messaging Layer Security (MLS) Protocol*. Barnes et al., 2023.
- RFC 9180: *Hybrid Public Key Encryption (HPKE)*. Barnes, Bhargavan, Lipp, Wood, 2022.
- RFC 9106: *Argon2 Memory-Hard Function for Password Hashing and Proof-of-Work Applications*. Biryukov, Dinu, Khovratovich, Josefsson, 2021.
- RFC 8032: *Edwards-Curve Digital Signature Algorithm (EdDSA)*. Josefsson, Liusvaara, 2017.
- NIST FIPS 204: *Module-Lattice-Based Digital Signature Standard (ML-DSA)*, 2024. Account identity keys, device certs, DS request auth, transparency-log tree heads, MLS leaves (§6.1).
- NIST FIPS 203: *Module-Lattice-Based Key-Encapsulation Mechanism Standard (ML-KEM)*, 2024. The post-quantum half of X-Wing (§6.1).
- RFC 7748: *Elliptic Curves for Security* (X25519). Langley, Hamburg, Turner, 2016.
- RFC 5869: *HMAC-based Extract-and-Expand Key Derivation Function (HKDF)*. Krawczyk, Eronen, 2010.
- RFC 8439: *ChaCha20 and Poly1305 for IETF Protocols*. Nir, Langley, 2018.
- `draft-irtf-cfrg-xchacha`: *XChaCha: eXtended-nonce ChaCha and AEAD_XChaCha20_Poly1305*. Arciszewski.
- NIST SP 800-38D: *Galois/Counter Mode (GCM) and GMAC*. Dworkin, 2007.
- RFC 6962 / RFC 9162: Certificate Transparency (Merkle tree, STH and proof formats used in §6.9).
- RFC 5763 / RFC 5764: DTLS-SRTP. Rescorla, McGrew, 2010.
- AWS SigV4, used by the DS presigner (§9.3), not the client. https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_aws-signing.html

**Implementations relied upon**
- OpenMLS (https://github.com/openmls/openmls), with the RustCrypto provider.
- SQLCipher (https://www.zetetic.net/sqlcipher/): AES-256-CBC + HMAC-SHA512, page-level.
- LiveKit (https://livekit.io/): WebRTC SFU. The Rust `livekit` crate's `FrameCryptor`, keyed from the MLS exporter (§10.2).
- `keyring` (https://crates.io/crates/keyring): macOS Keychain, Secret Service, Windows Credential Manager.
- Cloudflare R2 (https://developers.cloudflare.com/r2/): S3-compatible object storage.

**Comparable products**
- **Signal / WhatsApp / Messenger Secret Conversations:** Signal Protocol (X3DH + Double Ratchet), pairwise sessions, Sender Keys for groups. Pollis uses MLS for better asymptotic group cost and continuous group authentication. It has Signal-style 60-digit safety numbers with TOFU pinning (§5.4) on top of per-device cross-signing (§5.3), backed by the account-key transparency log (§6.9). Cross-signing covers server-injected devices; safety numbers, pinning and the log cover a swapped account key.
- **Wire / Element X / Webex:** also MLS. Their public deployments use the RFC 9420 mandatory suite `0x0001`, which Pollis ran until #669; Pollis now runs the PQ suite `0x0052` (§6.1).
- **Matrix / Element (legacy):** Megolm + Olm, with server-side key backup, which Pollis deliberately omits.
- **Slack / Microsoft Teams:** TLS in transit, server-side at-rest encryption, no E2EE for messages or media. Their operators can read content; Pollis's cannot.
- **Discord:** no E2EE for messages. The DAVE protocol (MLS key agreement, SFrame media encryption) provides E2EE for audio and video in DMs, group DMs, voice channels and Go Live since September 2024. Pollis's voice design (§10.2) has the same shape; its messages are also E2EE.
- **iMessage:** pairwise E2EE per device with per-user fan-out at send time. iCloud Messages backup is held under Apple's keys unless the user enables Advanced Data Protection (iOS 16.2+, December 2022). Pollis uses MLS group state and has no backup of any kind.
- **1Password:** Secret Key + master password, with PBKDF2-HMAC-SHA256 (650k iterations as of 2023) and the Secret Key mixed into the KDF. Pollis pairs a high-entropy Secret Key with a low-entropy local factor too, but uses Argon2id for the PIN, and its Secret Key wraps the account identity key on the server (HKDF-SHA256 + AES-256-GCM) rather than feeding the PIN KDF. Pollis splits device unlock (PIN) from server-side recovery (Secret Key).
