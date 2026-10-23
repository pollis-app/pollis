# QR device link — sign a device in by QR, in either direction

Status: v1 (#1207, the signed-in device shows the QR) and v2 (#1361, the NEW
device shows the QR, the signed-in phone scans it) are both implemented. This
is the reference for both protocols; the code comments point here. v1 is
described first, as it shipped; [v2](#v2--the-new-device-shows-the-qr-1361)
follows.

## Which one a person meets

| New device | Signed-in device | Flow |
|---|---|---|
| desktop | phone | **v2**: the desktop's "Sign in with another device" shows a QR; the phone scans it under Security → Scan to sign in a device |
| phone | phone | **v2**: the new phone taps "Show a code instead"; the signed-in phone scans it |
| phone | desktop | **v1**: the desktop shows a QR under Security → Link a new device; the new phone scans it |
| desktop | desktop | **v1**: the new desktop pastes the code shown under the signed-in desktop's QR (desktops have no scanner) |

#1207 asked for the Steam model, in which the device being signed in shows the
code and the signed-in phone scans it, and built the opposite. On a desktop that
left only pasting a ~70-character code read off a phone. v2 is the Steam
direction. The phone no longer *shows* a v1 code: a phone scans far better than
it is scanned, and the only way its code ever reached a desktop was by being
retyped.

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

An enrolled **desktop** shows the QR (Settings → Security → Link a new device).
A new **phone** scans it, or enters the code, and a new **desktop** pastes the
code shown under the QR, since desktops have no camera scanner. Until #1361 an
enrolled phone could show one too; that screen is now the v2 scanner. The
creator waits with `await_device_link` (one awaited call, Rust-side backoff),
never a renderer poll.

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
typed-code takeover. Desktop reads the nudge through the core's
`dispatch_data`, which must forward `link_id` in `RealtimeEvent::EnrollmentRequested`;
until it did, desktop's skip never fired and a phone scanning a desktop's QR
raised "A new device wants to enroll" on that desktop, asking for a code the
phone never shows. Mobile parses the packet itself and was unaffected.

Skipping on `link_id` is not an approval bypass. The flag only withholds a
prompt, which a DS can always do by not sending the nudge at all. It approves
nothing: `approve_device_enrollment` still requires the typed code for every
request it is given, and a link request is wrapped only by the device that
holds that link, after the core checks the link tag (v1, keyed by the QR's
secret, which the DS never sees) or the scanned key (v2). A DS that set
`link_id` on a genuine typed-code request could hide the prompt, but could not
get its key wrapped.

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

## v2 — the new device shows the QR (#1361)

### Threat model, additionally

Everything above still holds. v2 also has to keep these true:

- A photo of the new device's QR must grant nothing: not a session, and not a
  way to be the device the account key is wrapped to.
- Someone who gets a signed-in user to scan a QR from *their* device
  (QR-login phishing, the attack Discord's QR login is known for) is the risk
  this direction adds. The approving side's PIN and the explicit confirmation
  copy are the defence; see the tradeoff below.

### Protocol

`E = (e, E_pub)` is a fresh X25519 keypair. `p` is a fresh 32-byte **poll
secret**. Neither the private key nor the poll secret is ever in the QR.

1. **New device offers** (`create_device_offer`). It generates `E`, `p`, a link
   id and a device id, and sends `POST /v1/link/offer` (pre-credential, rate-limited in the
   `probe` tier) `{link_id, poll_verifier = H(p), device_id, device_name}`. The
   DS sets `expires_at = now + 120 s` itself and caps how many unaccepted
   offers it holds. The device shows
   `pollis-link:v2:<link_id>:<base64url(E_pub)>`, and when the code lapses it
   shows a fresh one (five times on its own, then on a tap).
2. **Signed-in device accepts** (`accept_device_offer`). It scans the code and
   parses `link_id` and `E_pub`, then requires the **PIN**
   (`pin::verify_pin`, same counter and lockout as unlock) before anything is
   sent. `POST /v1/link/accept-offer` (device-signed) binds the offer to the
   signer's account, **once**: a second accept, from any account, is refused,
   so an offer can never be moved between accounts. The device keeps `E_pub`
   in memory (`device_offer_accepts`).
3. **New device collects** (`await_device_offer`, one awaited Rust call with
   backoff, bounded by the offer's lifetime). It polls `POST /v1/link/poll-offer`
   `{link_id, p}` (pre-credential, `read` tier). Until the offer is accepted the
   answer is "waiting". Once it is accepted, the DS mints a **`DeviceOffer`-scoped
   session** for the accepting account and the offered `device_id`, exactly
   once. The device signs in with it (`register-device`), then
   `start_device_enrollment` files its request with **`E`, the key from the
   QR**, and no link tag.
4. **Signed-in device wraps** (`finish_device_offer`). It follows the link on
   the existing `/v1/link/status` until the request is there, reads it, and
   wraps `account_id_key` only if the request's `new_device_ephemeral_pub` is
   **byte for byte `E_pub` as scanned** (constant-time). That check takes the
   place of v1's link tag. It runs the same `wrap_and_approve` as every other
   approval, and records `device_enrolled` with `via=qr_scan`. There is no
   separate Approve tap: the person already decided when they scanned the
   device in front of them and entered their PIN, and only the holder of `e`
   can open the wrap.
5. **New device finishes** as in every other flow: unwrap, check against
   `account_id_pub`, set the PIN, finalize.

### Why this is at least as strong as v1

| Property | v1 | v2 |
|---|---|---|
| Key substitution by the DS | Prevented by the MAC; the DS never sees `t` | Prevented outright: the wrap goes only to the key the camera read |
| DS plus someone who photographed the QR | **Can** substitute: the photo gives `t`, so the pair can forge the tag | Cannot: the photo gives `E_pub`, which only the real device can use |
| Photo of the QR → session | No: claiming needs `t`, and the claim is single use | No: collecting needs `p`, which the QR does not carry |
| Session powers | `DeviceLink`, enrollment-only | `DeviceOffer`, enrollment-only, and refused by the same OTP gates |
| Who must prove presence | The QR's creator (PIN before the QR exists) | The scanner (PIN before the accept is sent) |
| Single use and TTL | Server-enforced | Server-enforced: one accept, one session delivery, 120 s to accept |

### The tradeoff, stated

v1's risk is someone persuading a user to *show* them a QR. v2's risk is
someone persuading a user to *scan* one, for example a phishing page that
relays a QR from the attacker's own desktop. That pairing is easier to stage,
and it is the known weakness of every scan-to-log-in scheme (WhatsApp Web,
Discord). It is mitigated here, not removed:

- the PIN is required before the accept is sent;
- the scanner says to scan only a code on a device in front of you that you
  are signing in yourself, and that it will be able to read your messages;
- a code is good for two minutes and one accept;
- the new device appears in the device list and the security events
  (`via=qr_scan`), and it can be revoked from there.

**Never a link.** What keeps this narrow is that a sign-in code is inert
outside the app. `pollis-link:` is a plain-text payload, not a URL scheme,
universal link or app link that either app registers, so the system camera
shows it as text and cannot open Pollis with it. The only path to an accept
is the user opening Pollis, going to the scanner, entering the PIN and
scanning within two minutes, so an attacker has to talk a specific person
through every step live. A code posted in public is dead in two minutes and
does nothing from the camera. Do not register a handler for either code, and
do not wrap one in an `https://` or `pollis://` URL: that turns "scan this"
into a one-tap path. `mobile/tests/link-codes.test.ts` fails if the app config
or a config plugin ever registers `pollis-link`.

The direction is the owner's explicit choice (#1361), and it is the model
Signal and WhatsApp use for linked devices. Email sign-in, v1, and the typed
SAS all remain.

### Scope and storage

- `SessionScope::DeviceOffer` is accepted by `register-device`,
  `enrollment-request` and the enrollment read, like `DeviceLink`. Its request
  must carry no tag, and a tag is a 400, just as a missing tag is on a v1 link.
- Offers live in the same `LinkStore`, in `Offered`, then `Accepted` (bound to
  an account), then `Claimed` (session collected), then `Requested`. A record's
  direction (`Kind`) is fixed when it is created. Unaccepted offers belong to
  no account, so `/v1/link/status` answers `expired` for them. A request bound
  to a live offer is left out of `pending_enrollments`, like a v1 one.
- Each side refuses the other direction's code with a message naming the right
  device (`parse_payload` / `parse_offer_payload`).
