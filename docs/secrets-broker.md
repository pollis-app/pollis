# Authorized-secrets broker (#393)

## The problem

Two on-device operations need a **long-lived server secret**:

- Minting a **LiveKit** access token needs the LiveKit API secret.
- Reaching **Cloudflare R2** needs the R2 access key + secret.

Historically those secrets shipped **inside the client bundle**. That is the
whole problem: anyone who unpacks the desktop app (or the mobile binary) can
extract them and mint tokens / hit the bucket as they please. A secret in a
distributed binary is a leaked secret.

The broker moves both operations **server-side**, into the Delivery Service
(`pollis-delivery`). The DS holds the secrets in its environment; the client —
already device-signed — asks the DS to mint a token or presign a URL, and the
secrets never leave the server.

Implementation: `pollis-delivery/src/broker.rs`, wired into the router in
`pollis-delivery/src/lib.rs`.

## Auth model

Both endpoints reuse the **existing device-signature auth** (`crate::auth` via
`crate::writes::gate`) — no new scheme. The point of server-side minting is that
the **identity is derived from the verified signer, never from client input**:

- **Auth on** (`require_auth = true`): the acting user is the verified signer.
  Any `user_id` in the request body is **ignored**. A client cannot mint a
  LiveKit token — or act — as another user.
- **Auth off** (default, e.g. the integration harness): there is no signed
  identity, so the body's `user_id` is used; missing/empty → `400`.

When a required secret is unset in the DS env, the endpoint still exists and
answers, returning **`503`** (mirrors OTP with no Resend key) rather than
failing at startup.

## Endpoints

These request/response shapes **are the contract** the frontend `bridge` and
mobile (via uniffi) call — the on-device LiveKit/R2 paths are now removed.

**Client cutover: DONE for every embeddable secret (#393).** `pollis-core` holds
no LiveKit or R2 secret:

- **R2** — `commands/r2.rs`'s `presign_r2` asks the DS to presign every
  upload/download/delete, then does a plain HTTP call against the returned URL.
- **LiveKit** — participant tokens via `ds_livekit_token` (`/v1/livekit/token`),
  server-side fan-out via `ds_livekit_send_data` (`/v1/livekit/send-data`), voice
  roster via `ds_livekit_participants` (`/v1/livekit/participants`). The on-device
  signers (`make_token` / `make_view_token` / `make_admin_token`) and
  `livekit_api_key` / `livekit_api_secret` are deleted. Connected-room pushes
  (typing, a ping on an already-joined room) still ride the participant's data
  channel and need no secret.
- **Turso** — `commands/turso_token.rs` refreshes `remote_db` onto a DS-minted
  short-TTL read-only token (`/v1/turso/token`). Because Turso reads are
  load-bearing, the baked read-only token remains as a **fail-soft fallback**
  until DS minting is confirmed live in prod (a config/ops flip, not a code gap).

### `POST /v1/livekit/token`

Mint a LiveKit **participant** token. Both halves of the identity come from the
**verified signer** — the `user` from the signature, the `device` from the
signature-verified `X-Pollis-Device` header — so a client cannot mint a token as
another user/device. Room authz (signed path only): the user's own inbox room
(`inbox-<user_id>`) and any `call-<ulid>` room (the ULID is an unguessable
capability; voice is MLS/E2EE) are always allowed; anything else requires
**current membership** (else `403`).

Request:

```json
{
  "room": "conv-abc",
  "kind": "realtime",
  "user_id": "u_123",
  "device_id": "d_456"
}
```

- `room` (required) — the LiveKit room to mint for.
- `kind` (optional, default `realtime`) — identity scheme: `realtime` →
  `{user}:{device}`; `voice` → `voice-{user}:{device}`; `view` →
  `{user}:{device}:view` (screenshare receive, `canPublishData=false`).
- `user_id` / `device_id` (optional) — **no-auth path only**; both ignored when
  auth is enforced (taken from the signer/header).

Response `200`: `{ "token": "<HS256 JWT>", "url": "wss://<livekit-host>" }`.

The JWT is HS256 (`typ=JWT`), byte-compatible with the old on-device
`make_token`: `iss` = API key, `sub` = identity, `iat`/`nbf` = now, `exp` = now +
3600, `name` = username, `video` grants `roomJoin`/`canPublish`/`canSubscribe` =
true (`canPublishData` = false only for `view`).

Env: `LIVEKIT_API_KEY`, `LIVEKIT_API_SECRET`, `LIVEKIT_URL` — all three required,
else `503`. The secret is never logged.

### `POST /v1/livekit/send-data`

Fan out a **content-free** control payload (new-message ping, call invite,
membership-changed, enrollment approval) to a room the caller isn't joined to,
via server-side `RoomService/SendData`. Requires an authenticated device; any
room is allowed (payloads are content-free routing nudges — recipients re-fetch
through independently-authenticated MLS paths, so the worst a spoof does is force
a refetch / dismissable ring; per-room authz is possible future hardening). Body
`{ room, payload }`. A room with no participants (`404`) is success.

### `POST /v1/livekit/participants`

Return a voice room's roster via server-side `RoomService/ListParticipants`
(internal `server` / `pollis-*` / `:view` identities filtered). Membership-gated
like the token endpoint. Body `{ room }` → `{ participants: [{ identity, name }] }`.

### `POST /v1/turso/token`

Mint a short-TTL **read-only** Turso token via the Turso Platform API and hand
back only the JWT. Body `{}` → `{ token, expires_in }`. Env:
`TURSO_PLATFORM_TOKEN`, `TURSO_ORG`, `TURSO_DB` — all required, else `503` (the
client then keeps its baked read-only token, so reads still work).

### `POST /v1/r2/presign`

Return a **SigV4 query-string presigned URL** for a GET (download), PUT
(upload), or DELETE (attachment cleanup) against the configured R2 bucket.
Requires an authenticated device (when auth is on, `gate` rejects an unsigned
request with `401`). There is **no per-object authz** — see below.

Request:

```json
{
  "operation": "get",
  "key": "media/<hash>/<file>.enc",
  "content_type": "application/octet-stream",
  "user_id": "u_123"
}
```

- `operation` (required) — `"get"`, `"put"`, or `"delete"`; anything else → `400`.
- `key` (required) — the R2 object key within the bucket.
- `content_type` (optional) — accepted for forward-compat; the presigned URL
  signs only `host`, so the client sets Content-Type at upload time.
- `user_id` (optional) — no-auth path only; unused beyond the auth gate.

Response `200`:

```json
{
  "url": "https://<endpoint>/<bucket>/<key>?X-Amz-Algorithm=...&X-Amz-Signature=...",
  "method": "GET",
  "expires_in": 900
}
```

The URL is single-chunk, `UNSIGNED-PAYLOAD`, with `host` the only signed header,
default lifetime 900 s. Path-style (`/<bucket>/<key>`).

Env: `R2_ENDPOINT`, `R2_BUCKET`, `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY` —
all required, else `503`. `R2_REGION` defaults to `auto` (Cloudflare R2). The
secret access key is never logged.

## Why R2 presign needs no per-object authz

Pollis media is **convergent-encrypted** (see pollis-core's `r2.rs`): the
AES-256-GCM key is derived from `SHA-256(plaintext)`, and the
`attachment_object` table is a **global content-hash dedup** with no
conversation binding. A presigned URL therefore only ever exposes
**ciphertext** — confidentiality comes from MLS key distribution (only a member
who decrypted the message learns the content hash, and only the content hash
derives the decryption key), **not** from the R2 ACL.

So the presign gate exists solely to stop **anonymous internet access** to the
bucket; it does not — and cannot meaningfully — enforce read authz per object.
Requiring an authenticated device is the right and sufficient gate.

## Tests

`pollis-delivery/tests/broker.rs` — known-answer tests for the pure signing
functions (both take an injected clock so output is deterministic):

- `sign_livekit_token` — decode the JWT, assert HS256/`typ=JWT` header, claim
  shape, and re-verify the HS256 signature against the secret.
- `presign_r2_url` — SigV4 golden test: every input pinned, the exact URL string
  asserted (GET + PUT-with-slash/space), plus a canonical-query ordering check.
  The GET signature is cross-checked against an independent SigV4 implementation.
