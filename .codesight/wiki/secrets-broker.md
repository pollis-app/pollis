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
| `POST /v1/livekit/token` | HS256 participant JWT; identity = an opaque **per-room participant pseudonym** derived from the **verified signer** + its device + `kind` (#836); no `name` claim; room authz (own `inbox-*` and `call-*` always ok, else membership **and no block from a DM peer**) on the **logical** room, then the grant carries the **room pseudonym** (#828); 15-min TTL | `LIVEKIT_API_KEY`, `LIVEKIT_API_SECRET`, `LIVEKIT_URL` |
| `POST /v1/livekit/send-data` | Server-side `RoomService/SendData` — signs an admin JWT + Twirp POSTs a content-free control payload to a room. **Target authz + sender stamping** (below): own inbox always; a peer's inbox only with a shared DM / group / pending invite and no block; a conversation room only as a member; `type` must be client-publishable (`enrollment_requested` is DS-only); identity keys stripped and the verified signer stamped in | same LiveKit env |
| `POST /v1/livekit/participants` | Server-side `RoomService/ListParticipants` (voice roster); each identity **resolved back to its user + username** server-side (#836), internal and `view` participants filtered; membership-gated | same LiveKit env |
| `POST /v1/livekit/identities` | Resolve opaque participant pseudonyms → `{user_id, name, kind}` for a room the caller may join (#836). The per-room key never leaves the DS | same LiveKit env |
| `POST /v1/r2/presign` | SigV4 query-string presigned URL (GET/PUT/DELETE), path-style, `UNSIGNED-PAYLOAD`. Keys are allow-listed by family; a PUT must be content-addressed and declare a bounded `content_length` (signed, so `host;content-length`); writes to an avatar/icon need the owner, writes to a referenced media/emoji object are refused (below) | `R2_ENDPOINT`, `R2_BUCKET`, `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY` (`R2_REGION` defaults `auto`) |

### `send-data` targets are authorized and the sender is stamped

`/v1/livekit/send-data` used to admit any room from any signed device and forward
the client's JSON verbatim under the DS's room-admin token, while the client
dispatcher trusted identity fields in that JSON (`caller_id`, `caller_username`,
`inviter_username`, …). Any account could ring any user's devices "from" anyone,
raise the enrollment-approval takeover on a stranger's screen, or storm an inbox
with refetch nudges, blocks never consulted. Three rules now hold, all in
`livekit_send_data` (`broker.rs`), checked before the secrets gate:

- **Target** (`SendTarget`): `inbox-<signer>` is always allowed; `inbox-<peer>`
  needs a shared DM channel, a shared group, or a pending `group_invite` from the
  signer to the peer, and no `user_block` in either direction (`may_reach_inbox`);
  `call_invite` needs a DM the peer has **accepted** or a shared group, so a bare
  DM request cannot ring; any other room needs `is_member`. `call-*` rooms are not
  send-data targets (calls are signalled through inboxes).
- **Type** (`ClientPayloadKind`): an allowlist of what clients originate
  (`new_message`, `edited_message`, `deleted_message`, `membership_changed`,
  `roster_changed`, `join_requests_changed`, `member_role_changed`, `dm_created`,
  `all_mention`, `user_mention`, `device_revoked`, `call_invite`, `call_canceled`).
  `enrollment_requested` is emitted only by `bootstrap::enrollment_request` via
  `room_send_data` directly and is refused from any client, as is any unknown type.
- **Identity** (`sanitize_client_payload`, pure): every identity key is stripped
  from every client payload. For an inbox target and a kind that names its actor
  (`dm_created`, `membership_changed`, `all_mention`, `user_mention`,
  `call_invite`) the DS stamps the verified signer as `sender_id` +
  `sender_username` (resolved from `users`), and re-stamps the legacy per-kind keys
  (`caller_id`/`caller_username`, `inviter_username`/`group_name` — the latter from
  `groups`) with the same verified values so shipped renderers keep working.
  Shared-room targets get nothing stamped (§5 stays routing-only).

Client side, `dispatch_data` (`pollis-core/src/commands/livekit/mod.rs`) attributes
these events from the publishing participant when there is one, or from the DS
stamp when there is none (the DS's own `SendData`), and never from the body's
own claims; `enrollment_requested` is honoured only with no participant.
Tests: `pollis-delivery/tests/livekit_send_data.rs` (refusals never reach a
recording fake LiveKit; allowed sends carry the stamped signer) and the
`dispatch_data` unit tests in `livekit/mod.rs`.

### Losing access ends the session you already hold

A LiveKit token is verified **once, at join**. A membership row deleted afterwards
reaches the SFU not at all, so a removed member, a leaver, a blocked peer and a
revoked device all used to keep the realtime/voice connection they were already
holding — presence, typing, control nudges, the voice room — until they chose to
disconnect. Two halves close that, both in `broker.rs`:

- **The kick.** `room_remove_participant` is the `RoomService/RemoveParticipant`
  sibling of `room_send_data` (same admin JWT, same Twirp base, same
  404-is-success rule). `evict_user_from_rooms` / `evict_device_from_rooms` fan it
  out over `participant_identities` — one identity per `(device, kind)`, since the
  pseudonym is keyed on both, plus the legacy empty-device identity for the
  user-level case. Concurrent (`JoinSet`) and awaited before the handler answers;
  failures are logged, never returned, exactly like a nudge.
- **The TTL.** `LIVEKIT_TOKEN_TTL_SECS` = 15 min (was 1 h) bounds how long a token
  minted *before* the loss can still be redeemed. `realtime.rs` re-mints on every
  reconnect and LiveKit only checks at join, so a live session never notices.

Call sites, and the room sets they evict from:

| Event | Rooms |
|-------|-------|
| `POST /v1/members/remove` | `group_rooms` — the group room **and every channel in it** (voice joins the *channel* as the room) |
| `POST /v1/groups/leave` | `group_rooms`, read **before** the leave (an emptied group is torn down, taking its `channels` rows) |
| `POST /v1/blocks/add` | `shared_dm_rooms(blocker, blocked)` — the blocked user only |
| `POST /v1/devices/revoke` | `user_rooms(owner)` — inbox, groups, their channels, DMs — for **that device only** |

A block deletes no membership row, so the eviction alone would be undone by the
blocked client's next reconnect. `authorize_room` therefore also refuses a token
to a user another member of that DM has blocked (`dm_peer_blocked`) —
one-directional, like the block itself: the blocker keeps their own access.
`call-<ulid>` rooms are not evictable: no membership row exists to enumerate them.

Tests: `pollis-delivery/tests/livekit_eviction.rs` (a recording fake
`RemoveParticipant` Twirp endpoint; per-room, per-device, per-kind assertions,
plus "a forbidden removal evicts nobody" and the blocked-peer token refusal) and
`a_participant_token_expires_in_minutes_not_hours` in `tests/broker.rs`.

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

**Dev and prod hold different secrets, and must.** Both DS deployments point at the one
`wss://rtc.pollis.com`, whose `keys:` block carries two pairs (see
[`livekit/DEPLOY.md`](../../livekit/DEPLOY.md) → "LiveKit API keys"). Because the room and
participant pseudonyms are HMACs keyed off `LIVEKIT_API_SECRET`, a secret shared across
the two environments would derive the *same* room name for the same conversation in both
— a dev-minted token would then join the production room. The deploy workflow refuses to
render two identical keys. Rotation procedure — including the ordering (LiveKit first,
then both DS deploys, since the DS reads its key from the Cloudflare Secrets Store that
`sync-ds-secrets.sh` repopulates from Doppler) — is in `DEPLOY.md`.

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
  The URL that comes back is DS-chosen, so the client re-checks it: `PresignedUrl`
  is the only type the request builders take and its constructor requires the
  configured R2 origin, and `r2_get_url` streams against a caller-supplied cap
  rather than buffering whatever arrives (see `media-server.md`).
- **LiveKit** — participant tokens via `ds_livekit_token`; SendData via
  `ds_livekit_send_data`; roster via `ds_livekit_participants`. `make_token` /
  `make_view_token` / `make_admin_token` and `livekit_api_key` / `livekit_api_secret`
  are deleted from the client. Connected-room pushes (typing, pings on an
  already-joined room) still ride the participant's data channel — no secret.
- **Turso** — there is no client database credential at all since #987. The
  broker's `/v1/turso/token` mint, `commands/turso_token.rs`, `RemoteDb`,
  `state.remote_db` / `state.log_db` and the baked `TURSO_URL` / `TURSO_TOKEN` /
  `LOG_DB_URL` / `LOG_DB_TOKEN` build inputs are all deleted; `pollis-core` does
  not depend on `libsql`. Every read is a `POST /v1/read/…` on the DS, on the
  same signed transport as the writes — see `commands.md`, "The client holds no
  database credential". This is the one broker cutover that ended by removing
  the secret rather than by shortening its life: a short-TTL read-only token is
  still a whole-database read-only token for its lifetime, so scoping it was
  never going to close #917's residual, and it made every shipped binary a
  credential to be extracted.

## Why R2 presign has no per-object READ authz — and what it does gate

Pollis media is convergent-encrypted (`pollis-core`'s `r2.rs`): the AES-256-GCM
key is `SHA-256(plaintext)` and `attachment_object` is a global content-hash
dedup with no conversation binding. A presigned URL only ever exposes
**ciphertext** — confidentiality comes from MLS key distribution, not the R2 ACL.
So for `get` the gate stops anonymous internet access to the bucket; an
authenticated device is the right and sufficient gate.

Writes are a different question, and the answer is not "an authenticated device".
`broker::r2_presign` applies four rules:

| Rule | Why |
| --- | --- |
| **Key allow-list** (`parse_r2_key`): only `media/<hex64>.enc`, `emoji/<hex64>.<ext>`, `avatars/<user_id>/<hex64>.<ext>`, `group-icons/<group_id>/<hex64>.<ext>` and their legacy read-only shapes. No empty segment, no `.`/`..`, conservative charset, ≤ 512 bytes | An unrestricted key is an unrestricted object: free storage on someone else's bucket, and a chance to traverse or to smuggle a separator into the canonical request |
| **A `put` must be content-addressed** | A legacy mutable key stays readable and stops being writable — whoever can overwrite `avatars/<uid>` replaces that user's picture everywhere, and nothing about the key says what the bytes should be |
| **A `put` must declare an exact, bounded `content_length`** — media ≤ `R2_MEDIA_MAX_BYTES`, emoji ≤ `EMOJI_MAX_BYTES`, avatar/icon ≤ `R2_PUBLIC_IMAGE_MAX_BYTES` (all in `pollis-api`, one constant per bound shared with the uploader) | Only R2 counts the bytes. A size the DS is *told* at registration is a promise; a size inside the signature is a bound |
| **Owned objects need their owner** — `avatars/<uid>` the user, `group-icons/<gid>` a group admin — and **shared objects must be unreferenced** to `put` or `delete` (#690, #848) | An avatar and a group icon have no reference count to protect them and were previously ungated entirely. The media reference gate existed but was DEAD for `media/<hex64>.enc`: the old extractor returned `"<hash>.enc"`, which matches no stored content hash, so no object written since #762 was ever protected |

Tests: `pollis-delivery/tests/r2_presign_scope.rs`.

## Pure signing functions (testable)

`sign_livekit_token` and `presign_r2_url` are pure — the clock/timestamp is
injected — so the signatures are deterministic and lockable. Known-answer tests
live in `pollis-delivery/tests/broker.rs`: JWT header/claim shape + HS256
re-verification, and a SigV4 golden URL (GET + PUT-with-slash/space) whose GET
signature is cross-checked against an independent SigV4 implementation.
