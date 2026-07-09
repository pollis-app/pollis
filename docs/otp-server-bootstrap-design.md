# OTP + Bootstrap Server-Side Migration — Design

**The last piece of Goal B (#419):** move the OTP (generation, validation, email)
and the credential-establishing "bootstrap" writes off the client and behind the
Delivery Service, gated by a **server-validated OTP session** instead of a device
signature. This is what lets the client hold a **read-only Turso token** and drop
the baked-in **Resend key** — both block mobile app review.

## Why these can't use the device-signature gate
Goal B signs every DS write with the device's MLS key. But a handful of writes
*establish* that credential, so they can't be signed by it (chicken-and-egg):
account creation, account-identity establishment (`account_id_pub` + `account_key_log` v1),
device registration, and the **first device-cert publish** (which populates the
very `mls_signature_pub` the DS verifies against). And the OTP itself is currently
generated/validated/emailed **entirely client-side** (`state.otp_store` HashMap +
a baked-in Resend key).

## Pre-signing-key vs post-signing-key (the pivot)
```
request_otp                                   PRE  (no identity)
verify_otp → INSERT users                     PRE  (bootstrap)
generate_account_identity:
  UPDATE users.account_id_pub, v=1            PRE
  INSERT account_key_log v=1                  PRE
  INSERT account_recovery                     PRE
register_device → INSERT user_device          PRE  (mls_signature_pub NULL)
set_pin (client-only key wrapping)            client-only
ensure_device_cert → UPDATE mls_signature_pub PIVOT (establishes the credential)
initialize_identity → key-package publish     POST (first device-SIGNED DS call)
```
Everything up to and including the cert publish is OTP-session-gated; everything
after uses the existing device-signature path.

## The secret/public boundary (STAYS client-side, never sent raw)
- `account_id_key` **private** (Ed25519, the human's cross-signing identity) — keystore (PIN-wrapped) + `state.unlock`; the server only ever holds it **wrapped under the Secret Key** in `account_recovery`.
- The **Secret Key** (shown once), the **db_key** (PIN-wrapped), and the **MLS device signing key** — all client-only.
- Sent to the server: only public/wrapped material — `account_id_pub`, `mls_signature_pub`, the account-key-signed `device_cert`, and `{salt, nonce, wrapped_key}`.

## Proposed DS surface (new `pollis-delivery/src/otp.rs` + `session.rs`)
1. **`POST /v1/auth/request-otp`** `{email}` — DS generates the OTP, stores it server-side (salted hash + TTL + attempt counter), emails via Resend (key in **DS env**). Always 200 (anti-enumeration). Per-email resend throttle + **per-IP throttle** (`pollis-delivery/src/ratelimit.rs`, client IP from `CF-Connecting-IP`; also applied to verify-otp — the email-bomb / cross-email-enumeration defense, #345).
2. **`POST /v1/auth/verify-otp`** `{email, code, account_id_pub?}` — **constant-time compare, attempt-limited (lockout at 5; deleted on lockout, and consumed on success only *after* the account-write + session mint succeed — validate-then-consume, so a transient/config DB failure returns 5xx and the same code still verifies on retry rather than being burned, #518)** — *fixes the current unlimited-guess bug*. Creates/loads the account (`INSERT users` moves here), issues a short-lived **OTP-session token** bound to `(user_id, email, device_id)`. Returns `{user_id, is_new_account, has_identity, session_token, expires_at}`.
3. **`POST /v1/auth/establish-identity`** (session-gated, signup only) — `UPDATE users SET account_id_pub, identity_version=1 WHERE id=:session AND account_id_pub IS NULL` (CAS — never overwrite) + `INSERT account_key_log v1` + `INSERT account_recovery`, one transaction. 409 if identity already exists.
4. **`POST /v1/auth/register-device`** (session-gated) — `INSERT user_device` + watermark seeds; `user_id` bound from session.
5. **`POST /v1/auth/publish-device-cert`** (gate: **session + cert-validity**) — `UPDATE user_device` cert columns + `mls_signature_pub`; the DS also Ed25519-verifies the cert against the stored `account_id_pub` (port `verify_device_cert` into the DS). Invalidate the session on success.

**Session token:** opaque random 256-bit bearer (not a JWT), stored **hashed** server-side, TTL ~10 min, capability = "bootstrap writes for this user_id only" (handlers bind user_id from the session, never the body — same property as `resolve_actor`). New `verify_session` auth mode in `pollis-delivery/src/auth.rs` alongside `verify_request`.

## Multi-device / re-login
Re-login does **not** establish identity (CAS `WHERE account_id_pub IS NULL` forbids overwrite — a new device must never replace the account key). Re-login: verify-otp → register-device (session-gated) → **stop**; the device obtains `account_id_key` via the existing **sibling-approval** or **Secret-Key recovery** paths, then publishes its first cert gated by **cert-validity alone** (proof it holds the account key — stronger than a session, and survives slow sibling approval that would outlast a session TTL). So: signup cert publish = session + cert-validity; subsequent devices = cert-validity alone.

## Decisions taken (defaults; flag to revisit)
- **Store:** in-DS in-memory map (DS is single-container; mirrors today's `otp_store`). **Constraint:** breaks under horizontal scaling (OTP + attempt-counter fork per replica) — swap to a `otp_session` Turso table if the DS ever scales out. Storage behind a small trait so it's swappable.
- **OTP strength:** keep 6 digits + 5-attempt lockout (adds the missing lockout to today's 6-digit).
- **Session bound to `device_id`** (the client always has one before bootstrap).
- **DEV_OTP** DS-side override (env) so the harness + local dev keep working.

## Migration: per-write seam, phased
Each piece checks `config.pollis_delivery_url`: `Some` → DS endpoint, `None` → today's direct path. **request-otp + verify-otp must flip together** (same OTP store side); the three bootstrap writes flip independently after. `generate_account_identity` is refactored to split pure crypto (stays client) from the DB writes (move to DS).

- **Slice 0 (DS-only, additive, no client change):** the modules, in-memory stores, 5 endpoints, `verify_session`, port `verify_device_cert`, `RESEND_API_KEY` in DS env. Harness-testable. **← start here.**
- **Slice 1:** client seam for request-otp + verify-otp; split `generate_account_identity`.
- **Slice 2:** flip establish-identity / register-device / publish-cert + enrollment-request behind the session gate.
- **Slice 3 (payoff):** downgrade client Turso token to read-only + drop Resend key from the client build (gated on all of Goal B A–G being DS-routed — done).

## Open questions for a human
1. DS replica count (drives in-memory vs Turso-table store).
2. OTP 6 vs 8 digits.
3. Email-change OTP (`request_email_change_otp`/`verify_email_change`) — fold in now (device-signed gate) or later? (separable)
4. Subsequent-device cert-publish gate asymmetry — accept (recommended) or require a fresh session each time?

## Inherent limit (note, not a bug)
Email control = account-bootstrap control (true of all email-OTP auth). The deeper
defense is cross-signing: a brand-new attacker device that didn't obtain the
`account_id_key` can't be cross-signed into existing MLS groups by honest members.
Identity **reset** (which overwrites `account_id_pub`) stays on its own CAS-guarded,
audit-logged `/v1/account/rotate-identity` path — `establish-identity` can never reset.
