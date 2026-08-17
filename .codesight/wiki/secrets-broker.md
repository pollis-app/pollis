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
| `POST /v1/livekit/token` | HS256 participant JWT; identity = an opaque **per-room participant pseudonym** derived from the **verified signer** + its device + `kind` (#836); no `name` claim; room authz (own `inbox-*` and `call-*` always ok, else membership) on the **logical** room, then the grant carries the **room pseudonym** (#828) | `LIVEKIT_API_KEY`, `LIVEKIT_API_SECRET`, `LIVEKIT_URL` |
| `POST /v1/livekit/send-data` | Server-side `RoomService/SendData` — signs an admin JWT + Twirp POSTs a content-free control payload to a room | same LiveKit env |
| `POST /v1/livekit/participants` | Server-side `RoomService/ListParticipants` (voice roster); each identity **resolved back to its user + username** server-side (#836), internal and `view` participants filtered; membership-gated | same LiveKit env |
| `POST /v1/livekit/identities` | Resolve opaque participant pseudonyms → `{user_id, name, kind}` for a room the caller may join (#836). The per-room key never leaves the DS | same LiveKit env |
| `POST /v1/turso/token` | Mints a short-TTL **read-only** Turso token via the Platform API | `TURSO_PLATFORM_TOKEN`, `TURSO_ORG`, `TURSO_DB` |
| `POST /v1/r2/presign` | SigV4 query-string presigned URL (GET/PUT/DELETE), path-style, `UNSIGNED-PAYLOAD`, `host`-only signed header | `R2_ENDPOINT`, `R2_BUCKET`, `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY` (`R2_REGION` defaults `auto`) |

### Room names are pseudonymous (#828)

Rooms used to be named with the raw `group.id` / `dm_channel.id`, and inboxes with
`inbox-<user_id>`. Room membership is visible to the SFU operator by construction, so
that handed LiveKit a second, independent copy of the social graph — which users belong
to which conversations — living outside Turso.

Every room name is now `r-<32 hex>` = HMAC-SHA256 over the logical name, keyed by a
label-separated derivation of `LIVEKIT_API_SECRET` (`pollis-delivery/src/room_id.rs`).

The split that matters: **authorization happens on the logical name, the pseudonym is
what crosses to LiveKit.** Membership checks are unchanged.

This is deliberately **server-only**. A LiveKit JWT carries the room inside its `video`
grant and `Room::connect` takes no room argument, so the client dials whatever its token
grants and never holds the mapping or the key — a mapping shipped in a release binary
could be extracted and used to re-link every room to its conversation. The three places a
room reaches LiveKit all map at the chokepoint (`sign_livekit_token`, the participants
handler, and inside `room_send_data`, so server-side emitters like
`bootstrap::enrollment_request` cannot forget).

Rotating `LIVEKIT_API_SECRET` re-keys every room name. That is self-healing rather than a
break: rotation already invalidates outstanding LiveKit tokens, so clients re-request one
and get the new name in the same round trip.

These are stable pseudonyms, not unlinkable ones — one name per conversation for the life
of the secret.

### Participant identities are pseudonymous too (#836)

Room names alone said nothing about *who* was in a room: identity was still
`{user_id}:{device_id}` (`voice-` / `:view` per kind) and the JWT `name` claim was the
user's **username**, so the operator kept the membership of every room and could cluster
the social graph by co-membership without ever naming a conversation.

Every identity is now `{prefix}{base64url(siv ‖ ct ‖ tag)}` — the user id **encrypted**
under a key derived per **logical room** (`pollis-delivery/src/participant_id.rs`), with
the device id folded into the synthetic IV so a user's two devices stay distinct
participants (#140) without either id reaching the wire. The `name` claim is empty.

Encryption rather than a one-way MAC, unlike room names, because identity is *matched
against*: the roster, speaking indicators, presence and the self-hear filter all need to
get from a participant back to a user. A MAC would force the resolver to enumerate
candidate `(user, device)` pairs, which is impossible for `call-<ulid>` rooms (no
membership rows) and stale for a device enrolled since the last sync.

The key stays **server-side**, exactly as in #828 — clients resolve through
`POST /v1/livekit/identities`, which answers only for rooms they may already join. A
per-room key shipped in a release binary could be extracted; one that resolved every room
would hand the whole graph back.

Client side, `pollis-core/src/commands/livekit_identity.rs` owns the two identity spaces:
the opaque **wire** pseudonym and the internal `voice-{user}:{device}` key the app and
renderer speak. Every LiveKit boundary translates inbound; nothing internal reaches a wire.

**What the SFU can still do:** count participants in a room, and recognise a returning one
**within that room** (identities are stable per room, like room names). For 1:1 calls that
is already per-call, because `call-<ulid>` rooms are ephemeral. What it can no longer do:
map a participant to an account, or tell that a participant in one room is the same person
as one in another.

**The `/v1/livekit/token` response is `{token, url}`, and the URL is authoritative.**
A LiveKit JWT is only accepted by the server that issued it, so the two travel
together. Clients used to destructure the response as `(token, _url)` at six call
sites and dial the compiled-in `config.livekit_url` instead — which baked the SFU
address into every shipped binary and made relocating or regionalising the SFU a
client-release problem. `ds_livekit_token` now resolves the precedence centrally
(`resolve_livekit_url`: DS wins, config is the self-host fallback for a DS with no
`LIVEKIT_URL`), so no call site can reintroduce it by ignoring a field.

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
