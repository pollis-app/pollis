# Shared import scaffolding: encrypted history archives, seats, claims

Canonical spec for everything provider-agnostic in history import. Provider docs: `docs/slack-import-design.md` (#1179) and `docs/discord-import-design.md` (#1180) define only how their source is read, mapped into the record format below, and how seat links reach people. Where a provider doc and this file disagree, this file wins. Verified against `main` at `ec22dec`.

## 0. Tickets and PRs

- **The shared scaffolding lands in #1179's PR. Slack is the first provider.** #1180 builds on it in its own PR (the Discord provider, Discord-specific pages, its docs), adds no migration (000037 only if it genuinely needs one), and does not re-land anything below.
- One ticket = one PR (CLAUDE.md). #1179's PR therefore contains all of: migration 000036, DS endpoints and the redeem change, the `R2Family::Import` broker family, teardown, `pollis-api` types, the `pollis-core` import module, the desktop and mobile member UI and admin seat management, e2e on both tiers, every doc change in §16, and the Slack provider. Suggested commits: P1 schema + DS + teardown, P2 archive format + keys, P3 seats + redeem + claims, P4 rendering + redaction, P5 desktop + mobile UI + e2e, P6 Slack provider, P7 docs.
- No split boundary applies: the flows harness runs the DS in-process, so nothing in P2-P6 needs P1 deployed to be verified. Deploy ordering (DS before any client that mints seats) is a release-checklist step inside the one PR, not a reason to split.
- Before any code: amend the ticket text of #1179 and #1180 ("never its messages") or the PR lands history under a ticket that forbids it. The owner's revision of #856's "no re-import" line is stated in the PR (§16).

## 1. Model and trust

- An **import** is one archive for one group (or, Slack Phase C only, one user's personal archive). It has a random archive key `K`. The DS stores a manifest, chunks and files as R2 ciphertext plus rows that hold sizes, versions and opaque HMAC refs. It never sees the provider, workspace or guild, channel names, display names, emails, source account ids, or a provider token.
- **Distribution**: `K` is wrapped under a subkey of the group's existing pin key `Kpin` (#99). No new MLS frame type, no K-over-application-message.
- **Seats**: a single-use #847 invite link plus an `import_seat` row binding it to a `source_ref`. **Claims**: a signature by the redeemer's account identity key saying "that source account was me". Not "I vouch for this content".
- **Trusted for content and identity mapping: the importing admin.** They hold the plaintext, sign the manifest, mint every seat secret, and can fabricate records or redeem a seat with a sockpuppet. Nothing here prevents that; the UI and whitepaper say so (provider docs list the mitigations).
- **Untrusted: the DS.** Clients verify the manifest signature, Merkle roots, seat signatures, claim proofs and tombstone signatures. A dishonest DS can withhold or roll back (availability), not forge attribution or alter records undetected.
- **Learned by the DS**: that a group has imports; chunk count and padded sizes; file count and sizes; per seat an opaque `source_ref` and whether it is email-bound; who redeemed which seat and when; who claimed, withdrew, disputed; for an email-bound seat at redemption, that this Pollis account's email equals the seat's bound email (§5.4).

## 2. Keys and labels

HKDF-SHA256 (no salt), AES-256-GCM with a fresh 12-byte random nonce, `OsRng` only. Every `||` in an `info` string or AAD is a sequence of u16-big-endian length-prefixed fields (ids are UTF-8), so field boundaries cannot shift. Labels follow the `pollis/import-*/v1` form reconciled in `docs/discord-import-design.md`.

| Key | Derivation | Protects | AAD |
|---|---|---|---|
| `K` | 32 random bytes per import; `ImportKey` has no constructor but `OsRng` (no `From<&[u8]>`) | everything below | |
| group wrap KEK | `HKDF(Kpin, "pollis/import-wrap/v1" \|\| import_id)` | `K`, stored as `import_key_wrap.wrapped_k` (48 B) | `"kpin" \|\| import_id \|\| group_id` |
| personal wrap KEK | `HKDF(account identity key IKM, "pollis/import-personal-wrap/v1" \|\| import_id)`, same IKM as the vault (`vault.rs:64-67`, `pollis-vault-v1`) | `K` of a personal archive | `"personal" \|\| import_id \|\| owner_id` |
| manifest key | `HKDF(K, "pollis/import-manifest/v1" \|\| import_id)` | the manifest | `import_id` |
| chunk key | `HKDF(K, "pollis/import-chunk/v1" \|\| import_id \|\| chunk_id)` | a chunk, every version | `import_id \|\| chunk_id \|\| u32be(version)` |
| file key | 32 random bytes per file, carried in its record | one file | `import_id \|\| file_id` |
| source-ref key | `HKDF(K, "pollis/import-source-ref/v1")` | `source_ref = HMAC-SHA256(key, provider \|\| namespace \|\| account_id)`, lowercase hex; namespace = Slack `enterprise_id` or `team_id`, empty for Discord | |
| annotation key | `HKDF(K, "pollis/import-annotation/v1" \|\| import_id)` | annotations (dispute, release, corroborate) | `import_id \|\| annotation_id` |
| claim proof | `HMAC-SHA256(link secret, "pollis/import-claim-proof/v1")`, hex; seat stores `proof_hash = SHA-256(claim_proof)` | seat binding a client can check | |
| email MAC | `HMAC-SHA256(link secret, "pollis/import-seat-email/v1" \|\| canonical email)`, canonicalization identical to `users.email` (000026) | optional email binding | |

Signatures are ML-DSA-44 (2420 B, `ds_client.rs:17`) by the account identity key, verified against `account_key_log` at the stated `identity_version` (000005; exempt from user purge, `teardown.rs:125-132`, so a deleted signer stays verifiable). Signed bytes are deterministic CBOR arrays starting with the label:

| Label | Signer | Fields |
|---|---|---|
| `pollis/import-manifest-sig/v1` | importer | `SHA-256(canonical manifest without sig)` |
| `pollis/import-seat/v1` | the admin minting the seat's link | `import_id, seat_id, source_ref, proof_hash, email_bound, minted_by, identity_version` |
| `pollis/import-seat-revoke/v1` | a group admin | `import_id, seat_id, revoked_at, revoked_by, identity_version` |
| `pollis/import-claim/v1` | claimant | `group_id, import_id, seat_id, source_ref, user_id, identity_version, claim_proof, issued_at` |
| `pollis/import-claim-withdraw/v1` | claimant | `import_id, seat_id, withdrawn_at` |
| `pollis/import-redact/v1` | redactor | `import_id, chunk_id, leaf_hash, reason, by_user, identity_version, at` |
| `pollis/import-annotation-sig/v1` | annotation author (inside the ciphertext) | `import_id, annotation_id, kind, body hash` |

Hygiene note for the whitepaper: pins use `Kpin` directly as an AES key (`pinned_messages.rs:120`), imports only as HKDF input. A wrap can never be confused with, or swapped for, a pin row.

## 3. Archive format

### 3.1 Record and tombstone (deterministic CBOR, RFC 8949 §4.2.1; JSON has no canonical form to hash)

```
Record {
  src_id      text   provider-opaque, unique within the import ("C123/1700000000.123456", a Discord snowflake)
  conv        uint   index into manifest.conversations
  thread_root text?  src_id of the thread root
  reply_to    text?  src_id; a dangling one renders "original not imported"
  author      text   source_ref (hex)
  ts          text   RFC3339 UTC
  edited_at   text?  final text only; no edit history
  kind        text   default | reply | forward | system | bot
  body        text   Pollis markdown; mentions <src:SOURCE_REF>, channel refs <conv:N>
  files       [ { file_id, r2_key, file_key, nonce, name, mime, size, sha256_plain } ]
  reactions   [ { emoji, count, authors?: [source_ref] } ]   authors optional (Discord: counts only)
  pinned      bool   archive flag only; never a pinned_message row
  ext         map?   small provider extras (polls, stickers, forward snapshot); provider docs define keys
}
Tombstone { leaf_hash, reason: "author" | "erasure", by_user, identity_version, at, sig }
```

Leaf hash = `SHA-256("pollis/import-leaf/v1" || canonical Record)`. A tombstone keeps its record's leaf hash, so the importer-signed root still verifies after any number of redactions.

### 3.2 Chunks

- Records of one conversation in `ts` order, cut at a 1 MiB compressed target, zstd-compressed, then padded: `u32be(len) || compressed || zeros` up to a multiple of 64 KiB, at most 32 buckets (2 MiB).
- Object bytes = `nonce(12) || AES-GCM(padded) (+16 tag)`, so `size_bytes = 65536 * n + 28`, `1 <= n <= 32`. The migration CHECKs exactly that shape, so an unpadded or oversized chunk is unrepresentable.
- Plaintext = `{ import_id, chunk_id, version, leaves: [Record | Tombstone] }`.
- `chunk_id` = 128 random bits, 32 hex chars. Not channel + date: the DS learns only how many chunks and their padded sizes, never a per-channel activity histogram. Window, conversation and record count live only in the encrypted manifest.
- Root = RFC 6962 Merkle tree hash over the leaf hashes in order.

### 3.3 Manifest (write-once)

```
Manifest v1 {
  import_id, scope ("group"|"personal"), group_id | owner_id,
  provider ("slack"|"discord"|"fixture"), source_label, created_at,
  importer { user_id, identity_version },
  conversations [ { ref, kind: "channel"|"personal", pollis_channel_id?, name } ],
  identities    [ { source_ref, display_name, kind: "member"|"bot"|"external"|"deactivated"|"excluded" } ],
  chunks        [ { chunk_id, conv, first_ts, last_ts, record_count, merkle_root } ],
  files_total, files_skipped,
  consent { mode: "none"|"opt_out"|"opt_in", exclusion_list_hash? },
  provider_ext map?,
  sig
}
```

- Padded to 64 KiB buckets like a chunk, cap 16 MiB.
- **Decision (new):** the manifest is immutable once live, and `manifest_ver` from `docs/slack-import-design.md` is dropped. The manifest's R2 key is content-addressed (§9), and the DB refuses changing `manifest_key` after publish, so the key pins the bytes. Clients also record the manifest hash on first load and refuse a different one. Records found after publish (a late delta pass) become a new import. This closes `docs/slack-import-design.md`'s "nothing pins the manifest" concern without a version protocol.
- No email is ever in the archive. Emails stay in the admin's local import journal for link delivery and are discarded with it.

### 3.4 Files

- Encrypted under their own random file key (not convergent: convergent dedup would tell the operator this group imported a file equal to someone else's upload), uploaded as `import/<import_id>/<hex64>.enc`.
- Object cap `R2_MEDIA_MAX_BYTES` = 104 MiB (`pollis-api/src/broker.rs:99`), the same bound attachments have. Larger files are skipped and listed in the report. Chunked large-file upload is an open decision (§18).
- File sizes are visible to the operator, as for attachments (§9 of the whitepaper).

### 3.5 Not a restore path for #856

No `ImportProvider` exists for the #856 export format, and every provider parser refuses an input carrying the export marker (I24). That blocks only the naive route: a user can script their own `pollis-export.json` into Slack-ZIP or crawl-record shape. After this ships a restore-from-export channel exists in practice, and the docs say so (§16).

## 4. Key distribution and the waiting state

### 4.1 Group imports

- The importer obtains `Kpin` through `ensure_kpin(state, group_id, user_id, mint = true)` (`pollis-core/src/commands/pinned_messages.rs:258`, private today; make it `pub(crate)`). The create-time mint is best-effort (`pinned_messages.rs:452-463`), so a group may have no `Kpin` until then.
- Every current member holds `Kpin` by #99's construction (creator mint, Welcome adoption, or lazy adoption after external join, `.codesight/wiki/mls.md:641-652`), so every member unwraps `K`.

### 4.2 The waiting state, stated honestly

- **Seat redeemers and every newly enrolled device join by MLS external commit** (`pollis-core/src/commands/groups/invites.rs:333-339,397-410`). The joiner's own commit advances the epoch past the stored `Kpin` wrap, so it cannot unwrap `Kpin`. It adopts `Kpin` only after some `Kpin`-holding member device processes the join and runs `maybe_rewrap_pin_key` (`.codesight/wiki/mls.md:649-652`).
- Redeem already publishes `membership_changed` to the group room (`invites.rs:416-427`), so an online holder re-wraps promptly. The re-wrap also runs once per catch-up pass, so a holder that was offline re-wraps at its next launch.
- **There is no upper bound.** If no holder device runs Pollis, the joiner waits until one does. There is no polling and no fallback sender. Builds that predate #99 never re-wrap, so they cannot end the wait either. UI copy, ambient in the channel and on the claim banner: "Imported history will appear when another member of this group opens Pollis." It is a defined, honest intermediate state (the same one pins have), never an error.
- **Decision (new): claiming waits for `K`.** The claim action is disabled until the client can read the manifest, so the claimant sees the source display name and a per-channel count of "their" records before signing. Until then, `pending_import_claim` (local) keeps `seat_id, import_id, source_ref, claim_proof` from the redeem, and the secret itself is discarded.

### 4.3 Loss and exposure

- **Loss mode, inherited from #99:** if every device that ever held `Kpin` is lost and the stored wrap is stale, pins become unreadable (`pinned_messages.rs:43-46`), and with this design every import of that group does too. **Decision (new): no extra `K` escrow** in the admin's journal. Any device that still has `K` also has `Kpin` in `pin_key_cache` and can re-wrap, so a journal copy would add nothing but a second place to steal it from.
- **Removed members keep `K` and `Kpin`.** With a DS/R2 breach they read the whole archive, including chunks they never fetched (exposure is the archive, not "what they saw"), and the `K` of any later import of that group. **Decision (new): v1 does not rotate `Kpin` on removal.** Rotation is a #99 change that also re-encrypts pins and belongs in its own ticket. The schema keeps the wrap updatable by CAS (`wrap_version`) so rotation needs no migration. Documented as #99's residual widened to imports.

### 4.4 Personal archives (Slack Phase C only)

`K` is wrapped under the vault-derived personal KEK. Every device of the owner reads it, and account recovery via the Secret Key restores it. A destructive `reset_identity` (whitepaper §11.5) makes it unreadable, as it does the vault. The reset flow warns when personal archives exist. Personal archives have no seats, claims or annotations (triggers below).

## 5. Seats

### 5.1 Shape

- A seat is an `import_seat` row plus at most one #847 link that is `max_uses = 1`, `uses = 0`, unrevoked, has an `expires_at`, and belongs to the import's group. The DB enforces this by trigger, on insert and on every link change.
- **Lazy or eager minting.** A seat row may exist without a link (`link_id`, `proof_hash`, `seat_sig` all NULL). Attaching a link (`/v1/import-seats/attach-link`) sets `link_id`, `proof_hash`, optional `email_mac`, `seat_sig` and `minted_by` together, and re-attaching (re-issue after expiry) is allowed only while the seat is unredeemed and unrevoked. After redemption the binding is frozen.
- Dropped from the brief's `group_reservation`: `role` (a seat that grants admin turns a leaked link into a takeover; every seat lands as `member`), `channel_grants` (no per-channel ACL exists, `000019_pinned_messages.sql:3-4`), and any clear `display_hint` / `sealed_hint` (names live in the `K`-encrypted manifest).
- **Decision (new): `source_account_mac` is not in the migration.** Its only use was the Slack OAuth binding, which binds nothing unless the DS verifies a provider-signed ID token (`docs/slack-import-design.md` Phase B). A column that is always NULL is dead schema, and adding a nullable column later is additive.
- **Decision (new): the seat binding is signed** (`seat_sig`, §2) by the admin who minted the link. The importer-signed manifest cannot carry `proof_hash` once links are minted lazily or re-issued, so without `seat_sig` a compromised DS could insert a seat whose `proof_hash` it chose. Clients accept a seat signature by the importer or by an account the roster shows as a group admin when the seat was minted. That roster is DS-supplied, which is the same trust admin actions already rest on. The residual is stated in the whitepaper.

### 5.2 Link form

- **Decision (new):** seat links are delivered as `pollis://invite/<selector>.<secret>` plus the bare token for pasting. The https form is not used for seats, and no fragment form is introduced.
- Why: `extract()` takes the last path segment and drops `?` and `#` suffixes (`pollis-core/src/commands/groups/invite_token.rs:120-131`), so `https://pollis.com/invite/#<token>` fails to parse on every shipped build. The oldest mobile build in the stores could never redeem a fragment link. The path https form would put the secret in pollis.com edge logs, and `website/` has no `/invite` page anyway (the link 404s in a browser).
- The custom scheme never reaches a web server. On mobile it opens `mobile/app/invite/[token].tsx`, whose #1094 confirm-before-join step stays. On desktop no OS scheme handler is registered, so the member pastes the link or token into the join field. Universal links and app links remain unconfigured (`[token].tsx:27-29`).
- Provider docs define the delivery channel: the Slack bot DM or CSV, the Discord reaction-then-DM or claim desk.

### 5.3 Redeem change (`pollis-delivery/src/groups.rs` `apply_redeem_invite_link`, ~1587-1735)

Today an existing member short-circuits to idempotent success before the transaction (`groups.rs:1687-1695`): no use is burned, no redemption row is written. An existing member could therefore never claim a seat. New algorithm, for links that have an `import_seat` row:

1. Compute today's predicates (`secret_ok`, `not_revoked`, `not_expired`, `uses_left`) unchanged.
2. Look up the seat by `link_id`. Compute `email_ok` = `email_mac IS NULL` or a constant-time match of `HMAC(secret, label || users.email)`. Compute `reopen` = `seat.redeemed_by = caller`.
3. Verdict = `secret_ok && not_revoked && (reopen || (not_expired && uses_left && email_ok && seat unredeemed && seat unrevoked))`. All predicates are evaluated first, then combined: no early return between the secret compare and the verdict, as today.
4. Any failure produces the same `Rejected` as every other failure, and is counted by the durable rate limit as today.
5. `reopen` returns `Ok` with the `SeatGrant` and writes nothing. **Decision (new):** re-opening your own seat link is the recovery path for a redeemer whose client did not keep the claim proof, including a member who redeemed on an old build and updated later (§15). It works while the link row exists.
6. Otherwise, in one transaction: `add_member_rows` only if the caller is not already a member; the guarded `uses = uses + 1` UPDATE as today; `UPDATE import_seat SET redeemed_by, redeemed_at WHERE id = ? AND redeemed_by IS NULL AND revoked_at IS NULL`, which must touch exactly one row; then the redemption audit row. A unique-index violation (the caller already redeemed another seat of this import) rolls back to `Rejected`.
7. The `is_member` short-circuit stays as it is for links without a seat.

Response: the `Ok` variant of the internally tagged `RedeemInviteLinkResponse` (`pollis-api/src/groups.rs:253-266`) gains `#[serde(default)] seat: Option<SeatGrant { import_id, seat_id, source_ref }>`. `deny_unknown_fields` is used nowhere (grep), so old clients ignore the field.

### 5.4 Email binding (optional; Slack only in practice)

- At redemption the DS holds the secret (it is in the token) and the redeemer's OTP-verified `users.email`. Before redemption it holds only `sha256(secret)` and the MAC, so with a 256-bit unknown key it cannot test a dictionary of emails (I12). It never learns the email of anyone who does not redeem.
- Rejected alternative: storing `sha256(email)` so seats auto-surface at signup. That is dictionary-reversible and hands the operator every source-workspace email.
- The UX cost (the redeemer's Pollis email must equal the source's) and the linkage the DS learns are in `docs/slack-import-design.md` §4.

### 5.5 Teardown fix for the link-creator cascade

`group_invite_link.created_by` is `ON DELETE CASCADE` on paper (`000014_group_invite_link.sql:42`). Production runs `foreign_keys=OFF` (`teardown.rs:8-11`), so the clause never fires. What does run is `purge_user_rows`, which deletes every link the user created (`teardown.rs:299-306`). Both drafts' hazard (the importing admin's account deletion wipes everyone's seats and claims) is closed like this:

- Seats are never joined through `group_invite_link` or its redemption audit for anything after redemption. The redeemer lives on the seat row, and claims are keyed by `seat_id`.
- `purge_user_rows`, **before** it deletes the user's links: for unredeemed seats on those links, clear `link_id, proof_hash, email_mac, seat_sig, minted_by` (the seat becomes unminted, and another admin re-mints it). For redeemed seats, clear `link_id` only. Their binding, `redeemed_by` and claims survive.
- `minted_by` and `revoked_by` on a redeemed seat survive the minter's or revoker's account deletion. They are verification pointers into `account_key_log` (exempt), the same reasoning as that exemption. `docs/metadata-retention-policy.md` states this one retained id.
- The redeemer's own account deletion deletes their claims and their redeemed seats. The source identity becomes unseated, and an admin may create a new seat for it. **Decision (new; was open decision 2 in `docs/slack-import-design.md`):** delete, not "claimed by a deleted account", because a retained `redeemed_by` would be a deleted user's id kept forever.

## 6. Claims

- One claim per seat, ever (PRIMARY KEY `seat_id`). The DS inserts only after checking `SHA-256(claim_proof) = seat.proof_hash` in code (SQLite has no SHA-256), and a trigger requires that the caller is the seat's `redeemed_by`, is a current `group_member`, and that the seat is unrevoked on a live group import. `user_id` comes from device authentication, never from the body.
- **Client verification chain.** A client renders a source identity as a Pollis user only if every step passes, and otherwise renders it unclaimed (I9):
  1. The claim signature verifies under `account_key_log[user_id, identity_version]`, with safety pinning as for any peer key.
  2. `seat_sig` verifies (§5.1).
  3. `SHA-256(claim_proof) = proof_hash`.
  4. `source_ref` is in the importer-signed manifest.
  5. The claim is not withdrawn: if `withdraw_sig` is present and valid, it is.
  6. The seat is not revoked by a valid `revoke_sig`.
- **Withdraw**: one-way, signed. **Decision (new):** no re-claim by that user in that import in v1. The redeemed seat stays theirs (one seat per user per import), so a re-claim would need the admin to delete and re-create the seat, which v1 does not offer. This removes the re-claim loophole `docs/discord-import-design.md` found, at the DB layer.
- **Admin revocation** of a claimed seat (misattribution, sockpuppet found) is signed and one-way, and renders to everyone as "attribution revoked by @admin".
- A claim survives the claimant leaving or being removed from the group: it attests a historical fact. Rewrite rights do not survive (§8).
- I3 (one seat per user per import) forbids one person claiming two source identities (Slack U and W ids, or alt accounts). Intended for v1; an admin override is §18.

## 7. Rendering and local storage

- Imported records never enter `message`, `message_envelope` or the ingest, watermark, `known_envelope` and `undecryptable` paths (I21). They are not MLS-authenticated, and storing them there would make `sender_id` lie.
- Local tables (beside `pin_key_cache`, same trust domain): `import_key_cache(import_id, k)`; `import_chunk_cache(import_id, chunk_id, version, max_version_seen, last_used_at, plaintext)`; `imported_record(import_id, chunk_id, src_id, conversation_id, thread_root, ts, author_ref, ...)` with `imported_record_fts`, covering downloaded chunks only; `pending_import_claim`; admin-only `import_job` (resumable journal, provider checkpoints, emails until delivery).
- Imported caches sit outside lookback eviction: they are re-fetchable, with their own size cap and LRU, re-fetched on scroll. Full-archive search is desktop "download all"; mobile searches cached chunks only.
- Channel timeline: an "Imported history" boundary above the first live message, an ambient "This channel includes imported <provider> history" marker, and a permanent badge on every imported message: "Imported by @importer on <date>, not verified by its author". A claimed author renders as the Pollis user with "Imported from <provider>" and the original name on hover. An unclaimed one renders as "<display_name> (<provider>)" with no profile link. Bots render "(bot)" and are unclaimable. Excluded authors render as a timestamp-only placeholder.
- Threads render in the existing thread panel from `imported_record`. Imported pins appear as a separate "Imported pins" section, never as `pinned_message` rows (the DS learns nothing). Reactions are static emoji + count, never `message_reaction` rows: those are server plaintext keyed by `users.id`, so they would leak who reacted and cannot represent non-members.

## 8. Redaction, erasure, disputes

- **Author redaction (claimant).** A user with a live claim who is a current member rewrites the chunks holding their records, tombstoning leaves whose `author` equals their seat's `source_ref`, reason `author`. No admin liveness is needed.
- **Admin erasure.** A group admin tombstones every leaf of a `source_ref` (the per-author erasure tool, GDPR Art. 17/21; mandatory, because Pollis cannot find a person inside ciphertext), or any single leaf (moderation), reason `erasure`.
- **Client acceptance rule (I17).** Version `v+1` is accepted only if:
  - its root equals the importer-signed root;
  - every changed leaf went Record to Tombstone, never back, and nothing else changed;
  - each tombstone signature verifies by either the live claimant of that leaf's author or an account the roster shows as admin.

  Otherwise the client keeps its last-good cached version and reports it. The DS keeps no prior version: that would retain exactly what was erased.
- **Physical removal.** `/v1/imports/chunks/put` releases the old chunk object in the same transaction. `/v1/imports/gc` hands its key to a client, which deletes it via presign.
- **Files of tombstoned records.** The DS cannot tell which files a claimant may release, and tying files to authors would show the operator per-author file counts. **Decision (new):** a claimant's rewrite files a signed `release` annotation listing the file keys. Admin devices apply it on the `import_annotation` hint through `/v1/imports/objects/release` (admin only). Until then the bytes stay on R2, but their file key existed only in the deleted chunk version.
- **Version bump evicts caches.** On the `import_changed` hint, clients drop cached plaintext of older versions. Copies already on members' devices, and the archive as a whole for removed members holding `K`, are beyond Pollis's reach. The UI says so.
- **Rollback by a dishonest DS** (serving a pre-redaction version it kept) is detected only by clients that saw a newer version (`max_version_seen`). A fresh device cannot detect it. Physical erasure ultimately rests on the operator deleting bytes, as for any server-held data. Stated in the whitepaper.
- **Disputes.** A `dispute` annotation, encrypted under the annotation key and signed inside, names the `src_id`s and a reason, and renders to every member on those records. It is bounded to 4 KiB, 1000 per author per import (trigger). Flag-misattribution is a dispute. A `corroborate` annotation is a claimant's signed "confirmed by author" mark on records they matched against their own provider data (Discord data package, `docs/discord-import-design.md` step 6); clients honour it only from the live claimant of those records.

## 9. R2 family

- `R2Family::Import` joins the four existing families (`pollis-delivery/src/broker.rs:1431-1441`). Key shape `import/<import_id>/<hex64>.enc`, with `hex64 = SHA-256(object bytes)`. That satisfies the broker's "a PUT must be content-addressed" rule (`broker.rs:1576-1581`) and reveals nothing, because the bytes are ciphertext under random keys. It is never in `live_ref_exists!` and never shares objects with `media/`.
- Gates in `/v1/r2/presign`:
  - **PUT**: an unreleased `import_object` row names the key, `registered_by` = caller, and the signed `content_length` equals `size_bytes`.
  - **GET**: the caller is a member of the import's group, or its owner.
  - **DELETE**: no `import_object` row names the key.
- `put_max_bytes(Import)` = 16 MiB + 28 for manifests, 2 MiB + 28 for chunks, `R2_MEDIA_MAX_BYTES` for files, selected by the registered row's kind.
- **Collection follows `/v1/emoji/gc` exactly** (`pollis-delivery/src/emoji.rs:500-560`): event-driven, any authenticated device, idempotent. `/v1/imports/gc` deletes rows that are released or whose import is gone, and returns their keys. The caller deletes the bytes. Clients call it after delete-import, after a rewrite, after deleting a group, and before account deletion completes. A client that crashes in between leaves unreadable ciphertext until the next gc caller, the same residual emoji has.

## 10. Migration `pollis-schema/migrations/000036_import_archive.sql`

One file, all shared tables. 000035 is the newest on `main`, so renumber only if another branch lands 000036 first. Additive only: no edit to existing tables, no baseline edit, no `schema_migrations` insert. No `REFERENCES` clause: production runs `foreign_keys=OFF`, so a declared cascade is inert and misleading (`teardown.rs:1-21`). Every invariant is a CHECK, a UNIQUE, or a `RAISE(ABORT)` trigger (precedent `000017_conversation_guard_triggers.sql`), and every delete is explicit in `teardown.rs`.

```sql
-- #1179 / #1180: encrypted history import (spec: import-history-design.md).
-- The DS stores ciphertext, padded sizes, versions and opaque HMAC refs. No
-- column names the source platform, a source account, a display name or an
-- email. foreign_keys is OFF in production, so there are no REFERENCES
-- clauses: invariants are CHECKs, UNIQUEs and triggers, deletes are explicit
-- in pollis-delivery/src/teardown.rs.

CREATE TABLE import_archive (
    id           TEXT PRIMARY KEY,
    scope        TEXT NOT NULL CHECK (scope IN ('group', 'personal')),
    group_id     TEXT,
    owner_id     TEXT,
    -- NULLed on the importer's account deletion once live; a staging import
    -- of a deleted importer is deleted instead (nobody can finish it).
    created_by   TEXT,
    status       TEXT NOT NULL DEFAULT 'staging' CHECK (status IN ('staging', 'live')),
    -- Content-addressed R2 key of the write-once manifest; set at publish.
    manifest_key TEXT UNIQUE,
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),
    published_at TEXT,
    CHECK ((scope = 'group') = (group_id IS NOT NULL)),
    CHECK ((scope = 'personal') = (owner_id IS NOT NULL)),
    CHECK (scope = 'group' OR created_by = owner_id),
    CHECK (status = 'live' OR created_by IS NOT NULL),
    CHECK ((status = 'live') = (manifest_key IS NOT NULL)),
    CHECK ((status = 'live') = (published_at IS NOT NULL))
);
CREATE INDEX idx_import_archive_group ON import_archive(group_id) WHERE group_id IS NOT NULL;
CREATE INDEX idx_import_archive_owner ON import_archive(owner_id) WHERE owner_id IS NOT NULL;
CREATE INDEX idx_import_archive_created_by ON import_archive(created_by) WHERE created_by IS NOT NULL;

-- An import starts staging, on a group that exists now (the conversation
-- registry is append-only, so `groups`, not `conversation`, is the live check)
-- or on the owner's own live account.
CREATE TRIGGER import_archive_insert_gate
BEFORE INSERT ON import_archive FOR EACH ROW
WHEN NEW.status <> 'staging'
  OR (NEW.scope = 'group' AND NOT EXISTS (SELECT 1 FROM groups WHERE id = NEW.group_id))
  OR (NEW.scope = 'personal' AND NOT EXISTS (SELECT 1 FROM users WHERE id = NEW.owner_id))
BEGIN SELECT RAISE(ABORT, 'import must start staging on a live group or account'); END;

-- Identity immutable; publish one-way and complete; manifest frozen once live;
-- created_by may only be NULLed.
CREATE TRIGGER import_archive_update_gate
BEFORE UPDATE ON import_archive FOR EACH ROW
WHEN NEW.id IS NOT OLD.id OR NEW.scope IS NOT OLD.scope
  OR NEW.group_id IS NOT OLD.group_id OR NEW.owner_id IS NOT OLD.owner_id
  OR NEW.created_at IS NOT OLD.created_at
  OR (NEW.created_by IS NOT OLD.created_by AND NEW.created_by IS NOT NULL)
  OR (OLD.status = 'live' AND (NEW.status IS NOT 'live'
        OR NEW.manifest_key IS NOT OLD.manifest_key
        OR NEW.published_at IS NOT OLD.published_at))
  OR (OLD.status = 'staging' AND NEW.status = 'live' AND (
        NOT EXISTS (SELECT 1 FROM import_object o
                    WHERE o.r2_key = NEW.manifest_key AND o.import_id = NEW.id
                      AND o.kind = 'manifest' AND o.released_at IS NULL)
     OR NOT EXISTS (SELECT 1 FROM import_chunk c WHERE c.import_id = NEW.id)
     OR NOT EXISTS (SELECT 1 FROM import_key_wrap w WHERE w.import_id = NEW.id)))
BEGIN SELECT RAISE(ABORT, 'import update not permitted'); END;

-- K wrapped under the group's Kpin-derived KEK, or the owner's vault-derived
-- KEK. Exactly one row per import (the target is forced to the import's own
-- group or owner). Updatable only by CAS on wrap_version, so a future Kpin
-- rotation needs no migration.
CREATE TABLE import_key_wrap (
    import_id    TEXT NOT NULL,
    wrap_target  TEXT NOT NULL,
    kek_kind     TEXT NOT NULL CHECK (kek_kind IN ('kpin', 'personal')),
    wrapped_k    BLOB NOT NULL CHECK (length(wrapped_k) = 48),
    nonce        BLOB NOT NULL CHECK (length(nonce) = 12),
    wrap_version INTEGER NOT NULL DEFAULT 1 CHECK (wrap_version >= 1),
    PRIMARY KEY (import_id, wrap_target)
);

CREATE TRIGGER import_key_wrap_insert_gate
BEFORE INSERT ON import_key_wrap FOR EACH ROW
WHEN NEW.wrap_version <> 1 OR NOT EXISTS (
    SELECT 1 FROM import_archive a
    WHERE a.id = NEW.import_id AND a.status = 'staging'
      AND ((a.scope = 'group' AND NEW.kek_kind = 'kpin' AND NEW.wrap_target = a.group_id)
        OR (a.scope = 'personal' AND NEW.kek_kind = 'personal' AND NEW.wrap_target = a.owner_id)))
BEGIN SELECT RAISE(ABORT, 'key wrap does not match its import'); END;

CREATE TRIGGER import_key_wrap_cas
BEFORE UPDATE ON import_key_wrap FOR EACH ROW
WHEN NEW.import_id IS NOT OLD.import_id OR NEW.wrap_target IS NOT OLD.wrap_target
  OR NEW.kek_kind IS NOT OLD.kek_kind OR NEW.wrap_version <> OLD.wrap_version + 1
BEGIN SELECT RAISE(ABORT, 'key wrap update must advance wrap_version by one'); END;

-- Every R2 object an import owns. The single liveness registry the broker's
-- PUT and DELETE gates read. Rows go only through /v1/imports/gc (released,
-- or import gone), mirroring custom_emoji_object.
CREATE TABLE import_object (
    r2_key        TEXT PRIMARY KEY,
    import_id     TEXT NOT NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('manifest', 'chunk', 'file')),
    size_bytes    INTEGER NOT NULL,
    -- PUT authz; NULLed on the registrant's account deletion.
    registered_by TEXT,
    registered_at TEXT NOT NULL DEFAULT (datetime('now')),
    released_at   TEXT,
    -- import/<import_id>/<sha256 of the object bytes, lowercase hex>.enc
    CHECK (length(r2_key) = length(import_id) + 76
       AND substr(r2_key, 1, length(import_id) + 8) = 'import/' || import_id || '/'
       AND substr(r2_key, length(import_id) + 9, 64) NOT GLOB '*[^0-9a-f]*'
       AND substr(r2_key, length(import_id) + 73) = '.enc'),
    -- nonce(12) || AES-GCM over 64 KiB-bucketed plaintext (+16 tag)
    CHECK (kind <> 'chunk' OR (size_bytes BETWEEN 65564 AND 2097180
                               AND (size_bytes - 28) % 65536 = 0)),
    CHECK (kind <> 'manifest' OR (size_bytes BETWEEN 65564 AND 16777244
                                  AND (size_bytes - 28) % 65536 = 0)),
    -- R2_MEDIA_MAX_BYTES, pollis-api/src/broker.rs:99
    CHECK (kind <> 'file' OR (size_bytes > 28 AND size_bytes <= 109051904))
);
CREATE INDEX idx_import_object_import ON import_object(import_id);
CREATE INDEX idx_import_object_registered_by ON import_object(registered_by) WHERE registered_by IS NOT NULL;

-- Manifests and files only while staging; chunk objects also while live
-- (a rewrite registers its new version).
CREATE TRIGGER import_object_insert_gate
BEFORE INSERT ON import_object FOR EACH ROW
WHEN NEW.released_at IS NOT NULL OR NEW.registered_by IS NULL OR NOT EXISTS (
    SELECT 1 FROM import_archive a
    WHERE a.id = NEW.import_id AND (a.status = 'staging' OR NEW.kind = 'chunk'))
BEGIN SELECT RAISE(ABORT, 'object registration not permitted'); END;

-- Objects change only by a one-way release of an unreferenced object, or by
-- NULLing registered_by in teardown.
CREATE TRIGGER import_object_update_gate
BEFORE UPDATE ON import_object FOR EACH ROW
WHEN NEW.r2_key IS NOT OLD.r2_key OR NEW.import_id IS NOT OLD.import_id
  OR NEW.kind IS NOT OLD.kind OR NEW.size_bytes IS NOT OLD.size_bytes
  OR NEW.registered_at IS NOT OLD.registered_at
  OR (NEW.registered_by IS NOT OLD.registered_by AND NEW.registered_by IS NOT NULL)
  OR (OLD.released_at IS NOT NULL AND NEW.released_at IS NOT OLD.released_at)
  OR (NEW.released_at IS NOT NULL AND OLD.released_at IS NULL AND (
        EXISTS (SELECT 1 FROM import_chunk c WHERE c.r2_key = NEW.r2_key)
     OR EXISTS (SELECT 1 FROM import_archive a WHERE a.manifest_key = NEW.r2_key)))
BEGIN SELECT RAISE(ABORT, 'objects change only by release'); END;

-- Versioned chunk pointers. No user column: who rewrote a chunk is in the
-- signed tombstones, where members can check it.
CREATE TABLE import_chunk (
    import_id  TEXT NOT NULL,
    chunk_id   TEXT NOT NULL CHECK (length(chunk_id) = 32 AND chunk_id NOT GLOB '*[^0-9a-f]*'),
    version    INTEGER NOT NULL CHECK (version >= 1),
    r2_key     TEXT NOT NULL UNIQUE,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (import_id, chunk_id)
);

CREATE TRIGGER import_chunk_insert_gate
BEFORE INSERT ON import_chunk FOR EACH ROW
WHEN NEW.version <> 1
  OR NOT EXISTS (SELECT 1 FROM import_archive a WHERE a.id = NEW.import_id AND a.status = 'staging')
  OR NOT EXISTS (SELECT 1 FROM import_object o
                 WHERE o.r2_key = NEW.r2_key AND o.import_id = NEW.import_id
                   AND o.kind = 'chunk' AND o.released_at IS NULL)
BEGIN SELECT RAISE(ABORT, 'chunk insert not permitted'); END;

-- A rewrite: live import, version + 1 exactly (the endpoint's CAS is
-- WHERE version = expected_version), onto a fresh registered chunk object.
CREATE TRIGGER import_chunk_rewrite_gate
BEFORE UPDATE ON import_chunk FOR EACH ROW
WHEN NEW.import_id IS NOT OLD.import_id OR NEW.chunk_id IS NOT OLD.chunk_id
  OR NEW.version <> OLD.version + 1 OR NEW.r2_key = OLD.r2_key
  OR NOT EXISTS (SELECT 1 FROM import_archive a WHERE a.id = NEW.import_id AND a.status = 'live')
  OR NOT EXISTS (SELECT 1 FROM import_object o
                 WHERE o.r2_key = NEW.r2_key AND o.import_id = NEW.import_id
                   AND o.kind = 'chunk' AND o.released_at IS NULL)
BEGIN SELECT RAISE(ABORT, 'chunk rewrite must advance by one onto a fresh registered object'); END;

-- A seat: the sidecar of a single-use #847 link, bound to an opaque source_ref.
-- Not a nullable invitee on group_invite (000014's comment says why).
CREATE TABLE import_seat (
    id          TEXT PRIMARY KEY,
    import_id   TEXT NOT NULL,
    -- HMAC-SHA256 under a K-derived key; the DS never holds K.
    source_ref  TEXT NOT NULL CHECK (length(source_ref) = 64 AND source_ref NOT GLOB '*[^0-9a-f]*'),
    -- NULL until minted (lazy), or after the minter's account teardown.
    link_id     TEXT UNIQUE,
    proof_hash  TEXT CHECK (proof_hash IS NULL OR (length(proof_hash) = 64 AND proof_hash NOT GLOB '*[^0-9a-f]*')),
    email_mac   TEXT CHECK (email_mac IS NULL OR (length(email_mac) = 64 AND email_mac NOT GLOB '*[^0-9a-f]*')),
    seat_sig    BLOB CHECK (seat_sig IS NULL OR length(seat_sig) = 2420),
    minted_by   TEXT,
    redeemed_by TEXT,
    redeemed_at TEXT,
    revoked_at  TEXT,
    revoked_by  TEXT,
    revoke_sig  BLOB CHECK (revoke_sig IS NULL OR length(revoke_sig) = 2420),
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (import_id, source_ref),
    CHECK ((redeemed_by IS NULL) = (redeemed_at IS NULL)),
    CHECK ((proof_hash IS NULL) = (seat_sig IS NULL)),
    CHECK ((seat_sig IS NULL) = (minted_by IS NULL)),
    CHECK (link_id IS NULL OR proof_hash IS NOT NULL),
    CHECK (redeemed_by IS NULL OR proof_hash IS NOT NULL),
    CHECK (email_mac IS NULL OR proof_hash IS NOT NULL),
    CHECK ((revoked_at IS NULL) = (revoke_sig IS NULL)),
    CHECK ((revoked_at IS NULL) = (revoked_by IS NULL))
);
-- I3: one redeemed seat per user per import (also makes withdrawal final).
CREATE UNIQUE INDEX idx_import_seat_one_per_user
    ON import_seat(import_id, redeemed_by) WHERE redeemed_by IS NOT NULL;
CREATE INDEX idx_import_seat_redeemed_by ON import_seat(redeemed_by) WHERE redeemed_by IS NOT NULL;

CREATE TRIGGER import_seat_insert_gate
BEFORE INSERT ON import_seat FOR EACH ROW
WHEN NEW.redeemed_by IS NOT NULL OR NEW.revoked_at IS NOT NULL
  OR NOT EXISTS (SELECT 1 FROM import_archive a
                 WHERE a.id = NEW.import_id AND a.scope = 'group' AND a.status = 'live')
  OR (NEW.link_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM group_invite_link l JOIN import_archive a ON a.id = NEW.import_id
        WHERE l.id = NEW.link_id AND l.group_id = a.group_id AND l.max_uses = 1
          AND l.uses = 0 AND l.revoked_at IS NULL AND l.expires_at IS NOT NULL))
BEGIN SELECT RAISE(ABORT, 'seat needs a live group import and a fresh single-use expiring link'); END;

CREATE TRIGGER import_seat_identity_immutable
BEFORE UPDATE ON import_seat FOR EACH ROW
WHEN NEW.id IS NOT OLD.id OR NEW.import_id IS NOT OLD.import_id
  OR NEW.source_ref IS NOT OLD.source_ref OR NEW.created_at IS NOT OLD.created_at
BEGIN SELECT RAISE(ABORT, 'seat identity is immutable'); END;

-- (Re)binding a link: unredeemed, unrevoked seat; fresh single-use expiring
-- link of the same group.
CREATE TRIGGER import_seat_link_change
BEFORE UPDATE OF link_id ON import_seat FOR EACH ROW
WHEN NEW.link_id IS NOT NULL AND NEW.link_id IS NOT OLD.link_id AND (
     OLD.redeemed_by IS NOT NULL OR OLD.revoked_at IS NOT NULL
  OR NOT EXISTS (SELECT 1 FROM group_invite_link l JOIN import_archive a ON a.id = NEW.import_id
        WHERE l.id = NEW.link_id AND l.group_id = a.group_id AND l.max_uses = 1
          AND l.uses = 0 AND l.revoked_at IS NULL AND l.expires_at IS NOT NULL))
BEGIN SELECT RAISE(ABORT, 'seat needs a fresh single-use expiring link of its group'); END;

-- The binding changes only together with the link, and never after redemption.
CREATE TRIGGER import_seat_binding_frozen
BEFORE UPDATE ON import_seat FOR EACH ROW
WHEN (NEW.proof_hash IS NOT OLD.proof_hash OR NEW.email_mac IS NOT OLD.email_mac
      OR NEW.seat_sig IS NOT OLD.seat_sig OR NEW.minted_by IS NOT OLD.minted_by)
  AND (OLD.redeemed_by IS NOT NULL OR NEW.link_id IS OLD.link_id)
BEGIN SELECT RAISE(ABORT, 'seat binding changes only with a new link on an unredeemed seat'); END;

-- Redeemed once, through its current link, never moved.
CREATE TRIGGER import_seat_redeem_once
BEFORE UPDATE OF redeemed_by ON import_seat FOR EACH ROW
WHEN OLD.redeemed_by IS NOT NULL OR NEW.redeemed_by IS NULL
  OR OLD.link_id IS NULL OR NEW.link_id IS NOT OLD.link_id OR OLD.revoked_at IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'a seat is redeemed once, through its link'); END;

CREATE TRIGGER import_seat_revoke_once
BEFORE UPDATE ON import_seat FOR EACH ROW
WHEN OLD.revoked_at IS NOT NULL AND (NEW.revoked_at IS NOT OLD.revoked_at
     OR NEW.revoke_sig IS NOT OLD.revoke_sig OR NEW.revoked_by IS NOT OLD.revoked_by)
BEGIN SELECT RAISE(ABORT, 'seat revocation is one-way'); END;

-- One claim per seat, ever.
CREATE TABLE import_claim (
    seat_id          TEXT PRIMARY KEY,
    import_id        TEXT NOT NULL,
    user_id          TEXT NOT NULL,
    identity_version INTEGER NOT NULL,
    -- Preimage of import_seat.proof_hash (the DS checks the hash in code).
    claim_proof      TEXT NOT NULL CHECK (length(claim_proof) = 64 AND claim_proof NOT GLOB '*[^0-9a-f]*'),
    issued_at        TEXT NOT NULL,
    signature        BLOB NOT NULL CHECK (length(signature) = 2420),
    created_at       TEXT NOT NULL DEFAULT (datetime('now')),
    withdrawn_at     TEXT,
    withdraw_sig     BLOB CHECK (withdraw_sig IS NULL OR length(withdraw_sig) = 2420),
    CHECK ((withdrawn_at IS NULL) = (withdraw_sig IS NULL))
);
CREATE INDEX idx_import_claim_import ON import_claim(import_id);
CREATE INDEX idx_import_claim_user ON import_claim(user_id);

CREATE TRIGGER import_claim_needs_seat
BEFORE INSERT ON import_claim FOR EACH ROW
WHEN NEW.withdrawn_at IS NOT NULL OR NOT EXISTS (
    SELECT 1 FROM import_seat s
    JOIN import_archive a ON a.id = s.import_id AND a.status = 'live' AND a.scope = 'group'
    JOIN group_member m ON m.group_id = a.group_id AND m.user_id = NEW.user_id
    WHERE s.id = NEW.seat_id AND s.import_id = NEW.import_id
      AND s.redeemed_by = NEW.user_id AND s.revoked_at IS NULL)
BEGIN SELECT RAISE(ABORT, 'claim without a redeemed seat'); END;

CREATE TRIGGER import_claim_immutable
BEFORE UPDATE ON import_claim FOR EACH ROW
WHEN OLD.withdrawn_at IS NOT NULL OR NEW.withdrawn_at IS NULL
  OR NEW.seat_id IS NOT OLD.seat_id OR NEW.import_id IS NOT OLD.import_id
  OR NEW.user_id IS NOT OLD.user_id OR NEW.identity_version IS NOT OLD.identity_version
  OR NEW.claim_proof IS NOT OLD.claim_proof OR NEW.issued_at IS NOT OLD.issued_at
  OR NEW.signature IS NOT OLD.signature OR NEW.created_at IS NOT OLD.created_at
BEGIN SELECT RAISE(ABORT, 'claims are immutable except one-way withdrawal'); END;

-- Disputes, file-release requests, author corroborations. Encrypted under a K-derived key and
-- signed inside, so which records they name is not server-visible.
CREATE TABLE import_annotation (
    id          TEXT PRIMARY KEY,
    import_id   TEXT NOT NULL,
    kind        TEXT NOT NULL CHECK (kind IN ('dispute', 'release', 'corroborate')),
    author_id   TEXT NOT NULL,
    blob        BLOB NOT NULL CHECK (length(blob) BETWEEN 17 AND 4096),
    nonce       BLOB NOT NULL CHECK (length(nonce) = 12),
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    resolved_at TEXT
);
CREATE INDEX idx_import_annotation_import ON import_annotation(import_id, created_at);
CREATE INDEX idx_import_annotation_author ON import_annotation(author_id);

CREATE TRIGGER import_annotation_insert_gate
BEFORE INSERT ON import_annotation FOR EACH ROW
WHEN NEW.resolved_at IS NOT NULL
  OR NOT EXISTS (SELECT 1 FROM import_archive a
                 JOIN group_member m ON m.group_id = a.group_id AND m.user_id = NEW.author_id
                 WHERE a.id = NEW.import_id AND a.status = 'live' AND a.scope = 'group')
  OR (SELECT COUNT(*) FROM import_annotation x
      WHERE x.import_id = NEW.import_id AND x.author_id = NEW.author_id) >= 1000
BEGIN SELECT RAISE(ABORT, 'annotation not permitted'); END;

CREATE TRIGGER import_annotation_resolve_only
BEFORE UPDATE ON import_annotation FOR EACH ROW
WHEN OLD.resolved_at IS NOT NULL OR NEW.resolved_at IS NULL
  OR NEW.id IS NOT OLD.id OR NEW.import_id IS NOT OLD.import_id OR NEW.kind IS NOT OLD.kind
  OR NEW.author_id IS NOT OLD.author_id OR NEW.blob IS NOT OLD.blob
  OR NEW.nonce IS NOT OLD.nonce OR NEW.created_at IS NOT OLD.created_at
BEGIN SELECT RAISE(ABORT, 'annotations change only by one-way resolution'); END;
```

Still admitted, deliberately, each with its enforcing layer:

- Rewrite and release authz (admin, or live claimant who is a current member) needs the actor, which a trigger does not have. It is a DS chokepoint with a test (I16).
- `SHA-256(claim_proof) = proof_hash` is checked in DS code (no SHA-256 in SQLite) and again by every client (I9).
- The DS cannot check that a `source_ref` is in the manifest. Clients ignore seats and claims whose ref is not.
- A claim outlives its claimant's group membership (intended, §6).
- Staging imports abandoned by a lost device: the existing hourly DS sweep (`main.rs:137-175`, `sweep_aged_records`) deletes `staging` imports older than 30 days with their rows. That is the only TTL, it applies to unpublished uploads, and it never touches a live import.

## 11. Teardown (`pollis-delivery/src/teardown.rs`)

- `purge_group`: delete `import_annotation`, `import_claim`, `import_seat`, `import_chunk`, `import_key_wrap`, `import_archive` for the group's imports, each reached by a subquery anchored on `import_archive.group_id = ?1` (the scoping rule in the module header), so it runs before the `import_archive` anchor rows go (children before parents). `import_object` rows are left for `/v1/imports/gc` (their import is gone), exactly as `release_group_emoji` leaves emoji objects.
- `purge_user_rows` (account deletion and reset-recover):
  - delete the user's personal imports and their rows;
  - delete `staging` imports with `created_by = ?1`;
  - NULL `created_by` on their live imports and `registered_by` on their objects;
  - apply the §5.5 seat handling before the user's links go;
  - delete their claims, their redeemed seats and their annotations.
- Lists: all seven tables join `CONVERSATION_PURGED_TABLES` and `USER_PURGED_TABLES`, except `import_object`, which joins both exemption lists with the reason "released, collected by `/v1/imports/gc`" (the `custom_emoji_object` precedent). `pollis-delivery/tests/teardown.rs` `USER_COLUMNS` gains `import_archive.created_by`, `import_archive.owner_id`, `import_object.registered_by`, `import_seat.redeemed_by`, `import_seat.minted_by`, `import_seat.revoked_by`, `import_claim.user_id`, `import_annotation.author_id`; `GROUP_COLUMNS` gains `import_archive.group_id`.

## 12. DS endpoints (new `pollis-api/src/imports.rs`, registered in the `endpoints!` table)

Existing naming: `/v1/<resource>/<verb>`, hyphenated multi-word resources (`/v1/invite-links/redeem`, `/v1/join-requests/approve`), reads as `/v1/read/<resource>` (`/v1/read/pin-keystate`), and the `/v1/emoji/gc` collect precedent (`pollis-api/src/lib.rs:237-390`). No existing path or table uses `import` (grep). All writes are device-signed `ds_post` (`pollis-core/src/commands/mls/ds_client.rs`). `user_id` always comes from device auth, never a body. `pollis-delivery/tests/endpoint_coverage.rs` and the flows-harness mirror cover routing automatically.

| Path | Body | Who | Rule |
|---|---|---|---|
| `/v1/imports/create` | `CreateImportBody { import_id, scope, group_id? }` | group admin; self for personal | inserts `staging` |
| `/v1/imports/key-wrap` | `PutImportKeyWrapBody { import_id, wrap_target, kek_kind, wrapped_k, nonce, expected_version? }` | creator while staging; admin for a CAS update | |
| `/v1/imports/objects/register` | `RegisterImportObjectsBody { import_id, objects: [{ r2_key, kind, size_bytes }] }` | creator (staging); rewriter (chunk, live) | batch at most 500; precedes the presigned PUT |
| `/v1/imports/chunks/put` | `PutImportChunksBody { import_id, chunks: [{ chunk_id, r2_key, expected_version? }] }` | absent version: creator, staging; `Some(v)`: group admin, or live claimant who is a current member | rewrite = `UPDATE ... WHERE version = v`, 409 on a lost race, old object released in the same tx; batch at most 100 |
| `/v1/imports/objects/release` | `ReleaseImportObjectsBody { import_id, r2_keys }` | group admin | `kind = 'file'` only |
| `/v1/imports/publish` | `PublishImportBody { import_id, manifest_key }` | creator | one-way (trigger) |
| `/v1/imports/delete` | `DeleteImportBody { import_id }` | group admin; owner | deletes every row but `import_object` |
| `/v1/imports/gc` | `ImportGcBody {}` -> `ImportGcResponse { r2_keys }` | any authenticated device | collects released and orphaned objects |
| `/v1/import-seats/create` | `CreateImportSeatsBody { import_id, seats: [{ seat_id, source_ref, link?: SeatLink }] }`, `SeatLink { link: CreateInviteLinkBody, proof_hash, email_mac?, seat_sig, identity_version }` | group admin | links (`max_uses = 1`, `expires_at` required) and seats in one tx; batch at most 500 |
| `/v1/import-seats/attach-link` | `AttachImportSeatLinkBody { seat_id, link: SeatLink }` | group admin | lazy mint or re-issue; revokes the previous link in the same tx |
| `/v1/import-seats/revoke` | `RevokeImportSeatBody { seat_id, revoked_at, revoke_sig, identity_version }` | group admin | one-way; also revokes the link |
| `/v1/import-claims/create` | `CreateImportClaimBody { seat_id, import_id, identity_version, claim_proof, issued_at, signature }` | the redeemer | proof hash checked in code, rest by trigger |
| `/v1/import-claims/withdraw` | `WithdrawImportClaimBody { seat_id, withdrawn_at, withdraw_sig }` | claimant | one-way |
| `/v1/import-annotations/create` | `CreateImportAnnotationBody { id, import_id, kind, blob, nonce }` | current member | size and count bounds (trigger) |
| `/v1/import-annotations/resolve` | `ResolveImportAnnotationBody { id }` | group admin | one-way |
| `/v1/invite-links/redeem` (existing) | `RedeemInviteLinkBody` unchanged | as today | seat branch §5.3; response gains `seat` |
| `/v1/invite-links/revoke` (existing) | unchanged | as today | revokes an unredeemed seat's link |
| `/v1/read/imports` | `ImportsBody { group_id? }` (None = my personal imports) | member; owner | imports + wraps + manifest keys |
| `/v1/read/import-chunks` | `ImportChunksBody { import_id, chunk_ids? }` | member | `chunk_id, version, r2_key` |
| `/v1/read/import-claims` | `ImportClaimsBody { import_id }` | member | claims joined with their seat binding (`source_ref, proof_hash, seat_sig, minted_by, revoked_*`) |
| `/v1/read/import-seats` | `ImportSeatsBody { import_id }` | group admin | every seat incl. link and redemption state |
| `/v1/read/import-annotations` | `ImportAnnotationsBody { import_id, since? }` | member | |

- R2 bytes go through the existing `/v1/r2/presign` with the §9 gates.
- **Realtime hints, no polling.** After a write the client publishes `import_changed { group_id, import_id }` to the group room (publish, rewrite, claim, withdraw, revoke, delete), and `import_annotation { group_id, import_id }` to admins' `inbox-{userId}` rooms (`livekit_signalling.rs:173`), via `livekit::publish_to_room_server` exactly as redeem does (`invites.rs:416-427`).
- None of these bodies has a field that could carry a provider token, a source account id, a name or an email (I23).

## 13. pollis-core (`pollis-core/src/commands/import/`; does not exist today)

- `mod.rs`: the `ImportProvider` trait. `plan(source) -> ImportPlan`, `identities() -> Vec<SourceIdentity>`, `records(conversation) -> impl Iterator<Item = ImportRecord>`, `fetch_file(file_ref) -> bytes`. Providers: `slack/` (#1179), `discord/` (#1180), and `fixture/` compiled only under `test-harness` for flows and e2e seeding.
- `archive.rs` (`ImportKey`, chunker, padding, Merkle, manifest build + sign), `keys.rs` (labels as constants; wrap via `ensure_kpin` / `pin_key_cache`), `seats.rs`, `claims.rs` (sign, verify chain, `source_ref -> Option<UserId>`), `rewrite.rs` (tombstones, the acceptance rule), `render.rs` (local tables, FTS), `ds.rs` (typed `ds_post` calls), reads in `ds_reads.rs`.
- `pollis-core` stays libsql-free. Provider clients are outbound HTTPS like the R2 client. A provider token never leaves the provider module: it is not `Serialize`, and it lives in memory or in the keystore with a deadline, never in the local DB, logs, report, or any DS body.
- Shared commands (impl here, thin shims in `src-tauri/src/commands/`, registered in `src-tauri/src/lib.rs` `invoke_handler!` and `src-tauri/src/test_harness.rs`, uniffi-exported for mobile, listed in `.codesight/wiki/commands.md`). Provider-specific `import_slack_preview` / `import_discord_plan` produce an `ImportPlan` that the shared ones consume.
  - Admin, long-running (desktop): `import_apply(plan_id)`, `import_job_status(job_id)`, `import_cancel(job_id)`.
  - Admin, short (desktop + mobile): `list_imports(group_id)`, `delete_import(import_id)`, `list_import_seats(import_id)`, `mint_import_seat_links(import_id, seat_ids, expires_at, email_bound)` (returns tokens to the provider's delivery step), `revoke_import_seat(seat_id)`, `erase_import_source(import_id, source_ref)`, `apply_import_release_requests(import_id)`.
  - Member (desktop + mobile): `list_imported_messages(conversation_id, before, limit)`, `get_imported_thread(import_id, src_id)`, `search_imported_messages(query)`, `pending_import_claims()`, `claim_import_seat(seat_id)`, `withdraw_import_claim(seat_id)`, `redact_imported_messages(import_id, src_ids)`, `dispute_imported_message(import_id, src_ids, reason)`, `list_import_disputes(import_id)`.
- `redeem_group_invite_link` (`invites.rs`) stores the `SeatGrant` and `claim_proof` into `pending_import_claim` before discarding the token.
- TypeScript types in `frontend/src/types/` and `mobile/types/` mirror the Rust structs (CLAUDE.md).

## 14. Invariants and tests

Each test must fail with its guard removed. DS tests live in a new `pollis-delivery/tests/imports.rs`, run on `pollis_schema::apply::single_db` with `PRAGMA foreign_keys=OFF` (the `tests/teardown.rs` fixture). Flows tests live in a new `src-tauri/tests/flows/imports.rs` (`cargo test --features test-harness --test flows`) using the `fixture` provider, never a real Slack or Discord API.

| # | Invalid state | Enforced by | Test |
|---|---|---|---|
| I1 | Seat on a multi-use, used, revoked, non-expiring or foreign-group link | `import_seat_insert_gate`, `import_seat_link_change` | `imports.rs::seat_rejects_unbounded_used_or_foreign_link` |
| I2 | Two seats for one source identity in one import | `UNIQUE (import_id, source_ref)` | `imports.rs::seat_unique_per_source` |
| I3 | One user holding two seats of one import | `idx_import_seat_one_per_user`; redeem maps the violation to `Rejected` | `imports.rs::second_seat_redeem_rejected` |
| I4 | A seat that grants admin | no role column; redeem uses `add_member_rows` | `flows::imports::seat_lands_as_member` |
| I5 | A seat redeemed twice, moved, or redeemed after revocation | link `CHECK (uses <= max_uses)` (000014:58) + `import_seat_redeem_once` | `flows::imports::seat_redeems_once_under_race`, `imports.rs::revoked_seat_cannot_be_redeemed` |
| I6 | An existing member who redeems a seat but can never claim | redeem seat branch before the `is_member` short-circuit | `flows::imports::existing_member_redeems_and_claims` |
| I7 | A claim by anyone but the seat's current-member redeemer, or on a staging/personal import or revoked seat | `import_claim_needs_seat` | `imports.rs::admin_cannot_claim_a_seat_it_did_not_redeem`, `member_cannot_claim_other_seat`, `claim_on_staging_import_rejected`, `claim_on_revoked_seat_rejected` |
| I8 | A claim mutated, a withdrawal revived, a seat re-claimed | `import_claim_immutable`, claim PK, I3 | `imports.rs::withdrawal_is_one_way`, `withdrawn_seat_cannot_be_reclaimed` |
| I9 | Attribution rendered without a fully verified claim | client verification chain (§6) | `import::claims::tests::{ds_forged_redeemed_by_renders_unclaimed, claim_without_valid_proof_renders_unclaimed, replayed_claim_on_other_seat_renders_unclaimed, unsigned_seat_renders_unclaimed}` |
| I10 | Redeemed seats or claims lost when the minting admin deletes their account | §5.5 teardown order | `tests/teardown.rs::seat_and_claim_survive_minter_account_delete` |
| I11 | Email-bound failure distinguishable from any other | folded verdict | `tests/invite_links.rs::email_bound_seat_wrong_email_is_plain_rejection` |
| I12 | DS testing emails against unredeemed seats | MAC keyed by the link secret | `import::seats::tests::email_mac_needs_secret` |
| I13 | Import on a deleted group or another user, or born live | `import_archive_insert_gate` | `imports.rs::import_requires_live_target` |
| I14 | Unpublish, manifest swap, or publish without manifest, chunk or wrap | `import_archive_update_gate` | `imports.rs::publish_is_one_way_and_complete` |
| I15 | Chunk added after publish, rewrite skipping or rolling back a version, rewrite onto an unregistered object | `import_chunk_insert_gate`, `import_chunk_rewrite_gate` + CAS | `imports.rs::stale_rewrite_loses_cas`, `chunk_insert_after_publish_rejected` |
| I16 | Rewrite or release by a non-admin, non-claimant, or a claimant who left | DS chokepoint (code) | `imports.rs::rewrite_authz_requires_admin_or_live_member_claim` |
| I17 | Client accepting a version that alters, drops, or un-tombstones a leaf, or carries an unauthorized tombstone | acceptance rule (§8) | `import::rewrite::tests::{rewrite_cannot_alter_others_records, dropped_leaf_is_refused, non_author_redaction_is_ignored}` |
| I18 | Import object of wrong key shape or unpadded size | `import_object` CHECKs | `imports.rs::import_object_shape_and_padding` |
| I19 | Import object writable by a non-registrant, readable by a non-member, deletable while registered | broker gates | `tests/r2_presign_scope.rs::import_family_gates` |
| I20 | Import rows surviving group or account teardown; objects never collectable | teardown lists + `/v1/imports/gc` | `tests/teardown.rs` partition test (fails on any unlisted table), `group_delete_leaves_import_objects_collectable` |
| I21 | Imported content in `message_envelope` or local `message` | separate types; `ImportRecord` has no conversion to `OutgoingMessage`; apply holds no `send_message` handle | `flows::imports::apply_sends_no_envelopes`, `imports_never_enter_message_table` |
| I22 | Non-random `K`; label reuse across keys | `ImportKey` constructor, label constants | `import::keys::tests::{import_key_is_random, labels_are_distinct}` + compile-fail doctest |
| I23 | Provider token, source id, name or email in any DS body or path | `pollis-api` types | `pollis-api` `import_bodies_have_no_source_identity_fields` (serde walk), `endpoints_name_no_provider` |
| I24 | Ingesting a #856 export | no provider for it; parsers refuse the marker | `import::tests::pollis_export_is_rejected` |
| I25 | Key wrap on the wrong target or kind, or a non-CAS update | `import_key_wrap_*` triggers | `imports.rs::key_wrap_matches_scope` |
| I26 | Annotation by a non-member, oversized, or unbounded count | `import_annotation_insert_gate` | `imports.rs::annotation_bounds` |
| I27 | Seat binding changed after redemption | `import_seat_binding_frozen` | `imports.rs::seat_binding_frozen_after_redeem` |
| I28 | Seat link secret in a URL a web server sees | seat links rendered only as `pollis://invite/` + bare token | `import::seats::tests::seat_link_is_custom_scheme_only` |

End-to-end flow test: `flows::imports::full_cycle` (fixture plan, apply, publish, seats, redeem by a new and an existing member, external join, waiting state shown, holder re-wraps, K unwrap, claim, render, author redaction, admin erasure, withdraw, group delete, gc).

## 15. Mobile parity and backward compatibility

- **Ships on mobile** (same PR, verified running on a Release build on the Android emulator and the iOS simulator against the dev DS):
  - member: redeem a seat link, the waiting state, claim, imported history (timeline, threads, files, badges, marker), withdraw, redact own, dispute;
  - admin, short actions: seat list, mint/re-issue, revoke, delete import, per-source erasure, apply release requests.
- **Exempt, stated in the PR:** the long-running admin job (`import_apply`: multi-GB inputs, crawls lasting hours to days; iOS gives no background-execution guarantee, [BackgroundTasks](https://developer.apple.com/documentation/backgroundtasks)). Provider docs list their own extra exemptions.
- **E2E, kebab-case** like `mobile/.maestro/flows/` (`change-email.yaml`, `deep-link.yaml`):
  - Maestro: `import-claim.yaml`, `import-history.yaml`, `import-redact.yaml`, `import-seats.yaml`;
  - Playwright: `e2e/import-claim.spec.ts`, `import-history.spec.ts`, `import-redact.spec.ts`, `import-seats.spec.ts`.
  - The seed import is published by a second client running the `fixture` provider (the `_two-client.md` pattern), never a real provider.
- **Oldest mobile build still in the stores:**
  - Every table and endpoint is new; old builds never call them.
  - The redeem response gains only a `#[serde(default)]` field, and no `pollis-api` type denies unknown fields.
  - Seat links use the path form every shipped `extract()` parses (§5.2).
  - An old build redeems a seat link, joins by external commit, and gets the seat stamped server-side; email binding is checked server-side too. It cannot claim or render imported history, so that member shows as "seat redeemed, unclaimed", a defined state.
  - After updating, re-opening the same link returns the `SeatGrant` again (§5.3 step 5), so the claim is recoverable without the admin.
  - Builds predating #99 never re-wrap `Kpin`. Their joiners wait for an updated holder (§4.2).
- `pollis-core` changes reach phones only in a new store build. The PR says so. Release order: DS first (tables, endpoints, redeem branch, broker family), then the desktop release and store submissions. No client mints seats against a DS without the redeem branch, because the endpoint does not exist there.
- Post-merge release checklist (`docs/deployments.md`): DS (confirm `/version` SHA), desktop, mobile store builds and sideload APK, CLI (`pollis-tui` links `pollis-core`), with no change to the verifier or transparency log.

## 16. Docs and principle changes (same PR)

- **CLAUDE.md**, "Messages must work":
  - Loss (1) becomes: "messages sent before you joined the MLS tree, except content a member deliberately shares under a group-held key (pins #99, imported history #1179/#1180), which every current member reads regardless of join time".
  - Loss (2)'s parenthesis becomes: "(no key backup; the vault and imported archives are the only server-held encrypted history, keyed by member-held keys; never extend that to live messages or add Megolm-style backup unless explicitly asked)".
  - Note that pins already made the old text of (1) inaccurate. The count of losses stays three: imports add readable content, they do not add a loss.
- **`docs/backend-core-invariants.md`**: caveats (a)/(b) (lines 32-36) get the same wording; I3's "caveat (a) preserved" (line 147) stays true for envelopes and says so. A new section, "Disclosed pre-join content: pins and imports", covers the Kpin loss mode (§4.3), the waiting state (§4.2) and the admin-trust ceiling.
- **`docs/security-whitepaper.md`**:
  - §1.2: what the server sees about imports (§1).
  - §6.4: external joiners adopt `Kpin` lazily, and imports inherit that.
  - §6.8 (line 395): pins and imports are pre-join content readable by joiners; imports are server-held encrypted history under member-held keys.
  - §9: a new §9.5 for the non-convergent `import/` family.
  - §12: `K` and its wrap in the key summary.
  - §13: item 7 (line 709) corrected, plus a new item for the admin-trust ceiling (fabrication, sockpuppet seats), the DS roster trust in seat and tombstone signatures, and rollback detection limits.
  - Comparison lines 742-748: "has no backup of any kind" becomes accurate wording.
  - The Kpin-as-HKDF-input hygiene line.
- **`docs/metadata-retention-policy.md`** and **`website/retention.html`**: the import data class (lifetime = until the import, its group or its owner is deleted; staging TTL 30 days), the `minted_by`/`revoked_by` verification pointers, and that Pollis can delete a whole import but cannot locate a person inside one.
- **`pollis-core/src/commands/export.rs`**: the "No re-import" paragraph (lines 22-24) and the `there_is_no_import_command` test comment (lines 1024-1025). The export format still has no reader and the test still holds, because it scans only `export.rs`. Add that source-platform import exists (#1179), refuses this format (I24), and that a scripted conversion is a restore path the owner has knowingly accepted.
- **`.codesight/wiki/`**: `database.md` (seven tables, triggers, teardown), `mls.md` (Kpin section: also unlocks import keys; the external-join wait), `commands.md` (§13), `testing.md` (new suites), `index.md` (link to the import article).
- **`docs/deployments.md`**: the DS-before-clients ordering for this feature.

## 17. Decisions made in this spec

Reconciled choices are taken from `docs/discord-import-design.md` "Reconciliation with slack-import-design.md" unchanged, except for these, which are new:

1. `manifest_ver` is dropped; the manifest is write-once and pinned by its content-addressed key (§3.3).
2. `source_account_mac` is not in the migration (§5.1).
3. Seat bindings carry `seat_sig` by the minting admin, because lazy mint and re-issue make a manifest-carried `proof_hash` impossible (§5.1).
4. Seat links are `pollis://invite/<token>` plus the bare token; no https and no fragment form, because of shipped `extract()` and web-log exposure (§5.2).
5. Re-opening your own redeemed seat link returns the `SeatGrant` (claim recovery and the old-build upgrade path, §5.3).
6. The redeemer's account deletion deletes their seat and claim (§5.5).
7. No re-claim after withdrawal in v1 (§6).
8. Claim waits for `K` (§4.2); no `K` escrow in the admin journal (§4.3); no `Kpin` rotation in v1 (§4.3).
9. Chunk padding and size shape is a DB CHECK, and chunk R2 keys are SHA-256 of the object bytes (§3.2, §9).
10. `import_chunk` has no user column; `import_annotation` kinds are `dispute`, `release` and `corroborate`, with `chunk_id` moved inside the ciphertext.
11. File release after a claimant redaction is an admin-applied `release` annotation (§8).
12. Object collection mirrors `/v1/emoji/gc` (`/v1/imports/gc`); staging imports expire after 30 days in the existing DS sweep.
13. `import_archive.scope` (`group` | `personal`) carries Slack's personal archives inside the same tables.

## 18. Open decisions that remain shared

1. Storage quota per import and per plan tier (blocks GA for large sources; an over-quota import skips files, never messages).
2. Chunked upload for files above 104 MiB.
3. `Kpin` rotation on member removal, as a #99 follow-up ticket (§4.3).
4. Admin override for one person holding two source identities in one import.
5. Admin re-seat after withdrawal (delete and re-create the seat).
