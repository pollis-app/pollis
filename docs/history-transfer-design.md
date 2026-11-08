# History transfer between a user's own devices (#1735)

Status: implemented (#1735). v1 below, then the v2 amendments (which override v1), then the implementation notes (which record where the code deliberately differs, and why). Owner decisions (2026-11-07) are binding:
1. Opt-in, easy. Right after a device is linked, the NEW device offers "Copy your message history from <old device>?". Later, any two enrolled devices of the account can do it: the requesting device picks one of its sibling devices (online ones shown first) and asks; **both devices must confirm**. A device must never be able to pull history from a sibling without the sibling's explicit approval.
2. Scope: all message history, media/files as references only (the `_att` JSON inside `content`; the receiver re-fetches from R2 with its own file-key ring). Vault is out (already syncs).
3. Deletion/retention follow the data: rows arrive in their current state (edited text, tombstones), never resurrect, and obey the receiver's retention window. Conversations the user left must not come across.
4. Chunked, resumable; a kill on either side at any point leaves both consistent and resumable; never a half-applied chunk.
5. Only decrypted history rows. No MLS state, no key caches, no outbox, no ingest bookkeeping.
6. Both at link time and later ("backfill": a device used for weeks pulls older messages from a sibling that has more).

**Ship bar: if adversarial testing shows instability (duplicates, resurrection, broken ingest after import, partial chunks, flaky resume), it does not merge.**

Terminology: **R** = receiver/requester (wants history). **S** = source (has history). Same account, both enrolled, both non-revoked.

## 1. Why this is acceptable under the product rules
Loss (2) "a new device starts empty (no key backup)" stays true by default. This is not a key backup: no keys move, the server never holds a decryptable copy, and it needs a second enrolled device the user still holds plus an explicit approval (with PIN, except the link-time shortcut in §4.2) on that device. Update root CLAUDE.md loss (2), `docs/backend-core-invariants.md`, `local_schema.sql` bookmark comment to say: "...starts empty unless the user copies history from another of their enrolled devices (#1735)".

## 2. Threat model
Untrusted: network, DS, Turso, R2, operators. They must not be able to: read history; substitute either device's key; replay/reorder/truncate/splice chunks into an accepted transfer; redirect a transfer to another device or account; cause rows to be attributed to a different sender; make the receiver import rows for conversations it isn't in; resurrect deleted messages.
A sibling device R of the same account is trusted only as much as "an enrolled device of mine": it can request, but cannot obtain anything without S's human approval. A revoked device can do nothing.
S is trusted as the user's own device: its rows are believed (sender_id, content). It is still parsed defensively (bounded, schema-checked) so a bug or corrupt DB on S cannot corrupt R.

## 3. Cryptography
- **Device authentication:** each side signs its handshake message with its ML-DSA-44 device key (`load_device_pq_signing_key`). The peer fetches the sibling's `DeviceRow` (`ds_reads::devices`), verifies the device cert with `verify_device_cert` against the **locally pinned** `account_id_pub` (never the DS's copy), rejects revoked rows, and checks the signature against the certified `mls_signature_pub_pq`. Mirror `IdentityDirectory::leaf_verdict` (mls/device.rs:522-616).
- **Key agreement:** X-Wing (X25519 + ML-KEM-768, crate `x-wing` already in Cargo.lock; if unusable from pollis-core, use `ml-kem` + `x25519-dalek` combined via HKDF over both shared secrets and both ciphertexts/pubs). R generates the KEM keypair; S encapsulates.
- **Transcript:** `req` = canonical bytes of `{v:1, transfer_id, user_id, dst_device_id(R), src_device_id(S), kem_pub, retention_days(R), via: "link"|"sibling", created_at}`; `sig_R = Sign_R("pollis-history-transfer-req-v1" ‖ req)`. `acc` = `{v:1, transfer_id, H(req), kem_ct}`; `sig_S = Sign_S("pollis-history-transfer-acc-v1" ‖ acc)`. Each side verifies the other's signature AND that the device ids in the signed bytes match the peer it verified and itself (no reflection).
- **Transfer key:** `K = HKDF-SHA256(ikm = ss, salt = transfer_id, info = "pollis-history-transfer-key-v1" ‖ H(req) ‖ H(acc))`.
- **Chunks:** ChaCha20-Poly1305 (or AES-256-GCM — match what core already uses), key K, nonce = 96-bit big-endian chunk index (unique per key; K is per transfer), AAD = `"pollis-history-transfer-chunk-v1" ‖ transfer_id ‖ u32 index ‖ u8 is_final`. Plaintext carries `prev_hash = H(previous chunk plaintext)` (zero for index 0). Final chunk carries `total_chunks` and `total_rows`. R accepts only index == next expected, checks prev_hash, and completes only after a valid final chunk whose totals match what it imported+skipped. → reorder, replay, splice, truncation, and cross-transfer chunks are all rejected.
- Key material (KEM secret on R, K on both) lives only inside the encrypted local DB (`history_transfer_local`), zeroized on completion/cancel/expiry. Never logged.

## 4. Protocol
### 4.1 Request (R)
`request_history_transfer(source_device_id, via)`:
- Refuse if R already has an active transfer as receiver, if source == self, or source is not a live sibling with a valid cert.
- Persist local row (role dst, state requested, kem_sk, req bytes) **before** posting.
- `POST /v1/history-transfer/request {transfer_id, src_device_id, req_b64, sig_b64}`.
- DS: signer device == req.dst; src belongs to the same account, non-revoked, != dst; at most 1 open transfer per dst and per (src,dst); request rate-limited per account; `accept_by = now + 15 min`. Sends a content-free nudge `{type:"history_transfer", transfer_id, event:"requested"}` to `inbox-<user>`.

### 4.2 Approval (S)
- S learns of pending requests from the nudge AND from `POST /v1/history-transfer/pending` on landing/unlock/foreground (mobile pattern of `useEnrollmentApprovals`; realtime is foreground-only on mobile).
- S verifies sig_R + R's cert (§3) before showing anything. A request failing verification is never shown (logged, rejected).
- Prompt (a page, never a modal on desktop): "**<R device name>** (added <relative date>) wants to copy your message history. It will be able to read all your past messages." → Approve / Decline. Approve requires the **PIN** (`pin::verify_pin`, same counter).
- **Link-time shortcut:** if S itself approved R's enrollment (`approve_device_link`/`wrap_and_approve`) in this app session less than 10 minutes ago (in-memory record keyed by R's device_id, cleared on lock), the approval is a single tap without the PIN, because the PIN was just entered for the link. Still an explicit tap.
- On Approve: encapsulate, derive K, persist local row (role src, state accepted, K, cursor = start) **before** `POST /v1/history-transfer/accept {transfer_id, acc_b64, sig_b64}`. DS: signer == req.src, state requested, unexpired. Nudge `accepted`. Record security event `history_transfer_approved` (device_id = R, metadata `via=link|sibling,to=<R>`).
- Decline → `POST /reject`; R shows "Declined on <S>".

### 4.3 Data (S → R)
- Before snapshotting, S runs a normal catch-up ingest of its conversations so its rows reflect the latest edits/deletes it can see.
- Selection on S: rows in `message` where `undecryptable IS NULL AND send_state IS NULL`, conversation in S's **current** DS membership list (`ds_reads::conversations`), and within R's retention window (`received_at >= now - R.retention_days` when R.retention_days > 0). Order key `(conversation_id, sent_at, id)`; the persisted cursor is the last key sent.
- Per-row fields: `id, conversation_id, sender_id, content (raw string, exactly as stored; NULL only for tombstones), reply_to_id, thread_id, sent_at (byte-identical), received_at, edited_at, deleted_at`. Never: ciphertext, delivered, send_state, undecryptable, outbox_*, known_envelope, any key cache, MLS tables.
- Chunk 0 additionally carries the read cursors for selected conversations. Each chunk carries `message_receipt` rows for its messages. Bookmarks are NOT transferred (device-local by design).
- Chunk plaintext target ≤ 512 KiB, sealed ≤ 1 MiB hard cap (DS enforces). Encoding: a versioned, length-bounded format (serde_json or CBOR with explicit caps on counts and string lengths); unknown version → abort.
- **Crash safety on S:** in one local transaction S writes `(transfer_id, index, sealed_bytes, cursor_after)` to `history_transfer_outgoing` and advances `next_index`; only then PUTs. On restart S re-PUTs stored-but-unacked chunks byte-for-byte. The DS accepts a re-PUT of identical bytes as a no-op and refuses different bytes for an existing index (409 → S aborts). S deletes its stored chunk once the DS reports it acked.
- Flow control: the DS holds at most 8 un-acked chunks per transfer; S waits (event-driven: nudge on ack + bounded backoff inside one awaited Rust call, the `await_device_link` style) when the window is full.

### 4.4 Import (R)
- R loads its current membership set from the DS at start and refreshes it on a miss before skipping.
- For chunk i: open (fail → abort transfer, nothing written), validate everything (index, prev_hash, version, bounds, row invariants: id is a ULID, sent_at/received_at parse, tombstone ⇔ content NULL ⇔ deleted_at set, conversation in membership set else skip+count, row size limits). Any invariant violation → abort the transfer (S is buggy or corrupt), nothing from this chunk written.
- In ONE local transaction: insert rows, apply pending changes (§5), merge read cursors (forward-only max, `merge_read_cursors`), insert receipts `INSERT OR IGNORE`, set `next_index = i+1`, update counts. Then `persisted::note(conversation_ids)`. Then ack to the DS (an un-acked imported chunk is acked without re-import on resume because `next_index` > i; re-import would be a no-op anyway).
- Row insert: `INSERT INTO message (..., ciphertext = X'', delivered = 1, origin = 'transfer', received_at = min(carried, now)) ON CONFLICT(id) DO UPDATE SET <all content columns>, origin='transfer' WHERE message.undecryptable IS NOT NULL AND message.conversation_id = excluded.conversation_id` — i.e. never overwrite a real row (R's own row is authoritative: R received every edit/delete since it joined), only heal R's placeholders.
- FTS is maintained by the existing triggers. No notifications, no news, no delivery receipts. `mark_messages_read` must NOT send read receipts for `origin='transfer'` rows.
- Final chunk: verify totals, mark complete, zeroize K, `POST /complete` (DS deletes transfer + chunks), security event `history_transfer_completed` from R.

## 5. Edits/deletes that reach R before the row they target (critical)
Today ingest drops an edit / self-redaction / admin tombstone whose target row is absent and records the envelope as handled (ingest.rs ~1029-1046, 1114-1126, resolve_delete 132-156/671-725). With transfer, the target row can arrive later from S and would carry stale content → a deleted message would come back. Fix at the lowest layer:
- New local table `pending_message_change(envelope_id PK, target_id, kind CHECK IN ('edit','redact','tombstone'), actor_id, content, sent_at, recorded_at)` + index on target_id. Ingest, at exactly the places that currently drop for "no target row", records the change instead (actor from the MLS credential, as ingest already computes; tombstones as ingest already trusts them).
- On import of a newly inserted (or healed) row: if any `redact` with actor == row.sender_id or any `tombstone` exists for it → soft-delete (`SOFT_DELETE_SQL` semantics). Else the latest `edit` (by sent_at, envelope_id) with actor == row.sender_id → set content/edited_at. Delete applied changes. Changes whose actor doesn't match are discarded (same rule ingest applies).
- Correctness argument: every edit/delete posted after R joined a conversation reaches R (as an orphan change or applied to a row R holds); every edit/delete before R joined is reflected in S's row. So "S's row, then R's post-join changes" is the current state.
- Prune `pending_message_change` older than 400 days (same horizon as known_envelope) and on retention eviction.
- This also improves today's behaviour without transfer, and must not change ingest outcomes when the target exists. Tests must prove ingest is unchanged for existing targets.

## 6. Storage
### Remote (migration `000045_history_transfer.sql`, additive)
- `history_transfer(transfer_id TEXT PK, user_id, src_device_id, dst_device_id, state CHECK IN ('requested','accepted'), req BLOB, req_sig BLOB, acc BLOB NULL, acc_sig BLOB NULL, acked_through INTEGER NOT NULL DEFAULT -1, created_at, updated_at, expires_at)`; CHECK src != dst; unique partial index: one open transfer per dst_device_id.
- `history_transfer_chunk(transfer_id, idx, sealed BLOB, created_at, PRIMARY KEY(transfer_id, idx))`.
- Expiry: requested → 15 min; accepted → 24 h since last activity (so a phone backgrounded overnight can resume). Terminal states delete the rows (complete, cancel, reject, expiry). Sweep in the existing GC tick in `pollis-delivery/src/main.rs`. Purged on account teardown (`tests/teardown.rs` coverage) and on revocation of either device.
- Endpoints (all device-signed `ds_post*`, `pollis_api::ENDPOINTS` rows, `Client`): `request`, `pending` (as src), `status` (either party), `accept`, `reject`, `chunk/put` (src), `chunk/get` (dst), `chunk/ack` (dst), `complete` (dst), `cancel` (either). Every handler binds the actor from the signature and checks it is the right party of that transfer; cross-account and wrong-party access returns the same 404. Rate limits via `ratelimit.rs::classify` (request is a low-cap tier; chunk endpoints bounded by window).
- Content-free inbox nudges on requested/accepted/chunk-available/acked/done/cancelled via `broker::room_send_data`. Old clients drop unknown types.
- Body size: confirm axum's default body limit vs base64 1 MiB chunks; set an explicit per-route limit.

### Local (additive; never bump LOCAL_SCHEMA_VERSION)
- `message.origin TEXT CHECK (origin IS NULL OR origin = 'transfer')` via `ADDITIVE_COLUMNS` + local_schema.sql.
- `history_transfer_local(transfer_id PK, role CHECK IN ('src','dst'), peer_device_id, via, state, kem_sk BLOB NULL, key BLOB NULL, req BLOB, acc BLOB NULL, cursor TEXT NULL, next_index INTEGER, prev_hash BLOB, rows_imported, rows_skipped, created_at, updated_at)`.
- `history_transfer_outgoing(transfer_id, idx, sealed BLOB, cursor_after TEXT, PRIMARY KEY(transfer_id, idx))`.
- `pending_message_change` (§5).
- All cleared on logout(delete_data)/wipe like other local tables (whole DB).

## 7. Orchestration and UX
- Rust: `run_history_transfer(transfer_id)` — one awaited command that drives either role to completion/pause with progress events (EventSink / Tauri Channel; `{transfer_id, role, state, chunks_done, rows_done, estimated_total?}`), resumes from local state, and returns `Paused` after a bounded idle (no indefinite polling). Auto-resume active transfers after unlock (desktop) and on unlock/foreground (mobile). `list_history_transfers`, `cancel_history_transfer`, `list_pending_history_transfer_requests`, `approve_history_transfer(transfer_id, pin?)`, `decline_history_transfer`, `request_history_transfer`, `list_transfer_source_candidates` (own non-revoked devices except self, with name, platform, last active, online-now if cheaply derivable from own-inbox LiveKit participants, else "active recently" from last_seen).
- **Link time (new device):** after `finalize_device_enrollment`, before entering the app: page "Copy your message history from **<approver device>**?" (approver from `approved_by_device_id` in the enrollment status) — [Copy history] [Not now]. Copy → request with via=link → page shows "Approve on <device>", then progress. The user can continue into the app; progress continues in Settings → Devices (or a slim banner).
- **Link time (old device):** its "Linked" page (desktop `LinkDevicePage`, mobile `self/link-device.tsx`) waits briefly for the request and shows the approval inline (single tap, §4.2 shortcut).
- **Later:** Settings → Security/Devices → "Copy message history from another device" → pick device → request → S shows approval page (PIN) → both show progress, with Cancel. Desktop approval = routed page (no modal); mobile = full-screen page like `approve-device.tsx`.
- Copy: never call the local DB "plaintext"; say "message history". Don't mention E2EE in mobile copy (per mobile redesign rules).

## 8. Backward compatibility / rollout
- DS + migration ship first (additive). The feature needs both devices on the new client; an old S never sees the request (drops unknown nudge, no pending check) → R's request expires; R's UI says "Open Pollis on <device> — it may need an update."
- pollis-core change → new mobile store build + desktop release. Feature flag not required; the UI only offers it on new builds.

## 9. Test plan (must all pass; failure = don't ship)
Flows (`src-tauri/tests/flows/history_transfer.rs`), DS tests, core unit tests, fuzz target, Playwright, Maestro — see the risk matrix in research-testing.md. Must include at least:
1. Link-time transfer (v1 and v2 link) end to end: R ends with exactly S's rows for current conversations (ids, content, sent_at, edited_at, deleted_at), no duplicates, then live messaging both ways works, Drop's zero-loss check passes.
2. Backfill after weeks: R has its own post-join rows (some placeholders); transfer heals placeholders, never overwrites R's real rows; no duplicates; counts right.
3. Edit/delete races: edits/deletes/admin tombstones of pre-join messages arriving at R before, during, and after the transfer — final state equals S's final state; never resurrected. Edit by a different actor ignored.
4. Retention: R window 30 days, rows older not sent / evicted; received_at carried; no fresh clock.
5. Left/kicked conversations never cross; mid-transfer leave on R → rows for it skipped.
6. Kill points (Crash hooks) on S: after local chunk store before PUT, after PUT before mark; on R: after decrypt before commit, after commit before ack. Restart (lock/unlock) → resume → exactly-once, identical final state.
7. Malicious DS (DsFault variants + writable_remote tampering): drop, replay, reorder, swap index, bit-flip, truncate (withhold final), splice a chunk from another transfer, substitute kem_pub / kem_ct / signatures, redirect to another device, cross-account → transfer aborts with nothing written from the bad chunk; never completes silently short.
8. Authorization: a sibling cannot fetch chunks/status of a transfer it isn't party to; R cannot approve its own request; revoked devices refused; request without approval yields nothing; old pending request expires.
9. Duplicate requests, transfer twice (second is a no-op), concurrent transfers refused.
10. Read cursors: imported history doesn't inflate unread; no read receipts sent for transferred rows.
11. Soak: extend the seeded zero-loss multi-device soak with transfer + backfill + kills.
12. Fuzz target for chunk parsing/validation (+ mutant).
13. DS: teardown purges tables; revocation purges; expiry sweep; window cap; 409 on different-bytes re-PUT; size caps; rate limits.
UI: Playwright mocked states (offer, waiting, approve w/ PIN, progress, declined, paused, done); Maestro two-device flow on iOS sim + Android emulator (or one device + scripted peer extending `pollis-core/examples/enroll_peer.rs`).

## 10. Docs to update in the PR
New `docs/history-transfer-design.md` (this, finalized), `.codesight/wiki/` (database.md local + remote tables, mls.md multi-device, commands.md, testing.md incl. the stale shared-DB note), `docs/security-whitepaper.md` (new section), root CLAUDE.md loss (2), `docs/backend-core-invariants.md`, `docs/qr-device-link-design.md` (link-time follow-on).

---
# v2 AMENDMENTS (after adversarial design review, 2026-11-08). These OVERRIDE v1 where they conflict.

**A1 (blocker B1: placeholders).** A placeholder row's sender_id is the sealed sentinel "sealed" (loss.rs:174-188), so ingest's author checks (ingest.rs:1030-1045 redaction, 1115-1126 edit) silently drop changes targeting a placeholder, and admin tombstones soft-delete the placeholder (deleted_at set, undecryptable still set).
- Ingest records a `pending_message_change` when the target is ABSENT **or a placeholder** (`undecryptable IS NOT NULL`). Never for an author mismatch on a real row (keep today's drop).
- A single `apply_pending_changes(conn, message_id)` is called from EVERY path that creates or heals a real row: transfer import AND ingest's own placeholder heal (ingest.rs:1052-1061, same latent bug today).
- Heal upsert preserves R's tombstone: `deleted_at = COALESCE(message.deleted_at, excluded.deleted_at)`, content NULL if either side deleted, and always `undecryptable = NULL` (CHECK at local_schema.sql:42-45).
- Tests for each change kind (edit / self-redaction / admin tombstone) against a placeholder target, both via transfer heal and via ingest heal.

**A2 (blocker B2: "joined" = MLS tree join, not DS watermark).** R can be on DS fan-out before it is in a group's tree; changes sealed at epochs before R's join are undecryptable to R and only S can reflect them.
- The signed `req` carries, per conversation R currently holds an MLS group for: `(conversation_id, mls_group_id, R's join epoch)`.
- S includes a conversation ONLY IF its own local group epoch >= R's join epoch for it AND a catch-up of that conversation completed successfully (no error, no DeletePending holding the cursor) during this transfer's snapshot phase, after reaching that epoch. Otherwise the conversation is reported in the final chunk as `deferred` (not an error).
- R rejects (skips+counts) rows for conversations not in its signed list.
- At link time, R requests only after `finalize_device_enrollment` has completed its external joins; conversations R joins later (or that S deferred) are offered for a follow-up backfill (UI: "N conversations weren't ready; copy again later" + automatic offer next time both are online — at minimum a manual retry). Re-running a transfer is always safe (idempotent).
- **Deletes merge one way:** if S's row is a tombstone and R holds a real live row with the same id+conversation, R applies the soft-delete (monotone, safe, repairs deletes R couldn't decrypt). Edits do NOT merge onto R's real rows.
- Document: undecryptable post-join changes remain a loss, exactly as for native rows.

**A3 (nonce reuse).** Use XChaCha20-Poly1305 (chacha20poly1305 0.10 already a dep) with a random 24-byte nonce per seal; AAD = domain ‖ transfer_id ‖ u32 index ‖ u8 is_final. `next_index` only moves forward; an index below next_index with no stored bytes ⇒ abort, never re-seal.

**A4 (notifications).** notify/fetch.rs uses rowid floors and a received_at fallback (fetch.rs:31 LIVE_INBOUND, 136-139, 284-287, 620-628, 648-656). Add `AND origin IS NULL` to LIVE_INBOUND/fallback. Each import transaction holds the cross-process StateLock exactly as ingest does. Test: a push wake concurrent with an import previews no transferred row.

**A5 (pending table bounds).** Record a pending change only when the target ULID's timestamp is before R's join time for that conversation (only those can arrive by transfer) OR the target is a placeholder; store conversation_id and apply only on match; hard cap per conversation (e.g. 5000, drop oldest); prune at 400 days and on retention eviction.

**A6 (bad rows).** Skip-and-count rows that violate row invariants (non-ULID id, content+deleted_at both set, sender "sealed", unparsable dates, oversize). Abort only on: AEAD failure, framing/version/bounds error, broken prev_hash chain, index mismatch. Never permanently stuck on one weird legacy row.

**A7 (minors, all required).**
- PIN-less link-time shortcut only recorded by the PIN-verified link flows (`create_device_link` device_link.rs:198 path → approve_device_link, and `accept_device_offer` :498 → finish_device_offer), NOT by typed-code `approve_device_enrollment`.
- S enforces `req.created_at` freshness (±10 min skew) and remembers handled transfer_ids locally (refuse a replayed req).
- "Added <date>" in the prompt comes from the verified cert's `issued_at`. Doc notes that revocation status is DS-reported; the PIN is the real gate.
- Purge transfers on account teardown, device revocation AND the reset-identity path (account.rs:648-683). Either side treats DS 404 / terminal status as terminal: zeroize + delete local state. A new request from the same dst replaces that dst's own orphaned open transfer.
- Read cursors are NOT carried in chunks; after import call the existing `converge_read_cursors` (read_state.rs:380-476). Test: R's unread counts equal S's for transferred conversations after convergence.
- Imported receipts only if `receipts_permitted(is_dm, receipts_enabled)` on R (receipts.rs:52-76). Do NOT suppress read receipts for transferred rows (remove that from v1).
- `received_at` and all dates normalized on R: parse, then write `YYYY-MM-DD HH:MM:SS` UTC (eviction compares text, local.rs:621-626); `sent_at` stays byte-identical (ordering key). Document that R may re-acquire rows it had already evicted (source filters by R's window anyway).
- KEM: do not depend directly on draft `x-wing`. Use `ml-kem` (0.3.2 in Cargo.lock) + `x25519-dalek`, combining both shared secrets, both pubs and both ciphertexts through HKDF (X-Wing-style combiner). Respect supply-chain/ (cargo vet) and deny.toml.
- Nudges are honoured only when DS-originated (ignore peer-sent, like read_cursors_changed at livekit/mod.rs:256-261). They are content-free hints only.
- Per-account durable rate/count limits in the DS handler (the limiter is per-IP: ratelimit.rs:253-264).
- Explicit per-route body limit on chunk/put.
- Whitepaper notes the DS learns approximate history size and timing.

---
# Implementation notes (as built)

Code: `pollis-core/src/commands/history_transfer/` (`crypto.rs`, `wire.rs`, `store.rs`, `engine.rs`, `mod.rs`), `pollis-core/src/commands/messages/pending_change.rs`, `pollis-delivery/src/history_transfer.rs`, `pollis-api/src/history_transfer.rs`, migration `pollis-schema/migrations/000045_history_transfer.sql`.

## Where invariants live
| Invariant | Layer | Test that tries to break it |
|---|---|---|
| src ≠ dst; one OPEN transfer per receiving device; acceptance present iff accepted/completed | DB CHECK + partial UNIQUE (`000045`) | `pollis-delivery/tests/history_transfer.rs::the_schema_refuses_invalid_transfers` |
| a chunk only for an accepted transfer, inside the 8-chunk window, contiguous; stored chunks immutable; the ack only moves forward; 1 B..1 MiB | DB triggers + CHECK | `the_schema_refuses_chunks_outside_the_window_or_out_of_order` (raw SQL, handlers bypassed) |
| every call bound to the SIGNING device and its party role; stranger/sibling/wrong party/expired all get the same 404 body | DS handler chokepoint (`load_for`) | `strangers_and_siblings_get_the_same_404_as_an_unknown_id`, `each_party_is_held_to_its_role` |
| one open receiving transfer locally; ended never reopens; ended rows hold no key; `next_index` only forward; key is 32 bytes | local DB CHECK/UNIQUE/triggers (`local_schema.sql`) | `store::tests::the_local_schema_refuses_invalid_transfer_states` |
| a pending change is well-formed (edit ⇔ text, tombstone ⇔ no actor) | local CHECK | `pending_change::tests::the_schema_refuses_malformed_changes` |
| a transferred row never overwrites a real row; heals only a same-conversation placeholder; a tombstone on either side wins | the one upsert (`engine::IMPORT_SQL`) | flows `backfill_heals_placeholders_and_never_overwrites_a_real_row` (fails with the guard removed — mutation-checked) |
| a chunk is applied whole or not at all, with the position that names it | one SQLite transaction (`engine::import_chunk`) under the cross-process state lock | flows `a_kill_at_any_point_resumes_to_exactly_the_same_result` (`TransferReceiverAfterOpen` rolls back) |
| an index is never sealed twice (no nonce reuse even with random nonces; no "different bytes" 409) | store-before-put in one transaction (`seal_next_chunk`), verbatim re-upload | kill points `TransferSourceAfterStore` / `AfterPut`; DS `a_re_put_is_idempotent_and_a_rewrite_is_refused` |
| reorder / replay / splice / truncate / bit-flip / substituted acceptance or request / redirected receiver | AEAD AAD (domain ‖ transfer ‖ index ‖ final), `prev_hash` chain, final totals, ML-DSA transcript signatures checked against certs pinned to this account's own key | `crypto::tests`, `wire::tests`, flows `a_tampering_ds_is_caught_and_nothing_bad_is_written`, `a_replayed_chunk_is_refused`, `a_forged_or_redirected_request_is_never_shown`; fuzz `history_transfer_chunk` |

## Deviations from the design (and why)
1. **Ended transfers are kept an hour on the DS** (`completed` / `declined` / `cancelled`, chunks deleted at once) instead of deleted outright, so the other device shows "Declined on <device>" rather than "expired". The unique index is partial on the open states, so an ended row never blocks a new request; a new request from the same receiver deletes its own older rows (A7).
2. **A5 bound without a wall-clock join time.** The client records the join EPOCH per lineage (`mls::join_branch::first_join`) but no join INSTANT, and backfilling one for every group already held is not possible. Ingest's "target absent" is already its own definition of pre-join (an absent target that is still awaiting ingest is the DeletePending case, which retries anyway), so the table is bounded instead by: a plausible target (a ULID no later than the change + 10 min), 5 000 per conversation (oldest dropped), the 400-day horizon, and the retention window.
3. **Ingest applies pending changes on every fresh insert, not only on a placeholder heal** — an indexed lookup per stored message. Consequence: an edit or delete that arrived before its target (any reason) now converges; outcomes for a present real target are unchanged (a change for a real row is never recorded).
4. **An admin tombstone on a placeholder is kept on the row** (`deleted_at`) rather than recorded; both heals (ingest's and the transfer's) keep it and null the content.
5. **Join position** = `first_join` of the lineage the receiver holds, else its current epoch (a lineage held since before #1312) — later than the true join, so the source is held to a stricter bar.
6. **"Catch-up succeeded, no DeletePending"** is implemented as: `catch_up_mls_group_interleaved` returned `Ok`, then the local group is at or past the receiver's join. A DeletePending tombstone means the source does not yet hold the target row, so it sends nothing for it — nothing stale can cross. It is therefore not tracked separately.
7. **Only `received_at` is normalized** (to `YYYY-MM-DD HH:MM:SS`, never later than now; eviction compares it as text). `sent_at` is byte-identical; `edited_at` / `deleted_at` are validated as parseable and carried verbatim, because ingest writes RFC 3339 there and the renderers read that.
8. **`message.delivered` is not set** — nothing reads it for inbound rows.
9. **Mobile progress.** uniffi has no event channel (mobile's realtime is parsed in JS). The bridge runs `run_history_transfer` with a no-op sink and adds `await_history_transfer(transferId, since)`: returns the transfer as soon as its `updated_at` differs from `since` (event-driven, an in-process `Notify`), or after 25 s. `updated_at` strictly increases on every change.
10. **`paused` is stored** (a run that idles out records it), so the UI shows it after a restart; it is an open state like `transferring`.
11. **The receiver re-checks membership before skipping** (once per chunk with a miss) and also requires that it still holds the conversation's MLS group in the import transaction — that is what drops a conversation left mid-transfer.
12. **Transitivity.** The source sends rows it itself received by transfer (`origin = 'transfer'`); they carry the same guarantees.
13. **Request budget** 12 / hour / account (durable `history_transfer_budget`) and at most 4 open transfers per account; the polls are in the `read` rate tier.
14. **The DS needs a signed device identity** for every transfer endpoint: the no-auth dev mode refuses them (403).
15. **Interface additions:** `TransferSource.approved_this_device` (the enrollment approver, recorded at `finalize_device_enrollment` from the enrollment read's new optional `approved_by_device_id`), `HistoryTransfer.estimated_total` (the source counts selected rows when it plans; the receiver learns it from chunk 0), `await_history_transfer`.

## Known, accepted differences between devices after a copy
- A message deleted before a device ever fetched it leaves that device no tombstone row at all (pre-existing, independent of #1735); the transfer copies the source's tombstone rows, and never makes a deleted message live.
- Whether a deleted message's row has `edited_at` set depends on whether an edit reached that device before the delete; the text is gone either way.
- A pending change may wait forever for a target nobody holds (a message deleted before any device fetched it); bounded as in (2).
- Attachments copy as `_att` references; a receiver that lacks the conversation's file-key generation for an old attachment cannot open it (same as any device that joined later).

## Review round 1 (fixed)
- **Transient errors are never verdicts.** A network or DB error while verifying the acceptance propagates (the run stops, the transfer stays open, the next run resumes); only a signature, binding or decapsulation failure ends it as `failed`.
- **Re-put racing its own ack.** The DS answers a re-upload of an index the receiver acknowledged in the meantime with the ack, never a 409 (re-read after the insert refusal).
- **Ends are atomic and from an expected state.** Decline / cancel / complete flip the row and drop its chunks in one transaction, guarded on the state they end from, so a stale read cannot turn an accepted transfer into a declined one; an identical re-accept (re-signed) is a no-op; a taken transfer id is the uniform 404; the source must be an enrolled device, checked inside the request's transaction.
- **Local writes are guarded on an open state.** Sealing and importing update `history_transfer_local` only `WHERE state IN ('transferring','paused')` and require exactly one changed row, else roll back; a cancel wakes a waiting run; orphan outgoing chunks are pruned.
- **Conversations not joined yet are deferred, not dropped.** The receiver counts member conversations it holds no group for (`local_deferred`) and reports them in `deferred_conversations`.
- **Pending-change bounds.** The cap evicts edits first (a flood of edits never pushes out a delete), one edit is kept per (target, author), oversize edits are not stored, and an applied edit carries the time it was received.
- **Approver binding.** `/v1/enrollment/approve` now refuses an `approved_by_device_id` other than the signing device; the enrollment status returns it, and both apps offer the link-time copy only after a device approval.
- **Kills are real.** The `Crash::Transfer*` points unwind (`test_hooks::die`), with the import transaction open where it matters.
- Not changed, with reason: nudges on idempotent no-ops (content-free hints; one extra re-read), `PRAGMA secure_delete` (the local DB is SQLCipher: freed pages hold ciphertext).

## Review round 2 (fixed)
- **A lost answer is not a refusal.** If the answer to a request or an acceptance is lost (transport error, 5xx — a DS `SQLITE_BUSY` is already a retryable 503 — or a 429), the local row stays open with its key: `awaiting_approval` after a request, `transferring` after an approval. The next run reads the DS: it continues, re-sends the (idempotent) acceptance, or learns the request never landed (404 → expired, and a new request is not blocked). Only a DS that answered with a refusal ends the row, and then the DS is told to cancel.
- **Back-off on every non-Ok answer.** A throttled re-accept or ack goes through the waiter like every other wait; a run never hammers the DS.
- **The desktop remembers a nudge that lands mid-run** (`utils/transferRunCoalescer.ts`, mobile's rule): a run that ends unfinished goes once more.
- **Pending changes are bounded in bytes too** (4 MiB of edit text per conversation, edits evicted oldest first; deletes never by this cap), and eviction runs only past a cap. The 400-day horizon stays, because a backfill can come at any time and a change pruned before its target arrives would let an edited or deleted message come back.
- **Repeated DS calls nudge nobody** (`Outcome::Unchanged`).
- **A finished copy reads completed on the source** when the DS record is gone only if the source saw the final chunk's ack (else expired: the receiver may have cancelled or vanished).
- **Round 3:** a request or approval whose answer was lost is driven by a background run straight away (the in-flight claim dedups), so the transfer neither waits for the next unlock nor silently expires; the error says so. The pending-edit byte cap counts bytes (`length(CAST(content AS BLOB))`).

