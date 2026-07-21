# Design Spec: Metadata Minimization — Reducing What Turso and LiveKit Learn About Who Talks to Whom

**Status:** Partly shipped. **v1 (sealed sender), v2 (ciphertext size padding + LiveKit signalling minimization) are SHIPPED** — sealed row + MLS-credential attribution (migration `000008_message_envelope_sealed_sender.sql`; `pollis-core/src/commands/messages/framing.rs` for padding; `new_message` payload stripped of sender in `livekit_signalling`). Scope is honest and bounded: sealed sender is **at-rest only** — the DS still sees the sender live via the `X-Pollis-User` auth header until anonymous membership proofs land. **v1.5 (anonymous membership proof), v3 (per-conversation membership pseudonyms), and v4 (timing batching / private contact discovery) remain deferred → #489.** The original decision-document rationale below is retained as the design of record.
**Audience:** Pollis engineering + whoever owns the security roadmap.
**Scope:** the **application layer** — the *rows* Turso stores and the *JSON* LiveKit forwards. This is the deliberate complement to `docs/relay-overlay-design.md`, which covers the **network layer** (source IP). The two compose; **neither substitutes for the other** (§6). This spec does **not** redesign MLS, the transparency logs, or the E2EE content guarantee — those already hold. It reduces *metadata*.
**Author's stance:** written to be argued with. Every place I think a technique is fundamentally limited, I say so. The honest headline is: **sealed sender is real, cheap, and high-value; full social-graph hiding is fundamentally bounded because the server must route.** Do not let the second fact discourage the first.

If you skim two sections, skim §1 (the thesis) and §11 (the recommendation + issue summary).

---

## 0. TL;DR

- Turso today sees, in plaintext, the entire **social graph and per-message metadata keyed by stable `user_id`** — `message_envelope.sender_id` on every send, `group_member` / `dm_channel_member` / `user_block` rows, `mls_commit_log` / `mls_welcome` timing, ciphertext size (`docs/security-whitepaper.md:26`). LiveKit sees plaintext **signalling JSON** (`new_message`, `membership_changed`, …) carrying `sender_id` (`docs/security-whitepaper.md:30`, `pollis-core/src/commands/livekit/publish.rs:142-148`). The relay overlay hides your *IP*; it does **nothing** about any of this (`docs/relay-overlay-design.md:68-85`).
- **The single highest-value move is sealed sender.** MLS *already* authenticates the sender cryptographically inside the ciphertext — the `{user_id}:{device_id}` `BasicCredential` on the message's MLS signature (`docs/security-whitepaper.md:57-59`). The recipient recovers the true sender from that credential when it decrypts. So the server-visible `message_envelope.sender_id` (`000000_baseline.sql:92`) is **cryptographically redundant** and can be dropped/blinded without weakening authentication. Today the ingest path *ignores* the MLS credential and trusts the envelope column — that's the bug-shaped gap this spec closes.
- **Private membership is fundamentally bounded.** The server must know *which recipients to route an envelope to*, so it must know group/DM membership at *some* granularity. You can pseudonymize it (per-group opaque ids), you cannot eliminate it without a mixnet or PIR-per-read — over-scoped for a fast consumer messenger. Be honest about this.
- **Timing/size** leaks (commit/welcome timing, ciphertext size) are cheaply narrowed by **size padding** (bucket ciphertext) and, later, decoupling commit-submission timing. Padding is nearly free; batching trades latency and is a v3 lever.
- **LiveKit signalling** should stop carrying `sender_id` in cleartext — it's a wake-up ping, and the recipient re-derives the sender from the decrypted envelope anyway. Minimize the JSON to an opaque conversation handle; the sender field is pure leakage.
- **Everything here is server-side / protocol-only. UX is byte-for-byte identical** — no new user steps, no degraded experience. That is a hard constraint (CLAUDE.md product principles), and it's achievable because the recipient already has the MLS state to attribute messages without the server's help.

**Recommendation (full version §11):** Ship **v1 = sealed sender** first (additive `message_envelope` migration, credential-based attribution on ingest, DS auth that proves *membership* without binding the *sender identity* into the routed row). Then **v2 = LiveKit signalling minimization + ciphertext size padding** (both cheap, both invisible). Treat **private membership pseudonymization (v3)** and **timing batching / private contact discovery (v4)** as demand-gated, and be explicit that they buy diminishing returns against an irreducible routing floor.

---

## 1. Thesis: The Server-Visible Sender Is Redundant; The Routing Set Is Not

Two facts about Pollis's data model drive this entire spec.

**Fact 1 — the sender id is redundant.** Every application message is a TLS-serialised MLS `MlsMessageOut` (`pollis-core/src/commands/mls/group_state.rs:1229-1238`). When a recipient decrypts it, `MlsGroup::process_message` returns a `ProcessedMessage` whose `credential()` is the sender's `BasicCredential` — the UTF-8 string `{user_id}:{device_id}` (`docs/security-whitepaper.md:57-59`, `provider.rs::make_credential`). MLS *cryptographically authenticates* that credential: a member cannot forge a message as another member, because the message is signed under the sender's per-device MLS signing key, which is cross-signed by that user's account identity (`docs/security-whitepaper.md:53-59`). **The recipient therefore already knows, with cryptographic certainty, who sent every message — from inside the ciphertext, without consulting the server.**

Yet the delivery path *also* writes the sender in the clear: `send_message` puts `sender_id` into the `message_envelope` row (`pollis-core/src/commands/messages/send.rs:143-151`, column at `000000_baseline.sql:92`), and the ingest path attributes the message by reading that **plaintext column** (`ingest.rs:378` unpacks `sender_id` from the envelope row and passes it straight into the local `message` INSERT at `ingest.rs:404-407`) — it never consults the authenticated MLS credential. So the server learns the sender for free, the client double-stores it, and the authentication that would make the plaintext copy unnecessary is sitting unused inside `try_mls_decrypt` (`group_state.rs:1267-1285`, which currently discards everything but the application bytes).

> **The plaintext `sender_id` is a metadata leak with no security value.** Removing it costs the server nothing it needs (it routes by *recipient*, §1.2) and costs the client nothing (it re-derives the sender from the MLS credential it already processes). This is the sealed-sender opportunity, and it is unusually clean in Pollis because MLS does the authentication work Signal's sealed-sender has to bolt on with a separate certificate.

**Fact 2 — the routing set is not redundant.** The server's *job* is to hold an envelope until every current member's device fetches it (`docs/security-whitepaper.md`, the offline-delivery model; `messages/read.rs` + `ingest.rs` fetch by `conversation_id` and membership). To do that it must know **which conversation an envelope belongs to and which users/devices are members of that conversation** — that is exactly `group_member` / `dm_channel_member` and `message_envelope.conversation_id`. You can *pseudonymize* those identifiers, you cannot *delete* them, short of making every client fetch every envelope (PIR / mixnet economics, §3.4, §7). **This is the irreducible floor.** Any honest metadata-minimization story names it up front.

The whole spec follows from holding these two facts at once: **strip what's redundant (sender id, cleartext signalling, size), pseudonymize what must exist (routing set), and don't pretend you can erase routing without becoming a different (much slower) product.**

### 1.1 The exposure we are minimizing (from `docs/security-whitepaper.md:24-34`)

| Surface | What the operator sees today | Source |
|---|---|---|
| Turso — social graph | `group_member`, `dm_channel_member`, `user_block` rows (who is in what, who blocked whom) | `000000_baseline.sql:48-54,74-80,144-149` |
| Turso — per-message | `message_envelope.sender_id`, `conversation_id`, `sent_at`, ciphertext **size** | `000000_baseline.sql:89-97`; `send.rs:143-151` |
| Turso — MLS control plane | `mls_commit_log` (sender, epoch, timing), `mls_welcome` (recipient, timing), `mls_key_package` availability | `000000_baseline.sql:106-135` |
| Turso — directory | `users` (id, email, username), `search_user_by_username` lookups | `000000_baseline.sql:162-169`; `user.rs:147-169` |
| LiveKit — signalling | plaintext JSON: `new_message`, `membership_changed`, `edited_message`, … each carrying `sender_id` | `publish.rs:142-148,183-189,226-232,263-267` |
| Network | source IP, Hrana streams, RTP routing | `docs/security-whitepaper.md:26,30` — **defended by the relay overlay, not here** |

### 1.2 Routing is by recipient, not sender — which is what makes sealed sender possible

The critical asymmetry: **delivery does not need the sender.** An envelope is fetched by members of its `conversation_id` (`ingest.rs:148-159` selects envelopes `WHERE conversation_id = ?` for a user who is a member; membership is checked at `ingest.rs:43-55` / `ingest.rs:453-461`). Nothing in the read path filters or routes by `sender_id`. The sender column exists only so the *client* can label the message — a job MLS already does better. That is precisely why a sender can be blinded while routing keeps working: **the server routes on the recipient axis; the sender axis is display metadata it has no operational need for.**

---

## 2. Sealed Sender (v1 — the headline slice)

**Goal:** the server no longer learns *who sent* a given message envelope. It still routes the envelope to the right recipients (by `conversation_id`), and recipients still attribute the message to the correct sender (via the MLS credential inside the ciphertext).

### 2.1 What changes, precisely

**Attribution moves from the envelope column to the MLS credential.** `try_mls_decrypt` (`group_state.rs:1267-1285`) already runs `process_message` and has the `ProcessedMessage` in hand; today it throws away everything but `into_bytes()`. Change it to *also* return the authenticated sender credential (`parse_credential_user_id` on `processed.credential()` — the helper already exists, `provider.rs:71`). `decrypt_and_persist_one` (`ingest.rs:372-431`) then writes the local `message.sender_id` from the **credential**, not the envelope column. This is strictly *more* trustworthy than today: the current code trusts a server-writable plaintext column; the new code trusts the MLS-authenticated identity.

**The envelope schema change is additive** (CLAUDE.md migration constraint). `message_envelope.sender_id` is `NOT NULL` today (`000000_baseline.sql:92`), so we cannot drop or null it without breaking the currently-shipped app that still reads it — that's an unsafe change per CLAUDE.md. The additive path:

- **Migration `NNNNNN_message_envelope_sealed_sender.sql`** — take the next free migration number at implementation time; do not pre-claim one here (several program docs each "reserve" the next slot, and at least one candidate number belongs to a previously-reverted DS-trigger migration, `docs/mls-reconcile-hardening.md` — a collision hazard). Add nullable columns. No `DROP`, no nullability tightening. Sketch:
  ```sql
  -- Additive: a blinded routing tag replaces the need to read sender_id.
  ALTER TABLE message_envelope ADD COLUMN sealed INTEGER NOT NULL DEFAULT 0;
  -- (No new sender column needed — the sender lives in the ciphertext.)
  ```
- **Transition writes.** Old clients keep writing a real `sender_id`. New clients, when sending, write `sealed = 1` and put a **non-identifying placeholder** in the still-`NOT NULL` `sender_id` column — a per-message random token, or a fixed sentinel like the string `sealed`. The column stays populated (satisfying `NOT NULL` for the old app's SQL) but carries **no information** the server can join to a user.
- **Old readers still work.** A pre-migration client ingesting a `sealed=1` envelope reads the sentinel `sender_id` and would mislabel the message — *except* that the safe rollout is: **ship the credential-based attribution reader first, wait for uptake, then flip senders to sealed.** This is the standard additive two-release dance CLAUDE.md prescribes ("first ship an app that stops using the thing, wait for uptake, then …"). Release N teaches every client to attribute from the MLS credential (ignoring the envelope column); release N+1 turns on sealed sending. Between them, correctness holds because new readers already ignore `sender_id`.

**The DS auth question — the subtle part.** The Delivery Service authenticates every write by a **device signature**, and the request carries `X-Pollis-User` + `X-Pollis-Device` headers (`pollis-delivery/src/auth.rs`, the signing contract). `apply_send_message` then gates: *is this authenticated user a member of the conversation?* (`pollis-delivery/src/messages.rs:250-277`). So **even with a blinded envelope column, the DS still sees the sender's `user_id` in the auth headers.** Sealing the *row* without addressing the *auth channel* moves the leak from a stored column to a request header — a real improvement against **at-rest** exposure (a Turso breach or the stored `message_envelope` table no longer reveals sender-per-message) but **not** against a DS operator watching live requests. To close the live-request axis you need one of:

  - **(A) Membership-proof-without-identity.** Replace "prove you are user U and U is a member" with "prove you hold a credential that is a member of conversation C, without revealing *which* member." This is a **blinded/anonymous credential** or a **group-signature / ring-signature over the member set** — the client proves membership in the routing set without naming itself. Cryptographically real (BBS+ anonymous credentials, or a keyed-verification anonymous credential à la Signal's sealed-sender "delivery token"), but a meaningful build: the DS must issue per-conversation membership credentials and verify anonymous proofs. **This is the honest ceiling of sealed sender** and the reason Signal's own sealed sender pairs a blinded delivery certificate with the server-side check.
  - **(B) Decouple the write from the identity via the relay/DS split.** If the envelope write rides the relay overlay (`docs/relay-overlay-design.md`), the *IP* is already hidden; combine that with a **membership token** (a short-lived, per-conversation opaque bearer the DS issues to members out-of-band) so the send request proves "a member of C is writing" without an `X-Pollis-User` header. The token is unlinkable to the user across sends if minted with a blind signature. This is a lighter-weight approximation of (A). **Sequencing caveat:** the relay overlay is deferred until after the PQ-hybrid work in the agreed program order, so this infrastructure will not exist by the time v1.5 is reachable — prefer (A) or (C) for that reason.
  - **(C) Accept the honest-but-curious-*at-rest* scope for v1.** Ship the sealed *row* (breach/subpoena/`message_envelope`-dump defense) and the credential-based attribution now; ship anonymous membership proofs (A/B) as v1.5 gated on threat-model demand. **This is what I recommend for the first slice** — it's the 80% value (the persisted social-graph-by-message artifact disappears) at 20% of the crypto cost, and it's honest about what remains (§7).

> **Decision to force:** v1 = sealed row + credential attribution (scope: at-rest / breach / subpoena defense). v1.5 = anonymous membership proof for the DS auth channel (scope: live honest-but-curious DS operator). Do not let v1 be marketed as "the server can't tell who sent it" until v1.5 lands — until then the DS *can*, via the auth header, in real time. Say so.

### 2.2 Abuse / rate-limiting without a server-visible sender

The objection to sealed sender is always the same: *if the server can't see the sender, how do you rate-limit or stop spam?* Pollis's structure answers most of it for free:

- **Membership is the gate, not identity.** A sealed send is still authorized only if the writer proves membership in `conversation_id` (§2.1 (A)/(B), or the current member-check in `apply_send_message:259`). A non-member cannot inject into a conversation, sealed or not. This is *stronger* than open messengers where anyone can address anyone: Pollis conversations are closed sets, so "who can send to this conversation" is already tightly bounded regardless of sender visibility.
- **Rate-limit per conversation, or per anonymous-credential-epoch.** Instead of "N messages/min per user," use "N messages/min per conversation" (visible without the sender) plus, under scheme (A), a **rate-limited anonymous credential** — the DS issues each member a bounded number of single-use sending tokens per epoch, so a spammer is throttled without being identified (this is exactly the "sealed sender + rate-limiting" pattern from Signal's design; the anonymous credential carries a hidden counter).
- **Block enforcement is unaffected.** DM block suppression happens *client-side at send time* (`send.rs:45-92`, `blocks::is_blocked_either_way`) — the sender's own client refuses to encrypt/post to a blocked peer. Sealing the envelope doesn't touch this: the block check runs before the envelope is ever built. Group-channel blocks are already render-side only (`docs/security-whitepaper.md:411-421`), also unaffected.
- **The device-signature floor still exists.** Even under sealed sending, the DS still verifies *a real Pollis device signed the request* (`auth.rs`) — it just may not learn *which user*, under scheme (A). Sybil/abuse throttling keyed on "valid device attestation" survives.

### 2.3 Delivery-receipt implications

Pollis does not have per-message read receipts in the envelope path — delivery is watermark-based (`conversation_watermark`, `ingest.rs:152-159`), advanced per `(conversation, user, device)` via `/v1/watermarks/advance` (`pollis-delivery/src/messages.rs:591-636`). The watermark **does** reveal `user_id` (it's the read-position of a specific user's device). Sealed sender is about the *send* axis; watermarks are a *read* axis leak and are **separate** — they tell the server "user U's device D has read conversation C up to time T." That's routing-adjacent (it's how envelope GC knows when everyone's caught up, `messages.rs:74-114`) and largely **irreducible** for the same reason as membership: the server must know when it can drop an envelope. Note it honestly; do not claim sealed sender hides read progress. A future pseudonymous-membership scheme (§3) would pseudonymize the watermark's `user_id` too, since it's the same routing set.

### 2.4 Performance & UX cost

- **Crypto:** *zero new crypto on the read path* for v1 — `process_message` already runs; we just stop discarding its credential. On the write path, v1 adds nothing (blinded column is a constant). v1.5's anonymous membership proof adds one credential presentation per send (sub-millisecond for BBS+/KVAC; amortizable by caching a per-epoch credential).
- **Bandwidth:** negligible — one nullable int column; the blinded `sender_id` sentinel is *smaller* than a real ULID.
- **Latency / round-trips:** none added in v1. v1.5 adds a periodic (per-epoch) credential-issuance round-trip, off the interactive path.
- **UX:** **identical.** The user sends and reads messages exactly as before; attribution in the UI comes from the MLS credential instead of the envelope column, which the user cannot perceive. No new setting, no new step. This is the zero-user-burden property: it holds because the client already possesses the MLS state needed to attribute without the server.

---

## 3. Private Membership / Group Metadata (v3 — honestly bounded)

**The exposure:** `group_member` / `dm_channel_member` are the social graph in rows (`000000_baseline.sql:48-54,74-80`). The server sees the complete "who is in what." This is the metadata that survives *even after* sealed sender and *even after* the relay overlay — it is the hardest problem, and the one where overclaiming is most tempting.

### 3.1 The irreducible core (say this first)

**The server must be able to route an envelope to a conversation's current members.** `catch_up_mls_group_interleaved` fetches envelopes for a conversation the user is a member of (`ingest.rs:43-55`); envelope GC needs the full device/watermark set (`pollis-delivery/src/messages.rs:74-114`). So *some* mapping "conversation ↔ set of recipient handles" **must** exist server-side. You cannot delete it. The only question is **how identifying those handles are.**

### 3.2 What's achievable: per-group pseudonymous ids

Replace the stable, globally-joinable `user_id` in membership rows with a **per-conversation pseudonym** — a value derived so that the same user in two different conversations gets two *unlinkable* handles:

```
member_handle(conversation_id, user_id) = HKDF( shared_conv_secret, user_id )   -- derived by members
```

The server stores `(conversation_id, member_handle)` instead of `(conversation_id, user_id)`. It can still route (it knows the handle set per conversation) but **can no longer join a user across conversations** — it can't tell that the `alice` in group G and the member in DM D are the same person. This defeats the "assemble one person's entire social graph" attack while preserving routing.

**Honest limits of pseudonymous ids:**
- The **membership-change events** (adds/removes, i.e. MLS commits in `mls_commit_log`, and `RosterChanged` in `realtime.rs:158-175`) still correlate handles *within* a conversation and reveal *cardinality* and *churn timing*. The server learns "conversation C has 5 members and one joined at time T" — just not who.
- **The device→handle mapping for delivery** must exist somewhere the server can act on, and the *device* (`user_device`) is still keyed by `user_id` for cross-signing (`000000_baseline.sql:150-156`). Fully unlinking devices from users breaks the cross-signing trust chain (`docs/security-whitepaper.md:161-182`). So the *device registry* remains a linkable point; pseudonymizing membership rows narrows the graph but the device directory is a residual join.
- **Intersection attacks.** If the server sees the handle-sets of many conversations plus timing/size side-channels, it can attempt statistical re-linking (two handles that always go online together are probably one user). Pseudonymization raises the cost; it does not make it zero. This is why it's v3, not v1 — the marginal privacy over sealed-sender is real but softer, and the migration (re-keying membership) is heavier.

### 3.3 What's NOT achievable without becoming a different product

- **Oblivious membership checks / PIR reads** (the server routes without learning *which* conversation you're fetching) require Private Information Retrieval or a mixnet: every client effectively fetches from an oblivious structure, paying Ω(√n) or full-download bandwidth. This breaks Pollis's "1 hop, simple and fast" posture (`ARCHITECTURE.md`) and its offline-delivery efficiency. **Out of scope** — name it as the theoretical ceiling, not a roadmap item.
- **Hiding conversation *existence* and *cardinality*** from the server is incompatible with the server holding envelopes for offline members. Fundamentally irreducible in a store-and-forward design.

### 3.4 Recommendation for membership

Ship **pseudonymous per-conversation membership handles as v3, gated on demand**, and be explicit in any user-facing copy: *"the server knows conversations exist and how many members each has, but (after v3) cannot link a member across conversations or to a global identity."* That is a true, defensible, bounded claim. Anything stronger requires PIR/mixnet and a different product.

---

## 4. Timing / Size Metadata (v2 padding, v3+ batching)

**The exposure (`docs/security-whitepaper.md:26`):** ciphertext **size** on `message_envelope`, and **MLS commit/welcome timing** (`mls_commit_log.created_at`, `mls_welcome.created_at`, `000000_baseline.sql:106-135`).

### 4.1 Ciphertext size — pad to buckets (cheap, invisible)

**Status: SHIPPED (issue #331 v2, this slice).** Text plaintext is padded to size
buckets before `try_mls_encrypt` and stripped after `try_mls_decrypt`. Framing
module: `pollis-core/src/commands/messages/framing.rs`; padding applied at the
send/edit encrypt sites (`messages/send.rs`, `messages/edit_delete.rs`) and
stripped at the ingest decrypt sites (`messages/ingest.rs`, both the `message`
and `edit` paths). Attachment envelopes are left unpadded (scoped via
`edit_delete::is_attachment_content`). Sealed sender (v1) shipped earlier; the
rest of v2 (LiveKit signalling minimization) and v1.5/v3/v4 remain unbuilt.

MLS application ciphertext length tracks plaintext length, so the server (and anyone reading `message_envelope`) learns approximate message length — enough to distinguish "ok" from a paragraph, to fingerprint forwarded content, or to correlate a send with a receive by size.

- **Fix (implemented):** pad plaintext to fixed **size buckets** before `try_mls_encrypt` (`group_state.rs:1229`). The shipped scheme is **PADMÉ** (log-bucketed, ~12% overhead worst case) above a 256 B floor bucket, so every short message (empty, "ok", a single emoji) collapses to one observable 256 B size and larger messages land in coarse power-of-two-ish bands. Padding is stripped after decrypt via a length prefix on the real plaintext inside the padded buffer.
- **Framing (implemented).** Inside the MLS ciphertext the padded buffer is `[version byte 0xF5][u32 LE real length][real plaintext][zero padding to bucket]`. The version byte is chosen from `0xF5..=0xFF` — bytes that can never begin a valid UTF-8 string — so a **legacy unpadded** message (always valid UTF-8) and an **unpadded attachment envelope** (JSON beginning with `{`) are both detected by their first byte and returned verbatim by `strip`. This is the version-byte back-compat gate: old and new clients interoperate, and a sibling framing version stays unambiguous. **`0xF6` is now in use** for the **redaction control frame** — `[0xF6][u32 LE id-len][target message id][zero padding to bucket]` — that carries an E2EE "delete for everyone" (`pad_redaction`/`classify` in the same module). It is padded to the same size buckets as text, so a redaction is length-indistinguishable from a short message and the server never learns which message was deleted (see [mls.md](../.codesight/wiki/mls.md#message-deletion--delete-for-everyone-e2ee-redaction)).
- **Cost:** a few hundred bytes average bandwidth per message; **zero latency, zero UX**. Attachments already ride convergent-encrypted R2 blobs whose size is inherent (dedup depends on it) — padding there would break dedup, so size-padding is scoped to **text envelopes only**.
- **Additive:** pure client-side change to the plaintext framing inside the MLS ciphertext; no schema change, no server change. Old and new clients interoperate (padding is inside the encrypted payload; only members decrypt it, and the framing version byte gates the strip).

### 4.2 Commit/welcome timing — decouple submission (v3+)

`mls_commit_log` and `mls_welcome` timestamps reveal *when* membership changed and *when* a device was invited — a strong side-channel for "who joined whose group when." The commit-submission ordering is currently tightly coupled to the user action (`reconcile_group_mls_impl`, `docs/security-whitepaper.md:208-217`).

- **Lever:** **jitter / batch** commit and welcome submission — add randomized delay, or batch multiple control-plane writes, so the server can't tie a commit to the exact instant of a user action. This is genuinely useful only against a *timing-correlating* adversary and it **trades latency**: a delayed commit delays when a new member can decrypt, brushing against the "messages must work" invariant (CLAUDE.md) and the split-brain ordering guarantees (`docs/security-whitepaper.md:208-217`). Must be bounded (seconds, not minutes) and must never reorder the commit/remote/merge sequence.
- **Verdict:** **v3+, demand-gated.** Padding (§4.1) is the cheap timing/size win; commit-timing batching is a fiddly, latency-taxing lever with real correctness hazards. Do not build speculatively.

### 4.3 Cover traffic

Full timing defense (indistinguishable send/no-send) needs **cover traffic** — dummy envelopes on a schedule. This is mixnet-grade, bandwidth/battery-costly, and over-scoped for a fast consumer messenger (same conclusion the relay doc reaches for its §6.3 mixnet option, `docs/relay-overlay-design.md:215-221`). Name it as the ceiling; do not build it.

---

## 5. Realtime Signalling Leakage (v2 — cheap, invisible)

**The exposure:** LiveKit forwards **plaintext JSON** data packets. `publish_new_message_to_room` sends `{type: "new_message", channel_id, conversation_id, sender_id, sender_username}` (`publish.rs:142-148`). LiveKit operators read all of it (`docs/security-whitepaper.md:30,388-389`). So even after sealed sender hides the sender from *Turso*, LiveKit still sees `sender_id` (and `sender_username`) on every send, plus `membership_changed` / `edited_message` / `deleted_message` payloads (`publish.rs:183-189,226-232,263-267`).

### 5.1 The signalling is a wake-up, not data — so minimize it

The `new_message` event is only a **hint to fetch** — the actual ciphertext comes from Turso, and offline recipients catch up via `poll_pending_messages` regardless (`send.rs:153-160` comment; `docs/security-whitepaper.md:243`). The recipient re-derives the true sender from the decrypted MLS credential (§2.1). **Therefore `sender_id` and `sender_username` in the LiveKit payload are pure leakage with no functional need.**

- **Fix (v2):** strip identifying fields from the wake-up. `new_message` becomes `{type: "new_message", conversation_id}` — or even just an opaque per-conversation wake token — with **no sender**. The client, on receiving it, ingests the conversation and learns the sender from MLS. `sender_username` disappears entirely (it was a UI nicety the client can now fill from its own cache post-decrypt). Same for `edited_message` / `deleted_message`: they need `message_id` + `conversation_id`, not `sender_id` / `deleted_by` (the client re-derives actor from the durable envelope/tombstone it ingests anyway; `deleted_by` is already re-derivable from the `type='delete'` envelope's authenticated writer).
- **Stronger (v2.5):** the wake token can be a **per-conversation pseudonym** (same construction as §3.2) so LiveKit can't even use `conversation_id` to build the graph — it just sees "wake token X fired." Because LiveKit rooms are already keyed by conversation/group (`send.rs:155-181`), the *room* still leaks the conversation to LiveKit; fully hiding that requires moving wake-ups off LiveKit (see §5.2). Pseudonymizing the payload is the cheap 80%.
- **Cost:** **zero** — the payload gets *smaller*; the client does one extra ingest it was already going to do; UX identical. This is the second-cheapest win after size padding.

### 5.2 Or move notification off the metadata-leaking path

An alternative to minimizing LiveKit JSON is to route wake-ups through the **push path** (`push.rs::notify_new_message`, already content-free per `send.rs:219-248`) or the relay overlay, so LiveKit isn't in the notification metadata path at all. This is a bigger architectural move; v2's payload minimization captures most of the value first. Note it as a follow-on, not a prerequisite.

### 5.3 The membership_changed / RosterChanged leak

`membership_changed` (`publish.rs:263-267`) and the richer `RosterChanged` event (`realtime.rs:158-175`, carrying `joined_user_ids` / `left_user_ids` / device ids) are **broadcast to a LiveKit room in cleartext** — a direct social-graph leak to LiveKit. These carry real `user_id`s. Minimize to "the roster changed, refetch" (drop the id lists from the *LiveKit* broadcast; keep them only on the *local* sink where they render banners). The refetch then goes through Turso (which, post-v3, sees pseudonymized handles). This is part of v2.

---

## 6. Composition With the Relay Overlay + Sealed Sender (the layer table)

The relay overlay (`docs/relay-overlay-design.md`) and this spec attack **orthogonal** axes. Getting this table right is the whole point of writing both docs.

| Metadata axis | Hidden by **relay overlay** (network) | Hidden by **sealed sender + this spec** (application) | Fully hidden only by |
|---|---|---|---|
| Source **IP** of the connecting user | ✅ (this is its entire job, `relay-overlay-design.md:59-66`) | ❌ (application layer can't touch packet headers) | relay overlay |
| **Sender identity** on a stored message row | ❌ (relay sees ciphertext-to-a-known-host, not the row's sender field, `relay-overlay-design.md:76-79`) | ✅ v1 (sealed row) / ✅ live-request v1.5 (anon membership) | sealed sender |
| **Which conversation** an envelope belongs to (server side) | ❌ | 🟡 pseudonymized v3; **existence irreducible** (§3.1) | pseudonyms (partial) |
| **Social graph** (who is in what), server side | ❌ (still keyed by `user_id` in rows, `relay-overlay-design.md:70-74`) | 🟡 pseudonymized v3; cardinality/churn irreducible | pseudonyms (partial) |
| **Ciphertext size** | ❌ | ✅ v2 padding (§4.1) | size padding |
| **Commit/welcome timing** | 🟡 partial (hides the IP behind the timing) | 🟡 v3+ batching, latency-taxed (§4.2) | mixnet (out of scope) |
| **LiveKit signalling** (sender in JSON) | ❌ (LiveKit terminates the media/data session; relay carries opaque bytes to it) | ✅ v2 payload minimization (§5) | signalling minimization |
| **Read progress** (watermarks by user) | ❌ | 🟡 pseudonymized with v3 membership; existence irreducible (§2.3) | pseudonyms (partial) |
| **Content** (messages / files / voice) | ➖ already E2EE (MLS / convergent AEAD / FrameCryptor) | ➖ already E2EE | already solved |
| **Global passive adversary** (both ends) | ❌ (Tor's known limit, `relay-overlay-design.md:87-96`) | ❌ | nothing short of a mixnet |

> **Neither layer substitutes for the other.** Relay-without-sealed-sender: the operator can't see your IP but sees your entire `user_id`-keyed graph. Sealed-sender-without-relay: the operator can't tie an envelope to a sender in its data model, but sees the source IP that delivered it and can often re-link (`relay-overlay-design.md:82-85`). **Meaningful metadata privacy against the operator needs both.** This spec is the application half; the relay doc is the network half. Ship them independently, market them together, overclaim neither.

---

## 7. Threat Model — Before / After, and What Is Irreducible

Three adversaries, per the relay doc's framing (`relay-overlay-design.md:126-169`), evaluated for the **application-metadata** axis this spec governs.

### 7.1 Honest-but-curious operator (Turso / DS / LiveKit)

Runs the infra, logs faithfully, reads what it has.

| What it learns | Today | After v1 (sealed row) | After v1.5 (anon membership) | After v2 (signalling+size) | After v3 (pseudonyms) |
|---|---|---|---|---|---|
| Sender of a stored message | ✅ (`sender_id` column) | ❌ at rest; ✅ live via DS header | ❌ | ❌ | ❌ |
| Message size | ✅ | ✅ | ✅ | ❌ (bucketed) | ❌ |
| Sender in LiveKit JSON | ✅ | ✅ | ✅ | ❌ (stripped) | ❌ |
| Social graph by global `user_id` | ✅ | ✅ | ✅ | ✅ | ❌ (per-conv pseudonyms) |
| Conversation existence + cardinality | ✅ | ✅ | ✅ | ✅ | ✅ **irreducible** |
| Read progress (watermarks) | ✅ | ✅ | ✅ | ✅ | 🟡 pseudonymized |
| Content | ❌ | ❌ | ❌ | ❌ | ❌ |

### 7.2 Compelled operator (subpoena / lawful order / breach with logs)

The operator is forced to hand over what it *stored*, or an attacker dumps the DB.

- **Today:** a `message_envelope` dump reveals **sender-per-message** and the full membership graph — a devastating retrospective artifact ("show us everyone Alice messaged, when, and how long each message was").
- **After v1:** the stored envelope no longer carries the sender. A cold dump of `message_envelope` yields only `(conversation_id, sentinel, sent_at, size)`. **This is the biggest single win of the whole spec against the compelled/breach adversary** — the persistent, retrospectively-subpoenable sender artifact simply stops existing. (Live *interception* under compulsion still catches the DS auth header until v1.5.)
- **After v3:** the membership dump is per-conversation-pseudonymized — the graph can't be reassembled into "Alice's entire social world" from stored rows.
- **Irreducible:** the operator can always be compelled to *log future traffic*; no at-rest change defends against prospective live interception except moving identity out of the live request (v1.5) and IP out of the connection (relay). Even then, conversation existence/cardinality remain.

### 7.3 Network observer (ISP / on-path)

- **Application-layer changes do little for this adversary directly** — they see TLS to Turso/LiveKit regardless. This is the **relay overlay's** job (`relay-overlay-design.md:143-147`). Size padding (§4.1) marginally hurts traffic-analysis fingerprinting; that's the only application-layer touch here.

### 7.4 What is FUNDAMENTALLY unavoidable

State it plainly, because overclaiming here is the failure mode:

1. **Conversation existence and cardinality.** A store-and-forward server holding envelopes for offline members must know conversations exist and roughly how many recipients each has. Only a mixnet/PIR design erases this — a different product.
2. **The routing set at *some* granularity.** The server must map envelopes to recipient handles. Pseudonyms hide *identity*; they cannot hide *that a routing set exists*.
3. **Read progress existence.** GC needs to know when everyone's caught up (`messages.rs:74-114`). Watermarks can be pseudonymized, not eliminated.
4. **Coarse timing.** Even batched, the server learns activity happened within a window. Full timing obfuscation needs cover traffic (mixnet-grade, rejected §4.3).
5. **The device directory.** Cross-signing (`docs/security-whitepaper.md:161-182`) needs `user_device` keyed by `user_id`; this is a residual linkable point pseudonymization can't remove without breaking rogue-device defense.

**We do not claim anonymity. We claim: after the full roadmap, the server (and a breach/subpoena of it) cannot learn *who sent which message* or *link a user's identity across conversations* — but it still knows conversations exist, how big they are, and roughly when they're active.** That is a true, strong, bounded claim.

---

## 8. Performance & UX Cost Summary

| Technique | Crypto cost | Bandwidth | Latency / round-trips | UX change |
|---|---|---|---|---|
| v1 sealed row + credential attribution | none new (reuse `process_message` credential) | −(smaller sentinel) | none | **none** |
| v1.5 anonymous membership proof | 1 credential presentation / send (sub-ms), per-epoch issuance | tiny | 1 periodic off-path issuance | none |
| v2 ciphertext size padding | none | +~100s of bytes avg | none | none |
| v2 LiveKit signalling minimization | none | −(smaller payload) | none (client re-derives on ingest it already does) | none |
| v3 pseudonymous membership | 1 HKDF per member/conv | tiny | none steady-state (re-key on membership change) | none |
| v3+ commit-timing batching | none | none | **+bounded delay** (correctness-sensitive) | none if bounded well |
| (rejected) cover traffic / PIR / mixnet | heavy | heavy | seconds+ | degraded |

**Why UX stays identical across v1–v3:** every technique is a **server-side or protocol-framing change**. The user still types, sends, and reads exactly as today. Attribution, size-stripping, and pseudonym derivation all happen inside the client using MLS state the client already holds — the user cannot perceive them, and there is no new setting, prompt, or step. This is the zero-user-burden guarantee, and it is *only* achievable because Pollis put E2EE in the client with authenticated sender credentials — the client never needed the server to tell it who sent a message.

---

## 9. Additive-Migration Constraint (CLAUDE.md)

Every schema touch obeys the additive/backward-compatible rule (CLAUDE.md: old and new app versions hit prod for days/weeks after a release):

- **v1:** `NNNNNN_message_envelope_sealed_sender.sql` (next free migration number at implementation time, §2.1) — `ADD COLUMN sealed INTEGER NOT NULL DEFAULT 0`. Safe (nullable-with-default add). `sender_id` stays `NOT NULL`; new senders write a non-identifying sentinel into it. **Two-release dance:** release N ships credential-based attribution (readers stop trusting `sender_id`); release N+1 turns on `sealed=1` sending. No `DROP COLUMN sender_id` — ever, unless a much later multi-release retirement is explicitly undertaken.
- **v2:** no schema change (padding is inside the ciphertext; signalling minimization is a JSON payload change). Gate padding on a framing-version byte inside the plaintext so mixed-version members interoperate.
- **v3:** membership pseudonyms need an additive `member_handle` column alongside the existing `user_id` (dual-write), with the same teach-readers-first dance before any reliance on the pseudonym for routing. `user_id` membership rows are retired only in a far-future multi-release step, if ever.
- **DS side:** `apply_send_message` (`pollis-delivery/src/messages.rs:250-277`) must tolerate a sealed body (sentinel `sender_id`, `sealed=1`) — additive parsing, default `sealed=0` for old clients.

---

## 10. In-Box Testability (the flows harness)

The integration harness (`src-tauri/tests/flows.rs`, `tests/flows/harness.rs`) drives the real command implementations through the real dispatch path against a process-local Turso, and gives tests **direct read access to the shared remote DB** via `world().await.remote` (`harness.rs:2176`, `2334`). That is exactly the seam needed to assert the server-visible envelope no longer carries the sender while delivery still works. Concretely, add to `tests/flows/messages.rs`:

- **`sealed_sender_hides_sender_but_delivers`:** Alice and Bob in a DM/channel. Alice sends. Then:
  1. Query the shared remote directly: `SELECT sender_id, sealed FROM message_envelope WHERE conversation_id = ?` — **assert `sealed = 1` and `sender_id` is the sentinel, NOT Alice's `user_id`.** (This is the core metadata-minimization assertion.)
  2. Bob ingests (`get_dm_messages` / `get_channel_messages`) — **assert the message is delivered, decrypts, and Bob's local `message.sender_id` correctly attributes it to Alice** (proving credential-based attribution works without the envelope column).
- **`sealed_sender_non_member_rejected`:** a non-member's sealed send is refused by the DS membership gate — proving abuse control survives sealing.
- **`padding_hides_size`:** two messages of very different plaintext length produce **equal-bucket ciphertext lengths** in `message_envelope` (assert the sizes collapse to the same bucket).
- **`signalling_carries_no_sender`:** assert the `new_message` wake-up payload contains no `sender_id`. One wrinkle: the real payload builder (`publish_new_message_to_room`, `publish.rs:142-148`) lives in the media-gated `livekit` module — under `--no-default-features`, `commands/mod.rs:14-18` swaps in `livekit_stub.rs`, so the real payload construction does not compile headless. Extract the payload construction into an always-compiled pure function (mirroring the `livekit_jwt` split, `commands/mod.rs:23` — pure code pulled out of the gated module precisely so it compiles on every target) and unit-test *that*, so the headless harness asserts on the real payload bytes (the harness's `security.rs` module is the natural home). If extraction is rejected at implementation time, the fallback is running this one test in the media-ON CI gate.

These are the acceptance tests: **the server-visible envelope no longer carries the real sender id, and delivery + attribution still work end-to-end** — encoding the invariant per CLAUDE.md's "ship a test that tries to create the invalid state and proves it can't" doctrine (here: "server learns the sender" is the invalid state).

---

## 11. Recommendation + GitHub-Issue-Ready Summary

**Build it — sealed sender first, phased, additive, invisible to users, and honest about the irreducible routing floor.**

- **v1 (do first — highest value/cost ratio): Sealed sender, at-rest scope.** Additive `message_envelope` migration + credential-based attribution on ingest (stop trusting the plaintext `sender_id` column; read the MLS-authenticated credential `try_mls_decrypt` already computes). Two-release dance. Delivers: a `message_envelope` dump / subpoena / breach no longer reveals sender-per-message. Scope it honestly as **breach/subpoena/at-rest** defense — the DS auth header still reveals the sender live until v1.5.
- **v1.5 (close the live axis): Anonymous membership proof** for the DS write path (BBS+/KVAC anonymous credential, or blind-signed per-conversation membership token) so the send request proves "a member of C" without an `X-Pollis-User` header. Doubles as rate-limited-anonymous-credential abuse control.
- **v2 (cheap, invisible, do alongside v1): LiveKit signalling minimization + ciphertext size padding.** Strip `sender_id`/`sender_username` from `new_message` and id-lists from `membership_changed`/`RosterChanged` broadcasts; bucket text-ciphertext sizes. Both zero-latency, zero-UX, no schema change.
- **v3 (demand-gated): Pseudonymous per-conversation membership handles.** Unlink a user's graph across conversations while preserving routing. Bounded: cardinality/churn/existence remain irreducible.
- **v3+ / rejected: commit-timing batching** (latency-taxed, correctness-sensitive — build only on real demand); **cover traffic / PIR / mixnet** (out of scope — a different product).

**Framing guardrails (non-negotiable):** market this as *"the server can't tell who sent which message, and can't link you across conversations"* — never as *"anonymous"* or *"the server can't see who talks to whom"* (it always knows conversations exist and how big they are, §7.4). Pair it with the relay overlay for IP; state plainly that neither substitutes for the other (§6).

---

### GitHub issue summary

**Title:** Metadata minimization: sealed sender + application-layer metadata reduction (phased)

**Problem.** Turso stores `message_envelope.sender_id` in plaintext on every send (`000000_baseline.sql:92`, `messages/send.rs:143-151`) and the full `user_id`-keyed social graph (`group_member`/`dm_channel_member`), and LiveKit forwards signalling JSON carrying `sender_id` in cleartext (`livekit/publish.rs:142-148`). Yet MLS already authenticates the true sender inside every ciphertext via the `{user_id}:{device_id}` credential (`docs/security-whitepaper.md:57-59`), which the ingest path currently ignores in favor of the server-writable plaintext column (`ingest.rs:378,404-407`). The server-visible sender is therefore **cryptographically redundant** and a pure metadata leak (breach/subpoena artifact). The relay overlay (`docs/relay-overlay-design.md`) hides IP but explicitly not this. Routing (conversation existence + membership) is irreducible; sender identity, cleartext signalling, and ciphertext size are not.

**Phased milestones.**
1. **v1 — Sealed sender (at-rest):** additive `sealed` column; ingest attributes senders from the MLS credential (`try_mls_decrypt`), not the envelope column; new senders write a non-identifying sentinel. Two-release additive dance (CLAUDE.md). Scope: breach/subpoena/at-rest.
2. **v1.5 — Anonymous membership proof:** DS write path proves conversation membership without `X-Pollis-User` (BBS+/KVAC or blind-signed token); doubles as rate-limited abuse control. Closes the live-request sender axis.
3. **v2 — Signalling + size:** strip sender/id fields from LiveKit `new_message`/`membership_changed`/`RosterChanged` payloads; bucket-pad text ciphertext. Zero UX, no schema change.
4. **v3 (demand-gated) — Pseudonymous per-conversation membership handles:** unlink the social graph across conversations while preserving routing.
5. **Rejected / documented ceiling:** commit-timing batching (latency-taxed), cover traffic / PIR / mixnet (different product). Conversation existence, cardinality, and read-progress existence are fundamentally irreducible.

**Acceptance.**
- Flows-harness test (`tests/flows/messages.rs`) asserts, via direct remote read (`world().await.remote`), that after a send `message_envelope.sender_id` is the sentinel (NOT the real sender) with `sealed=1`, **and** the recipient still ingests, decrypts, and correctly attributes the message to the real sender via the MLS credential.
- Non-member sealed send rejected by the DS membership gate (abuse control survives sealing).
- Size-padding test: two very different plaintext lengths collapse to the same ciphertext bucket in `message_envelope`.
- Signalling test: `new_message` LiveKit payload contains no `sender_id`/`sender_username`.
- All migrations additive and backward-compatible with the currently-shipped app (CLAUDE.md); no `DROP COLUMN`/nullability tightening.
- No user-visible change: no new setting, prompt, or step; attribution/padding/pseudonym derivation happen client-side from MLS state the client already holds.
