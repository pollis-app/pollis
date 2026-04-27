# Pollis Security Whitepaper

**Audience:** independent security auditors evaluating the cryptographic protocol design and surrounding flows.
**Scope:** the desktop application in this repository, its remote services (Turso, Cloudflare R2, LiveKit, Resend), and the trust boundaries between them. Web-app concerns (XSS, CSP, SOP) are out of scope; this document covers cryptographic protocol design, key custody, identity, group membership, and the data-flow paths that move plaintext or key material across trust boundaries.
**Status:** authoritative. The legacy `ARCHITECTURE.md` file at the repo root predates the migration from Signal Protocol to MLS and should not be used as a reference. Where `ARCHITECTURE.md` and this document disagree, this document wins. The wiki under `.codesight/wiki/` is also authoritative for implementation specifics.

---

## 1. Trust Model

### 1.1 Boundaries

| Trusted | Untrusted |
|---|---|
| The user's device | Network (any path between the device and any remote service) |
| The OS keystore (Keychain / Secret Service / Credential Manager) | Turso (libSQL) — the remote relational database |
| The Tauri 2 application binary at the version the user installed | Cloudflare R2 — object storage for attachments |
| The local SQLCipher database file | LiveKit — SFU and signalling for voice and realtime events |
| The user-held Secret Key (printed once, expected to be stored offline) | Resend — outbound email transit for OTPs |
| The user-held PIN (in the user's head) | Anyone with read access to a copy of `accounts.json` or the keystore who does not also have the PIN |

The application is built and shipped by the operators of the Pollis services. The trust delegation is the same as Signal Desktop or WhatsApp Desktop: the binary is trusted at install time, after which the cryptographic protocol is what defends against the *server* side of the same operator. Reproducible builds are not currently a goal; binary integrity rests on platform code-signing (Apple notarization on macOS, Azure Artifact Signing on Windows — see `.codesight/wiki/windows-signing.md`).

### 1.2 What the server can and cannot see

Turso is the canonical store of *metadata*. It can observe, in plaintext: user records (id, email, username, avatar URL), social graph (group membership, DM channel membership, blocks), conversation metadata (sender id, timestamp, ciphertext size, MLS commit and welcome timing), key-package availability, device registration (cert blobs and `mls_signature_pub`), security events (`security_event`), and connection patterns (IP address, libSQL Hrana streams).

Turso cannot recover, by design: any message plaintext, any private key (the `account_id_key` is only present on the server in the form of a `account_recovery` blob whose key derivation input — the user's Secret Key — is never sent to the server), MLS group state, MLS application secrets, or attachment plaintext (R2 attachments are convergent-encrypted by the device before upload).

LiveKit can see: real-time data-channel events (`new_message`, `membership_changed`, `enrollment_requested`, voice presence) — these payloads are JSON, not encrypted at the application layer; they are signalling, not message content. LiveKit also handles voice plaintext at the SFU (see §10).

R2 can see: opaque AEAD ciphertext at deterministic content-hash-derived keys. The plaintext, content-hash, and AEAD key are never on-wire to R2.

Resend sees: an email address and a 6-digit OTP, in plaintext, for the duration of the email-delivery transaction.

---

## 2. Identity Layers

Pollis carries three nested identities. Distinguishing them is essential for the rest of the document.

### 2.1 Account identity (per user)

A long-lived Ed25519 keypair (RFC 8032), generated on the device that completes signup. Source: `src-tauri/src/commands/account_identity.rs::generate_account_identity`. The public half is published to `users.account_id_pub` (BLOB, 32 bytes) at signup. The private half exists in exactly two places:

1. On the user's enrolled devices, on disk only as ciphertext in the OS keystore slot `account_id_key_wrapped_{user_id}` (see §3 for wrapping).
2. On the server, on disk only as ciphertext in the `account_recovery` table, wrapped under a key derived from a user-held *Secret Key* the server has never seen.

When `users.account_id_pub` rotates (`reset_identity`), `users.identity_version` increments. Every device whose locally-held private key does not derive a public key matching the current `account_id_pub` is treated as orphaned and wiped on next sign-in (`auth.rs:213-228`, `account_identity.rs::has_matching_local_account_identity`).

### 2.2 Device identity (per device per user)

Each device gets a stable ULID `device_id` on first sign-in (`auth.rs::register_device`), persisted in the OS keystore at `device_id_{user_id}`. The device also generates a stable per-device MLS signing keypair (Ed25519, picked because it matches the MLS ciphersuite — see §6); the public half is stored both in `mls_kv` locally and in `user_device.mls_signature_pub` remotely.

Each device's MLS signing public key is *cross-signed* by the user's account identity key. This produces a `device_cert`: an Ed25519 signature over a domain-separated, length-prefixed payload binding `device_id`, `mls_signature_pub`, the `identity_version` at issuance, and the issuance timestamp (`account_identity.rs::device_cert_signed_payload`, domain separator `pollis-device-cert-v1\0`). Cross-signing is what lets every other client decide whether to admit a particular leaf into an MLS group.

### 2.3 MLS leaf identity (per device per group)

Each device's stable MLS signing keypair populates a `BasicCredential` (RFC 9420 §5.3) whose serialised content is the UTF-8 string `{user_id}:{device_id}` (`mls.rs::make_credential`). One credential per device covers every KeyPackage and every leaf node that device produces in any MLS group, so a single `device_cert` is sufficient cross-signing for the device's entire MLS surface.

---

## 3. PIN-Wrapped Key Storage

The local PIN is a *device-local unlock* factor, not a server credential. It does not travel; the server has no record of it.

### 3.1 KDF and AEAD choices

Source: `src-tauri/src/commands/pin.rs`.

- **PIN format:** 4 ASCII digits — `validate_pin`. ~13 bits of entropy.
- **KDF:** Argon2id (RFC 9106), Argon2 crate `0.5`, version 0x13. Parameters: `m_cost = 64 MiB`, `t_cost = 3`, `p_cost = 1`, output 32 bytes. Tuned to ~250 ms on a mid-range Apple-silicon or Ryzen 5 device, deliberately above the OWASP 2024 first-choice password-storage minimum (m=19 MiB, t=2). Parameters are stored inside the `pin_meta_{user_id}` blob, not hard-coded at unwrap time, so they can be bumped on any future re-wrap without a migration.
- **Salt:** 16 bytes, `OsRng::fill_bytes` (rand 0.8). Per-user, per re-wrap.
- **AEAD:** XChaCha20-Poly1305 (Mehegan / Nir, IRTF CFRG draft, `chacha20poly1305` crate `0.10`) with 24-byte random nonces. Chosen over AES-256-GCM specifically because the 24-byte nonce eliminates nonce-reuse risk across the small number of wrap events (initial set, change-PIN, lockout-recovery).

### 3.2 Wrapped material

Three slots are written under the PIN-derived KEK:

- `pin_meta_{user_id}` (verifier blob): a fixed plaintext `b"pollis-pin-ok\0\0\0"` AEAD-encrypted under the KEK. Letting unlock reject a wrong PIN by AEAD failure on this 16-byte plaintext, without unwrapping the two larger blobs, costs one Argon2 evaluation rather than three.
- `db_key_wrapped_{user_id}`: 32 random bytes, the SQLCipher key for `pollis_{user_id}.db`.
- `account_id_key_wrapped_{user_id}`: the 32-byte Ed25519 private of §2.1.

The `pin_meta` blob also carries `failed_attempts` (u32, big-endian) and `last_attempt_unix` (u64, BE), outside the AEAD. They are not secret — the threat model is a local attacker who already has keystore read access and can count attempts independently.

### 3.3 Lockout

`MAX_FAILED_ATTEMPTS = 10`. On the 10th wrong attempt, all three keystore slots and the local SQLCipher file (and its WAL/SHM siblings) are deleted (`pin.rs::nuke_wrapped`, `device_enrollment.rs::reset_identity_and_recover`). The Turso-side account is untouched. The device is now in the same state as a brand-new device: the user must re-enrol via Secret Key recovery (§5.2) or another device's approval (§5.1).

There is no time-based backoff. The Argon2id ~250 ms-per-attempt cost combined with a 10-attempt ceiling is the offline-brute-force defence; for online (UI-driven) attempts the same ceiling is the rate limit.

### 3.4 Key custody at rest

After PIN setup, raw `db_key` and raw `account_id_key` exist on disk only inside AEAD ciphertext. In-process they live in `Zeroizing<Vec<u8>>` containers (`AppState.unlock`) which scrub on drop (`zeroize` crate). `lock()` drops the unlock state and closes the SQLCipher handle, returning the device to a "needs PIN" state without forcing a full sign-out.

### 3.5 Comparable systems

- **Signal Desktop** uses an OS-keystore-stored randomly generated key to encrypt its local SQLCipher store, with no user PIN. Pollis adds the PIN factor; the consequence is that an attacker who clones the keystore but not the PIN cannot decrypt local data, at the cost of requiring the user to enter a PIN to unlock. This is closer to iOS message-cache encryption (PIN/biometric) than to Signal Desktop.
- **1Password / Bitwarden** use Argon2id with comparable parameters as their master-password KDF; the difference is that they have a high-entropy master password to begin with, while Pollis has a 4-digit PIN. The 10-attempt nuke-and-recover policy is what closes that gap.
- **WhatsApp Desktop** retains a database key on disk without a user-supplied factor — equivalent to Pollis' pre-PIN behaviour, kept only as a migration path.

---

## 4. Authentication Flow (OTP)

Source: `src-tauri/src/commands/auth.rs::request_otp`, `verify_otp`.

The OTP factor exists only to prove control of an email address. It is *not* the device unlock factor (that's the PIN) and it is *not* the account-recovery factor (that's the Secret Key).

- 6-digit numeric, generated via `rand::thread_rng` with `gen_range(0..1_000_000u32)` and zero-padded.
- Stored in-memory on the Pollis Rust process as `SHA-256(otp)` (not the plaintext) inside `state.otp_store: HashMap<email, OtpEntry>`. TTL: 10 minutes. Entry is removed on first successful verification (single-shot).
- Email transit: HTTPS POST to Resend's `api.resend.com`, with the bearer `RESEND_API_KEY` baked into the binary's environment.
- Comparison on `verify_otp` uses string equality on hex-encoded SHA-256 digests, after trimming user input. Constant-time comparison is not used at this site; the secret being compared is a 6-digit code with 20-bit entropy and is wiped on first successful match, so timing analysis is not a meaningful attack surface, but a future hardening pass could swap to `subtle::ConstantTimeEq`.
- **No application-layer rate limit on `request_otp`.** Rate limiting at email send time is the responsibility of Resend (provider-level) and DNS-level reputation. This is a known gap relative to Signal's own SMS quotas; mitigations are noted in §13.

OTP is consumed in two scenarios:
1. First-time signup (user has no `users` row). `verify_otp` creates the row, calls `generate_account_identity` to mint the account-identity Ed25519 keypair and a Secret Key, and seeds `AppState.unlock` with the freshly-generated material. The frontend then transitions to the PIN-create screen, which is what causes that material to be persisted to disk (as ciphertext under the PIN-derived KEK).
2. Soft-recovery (`reset_identity_and_recover`). This *requires* both the OTP and a constant-time match of the user-typed email against `users.email` (constant-time via the local helper `constant_time_eq`).

Returning users on a previously-enrolled device do *not* go through OTP. They go through PIN entry against `pin_meta_{user_id}`. This is by deliberate design: in pre-PIN versions, transient OS-keystore read failures (macOS keychain hiccups, Linux secret-service races) caused returning users to be bounced back to the OTP screen on every cold start. The PIN gate replaced that. See `pin-design.md` for the full rationale and `accounts.json`'s atomic write / loud-parse-failure protocol that was added in the same change.

---

## 5. Multi-Device Enrollment

A user with an existing `account_id_pub` can add a second device through one of two paths. Both end with the same outcome: the new device holds a copy of the account-identity private key, has published a `device_cert`, has published `KeyPackage`s, and has joined every existing MLS group via external commit.

### 5.1 Approval path (in-band, sibling-device-mediated)

Source: `src-tauri/src/commands/device_enrollment.rs`.

1. New device generates an **ephemeral X25519 keypair** (`x25519-dalek` 2.0, `StaticSecret` from `OsRng` bytes). The private half is held in `AppState.enrollment_ephemeral_keys: HashMap<request_id, Vec<u8>>` — *in memory only*. App restart mid-enrollment forfeits the request.
2. New device generates a 6-digit verification code (`OsRng` → `u32 mod 1_000_000`, zero-padded).
3. The request row is inserted into `device_enrollment_request` (Turso), carrying the new device's ephemeral *public* X25519 key, the verification code, status `pending`, a 10-minute TTL.
4. New device fans out a notification to LiveKit room `inbox-{user_id}` so any online sibling device sees the request immediately.
5. The sibling device fetches the request, the user confirms the code matches between screens, and the sibling calls `approve_device_enrollment(request_id, verification_code)`. The verification code is compared with `subtle::constant_time_eq` (local helper).
6. The sibling generates a **second** ephemeral X25519 keypair, computes ECDH(approver_priv, requester_pub), and feeds the 32-byte shared secret to **HKDF-SHA256** (RFC 5869) with `info = b"pollis-enrollment-wrap-v1"` and no salt to derive a 32-byte wrap key. AES-256-GCM (12-byte random nonce) wraps the account-identity Ed25519 private key. The on-wire blob is a fixed-layout `approver_pub || nonce || ciphertext+tag` (92 bytes total). The approver writes this blob to `device_enrollment_request.wrapped_account_key` and flips the status to `approved`. A `security_event` row of kind `device_enrolled` (metadata `via=approval,approver={device_id}`) is inserted.
7. The new device's `poll_enrollment_status` sees `approved`, recovers the ephemeral private from in-memory state, and unwraps. The unwrapped 32 bytes plus a freshly generated `db_key` populate `AppState.unlock`. The frontend transitions to PIN-create; `set_pin` writes the wrapped slots.
8. `finalize_device_enrollment` runs: the new device publishes its own `device_cert`, writes 5 fresh `KeyPackage`s to `mls_key_package`, and for each existing group / DM the user belongs to, fetches the latest `mls_group_info` and joins via MLS external commit (§6.4).

This is a one-shot ECDH-then-AEAD scheme analogous to a sealed-sender envelope. It is **not** an authenticated key exchange — there is no signature on the approver's ephemeral public from the long-term account identity key. The replacement for AKE authentication is the user-confirmed 6-digit verification code shown on both screens at the same time. An attacker who can read but not write Turso cannot forge an approval; an attacker who can write Turso can race a forged request, which is detected at the human channel (the user sees a code they did not initiate). The 10-minute TTL bounds exposure if the code is observed but not used.

This is broadly comparable to Signal's "PIN-based reregistration" flow combined with its "approval QR code" linked-device flow, with the simplifying property that Pollis runs on desktop only — there is no QR code; the user just types the displayed digits.

### 5.2 Secret Key recovery path (out-of-band)

Source: `device_enrollment.rs::recover_with_secret_key`, `account_identity.rs::unwrap_recovery_blob`.

The Secret Key is a 30-character Crockford base32 string (alphabet drops I/L/O/U for visual disambiguation), prefixed with the version `A3-`, with dashes inserted every 5 characters for legibility. Entropy: 30 × 5 = **150 bits**, comfortably above the 128-bit floor for offline-uncrackable secrets.

Recovery wraps and unwraps via:

- **KDF:** HKDF-SHA256 (RFC 5869) with `info = b"pollis-account-key-wrap-v1"` and a per-user 32-byte salt drawn from `OsRng` at signup. The IKM is the *normalized* Secret Key body (case-folded, dash-stripped, whitespace-stripped).
- **AEAD:** AES-256-GCM with 12-byte random nonces.
- **On-disk format:** the `account_recovery` row carries `salt` (32 B), `nonce` (12 B), and `wrapped_key` (48 B = 32 B Ed25519 private + 16 B AEAD tag).

Argon2 is **not** used for the Secret Key, because a 150-bit truly-random secret does not need PBKDF stretching — that's the entire point of generating it for the user rather than asking them to come up with one. HKDF is the right primitive: it derives a uniformly-distributed 256-bit key from a high-entropy IKM with a domain-separating `info` string.

The user is shown the formatted Secret Key exactly once at signup. It is also returned (once) by `reset_identity` on identity rotation. The application does not store or retransmit it. This is the same shape as 1Password's Secret Key and Apple's iCloud Recovery Key — a user-held high-entropy secret that allows the operator to deliver an encrypted backup blob without ever holding the key to it.

### 5.3 Device cross-signing

Cross-signing is what stops the server from inserting a rogue device into a user's MLS groups by writing a fake `user_device` row.

Source: `account_identity.rs::sign_device_cert`, `verify_device_cert`. The signed payload is:

```
DEVICE_CERT_DOMAIN ("pollis-device-cert-v1\x00", 22 bytes)
|| u8(device_id_len) || device_id (UTF-8)
|| u8(mls_signature_pub_len) || mls_signature_pub (32 bytes for Ed25519)
|| u32(identity_version, BE)
|| u64(issued_at, BE)
```

Length prefixes prevent payload-extension and concatenation ambiguity; the trailing-NUL'd domain separator prevents the same Ed25519 key being abused to forge a signature that passes verification under some other format. Signatures are Ed25519 (RFC 8032), 64 bytes.

Inbound verification fires before MLS commit processing in two places:

1. **Outbound** — `reconcile_group_mls_impl` records `added_user_id` and `added_device_ids` in `mls_commit_log` alongside the commit. (The inbound side reads this metadata to know which devices to verify.)
2. **Inbound** — `process_pending_commits_inner` calls `verify_added_devices` on every commit that adds devices (`mls.rs:1290-1311`). Verification fetches `account_id_pub` for the target user, then for each added `device_id` looks up `device_cert`, `cert_issued_at`, `cert_identity_version`, `mls_signature_pub` in `user_device` and runs `verify_device_cert`.

Verification failures currently log a warning and proceed (the comment block at `mls.rs:1287-1312` makes this explicit). The reasoning: blocking would strand the local epoch behind the rest of the group, since the sender already merged the commit. The honest description for an audit is: *Pollis detects and logs a missing or invalid cross-signing cert but does not refuse to apply the commit.* Closing this gap requires moving from "warn and proceed" to a quarantine-and-resync protocol; this is on the roadmap but not yet implemented. The corresponding invariant in adversarial models is: **a server that creates a fake device cannot mount a passive eavesdropping attack — the rogue device's leaf will appear in the MLS tree, and the warning is loud — but a sufficiently silent operator could attempt this and rely on users not reading logs.**

---

## 6. End-to-End Encryption (MLS)

### 6.1 Standard and library

- **Specification:** RFC 9420 — The Messaging Layer Security (MLS) Protocol.
- **Implementation:** `openmls` 0.8 (https://github.com/openmls/openmls), with `openmls_rust_crypto` 0.5 providing the crypto provider over the `RustCrypto` AEAD/HKDF/HPKE primitives, and a Pollis-defined `MlsStore` (`src-tauri/src/signal/mls_storage.rs`) implementing the `openmls_traits::storage::StorageProvider` trait against the local SQLCipher `mls_kv` table.
- **Cipher suite:** `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` (`mls.rs:76`). This is suite 1 in RFC 9420 §17.1, MTI for MLS 1.0:
  - HPKE (RFC 9180): DHKEM(X25519, HKDF-SHA256), HKDF-SHA256, AES-128-GCM
  - Hash: SHA-256
  - Signature: Ed25519 (RFC 8032)

This is the same cipher-suite tier as Wire's OpenMLS deployment and the Cisco MLS reference. AES-128 (rather than AES-256) is per the MTI suite; AES-128 in the AES-GCM construction provides 128-bit symmetric strength, which is the level of all other primitives in the suite (X25519 is at 128 bits, SHA-256 collision resistance is 128 bits, Ed25519 is at 128 bits) — the upgrade to AES-256 would not raise the suite floor.

### 6.2 Group lifecycle

- **One MLS group per Pollis Group.** Every channel in the same Group shares the Group's MLS group; the channel ID is metadata on the application message. (Source: `messages.rs::send_message:172-186`.)
- **One MLS group per DM channel.**
- **Group ID:** the Pollis conversation ID (a ULID for groups, a ULID for DM channels).
- **Group creator** seeds the tree (epoch 0) at `init_mls_group`; `MlsGroupCreateConfig::use_ratchet_tree_extension(true)` is set so every Welcome carries the full ratchet tree inline (no separate tree-fetch).
- **Membership changes** flow through one function: `reconcile_group_mls_impl` (`mls.rs:1701`). It builds the *desired* roster from `group_member` ∪ `group_invite` (for groups) or `dm_channel_member` (for DMs), peeks at the actual MLS tree, claims unclaimed `KeyPackage`s for devices not yet in the tree, and emits a single combined commit with both `Add` and `Remove` proposals. Pending invitees are pre-added so that accepting an invite is a no-MLS-roundtrip operation — the Welcome is already in `mls_welcome` at invite time.

### 6.3 Commit/Welcome ordering

The remote DB is the source of truth for MLS state. The reconcile staging order (`mls.rs:1909-2050`) is:

1. Build and **stage** the commit locally (persisted to MLS storage as a *pending* commit, no local epoch advance).
2. Open a **fresh** libsql connection (the original may have had its Hrana stream evicted during the slow MLS crypto work — the wiki explicitly calls this out as the cause of the "9-user churn flake," commit 83df6ef).
3. Insert the commit row into `mls_commit_log` and per-recipient Welcome rows into `mls_welcome`.
4. Only on remote success: `merge_pending_commit` locally, advancing the epoch.
5. On remote failure: `clear_pending_commit` locally, leaving the device at the prior epoch so a retry recomputes from scratch.

This ordering is the explicit defence against "local is ahead of remote" split-brain and is invariant for the audit. A device that attempted to break it (e.g. by merging locally first) would create permanent forward-secrecy violations: members at the new epoch could no longer decrypt because their tree state would never converge.

### 6.4 External commit / new-device join

Source: `mls.rs::external_join_group`. New devices joining an existing group post-enrollment (§5) cannot rely on a Welcome from a sibling — sibling devices may be offline. They use the MLS *external commit* mechanism (RFC 9420 §11.2.1):

1. Fetch `mls_group_info` for the conversation. The row carries the latest TLS-serialised `GroupInfo` snapshot, plus its epoch.
2. Build a `MlsGroup::external_commit_builder` with the `GroupInfo` and the new device's `BasicCredential`. The ratchet tree extension carried in the GroupInfo is sufficient for the joining device to reconstruct enough state to issue a commit.
3. Post the resulting commit to `mls_commit_log` at the GroupInfo's epoch. Existing members merge it on their next `process_pending_commits` pass. The new device immediately sees itself as a member at the new epoch.

The path **does not currently** route through outbound cross-signing cert verification (`external_join_group` issues the commit but does not surface metadata for `verify_added_devices`). Existing members will detect the new device's cert through the same "added_user_id / added_device_ids" metadata path used by `reconcile_group_mls_impl`'s commits — but only when *those members* run `verify_added_devices`, which currently warns rather than rejects (§5.3). The same hardening item applies here.

### 6.5 KeyPackage lifecycle

Each device publishes 5 `KeyPackage`s at `initialize_identity` (`mls.rs::ensure_mls_key_package`, target = 5). KeyPackages are one-shot — claiming one increments `mls_key_package.claimed = 1` atomically (libSQL `UPDATE … WHERE ref_hash = (SELECT … LIMIT 1) RETURNING …`). Replenishment happens after every Welcome a device processes (`mls.rs::replenish_key_packages` callsite from `poll_mls_welcomes_inner`).

KeyPackages are validated by the consumer at claim time — `KeyPackageIn::validate(crypto, ProtocolVersion::Mls10)` checks the embedded leaf-node signature against the credential's public key, the cipher suite, and the protocol version. An attacker who tampers with a published KeyPackage cannot make it pass `validate`; the worst they can do is make it fail and waste a slot.

### 6.6 Application message encryption

`send_message` (`messages.rs:161`) is the single entry point. The path is:

1. Poll Welcomes for this device (`poll_mls_welcomes_inner`).
2. Process pending commits (`process_pending_commits_inner`) — falls through to external-join if no local group.
3. `try_mls_encrypt(local_db, mls_group_id, plaintext)` produces a TLS-serialised `MlsMessageOut` (an MLS `application_data` message).
4. Hex-encode the ciphertext, prefix `mls:`, and `INSERT INTO message_envelope`.
5. Fire a LiveKit data event (`new_message`) to wake online recipients. Non-fatal — offline recipients catch up via `poll_pending_messages` on next read.

The `mls:` prefix is a *forward-compatibility marker* from the migration; the codebase no longer has a non-MLS path on the inbound side, but the prefix is preserved so that a stored ciphertext from before MLS rollout is still recognisable. Decrypt (`messages.rs::list_messages` → `try_mls_decrypt`) hex-decodes after the prefix and feeds bytes to `MlsGroup::process_message`.

### 6.7 Forward secrecy and post-compromise security

Both follow directly from MLS (RFC 9420 §15.4-§15.6):

- **Forward secrecy** is provided by the TreeKEM ratchet: an attacker who recovers a member's leaf private key at epoch N can decrypt only messages within epoch N, because every commit advances the tree and rotates path secrets. In Pollis, every membership change triggers at least one commit, and group state is rotated whenever members are added or removed — there is no minimum heartbeat ratchet, but typical group activity (sends, opens, membership churn) keeps the epoch advancing.
- **Post-compromise security** is provided by the same mechanism: an attacker holding a member's leaf private key at epoch N retains plaintext access only until that member's next *self-update* commit, at which point their leaf path secret rotates and the attacker is locked out. Pollis does not currently issue periodic self-updates on idle clients; this is a known gap relative to RFC 9420's recommendation, mitigated by the fact that membership change traffic in active conversations rotates path secrets frequently.

### 6.8 Bounded-history property (deliberate)

The product principle in `CLAUDE.md` is exactly stated: messages sent before a member joined an epoch are not visible to that member. This is a property of MLS, not an additional restriction. New devices for an existing user begin empty; Pollis does not implement Megolm-style key backup. The deliberate consequence is that `account_recovery` only restores account *identity*, not message history — anyone reviewing the protocol who expects a backup blob to also seal historical message keys should note that no such mechanism exists by design.

---

## 7. Local Encrypted Storage (SQLCipher)

Source: `src-tauri/src/db/local.rs`.

- **Library:** `rusqlite` 0.31 with the `bundled-sqlcipher` feature, which links a vendored SQLCipher 4 (a fork of SQLite providing page-level AES-256-CBC with per-page HMAC-SHA512 for tamper detection; PBKDF2-HMAC-SHA512 page-key derivation is part of the default profile but is not used by Pollis — see "Key application" below).
- **Key application:** `PRAGMA key = "x'{hex}'";` with the 32-byte raw key; this skips SQLCipher's own KDF and uses the raw key directly as the page key — appropriate because the input is a CSPRNG-generated 32-byte uniform key, not a passphrase.
- **Path:** `pollis_{user_id}.db` under the OS-appropriate data dir (Linux `~/.local/share/pollis`, macOS `~/Library/Application Support/com.pollis.app`, Windows `%APPDATA%\pollis`). PRAGMAs: `journal_mode=WAL`, `foreign_keys=ON`.
- **Schema-version semantics:** if `LOCAL_SCHEMA_VERSION` mismatches, the DB file is wiped and recreated. The wipe is *narrow* — it triggers only on missing schema-version row, version-string mismatch, or `SqliteError::NotADatabase` (wrong key). Any other rusqlite error surfaces, refusing to eat the local database on an unfamiliar failure.

### 7.1 What's local-only

- Decrypted message plaintext (`message.content`).
- MLS group state (`mls_kv` rows: epoch state, ratchet tree state, leaf private keys, signature keypairs, KeyPackage private halves).
- Per-device stable MLS signing-key public reference (`mls_kv` scope `PollisDeviceSigPub`).
- UI/preferences cache.

### 7.2 What's deliberately not local

User profile rows, group/channel metadata, membership, blocks: those live on Turso and are fetched at read time. The argument for this separation is partial-trust: a stolen device with the SQLCipher key cannot enumerate the user's social graph without also being authenticated to Turso (via `TURSO_TOKEN`, baked into the binary — see §13 for trust caveats).

---

## 8. Remote Database Transport (Turso / libSQL)

Source: `src-tauri/src/db/remote.rs`.

- **Library:** `libsql` 0.6 with the `remote` feature, which uses Turso's **Hrana over HTTP/2** (the libSQL native protocol). The connection URL scheme is `libsql://...`. TLS is mandatory; `libsql` 0.6's `remote` feature uses `rustls` under the hood with the system trust store.
- **Authentication:** a long-lived bearer `TURSO_TOKEN` baked into the desktop binary's environment (`src-tauri/src/config.rs::Config::load`). Per-user authentication is **not** layered on top of this — every Pollis client signs into the same Turso database with the same token. Row-level security is enforced at the *application* layer, in Rust commands, not by Turso.
- **Resilience:** `RemoteDb::with_retry` handles transient Hrana stream eviction (libsql idle-stream GC) by reconnecting and retrying once. Non-transient errors surface.

### 8.1 Threat consequence of a single shared token

A reverse-engineer who extracts `TURSO_TOKEN` from a built binary can open a libSQL connection equivalent to any Pollis client. They can:

- Read every public-metadata table (which is the same threat surface as a server-side database compromise).
- Insert rows into tables not protected by application-level checks. The application enforces:
  - Per-actor permission on group/channel CRUD inside Tauri commands (the actor's `user_id` is supplied by the frontend and trusted because the frontend got it from the unlocked `account_id_key`).
  - Atomic claim semantics on `mls_key_package`.
  - `device_cert` cryptographic verification *on the read path*.
- They cannot decrypt any message — those are MLS-encrypted.
- They cannot forge a device into a user's MLS group without that device's cert verifying against the user's `account_id_pub`. The cross-signing check is the floor.

The general shape — a desktop client carrying a credential to talk to backing services, with the cryptographic protocol (not the token) acting as the defence against server compromise — is similar to Signal Desktop, but with a meaningful difference: Signal Desktop holds a *per-account* auth token issued at registration, while Pollis ships a *single shared* `TURSO_TOKEN` baked into every binary. The shared-token simplification compared to per-account tokens is a known cost; mitigations are in §13.

---

## 9. Object Storage (Cloudflare R2)

Source: `src-tauri/src/commands/r2.rs`.

### 9.1 Convergent encryption (attachments)

- **Content hash:** SHA-256(plaintext). Used as the dedup anchor and the input to key derivation.
- **Key/nonce derivation:** HKDF-SHA256 with the content-hash as IKM, `info = b"pollis-att-key"` for the 32-byte AES-256-GCM key and `info = b"pollis-att-nonce"` for a 12-byte base nonce. No salt (the input is already uniformly random for any non-pathological input file).
- **AEAD:** AES-256-GCM (NIST SP 800-38D), 12-byte nonces. The plaintext is split into 4 MiB chunks; each chunk's nonce = `base_nonce XOR LE(u32(chunk_index))` in the first 4 bytes. The chunked construction lets large files stream without buffering, while the per-chunk nonce derivation ensures uniqueness without state.
- **Object key:** `media/{content_hash}/{sanitised_filename}.enc`. Same input → same R2 object; cross-user dedup falls out naturally.

### 9.2 Visibility on R2

R2 sees: opaque AEAD ciphertext, the deterministic object key (which includes the content hash), the size, and the upload time. R2 *does not* see the AEAD key — it never leaves the device.

This is the same shape as MEGA's "Convergent Encrypted" layer (without its block-level dedup) and Tresorit's deduplication scheme. The intentional security trade-off is the **confirmation-of-file attack**: an adversary who already has a candidate plaintext can compute its content-hash and check whether the corresponding R2 key exists. Pollis accepts this trade as the cost of cross-user dedup. A dedicated audit recommendation could replace this with per-conversation key wrapping (drop convergence, lose dedup), if the threat model warrants it.

### 9.3 R2 transport

R2 is reached over HTTPS with **AWS SigV4** (`sigv4_headers`): canonical request → string-to-sign → date-region-service-derived signing key (HMAC-SHA256) → signature in the `Authorization` header. The `R2_ACCESS_KEY_ID` and `R2_SECRET_KEY` are baked into the binary, like `TURSO_TOKEN`. Same shared-credential trust model (§8.1).

The upload Tauri command (`upload_media`) reads files from disk by path, not over IPC, so arbitrary-size attachments do not hit IPC framing limits.

### 9.4 Avatars and group icons

These go through `upload_file` / `download_file` (the non-`upload_media` path) and are **not** encrypted. They are public to anyone with the R2 URL. This is intentional — avatars and group icons are visible to anyone who can see the user/group on Turso anyway, so the additional surface from making them public bytes is zero. It is, however, worth flagging in an audit: an attacker who guesses or scrapes Turso `users.avatar_url` / `groups.icon_url` can fetch the underlying images without authentication. The dedup-via-hash property does not apply to this path.

---

## 10. Real-Time Media (LiveKit)

Source: `src-tauri/src/commands/livekit.rs`, `voice.rs`, `realtime.rs`.

### 10.1 Authentication

LiveKit uses room-scoped JWT tokens (`make_token`, `make_admin_token`):

- HS256, 1-hour validity for participant tokens, 5-minute for admin tokens used by RoomService.
- The signing secret (`LIVEKIT_API_SECRET`) is baked into the desktop binary. Same caveat as §8.1: any client can mint any token. Authorisation to join a particular room is therefore enforced at the *Pollis application* layer (`get_livekit_token` is only called for rooms the user has demonstrated membership of), not by LiveKit.

### 10.2 Voice plaintext at the SFU

LiveKit is a Selective Forwarding Unit (SFU). Audio frames are encrypted on each peer-to-SFU hop using **DTLS-SRTP** (RFC 5763, RFC 5764) — the same primitive WebRTC uses everywhere — but the SFU sees plaintext audio frames in order to mix and forward. **Voice is not end-to-end encrypted in the sense of being unreadable to LiveKit operators.** This is the same architecture as Slack Huddles, Microsoft Teams, and Google Meet. **Discord voice differs:** since September 2024, Discord's DAVE protocol layers MLS-derived SFrame encryption on top of WebRTC so the SFU does *not* see plaintext audio or video. Pollis does not yet do this; the SFU sees plaintext.

LiveKit Cloud / our self-hosted LiveKit do support insertable-streams-based E2EE (`livekit` crate exposes a key-provider hook), but Pollis does not currently enable it. Adding it is straightforward at the protocol level (one `EncryptionType::Custom` on the `RoomOptions` plus an MLS-keyed key provider) but is not done. This is the single largest deviation between Pollis' messaging-side and media-side cryptographic guarantees and is the most important audit-relevant gap to flag.

### 10.3 Audio pipeline (defensive context)

Mic capture: `cpal` in 10 ms i16 mono frames → optional RNNoise (`nnnoiseless`) → WebRTC AudioProcessing module (AGC2 + NS + HPF + AEC, via `webrtc-audio-processing`) → LiveKit `NativeAudioSource.capture_frame` → SRTP. There is no JS-layer media path because Tauri's WebKitGTK webview on Linux does not expose WebRTC; this is enforced by the architecture and is described in `CLAUDE.md`. Audio never enters the webview.

### 10.4 Signalling channel

LiveKit data packets carry application-level events: `new_message` (a wake-up; the actual ciphertext is fetched from Turso), `membership_changed`, `enrollment_requested` with the verification code in cleartext (rationale: the verification code is a *human* channel for the user to compare across screens — it's not authenticating; the cryptographic authentication is the ECDH wrap in §5.1). LiveKit operators see all of these. They do not see message ciphertext, MLS state, or any private key material.

---

## 11. Rate Limiting, Block Enforcement, Abuse Surfaces

### 11.1 OTP request rate limiting

The Pollis Rust process does not throttle `request_otp`. Throttling lives at:
- Resend (per-domain reputation, per-API-key limits).
- The application token: a single shared `RESEND_API_KEY` that any client can use through the path described above.

This is a known gap. A future hardening pass could add an in-process token bucket keyed by email address; doing so requires careful UX on legitimate reattempts because the app is local-first.

### 11.2 PIN attempt rate limiting

Local, per-user, capped at 10 then nuke. No backoff. See §3.3.

### 11.3 Enrollment verification code

6 digits, 20 bits, single-use (10-minute TTL on the `device_enrollment_request` row), constant-time compared. Brute-forcing requires writing to Turso (which costs per-attempt latency) and racing the user's confirmation window.

### 11.4 Block enforcement

`user_block` is a directional table (A blocking B does not imply B blocks A) but enforcement is symmetric — both directions are checked at DM creation and at message send (`messages.rs::send_message:188-244`, `dm.rs::is_blocked_either_way`).

DM block mechanics (deliberately asymmetric in observability):
- The *blocker* sees the conversation disappear from their list (`list_dm_channels` filters by `user_block.blocker_id = me`).
- The *blockee* still sees the conversation. Sending succeeds locally — an entry appears in their local `message` table — but the message is *not* MLS-encrypted, *not* posted to `message_envelope`, and *not* broadcast on LiveKit. The blocker never receives it. The blockee sees no observable signal that they have been blocked.

This is the same observability pattern as Signal/iMessage. The privacy property is: "blocked" is not a backchannel for the blocker to signal anything about themselves to the blockee.

Group-channel blocks are render-side only — the blocker filters out blocked senders client-side, and the encrypted plaintext is still written to `message_envelope` and forwarded over LiveKit. The MLS group is not aware of blocks.

### 11.5 Identity reset (destructive)

`reset_identity_and_recover` (`device_enrollment.rs:663`) is the destructive recovery path. It requires:

- A valid OTP for the `users.email` (proven via prior `verify_otp`).
- A constant-time match between user-typed `confirm_email` and stored `users.email`.

It then:

1. Generates a fresh account-identity Ed25519 keypair, bumps `users.identity_version`, replaces the `account_recovery` blob.
2. Deletes the user from every `group_member`, `dm_channel_member`, `mls_key_package`, `mls_welcome` row. Promotes a new admin if the user was sole admin. Deletes empty groups.
3. Wipes the local SQLCipher DB and its WAL/SHM.
4. Records a `security_event` of kind `identity_reset`.

The cryptographic effect is that every other device the user previously enrolled is *cryptographically* orphaned: their locally-held account-identity private key no longer derives the published `account_id_pub`, so their `device_cert`s no longer verify, so their MLS leaves no longer admit into any new commit. This is the strongest action available without server cooperation, and it requires only the user's email and a working OTP delivery — which is the deliberate "soft recovery" UX.

The audit-relevant property is: an attacker who compromises only the user's email account can mount this against the user. The defence is the user's `security_event` log (visible in the Security settings page) and the catastrophic, observable nature of the attack — every other device the user owned will be locked out the next time it tries to do anything.

---

## 12. Key Material Summary

| Material | Algorithm | Where it lives | Where it does not live |
|---|---|---|---|
| Account identity private | Ed25519 (32 B) | OS keystore (`account_id_key_wrapped_{uid}`, AEAD under PIN-derived KEK); `AppState.unlock` (Zeroizing, in-process) | Anywhere unwrapped on disk; any server endpoint as plaintext |
| Account identity public | Ed25519 (32 B) | `users.account_id_pub` (Turso); local `mls_kv` indirectly via leaf nodes | — |
| Secret Key (recovery) | 150-bit Crockford base32 | User's offline backup | Any Pollis-operated system |
| Account recovery wrap key | HKDF-SHA256 → 32 B | Derived on-demand from Secret Key + per-user salt | Stored anywhere |
| Per-device MLS signing private | Ed25519 (32 B) | Local `mls_kv` (under SQLCipher) | Off-device |
| Per-device MLS signing public | Ed25519 (32 B) | `user_device.mls_signature_pub` (Turso); local `mls_kv` | — |
| MLS leaf / commit / welcome material | TreeKEM, RFC 9420 | Local `mls_kv` (under SQLCipher) | — |
| MLS application secrets | RFC 9420 | Ephemeral, per epoch | Persisted past their epoch |
| DB encryption key (SQLCipher) | 32 random bytes | OS keystore (`db_key_wrapped_{uid}`, AEAD under PIN-derived KEK); `AppState.unlock` | Anywhere unwrapped on disk |
| PIN | 4 ASCII digits | User's head | Stored anywhere on disk or wire |
| KEK (PIN-derived) | Argon2id → 32 B | Ephemeral, derived from PIN at unwrap time | Stored anywhere |
| OTP | 6-digit numeric | In-memory on Pollis Rust process as SHA-256 hash, 10-min TTL | Stored on disk |
| Device enrollment ephemeral X25519 private | X25519 (32 B) | `AppState.enrollment_ephemeral_keys` (in-memory) | Disk, server, anywhere persistent |
| Attachment AEAD key | HKDF-SHA256 over content-hash → 32 B | Derived on-demand from content-hash | Persisted; transmitted to R2 |
| `TURSO_TOKEN` | bearer | Baked into desktop binary; not user-scoped | — |
| `R2_ACCESS_KEY_ID` / `R2_SECRET_KEY` | AWS SigV4 creds | Baked into desktop binary | — |
| `LIVEKIT_API_KEY` / `LIVEKIT_API_SECRET` | JWT signing key | Baked into desktop binary | — |
| `RESEND_API_KEY` | bearer | Baked into desktop binary | — |

---

## 13. Known Gaps and Audit Focus Recommendations

Items below are ordered by adversary cost — easiest first.

1. **Voice is not E2EE.** LiveKit operators see plaintext audio at the SFU. Section 10.2. The remedy is enabling LiveKit's insertable-streams E2EE with an MLS-derived key provider; this is feasible but not implemented.
2. **Cross-signing verification is advisory on inbound MLS commits.** Section 5.3, `mls.rs:1287-1311`. A server able to write `user_device` and `mls_commit_log` rows can attempt to insert a rogue device and rely on the warning being unread. The fix requires a quarantine-and-resync state machine for commits with failed cert verification.
3. **External-join commits do not surface their own cross-signing metadata.** Section 6.4. Receivers will still verify via the `user_device` row, but the outbound side could populate `added_user_id` / `added_device_ids` for symmetry.
4. **Single shared Turso/R2/LiveKit/Resend tokens baked into the binary.** Section 8.1. Reverse-engineering the binary yields a database connection equivalent to any client. Mitigated by application-layer enforcement, MLS-layer cryptographic floors, and cross-signing — but the operator could mint a new client at will, and a leaked binary reveals the same secret. Per-user / per-device tokens with a small auth service is the standard fix.
5. **No server-side rate limiting on `request_otp`.** Section 11.1. Resend is the de-facto throttle.
6. **Avatars and group icons are public R2 objects.** Section 9.4. Anyone who guesses or scrapes `avatar_url` / `icon_url` from Turso (not directly exposed but available to any client with the bearer token) can read them.
7. **No periodic MLS self-update.** Section 6.7. PCS healing depends on natural group churn; idle groups do not self-heal.
8. **No Megolm-style key backup is by design.** Section 6.8. New devices and historical messages from before a member's join are not recoverable. Auditors should *not* report this as a gap unless the requirement statement they're auditing against asks for it; the product principle (`CLAUDE.md`) explicitly accepts it.
9. **Soft-recovery via OTP + email match alone (`reset_identity_and_recover`).** Section 11.5. Compromise of email account ⇒ ability to nuke the user's identity. Visible in the security event log; not preventable with the current factor set.
10. **OTP comparison uses non-constant-time string equality.** Section 4. The compared values are SHA-256 hex digests of a single-use, low-entropy code on a single-shot path; this is a best-practice item rather than a live attack.

---

## 14. References

**Core standards**
- RFC 9420 — *The Messaging Layer Security (MLS) Protocol*. Barnes et al., 2023.
- RFC 9180 — *Hybrid Public Key Encryption (HPKE)*. Barnes, Bhargavan, Lipp, Wood, 2022.
- RFC 9106 — *Argon2 Memory-Hard Function for Password Hashing and Proof-of-Work Applications*. Biryukov, Dinu, Khovratovich, Josefsson, 2021.
- RFC 8032 — *Edwards-Curve Digital Signature Algorithm (EdDSA)*. Josefsson, Liusvaara, 2017.
- RFC 7748 — *Elliptic Curves for Security* (Curve25519, X25519). Langley, Hamburg, Turner, 2016.
- RFC 5869 — *HMAC-based Extract-and-Expand Key Derivation Function (HKDF)*. Krawczyk, Eronen, 2010.
- RFC 8439 — *ChaCha20 and Poly1305 for IETF Protocols*. Nir, Langley, 2018.
- IRTF CFRG draft `draft-irtf-cfrg-xchacha` — *XChaCha: eXtended-nonce ChaCha and AEAD_XChaCha20_Poly1305*. Arciszewski, current.
- NIST SP 800-38D — *Recommendation for Block Cipher Modes of Operation: Galois/Counter Mode (GCM) and GMAC*. Dworkin, 2007.
- RFC 5763 / RFC 5764 — DTLS-SRTP. Rescorla, McGrew, 2010.
- AWS SigV4 — *Signing AWS API Requests*. https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_aws-signing.html.

**Implementations relied upon**
- OpenMLS — https://github.com/openmls/openmls. RustCrypto-backed reference implementation of RFC 9420.
- SQLCipher — https://www.zetetic.net/sqlcipher/. AES-256-CBC + HMAC-SHA512, page-level.
- LiveKit — https://livekit.io/. WebRTC-based SFU; insertable-streams E2EE supported but not enabled in this codebase.
- `keyring` (Rust) — https://crates.io/crates/keyring. Wraps macOS Keychain Services, freedesktop Secret Service, and Windows Credential Manager.
- Cloudflare R2 — https://developers.cloudflare.com/r2/. S3-API-compatible object storage.

**Comparable products and their cryptographic shapes**
- **Signal / WhatsApp / Messenger Secret Conversations** — Signal Protocol (X3DH + Double Ratchet). Pairwise sessions, Sender Keys for groups. Pollis differs by using MLS, which provides better asymptotic group performance and continuous group authentication. Pollis matches Signal on E2EE messaging. The two systems take different approaches to the "is this device really who it says it is" problem: Signal uses out-of-band safety numbers compared between users, while Pollis uses MLS leaf credentials with per-device cross-signing certificates issued by the user's account identity key. Cross-signing addresses a different threat (a server adding a rogue device to a user's account) than safety numbers (a person impersonating another person); neither subsumes the other.
- **Wire / Element X / Webex** — also MLS-based, all using OpenMLS or equivalent. Pollis is in the same cipher-suite tier (suite 1) as the public references for these deployments.
- **Matrix / Element (legacy)** — Megolm + Olm. Adds key backup, which Pollis intentionally does not.
- **Slack / Microsoft Teams** — TLS-in-transit, server-side at-rest encryption, no E2EE on messages or media. Pollis differs categorically: server operators can read Slack/Teams content; they cannot read Pollis messages.
- **Discord** — TLS-in-transit, no E2EE on messages, **DAVE protocol** (MLS for key agreement, SFrame for media frame encryption) provides E2EE for audio and video in DMs, group DMs, voice channels, and Go Live streams as of September 2024. Pollis is *behind* Discord on voice (Discord's SFU does not see plaintext under DAVE; Pollis' LiveKit SFU does — see §10.2) and *ahead* on messages (Discord chat is plaintext at rest on the server; Pollis chat is MLS-encrypted).
- **iMessage** — pairwise E2EE per device; per-user multi-device fan-out at send time; iCloud Messages backup historically held by Apple (and therefore subject to Apple's key custody) and only end-to-end encrypted when the user has opted into Advanced Data Protection (iOS 16.2+, December 2022). Pollis differs by using MLS group state instead of pairwise fan-out, and by not implementing any backup mechanism — Pollis has no equivalent to either default-iCloud or ADP-iCloud Messages backup.
- **1Password** — Secret Key + master password, with PBKDF2-HMAC-SHA256 (650k iterations as of 2023) stretching the master password and the Secret Key folded in as additional KDF input. Pollis' Secret Key + PIN combination is shaped similarly in spirit (a user-held high-entropy secret combined with a low-entropy local factor), with two implementation differences: Pollis uses Argon2id rather than PBKDF2 for the local-factor KDF, and Pollis' Secret Key wraps the *account identity key* on the server (HKDF-SHA256 + AES-256-GCM) rather than being mixed into the password KDF. The two roles 1Password merges into one master-password unlock, Pollis splits across the PIN (device unlock) and the Secret Key (server-side recovery wrap).
