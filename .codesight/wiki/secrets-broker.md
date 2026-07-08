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
| `POST /v1/livekit/token` | HS256 participant JWT; identity = `{user}:{device}` (or `voice-`/`:view` per `kind`) from the **verified signer**; room authz (own `inbox-*` and `call-*` always ok, else membership) | `LIVEKIT_API_KEY`, `LIVEKIT_API_SECRET`, `LIVEKIT_URL` |
| `POST /v1/livekit/send-data` | Server-side `RoomService/SendData` — signs an admin JWT + Twirp POSTs a content-free control payload to a room | same LiveKit env |
| `POST /v1/livekit/participants` | Server-side `RoomService/ListParticipants` (voice roster), internal identities filtered; membership-gated | same LiveKit env |
| `POST /v1/turso/token` | Mints a short-TTL **read-only** Turso token via the Platform API | `TURSO_PLATFORM_TOKEN`, `TURSO_ORG`, `TURSO_DB` |
| `POST /v1/r2/presign` | SigV4 query-string presigned URL (GET/PUT/DELETE), path-style, `UNSIGNED-PAYLOAD`, `host`-only signed header | `R2_ENDPOINT`, `R2_BUCKET`, `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY` (`R2_REGION` defaults `auto`) |

**Client cutover: DONE for every embeddable secret (#393).** `pollis-core` holds
no LiveKit or R2 secret:
- **R2** — `commands/r2.rs`'s `presign_r2` presigns every get/put/delete via the DS.
- **LiveKit** — participant tokens via `ds_livekit_token`; SendData via
  `ds_livekit_send_data`; roster via `ds_livekit_participants`. `make_token` /
  `make_view_token` / `make_admin_token` and `livekit_api_key` / `livekit_api_secret`
  are deleted from the client. Connected-room pushes (typing, pings on an
  already-joined room) still ride the participant's data channel — no secret.
- **Turso** — `commands/turso_token.rs` refreshes `remote_db` onto a DS-minted
  short-TTL read-only token; the baked read-only token stays only as a fail-soft
  fallback (Turso reads are load-bearing) until DS minting is live in prod.

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
