# Metadata Retention Policy

**Status:** **Signed off and PUBLISHED** (owner sign-off, 2026-08-28, under #877). The public rendering
derived from this document is `website/retention.html`, served at pollis.com/retention. Resolves the
engineering half of #702; part of #662.
**Audience:** this document stays the engineering source of truth — every claim cites the enforcing code
or migration, and the public page is derived from it, never the other way round. It is also what whoever
fills in the App Store / Play privacy forms (#708) answers from.
**Changing a retention period is a change to a published promise.** Edit the enforcing code, then this
document, then `website/retention.html`, in that order, and deploy the site (`website-deploy.yml` is
manual by design) — a public page that outlives the behaviour it describes is the defect this document
exists to prevent.
**Scope:** what the *first-party services* retain — Turso (the Delivery Service database), Cloudflare R2,
LiveKit, the Expo/APNs/FCM push path, the relay overlay, and the public transparency ledger. Message
*content* is out of scope: it is end-to-end encrypted under MLS and no first-party service can read it.

This document is written from the code, not from intent. Every retention claim below cites the
enforcing code or migration. Where the code retains something forever, this document says "indefinite"
rather than inventing a period we do not actually enforce. **A policy that overstates our deletion
behaviour is worse than no policy** — it is the thing a store privacy form or a regulator would hold us
to.

---

## 0. The one-paragraph version

Pollis stores no message plaintext anywhere. What the Delivery Service does hold is routing metadata:
which conversations exist, which users and devices belong to them, when envelopes were written, and
how the MLS group state evolved. Encrypted envelopes are retained **only until every current member
device has collected them** — bounded by the slowest device, never by a clock. Client IP addresses are
used for rate limiting only, held in memory as a keyed per-process hash, and never written to a database. Push tokens are stored until the device
re-registers, the token goes 180 days without a refresh, the push service reports it unregistered, or the
account is deleted, and the notification they carry is content-free. Account
records and membership rows are retained for the life of the account and removed when the account is
deleted, by an enumerated, table-by-table teardown (§2.1). The account-key history is the deliberate exception: it is
append-only and survives deletion (§8). The public transparency ledger is, by design, permanent and
append-only; its commit-log leaves carry windowed pseudonyms rather than conversation and sender
identifiers, so a third party cannot build a longitudinal activity map from it (§6).

---

## 1. Retention model: watermark-bounded, not time-bounded

The single most important thing to understand about Pollis retention is that **the primary mechanism is
a delivery watermark, not a TTL.**

An encrypted envelope is deleted when *every current, LIVE member device of its conversation has reported
a fetch watermark past it* (`pollis-delivery/src/messages.rs`, the `CLEANUP_*` fragments). There is
**no TTL**: invariant I3 in `docs/backend-core-invariants.md` requires retention bounded by the slowest
member device, so message age never deletes (#688).

**Removed members do not pin (#1778).** "Current" is literal: a device whose user has been removed from
the conversation stops counting toward the floor the moment the membership row goes. Mail sealed to it
before the removal that it never fetched can then be collected once the remaining members read past it.
This is deliberate. A removed member has no in-app path to the conversation, so nothing it could read is
withheld. If the member is later re-added, that pre-removal mail is treated like mail sent before they
first joined: rejoining does not bring it back, and no placeholder is shown for it. Pinning envelopes for
removed devices would keep ciphertext around for people who are no longer in the conversation, which this
policy exists to avoid.

**Device-staleness bound (#720).** The floor is bounded by member watermarks **and** by device
*liveness*: a member device that has not **reported** a watermark within a configured window
(`POLLIS_DS_WATERMARK_STALE_MONTHS`, **12 months** — the code default since #1486, and set explicitly in
both deployed environments) stops pinning retention and is excluded from
the floor, mirroring how a revoked device is excluded (#685). This is **not** a TTL: the bound is on the device's own last **report** (`conversation_watermark.reported_at`, a
server-stamped wall-clock time), never on the message's age. A device that reported yesterday still pins
every envelope below its cursor, however old. Coming back and reporting makes the device live again
immediately (reversible; the device is not revoked or removed from any roster).

**Why 12 months** (decided 2026-08-04; the code default since #1486). This window is the *only* thing
standing between a returning user and missing mail, so its length is the blast radius of the third
accepted loss below. The product principle in `CLAUDE.md` — *"given 'simpler but drops messages' vs
'more complex but delivers,' pick the one that delivers"* — points at the longer window, and three
things make it cheap:

- **The cost of a longer window is small and bounded.** Retention is keyed on the device's last
  *report*, not the message's age, so a live device already pins indefinitely; N only governs how
  long a *dormant* device keeps doing so. Envelope volume at present scale is negligible (#720's own
  follow-up: "not urgent at current scale; urgent before a large group with churn"), and
  `GET /v1/retention/metrics` now exists precisely to catch that turning into a bill.
- **The cost of too short a window is a silently broken promise.** Six months does not cover a phone
  in a drawer over a long trip, a hospital stay, or parental leave — all ordinary, and all cases
  where the user did nothing wrong and loses mail anyway.
- **The direction is reversible; the other one is not.** Shortening N later only changes what future
  sweeps collect. Lengthening it after a sweep has run cannot bring deleted envelopes back. When one
  direction is recoverable and the other is not, start on the recoverable side.

`0` disables the bound entirely, restoring pre-#720 watermark-only behaviour. That is deliberately
available but not chosen: unbounded growth was the problem #720 set out to fix.

The consequence, stated plainly because a privacy form will ask: **there is no wall-clock maximum
retention period for an undelivered envelope, but there is now a bound tied to recipient-device
liveness.** A member who joins a busy conversation and keeps at least one device reporting pins that
conversation's envelopes indefinitely; a member whose every device goes silent past the window stops
pinning. The cost is a **third accepted message loss** — beyond "messages sent before you joined" and "a
new device starts empty unless you copy history to it from another of your enrolled devices" — a device dormant past the window that later returns may find gaps. This is a
deliberate storage/correctness trade. **N is decided: 12 months** (2026-08-04, rationale above), the code
default (#1486) and also set explicitly in both deployed environments, and disclosed to users on
the Learn page rather than only in this document.

**Growth visibility (#720 checkbox 1).** The GC sweep computes an identity-free `message_envelope` growth
snapshot on the same walk — total envelope count, how many conversations hold envelopes, the oldest
retained `sent_at`, and the *shape* of the largest (its count + oldest `sent_at`, no id). It is logged
and served on `GET /v1/retention/metrics`. That endpoint is **gated by an operator
bearer token**, NOT open like `/health`/`/version`: those return a service name and a git sha, whereas this
is a pollable aggregate activity meter — scraped on a schedule it reveals how many conversations exist, how
fast the network is growing, and when it is busy. Identity-free (§3) is necessary but not sufficient; the
volume itself is metadata this product exists not to disclose. So it is **default-closed**:
`POLLIS_DS_METRICS_TOKEN` unset → the route 404s (not served); set → callers must send
`Authorization: Bearer <token>`. Alert *routing* (thresholds/paging in Cloudflare/Grafana/email) is the
owner's console and is out of scope — the DS only emits the numbers.

The MLS commit log solved the analogous problem differently — it prunes to a tier-1 floor (the minimum
applied epoch across current member devices) with a tier-2 per-conversation hard cap that bounds a stuck
member (`retention_tests::tier2_hard_cap_bounds_a_stuck_member`). #720 follows that *shape* (a bound
against a permanently-absent device) but keys the envelope bound on liveness, not epoch distance.

---

## 2. What the Delivery Service database retains

Schema: `pollis-schema/migrations/` (main DB) and `pollis-schema/migrations-log/` (commit-log
DB, split out in #420 Goal A).

| Data | Table | Retention | Enforced by |
|---|---|---|---|
| Encrypted message envelopes | `message_envelope` | Until every current **live** member device has reported a watermark past the row. **No TTL** (age never deletes); a device silent past `POLLIS_DS_WATERMARK_STALE_MONTHS` (set to 12) stops pinning, bounded on the device's last **report**, not the message's age. Also removed immediately on user-initiated delete, and on account deletion for every envelope the account still has waiting: the client lists them with their deletion capability (sealed envelopes name no sender) and the DS removes them in the deletion transaction. | `messages.rs` `CLEANUP_*`, `withdraw_envelopes`; #688/#716, #720 |
| Reactions | `message_reaction` | Life of the account; **explicitly deleted** on account deletion (both the user's own reactions and any reaction to a message that goes with them) | `teardown.rs` `purge_user_rows` / `purge_conversation_rows` |
| Per-device fetch cursors | `conversation_watermark` | **Indefinite** — advanced, never aged out. Deleted with the device (revoke, logout), with the conversation, and on account deletion. Carries `reported_at` (server-stamped last-report wall-clock, #720), the liveness signal the envelope floor reads. Written only by a current **member**, for its own **signing device**, and only with a canonical UTC RFC 3339 cursor no further ahead of the DS clock than the signature window (`check_cursor_stamp`) — the same bound every client-chosen `message_envelope.sent_at` must meet, since a far-future cursor never rewinds and would black out delivery and then trigger GC | `apply_advance_watermark`; #720; `teardown.rs` |
| Roster-departure counter (#1874) | `conversation_membership_version` (conversation id, a counter, a time; no user or device id) | One row per conversation that has had a departure; deleted with the conversation | `000046_conversation_membership_version.sql`; `teardown.rs` `purge_group` / `purge_dm_channel` |
| MLS commit history | `mls_commit_log` (log DB) | Pruned to the tier-1 floor (min applied epoch across current member devices, and across removed-but-unrevoked devices that reported within `POLLIS_DS_WATERMARK_STALE_MONTHS`, so a re-added device can replay to its own removal, #1772) with a tier-2 per-conversation hard cap. On account deletion the rows of surviving conversations stay, with the account's `sender_id`, `added_user_id` and `added_device_ids` blanked | commit-log retention code + `retention_tests`; `teardown.rs` `scrub_commit_log` |
| Latest MLS group state | `mls_group_info` | Latest epoch only; overwritten, no history | `000001_commit_log_db.sql` |
| Per-device commit catch-up | `mls_commit_since` | Upserted monotonically; deleted with the device, with the conversation, and on account deletion | `000003_mls_commit_since.sql`; `teardown.rs` `purge_user_log_rows` / `purge_device_log_rows` |
| Welcome messages | `mls_welcome` | Deduplicated per `(conversation_id, recipient_id, recipient_device_id)`; a resend for that tuple replaces the blob and re-arms delivery rather than stacking a row; **explicitly deleted** on account deletion and with the conversation | `migrations-log/000002_mls_welcome_unique_recipient.sql`; `writes.rs` `purge_welcomes`, `teardown.rs` |
| Key packages | `mls_key_package` | Claimed packages are marked `claimed=1`, not hard-deleted; **explicitly deleted** on account deletion, device revocation and logout | `000000_baseline.sql`, `000010`; `teardown.rs`, `account.rs` |
| Key-package claim budget | `mls_key_package_claim` (claimer, target account, optional target device, time) | **One hour**, the window the claim budget counts; older rows are deleted by the GC sweep. Also **explicitly deleted** on account deletion, as claimer or as target | `000022_key_package_claim_audit.sql`; `devices.rs` `sweep_key_package_claims`; `teardown.rs` `purge_user_rows` |
| Accounts | `users` | Life of the account; deleted on account deletion, last in the teardown transaction | `account.rs` `apply_delete_account` |
| Devices | `user_device` | Revocation writes a `revoked_at` tombstone (the row is retained so revoked devices stop pinning retention and cannot re-authenticate); rows are hard-deleted on logout and on account deletion, together with that device's key packages and fetch cursors | `000004_user_device_revoked_at.sql`; `account.rs` `apply_logout_device` / `apply_revoke_device`, `teardown.rs` |
| Account identity-key history | `account_key_log` | **Append-only, never deleted.** This is deliberate: it is the client-verifiable record that an account's key did not change behind your back | `000005_account_key_log.sql` |
| Abuse reports and suspensions (#1213) | `user_report` (reporter, reported account, reason, optional conversation/message ids — never content), `account_suspension` | Until the reporter or reported account is deleted (reports, both directions) / until the operator lifts it or the account is deleted (suspension) | `teardown.rs` `purge_user_rows`; operator: `scripts/suspend-account.sh` |
| Membership and social graph | `group_member`, `dm_channel_member`, `groups`, `channels`, `dm_channel`, `user_block`, `group_invite`, `group_join_request`, `group_invite_link`, `group_invite_link_redemption`, `group_emoji`, `account_recovery`, `user_preferences`, `device_enrollment_request` | Life of the account / conversation; **explicitly deleted** on account deletion and on group/DM teardown, table by table | `teardown.rs` `purge_user_rows` / `purge_group` / `purge_dm_channel`, enumerated in `USER_PURGED_TABLES` |
| Security events | `security_event` (`id`, `user_id`, `kind`, `device_id`, `created_at`, `metadata`) | **90 days** (#762, `POLLIS_DS_SECURITY_EVENT_RETENTION_DAYS`, §9); also **explicitly deleted** on account deletion | `messages.rs` `sweep_aged_records`; `teardown.rs` `purge_user_rows` |
| Attachment object index | `attachment_object` (`content_hash` → `r2_key`; `content_hash` holds the object's **random per-upload file id**, not a hash of the content) | **Reference-counted (#690):** collected only when no still-existing message references the file id — the same predicate for every caller, with no uploader identity in it. The DS holds no "which account stored this object" linkage (the `uploaded_by` / `registered_by` columns are unused and empty). | `messages.rs` `apply_delete_attachment` (conditional `NOT EXISTS (attachment_ref ⋈ message_envelope)`), `000032_attachment_ownership_retired.sql` |
| Wrapped file key rings (#1382) | `file_keystate` (`conversation_id`, wrapped ring, `key_version`, `(generation, epoch)`, `updated_by_device_id`) | One mutable row per MLS conversation, overwritten on every re-wrap/rotation; the ring is ciphertext under an MLS exporter key the DS never holds. **Explicitly deleted** on group/DM teardown | `000036_file_keystate.sql`; `teardown.rs` `purge_group` / `purge_dm_channel` |
| Attachment references | `attachment_ref` (`content_hash` = file id, `message_id`) | One declaration per (message that carries a file). The count is **derived** — a declaration counts only while its `message_envelope` exists — so a reference is released by deleting the message envelope (self/admin delete, envelope GC #689, retention #720, teardown), never by a dedicated call, and cannot outlive its message. Orphaned/forged declarations are reaped by `sweep_envelope_gc`. **New at-rest linkage — see the note below.** | `messages.rs` (`object_is_referenced`, `sweep_envelope_gc`), `000012_attachment_ref.sql` |
| Failed DS requests (#1305) | `ds_error_log` (time, method, matched route pattern, status, the response's `error` text, the `warn!`/`error!` lines the request raised, and a random per-request id the client generated) — **no user, device, email or IP column**; text is scrubbed (`pollis_api::redact::scrub`) before the insert | **72 hours and at most 500 rows**, whichever is smaller; written at most 30 rows a minute | The table's own `AFTER INSERT` trigger (`000035_ds_error_log.sql`) on every insert — no sweep and no code path can skip it; `pollis-delivery/tests/error_log.rs` |
| Push tokens | `push_token` | Until the device re-registers (upsert on `token`), **180 days** since last refresh (#762, `POLLIS_DS_PUSH_TOKEN_RETENTION_DAYS`, §9), account deletion, or Expo reporting the token `DeviceNotRegistered` in a ticket or receipt (#1423) | `000006_push_token.sql`; `sweep_aged_records`; `teardown.rs`; `push.rs` `prune_token` |
| Per-channel mutes (#1431) | `channel_mute` (user id, channel id, when muted) — read by the DS's push fan-out to skip a muted member | Until the user unmutes, leaves or is removed from the group, the channel or group is deleted, or the account is deleted | `000038_channel_mute.sql`; `profile.rs` `apply_set_channel_mute`; `teardown.rs` `purge_user_rows` / `purge_conversation_rows` / `purge_member_channel_mutes` |
| Sign-in code limits (#1879) | `otp_mailbox` (an HMAC-SHA256 of the normalized address under a key only the DS holds, never the address; send count, last send, wrong-guess count, lockout end, as unix times) — **no user, device, email or IP column**. The codes themselves are held in DS memory only, as salted hashes | While the row bears on a decision: until the mailbox's last code expires (10 minutes) or its lockout ends (15 minutes), whichever is later. Every insert or update deletes the rows past that | The table's own `AFTER INSERT` / `AFTER UPDATE` triggers (`000047_otp_mailbox.sql`) — no sweep; a `CHECK` admits only a 64-character hex hash in `mailbox`; `pollis-delivery/tests/otp_state_durability.rs` |
| Push tickets awaiting a receipt (#1423) | `push_ticket` (Expo ticket id, the push token it was sent to, a unix time) — **no user, conversation, device or IP column** | Until the GC sweep has fetched the ticket's receipt from Expo, and **24 hours at most** (Expo keeps receipts no longer); also deleted with the token and on account deletion | `000037_push_ticket.sql` (an `AFTER INSERT` trigger drops day-old rows); `push.rs` `check_push_receipts`; `teardown.rs` `purge_user_rows` |

### 2.1 Deletion is explicit

Deletion is **enumerated**, in `pollis-delivery/src/teardown.rs`:

- `purge_user_rows` names every main-DB table that carries a user identifier;
- `purge_group`, `purge_dm_channel` and `purge_conversation_rows` do the same per conversation;
- `purge_user_log_rows`, `purge_device_log_rows` and `purge_conversation_log` cover the commit-log DB,
  which is a separate database and so runs after the main transaction commits — deliberately in that
  order, so a rolled-back teardown can never have already destroyed a live conversation's MLS state.

Everything a single teardown does on the main DB is one transaction. Two properties are enforced by
tests rather than by review (`pollis-delivery/tests/teardown.rs`):

1. **Completeness.** `every_user_scoped_table_is_accounted_for` reads the *live* schema and fails if any
   table with a user-id column (by the column names the test lists, such as `user_id`, `sender_id`,
   `claimer_id`, `target_user_id`, `added_user_id`, `submitted_by`) is neither purged nor listed in
   `EXEMPT_FROM_USER_PURGE` with a stated reason. A
   new migration that adds a table with a `user_id` and forgets the teardown fails CI at the point it
   lands.
2. **Scoping.** `account_deletion_cannot_reach_another_account` and
   `group_deletion_cannot_reach_another_group` seed two fully-populated subjects and assert that
   deleting one changes not a single row of the other — the #878 defect class ("authorised on one field,
   deleted by another") pinned down for the destructive paths.

Three columns cannot be deleted or blanked, because they are `NOT NULL` pointers on rows that belong to
a conversation other people are still using: `groups.owner_id`, `dm_channel.created_by` and
`dm_channel_member.added_by`. Teardown **hands them over** to a remaining participant rather than
leaving a deleted user's id in a live row. For `dm_channel.created_by` that is also a correctness
property — it is an authorization input (the DM creator may remove a member), so left naming
a deleted account the capability belonged to somebody who can never authenticate again.

**What deletion does not remove** is listed in §8, and each exemption is also carried as a machine-read
reason string next to the code.

**Sealed sender.** `message_envelope.sender_id` (#607, migration `000008`) holds a sentinel rather
than the real sender; the true sender is authenticated inside the MLS ciphertext and recoverable only by
members. The stored row therefore does not show who sent what. The DS still observes the sender *live*
on the authenticated write, via the `X-Pollis-User` header — that is an at-rest guarantee, not an
in-flight one, and `docs/metadata-minimization-design.md` says so.

**Attachment references, stated plainly (#690).** Making attachment deletion correct required the client
to declare each reference to the DS, because the server cannot read message content and so cannot count
references itself. The chosen mechanism — `attachment_ref (content_hash, message_id)` — adds a
piece of *at-rest* linkage: joining `attachment_ref` to
`message_envelope.conversation_id` tells the operator which messages carry which stored object.
`content_hash` is a random file id minted per upload, so identical content uploaded twice is two
unrelated ids: the linkage is "this message carries this object", and two messages share an id only when
they reference the same upload. It does **not** reveal the file itself (plaintext is never seen, and nothing the DS holds is key material), the
filename (not in the R2 key, §5), or the sender (sealed, above). The `message_id` is **load-bearing**, not incidental: the
reference count is derived by joining `attachment_ref` to `message_envelope` on it (#690), which is what
makes a reference self-releasing when its message is deleted/GC'd and unforgeable by an unauthorized
caller. The considered alternative — an opaque, client-minted `ref_token` per reference — would expose
only a per-file *count*, never a linkage, but it cannot join to the envelope, so it loses exactly that
derived-release property: a device that loses local state can never release its tokens (references leak,
the object is never collected) and an admin deleting another member's message cannot release a token it
never held. That alternative fails deletion, which is the exact guarantee this ticket exists to establish,
so the design keeps the co-reference linkage as the price of a robust, collectable store.

---

## 3. IP addresses

**No client IP address is written to any database, log, or table — and none is readable in memory
either, operators included.** IPs are used for rate limiting only, and only as keyed hashes:

- **Delivery Service.** The client IP reaches the Cloudflare edge and the DS Worker's Durable Object,
  which deletes every IP-bearing header (`CF-Connecting-IP`, `CF-Connecting-IPv6`, `CF-Pseudo-IPv4`,
  `True-Client-IP`, `X-Real-IP`, `X-Forwarded-For`, `Forwarded`) and forwards only
  `X-Pollis-Client-Bucket` = HMAC-SHA256 of the IP under a random, non-extractable key generated per
  Durable Object instance (`pollis-delivery/worker/index.ts`). The container never receives a plain
  client IP. The DS rate limiter (`pollis-delivery/src/ratelimit.rs`) hashes that bucket again under its
  own 32-byte per-process key from the OS CSPRNG — held only in memory, never logged, persisted or
  configurable — and keys its fixed-window counters on `{tier}:{h}` (16 bytes of the HMAC, hex).
  Without the Worker in front (local runs) it falls back to `CF-Connecting-IP` / the first
  `X-Forwarded-For` hop, hashed the same way. Counters live in an in-process `Mutex<HashMap>`, pruned
  past 10,000 entries, for at most their tier's window (60 s or 600 s); none survive a restart. A
  raw-IP key is unrepresentable (`ClientKey` can only be built by hashing), and
  `pollis-delivery/tests/no_client_ip_exposure.rs` fails the build if DS or Worker code logs, traces or
  echoes an IP-bearing header, if the Worker stops stripping one, or if Workers Logs / Logpush are
  enabled in either wrangler config.
- **DS ops log (dev environment only).** With `POLLIS_DS_OPS_LOG` set, which only the dev
  config does, the DS keeps an in-memory ring of its last 1000 `request-otp` and rate-limited
  requests (`pollis-delivery/src/ops_log.rs`). Each entry holds the limiter's client pseudonym
  above (no IP), the truncated user agent with IP-shaped tokens redacted, the email's domain and a
  keyed per-process tag of the address (never the address), the endpoint, the outcome and a
  timestamp. It is readable only with the operator token and is gone at restart. Production never
  enables it (`pollis-delivery/tests/otp_ops_log.rs`).
- **DS failed-request log (#1305, every environment).** `ds_error_log` (§2) has no IP column,
  and the text it stores — the response's `error` and the request's own `warn!`/`error!` lines —
  passes `redact_ip_literals` and the email/token scrubber first. The request id it keys on is 16
  random hex characters the client mints per request; it identifies one request, never a user or
  device. The client's on-device diagnostics log (`pollis-core/src/diag.rs`) applies the same
  scrubber and never leaves the device unless the user copies it out.
- **Relay.** The relay reads the QUIC peer address for admission limits
  (`pollis-relay/src/server.rs`, accept loop and `handle_connection`) and immediately reduces it to an
  `IpKey` — 16 bytes of HMAC-SHA256 under a per-boot random key (`pollis-relay/src/ratelimit.rs`). Its
  per-IP connection and circuit maps hold only those keys. Nothing is logged.
- **OTA update server (`api.pollis.com/updates/`, the DS front-door Worker, #1250).** The mobile app's update check is a
  plain `GET` carrying only what the Expo Updates protocol needs (platform, runtime version,
  channel, the per-release current/embedded update ids). expo-updates would also attach an
  install-scoped random `EAS-Client-ID` and the previous launch's fatal-error text; the app
  overrides both with constants (`EAS-Client-ID: 00000000-…`, `Expo-Fatal-Error: ""`) through
  `updates.requestHeaders`, which both platforms apply last, so nothing in an update request
  identifies an install (`mobile/tests/ota-config.test.ts`). The Worker answers these routes
  itself (`pollis-delivery/worker/updates.ts`) — they never reach the DS container, so they never
  pass through the client-IP bucketing either: no IP is read at all. It reads only the routing
  headers, logs nothing, and both DS wrangler configs pin `observability.enabled: false` and
  `logpush: false`; `pollis-delivery/worker-tests/updates-no-client-ip.test.ts` fails the build
  otherwise. It keeps no per-client state at all — the `ota/` prefix of the release bucket holds
  only the published updates.
- **LiveKit (calls).** The media server must see the client IP to route media; it keeps no logs of it:
  nginx access and error logs are off, the PROXY-protocol hop that carried the client address to nginx is
  gone, LiveKit logs at `warn`, and both containers run with docker `logging: driver: none`
  (`livekit/`, see `livekit/DEPLOY.md` → "No client IPs in logs"). The STUN/TURN servers it hands
  clients are its own (`turn.udp_port` + `turn.domain` in `livekit/livekit.yml`): with no TURN/UDP
  listener configured LiveKit falls back to Google's public STUN servers, which would have every client
  send its address to a third party on every join.
- **Device names.** Clients register a device with an OS label ("macOS desktop"), never a hostname, which
  can embed an address; the DS also redacts any IP-shaped token in a device name or security-log note on
  write (`pollis-delivery/src/util.rs` `redact_ip_literals`).

**Cloudflare, as our edge, necessarily sees the client IP**,
as does any transit provider. An account member running `wrangler tail` against the DS Worker sees
live request metadata for the duration of the tail; that is a Cloudflare account permission, not a
config setting, and nothing is retained by it. Likewise **the edge provider's account administrators
can view client IPs in its security dashboards** (firewall/WAF events, analytics); that is outside what
our own configuration can turn off. The relay overlay (`docs/relay-overlay-design.md`) exists precisely to hide
client IPs from the first-party services, and is opt-in and off by default. This policy governs what
*we* retain; it does not claim our infrastructure providers see nothing.

---

## 4. Push notifications

- **Stored:** the Expo push token, the owning `user_id`, the platform string (`ios`/`android`), and an
  `updated_at` timestamp (`000006_push_token.sql`).
- **Retention:** until the same device re-registers (which upserts by token), 180 days without a refresh
  (`POLLIS_DS_PUSH_TOKEN_RETENTION_DAYS`, the GC sweep's `sweep_aged_records`), Expo reporting the token
  `DeviceNotRegistered` (`push.rs` `prune_token`), or account deletion.
- **Payload:** deliberately content-free and conversation-free (#1157). The notification
  carries a fixed title and body ("New message" / "You have a new message") plus
  `data: { h }` and nothing else — a random 128-bit handle, minted per notification per recipient.
  Never plaintext, never a sender, never a preview, never the conversation id
  (`pollis-delivery/src/push.rs`; `push::tests::the_payload_data_carries_only_the_handle` asserts it
  on the serialized blob, which is what the providers actually receive). A token registered with the
  `rich` capability (#1599, `push_token.caps`) gets the same `{ h }` in a different envelope — iOS adds
  `mutableContent`, Android drops the title/body for a data-only message — so the device can decrypt
  and write the preview itself; nothing new reaches the providers
  (`push::tests::every_payload_variant_carries_only_the_handle`). The DS learns one bit per token:
  whether that build supports it.
- **Handles:** `push_handle` stores `handle → (user_id, conversation_id, kind)` so the client can
  trade the handle for routing information at `POST /v1/push/resolve` over its own authenticated
  channel. Retention is `PUSH_HANDLE_TTL_DAYS` (7), swept on the DS's retention schedule, and rows
  go with account deletion and conversation teardown (`000030_push_handle.sql`).
  This is a real, if short-lived, addition to what the DS keeps: a record that a notification for a
  given conversation went to a given user. The DS already knew that while building the payload — the
  change is that it now lasts for the TTL rather than an instant, which is why the TTL is short and
  the sweep is not optional.
- **Read dismissals (#1508):** when a client's read cursor moves it tells the DS which conversation
  it read and how many seconds ago (`POST /v1/push/dismiss`), and the DS stamps `dismissed_at` on that
  user's handles for it (`000040_push_handle_dismissed.sql`; same TTL and sweep as the row). That
  record — this user read this conversation at about this time — is what the read-receipt envelope
  (on by default) already discloses; the encrypted read-cursor blob is unaffected. The DS then sends
  the user's devices a silent push whose `data` is `{ d: [handles] }` and nothing else: handles the
  providers already saw, so they learn which notifications were read, never a conversation. Android
  message pushes also carry the handle as the notification `tag`, a value already in the payload.
- **Third parties:** the payload transits Expo's push service and then APNs or FCM. Those services see
  the token, the fixed strings, and an opaque handle that is **different on every notification**. They
  do not see message content, the sender, or which conversation the notification is for — and because
  the handle never repeats, they cannot count activity per conversation either, which a stable
  per-conversation identifier would have allowed even without naming it.

---

## 5. Media (Cloudflare R2)

- Object keys are `media/{random file id}.enc` (`pollis-core/src/commands/r2.rs`). The key contains
  no user, device, group, or conversation identifier, and **no filename**, because a distinctive name
  would itself be metadata visible to the storage operator (#762).
- Objects are client-side encrypted (AES-256-GCM) before upload under a key derived from the conversation's
  MLS-wrapped file key ring (or, for Vault files, the account key); R2 and the DS hold ciphertext and no key
  material.
- Deletion is **reference-counted**, and the count is **derived** (#690): a declaration counts only while
  its `message_envelope` exists, so a reference is released by deleting the message envelope — through the
  ordinary self/admin delete, envelope GC (#689), the retention sweep (#720) or account/group teardown —
  and the object is collected only once no still-existing message references the file id. A reference cannot
  outlive its message, and releasing is not a separately-authorized action a caller can abuse. The DS is
  the chokepoint — `/v1/r2/presign` **refuses to mint a `delete`** for an object that still has a live
  reference (`broker.rs`, `object_is_referenced`), so a client that deleted its own message cannot blow
  away a blob another conversation still needs — not even the account that uploaded the bytes, since a
  blob can back several messages and the gate asks only whether anything still references it.
  There is no lifecycle rule and no automatic expiry. See the `attachment_object` / `attachment_ref` rows
  and the linkage note in §2.
- The on-device decrypted cache lives under the app sandbox, is itself encrypted with a key derived from
  the local database key, is capped at 500 MiB, and is LRU-evicted.

---

## 6. The public transparency ledger

The ledger is **permanent, public, and append-only by design** — that is what makes it a transparency
log. Nothing published there can be retracted, so what goes in is a privacy decision, not a retention
decision.

Three trees are published, all signed under one key with distinct context strings
(`verifiable-log/src/sth.rs`):

| Tree | Tenant | Leaf contents |
|---|---|---|
| MLS commit log | `mls-commit-log` | `conversation_pseudonym`, `generation`, `epoch`, `sender_pseudonym`, `seq`, `sha256(commit_data)` — **windowed pseudonyms, not raw ids** (#701, `verifiable-log-builder/src/commit_log.rs`) |
| Account-key directory | account-keys | user id + key version + key material commitment |
| Released binaries | binaries | release tag, platform, artifact name, digest, provenance URI |

The Signed Tree Head is `{tree_size, root_hash, timestamp, signature}` — ML-DSA-44 over
`context || tree_size || root_hash || timestamp`.

**The log of record is the persisted leaves, not the DS table (#1579).** The Delivery Service prunes
`mls_commit_log` below its retention floor (§1, #539), so that table is only a window onto the commit log.
The leaves the publisher has published are kept, append-only, in the transparency bucket at
`internal/commit-log-2-leaves.json` (with a copy per size under `internal/commit-log-2-leaves/<size>.json`),
the same way `internal/binary-records.json` holds the binaries tree. Each publish appends only the rows
above the last persisted `seq`, so pruning a row never changes or removes a published leaf, and a missing,
shrunk or rewritten leaves file stops the commit-log publish instead of rebuilding the tree. The two
retention rules therefore do not conflict: the DS keeps a commit's bytes only as long as members need them,
while the ledger keeps the commit's public leaf (pseudonyms, epoch, `seq`, `sha256(commit_data)`) forever.
The persisted file holds exactly the pseudonymous leaves `/v1/commit-log-2/entries.json` serves and no
real `conversation_id` or `sender_id`, so keeping it adds no retention beyond what the public ledger
already holds.

`commit-log-2` superseded the first tree on 2026-10-29; `/v1/commit-log-reset.json` lists the retired heads.
Every file of the retired tree stays served under `/v1/`.

**Windowed pseudonyms (#701).** The commit-log leaf carries no stable `conversation_id` and no real
`sender_id`, so nobody outside can build a longitudinal activity map (which groups exist, how often each
changes, who commits to them) from it. It carries **windowed pseudonyms** instead: `conversation_pseudonym =
H(dom || conversation_id || window)` and `sender_pseudonym = H(dom || conversation_id || sender_id ||
window)`, where `window = seq / PSEUDONYM_WINDOW_SIZE`. The pseudonym is stable *within* a window (so
the fork/monotonicity invariant stays publicly checkable inside one) and rotates at each boundary (so an
outside observer cannot link a conversation across windows, nor a user across conversations). Deriving a
pseudonym is keyless — it needs only `conversation_id`, which members and the server already hold but a
third-party scraper does not — so **no new signing key or custody problem is introduced** (contrast
`docs/sth-signing-key-custody.md`). The raw commit bytes were never published (the leaf commits to
`sha256(commit_data)` only). The account-key tree's `user_id` is deliberately **not** pseudonymised —
key transparency looks a user up by identity, so it is load-bearing. Full design:
`docs/transparency.md` → "Windowed pseudonyms in the commit log".

**Forks across a window boundary.** Windowing narrows the *public* invariant at window boundaries — a fork or
regression whose two branches straddle a boundary carries two different pseudonyms, so an outside-only
replay grouping by pseudonym cannot see it. This is inherent: a public commitment that let an outsider
re-link two windows would also defeat the unlinkability, so the two are mutually exclusive for a
third-party observer. Members and the builder still see it: a member (who knows
`conversation_id`) regroups every window and catches it, and `build_bundle` runs the invariant at full
strength over the real ids before pseudonymising, so the honest builder never emits a
boundary-straddling fork. Only a substituted/compromised publisher could, and a member catches that on
replay.

**Format version.** Served manifests carry a `format_version` that `pollis-verify` reads *before* it
verifies anything, so a binary older than the log reports version skew (exit code 2, "upgrade your
verifier"), never a verification failure.

---

## 7. Calls (LiveKit)

Call and realtime-signalling state is held in memory for the life of the connection
(`pollis-core/src/commands/livekit/realtime.rs`). No call record, participant list, or duration is
written to any first-party database. Media frames are E2EE under a key derived from the MLS group
exporter secret; the SFU forwards ciphertext.

LiveKit sees connection metadata inherent to routing — a participant, a room, and a time — and
(#828/#835/#836) it sees **pseudonyms, not identities**. Room names are HMAC-derived
(`pollis-delivery/src/room_id.rs::room_pseudonym`) and participant identities are SIV-encrypted per room
(`pollis-delivery/src/participant_id.rs::participant_pseudonym`), both applied at token-mint and roster
time, so the SFU cannot map a participant back to a user, a device, or a logical room. Tests pin both
directions (`never_leaks_the_user_or_device_id`, `never_leaks_the_logical_name`). Realtime signalling JSON
does not carry the sender (`docs/metadata-minimization-design.md` v2).

A pseudonym is stable within a room for the lifetime of the signing secret, so the SFU sees which
pseudonyms share a call, without being able to name them.

---

## 8. What we cannot delete on request

Being specific here is what makes the rest of the document credible:

1. **Transparency-ledger entries.** Permanent and public by construction. An account deletion does not
   and cannot remove past commit-log or account-key leaves.
2. **Your commits in a conversation that outlives you.** `mls_commit_log` is an append-only MLS epoch
   chain shared by every member. Deleting a departed member's commits from the middle of it would leave
   permanent epoch gaps that stop the *remaining* members from advancing, so those rows stay for as long
   as the conversation does — and go the moment it is deleted. Once your account is deleted they carry
   the commit blob and nothing that names you: the sender, added-account and added-device fields that
   referred to your account are blanked. The baseline schema's
   `sender_id ... ON DELETE CASCADE` would do the destructive thing here; it is inert because Turso runs
   with foreign keys off, and `teardown.rs` records it as a deliberate exemption so it is never "fixed"
   into firing.
3. **Message copies on recipients' devices.** Delivered messages live in recipients' local encrypted
   databases. We have no mechanism to reach them and no backup of history anywhere
   (`docs/security-explained.md`).
4. **Third-party operational data.** Cloudflare edge logs, Turso platform logs, LiveKit's own telemetry,
   and APNs/FCM delivery records are governed by those providers' policies, not this one.

---

## 9. Retention decisions

- **`security_event` retention: 90 days** (2026-08-04, #762). It is a per-user, timestamped,
  device-attributed audit trail — precisely the record a privacy-first product should age out. 90 days
  is long enough to investigate an incident someone noticed late, short enough that it is not a
  standing longitudinal profile of when each user changed devices. Enforced on the GC sweep
  (`sweep_aged_records`); `POLLIS_DS_SECURITY_EVENT_RETENTION_DAYS`, `0` retains forever.
- **`conversation_watermark`: deliberately not reaped** (2026-08-04, #762). These rows are load-bearing
  for envelope retention: the GC floor is computed from `reported_at`, so deleting a live member's
  watermark would make their device look like it never reported and could *advance* the floor past mail
  they have not collected. The rows go with the device, the conversation and the account.
- **`push_token` reap: 180 days since last refresh** (2026-08-04, #762). Clients re-register on
  launch, so `updated_at` is a direct liveness signal and a live device refreshes long before this.
  A reaped token costs a missed notification until the next launch re-registers it — a nuisance, not
  data loss. Enforced on the same sweep; `POLLIS_DS_PUSH_TOKEN_RETENTION_DAYS`, `0` disables.
- **Undelivered envelopes: bounded by member watermarks and device liveness** (§1, #720,
  `POLLIS_DS_WATERMARK_STALE_MONTHS`). **N = 12 months** (2026-08-04), the code default and also set
  explicitly in both `wrangler.{dev,prod}.jsonc`; the resulting third accepted loss is stated publicly
  (`website/learn.html`) and in `CLAUDE.md` / `docs/backend-core-invariants.md`. Rationale in §1.
- **Attachments are reference-counted** (#690). The count is derived by joining `attachment_ref` to
  `message_envelope`, so a reference is released by deleting its message (self/admin delete, envelope
  GC #689, retention #720, teardown) and cannot outlive it; the Turso row and the R2 delete gate both
  consult that derived count. The co-reference linkage this leaves at rest is described in §2.
- **The commit-log ledger publishes windowed pseudonyms** (#701, §6), not `conversation_id` /
  `sender_id`. The served bundle carries a `format_version`, and `pollis-verify` reports a too-old
  binary as version skew, not a verification failure.
- **No filenames in R2 object keys** (2026-08-04, #762). Keys are `media/<random file id>.enc`; a
  filename would be readable by the operator from an object listing alone, without decrypting anything.
  The key is stored per object in `attachment_object.r2_key` and download paths take it explicitly.

---

## 10. Sign-off — GIVEN 2026-08-28 (#877)

This document describes what the code does today, with citations, and it deliberately does not soften
anything. All three items reserved to the owner are now decided:

1. **The retention periods described are the intended policy** (§9), and each is enforced in code.
2. **The public wording derived from this document is signed off** and is now live as
   `website/retention.html` — including §6's answer to "what happens to the permanent ledger when I
   delete my account", which is the tension a reader spots first.
3. **The App Store / Play privacy-form answers** are unblocked and answered from §2, §3 and §4 (#708).

Related: #662 (parent), #702 (this ticket), #708 (mobile compliance paperwork),
#690, #699, #701, #720.
