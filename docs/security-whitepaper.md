# Pollis Security Whitepaper

**Audience:** independent security auditors evaluating the cryptographic protocol design and surrounding flows.
**Scope:** the desktop application in this repository, its Delivery Service (`pollis-delivery`), its remote services (Turso, Cloudflare R2, LiveKit, Resend), and the trust boundaries between them. Web-app concerns (XSS, CSP, SOP) are out of scope; this document covers cryptographic protocol design, key custody, identity, group membership, and the data-flow paths that move plaintext or key material across trust boundaries.
**Status:** authoritative. `ARCHITECTURE.md` at the repo root and the wiki under `.codesight/wiki/` are also authoritative for implementation specifics. Where this document disagrees with those sources on cryptographic claims, this document wins.

---

## 1. Trust Model

### 1.1 Boundaries

| Trusted | Untrusted |
|---|---|
| The user's device | Network (any path between the device and any remote service) |
| The device keystore — the OS keychain (Keychain / Secret Service / Credential Manager) where one exists, otherwise a machine-bound encrypted file (§3.5), which is weaker | Turso (libSQL) — the remote relational database |
| The signed Tauri application binary (Tauri host + WebView renderer + `pollis-core`) at the version the user installed | Cloudflare R2 — object storage for attachments |
| The local SQLCipher database file | LiveKit — SFU and signalling for voice and realtime events |
| The user-held Secret Key (printed once, expected to be stored offline) | Resend — outbound email transit for OTPs |
| — | The Delivery Service (`pollis-delivery`) — the sole writer to the remote database, and the broker that now holds the Resend, R2 and LiveKit credentials on the client's behalf (§4, §9.3, §10.1) |
| The user-held PIN (in the user's head) | Anyone with read access to a copy of `accounts.json` or the keystore who does not also have the PIN |

The application is built and shipped by the operators of the Pollis services. The trust delegation is the same as Signal Desktop or WhatsApp Desktop: the binary is trusted at install time, after which the cryptographic protocol is what defends against the *server* side of the same operator. Binary integrity now rests on two layers. The first is platform code-signing (Apple Developer ID + notarization on macOS, Azure Trusted Signing on Windows — see `.codesight/wiki/windows-signing.md`); the auto-update path verifies the same OS-native signature on every downloaded installer before launch (Gatekeeper on macOS, Authenticode on Windows), so an attacker who tampers with a release artifact in transit cannot get the running binary to install it.

The second layer, **now shipped**, is **binary transparency**. Every released build's reproducible pre-signature payload *and* its signed artifact are content-hashed and appended as leaves to a third append-only, ML-DSA-44-signed Merkle tree — served alongside the commit-log and account-key trees (§6.9) at **https://verify.pollis.com/v1/binaries**, under its own domain-separated STH context so a binary head can never be replayed as a commit-log or account-key head. Anyone can confirm that every artifact published for a given release tag is provably included in that log by running `pollis-verify release <tag>`, which trusts **only** the pinned log key (ML-DSA-44, rotated to fresh material in #732) — not Pollis, not Turso, not the host serving the files. Code-signing proves only "the holder of Pollis's key produced these bytes"; the transparency log additionally makes the *set of bytes Pollis has ever published for a release* public and non-repudiable, so a compromised or compelled operator cannot quietly ship a targeted per-user build — its hash is either logged (permanently, publicly, and cross-checkable by any monitor) or conspicuously absent from the log. **Honest limits** (full design in `docs/verifiable-builds-design.md`; P0–P2 shipped, and P5 Linux reproducibility + independent rebuilder, P3 keyless SLSA/cosign provenance, and P4 in-app verify all shipped in #484): the log records a correct leaf structure — both hashes plus the pinned build recipe — and the release pipeline appends them; and the **Linux AppImage payload is now reproducible modulo a documented residual list** (`docs/reproducible-builds-residuals.md`), with the toolchain pinned to an exact version, absolute build paths remapped out of the binary, and a `SOURCE_DATE_EPOCH` derived from the tag commit. An independent, fork-runnable rebuilder (`.github/workflows/rebuild-verify.yml`) rebuilds that payload from public source at a tag and asserts the reproduced hash is the one logged, trusting **only** the pinned key. **This is demonstrated, not merely implemented:** at `v1.8.4` the rebuild matched the logged payload `1a4213a1…` exactly, and the rebuilder now runs automatically after every release, so the property is continuously checked rather than asserted. Two honest bounds on that result: it is the *Linux AppImage payload* only, and reproduction currently requires building at the same filesystem path, because `--remap-path-prefix` is a rustc flag and does not cover C/C++ compiled through `cc-rs` (`docs/reproducible-builds-residuals.md`). **macOS and Windows payload reproducibility remains best-effort** (cross-platform, not asserted by the rebuilder), and the signing/notarization outer layer is non-reproducible *by construction* — transparency-logged and cryptographically bound to the payload, not reproduced. On **macOS and Windows**, however, the logged payload digest is now **recomputable by anyone holding the public `.dmg` / `.exe`** (#750): it is defined as the shipped artifact with that platform's per-signing material normalized back out — on macOS the stapled notarization ticket, each Mach-O code signature and the `_CodeSignature` manifests; on Windows the Authenticode certificate table and the PE checksum signtool recomputes over it, stripped from every executable inside the installer — so a third party can open the release, apply the same normalization, and check the result against the log. This is the ordinary reproducible-builds treatment of signatures (exclude, don't reproduce; cf. F-Droid's "identical apart from the signature"). It replaced a scheme that hashed a separate unsigned build existing only inside CI, which no outside party could obtain and therefore could not check — and which additionally made each release conditional on the Rust compiler emitting identical bytes across two compiles of one source tree, a gate that failed a real release twice in a row for reasons unrelated to that release's soundness. It does **not** by itself establish that the macOS or Windows bytes rebuild from source — that remains best-effort, pending a matching-platform reproducer and the compiler-determinism work itemized in `docs/reproducible-builds-residuals.md`. **A second, independent transparency anchor now ships (P3, #484):** every released installer and updater bundle additionally carries a keyless **SLSA v1 build-provenance attestation** (GitHub's `actions/attest-build-provenance`) *and* a **cosign signature**, both anchored in the **public Rekor** log and cryptographically bound to Pollis's **GitHub Actions OIDC identity** — published next to the artifact on `cdn.pollis.com`, the attestation at exactly the `provenance_uri` each binaries-log leaf records. Anyone can run `cosign verify-blob` (or a SLSA verifier) and confirm the bytes were produced by the pinned Pollis release workflow, checked against Rekor, with **no Pollis-held key on that verification path** — defense in depth against a compromised or compelled Pollis signing key, and a transparency anchor Pollis does not control. This proves *build provenance* and adds a *non-Pollis* anchor; it does **not**, by itself, prove the bytes reproduce from source — that remains the reproducibility story above. Remaining gaps, itemized in the residual list: the client no longer bakes any R2 or LiveKit credentials — those moved off-client in the #506 secrets-broker cutover — so the only baked credentials left are the publishable read-only Turso token and an **optional** observability log-DB token. It is that optional log-DB token that, when a release bakes it, still prevents a *fully secretless* third party from bit-reproducing even the Linux payload (a party given the published recipe reproduces it, as before). (An optional in-app **"Verify this build"** affordance on the Security page already lets the running app confirm its own payload is published in the log — P4, #484.) For the platforms and inputs not yet reproducible, the log still proves *what* Pollis published for a tag; independent proof that those bytes *match public source* holds today for the Linux payload (given the recipe, on a matching runner) and is the remaining work elsewhere.

### 1.2 What the server can and cannot see

Turso is the canonical store of *metadata*. It can observe, in plaintext: user records (id, email, username, avatar URL), social graph (group membership, DM channel membership, blocks), conversation metadata (timestamp, MLS commit and welcome timing), key-package availability, device registration (cert blobs and `mls_signature_pub`), security events (`security_event`), and connection patterns (IP address, libSQL Hrana streams). Two former per-message leaks have been closed by the metadata-minimization work (see the shipped paragraph below): the stored `message_envelope` row **no longer carries the per-message sender** (sealed sender), and text-message **ciphertext size is padded to coarse buckets**, so a stored-row dump reveals neither *who sent* a message nor its exact length.

Turso cannot recover, by design: any message plaintext, any private key (the `account_id_key` is only present on the server in the form of a `account_recovery` blob whose key derivation input — the user's Secret Key — is never sent to the server), MLS group state, MLS application secrets, or attachment plaintext (R2 attachments are convergent-encrypted by the device before upload).

LiveKit can see: real-time data-channel events (`new_message`, `membership_changed`, `enrollment_requested`, voice presence) — these payloads are JSON, not encrypted at the application layer; they are signalling, not message content. As of the signalling-minimization work, the `new_message` wake-up is a bare conversation-routing ping and **no longer carries `sender_id` / `sender_username`** — the recipient re-derives the true sender from the decrypted MLS credential (§6.6), so the field was pure leakage and is gone. The same rule now covers the two shared-room broadcasts that still named their actor: `typing` and `voice_joined`/`voice_left` no longer carry `user_id`/`username`/`display_name`, and the recipient attributes them from the **publishing participant** (#836). **What LiveKit sees of *identity* is also pseudonymous now:** room names have been opaque per-conversation pseudonyms since #828, and since #836 so are participant identities — the user and device are encrypted into a per-room handle, and the JWT carries no username — so the SFU can no longer map a participant to an account, nor recognise the same account across two rooms, which is what previously let it rebuild the social graph by co-membership independently of Turso. It remains able to count participants in a room and to recognise a returning participant *within* that room: these are stable pseudonyms, not unlinkable ones (for 1:1 calls that is already per-call, since `call-<ulid>` rooms are ephemeral). Voice audio is forwarded by the SFU as ciphertext: every audio frame is encrypted with AES-128-GCM by libwebrtc's `FrameCryptor` before it leaves the device (see §10.2), so LiveKit operators see RTP routing metadata but not voice plaintext.

**Metadata minimization (shipped, with an honest live-request caveat).** Three application-layer minimizations have shipped (full design and threat model in `docs/metadata-minimization-design.md`; v1/v2 shipped, v1.5/v3/v4 tracked in #489):

- **Sealed sender (v1)** — the delivery path no longer writes the real sender into the `message_envelope` row; it writes a non-identifying sentinel with `sealed = 1`, and recipients attribute each message from the MLS-authenticated `{user_id}:{device_id}` credential inside the ciphertext (§2.3, §6.6) rather than from the server-visible column. **Scope is at-rest only.** A Turso breach, subpoena, or cold dump of `message_envelope` no longer reveals who sent which message — the persistent, retrospectively-subpoenable sender artifact stops existing. It does **not** hide the sender from a Delivery Service operator watching *live* requests: the DS still authenticates every send by a device signature carrying the sender's `X-Pollis-User` header (`pollis-delivery/src/auth.rs`), so it sees the sender in real time. Closing that live axis needs anonymous membership proofs (v1.5, deferred, #489); until then, do not read sealed sender as "the server can't tell who sent it" — the *stored row* can't, the *live DS* still can.
- **Size padding (v2)** — text plaintext is padded to size buckets (PADMÉ, ~12% worst-case overhead, above a 256 B floor) *inside* the MLS ciphertext before encryption and stripped after decryption, so `message_envelope` sizes collapse to coarse bands. This is **text envelopes only**: attachment blobs ride convergent-encrypted R2 objects whose size is inherent to cross-user dedup (§9.1), so their sizes are not padded.
- **Signalling minimization (v2)** — the LiveKit `new_message` payload carries no sender (above).

What remains visible — and is **irreducible** for a store-and-forward server — is that conversations exist, roughly how many members each has, and when they are active; the social graph is still keyed by `user_id` in membership rows (per-conversation pseudonyms are the deferred v3, #489). Pollis does **not** claim anonymity or IP-hiding (the relay overlay, #455, is deferred). It **does** claim post-quantum *confidentiality* for group traffic (#454, shipped — §6.1, §6.10): the key exchange is a hybrid X25519 + ML-KEM-768 KEM, so a recording made today is not decryptable by a future quantum computer. Post-quantum *authentication* now ships alongside it (#668, §6.1): account identity keys, device certs, Delivery-Service request auth, transparency-log tree heads, and the PQ suite's MLS leaves are all ML-DSA-44 (FIPS 204). #669 has since retired the classic suite, so every MLS leaf now signs ML-DSA-44 — but any group created before the fleet finished adopting the PQ suite ran classic until it migrated, and traffic sealed before that boundary stays classically sealed.

R2 can see: opaque AEAD ciphertext at deterministic content-hash-derived keys. The plaintext, content-hash, and AEAD key are never on-wire to R2.

Resend sees: an email address and a 6-digit OTP, in plaintext, for the duration of the email-delivery transaction.

---

## 2. Identity Layers

Pollis carries three nested identities. Distinguishing them is essential for the rest of the document.

### 2.1 Account identity (per user)

A long-lived **ML-DSA-44** keypair (FIPS 204), generated on the device that completes signup — Ed25519 (RFC 8032) until #668. Source: `pollis-core/src/commands/account_identity.rs::generate_account_identity`. The public half is published to `users.account_id_pub` (BLOB, **1312 bytes**) at signup. The private half is canonically the **32-byte seed** the key expands from, exactly the size the Ed25519 private was, so every place that holds, wraps, or transports it (§3.2, §5.1, §5.2) is byte-identical in size to the pre-#668 era. It exists in exactly two places:

1. On the user's enrolled devices, on disk only as ciphertext in the OS keystore slot `account_id_key_wrapped_{user_id}` (see §3 for wrapping).
2. On the server, on disk only as ciphertext in the `account_recovery` table, wrapped under a key derived from a user-held *Secret Key* the server has never seen.

When `users.account_id_pub` rotates (`reset_identity`), `users.identity_version` increments. Every device whose locally-held private key does not derive a public key matching the current `account_id_pub` is treated as orphaned and wiped on next sign-in (`auth.rs::verify_otp` orphan-wipe branch, `account_identity.rs::has_matching_local_account_identity`).

### 2.2 Device identity (per device per user)

Each device gets a stable ULID `device_id` on first sign-in (`auth.rs::register_device`), persisted in the OS keystore at `device_id_{user_id}`. The device also generates stable per-device MLS signing keypairs — **one per signature scheme**, a split introduced by #668 when the two suites then in use stopped agreeing on one: Ed25519 for the classic suite's leaves, ML-DSA-44 for the PQ suite's (`pollis-core/src/commands/mls/device.rs`, keyed on the scheme rather than the suite so two suites sharing a scheme share a key). #669 retired the classic suite, so no live suite mints an Ed25519 leaf any more, but both keys are still generated, published and certified — the scheme, not the suite, decides whether a stored key can verify a given leaf, and a group persisted under an older code point must stay readable. The public halves are stored in `mls_kv` locally and in `user_device.mls_signature_pub` (Ed25519) and `user_device.mls_signature_pub_pq` (ML-DSA-44, nullable, migration `000011_device_pq_signature_pub.sql`) remotely. A device that is in groups of both suites at once holds both keys; a device that has only ever run a pre-#668 build has only the Ed25519 one, and the legacy unsuffixed `mls_kv` row it lives in is still read, since that key is what its existing classic groups' leaves are signed with.

The ML-DSA-44 key does double duty as the device's Delivery-Service request-auth key: every write the client routes through the DS is signed with it and verified against `user_device.mls_signature_pub_pq` (`pollis-core/src/commands/mls/ds_client.rs`, `pollis-delivery/src/auth.rs`). The `X-Pollis-Signature` header is base64 of a 2420-byte signature — ~3228 characters, up from 88 for Ed25519.

Both of a device's MLS signing public keys are *cross-signed* by the user's account identity key, in **one** signature. This produces a `device_cert`: an ML-DSA-44 signature over a domain-separated, length-prefixed payload binding `device_id`, *both* device public keys, the `identity_version` at issuance, and the issuance timestamp (`pollis-device-cert/src/lib.rs::device_cert_signed_payload`, domain separator `pollis-device-cert-v2\0`). Certifying both keys at once is what keeps every leaf covered across the classic→PQ overlap: there is no window in which a device presents a leaf key the account key has not certified. Cross-signing is what lets every other client decide whether to admit a particular leaf into an MLS group.

### 2.3 MLS leaf identity (per device per group)

Each device's stable MLS signing keypair populates a `BasicCredential` (RFC 9420 §5.3) whose serialised content is the UTF-8 string `{user_id}:{device_id}` (`mls.rs::make_credential`). One credential per device covers every KeyPackage and every leaf node that device produces in any MLS group, so a single `device_cert` is sufficient cross-signing for the device's entire MLS surface.

---

## 3. PIN-Wrapped Key Storage

The local PIN is a *device-local unlock* factor, not a server credential. It does not travel; the server has no record of it.

### 3.1 KDF and AEAD choices

Source: `pollis-core/src/commands/pin.rs`.

- **PIN format:** 4 ASCII digits — `validate_pin`. ~13 bits of entropy.
- **KDF:** Argon2id (RFC 9106), Argon2 crate `0.5`, version 0x13. Parameters: `m_cost = 64 MiB`, `t_cost = 3`, `p_cost = 1`, output 32 bytes. Tuned to ~250 ms on a mid-range Apple-silicon or Ryzen 5 device, deliberately above the OWASP 2024 first-choice password-storage minimum (m=19 MiB, t=2). Parameters are stored inside the `pin_meta_{user_id}` blob, not hard-coded at unwrap time, so they can be bumped on any future re-wrap without a migration.
- **Salt:** 16 bytes, `OsRng::fill_bytes` (rand 0.8). Per-user, per re-wrap.
- **AEAD:** XChaCha20-Poly1305 (Mehegan / Nir, IRTF CFRG draft, `chacha20poly1305` crate `0.10`) with 24-byte random nonces. Chosen over AES-256-GCM specifically because the 24-byte nonce eliminates nonce-reuse risk across the small number of wrap events (initial set, change-PIN, lockout-recovery).

### 3.2 Wrapped material

Three slots are written under the PIN-derived KEK:

- `pin_meta_{user_id}` (verifier blob): a fixed plaintext `b"pollis-pin-ok\0\0\0"` AEAD-encrypted under the KEK. Letting unlock reject a wrong PIN by AEAD failure on this 16-byte plaintext, without unwrapping the two larger blobs, costs one Argon2 evaluation rather than three.
- `db_key_wrapped_{user_id}`: 32 random bytes, the SQLCipher key for `pollis_{user_id}.db`.
- `account_id_key_wrapped_{user_id}`: the 32-byte ML-DSA-44 seed of §2.1 (32-byte Ed25519 private before #668 — same length, so the wrapped blob's size is unchanged).

The `pin_meta` blob also carries `failed_attempts` (u32, big-endian) and `last_attempt_unix` (u64, BE), outside the AEAD. They are not secret — the threat model is a local attacker who already has keystore read access and can count attempts independently.

### 3.3 Lockout

`MAX_FAILED_ATTEMPTS = 10`. On the 10th wrong attempt, all three keystore slots and the local SQLCipher file (and its WAL/SHM siblings) are deleted (`pin.rs::nuke_wrapped`, `device_enrollment.rs::reset_identity_and_recover`). The Turso-side account is untouched. The device is now in the same state as a brand-new device: the user must re-enrol via Secret Key recovery (§5.2) or another device's approval (§5.1).

There is no time-based backoff. The Argon2id ~250 ms-per-attempt cost combined with a 10-attempt ceiling is the offline-brute-force defence; for online (UI-driven) attempts the same ceiling is the rate limit.

### 3.4 Key custody at rest

After PIN setup, raw `db_key` and raw `account_id_key` exist on disk only inside AEAD ciphertext. In-process they live in `Zeroizing<Vec<u8>>` containers (`AppState.unlock`) which scrub on drop (`zeroize` crate). `lock()` drops the unlock state and closes the SQLCipher handle, returning the device to a "needs PIN" state without forcing a full sign-out.

### 3.5 Where the wrapped blobs physically sit (#882)

The keystore backend is selected **at runtime**, once per process, and frozen for
that process's lifetime (a store split across two backends is worse than either):

1. **OS keychain** — macOS Keychain, Windows Credential Manager, Linux Secret
   Service — wherever a credential store answers a read probe. Preferred always;
   the OS guards the key.
2. **Machine-bound encrypted file** — where none answers. This is the headless
   case: a server reached over SSH has no secret-service, and before #882 the
   only options were "the client refuses to start" or "the keys go to disk in
   the clear" (#879 chose the former).

The file's bytes are AES-256-GCM under a KEK derived as
`HKDF-SHA256(ikm = platform machine ID, salt = fresh 16 bytes per write)`. The
machine ID is `/etc/machine-id` or `/var/lib/dbus/machine-id` (Linux),
IOPlatformUUID (macOS), MachineGuid (Windows) — a 128-bit host-unique value that
does **not** travel with a copy of the data directory. HKDF and not Argon2id
because the input is already high-entropy; the memory-hard cost belongs on the
PIN, where §3.1 already puts it. Where no machine ID exists, the keystore errors
and names `POLLIS_KEYSTORE_MACHINE_ID` rather than degrading to a constant.

**What the machine binding defends against:** the keystore file *leaving the
machine* — a home-directory backup, an `scp` of the data dir, a synced folder, a
container image built over a live data dir, a resold disk read on other hardware.
Those bytes are inert elsewhere.

**What it does not:**

- A local attacker running as the **same UID**. Whatever this process derives to
  decrypt, they derive too. No userspace design changes that; the OS keychain is
  what raises this bar, which is why it stays preferred.
- A **full-disk image or VM snapshot** — `/etc/machine-id` is on the same disk
  and world-readable. This defeats selective exfiltration, not an image of
  everything.
- **Forensic recovery** of the pre-migration plaintext blocks. The migration
  replaces the file atomically; the old inode's blocks are not securely erased,
  and no userspace API can promise that on a modern SSD or CoW filesystem.

This is one of two layers, and each covers the other's gap. The PIN layer (§3.1)
alone is weak against an exfiltrated file — 4 digits is 10⁴ candidates, roughly
40 minutes of single-core offline sweep at the tuned ~250 ms/guess. The machine
layer alone is weak against a same-UID local attacker. Together an attacker needs
both the host's identity and the PIN.

The PIN is deliberately **not** mixed into the file KEK. It cannot be: the
keystore is read before the PIN exists (boot reads `pin_meta_{uid}` to choose
between the "enter PIN" and "set PIN" screens, and `device_id_{uid}` to identify
the device). And it need not be — the secrets inside the file are already
PIN-wrapped one layer down.

A TPM or Secure Enclave would strictly improve the first bullet, since a sealed
key appears in no backup and no disk image. It is **not** used: `tss-esapi` is a
heavy C dependency on `tpm2-tss`, requires both a TPM and a resource manager
reachable by the invoking user, and would need three separate platform
integrations. A partial integration with a silent fallback would advertise a
protection the deployment might not have.

Mobile has always worked this way — the same file, sealed under a key held by the
Android Keystore or the iOS Keychain. Desktop was the outlier until #882.

### 3.6 Migrating an existing plaintext keystore

Installs predating #882 have a plaintext keystore. It stays readable — locking a
user out of their identity key is unrecoverable and worse than one more session
of plaintext — and the first read or write re-encrypts it in place through the
existing tempfile + fsync + rename. Because the encoding completes before the old
file is touched, every interruption leaves either the complete old file or the
complete new one, never a mixture. A file that fails to *decrypt* is never
rewritten, never backed away, and never treated as empty: those bytes are
somebody's identity key, and starting fresh would orphan it.

The same reasoning governs a file that decrypts but does not *parse*. Until #950
that case renamed the file to a timestamped sidecar, which reads like a backup
and behaves like a wipe: the next operation finds no store, treats the device as
new, and the first write establishes an empty one. It now leaves the bytes exactly
where they are and reports a stable error, so the content stays recoverable and
the only way to start over is the user's explicit "wipe this computer".

#950 also renamed the file itself, from `dev-keystore.json` to `keystore.pks`.
Both halves of the old name had become false — it is the production store on
every headless install and its contents are ciphertext, not JSON — but a rename
is a delete of live key material, so it runs as write-new, read-the-new-one-back,
then unlink-old. An interruption leaves both files, never neither, and the next
start finishes the job.

### 3.7 Comparable systems

- **Signal Desktop** uses an OS-keystore-stored randomly generated key to encrypt its local SQLCipher store, with no user PIN. Pollis adds the PIN factor; the consequence is that an attacker who clones the keystore but not the PIN cannot decrypt local data, at the cost of requiring the user to enter a PIN to unlock. This is closer to iOS message-cache encryption (PIN/biometric) than to Signal Desktop.
- **1Password / Bitwarden** use Argon2id with comparable parameters as their master-password KDF; the difference is that they have a high-entropy master password to begin with, while Pollis has a 4-digit PIN. The 10-attempt nuke-and-recover policy is what closes that gap.
- **WhatsApp Desktop** retains a database key on disk without a user-supplied factor — equivalent to Pollis' pre-PIN behaviour, kept only as a migration path.

---

## 4. Authentication Flow (OTP)

Source: `pollis-delivery/src/otp.rs` (the OTP machinery) and `pollis-core/src/commands/auth.rs::request_otp`, `verify_otp` (thin clients of it).

The OTP factor exists only to prove control of an email address. It is *not* the device unlock factor (that's the PIN) and it is *not* the account-recovery factor (that's the Secret Key).

- Generated, stored, verified and emailed **server-side, by the Delivery Service** (`pollis-delivery/src/otp.rs`). The client calls `POST /v1/auth/request-otp` and `POST /v1/auth/verify-otp` (`pollis-core/src/commands/auth.rs::request_otp`, `verify_otp`) and never handles a code it did not receive from the user.
- 6-digit numeric, drawn from `OsRng` with `gen_range(0..1_000_000u32)` and zero-padded.
- Held in DS memory as a **salted** hash — `SHA-256(salt ‖ code)` with a fresh 16-byte `OsRng` salt — never the plaintext. TTL: 10 minutes, single-shot: deleted on first successful verification.
- Email transit: HTTPS POST from the **DS** to Resend's `api.resend.com`, with the bearer `RESEND_API_KEY` supplied to the DS by its environment. **The client does not hold a Resend key.** It once did; that moved off-client with the rest of the credentials (§9.3, §10.1), so extracting a Pollis binary yields no ability to send mail as Pollis.
- Comparison is **constant-time** (`pollis-delivery/src/otp.rs::constant_time_eq`) against the stored hash. Earlier versions of this document noted that constant-time comparison was *not* used and argued the 20-bit secret made it immaterial; the server-side rewrite took the hardening anyway, so the caveat is obsolete rather than merely tolerable.
- **Rate limiting is now application-layer, at the DS.** `OtpConfig` enforces a resend throttle (30 s between sends for one address, `PrepareOutcome::Throttled`) and an attempt cap (5 wrong codes, after which the entry is locked out and deleted) — `pollis-delivery/src/otp.rs`. Earlier versions of this document said there was no application-layer limit and deferred entirely to Resend and DNS reputation; that was true of the client-side implementation and is no longer true of the DS. A per-client-IP window over the same endpoints (`pollis-delivery/src/ratelimit.rs`) bounds the cross-address case; provider-level limits remain a third layer rather than the only one. See §11.1.

OTP is consumed in two scenarios:
1. First-time signup (user has no `users` row). `verify_otp` creates the row, calls `generate_account_identity` to mint the account-identity ML-DSA-44 keypair and a Secret Key, and seeds `AppState.unlock` with the freshly-generated material. The frontend then transitions to the PIN-create screen, which is what causes that material to be persisted to disk (as ciphertext under the PIN-derived KEK).
2. Soft-recovery (`reset_identity_and_recover`). This *requires* both the OTP and a constant-time match of the user-typed email against `users.email` (constant-time via the local helper `constant_time_eq`).

Returning users on a previously-enrolled device do *not* go through OTP. They go through PIN entry against `pin_meta_{user_id}`. This is by deliberate design: in pre-PIN versions, transient OS-keystore read failures (macOS keychain hiccups, Linux secret-service races) caused returning users to be bounced back to the OTP screen on every cold start. The PIN gate replaced that. See `pin-design.md` for the full rationale and `accounts.json`'s atomic write / loud-parse-failure protocol that was added in the same change.

---

## 5. Multi-Device Enrollment

A user with an existing `account_id_pub` can add a second device through one of two paths. Both end with the same outcome: the new device holds a copy of the account-identity private key, has published a `device_cert`, has published `KeyPackage`s, and has joined every existing MLS group via external commit.

### 5.1 Approval path (in-band, sibling-device-mediated)

Source: `pollis-core/src/commands/device_enrollment.rs`.

1. New device generates an **ephemeral X25519 keypair** (`x25519-dalek` 2.0, `StaticSecret` from `OsRng` bytes). The private half is held in `AppState.enrollment_ephemeral_keys: HashMap<request_id, Vec<u8>>` — *in memory only*. App restart mid-enrollment forfeits the request.
2. New device **derives** the verification code from its own ephemeral public key: `HKDF-SHA256(ikm = ephemeral_pub, info = "pollis-enrollment-sas-v1")`, mapped 5 bits per character onto the 32-symbol Secret-Key alphabet, 8 characters (40 bits). It is not random and it is not a secret — the server can compute it too. The approving device derives it independently from the ephemeral public key it fetched, and checks the user's input against *that*, never against the server's stored copy (#793).
3. The request row is inserted into `device_enrollment_request` (Turso), carrying the new device's ephemeral *public* X25519 key, the verification code, status `pending`, a 10-minute TTL.
4. New device fans out a notification to LiveKit room `inbox-{user_id}` so any online sibling device sees the request immediately.
5. The sibling device fetches the request, the user confirms the code matches between screens, and the sibling calls `approve_device_enrollment(request_id, verification_code)`. The verification code is compared with `subtle::constant_time_eq` (local helper).
6. The sibling generates a **second** ephemeral X25519 keypair, computes ECDH(approver_priv, requester_pub), and feeds the 32-byte shared secret to **HKDF-SHA256** (RFC 5869) with `info = b"pollis-enrollment-wrap-v1"` and no salt to derive a 32-byte wrap key. AES-256-GCM (12-byte random nonce) wraps the account-identity private key — the 32-byte ML-DSA-44 seed (§2.1). The on-wire blob is a fixed-layout `approver_pub || nonce || ciphertext+tag` (92 bytes total, unchanged by #668 because the seed is the same length as the Ed25519 private it replaced). The approver writes this blob to `device_enrollment_request.wrapped_account_key` and flips the status to `approved`. A `security_event` row of kind `device_enrolled` (metadata `via=approval,approver={device_id}`) is inserted.
7. The new device's `poll_enrollment_status` sees `approved`, recovers the ephemeral private from in-memory state, and unwraps. The unwrapped 32 bytes plus a freshly generated `db_key` populate `AppState.unlock`. The frontend transitions to PIN-create; `set_pin` writes the wrapped slots.
8. `finalize_device_enrollment` runs: the new device publishes its own `device_cert`, writes 5 fresh `KeyPackage`s to `mls_key_package`, and for each existing group / DM the user belongs to, fetches the latest `mls_group_info` and joins via MLS external commit (§6.4).

This is a one-shot ECDH-then-AEAD scheme analogous to a sealed-sender envelope. It is **not** an authenticated key exchange — there is no signature on the approver's ephemeral public from the long-term account identity key. The replacement for AKE authentication is the user-confirmed verification code shown on both screens at the same time, and since #793 that code is a function of the new device's ephemeral public key rather than an independent random value. This is what makes the human channel load-bearing: an attacker who can write Turso and substitutes their own ephemeral public key changes the code the approving device derives, so the two screens disagree and the user stops. Searching for a substitute keypair that reproduces the victim's code costs ~2^40 keygens; at the previous 6-digit width it would have cost ~2^20, which is seconds, so the width is part of the mitigation and not cosmetic. The wrap key is additionally bound to the transcript — `HKDF(ikm = ECDH, info = "pollis-enrollment-wrap-v1" ‖ requester_pub ‖ approver_pub)`. The 10-minute TTL bounds exposure. Residual: the scheme still has no signature from the long-term account identity key over the approver's ephemeral public, so its authentication rests entirely on the human comparison.

This is broadly comparable to Signal's "PIN-based reregistration" flow combined with its "approval QR code" linked-device flow, with the simplifying property that Pollis runs on desktop only — there is no QR code; the user just types the displayed digits.

### 5.2 Secret Key recovery path (out-of-band)

Source: `device_enrollment.rs::recover_with_secret_key`, `account_identity.rs::unwrap_recovery_blob`.

The Secret Key is a 30-character Crockford base32 string (alphabet drops I/L/O/U for visual disambiguation), prefixed with the version `A3-`, with dashes inserted every 5 characters for legibility. Entropy: 30 × 5 = **150 bits**, comfortably above the 128-bit floor for offline-uncrackable secrets.

Recovery wraps and unwraps via:

- **KDF:** HKDF-SHA256 (RFC 5869) with `info = b"pollis-account-key-wrap-v1"` and a per-user 32-byte salt drawn from `OsRng` at signup. The IKM is the *normalized* Secret Key body (case-folded, dash-stripped, whitespace-stripped).
- **AEAD:** AES-256-GCM with 12-byte random nonces.
- **On-disk format:** the `account_recovery` row carries `salt` (32 B), `nonce` (12 B), and `wrapped_key` (48 B = the 32 B ML-DSA-44 seed + 16 B AEAD tag).

Argon2 is **not** used for the Secret Key, because a 150-bit truly-random secret does not need PBKDF stretching — that's the entire point of generating it for the user rather than asking them to come up with one. HKDF is the right primitive: it derives a uniformly-distributed 256-bit key from a high-entropy IKM with a domain-separating `info` string.

The user is shown the formatted Secret Key exactly once at signup. It is also returned (once) by `reset_identity` on identity rotation. The application does not store or retransmit it. This is the same shape as 1Password's Secret Key and Apple's iCloud Recovery Key — a user-held high-entropy secret that allows the operator to deliver an encrypted backup blob without ever holding the key to it.

### 5.3 Device cross-signing

Cross-signing is what stops the server from inserting a rogue device into a user's MLS groups by writing a fake `user_device` row.

Source: `account_identity.rs::sign_device_cert`, `verify_device_cert`, both thin wrappers over the canonical format in the dependency-free `pollis-device-cert` crate, which `pollis-delivery` re-verifies through verbatim at `POST /v1/auth/publish-device-cert` so client and server cannot drift on the wire format. The signed payload is:

```
DEVICE_CERT_DOMAIN ("pollis-device-cert-v2\x00", 22 bytes)
|| u8(device_id_len)   || device_id (UTF-8)
|| u16(ed25519_pub_len, BE) || ed25519 device pub (32 bytes)
|| u16(mldsa_pub_len, BE)   || ML-DSA-44 device pub (1312 bytes)
|| u32(identity_version, BE)
|| u64(issued_at, BE)
```

Length prefixes prevent payload-extension and concatenation ambiguity; the trailing-NUL'd domain separator prevents the same account key being abused to forge a signature that passes verification under some other format — and the `v1`→`v2` bump is what stops a v1 cert (which bound only one device key) being reinterpreted under the two-key schema. The `u8` length prefixes of v1 became `u16` because an ML-DSA-44 public key is 1312 bytes; `device_id` keeps its `u8` prefix (ULIDs are 26 bytes). Signatures are ML-DSA-44 (FIPS 204), 2420 bytes — up from Ed25519's 64.

Inbound verification fires before MLS commit processing in two places:

1. **Outbound** — `reconcile_group_mls_impl` records `added_user_id` and `added_device_ids` in `mls_commit_log` alongside the commit. (The inbound side reads this metadata to know which devices to verify.)
2. **Inbound** — `process_pending_commits_inner` calls `verify_added_devices` on every commit that adds devices (see the "Inbound cert verification (advisory)" block inside `process_pending_commits_inner`). Verification fetches `account_id_pub` for the target user, then for each added `device_id` looks up `device_cert`, `cert_issued_at`, `cert_identity_version`, `mls_signature_pub` and `mls_signature_pub_pq` in `user_device` and runs `verify_device_cert` over both device keys at once.

Verification failures currently log a warning and proceed (the comment block at the top of that branch in `mls.rs::process_pending_commits` makes this explicit). The reasoning: blocking would strand the local epoch behind the rest of the group, since the sender already merged the commit. The honest description for an audit is: *Pollis detects and logs a missing or invalid cross-signing cert but does not refuse to apply the commit.* Closing this gap requires moving from "warn and proceed" to a quarantine-and-resync protocol; this is on the roadmap but not yet implemented. The corresponding invariant in adversarial models is: **a server that creates a fake device cannot mount a passive eavesdropping attack — the rogue device's leaf will appear in the MLS tree, and the warning is loud — but a sufficiently silent operator could attempt this and rely on users not reading logs.**

---

## 6. End-to-End Encryption (MLS)

### 6.1 Standard and library

- **Specification:** RFC 9420 — The Messaging Layer Security (MLS) Protocol.
- **Implementation:** `openmls` 0.8 (https://github.com/openmls/openmls) — since #668 pinned by a workspace-wide `[patch.crates-io]` to an exact upstream `main` revision, because the `draft-ietf-mls-pq-ciphersuites` feature carrying the ML-DSA suite is not in a release; the patch covers every `openmls_*` crate so the dependency graph cannot split. Bumping that revision is a protocol-visible act: the draft renumbers provisional code points between revisions. Storage is a Pollis-defined `MlsStore` (`pollis-core/src/signal/mls_storage.rs`) implementing the `openmls_traits::storage::StorageProvider` trait against the local SQLCipher `mls_kv` table. **One crypto/rand provider, and since #669 one suite for it to serve** (`pollis-core/src/commands/mls/provider.rs`): `openmls_rust_crypto` over the `RustCrypto` AEAD/HKDF/HPKE primitives, whose AES-GCM tag check is constant-time (`subtle`). Under #454 the provider was *routed* per suite instead: the hybrid suite was X-Wing / `0x004D`, which only `openmls_libcrux_crypto` implemented, and the classic suite had to be kept away from that backend because its AES-GCM decryption has an unpatched non-constant-time tag check (RUSTSEC-2026-0211). #668 dissolved the problem rather than routing around it — the PQ suite moved to `0x0052`, which keeps the same X-Wing KEM but pairs it with ChaCha20-Poly1305 and ML-DSA-44, and which RustCrypto implements while libcrux implements no ML-DSA suite at all. The second backend left the dependency graph entirely, and with it six advisory ignores in `deny.toml` (`-0211`, `-0209`, `-0210`, `-0124`, `-0075`, `-0073`) — retired by removal, not by re-arguing reachability. The single-backend invariant is pinned by `mls_backend_is_rustcrypto` in `pollis-core/src/commands/mls/tests.rs`; reintroducing a second backend means re-arguing every one of those advisories from scratch.
- **Cipher suite (one, and every group is on it).** Every group carries its suite in its own `GroupContext`; there is no global switch and no per-message negotiation.

  | | `CS_PQ` |
  |---|---|
  | Name | `MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44` |
  | RFC 9420 code point | `0x0052` |
  | KEM | **X-Wing** — X25519 **+ ML-KEM-768** (FIPS 203) |
  | AEAD | ChaCha20-Poly1305 |
  | Hash / KDF | SHA-384 / HKDF-SHA384 |
  | Signature | **ML-DSA-44** (FIPS 204), scheme `0x0904` |

  X-Wing is a *hybrid* KEM: the shared secret is derived from both the X25519 and the ML-KEM-768 encapsulations, so the suite is at least as strong against a classical adversary as the classical DHKEM it replaced, and additionally resists a cryptographically-relevant quantum computer. An attacker must break **both** to recover a group secret.

  **There were two suites, and now there are not.** #454 shipped this one *alongside* the RFC 9420 mandatory-to-implement `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` (`0x0001`, DHKEM(X25519, HKDF-SHA256) / AES-128-GCM / SHA-256 / Ed25519) — the same tier as Wire's OpenMLS deployment and the Cisco MLS reference — so that a fleet mid-upgrade could still add each other; §6.10 records how a group chose between them. **#669 retired the classic suite**: `CS_CLASSIC` is deleted, `CS_HYBRID` is renamed `CS_PQ`, and there is nothing left to choose. Traffic sealed under the classic suite before its retirement stays sealed under it — nothing is re-encrypted.

  #668 had earlier moved the PQ suite **in place**, from `0x004D` to `0x0052`. The KEM is unchanged — the same X-Wing construction, so the hybrid-confidentiality argument #454 made carries over untouched — and what changed is the signature (Ed25519 → ML-DSA-44) and, as a consequence of the registry entry, the KDF (SHA-256 → SHA-384). The code point is provisional: `draft-ietf-mls-pq-ciphersuites` has already renumbered once, and a further renumber is a wire-format break for every group already on the suite, to be handled as a lineage migration (§6.10) rather than a version bump. That is why the suite is still an argument to the functions that mint suite-bound material, and why `signature_scheme` is still a function of the suite rather than a constant, even with one suite live.

- **Signatures are post-quantum too, and the reason is longevity, not harvest-now-decrypt-later.** The argument #454 made for leaving signatures classical was sound as far as it went, and it still is: a *store-and-forward* adversary recording traffic today and decrypting it after a CRQC exists is defeated by the KEM change, because confidentiality is what a recording attacks; forging a signature, by contrast, requires being live at the moment of use, so a future CRQC cannot reach back and forge a commit that was already delivered and verified. What that argument does not cover is the material whose *verifiability* is meant to outlive the moment. An account identity key is checked by every device that ever admits a leaf for that user, for the life of the account. A device cert is a standing claim re-checked by every client that admits the device's leaf and by the DS at publish time, and the verification primitive is pure — no clock, no I/O — so a cert stays checkable by anyone holding the account key, for as long as the cert is on file. And the public transparency log (§6.9) exists precisely so that its signed statements about history stay re-checkable *indefinitely*: an auditor in 2040 replaying the account-key or commit-log tree is verifying signatures made today, and a signature scheme a CRQC can forge is one that lets a future operator rewrite the past and produce a tree that verifies. Those are the things #668 moved. ML-DSA-44 (FIPS 204) now signs account identity keys, device certs, DS request auth, the transparency log's tree heads, and the hybrid suite's MLS leaves.

  **The cost is size, and it is not small.** An ML-DSA-44 public key is 1312 bytes against Ed25519's 32, and a signature 2420 bytes against 64. Those sit on every leaf node in a group's ratchet tree, in every KeyPackage, in every device cert, and on every request the client makes to the Delivery Service — roughly an **8× payload increase** on the authentication material, stacked on top of the PQ encapsulations §6.7 already accounts for. The DS `X-Pollis-Signature` header grows from 88 to ~3228 base64 characters — inside every default header-size limit on the path (hyper 16 KiB per header, Cloudflare 16 KiB total), but it is the reason no proxy in front of the DS may be configured below an 8 KiB header budget. The classic suite's leaves signed Ed25519 and escaped that cost; since #669 retired the suite, every live leaf pays it. The Ed25519 device key itself is still minted, published to `user_device.mls_signature_pub` and bound by the same v2 device cert, so a group persisted under the older code point stays readable — but no live suite mints a leaf under it.

### 6.2 Group lifecycle

- **One MLS group per Pollis Group.** Every channel in the same Group shares the Group's MLS group; the channel ID is metadata on the application message. (Source: `messages.rs::send_message`, ~lines 173-186.)
- **One MLS group per DM channel.**
- **Group ID:** the Pollis conversation ID (a ULID for groups, a ULID for DM channels).
- **Group creator** seeds the tree (epoch 0) at `init_mls_group`; `MlsGroupCreateConfig::use_ratchet_tree_extension(true)` is set so every Welcome carries the full ratchet tree inline (no separate tree-fetch).
- **Membership changes** flow through one function: `reconcile_group_mls_impl` (`mls.rs::reconcile_group_mls_impl`). It builds the *desired* roster from `group_member` ∪ `group_invite` (for groups) or `dm_channel_member` (for DMs), peeks at the actual MLS tree, claims unclaimed `KeyPackage`s for devices not yet in the tree, and emits a single combined commit with both `Add` and `Remove` proposals. Pending invitees are pre-added so that accepting an invite is a no-MLS-roundtrip operation — the Welcome is already in `mls_welcome` at invite time.

### 6.3 Commit/Welcome ordering

The remote DB is the source of truth for MLS state. The reconcile staging order (inside `reconcile_group_mls_impl`) is:

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

The path **does** populate `added_user_id` (the joining user) and `added_device_ids` (the single joining device) on the `mls_commit_log` row it writes, so existing members run `verify_added_devices` against the new device's `device_cert` on inbound just as they would for a normal `reconcile_group_mls_impl` add. The same advisory-rather-than-blocking caveat from §5.3 applies: a verification failure here logs a warning and proceeds.

### 6.5 KeyPackage lifecycle

Each device publishes 5 `KeyPackage`s at `initialize_identity` (`mls.rs::ensure_mls_key_package`, target = 5). KeyPackages are one-shot — claiming one increments `mls_key_package.claimed = 1` atomically (libSQL `UPDATE … WHERE ref_hash = (SELECT … LIMIT 1) RETURNING …`). Replenishment happens after every Welcome a device processes (`mls.rs::replenish_key_packages` callsite from `poll_mls_welcomes_inner`).

KeyPackages are validated by the consumer at claim time — `KeyPackageIn::validate(crypto, ProtocolVersion::Mls10)` checks the embedded leaf-node signature against the credential's public key, the cipher suite, and the protocol version. An attacker who tampers with a published KeyPackage cannot make it pass `validate`; the worst they can do is make it fail and waste a slot.

### 6.6 Application message encryption

`send_message` (in `messages.rs`) is the single entry point. The path is:

1. Poll Welcomes for this device (`poll_mls_welcomes_inner`).
2. Process pending commits (`process_pending_commits_inner`) — falls through to external-join if no local group.
3. `try_mls_encrypt(local_db, mls_group_id, plaintext)` produces a TLS-serialised `MlsMessageOut` (an MLS `application_data` message).
4. Hex-encode the ciphertext, prefix `mls:`, and `INSERT INTO message_envelope`.
5. Fire a LiveKit data event (`new_message`) to wake online recipients. Non-fatal — offline recipients catch up via `poll_pending_messages` on next read.

The `mls:` prefix is a *forward-compatibility marker* from the migration; the codebase no longer has a non-MLS path on the inbound side, but the prefix is preserved so that a stored ciphertext from before MLS rollout is still recognisable. Decrypt (`messages.rs::list_messages` → `try_mls_decrypt`) hex-decodes after the prefix and feeds bytes to `MlsGroup::process_message`.

### 6.7 Forward secrecy and post-compromise security

Both follow directly from MLS (RFC 9420 §15.4-§15.6):

- **Forward secrecy** is provided by the TreeKEM ratchet: an attacker who recovers a member's leaf private key at epoch N can decrypt only messages within epoch N, because every commit advances the tree and rotates path secrets. In Pollis, every membership change triggers at least one commit, and group state is rotated whenever members are added or removed — there is no minimum heartbeat ratchet, but typical group activity (sends, opens, membership churn) keeps the epoch advancing.
- **Post-compromise security** is provided by the same mechanism: an attacker holding a member's leaf private key at epoch N retains plaintext access only until that member's next *self-update* commit, at which point their leaf path secret rotates and the attacker is locked out. It must be the victim's *own* commit — a commit by anyone else re-keys its issuer's direct path and addresses the copath, which still contains the victim's leaf, so the attacker rides straight through it. Pollis issues self-updates from two places (#666): every device rotates its leaf immediately after joining a group, and the cold-launch/reconnect sweep rotates any group in which this device has not committed for 7 days plus a deterministic per-conversation jitter of up to 2 further days, at most 3 groups per sweep. The jitter keeps a many-group device from firing every rotation in one burst; the per-sweep cap bounds the cost of a device returning from a long absence. Idle groups therefore *do* heal — on the next launch of any member rather than on a background timer, because Pollis runs no periodic polling by design (`CLAUDE.md`). The residual is that a group all of whose members stay offline does not heal while they are gone, which is inherent: there is nobody to issue the commit. The lockout is proved end-to-end rather than argued: `a_stolen_leaf_is_locked_out_once_the_victim_rotates` (`src-tauri/tests/flows/adversarial.rs`) exfiltrates a real device's whole MLS state, shows it reading live traffic, shows a *third party's* commit failing to evict it, and only then asserts the victim's own rotation does.
- **Post-join rotation is also what keeps commits logarithmic.** A member added by someone else knows none of the secrets above its leaf, so every node on its direct path stays blank and the leaf lands in a copath resolution on its own — one HPKE ciphertext per such member in every commit anyone issues, forever, until that member itself commits. That is a linear cost in roster size that never decays on its own, and on the hybrid suite each of those ciphertexts is ~35× the classic one. The post-join self-update collapses it back to TreeKEM's logarithm. Measured in `self_update_turns_linear_commit_growth_into_logarithmic` (`pollis-core/src/commands/mls/tests.rs`): on the hybrid suite, doubling a group from 8 to 16 members costs +10.6 KB per commit with unmerged leaves versus +2.4 KB with every leaf merged.

### 6.8 Bounded-history property (deliberate)

The product principle in `CLAUDE.md` is exactly stated: messages sent before a member joined an epoch are not visible to that member. This is a property of MLS, not an additional restriction. New devices for an existing user begin empty; Pollis does not implement Megolm-style key backup. The deliberate consequence is that `account_recovery` only restores account *identity*, not message history — anyone reviewing the protocol who expects a backup blob to also seal historical message keys should note that no such mechanism exists by design.

### 6.9 Verifiable transparency logs (commit history + account keys)

Pollis publishes two append-only, ML-DSA-44-signed Merkle trees (RFC 6962 / RFC 9162) at **https://verify.pollis.com**, so anyone — a user, a journalist, an independent researcher — can prove for themselves that the server has not quietly rewritten history. The two trees are domain-separated: signed by the same key under different STH contexts (`pollis-verifiable-log:sth:v2` for commits, `pollis-verifiable-log:sth:v2:account-keys` for account keys), so a head minted for one tree can never be replayed as the other.

- **The MLS commit log** records every membership/key-change commit. Replaying it under its invariant proves no fork (no two commits share a `(conversation_id, generation, epoch)`), no epoch regression/replay (`(generation, epoch)` increases lexicographically), and that a new suite generation opens only at epoch 0. This closes the server's ability to fork a conversation, roll an epoch back, or equivocate between auditors (§1.1). Full detail in `docs/transparency.md`.
- **The account-key directory** records one leaf per account identity-key version — `(user_id, identity_version, account_id_pub)` — written to the append-only `account_key_log` table (§12) in lock-step with every `users.account_id_pub` change (signup and `reset_identity`). Replaying it under its invariant proves each user's published key history is append-only and that `identity_version` only ever increases: no silent key substitution, no replay of a revoked key. This is the auditable backstop for the TOFU pinning / safety-number layer of §11.4: where TOFU catches a swap only on the *next* message and only for keys *this* device has seen, the log makes the *entire* key history of *every* user publicly checkable by anyone.

The trust model is the one in `docs/transparency.md`: a verifier trusts **only** the log's published ML-DSA-44 public key, the signed tree head, and the Merkle proofs checked against it — not the server, not Turso, not the host serving the files. The auditor CLI `pollis-verify` (released to researchers) verifies the whole log (`remote`), one conversation (`group`), or one user's key history (`account <user_id>`) over plain HTTP, trusting only that key. After every publish, CI re-verifies its own freshly-served tree and runs an equivocation tripwire that compares the new heads against the previously-published ones; a regression aborts the publish and alerts rather than serving a forked head.

The running client self-audits too. `self_audit_account_key` verifies this user's own published key history — reusing the *same* `verify_account` function the CLI runs, never a re-implementation — and compares the chain's latest published version against this device's current key; `audit_peer_account_key` does the same against a TOFU-pinned peer. The log's public key is pinned in the client (`PINNED_LOG_PUBLIC_KEYS`, `pollis-core/src/commands/transparency.rs`; the copies of it elsewhere in the repo and on the website are held in agreement by `scripts/check-pinned-log-key.py` on every PR — #945); a served key that differs from the pin is a hard alarm, because any key can sign a self-consistent forged tree. #668 moved the STH signature from Ed25519 to ML-DSA-44 under bumped domain contexts (`pollis-verifiable-log:sth:v2`, `…:sth:v2:account-keys`, `…:sth:v2:binaries`), but derived the v2 key from the same 32-byte seed — a format migration, not a rotation. #732 rotated to fresh material and republished all three trees under it, and that key is what is pinned today. Were the pin ever absent it could only *withhold* trust — every audit resolving to *unverified*, never `ok` and never `alarm`.

**Honest limits.** (1) *Daily publish lag* — the tree is rebuilt and signed on a schedule, so a brand-new signup or a key rotation is invisible to the log (and to every auditor) until the next publish; a `pending` status, never an alarm, covers that window. (2) *Client checks are advisory* — both commands alert, they never block a send; the app keeps working whether or not the log agrees, exactly as the TOFU layer does. (3) *No private lookups (no VRF)* — `user_id`s and account public keys are enumerable in the tree. This is acceptable because those keys are public by design (they are what every device cert chains to), but it does leak the set of users and their rotation cadence; a VRF-backed private-lookup layer (CONIKS / Key-Transparency style) is the noted upgrade path. (4) *Single first-party log and auditor* — Pollis runs the only log and the only first-party auditor today; the defence against a dishonest operator is that the verifier and `pollis-verify` are released so **anyone** can run an independent auditor, but no third party is contractually watching yet. (5) *CI/GitHub is in the publishing TCB* — the signing key lives in GitHub Actions secrets and the tree is built and signed in CI, so a compromise of the Actions environment could sign a tree; this is the same custody trade-off as the release-signing keys (§3.4), mitigated only in that the post-publish self-audit and equivocation tripwire *detect* (they cannot prevent) a bad head after the fact. (6) *A rotation is indistinguishable from equivocation* — re-signing every published head under a new key changes every signature, which is byte-identical to what a rewriting operator would do. #732's rotation shipped without an overlap window, so any auditor holding cached pre-rotation heads must re-pin from the announcement rather than verify the transition. A small pinned key set with an overlap window, designed before it is next needed, is the fix (#700).

### 6.10 Which suite a group runs, and how a group changes suite

A group's ciphersuite is decided once, at creation, and thereafter changes only by an explicit migration. The answer to a suite question is never "negotiate at runtime".

**Birth.** `CS_PQ`, unconditionally (`init_mls_group`, `pollis-core/src/commands/mls/group_state.rs`). There is one suite, so there is no decision and no code that makes one.

**What the decision used to be, and why it is gone.** Under #454 the two suites ran side by side and `suite_for_new_group` chose between them, behind two gates that both had to hold before a group was born hybrid: a **roster gate** (every registered device of every user on the conversation's desired roster was `pq_capable`) and a deployment-wide **fleet gate** (no unrevoked device seen within a 90-day dormancy window was still classic-only). Capability was *measured, not advertised*: a device was `pq_capable` because it had published a hybrid KeyPackage pool, and the DS set the flag in the same write that landed the pool — no self-declared version string entered the decision anywhere. The fleet gate existed because the roster gate alone was nearly vacuous at creation time: a new group's roster is only its creator, and the first classic-only device invited a second later has no hybrid KeyPackage, so membership reconcile could only *skip* it — leaving it on the roster, never in the tree, permanently unable to read its own conversation. The question a new group actually had to answer was not "can today's roster take hybrid" but "can whoever is invited tomorrow", and the only sound answer was fleet-wide. Both gates failed toward **availability**, and `may_birth_hybrid` (`pollis-core/src/commands/mls/invariants.rs`) was proved exhaustively under Kani alongside a refuted mutant encoding the roster-only version.

**#669 retired the classic suite, and the whole apparatus with it.** With one suite there is no classic-only device to strand, so `suite_for_new_group`, both capability predicates, the dormancy constant, `may_birth_hybrid` and its Kani harnesses, and the DS's `mark_pq_capable` writer are all deleted; `user_device.pq_capable` survives only as a dead column, retired in place because migrations must stay additive. The reasoning above is not withdrawn — it is what a *mixed* fleet requires, and it shipped and worked. What dissolved is its premise: this deployment has no active users, so there was no old app in the field for the gradual path to protect, and no fleet turnover to wait out.

**Migration (`pollis-core/src/commands/mls/migrate.rs`).** MLS has no in-place suite change, so a group moves suite by standing up a **successor group** and moving the roster across by Welcome. `migrate_to_current_suite_if_due` fires for any conversation whose stored suite is not `CS_PQ`. It survives the retirement of the classic suite it was written for because `0x0052` is a provisional code point: a renumber is this same migration with a different constant, and the receiving half of the mechanism — generations, the DS's `(conversation, generation, epoch)` key — is load-bearing either way. The pair `(conversation_id, generation)` names a lineage — generation 0 is the group as originally created — and the monotone key the transparency log and the DS both order by is `(conversation_id, generation, epoch)` lexicographically. Opening generation *N+1* at epoch 0 is accepted by the DS only when the submitter names the head of generation *N* in `closes_epoch` (`pollis_delivery::commit::accepts()`, Kani-proved), so a lineage can be succeeded exactly once and never forked.

**No member is stranded by a migration.** Before anything is created, the migration claims a KeyPackage in the *target* suite for every roster device and aborts on the first miss: move everyone or nobody. A partial move would leave a device on the roster but never in the tree. #454 additionally gated migration on the same two `pq_capable` predicates as birth; #669 removed both reads, because the flag was only ever a *predictor* of this claim, and the claim is the direct test.

**What the transition costs and does not cost.** The successor restarts at MLS epoch 0 with fresh key material that is *not* derivable from the predecessor's, so an adversary holding a leaf stolen before the boundary is evicted at it. Members keep the history they already decrypted (it is stored locally, decrypted, and the migration does not touch it), and a member offline across the boundary drains the retired lineage to its head *before* adopting the successor, so nothing in the window is lost — `max_past_epochs = 0` makes that ordering load-bearing rather than merely tidy. A member whose successor Welcome is lost external-joins the successor instead of stalling on a lineage that will never take another commit. Each of these is a headless `flows` scenario (`src-tauri/tests/flows/pq_migration.rs`), and the model-based fuzzer additionally crosses the boundary under generated membership churn, offline stints, and injected DS faults.

**Honest scope.** Traffic sealed *before* a group migrated was sealed under X25519 and stays that way; a migration is forward-only and cannot retract a recording an adversary already holds. That is exactly why the fleet gate is a completion target rather than a permanent tolerance — the value of the boundary is measured from the moment it is crossed.

---

## 7. Local Encrypted Storage (SQLCipher)

Source: `pollis-core/src/db/local.rs`.

- **Library:** `rusqlite` 0.31 with the `bundled-sqlcipher` feature, which links a vendored SQLCipher 4 (a fork of SQLite providing page-level AES-256-CBC with per-page HMAC-SHA512 for tamper detection; PBKDF2-HMAC-SHA512 page-key derivation is part of the default profile but is not used by Pollis — see "Key application" below).
- **Key application:** `PRAGMA key = "x'{hex}'";` with the 32-byte raw key; this skips SQLCipher's own KDF and uses the raw key directly as the page key — appropriate because the input is a CSPRNG-generated 32-byte uniform key, not a passphrase.
- **Path:** `pollis_{user_id}.db` under the OS-appropriate data dir (Linux `~/.local/share/pollis`, macOS `~/Library/Application Support/com.pollis.app`, Windows `%APPDATA%\pollis`). PRAGMAs: `journal_mode=WAL`, `foreign_keys=ON`.
- **Schema-version semantics:** if `LOCAL_SCHEMA_VERSION` mismatches, the DB file is wiped and recreated. The wipe is *narrow* — it triggers only on missing schema-version row, version-string mismatch, or `SqliteError::NotADatabase` (wrong key). Any other rusqlite error surfaces, refusing to eat the local database on an unfamiliar failure.

### 7.1 What's local-only

- Decrypted message plaintext (`message.content`).
- MLS group state (`mls_kv` rows: epoch state, ratchet tree state, leaf private keys, signature keypairs, KeyPackage private halves).
- Per-device stable MLS signing-key public references (`mls_kv` scope `PollisDeviceSigPub`, one row per signature scheme since #668 — Ed25519 and ML-DSA-44).
- UI/preferences cache.

### 7.2 What's deliberately not local

User profile rows, group/channel metadata, membership, blocks: those live on Turso and are fetched at read time. The argument for this separation is partial-trust: a stolen device with the SQLCipher key cannot enumerate the user's social graph without also being authenticated to Turso (via the read-only `TURSO_TOKEN`, baked into the binary — see §13 for trust caveats).

---

## 8. Remote Database Transport (Turso / libSQL)

Source: `pollis-core/src/db/remote.rs`.

- **Library:** `libsql` 0.6 with the `remote` feature, which uses Turso's **Hrana over HTTP/2** (the libSQL native protocol). The connection URL scheme is `libsql://...`. TLS is mandatory; `libsql` 0.6's `remote` feature uses `rustls` under the hood with the system trust store.
- **Authentication:** a **read-only** bearer `TURSO_TOKEN` baked into the desktop binary's environment (`pollis-core/src/config.rs::Config::from_env`). It is read-only because the client has no write path to Turso at all — every write goes through the Delivery Service, which holds the writing credential. Since #393 the baked token is additionally only a *fallback*: on unlock the client asks the DS to mint a **short-TTL read-only** token and moves `remote_db` onto it, keeping the baked one only if the DS cannot be reached (`pollis-core/src/commands/turso_token.rs`). Per-user authentication is **not** layered on top of either token — every Pollis client reads the same Turso database with the same *class* of credential, and the token is whole-DB rather than row-scoped. Row-level security is enforced at the *application* layer, in Rust commands and in the DS, not by Turso.
- **Resilience:** `RemoteDb::with_retry` handles transient Hrana stream eviction (libsql idle-stream GC) by reconnecting and retrying once. Non-transient errors surface. It is applied to the reads where a dropped stream costs the user something they cannot retry — the ingest envelope fetch, the reconcile key-package read, and the enrollment poll (#914) — not blanket-applied: paths that already degrade gracefully (avatar enrich, username backfill, TOFU key pin) deliberately do not use it.

### 8.1 Threat consequence of a single shared token

A reverse-engineer who extracts `TURSO_TOKEN` from a built binary can open a libSQL connection equivalent to any Pollis client. They can:

- Read every public-metadata table (which is the same threat surface as a server-side database compromise).
- **Not** insert, update or delete anything: the token is read-only, and every write goes through the Delivery Service, which re-derives the actor from a device signature. (This bullet previously described inserting into tables "not protected by application-level checks".) The application-layer rules the DS enforces are:
  - Per-actor permission on group/channel CRUD inside the backend commands (the actor's `user_id` is supplied by the frontend and trusted because the frontend got it from the unlocked `account_id_key`).
  - Atomic claim semantics on `mls_key_package`.
  - `device_cert` cryptographic verification *on the read path*.
- They cannot decrypt any message — those are MLS-encrypted.
- They cannot forge a device into a user's MLS group without that device's cert verifying against the user's `account_id_pub`. The cross-signing check is the floor.

The general shape — a desktop client carrying a credential to talk to backing services, with the cryptographic protocol (not the token) acting as the defence against server compromise — is similar to Signal Desktop, but with a meaningful difference: Signal Desktop holds a *per-account* auth token issued at registration, while Pollis ships a *single shared* read-only `TURSO_TOKEN` in every binary. The shared-token simplification compared to per-account tokens is a known cost; mitigations are in §13.

What that extracted token is **not** is a set of keys to the backing services. It reads Turso and nothing else: it cannot write a row, send an email, read or write an R2 object, or mint a LiveKit token, because the client no longer carries the credentials for any of those (§4, §9.3, §10.1). Every one of those capabilities sits behind the Delivery Service, which re-derives the caller's identity from a device signature rather than trusting the request. Earlier versions of this document described a client that held all four credentials and a `TURSO_TOKEN` that was the smallest of the problems; that is the architecture as it was, not as it ships.

---

## 9. Object Storage (Cloudflare R2)

Source: `pollis-core/src/commands/r2.rs`.

### 9.1 Convergent encryption (attachments)

- **Content hash:** SHA-256(plaintext). Used as the dedup anchor and the input to key derivation.
- **Key/nonce derivation:** HKDF-SHA256 with the content-hash as IKM, `info = b"pollis-att-key"` for the 32-byte AES-256-GCM key and `info = b"pollis-att-nonce"` for a 12-byte base nonce. No salt (the input is already uniformly random for any non-pathological input file).
- **AEAD:** AES-256-GCM (NIST SP 800-38D), 12-byte nonces. The plaintext is split into 4 MiB chunks; each chunk's nonce = `base_nonce XOR LE(u32(chunk_index))` in the first 4 bytes. The chunked construction lets large files stream without buffering, while the per-chunk nonce derivation ensures uniqueness without state.
- **Object key:** `media/{content_hash}/{sanitised_filename}.enc`. Same input → same R2 object; cross-user dedup falls out naturally.

### 9.2 Visibility on R2

R2 sees: opaque AEAD ciphertext, the deterministic object key (which includes the content hash), the size, and the upload time. R2 *does not* see the AEAD key — it never leaves the device.

This is the same shape as MEGA's "Convergent Encrypted" layer (without its block-level dedup) and Tresorit's deduplication scheme. The intentional security trade-off is the **confirmation-of-file attack**: an adversary who already has a candidate plaintext can compute its content-hash and check whether the corresponding R2 key exists. Pollis accepts this trade as the cost of cross-user dedup. A dedicated audit recommendation could replace this with per-conversation key wrapping (drop convergence, lose dedup), if the threat model warrants it.

### 9.3 R2 transport

**The client holds no R2 credentials.** It asks the Delivery Service for a short-lived **presigned URL** (`POST /v1/r2/presign`, via `pollis-core/src/commands/r2.rs::presign_r2`) and then does a plain HTTPS `PUT`/`GET`/`DELETE` against that URL. The SigV4 signing — canonical request → string-to-sign → date-region-service-derived signing key (HMAC-SHA256) → signature — happens **in the DS**, which is where `R2_ACCESS_KEY_ID` and `R2_SECRET_ACCESS_KEY` live (`pollis-delivery/src/broker.rs`). Only `R2_S3_ENDPOINT` and `R2_PUBLIC_URL` — endpoint URLs, not secrets — are compiled into the client (`pollis-core/src/config.rs`).

This is a real reduction, not a relocation: a presigned URL authorises **one operation on one key for a short window**, so extracting a Pollis binary no longer yields the ability to enumerate, overwrite or delete the bucket. For `emoji/…` uploads the DS additionally signs `content-length` into the URL, so R2 itself rejects a body of any other size — a cap the client merely honoured was not a cap (#848). Earlier versions of this document described the R2 keys as baked into the binary under the same shared-credential trust model as `TURSO_TOKEN`; that stopped being true at the #506 secrets-broker cutover, which §1 already recorded and these sections did not.

The `upload_media` command reads files from disk by path inside `pollis-core` (in the Tauri host process), rather than marshalling bytes across the `invoke` IPC boundary, so arbitrary-size attachments do not hit IPC framing limits.

### 9.4 Avatars and group icons

These go through `upload_file` / `download_file` (the non-`upload_media` path) and are **not** encrypted. They are public to anyone with the R2 URL. This is intentional — avatars and group icons are visible to anyone who can see the user/group on Turso anyway, so the additional surface from making them public bytes is zero. It is, however, worth flagging in an audit: an attacker who guesses or scrapes Turso `users.avatar_url` / `groups.icon_url` can fetch the underlying images without authentication. The dedup-via-hash property does not apply to this path.

---

## 10. Real-Time Media (LiveKit)

Source: `pollis-core/src/commands/livekit/`, `voice/`, `realtime.rs`.

### 10.1 Authentication

LiveKit uses room-scoped JWT tokens, **minted by the Delivery Service**:

- HS256, 1-hour validity for participant tokens, 5-minute for admin tokens used by RoomService.
- **The client holds no LiveKit credential.** `LIVEKIT_API_KEY` and `LIVEKIT_API_SECRET` are DS environment (`pollis-delivery/src/broker.rs`); only `LIVEKIT_URL` is compiled into the client. The on-device `livekit_jwt` module that once held the secret has been **deleted**, and the client now calls `POST /v1/livekit/token` (`pollis-core/src/commands/mls/ds_client.rs::ds_livekit_token`).
- This closes the caveat this section used to carry. It is no longer true that "any client can mint any token": the token's `user_id` and `device_id` are derived **server-side from the verified request signature**, so a client cannot mint a token as another user or device, and a reverse-engineer with the binary cannot mint one at all. Room authorisation is still re-derived by the DS rather than by LiveKit, which remains the enforcement point for *which* room a caller may join.

### 10.2 Voice frame-level E2EE

LiveKit is a Selective Forwarding Unit (SFU). The peer-to-SFU hop is encrypted with **DTLS-SRTP** (RFC 5763, RFC 5764) like every WebRTC application, but DTLS-SRTP terminates at the SFU — in a vanilla deployment that means the SFU sees plaintext audio, the same posture Slack Huddles, Microsoft Teams, and Google Meet ship.

Pollis adds a second layer of encryption applied per-frame, post-Opus and pre-SRTP. The cipher is **AES-128-GCM**; the implementation is libwebrtc's native `FrameCryptor` (the same machinery that backs the `livekit-client` JS SDK's `setupE2EE` and Discord's 2024 DAVE protocol). It is wired up via `livekit::e2ee::E2eeOptions { encryption_type: EncryptionType::Gcm, key_provider }` passed into `RoomOptions::encryption` at `Room::connect` time in `pollis-core/src/commands/voice/lifecycle.rs::join_voice_channel`. The SFU still routes the RTP packets — packet headers stay readable — but the payload is opaque ciphertext to anyone without the shared key.

**Key derivation.** The shared 32-byte voice key is exported from the channel's MLS group at the current epoch:

```text
voice_key = MlsGroup::export_secret(
    label = "pollis/voice/v1",
    context = epoch.to_be_bytes(),
    length = 32,
)
```

The MLS group used is the same group that protects the channel's text messages (group channels share their parent group's MLS group; DMs use their `conversation_id` as the MLS group id). Because every current MLS member already holds the exporter secret, every member derives the same voice key without server involvement; non-members and the SFU cannot. The implementation is in `pollis-core/src/commands/voice_e2ee.rs::derive_voice_key`.

**Key rotation.** Whenever the MLS epoch advances — i.e., on any add or remove commit that lands locally — `mls::process_pending_commits_inner` calls `voice_e2ee::on_mls_epoch_changed`, which re-derives the voice key for the new epoch and pushes it into the live `KeyProvider` via `set_shared_key(new_key, new_key_index)`. No reconnect. The `key_ring_size = 16` ring keeps the previous key briefly so in-flight frames decrypt during the changeover. Removed members lose the ability to decrypt subsequent frames because they no longer hold the new epoch's exporter secret; newly added members gain decryption from the moment the new epoch lands.

**Defaults.** `KeyProviderOptions::default()` — PBKDF2 derivation, `LKFrameEncryptionKey` salt, 16-entry key ring, ratchet window 16. These match `livekit-client` JS so a future web or mobile peer that derives its key from the same MLS group can interoperate.

**No opt-out.** Voice E2EE is unconditional. The per-frame AES-GCM overhead is on the order of microseconds in libwebrtc's native cryptor; "sometimes-on" was rejected as a footgun where users might misjudge their threat model.

### 10.3 Audio pipeline (defensive context)

Mic capture: `cpal` in 10 ms i16 mono frames → optional RNNoise (`nnnoiseless`) → WebRTC AudioProcessing module (AGC2 + NS + HPF + AEC, via `webrtc-audio-processing`) → LiveKit `NativeAudioSource.capture_frame` → SRTP. The entire pipeline runs in the Rust core (the Tauri host process); audio never enters the renderer. This is a deliberate architecture choice (cross-platform parity with mobile, and predictable allocation that avoids JS-heap GC pressure on multi-MB media buffers), enforced by the surrounding code and described in `CLAUDE.md`.

### 10.4 Signalling channel

LiveKit data packets carry application-level events: `new_message` (a wake-up; the actual ciphertext is fetched from Turso), `membership_changed`, `enrollment_requested` with the verification code in cleartext (rationale: the verification code is a *human* channel for the user to compare across screens — it's not authenticating; the cryptographic authentication is the ECDH wrap in §5.1). LiveKit operators see all of these. They do not see message ciphertext, MLS state, or any private key material.

---

## 11. Rate Limiting, Block Enforcement, Abuse Surfaces

### 11.1 OTP request rate limiting

Throttling is enforced by the Delivery Service, at two independent scopes:

- **Per email address** (`pollis-delivery/src/otp.rs`): a 30-second resend throttle, and a 5-attempt cap on wrong codes after which the entry is locked out and deleted.
- **Per client IP** (`pollis-delivery/src/ratelimit.rs`): fixed windows over the unauthenticated `request-otp` / `verify-otp` endpoints, keyed on `CF-Connecting-IP`. The per-email limit alone bounds abuse of *one* address; it does nothing about a client spraying requests across thousands of addresses to email-bomb arbitrary mailboxes or burn Resend quota, which is what this second scope covers.
- Resend's own per-domain reputation and per-key limits sit underneath both, as a third layer rather than the only one.

This section previously described the absence of any application-layer throttle as a known gap. That gap closed when the OTP flow moved server-side: the client had no durable, shared place to keep a counter, which is precisely why it could not enforce one.

### 11.2 PIN attempt rate limiting

Local, per-user, capped at 10 then nuke. No backoff. See §3.3.

### 11.3 Enrollment verification code

6 digits, 20 bits, single-use (10-minute TTL on the `device_enrollment_request` row), constant-time compared. Brute-forcing requires writing to Turso (which costs per-attempt latency) and racing the user's confirmation window.

### 11.4 Block enforcement

`user_block` is a directional table (A blocking B does not imply B blocks A) but enforcement is symmetric — both directions are checked at DM creation and at message send (`messages.rs::send_message`'s `suppress_delivery` branch; `blocks::is_blocked_either_way`).

DM block mechanics (deliberately asymmetric in observability):
- The *blocker* sees the conversation disappear from their list (`list_dm_channels` filters by `user_block.blocker_id = me`).
- The *blockee* still sees the conversation. Sending succeeds locally — an entry appears in their local `message` table — but the message is *not* MLS-encrypted, *not* posted to `message_envelope`, and *not* broadcast on LiveKit. The blocker never receives it. The blockee sees no observable signal that they have been blocked.

This is the same observability pattern as Signal/iMessage. The privacy property is: "blocked" is not a backchannel for the blocker to signal anything about themselves to the blockee.

Group-channel blocks are render-side only — the blocker filters out blocked senders client-side, and the encrypted plaintext is still written to `message_envelope` and forwarded over LiveKit. The MLS group is not aware of blocks.

### 11.5 Identity reset (destructive)

`reset_identity_and_recover` (in `device_enrollment.rs`) is the destructive recovery path. It requires:

- A valid OTP for the `users.email` (proven via prior `verify_otp`).
- A constant-time match between user-typed `confirm_email` and stored `users.email`.

It then:

1. Generates a fresh account-identity ML-DSA-44 keypair, bumps `users.identity_version`, replaces the `account_recovery` blob.
2. Deletes the user from every `group_member`, `dm_channel_member`, `mls_key_package`, `conversation_watermark` and `mls_welcome` row, and orphans their other `user_device` rows. Promotes a new admin if the user was sole admin (handing `groups.owner_id` over with the role). Tears down groups and DMs left with nobody in them — explicitly, table by table, because production Turso runs `foreign_keys=OFF` and the schema's `ON DELETE CASCADE` clauses do not fire (`pollis-delivery/src/teardown.rs`). The `users` row and the account's own records survive: this is a reset, not a deletion.
3. Wipes the local SQLCipher DB and its WAL/SHM.
4. Records a `security_event` of kind `identity_reset`.

The cryptographic effect is that every other device the user previously enrolled is *cryptographically* orphaned: their locally-held account-identity private key no longer derives the published `account_id_pub`, so their `device_cert`s no longer verify, so their MLS leaves no longer admit into any new commit. This is the strongest action available without server cooperation, and it requires only the user's email and a working OTP delivery — which is the deliberate "soft recovery" UX.

The audit-relevant property is: an attacker who compromises only the user's email account can mount this against the user. The defence is the user's `security_event` log (visible in the Security settings page) and the catastrophic, observable nature of the attack — every other device the user owned will be locked out the next time it tries to do anything.

---

## 12. Key Material Summary

| Material | Algorithm | Where it lives | Where it does not live |
|---|---|---|---|
| Account identity private | ML-DSA-44 seed (32 B) | Device keystore (`account_id_key_wrapped_{uid}`, AEAD under PIN-derived KEK — and, on the file backend, inside a second AES-256-GCM layer under a machine-bound KEK, §3.5); `AppState.unlock` (Zeroizing, in-process) | Anywhere unwrapped on disk; any server endpoint as plaintext |
| Account identity public | ML-DSA-44 (1312 B) | `users.account_id_pub` (Turso); local `mls_kv` indirectly via leaf nodes | — |
| Secret Key (recovery) | 150-bit Crockford base32 | User's offline backup | Any Pollis-operated system |
| Account recovery wrap key | HKDF-SHA256 → 32 B | Derived on-demand from Secret Key + per-user salt | Stored anywhere |
| Per-device MLS signing private (one per scheme) | ML-DSA-44 seed (32 B) for `CS_PQ` leaves and DS request auth; Ed25519 (32 B), which the retired classic suite's leaves signed with and which is still minted so an older code point stays readable | Local `mls_kv` (under SQLCipher), keyed by signature scheme | Off-device |
| Per-device MLS signing public (one per scheme) | Ed25519 (32 B) / ML-DSA-44 (1312 B) | `user_device.mls_signature_pub` (Ed25519) and `user_device.mls_signature_pub_pq` (ML-DSA-44) on Turso; local `mls_kv`. Both are bound by one v2 `device_cert` | — |
| Device cert | ML-DSA-44 signature (2420 B) by the account identity key over both device signing publics | `user_device.device_cert` (Turso) | — |
| MLS leaf / commit / welcome material | TreeKEM, RFC 9420 | Local `mls_kv` (under SQLCipher) | — |
| MLS HPKE init private | X-Wing = X25519 + ML-KEM-768 decapsulation key | Local `mls_kv` (under SQLCipher) | Off-device |
| Published KeyPackages | Public halves only, one pool in `CS_PQ` | `mls_key_package` (Turso), tagged with its suite and claimed once each | Any private half, ever |
| MLS application secrets | RFC 9420 | Ephemeral, per epoch | Persisted past their epoch |
| DB encryption key (SQLCipher) | 32 random bytes | Device keystore (`db_key_wrapped_{uid}`, AEAD under PIN-derived KEK, §3.5); `AppState.unlock` | Anywhere unwrapped on disk |
| PIN | 4 ASCII digits | User's head | Stored anywhere on disk or wire |
| KEK (PIN-derived) | Argon2id → 32 B | Ephemeral, derived from PIN at unwrap time | Stored anywhere |
| OTP | 6-digit numeric | In-memory **on the Delivery Service** as a salted hash, 10-min TTL, attempt-capped | Stored on disk; held by the client |
| Device enrollment ephemeral X25519 private | X25519 (32 B) | `AppState.enrollment_ephemeral_keys` (in-memory) | Disk, server, anywhere persistent |
| Attachment AEAD key | HKDF-SHA256 over content-hash → 32 B | Derived on-demand from content-hash | Persisted; transmitted to R2 |
| `TURSO_TOKEN` | bearer, **read-only** | Baked into the desktop binary as a fallback; superseded at runtime by a DS-minted short-TTL read-only token (#393). Not user-scoped | Any write capability |
| `LOG_DB_TOKEN` | bearer, read-only | **Optional** observability / commit-log token; baked only when a release supplies one | — |
| `R2_ACCESS_KEY_ID` / `R2_SECRET_ACCESS_KEY` | AWS SigV4 creds | **Delivery Service environment only** | The client binary |
| `LIVEKIT_API_KEY` / `LIVEKIT_API_SECRET` | JWT signing key | **Delivery Service environment only** | The client binary |
| `RESEND_API_KEY` | bearer | **Delivery Service environment only** | The client binary |

---

## 13. Known Gaps and Audit Focus Recommendations

Items below are ordered by adversary cost — easiest first.

1. **Voice E2EE has no end-to-end integration test.** Section 10.2. The frame cryptor wiring (key derivation, key rotation on MLS epoch advance, KeyProvider lifecycle) is covered only by unit-level assertions and manual two-client testing. There is no automated test that spins up a real LiveKit server, sends audio between two harness clients, and asserts the SFU cannot decode the frames. Standing up that harness is the right next step before a third-party audit.
2. **Cross-signing verification is advisory on inbound MLS commits.** Section 5.3 / 6.4 (search for "Inbound cert verification (advisory)" in `mls.rs::process_pending_commits`). A server able to write `user_device` and `mls_commit_log` rows can attempt to insert a rogue device and rely on the warning being unread. The fix requires a quarantine-and-resync state machine for commits with failed cert verification. **Partial mitigation since the group-reconcile TOFU work (refs #277):** the batch `account_id_pub` check (`batch_check_and_pin_account_keys` in `pollis-core/src/commands/safety.rs`) now runs on every reconcile *before* roster devices are added to the MLS tree. A combined attack — fake `user_device` row plus a server-swapped `account_id_pub` — surfaces inline via the `KeyChanged` event (in groups as well as DMs). The advisory state on the device cert itself is unchanged; the new check covers the orthogonal account-key axis. **Further backstopped since the account-key transparency work (#330):** that account-key axis is now also covered by a publicly-auditable, append-only log of every key version (§6.9), so a swap is not only caught live by TOFU but is permanently visible to anyone running `pollis-verify account` — see the residual limits in item 10.
3. **A single shared read-only Turso token is baked into the binary.** Section 8.1. Reverse-engineering the binary yields *read* access to the metadata tables equivalent to any client — not write access, and not the R2, LiveKit or Resend credentials, none of which the client carries any more (#393/#506). Mitigated further by the DS-minted short-TTL token that supersedes it at runtime, by application-layer enforcement in the DS, by MLS-layer cryptographic floors, and by cross-signing. The residual is real: the token is whole-DB rather than row-scoped, so it reads any group's metadata. Row-scoped tokens are the standard fix and are not built. *(This item previously read "single shared Turso/R2/LiveKit/Resend tokens"; three of those four moved off-client, and it is narrowed accordingly.)*
4. **~~No server-side rate limiting on `request_otp`.~~ Closed.** Section 11.1. The DS throttles per email address and per client IP.
5. **Avatars and group icons are public R2 objects.** Section 9.4. Anyone who guesses or scrapes `avatar_url` / `icon_url` from Turso (not directly exposed, but available to any client holding the read-only bearer token) can read them.
6. **PCS healing is launch-driven, not timer-driven.** Section 6.7. Self-updates now fire on join and from the cold-launch sweep at a 7-day (+ up to 2 days jitter) interval, capped at 3 groups per sweep, so idle groups do heal — but only when some member launches the app. A group whose entire membership stays offline does not rotate while they are gone; there is nobody to issue the commit. The per-sweep cap also means a device returning from a very long absence takes several launches to work through a large group list.
7. **No Megolm-style key backup is by design.** Section 6.8. New devices and historical messages from before a member's join are not recoverable. Auditors should *not* report this as a gap unless the requirement statement they're auditing against asks for it; the product principle (`CLAUDE.md`) explicitly accepts it.
8. **Soft-recovery via OTP + email match alone (`reset_identity_and_recover`).** Section 11.5. Compromise of email account ⇒ ability to nuke the user's identity. Visible in the security event log; not preventable with the current factor set.
9. **OTP comparison uses non-constant-time string equality.** Section 4. The compared values are SHA-256 hex digests of a single-use, low-entropy code on a single-shot path; this is a best-practice item rather than a live attack.
10. **Account-key transparency now exists, but with residual limits.** Section 6.9. The account-key directory closes the *systemic* version of the swap attack in items 2 and §11.4 — every user's key history is now publicly auditable and CI self-audits each publish — but four caveats remain. (a) *Daily publish lag*: a rotation is invisible to any auditor until the next publish, so the window between a swap and the next build is covered only by the live TOFU check, not the log. (b) *Advisory, not blocking*: the client `self_audit_account_key` / `audit_peer_account_key` commands alert; they do not block sends. (c) *Enumerable, no VRF*: `user_id`s and public keys are listable in the tree — acceptable, since those keys are public by design, but it leaks the user set and rotation cadence; a VRF-backed private-lookup layer is the upgrade path. (d) *Single first-party log + CI in the TCB*: Pollis runs the only log and signs it in GitHub Actions, so the publishing pipeline is trusted to the same degree as the release-signing keys (§3.4); the mitigation is that `pollis-verify` is released so any third party can run an independent auditor. (e) *Key rotation has no overlap window (#700)*: #732 rotated the log key to fresh material and re-signed all three trees, which changes every published signature and so is byte-indistinguishable from equivocation to anyone holding cached heads. The pin is set and in-app audits resolve normally, but the transition itself was not verifiable — only re-pinnable. None of these reintroduce the original attack — they bound how quickly and by whom it is detected.
11. **Sealed sender is at-rest only; the live DS still sees the sender.** Section 1.2. The metadata-minimization work (sealed sender v1, size padding v2, signalling minimization v2 — all **shipped**) removed the stored sender-per-message artifact, padded text-ciphertext sizes, and stripped `sender_id` from LiveKit `new_message` payloads. The residual axis is a **live** Delivery Service operator: every send still authenticates with the sender's `X-Pollis-User` header (`pollis-delivery/src/auth.rs`), so the DS learns the sender in real time even though the persisted `message_envelope` row does not. Closing this requires anonymous membership proofs (v1.5, `docs/metadata-minimization-design.md`, deferred → #489). Conversation existence, cardinality, and the `user_id`-keyed social graph remain visible (per-conversation membership pseudonyms are the deferred v3, #489). No anonymity or IP-hiding (relay overlay, #455, deferred) is claimed. Post-quantum confidentiality **is** claimed for group traffic (#454, shipped — §6.1, §6.10) and post-quantum authentication alongside it (#668, shipped — §6.1), with the scope stated there: hybrid KEM, ML-DSA-44 signatures on everything but the classic suite's leaves (retired in #669), forward-only from each group's migration boundary.

---

## 14. References

**Core standards**
- RFC 9420 — *The Messaging Layer Security (MLS) Protocol*. Barnes et al., 2023.
- RFC 9180 — *Hybrid Public Key Encryption (HPKE)*. Barnes, Bhargavan, Lipp, Wood, 2022.
- RFC 9106 — *Argon2 Memory-Hard Function for Password Hashing and Proof-of-Work Applications*. Biryukov, Dinu, Khovratovich, Josefsson, 2021.
- RFC 8032 — *Edwards-Curve Digital Signature Algorithm (EdDSA)*. Josefsson, Liusvaara, 2017.
- NIST FIPS 204 — *Module-Lattice-Based Digital Signature Standard (ML-DSA)*. NIST, 2024. The signature scheme behind account identity keys, device certs, DS request auth, transparency-log tree heads, and the hybrid suite's MLS leaves (§6.1).
- NIST FIPS 203 — *Module-Lattice-Based Key-Encapsulation Mechanism Standard (ML-KEM)*. NIST, 2024. The post-quantum half of the hybrid suite's X-Wing KEM (§6.1).
- RFC 7748 — *Elliptic Curves for Security* (Curve25519, X25519). Langley, Hamburg, Turner, 2016.
- RFC 5869 — *HMAC-based Extract-and-Expand Key Derivation Function (HKDF)*. Krawczyk, Eronen, 2010.
- RFC 8439 — *ChaCha20 and Poly1305 for IETF Protocols*. Nir, Langley, 2018.
- IRTF CFRG draft `draft-irtf-cfrg-xchacha` — *XChaCha: eXtended-nonce ChaCha and AEAD_XChaCha20_Poly1305*. Arciszewski, current.
- NIST SP 800-38D — *Recommendation for Block Cipher Modes of Operation: Galois/Counter Mode (GCM) and GMAC*. Dworkin, 2007.
- RFC 5763 / RFC 5764 — DTLS-SRTP. Rescorla, McGrew, 2010.
- AWS SigV4 — *Signing AWS API Requests*. Used by the Delivery Service's presigner (§9.3), not by the client. https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_aws-signing.html.

**Implementations relied upon**
- OpenMLS — https://github.com/openmls/openmls. RustCrypto-backed reference implementation of RFC 9420.
- SQLCipher — https://www.zetetic.net/sqlcipher/. AES-256-CBC + HMAC-SHA512, page-level.
- LiveKit — https://livekit.io/. WebRTC-based SFU. The Rust `livekit` crate's `e2ee::FrameCryptor` (backed by libwebrtc's insertable streams) is enabled with AES-128-GCM and an MLS-exporter-derived shared key — see §10.2.
- `keyring` (Rust) — https://crates.io/crates/keyring. Wraps macOS Keychain Services, freedesktop Secret Service, and Windows Credential Manager.
- Cloudflare R2 — https://developers.cloudflare.com/r2/. S3-API-compatible object storage.

**Comparable products and their cryptographic shapes**
- **Signal / WhatsApp / Messenger Secret Conversations** — Signal Protocol (X3DH + Double Ratchet). Pairwise sessions, Sender Keys for groups. Pollis differs by using MLS, which provides better asymptotic group performance and continuous group authentication. Pollis matches Signal on E2EE messaging and also implements Signal-style 60-digit safety numbers for out-of-band human verification (§6.5), **layered on top of** per-device cross-signing certificates issued by the user's `account_id_pub`. The two mechanisms cover different threats: cross-signing protects the MLS tree from server-injected rogue devices (protocol level); safety numbers + TOFU pinning catch the case where a server (or anyone with Turso write access) swaps a peer's `account_id_pub` itself (account-key level), now additionally backed by a published append-only transparency log of every key version (§6.9), so the swap is not merely caught on the next message but is permanently auditable by anyone. Both run automatically. On every inbound DM message *and* on every group-reconcile commit, the batch TOFU helper (`batch_check_and_pin_account_keys` in `pollis-core/src/commands/safety.rs`) pins first-seen keys and emits a `KeyChanged` realtime event on mismatch — surfacing an inline banner in every conversation the affected peer is in and clearing the verified shield in member lists, DM lists, and channel author labels.
- **Wire / Element X / Webex** — also MLS-based, all using OpenMLS or equivalent. Pollis is in the same cipher-suite tier (suite 1) as the public references for these deployments.
- **Matrix / Element (legacy)** — Megolm + Olm. Adds key backup, which Pollis intentionally does not.
- **Slack / Microsoft Teams** — TLS-in-transit, server-side at-rest encryption, no E2EE on messages or media. Pollis differs categorically: server operators can read Slack/Teams content; they cannot read Pollis messages.
- **Discord** — TLS-in-transit, no E2EE on messages, **DAVE protocol** (MLS for key agreement, SFrame for media frame encryption) provides E2EE for audio and video in DMs, group DMs, voice channels, and Go Live streams as of September 2024. Pollis matches Discord on voice: §10.2 applies AES-128-GCM frame-level encryption via libwebrtc's `FrameCryptor` keyed by an MLS-exporter-derived shared secret, so the LiveKit SFU does not see audio plaintext, the same shape as DAVE. Pollis is *ahead* on messages (Discord chat is plaintext at rest on the server; Pollis chat is MLS-encrypted) and matched on voice.
- **iMessage** — pairwise E2EE per device; per-user multi-device fan-out at send time; iCloud Messages backup historically held by Apple (and therefore subject to Apple's key custody) and only end-to-end encrypted when the user has opted into Advanced Data Protection (iOS 16.2+, December 2022). Pollis differs by using MLS group state instead of pairwise fan-out, and by not implementing any backup mechanism — Pollis has no equivalent to either default-iCloud or ADP-iCloud Messages backup.
- **1Password** — Secret Key + master password, with PBKDF2-HMAC-SHA256 (650k iterations as of 2023) stretching the master password and the Secret Key folded in as additional KDF input. Pollis' Secret Key + PIN combination is shaped similarly in spirit (a user-held high-entropy secret combined with a low-entropy local factor), with two implementation differences: Pollis uses Argon2id rather than PBKDF2 for the local-factor KDF, and Pollis' Secret Key wraps the *account identity key* on the server (HKDF-SHA256 + AES-256-GCM) rather than being mixed into the password KDF. The two roles 1Password merges into one master-password unlock, Pollis splits across the PIN (device unlock) and the Secret Key (server-side recovery wrap).
