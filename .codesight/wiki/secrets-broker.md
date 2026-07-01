# Authorized-Secrets Broker

How Pollis mints LiveKit tokens and presigns R2 URLs **server-side** so the API
secrets never ship in the client bundle (#393).

## TL;DR

Two operations used to hold a long-lived server secret on-device: minting a
LiveKit token (LiveKit API secret) and reaching R2 (R2 access key + secret). A
secret in a distributed binary is a leaked secret, so both moved into the
Delivery Service. The DS holds the secrets in its env; the already-device-signed
client asks the DS to mint / presign, and the secrets never leave the server.

Code: `pollis-delivery/src/broker.rs`, routed in `pollis-delivery/src/lib.rs`.
Full contract + env vars: [`docs/secrets-broker.md`](../../docs/secrets-broker.md).

## Endpoints

Both reuse the existing device-signature auth (`crate::writes::gate`) — no new
scheme. Identity is derived from the **verified signer, never client input**, so
a client cannot act as another user. A missing secret → `503` (endpoint still
answers, like OTP with no Resend key).

| Endpoint | Does | Env (all required, else `503`) |
|----------|------|--------------------------------|
| `POST /v1/livekit/token` | HS256 JWT for the signer's identity; room authz (own `inbox-<user_id>` always ok, else current membership) | `LIVEKIT_API_KEY`, `LIVEKIT_API_SECRET`, `LIVEKIT_URL` |
| `POST /v1/r2/presign` | SigV4 query-string presigned URL (GET/PUT), path-style, `UNSIGNED-PAYLOAD`, `host`-only signed header | `R2_ENDPOINT`, `R2_BUCKET`, `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY` (`R2_REGION` defaults `auto`) |

The request/response JSON shapes are the contract the frontend `bridge` and
mobile uniffi will call once the on-device LiveKit/R2 paths are removed (the
client cutover is the follow-up to #393).

## Why R2 presign has no per-object authz

Pollis media is convergent-encrypted (`pollis-core`'s `r2.rs`): the AES-256-GCM
key is `SHA-256(plaintext)` and `attachment_object` is a global content-hash
dedup with no conversation binding. A presigned URL only ever exposes
**ciphertext** — confidentiality comes from MLS key distribution, not the R2 ACL.
So the gate stops anonymous internet access to the bucket; an authenticated
device is the right and sufficient gate.

## Pure signing functions (testable)

`sign_livekit_token` and `presign_r2_url` are pure — the clock/timestamp is
injected — so the signatures are deterministic and lockable. Known-answer tests
live in `pollis-delivery/tests/broker.rs`: JWT header/claim shape + HS256
re-verification, and a SigV4 golden URL (GET + PUT-with-slash/space) whose GET
signature is cross-checked against an independent SigV4 implementation.
