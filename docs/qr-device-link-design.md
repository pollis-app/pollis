# QR device link — sign a new phone in from a signed-in desktop (#1207)

Status: implemented in the #1207 PR. This is the reference for the protocol;
the code comments point here.

## What it replaces

Adding a device used to take two separate proofs:

1. **Email OTP** → the DS mints a short-lived *session* (`session.rs`) that
   authorizes the bootstrap writes a pre-credential device needs
   (`register-device`, `enrollment-request`).
2. **Device linking** (`device_enrollment.rs`) → the new device shows an
   8-character SAS derived from its ephemeral X25519 key; a human types it into
   an already-enrolled device, which wraps `account_id_key` to that key.

Step 2 is unavoidable: no server holds the account key, so an enrolled device
must hand it over. Step 1 is not — it only has to prove "the account holder
wants this device". A QR shown by an unlocked, PIN-verified desktop proves the
same thing, and the camera is a better out-of-band channel than a typed code.

## Who plays which role

Both apps play both roles, as device linking already did: an enrolled
**desktop or phone** shows the QR (Settings → Security → Link a new device),
and a new **phone** scans it (or enters the code) while a new **desktop** pastes
the code shown under the QR — desktops have no camera scanner. The creator waits
with `await_device_link` (one awaited call, Rust-side backoff), never a renderer
poll.

## Threat model

Untrusted, as everywhere: the network, the DS, its operator. In particular the
DS must not be able to:

- obtain or substitute the key the account identity is wrapped to;
- turn a link into a session for a *different* account, or into a session that
  can do anything beyond enrolling one device;
- reuse a link, or extend it past its lifetime.

A bystander who photographs the QR must not be able to finish a link without
the desktop user tapping Approve on a request that names their device.

## Protocol

Notation: `H` = SHA-256, `KDF(t, label)` = HKDF-SHA256(ikm = t, salt = none,
info = label), 32 bytes. `t` is a fresh 32-byte random **link token**.

### 1. Desktop creates the link (`create_device_link`)

- Requires the **PIN**, verified by `pin::unlock_inner` (same attempt counter
  and lockout as unlock). There is no way to mint a link without it.
- Derives `claim = KDF(t, "pollis-link-claim-v1")` and
  `mac_key = KDF(t, "pollis-link-mac-v1")`.
- `POST /v1/link/create` (device-signed) `{link_id, claim_verifier = H(claim)}`.
  The DS binds the account from the signature, sets `expires_at = now + 60 s`
  itself, and caps open links per account.
- Shows the QR: `pollis-link:v1:<link_id>:<base64url(t)>`.

The DS stores only `H(claim)`. It never sees `t`, so it can never derive
`mac_key`.

### 2. Phone claims it (`claim_device_link`)

- Parses the payload, derives `claim` and `mac_key` from `t`.
- `POST /v1/link/claim` (pre-credential, rate-limited)
  `{link_id, claim, device_id, device_name}`. The DS checks, in constant time,
  `H(claim) == claim_verifier`, that the link is unexpired and **unclaimed**,
  marks it claimed (single use — a second claim gets the same 401 as a wrong
  one), and mints a **`DeviceLink`-scoped session** for the link's account and
  this `device_id`. The response has the verify-otp shape (user id, username,
  email, `account_id_pub`, session token).
- The phone then runs the normal re-login bootstrap with that session
  (`register-device`) and `start_device_enrollment`, adding
  `link_tag = HMAC-SHA256(mac_key, link_id ‖ request_id ‖ new_device_id ‖ ephemeral_pub)`
  to the enrollment request.

### 3. Desktop approves (`approve_linked_enrollment`)

- Polls `POST /v1/link/status` (device-signed; the link must belong to the
  signer's account) → `{state, request_id, device_name, link_tag}`.
- Fetches the enrollment request (existing read) and **verifies `link_tag`**
  with `mac_key` over the request's `link_id`, `request_id`, `new_device_id` and
  `ephemeral_pub`. A DS that substituted the ephemeral key cannot produce a
  valid tag, because it does not have `mac_key`. This replaces the typed SAS:
  the camera carried the secret, the MAC proves the key came from whoever
  scanned it.
- Shows "**<device name>** wants to sign in" → **Approve** / Reject. On Approve
  it runs the existing wrap (`approve_device_enrollment`'s core, unchanged:
  ECDH → non-contributory check → HKDF → AES-GCM) and records a
  `device_enrolled` security event with `via=qr_link`.

### 4. Phone finishes

Unchanged from linking: poll → unwrap → check against `account_id_pub` → set
PIN (opens the local DB) → finalize (cert + external joins).

## The scoped session

An email-OTP session can also authorize the pre-enrollment **soft reset**
(`/v1/account/rotate-identity` under a session wipes memberships and devices)
and reset-and-recover. A link-minted session must not. `SessionRecord` carries
a `SessionScope`:

- `Otp` — minted by `verify-otp`. Unchanged powers.
- `DeviceLink` — minted by `link/claim`. Accepted **only** by
  `register-device` and `enrollment-request` (and the enrollment read). Refused
  by `gate_or_session*` (rotate-identity, reset-and-recover, every other
  session-or-signature endpoint) and by `establish-identity` (a linked account
  already has an identity).

The refusal is structural: `verify_session` returns the scope, and the gates
that predate links accept `Otp` only. A new session-gated endpoint has to opt
in to `DeviceLink` explicitly.

## Keeping a linked request out of typed-code approval

A link-tagged enrollment request is approved on the link tag by the device
showing that QR; the new device shows no code, so a typed-code approval of it
could never complete. Two places enforce that: the DS leaves link-bound
requests out of `pending_enrollments` (`LinkStore::bound_request_ids`), and the
`enrollment_requested` inbox nudge carries `link_id`, on which clients skip the
typed-code takeover.

## Guardrails

| Guardrail | Where |
|---|---|
| PIN before the QR exists | `create_device_link` → `pin::unlock_inner` |
| 60 s TTL, single use | DS `LinkStore`: `expires_at` set server-side; claim flips `open → claimed` atomically |
| Explicit Approve naming the device | desktop link page; core refuses to wrap without a verified `link_tag` |
| Photographed QR is useless alone | claiming yields only a `DeviceLink` session; the key moves only on Approve |
| Audit | `device_enrolled` security event, `via=qr_link` |
| Email sign-in stays | the OTP path is untouched |

## The tradeoff, stated

Before: adding a device needed the email inbox **and** an unlocked enrolled
device. After: an unlocked desktop **plus its PIN** is enough — the
WhatsApp-Web / Signal linked-device model. The email path remains for anyone
without another device.

## Storage

Links live in the DS's in-memory `LinkStore` (the DS is single-container, like
the OTP and session stores). A claimed link stays readable until its enrollment
request resolves or the session TTL passes, then is swept. A container restart
drops open links; the user scans a fresh QR. No migration.
