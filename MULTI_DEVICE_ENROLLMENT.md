# Multi-device Enrollment & Account Identity

Implementation plan for adding a second (or Nth) device to an existing
Pollis account without requiring the user to have another device online
in the same physical location.

This document is the source of truth for the design. If the code diverges
from this doc, fix the code or update this doc — don't let them drift.

## Progress

- [x] Step 1 — Migration 13 + truncate
- [x] Step 2 — First-device account identity generation
- [x] Step 3 — Device cross-signing (outbound)
- [x] Step 4 — `publish_group_info` + `mls_group_info` upkeep
- [x] Step 5 — New-device enrollment, approval path (backend)
- [x] Step 5 — frontend (gate, Secret Key save, approval prompt)
- [x] Step 6 — Secret Key path + external commits
- [x] Step 7 — Soft recovery (`reset_identity`)
- [x] Step 8 — Security events page
- [x] Step 3b — Inbound cert verification in `process_pending_commits_inner`

## Problem

Today, MLS group membership is per-device, and a new device is only
invited to existing groups at the moment of group creation via
`add_member_mls_for_own_devices`. If a user creates a group on Device A
and later signs in on Device B, nothing in the system ever invites
Device B to the existing MLS groups. Device B can sign in, but cannot
send or decrypt in any group that existed before it was registered.

Symptom in prod:
```
[messages] MLS group <id> missing locally — messages will be encrypted
until a Welcome is received
```

## Product principles

1. **Messages are not expected to live everywhere forever.** History
   sync on a new device is explicitly not a goal. Losing old messages
   when enrolling a new device is acceptable. See `CLAUDE.md` "Product
   Principles".
2. **Desktop-first means "no phone assumption".** A new laptop at a
   new location with no access to the previous machine must be able to
   enroll. Any design that requires another active device in the room
   fails this test.
3. **Server never sees plaintext or private keys.** The server may
   hold ciphertext and wrapped blobs, but must never be able to
   decrypt message content or impersonate a user.
4. **Seamless UX for normal operation.** No identity-change warnings
   for contacts, no friction for the common case. Security
   breadcrumbs belong in a Security settings page, not in the chat
   stream.
5. **Simpler > history-preserving.** When choosing between a simpler
   model that loses messages and a complex one that preserves
   history, pick simpler.

## Core design

### The account identity key

Each user has a long-lived Ed25519 **account identity key**
(`account_id_key`), generated once at first-device signup. This key is
*not* a device key — it represents "the human" and is what lets any
device of that user prove it legitimately belongs to them.

- `account_id_key.public` is published to `users.account_id_pub`.
- `account_id_key.private` is stored in each of the user's devices'
  OS keystores and also server-side in an encrypted "recovery blob".

### Device cross-signing

Every device the user owns publishes a **device cert**: a signature by
`account_id_key` over the device's MLS signing public key, device id,
identity version, and an issued-at timestamp. The cert lives in a new
column on `user_device`.

Every client, before accepting any Add commit or external-join commit
into an MLS group, verifies the target device's cert chains to the
user's `account_id_pub`. This means:

- The server cannot silently inject ghost devices into any group, even
  if it controls the MLS key package publishing flow.
- Any device of the user can authorize any other device of the user
  (because every device holds `account_id_key`).

### The Secret Key and the recovery blob

At signup the client generates a 140-bit random **Secret Key**,
formatted as `A3-XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX` (base32, no
ambiguous characters). The Secret Key is shown to the user exactly
once. It is never stored on the server.

The client derives a wrap key from the Secret Key via HKDF-SHA256 (no
Argon2 — 140 bits of entropy is already uncrackable). It encrypts
`account_id_key.private` with XChaCha20-Poly1305 under the wrap key
and uploads the ciphertext to Turso as the **recovery blob**
(`account_recovery` table).

The Secret Key is only needed when enrolling a new device on which no
other approach is available. It is never needed during normal
operation.

### Two enrollment paths

When a device signs in on a user that already has an
`account_id_pub` on the server but does not yet hold
`account_id_key` locally, the device is in the "new device enrollment"
state and is gated from the rest of the app until one of two paths
completes:

1. **Approval from another device (preferred).** The new device posts
   an enrollment request. Any existing online device of the same user
   sees the request via the LiveKit user inbox, prompts the user to
   approve, and on approval HPKE-encrypts `account_id_key.private` to
   the new device's ephemeral public key. The new device decrypts,
   stores `account_id_key` in its keystore, signs its own device cert,
   and receives MLS Welcomes for all the user's groups/DMs (which the
   approving device issued as part of approval).

2. **Secret Key recovery (fallback).** The user enters their Secret
   Key on the new device. The device fetches the recovery blob,
   unwraps `account_id_key` locally, stores it, signs its own device
   cert, and joins every existing group/DM via MLS **external
   commits** using the `GroupInfo` blobs the server maintains
   per-group.

Both paths end in the same state: the new device holds
`account_id_key`, has a valid published device cert, and is a full
member of every group/DM the user belongs to.

### Soft recovery (forgotten Secret Key, no active device)

If the user has lost their Secret Key and has no device holding
`account_id_key`, they can reset their identity using only OTP as
proof:

1. Client generates a fresh `account_id_key` and a fresh Secret Key.
2. Client bumps `users.identity_version` and overwrites
   `users.account_id_pub` + `account_recovery`.
3. User sees the new Secret Key once (same emergency-kit flow as
   signup).
4. Old devices that were still holding the previous `account_id_key`
   self-invalidate on next startup by comparing local vs server
   `identity_version` and wipe their local MLS state.
5. The user is orphaned from all their prior groups/DMs. They must be
   re-added by existing members. Prior messages are unrecoverable on
   every device.

This matches the product principle: reset is cheap, messages are
ephemeral, recovery is a clean slate.

**Noted tradeoff:** contacts are not shown any "identity changed"
warning when a reset happens. Silent email compromise can therefore
lead to an invisible takeover until the legitimate user notices they
are logged out. A `security_event` row is written on every reset and
surfaced in a Security settings page, providing a breadcrumb the user
can check but never an in-chat interruption.

## User-facing flows

### Flow A — First-device signup (brand-new account)

1. User signs in with OTP.
2. Client detects `users.account_id_pub IS NULL` for this user.
3. Client generates `account_id_key` and Secret Key, wraps and
   uploads recovery blob, publishes `account_id_pub`, stores
   `account_id_key` in OS keystore.
4. **Route gate — "Save your Secret Key" screen.** Big scary warning:
   "If you lose this key and all your devices, your account is
   unrecoverable and you will lose access to all your messages." Shows
   the Secret Key, offers a PDF emergency kit download, and requires
   the user to type it back to confirm they've saved it.
5. Main app unlocks.

### Flow B — New device, approval path

1. User signs in with OTP on new device.
2. Client detects `users.account_id_pub` exists but
   `account_id_key` is not in local keystore.
3. **Route gate — "Enroll this device" screen.** Visually distinct
   from the OTP screen (different layout/color/heading — see
   "Decisions") so the user can't confuse the two. Two CTAs:
   - Primary: "Approve from another device"
   - Fallback: "Use my Secret Key"
4. User picks "Approve from another device". Client generates an
   ephemeral X25519 keypair and a 6-digit verification code, inserts a
   `device_enrollment_request` row, publishes
   `{type:"enrollment_requested", ...}` to `inbox-{user_id}` via
   LiveKit.
5. On any of the user's other online devices, the LiveKit listener
   receives the payload and **immediately** takes over the UI with an
   approval prompt (this is intentional and loud — if it's not you,
   you want to see it instantly). The prompt shows the verification
   code the new device is displaying and asks the user to confirm it
   matches, then Approve/Reject.
6. On Approve, the existing device:
   - Verifies the code matches the request row.
   - HPKE-encrypts `account_id_key.private` to the request's
     ephemeral pub, writes it to `wrapped_account_key`.
   - Signs a device cert for the new device and writes it to
     `user_device`.
   - For each group/DM the user is a member of locally, calls
     `add_member_mls` to add the new device as a member (issues MLS
     Add commit + Welcome, publishes updated `GroupInfo`).
   - Writes `security_event` row: `device_enrolled`.
7. New device is polling `device_enrollment_request.status`. On
   `approved`, it unwraps `wrapped_account_key` with its ephemeral
   private key, stores `account_id_key` in keystore, finalizes
   enrollment (generates MLS signing keypair, publishes device cert —
   already signed by approver — polls welcomes).
8. Main app unlocks. User's existing messages are visible going
   forward (history in the strict sense is not synced, per principle
   1).

### Flow C — New device, Secret Key path

1. Same as Flow B steps 1–3.
2. User picks "Use my Secret Key". Enters the key.
3. Client fetches `account_recovery` row, HKDFs the wrap key,
   decrypts `account_id_key`, stores in keystore.
4. Client generates MLS signing keypair, calls `sign_device_cert`
   with its own freshly-loaded `account_id_key`, publishes device
   cert to `user_device`.
5. For each group/DM in the user's membership list, client fetches
   `mls_group_info` and constructs an **MLS external commit** joining
   the group. Posts each commit to `mls_commit_log` and publishes
   updated `GroupInfo`.
6. Other members process the commits on next online cycle, verify
   the new device's cert chains to `account_id_pub`, merge.
7. Writes `security_event` row: `device_enrolled` (kind includes
   `via=secret_key`).
8. Main app unlocks.

### Flow D — Soft recovery (lost Secret Key, no devices)

1. User signs in with OTP. No local `account_id_key`, no working
   approval path, user reports they've lost their Secret Key.
2. Route gate offers a "Reset my identity" action with severe
   warning language: "This will remove you from all groups you were
   in. All previous messages become unreadable on every device. You
   will need to be re-added to groups by other members."
3. User confirms by typing their email.
4. Client calls `reset_identity`: generates fresh `account_id_key`,
   fresh Secret Key, bumps `users.identity_version`, overwrites
   `account_id_pub` and `account_recovery`, writes
   `security_event: identity_reset`.
5. Shows new Secret Key with same emergency-kit flow as Flow A.
6. Main app unlocks in a fresh-account state (no group memberships).
7. Any other previously-logged-in device, on its next startup,
   notices `identity_version` mismatch and wipes its local MLS state
   + logs out.

## Schema changes

All new SQL goes in a numbered migration file at
`src-tauri/src/db/migrations/000013_account_identity_and_enrollment.sql`.
`remote_schema.sql` is frozen and must not be modified (see
`CLAUDE.md`).

```sql
-- ── Account identity ────────────────────────────────────────────────
ALTER TABLE users ADD COLUMN account_id_pub BLOB;
ALTER TABLE users ADD COLUMN identity_version INTEGER NOT NULL DEFAULT 1;

-- ── Device cross-signing ────────────────────────────────────────────
-- device_cert = account_id_key.sign(
--     device_id || mls_signature_pub || issued_at || identity_version
-- )
ALTER TABLE user_device ADD COLUMN device_cert BLOB;
ALTER TABLE user_device ADD COLUMN cert_issued_at TEXT;
ALTER TABLE user_device ADD COLUMN cert_identity_version INTEGER;
ALTER TABLE user_device ADD COLUMN mls_signature_pub BLOB;

-- ── Recovery blob (wrapped account_id_key private material) ────────
CREATE TABLE account_recovery (
    user_id          TEXT PRIMARY KEY,
    identity_version INTEGER NOT NULL,
    salt             BLOB NOT NULL,
    nonce            BLOB NOT NULL,
    wrapped_key      BLOB NOT NULL,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);

-- ── GroupInfo blobs (for Secret Key path's external-commit join) ────
-- Updated after every epoch change by any member.
CREATE TABLE mls_group_info (
    conversation_id      TEXT PRIMARY KEY,
    epoch                INTEGER NOT NULL,
    group_info           BLOB NOT NULL,
    updated_at           TEXT NOT NULL,
    updated_by_device_id TEXT NOT NULL
);

-- ── Device enrollment requests (inbox-approval flow) ───────────────
CREATE TABLE device_enrollment_request (
    id                       TEXT PRIMARY KEY,
    user_id                  TEXT NOT NULL,
    new_device_id            TEXT NOT NULL,
    -- Ephemeral X25519 pub the new device publishes for HPKE-wrapping
    -- account_id_key
    new_device_ephemeral_pub BLOB NOT NULL,
    -- 6-digit code shown on both screens for user confirmation
    verification_code        TEXT NOT NULL,
    -- Filled by approver: HPKE(account_id_key.private) to ephemeral pub
    wrapped_account_key      BLOB,
    status                   TEXT NOT NULL
        CHECK (status IN ('pending','approved','rejected','expired')),
    created_at               TEXT NOT NULL,
    expires_at               TEXT NOT NULL,
    approved_by_device_id    TEXT,
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);
CREATE INDEX idx_enrollment_user_pending
    ON device_enrollment_request(user_id, status)
    WHERE status = 'pending';

-- ── Security event log (breadcrumbs for the user) ──────────────────
CREATE TABLE security_event (
    id         TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL,
    -- 'device_enrolled', 'device_rejected', 'identity_reset',
    -- 'secret_key_rotated'
    kind       TEXT NOT NULL,
    device_id  TEXT,
    created_at TEXT NOT NULL,
    metadata   TEXT,
    FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);
CREATE INDEX idx_security_event_user
    ON security_event(user_id, created_at DESC);

INSERT INTO schema_migrations (version, description) VALUES
    (13, 'account identity, device cross-signing, enrollment, group info, security log');
```

**One-time migration action.** Migration 13 truncates the existing
data that would conflict (users, user_device, mls_key_package,
mls_welcome, mls_commit_log, groups, channels, dm_*, message_envelope,
etc.) as its first step, before applying the ALTER TABLE and
CREATE TABLE statements. Everyone re-signs up. This avoids writing
any backfill logic for a schema that is evolving at the identity
layer.

**OS keystore additions** (per-user, via `keystore::store_for_user`):
- `account_id_key` — Ed25519 private key bytes for the account
  identity.
- `mls_device_signature_key` — the per-device MLS signing keypair
  (currently only inside `mls_kv`; surface it so cross-signing has
  something explicit to sign).

## Backend changes

### New module: `src-tauri/src/commands/account_identity.rs`

| Command / function | Purpose |
|---|---|
| `generate_account_identity(state, user_id) -> String` | First-device signup only. Generates `account_id_key`, Secret Key, HKDF → wrap key, XChaCha20-Poly1305 wraps private key, uploads `account_recovery`, sets `users.account_id_pub` + `identity_version=1`, stores key in OS keystore. Returns the formatted Secret Key for display. |
| `has_local_account_identity(state, user_id) -> bool` | Does this device hold `account_id_key` in its keystore? |
| `sign_device_cert(state, user_id, device_id, mls_sig_pub) -> Vec<u8>` | Loads `account_id_key` from keystore, produces signature. |
| `verify_device_cert(account_id_pub, device_id, mls_sig_pub, identity_version, cert) -> bool` | Standalone verifier. Used by every client before accepting a new device in a group. |
| `rotate_secret_key(state, user_id) -> String` | Generates new Secret Key, re-wraps existing `account_id_key` (no identity rotation). Overwrites recovery row. Writes `secret_key_rotated` event. |
| `reset_identity(state, user_id) -> String` | Soft recovery. Generates fresh `account_id_key`, bumps `identity_version`, overwrites recovery row and `users.account_id_pub`. Writes `identity_reset` event. Does NOT touch existing MLS groups. |

### New module: `src-tauri/src/commands/device_enrollment.rs`

| Command / function | Purpose |
|---|---|
| `start_device_enrollment(state, user_id) -> EnrollmentHandle` | New-device side. Generates ephemeral X25519 keypair + 6-digit code. Inserts `device_enrollment_request` with 10-minute TTL. Publishes `{type:"enrollment_requested", request_id, verification_code, new_device_id}` to `inbox-{user_id}` via LiveKit. Returns `{request_id, verification_code, expires_at}`. |
| `poll_enrollment_status(state, request_id) -> EnrollmentStatus` | New-device polls every ~2s. On `approved`, unwraps `wrapped_account_key` via the stored ephemeral X25519 private key, stores `account_id_key` in keystore, calls `finalize_enrollment`. |
| `list_pending_enrollment_requests(state, user_id) -> Vec<EnrollmentRequest>` | Existing-device side. Returns open requests. Backup for the LiveKit push. |
| `approve_device_enrollment(state, request_id, verification_code) -> ()` | Existing-device side. Verifies code. HPKE-encrypts `account_id_key.private` to the requester's ephemeral pub. Signs a device cert for the new device. Writes `wrapped_account_key`, `approved_by_device_id`, `status='approved'`. For every group/DM the approver is in locally, calls `add_member_mls` (single-device variant) to add the new device and publish updated `GroupInfo`. Writes `device_enrolled` event. |
| `reject_device_enrollment(state, request_id) -> ()` | Sets status='rejected'. Writes `device_rejected` event. |
| `recover_with_secret_key(state, user_id, secret_key_input) -> ()` | New-device side, Secret Key path. Fetches `account_recovery`, derives wrap key via HKDF, decrypts, stores `account_id_key`, calls `finalize_enrollment`. |
| `finalize_enrollment(state, user_id) -> ()` | Private helper. Generates MLS signing keypair, signs device cert locally, publishes device cert + `mls_signature_pub` to `user_device`. If the user already has existing groups the device isn't in (Secret Key path), joins each via `external_join_group`. Otherwise (approval path), polls welcomes. |

### Modifications to existing modules

**`src-tauri/src/commands/auth.rs`**

- `verify_otp` and `dev_login_inner`: after `register_device`, branch
  on `users.account_id_pub`:
  - `NULL` → first-device-ever signup. Call `generate_account_identity`,
    include the Secret Key in the returned `UserProfile` (new optional
    field `new_secret_key: Option<String>`).
  - present AND `has_local_account_identity` → normal returning device.
  - present AND `!has_local_account_identity` → new device. Include
    `enrollment_required: true` in the returned profile. Frontend
    routes to the enrollment gate.
- `initialize_identity`: stop unconditionally polling welcomes. Only
  poll once the device is enrolled (has keystored `account_id_key`
  and a valid `device_cert`).
- `register_device`: if the device already holds `account_id_key` AND
  `user_device.device_cert` is missing/stale
  (`cert_identity_version != users.identity_version`), sign a fresh
  cert and update the row.

**`src-tauri/src/commands/mls.rs`**

- Add `parse_credential_device_id` next to `parse_credential_user_id`.
- Refactor `add_member_mls_impl` to accept an optional explicit
  `Vec<(user_id, device_id)>` instead of "all devices for user except
  excluded". Current callers pass `None` and keep today's behavior;
  enrollment passes a single pair.
- Before constructing the Add commit, for each target device: fetch
  `account_id_pub` + `device_cert` + `mls_signature_pub` +
  `cert_identity_version` from Turso, verify with
  `verify_device_cert`. Refuse to add on failure.
- After any epoch-changing operation (`init_mls_group`,
  `add_member_mls_impl`, `remove_member_mls_impl`), call a new
  `publish_group_info(state, conversation_id)` which serializes the
  current `GroupInfo` and upserts `mls_group_info`.
- New `external_join_group(state, user_id, conversation_id)`:
  1. Fetch `mls_group_info` for the conversation.
  2. Build `CredentialWithKey` from `make_credential(user_id, device_id)`.
  3. Call openmls `MlsGroup::join_by_external_commit`.
  4. Merge the resulting commit locally, post to `mls_commit_log`.
  5. Publish updated `GroupInfo`.
- `process_pending_commits_inner`: on every Add / external-join
  commit, identify the new leaf's `(user_id, device_id)`, fetch its
  `device_cert` + `account_id_pub` from Turso, verify. On failure,
  reject the commit (do not merge). **This is the critical check that
  prevents the server from injecting ghost devices.**

**`src-tauri/src/commands/groups.rs::create_group` and
`src-tauri/src/commands/dm.rs::create_dm_channel`**

- Keep `add_member_mls_for_own_devices` — still needed for
  multi-device users creating new groups.
- After `init_mls_group`, call `publish_group_info`.
- The refactored `add_member_mls_impl` now verifies device certs
  before adding.

**`src-tauri/src/commands/messages.rs`**

- The error in `send_message` when no local MLS group exists becomes:
  `"device not enrolled — complete enrollment before sending"`.
  Should be unreachable if the enrollment gate is correctly wired,
  but keep as defense.
- The `eprintln!` warning in `get_channel_messages` at line 336
  becomes an `error!` because it now represents a bug, not an
  expected transient state.

**`src-tauri/src/commands/livekit.rs`**

- Reuse the existing `publish_to_user_inbox`. No code changes;
  enrollment just sends a new payload type (`enrollment_requested`).

### New Cargo dependencies

- `hpke` (or `crypto_box`) for HPKE-wrapping `account_id_key.private`
  to the new device's ephemeral pub in the approval flow.
- `chacha20poly1305` — probably already pulled in transitively by
  openmls; verify before adding.
- `hkdf` — same, verify before adding.
- **No Argon2.** 140 bits of Secret Key entropy is already
  uncrackable; a password-style KDF is not needed.

### Tauri command registration (`src-tauri/src/lib.rs`)

New commands to register:

```
generate_account_identity
has_local_account_identity
rotate_secret_key
reset_identity
start_device_enrollment
poll_enrollment_status
list_pending_enrollment_requests
approve_device_enrollment
reject_device_enrollment
recover_with_secret_key
```

## Frontend changes (summary)

Not the focus of this doc, but the backend shape depends on these
assumptions holding:

1. **Post-OTP route gate.** A new routing state between "OTP
   succeeded" and "main app". States:
   `first_device_show_secret_key` → `enrollment_required` →
   `enrolled`. Nothing in the main app (groups, DMs, messages)
   renders until `enrolled`.
2. **First-device Secret Key screen.** Big scary warning, code
   display, PDF download of an emergency kit, type-back confirmation.
   Only one shot.
3. **New-device enrollment screen.** Two CTAs: "Approve from another
   device" (primary, renders verification code + polls) and "Use my
   Secret Key" (text field). **This screen must be visually
   distinct from the OTP screen** — different layout, heading, and
   color treatment — so a returning user does not confuse the two and
   assume they entered the OTP wrong. Only really matters on first
   multi-device use but is still required.
4. **Existing-device enrollment approval — immediate takeover.** The
   LiveKit inbox listener for `enrollment_requested` **interrupts the
   current UI** with a full-screen approval prompt. The user should
   see instantly that another device is trying to pair. The prompt
   shows the verification code, asks them to confirm it matches the
   other screen, and offers Approve / Reject. No modal (per
   `CLAUDE.md`) — use the full-page pattern or the chat-input-bar
   replacement pattern.
5. **Security settings page.** Simple list of `security_event` rows.
   No pings, no badges — the user opens it if they want to audit.

## Decisions

| Question | Decision |
|---|---|
| Secret Key display format | `A3-XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX-XXXXXX` (base32, 6 groups, unambiguous alphabet). A rare-use flow, memorability is not a driver. |
| Enrollment request TTL | **10 minutes.** Enough time for a user to walk to another device, short enough to limit exposure. |
| Existing-device approval UX | **Immediate takeover.** Interrupts whatever the user is doing. If it's not them, they must see it instantly. |
| Pre-production data migration | **Truncate the DB.** Do not write backfill logic. |
| Verification code format | **6 digits.** Familiar, fast, fine for once-per-device use. Enrollment screen MUST be visually distinct from the OTP screen so users don't mistake one for the other. |
| Contact-facing identity-change warnings | **None.** `security_event` log row only. Tradeoff accepted: silent email-based takeover is possible; user must audit the Security page if they suspect compromise. |

## Implementation order

Land in this order so every step is individually shippable and each
step unlocks the next:

1. **Migration 13 + truncate.** Schema lands, existing data wiped. No
   behavior change yet.
2. **Account identity (first-device only).** Generate
   `account_id_key` + Secret Key at signup, store in keystore,
   publish `account_id_pub`, upload recovery blob. Show Secret Key
   screen. Does not affect existing flows because every user on this
   code path is first-device.
3. **Device cross-signing — outbound.** Every device publishes a
   `device_cert`. `add_member_mls_impl` verifies each target
   device's cert against the user's `account_id_pub` before issuing
   the Add commit. This prevents a legitimate user's own client from
   being tricked into adding a ghost device whose key package was
   injected server-side. Also refactors the four ad-hoc
   `SignatureKeyPair::new` call sites in `mls.rs` to use a single
   stable per-device MLS signing key so one cert covers every key
   package the device will ever ship.
3b. **Device cross-signing — inbound.** Extend
    `process_pending_commits_inner` to verify certs on Add and
    external-join commits before merging. Non-trivial because the
    cert lookup is async but the staged commit lives inside the
    local_db sync scope, and `process_message` may advance per-epoch
    decryption state so naive re-processing after an async verify is
    unsafe. Likely implementation: have every commit writer also
    insert a `mls_commit_metadata` row listing the `(user_id,
    device_id)` pairs being added, which the receiver can verify
    async before calling `process_message`.
4. **`publish_group_info` and `mls_group_info` table upkeep.**
   Called after every epoch change. Required for step 5 but useful
   on its own for debugging.
5. **New-device enrollment, approval path only.** The LiveKit-based
   inbox-approval flow. At this point, users can multi-device as
   long as they have another active device. Fixes the original
   bug.
6. **New-device enrollment, Secret Key path.** Requires
   `external_join_group` to be implemented (MLS external commits).
   Unlocks the "new laptop, no old device" case.
7. **Soft recovery (`reset_identity`).** Last, because it is the
   lowest-frequency flow and benefits from everything above being
   stable.
8. **Security settings page.** Frontend-only, displays
   `security_event` rows.

Steps 1–3 are essentially standalone and can be reviewed/merged
independently. Step 5 is the one that closes out the original prod
bug. Steps 6–7 are the "proper" finish line.
