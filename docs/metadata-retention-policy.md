# Metadata Retention Policy

**Status:** Engineering draft — **owner sign-off required before publication.** Resolves the engineering
half of #702; part of #662.
**Audience:** the owner (who signs it off), whoever fills in the App Store / Play privacy forms (#708),
and anyone writing the public "what we can and cannot see" page.
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
used for rate limiting and are never written to a database. Push tokens are stored until the device
re-registers or the account is deleted, and the notification they carry is content-free. Account
records, membership rows, and the account-key history are retained for the life of the account and
removed when the account is deleted. The public transparency ledger is, by design, permanent and
append-only; #701 replaces the commit log's formerly-stable conversation and sender identifiers with
windowed pseudonyms, so a third party can no longer build a longitudinal activity map from it (§6). That
change takes effect at the next full republish (the #672 / #699 key-rotation ceremony), because the leaf
encoding is a frozen contract.

---

## 1. Retention model: watermark-bounded, not time-bounded

The single most important thing to understand about Pollis retention is that **the primary mechanism is
a delivery watermark, not a TTL.**

An encrypted envelope is deleted when *every current member device of its conversation has reported a
fetch watermark past it* (`pollis-delivery/src/messages.rs`, the `CLEANUP_*` fragments). A 30-day TTL
used to be OR-ed with that gate; it was removed in #688 / PR #716 because either arm alone could delete,
so a device offline for a month silently lost mail. That contradicted invariant I3 in
`docs/backend-core-invariants.md`, which requires retention bounded by the slowest member device and
specifies **no TTL**.

The consequence, stated plainly because a privacy form will ask: **there is no maximum retention period
for an undelivered envelope.** A member who joins a busy conversation and never opens the app again
pins that conversation's envelopes indefinitely. This is a deliberate trade — correctness of delivery
over convenience of storage — and it must not be "fixed" by reintroducing a TTL. Bounding the worst case
via a *device staleness* rule (a device that has not reported in N months stops pinning retention) is
tracked in #720 and is the correct shape; it is not implemented today.

The MLS commit log has already solved the analogous problem: it prunes to a tier-1 floor (the minimum
applied epoch across current member devices) with a tier-2 per-conversation hard cap that bounds a stuck
member (`retention_tests::tier2_hard_cap_bounds_a_stuck_member`).

---

## 2. What the Delivery Service database retains

Schema: `pollis-core/src/db/migrations/` (main DB) and `pollis-core/src/db/migrations-log/` (commit-log
DB, split out in #420 Goal A).

| Data | Table | Retention | Enforced by |
|---|---|---|---|
| Encrypted message envelopes | `message_envelope` | Until every current member device has reported a watermark past the row. **No TTL.** Also removed immediately on user-initiated delete. | `messages.rs` `CLEANUP_*`; #688/#716 |
| Reactions | `message_reaction` | Life of the account; cascade-deleted with the user | `000000_baseline.sql` FK |
| Per-device fetch cursors | `conversation_watermark` | **Indefinite** — advanced, never deleted, except with the device | `apply_advance_watermark` |
| MLS commit history | `mls_commit_log` (log DB) | Pruned to the tier-1 floor (min applied epoch across current member devices) with a tier-2 per-conversation hard cap | commit-log retention code + `retention_tests` |
| Latest MLS group state | `mls_group_info` | Latest epoch only; overwritten, no history | `000001_commit_log_db.sql` |
| Per-device commit catch-up | `mls_commit_since` | Indefinite, upserted monotonically | `000003_mls_commit_since.sql` |
| Welcome messages | `mls_welcome` | Deduplicated per `(conversation, recipient, recipient_device, generation)`; superseded rows are replaced on upsert; cascade-deleted with the user | `000002_mls_welcome_unique_recipient.sql` |
| Key packages | `mls_key_package` | Claimed packages are marked `claimed=1`, not hard-deleted; cascade-deleted with the user | `000000_baseline.sql`, `000010` |
| Accounts | `users` | Life of the account; deleted on account deletion | `account.rs:808` |
| Devices | `user_device` | Revocation writes a `revoked_at` tombstone (the row is retained so revoked devices stop pinning retention and cannot re-authenticate); rows are hard-deleted on logout and on account deletion | `000004_user_device_revoked_at.sql`; `account.rs:626,715,722` |
| Account identity-key history | `account_key_log` | **Append-only, never deleted.** This is deliberate: it is the client-verifiable record that an account's key did not change behind your back | `000005_account_key_log.sql` |
| Membership and social graph | `group_member`, `dm_channel_member`, `groups`, `channels`, `dm_channel`, `user_block`, `group_invite`, `group_join_request`, `user_groups`, `user_dms` | Life of the account / conversation; cascade-deleted with the user | `000000_baseline.sql`, `000009_directory_index.sql` |
| Security events | `security_event` (`id`, `user_id`, `kind`, `device_id`, `created_at`, `metadata`) | **Indefinite** — no TTL; cascade-deleted with the user | `000000_baseline.sql` |
| Attachment dedup index | `attachment_object` (`content_hash` → `r2_key`) | **Reference-counted (#690):** collected only when no still-existing message references the hash. | `messages.rs` `apply_delete_attachment` (conditional `NOT EXISTS (attachment_ref ⋈ message_envelope)`) |
| Attachment references | `attachment_ref` (`content_hash`, `message_id`) | One declaration per (message that carries a file). The count is **derived** — a declaration counts only while its `message_envelope` exists — so a reference is released by deleting the message envelope (self/admin delete, envelope GC #689, retention #720, teardown), never by a dedicated call, and cannot outlive its message. Orphaned/forged declarations are reaped by `sweep_envelope_gc`. **New at-rest linkage — see the note below.** | `messages.rs` (`object_is_referenced`, `sweep_envelope_gc`), `000012_attachment_ref.sql` |
| Push tokens | `push_token` | Until the device re-registers (upsert on `token`) or the account is deleted. **No TTL and no reap of stale tokens** | `000006_push_token.sql` |

**Sealed sender.** Since #607 / migration `000008`, `message_envelope.sender_id` holds a sentinel rather
than the real sender; the true sender is authenticated inside the MLS ciphertext and recoverable only by
members. The stored row therefore no longer shows who sent what. The DS still observes the sender *live*
on the authenticated write, via the `X-Pollis-User` header — that is an at-rest guarantee, not an
in-flight one, and `docs/metadata-minimization-design.md` says so.

**Attachment references, stated plainly (#690).** Making attachment deletion correct required the client
to declare each reference to the DS, because the server cannot read message content and so cannot count
references itself. The chosen mechanism — `attachment_ref (content_hash, message_id)` — introduces a new
piece of *at-rest* linkage the operator did not previously hold: because `content_hash` is convergent
(SHA-256 of plaintext), joining `attachment_ref` to `message_envelope.conversation_id` tells the operator
which messages, and therefore which conversations, **share the same file** — durably and in the clear.
It does **not** reveal the file itself (plaintext is never seen), the filename (that already sits in the
R2 key, §5), or the sender (sealed, above). The `message_id` is now **load-bearing**, not incidental: the
reference count is derived by joining `attachment_ref` to `message_envelope` on it (#690), which is what
makes a reference self-releasing when its message is deleted/GC'd and unforgeable by an unauthorized
caller. The considered alternative — an opaque, client-minted `ref_token` per reference — would expose
only a per-file *count*, never a linkage, but it cannot join to the envelope, so it loses exactly that
derived-release property: a device that loses local state can never release its tokens (references leak,
the object is never collected) and an admin deleting another member's message cannot release a token it
never held. That alternative fails deletion, which is the exact guarantee this ticket exists to establish,
so we **accept the co-reference linkage as-is** as the price of a robust, honestly-collectable store.
Note this is **not** picked up by #701/#662 — those minimize the *public transparency ledger*, not DS
tables — so reducing this specific DS-side linkage (e.g. pseudonymising the `message_id`, if it can be
done without breaking the envelope join) would be its own follow-up, deliberately not planned as part of
#690.

---

## 3. IP addresses

**No client IP address is written to any database, log, or table.**

IPs are read for rate limiting only:

- The Delivery Service reads `CF-Connecting-IP` (set by Cloudflare), falling back to the first hop of
  `X-Forwarded-For`, else the sentinel `"unknown"` — `pollis-delivery/src/ratelimit.rs`. It is used as a
  fixed-window counter key (`{tier}:{ip}`) held in an in-process `Mutex<HashMap>` and pruned
  opportunistically past 10,000 entries. It is not persisted and does not survive a restart.
- The relay reads the QUIC peer address for admission rate limiting only
  (`pollis-relay/src/server.rs:266`); it is not stored or logged.

Caveat to state honestly rather than hide: **Cloudflare, as our edge, necessarily sees the client IP**,
as does any transit provider. The relay overlay (`docs/relay-overlay-design.md`) exists precisely to hide
client IPs from the first-party services, and is opt-in and off by default. This policy governs what
*we* retain; it does not claim our infrastructure providers see nothing.

---

## 4. Push notifications

- **Stored:** the Expo push token, the owning `user_id`, the platform string (`ios`/`android`), and an
  `updated_at` timestamp (`000006_push_token.sql`).
- **Retention:** until the same device re-registers (which upserts by token) or the account is deleted.
  There is no expiry sweep for tokens belonging to uninstalled apps.
- **Payload:** deliberately content-free. The notification carries a fixed title and body
  ("New message" / "You have a new message") plus `data: { conversationId, kind }` — never plaintext,
  never a sender, never a preview (`pollis-core/src/commands/push.rs`).
- **Third parties:** the payload transits Expo's push service and then APNs or FCM. Those services see
  the token, the fixed strings, and the conversation id. They do not see message content or the sender.

---

## 5. Media (Cloudflare R2)

- Object keys are `media/{sha256-of-plaintext}/{sanitized-filename}.enc`
  (`pollis-core/src/commands/r2.rs`). The key contains no user, device, group, or conversation
  identifier — but note the **original filename is preserved in the key**, so a distinctive filename is
  itself metadata visible to the storage operator.
- Objects are client-side encrypted (AES-256-GCM) before upload; R2 holds ciphertext only.
- Deletion is **reference-counted**, and the count is **derived** (#690): a declaration counts only while
  its `message_envelope` exists, so a reference is released by deleting the message envelope — through the
  ordinary self/admin delete, envelope GC (#689), the retention sweep (#720) or account/group teardown —
  and the object is collected only once no still-existing message references the hash. A reference cannot
  outlive its message, and releasing is not a separately-authorized action a caller can abuse. The DS is
  the chokepoint — `/v1/r2/presign` **refuses to mint a `delete`** for an object that still has a live
  reference (`broker.rs`, `object_is_referenced`), so a client that deleted its own message cannot blow
  away a blob another conversation still needs. There is no lifecycle rule and no automatic expiry.
  **Reclaiming the R2 blob + Turso row for a hash that no live message references (e.g. after envelope GC
  aged the last message out) is a separable server-side sweep and is NOT built here — #690 makes such an
  object *collectable* (the reference no longer pins it); a follow-up gives the DS the R2-delete capability
  to actually reclaim it.** See the `attachment_object` / `attachment_ref` rows and the linkage note in §2.
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
| MLS commit log | `mls-commit-log` | `conversation_pseudonym`, `generation`, `epoch`, `sender_pseudonym`, `seq`, `sha256(commit_data)` — **windowed pseudonyms, not raw ids** since #701 (`verifiable-log-builder/src/commit_log.rs`) |
| Account-key directory | account-keys | user id + key version + key material commitment |
| Released binaries | binaries | release tag, platform, artifact name, digest, provenance URI |

The Signed Tree Head is `{tree_size, root_hash, timestamp, signature}` — ML-DSA-44 over
`context || tree_size || root_hash || timestamp`.

**Former exposure and the #701 mitigation, stated plainly:** the commit-log leaf *used to* publish a
**stable `conversation_id` and a real `sender_id` in the clear**, so anyone could build a longitudinal
activity map — which groups exist, how often each changes, and who commits to them — with no access to
Pollis. #701 replaces both with **windowed pseudonyms**: `conversation_pseudonym =
H(dom || conversation_id || window)` and `sender_pseudonym = H(dom || conversation_id || sender_id ||
window)`, where `window = seq / PSEUDONYM_WINDOW_SIZE`. The pseudonym is stable *within* a window (so
the fork/monotonicity invariant stays publicly checkable inside one) and rotates at each boundary (so an
outside observer cannot link a conversation across windows, nor a user across conversations). Deriving a
pseudonym is keyless — it needs only `conversation_id`, which members and the server already hold but a
third-party scraper does not — so **no new signing key or custody problem is introduced** (contrast
`docs/sth-signing-key-custody.md`). The raw commit bytes were never published (the leaf commits to
`sha256(commit_data)` only). The account-key tree's `user_id` is deliberately **not** pseudonymised —
key transparency looks a user up by identity, so it is load-bearing (out of #701's scope). Full design:
`docs/transparency.md` → "Windowed pseudonyms in the commit log".

**Residual, named honestly:** windowing weakens the *public* invariant at window boundaries — a fork or
regression whose two branches straddle a boundary carries two different pseudonyms, so an outside-only
replay grouping by pseudonym cannot see it. This is inherent: a public commitment that let an outsider
re-link two windows would also defeat the unlinkability, so the two are mutually exclusive for a
third-party observer. It is **not** a gap for members or the builder: a member (who knows
`conversation_id`) regroups every window and catches it, and `build_bundle` runs the invariant at full
strength over the real ids before pseudonymising, so the honest builder never emits a
boundary-straddling fork. Only a substituted/compromised publisher could, and a member catches that on
replay.

**Coupling, stated because it is not written down elsewhere:** the leaf encoding is a **frozen
contract** — re-encoding history invalidates every signed root ever published and every cached inclusion
proof — so #701's code cannot pseudonymise *existing* history in place. It takes effect only at a **full
republish of the tree from scratch**, which is exactly what the #672 / PL-11 ML-DSA-44 key rotation
(executed by #699, `docs/sth-signing-key-custody.md` §7) already performs under the `sth:v2` contexts.
**#701 ships with that ceremony:** its builder is what makes the republished commit-log tree carry
pseudonyms. Until the ceremony runs, the *code* is complete and tested but the *live* ledger still
carries the pre-#701 (raw-id) leaves under the current roots.

**Stale auditors after the republish:** the republish is a breaking change to the served wire shape, so
the served manifests now carry a `format_version` an auditor's `pollis-verify` reads *before* it
verifies anything. A binary older than the republished log reports version skew (exit code 2, "upgrade
your verifier") — never a verification failure — because a tool whose whole job is answering "was this
tampered with?" must not raise a false alarm the day the key rotates (the same principle as the absent
log pin, #668). Upgrade any `pollis-verify` older than `v0.6.0` after the republish; the website
explorer is unaffected (it calls the live server's dynamic endpoint, not a static manifest). Details:
`docs/transparency.md` → "A stale `pollis-verify` after the republish".

---

## 7. Calls (LiveKit)

Call and realtime-signalling state is held in memory for the life of the connection
(`pollis-core/src/commands/livekit/realtime.rs`). No call record, participant list, or duration is
written to any first-party database. Media frames are E2EE under a key derived from the MLS group
exporter secret; the SFU forwards ciphertext.

LiveKit does see connection metadata inherent to routing: participant identity (`{user_id}:{device_id}`),
which room, and when. Realtime signalling JSON no longer carries the sender
(`docs/metadata-minimization-design.md` v2).

---

## 8. What we cannot delete on request

Being specific here is what makes the rest of the document credible:

1. **Transparency-ledger entries.** Permanent and public by construction. An account deletion does not
   and cannot remove past commit-log or account-key leaves.
2. **Message copies on recipients' devices.** Delivered messages live in recipients' local encrypted
   databases. We have no mechanism to reach them and no backup of history anywhere
   (`docs/security-explained.md`).
3. **Third-party operational data.** Cloudflare edge logs, Turso platform logs, LiveKit's own telemetry,
   and APNs/FCM delivery records are governed by those providers' policies, not this one.

---

## 9. Gaps this policy exposes (each needs a ticket or a decision)

Writing this document surfaced retention behaviour we have not decided on, only inherited:

- [ ] **`security_event` has no retention bound.** A permanent, per-user, timestamped audit trail keyed
      by `user_id` and `device_id` is exactly the kind of record a privacy-first product should age out.
      Decide a period (90 days is the obvious default) or justify indefinite retention.
- [ ] **`conversation_watermark` rows are never deleted**, including for conversations whose envelopes
      are long gone. Harmless in size, but it is a per-device activity record with no expiry.
- [ ] **`push_token` has no stale-token reap.** A token for an uninstalled app persists until the
      account is deleted.
- [ ] **No maximum retention for undelivered envelopes** (§1) until #720 lands a device-staleness bound.
- [x] **Attachment deletion is not reference-counted** (#690). **Done** — the reference count is derived
      by joining `attachment_ref` to `message_envelope`, so a reference is released by deleting its message
      (self/admin delete, envelope GC #689, retention #720, teardown) and cannot outlive it; the Turso row
      and the R2 delete gate both consult that derived count. The trade-off is a new co-reference linkage
      the operator can read at rest (§2 note), accepted as-is (not covered by #701/#662, which are
      ledger-side). **Remaining:** an object whose last message aged out is now *collectable* but not yet
      auto-*reclaimed* — a server-side R2+row sweep (giving the DS R2-delete capability) is the identified
      follow-up, unblocked by this ticket.
- [x] **The commit-log ledger publishes stable identifiers** (#701). Closed by windowed pseudonyms (§6):
      `conversation_id` / `sender_id` are replaced with per-window `H(...)` pseudonyms. The *code* is
      complete and tested; it takes effect on the *live* ledger at the next full republish (the #672 /
      #699 ML-DSA-44 key-rotation ceremony), since the leaf is a frozen contract. Residual (a
      boundary-straddling fork is invisible to an outside-only replay, but not to members or the honest
      builder) is named in §6. The republish is a breaking wire change, so the served bundle now carries
      a `format_version` and `pollis-verify` (bumped to `v0.6.0`) reports a too-old binary as version
      skew, not a verification failure — see §6.
- [ ] **The original filename is preserved in R2 object keys** (§5). Decide whether to hash it.

None of these is a launch blocker on its own. All of them are questions a store privacy form or a
security reviewer will ask, and the honest answer today is "indefinite".

---

## 10. Sign-off

This is an engineering draft. It describes what the code does today, with citations, and it deliberately
does not soften anything. The owner signs off on:

1. Whether the retention periods described are the intended policy, or whether §9's gaps should be closed
   before publication.
2. The public wording derived from this document (the "what we can and cannot see" page).
3. The answers to the App Store / Play privacy forms, which depend on §2, §3 and §4.

Related: #662 (parent), #702 (this ticket), #708 (mobile compliance paperwork, blocked on this),
#690, #699, #701, #720.
