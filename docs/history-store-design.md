# Members-held encrypted history store (the "archive")

Status: design proposal. Alternative to the history-keeper epic #1181 (children #1182 to #1187), and the substrate the revised #1179/#1180 history import needs. Nothing here is built.

## 1. Summary and verdict

**What it is.** A server-held store of message history, sealed per message by the **sender**, at send time, in the same DS request as the live envelope. It is keyed from the MLS key schedule so that **who can read an entry is exactly who was in the MLS tree when it was sent**. Two opt-in widenings exist on top:

- **Member history** (per group, admin setting): a member's *own new device* gets the keys for every epoch that member's account was in. Serves #1185.
- **Joiner sharing** (per channel, admin setting, persistent ambient marker): a *new member* gets the keys for that channel's epochs since sharing was turned on. Serves #1186.

**Core construction (one paragraph).** At every epoch `e` a member device is in, it derives `H_e = MLS-Exporter("pollis-archive-epoch-v1", gen || epoch, 32)` and caches it locally, but only while some archive mode is on in that group. Per channel, `K(c,e) = HKDF(H_e, "pollis-archive-chan-v1" || c)`. A sender seals its message as an archive entry under `K(c,e)`, signed by its certified ML-DSA-44 device key **inside** the encryption (so the at-rest row names no sender; note that Pollis's sealed sender is at-rest only, and the live DS still sees the sending device on every signed request, `docs/security-whitepaper.md` §1.2), and posts it with the envelope. Current members need no key distribution at all: the MLS exporter is the rotation schedule. Joiners in a sharing channel receive `K(c,e)` rows encrypted under a group share master `S_g`, which is exporter-wrapped per epoch exactly like `Kpin` (#99) and rotated on every removal and self-update. A user's own new device receives its `H_e` ring from the sibling that approves its enrollment, which is online by definition (but may run a build that predates the archive, in which case no ring is sent).

**Author's verdict: build the store, and re-scope #1181.** (The reviewer does not fully agree; see "Reviewer verdict" at the end.) The store gives #1185 and #1186 with no customer-run box, no unattended unlock path, sender-authenticated history (the keeper's replays, as #1181 specifies them, are re-sent application messages that whoever runs the keeper could forge; a keeper that forwarded the original MLS signature would not have this problem, Section 7.2), DMs (the keeper tickets exclude them by policy), and keeps "prospective by construction" as a physical property for epochs before any archive mode was enabled (exporters for those epochs no longer exist anywhere). For joiner sharing enabled later than member history it is not physical: the keys exist on member devices and the guarantee rests on honest clients and an honest DS (Section 4.3.5). Its price is real and must be accepted explicitly: **Pollis's servers become a long-term store of history ciphertext and of a durable per-channel activity timeline**, which they are not today for message ciphertext (envelopes are collected once every live member device has reported a watermark past them). Other metadata is already durable today: the commit log stores `sender_id` and timing, `message_reaction` stores reactor, emoji and time in plaintext, and `pinned_message` stores `pinned_by`. It does not fix "lost every device" unless the owner separately opts into a Secret-Key-sealed ring, which is Megolm-style backup and is left as an open decision, recommended **no** for v1.

Keep #1182 (keeper identity, bots as members) as is; it is useful without history. Author's proposal: close or rewrite #1183 to #1187, since their history scope is subsumed. Reviewer: premature; hold them open until the owner decides the metadata regression (Section 4.11) and the fixes in Review notes land in this design. An always-on member device remains a useful *optional* add-on (Section 7.4), but as a key-holder, not a plaintext cache.

**Biggest risks**, detailed in Section 8: durable metadata at rest; mixed-fleet holes (old clients do not write entries, unlike a keeper which archives every sender); deletion against future joiners rests on DS honesty (same trust as today's redaction delivery); external-join joiners wait for one key-holder to come online; complexity concentrated in key-state code that already exists in shape (`pin_keystate`).

**Overturned from the brief and the keeper tickets** (Section 8.3): rotation and distribution are mostly unnecessary (exporter-derived keys); chunk rewrite is unnecessary (per-entry rows; any packing is server-side over opaque entries); handoff liveness is mostly free (committer and enrollment approver are online by definition); the brief's exposure list is incomplete; #1187's "an offline keeper permanently loses windows" is wrong for short outages (shipped invariant I3) but partly right for long ones (the 512-epoch commit-log cap and the 12-month watermark); "pre-join history is never visible" is already false in the shipped app because of pins.

## 2. Relationship to #1181 and its children

| Ticket | What the store does to it |
|---|---|
| #1181 epic | Replaces its history mechanism. Its non-negotiables are kept: per-channel never group-wide for joiners; persistent ambient marker; turning it off is meaningful and says what survives. "Replay is ordinary application messages" is dropped: history is not replayed at all, it is read from the archive. |
| #1182 keeper identity | Unaffected. Bots and an optional always-on member (Section 7.4) still need it. |
| #1183 runtime, pairing, retention | Unattended-unlock surface not needed for history. The retention-bound decision moves here (Section 4.10). |
| #1184 interaction spike | Its questions remain and are answered in Section 4.7: scroll-triggered fetch, one-time banner, explicit "unavailable" states, paging bounds. "Two hosts answer differently" disappears: there is one archive. |
| #1185 self-replay | Replaced by member history. Its entitlement rule ("member at time of send", wrong version unrepresentable) is enforced by the key schedule, not by a check (Section 6, I-A1). |
| #1186 joiner replay | Replaced by joiner sharing. "Prospective by construction" kept physically (Section 4.3.5). |
| #1187 multi-host | Unnecessary for the store. Its premise is overstated, not simply wrong (Section 8.3, item 6). |
| #1179/#1180 import | The import archive becomes archive entries written by the importing admin under an import key carried in the same share ring (Section 4.12). One reader, one key distribution, one deletion model. |
| #846 disappearing messages | Mutually exclusive with archiving per channel, by type (Section 4.9). |
| #150 lookback | Unchanged locally; its meaning changes and the UI must say so (Section 4.9). |
| #720 retention | Unchanged for envelopes. The archive can heal loss (3) for groups with an archive mode on (Section 4.10). |
| #1312 loss placeholders | The archive can fill `stranded` / `failed` / `lineage` placeholders for entitled readers. |
| #99 pins | Not replaced, but two findings (Section 8.4): pins already expose pre-join content to joiners, and pin snapshots are unsigned, so any member can forge a pin attributed to another sender. Pins should reference signed archive entries once the archive exists. |
| #856 export | Export can include archive-hydrated history. |

Searched the tracker for history sharing, backup, device transfer, history_visibility, disappearing, export: only #1181 family, #846, #150, #856, #1312 are related. No duplicate.

## 3. Prior art and lessons

| System | Mechanism | What went wrong or what it costs | Lesson for Pollis |
|---|---|---|---|
| Matrix MSC3061 (superseded) | Keys flagged `shared_history` when room `history_visibility` was `shared`; forwarded to invitees. Unknown visibility treated as `joined`, failing closed. [MSC3061] | matrix-js-sdk `sendSharedHistoryKeys` sent keys to unverified devices and was interceptable by a malicious homeserver, CVE-2024-47080, fixed in 34.8.0. [CVE-2024-47080], [Matrix 2024-10] | Never distribute history keys to a device set the server controls. Pollis distributes only through the MLS exporter, i.e. to leaves that passed §5.3 certification. Fail closed on unknown mode. |
| Matrix MSC4268 (merged 2026-04-08, spec 2026-06-24) | Inviter builds a "room key bundle" of `shared_history` sessions, encrypts it like an attachment, uploads to the media repo, sends an Olm to-device `m.room_key_bundle` to the invitee's cross-signed devices; explicit `withheld` entries (`m.history_not_shared`); invitee imports only when accepting an invite from that same user within a window (24 h in Element). [MSC4268 PR], [MSC4268 text] | No proof of authenticity: "we have to take the word of whoever sent us this key bundle"; recommends labelling "Alice shared this message". Rejected a shared key store over "who populates it, how to rotate on leave". No revocation. [MSC4268 text], [RoomKeyBundle docs] | Sender-signed entries remove the "trust the forwarder" problem. Rotation on leave is solvable with an exporter-wrapped master (Pollis already ships the shape in `pin_keystate`). Show explicit "not shared" states, never silent emptiness. Visual indicator when messages may be shared. |
| Matrix key backup v1 | Session keys uploaded under a backup public key; recovery key or passphrase restores. | v1 is unauthenticated: anyone can upload keys into a backup; MSC4048 adds a MAC so injected keys are rejected; the Rust SDK says v1 "is not recommended". [MSC4048], [matrix-sdk backups] | Anything in the store must be authenticated by a key the server and outsiders lack; key rows here are AEAD under a member-only key and entries are signed. |
| Matrix 2022 (Albrecht, Celi, Dowling, Jones) | Practical attacks against confidentiality and authentication assuming a malicious homeserver; root causes "insecure by design, protocol confusion, lack of domain separation, implementation bugs". CVE-2022-39251: Megolm-encrypted to-device messages accepted where Olm was required, letting a homeserver inject a malicious key backup secret during self-verification. [Albrecht et al.], [Black Hat EU-22], [CVE-2022-39251], [CVE-2022-39248] | Server-controlled membership and devices plus unauthenticated key channels. | Every label in this design is domain-separated; key material is accepted only from one channel type each; the server never decides who is a device of whom (§5.3 already). |
| Signal device transfer | On linking, the primary builds an encrypted compressed archive, sends it through Signal's servers, and passes the archive key during linking; media links kept 45 days. [CyberInsider 2025] | Requires the old device present. | The same shape as Pollis enrollment: the approving sibling is online, so a sealed ring transfer at approval costs no liveness. |
| Signal Secure Backups (2025) | Opt-in daily archive replacing the prior one, under a 64-character on-device recovery key never sent to Signal; excludes view-once and messages disappearing within 24 h; text unlimited, media 45 days free. Hosted backups add a daily-rotating key in a TEE for forward secrecy; the recovery key alone opens all local backups. [Signal blog], [Signal support], [BleepingComputer] | A static recovery key has no forward secrecy; Signal added a TEE-held rotating key to compensate. | Disappearing content must never enter the store. A single long-lived secret that opens all history is the thing to avoid by default; if offered, bound it. |
| Signal SVR / SVR3 | PIN-protected secrets in enclaves across heterogeneous providers, rate-limited guesses. [SVR3 OSDI24], [Green 2020] | Hardware trust; operational cost. | Pollis's Secret Key is 150-bit random, so no SVR is needed for brute-force resistance; the question is only whether to let it open history. |
| WhatsApp multi-device | Per-device identity keys; app-state copy on the server "end-to-end encrypted with constantly changing keys known only to that person's devices"; concern about a server adding devices. [WhatsApp multi-device] | | Rotating keys known only to the account's devices is the right model for member history. |
| WhatsApp E2EE backups | 64-digit key, or password with the key in an HSM "Backup Key Vault" enforcing attempt limits. [WhatsApp backups], [WeLiveSecurity] | HSM trust assumption. | Same as SVR. |
| Wire (MLS) | History is device-local; a new device or reinstall shows only new messages unless the user made a backup. No MLS history sharing documented. [Wire support] | Users lose history. | Confirms the status quo is the industry default for MLS; no standard to follow. |
| IETF MLS | RFC 9420: joiners cannot read messages sent before they were added; members delete consumed secrets for FS. [RFC 9420]. `draft-ietf-mls-extensions` defines a Safe Application Interface where exported secrets are bound to a component identifier, and an `application_data` extension; predecessor `draft-barnes-mls-appsync`. [mls-extensions], [appsync] No WG history-sharing draft was found. | | Derive archive keys through a domain-separated exporter label (or the safe-exporter component API once openmls ships it). |
| Apple Messages in iCloud | E2EE under ADP, or when iCloud Backup is off; with standard protection and Backup on, the Messages key is inside the backup Apple can open; enabling ADP rotates keys and "data written afterward can't be read with the old key". [Apple security guide], [Apple iCloud overview] | Default is not E2EE for most users. | Turning a mode on must rotate forward, never re-seal the past. |
| Telegram secret chats | Device-specific, no history sync, no backups, disappear on logout. [Telegram FAQ] | Users choose cloud chats instead. | The usability cost of no history pushes users to non-E2EE modes; a bounded opt-in is better than nothing. |
| Discord | "Read Message History" is a per-channel permission; users report there is no way to hide pre-join history from new members, and pins pre-join would vanish if hidden. [Discord forum] | | Per-channel admin setting with visible state is what users already understand. |

## 4. Design

### 4.1 Terms

- `G` an MLS group (one per Pollis group, all channels share it, §6.2), `c` a channel id, `gen` the lineage generation, `e` the epoch.
- Entry: one archived message revision. Ring: a set of `H_e` or `K(c,e)`.
- Holder: a device that has the key needed for an operation.

### 4.2 Modes, as one type

```rust
// Carried MLS-authenticated (Section 4.3.6), mirrored to the DS for gating.
enum ChannelHistory {
    Default,                       // today: nothing archived
    Archived { share_with_joiners: bool }, // needs group MemberHistory on
    Ephemeral { ttl: Duration },   // #846; never archived
}
enum GroupHistory { Off, MemberHistory }
```

`Archived` and `Ephemeral` are variants of one enum, so "archived and disappearing" cannot be constructed. `share_with_joiners` lives inside `Archived`, so "shared but not archived" cannot be constructed. The send path takes an `ArchiveTicket` that only `ChannelHistory::Archived` can mint.

DMs: `Archived { share_with_joiners: false }` only (no joiners in a 1:1 DM), settable by either party with a visible system row, the Signal disappearing-timer pattern. Open decision D4.

### 4.3 Key hierarchy

#### 4.3.1 Per-epoch history key

```
H_e = MLS-Exporter(label = "pollis-archive-epoch-v1",
                   context = u32_be(gen) || u64_be(epoch), length = 32)
```

Computed in the same epoch-change hook as `voice_e2ee::on_mls_epoch_changed` and `maybe_rewrap_pin_key` (committer reconcile, receiver replay, self-update, suite migration, generation hop, Welcome join, external join), **before** the next merge destroys the exporter (`max_past_epochs = 0`). Cached in local `archive_epoch_key` (SQLCipher, same trust domain as `mls_kv`) only while `GroupHistory::MemberHistory` is on for `G`.

Properties that fall out, with no protocol code:

- **Entitlement is exact.** A device holds `H_e` iff its leaf was in the tree at `e`. A message sent at `e` is readable by exactly the set the MLS ciphertext was readable by. #1185's rule ("member at time of send") is the key schedule.
- **No rotation protocol, no fork, no mint race** for current members. Every member at `e` computes the same `H_e`.
- **Removal is rotation.** A removed leaf never computes `H_{e+1}`.
- **PCS is MLS PCS, for the exporter path only.** A thief of leaf state stops getting new `H_e` from the key schedule once the victim's own self-update heals the leaf (§6.7). The delta transfer in Section 4.3.7, as drafted, breaks this: it seals new `H_e` under a key derived from the never-rotating account seed, so an account-key thief keeps receiving them. See Review notes.
- **No forward secrecy inside retention.** Every device in an archived group retains `H_e` for the whole retention window (up to 3 years), for its own future devices. That is a deliberate retention of per-epoch secrets: `max_past_epochs = 0` still protects live MLS traffic, but not the archive.
- **Catch-up does not lose keys, within the commit-log cap.** A device offline across 300 commits replays them in order and computes every `H_e` on the way. Commit retention (I4) is not subject to #720, but it is subject to the Tier-2 hard cap `PRUNE_MAX_BEHIND_HEAD = 512` (`pollis-delivery/src/commit.rs:600`): a device more than 512 epochs behind head external-joins at head and never computes `H_e` for the gap.

#### 4.3.2 Per-channel entry key

```
K(c,e) = HKDF-SHA256(salt = u32_be(gen) || u64_be(epoch), ikm = H_e,
                     info = "pollis-archive-chan-v1" || u8(len c) || c)
kc(c,e) = HMAC-SHA256(K(c,e), "pollis-archive-kc-v1")[0..16]
```

`K(c,e)` reveals nothing about `H_e` or about other channels, so a joiner given `#general` keys learns nothing about `#admins`. Note: channels are not cryptographically isolated from current members today (one MLS group per Pollis group); per-channel derivation matters only for what joiners receive.

#### 4.3.3 Group share master (joiner sharing only)

One random 32-byte `S_g^v` per group, version `v`. Same lifecycle as `Kpin` (#99), with rotation added:

- **Wrap.** `pollis_api` row `group_share_keystate`: `AEAD(KEK_e, v || S_g^v || check)`, `KEK_e = MLS-Exporter("pollis-archive-share-kek-v1", gen||epoch, 32)`. One mutable row per group, lexicographic `(gen, epoch)` CAS, I9 head ceiling (`refuse_above_head`). Re-wrapped from the local cache after every epoch advance by any holder (one local SELECT in steady state, as `maybe_rewrap_pin_key`). `check = HMAC(S_g^v, "check")` lets a reader detect a random-garbage wrap only. It does not detect a malicious member who holds `KEK_e` and wraps a well-formed fake `S'` with a valid `check`; under the monotone CAS the first writer at each new epoch wins, so a later joiner can adopt the fork and then re-wrap it from its own cache. `pin_keystate` has the same property today (`pollis-delivery/src/pins.rs:217-230`). Canonicality needs a binding the forger cannot produce, for example an `S_g` commitment carried in the authenticated mode setting (Section 4.3.6, D2).
- **Mint.** Insert-only when the first channel in `G` turns sharing on. Lost race adopts the winner.
- **Rotate** on every commit that removes a leaf (remove, leave, revoke, eviction) and on every self-update commit, so a stolen `S_g` heals on the same schedule as the leaf. The committer rotates; if the committer runs an old client, the first updated member that processes the commit rotates (the keystate records `rotated_at (gen, epoch)`; rotate iff a removal or self-update commit is newer). Rotation writes a **link** `L_v = AEAD(S_g^{v+1}, S_g^v)` to `group_share_key_link`, then wraps `v+1`. Links are keyed `(group, v, link_id)` and multiple candidates are allowed, so the reader tries each, validated by `check`. As drafted this does not stop pre-emption: `share_link_cap` allows 8 candidates per version, and nothing stops a malicious member from filling all 8, including for versions not yet minted, before the honest rotator writes. The link should be written in the same transaction as the keystate CAS that creates `v+1` (one link per version, by the rotator), or capped per writer device.
- **Read.** A joiner unwraps the current `S_g^v` and walks links down to the oldest version it needs.

#### 4.3.4 Share key rows

`group_share_key(channel_id, gen, epoch, kc, key_version, nonce, wrapped)`, value `AEAD(S_g^{key_version}, K(c,e))`. Written lazily by **the sender of the first entry in channel `c` at epoch `e`**, in the same send request, `INSERT OR IGNORE` on `(channel, gen, epoch, kc)`. The sender is online by definition, so no extra liveness. Rows are needed only for epochs that have entries.

A DB trigger refuses a row unless `(gen, epoch)` lies inside an open or closed range of `channel_history_share` for that channel, and unless an `archive_entry` with the same `(channel, gen, epoch, kc)` exists or is inserted earlier in the same transaction. So an honest DS cannot store a share-ring key for a pre-enable epoch, and a garbage row costs its writer a visible message. This is defence in depth against DS bugs, **not a cryptographic guarantee**: the DS and Turso are untrusted (CLAUDE.md), the operator can drop the trigger or write rows directly, and the ranges are themselves written by the DS. Against a malicious member the real control is that honest clients only wrap `K(c,e)` inside a range they verified from an authenticated commit (Section 4.3.6).

**Pre-emption, as drafted.** `INSERT OR IGNORE` on `(channel, gen, epoch, kc)` lets the first writer win. A malicious member holds `K(c,e)`, so it can compute the correct `kc`, send one real message at `e` (which satisfies the trigger), and attach a garbage `wrapped`. The honest row is then ignored and joiners cannot open any entry of `c` at `e`. Fix: allow several candidates per key (add a candidate id to the PK). Readers can validate a candidate because `kc = HMAC(K, ...)` is checkable after unwrap.

#### 4.3.5 Why consent stays prospective by construction

- Before any archive mode is on in `G`, no device caches `H_e`; the exporters are destroyed at merge. Keys for those epochs **do not exist anywhere**, which is the keeper's property (#1181 "What falls out for free"), kept physically.
- After `MemberHistory` is on but before a channel shares, `H_e` exists on member devices (for their own new devices). It reaches a joiner only as a `group_share_key` row, which the trigger refuses outside the share range, and which honest clients never compute outside their own MLS-authenticated view of the range. A malicious member can hand keys out of band; it could equally forward plaintext. The keeper has the same residual (an admin's own device holds pre-keeper plaintext). One difference: a leaked 32-byte `K(c,e)` opens server-held, sender-signed entries, so the leak is compact and carries transferable proof of authorship, which forwarded plaintext does not. Because `Archived` requires `MemberHistory`, every sharing channel had `H_e` cached on every member device for the epochs between enabling member history and enabling sharing. For joiner sharing, "prospective" is therefore client discipline plus an honest DS, not physics.

#### 4.3.6 Where the modes are authenticated

A mode change rides an admin-issued commit (typically a self-update) whose MLS `authenticated_data` carries `HistorySetting { channel, mode, v }`. All members process it as part of the commit, so the effective epoch is exact (`e+1` of that commit) and the server cannot flip it. The `/v1/commits` request mirrors the setting so the DS writes the `channel_history_share` range in the same transaction (needed for the trigger and for least-privilege reads). Members compute ranges from commits they processed and ignore DS ranges not backed by one. Joiners see ranges from the DS, and those ranges are unverifiable to them. The draft called this harmless because "only members write key rows". That reasoning is wrong: a joiner *becomes* a member and a writer. If a malicious DS tells a joiner that `#admins` shares (when only `#general` does), the joiner's client will wrap `K(#admins, e)` under `S_g` for every epoch at which it is the first sender. Every later joiner of `#general` then reads that member's `#admins` messages. So a device must not write share rows on the strength of a range it did not verify. That requires a GroupContext extension (`application_data`, mls-extensions) carrying the current mode, which every joiner verifies through the Welcome or the external-join GroupInfo. **D2 is therefore a v1 requirement, not optional.** Until it exists, a device that did not process the enabling commit must not write share rows. Openmls `set_aad` support on commits must be confirmed.

#### 4.3.7 Member-history ring transfer to the user's own new device

- **Snapshot at approval** (§5.1 and §5.1a QR link). The approving sibling already computes the enrollment ECDH. It derives `RK = HKDF(ECDH, "pollis-history-ring-v1" || requester_pub || approver_pub)` and uploads `AEAD(RK, ring)` to `history_ring_transfer` addressed to the new `device_id`, where `ring` is `{(G, gen, e, H_e)}` for every group in member-history mode. The new device fetches it while its ephemeral key is still in memory (it already must, to unwrap the account key). Bound server-side to the same account.
- **Delta after join.** The new device external-joins every group (§6.4) minutes later; epochs in between are missing. Any sibling that processes a commit adding a leaf of its **own** user uploads the delta `[snapshot, join)`, sealed under `HKDF(account_seed, "pollis-history-ring-delta-v1" || target_device_id)`. Event-driven: it happens when the sibling processes that commit; duplicates are harmless. **Reviewer: this sealing is unsound as drafted.** The account seed never rotates. It sits on every enrolled device and, wrapped under the Secret Key, in `account_recovery` (`docs/security-whitepaper.md` §2.1). `target_device_id` is public. So (a) anyone with the account seed and read access to Turso reads every delta, including deltas carrying `H_e` minted *after* the victim's leaf healed, which defeats the PCS claim in 4.3.1; and (b) the Secret Key plus a dump opens deltas in flight for up to 30 days, a small D1 that the doc says it is not taking. Seal the delta to a per-device KEM public key the new device publishes at enrollment, or bound the delta to the snapshot channel. Devices have no KEM key today (§2.2 lists signing keys only), so this is new key material.
- Transfer rows are deleted on pickup and carry a TTL (30 days).
- **Secret Key recovery path (§5.2)**: no sibling exists. The new device starts empty. That is loss (2) retained, unless the owner opts into D1.

### 4.4 Entry format

Sign-then-encrypt so the at-rest row carries no sender (consistent with Pollis's at-rest sealed sender, whitepaper §1.2; the live DS still sees the sending device on the request), split in two ciphertexts so receivers can verify equivocation without downloading anything.

```
ctx  = "pollis-archive-entry-v1" || G || c || u32 gen || u64 epoch || message_id || u32 rev
Body = padded framing { kind: text | tombstone | import, content, thread_id, reply_to,
                        sent_at, attachments, rev0_c1_hash (for rev > 0),
                        chain_prev, import_meta? }
C1   = AES-256-GCM-SIV(K(c,e), nonce = SHA-256(ctx)[0..12], aad = ctx, Body)   // deterministic, RFC 8452
sig  = ML-DSA-44(device_sk, "pollis-archive-sig-v1" || SHA-256(C1) || ctx || user_id || device_id)
C2   = AES-256-GCM(HKDF(K(c,e), "pollis-archive-sig-key-v1"), random nonce, user_id || device_id || sig)
blob = len(C1) || C1 || C2
```

- **Nonces.** `C1` uses AES-256-GCM-SIV with a nonce derived from `ctx`. That is safe: `ctx` is unique per `(message_id, rev)`, and GCM-SIV is nonce-misuse resistant, so a repeated `ctx` leaks only equality. `C2` uses random 96-bit nonces under a per-`(c,e)` key, which is well within limits. `C2` should take `aad = ctx || SHA-256(C1)` so it cannot be spliced onto another entry. The signature already covers `SHA-256(C1) || ctx`, so a splice fails verification today, but the AAD makes the failure a decryption error rather than a verify error.
- **Exporter label separation.** `pollis-archive-epoch-v1` and `pollis-archive-share-kek-v1` are distinct from the shipped `pollis-pin-kek` (`pinned_messages.rs:81`) and the voice labels. The `gen || epoch` context is redundant (the exporter secret is already epoch-unique) but harmless. Groups still on the classic suite have a classical, not PQ, exporter.
- **Signer** is the device ML-DSA-44 key, certified by the account key (§2.2, §5.3), not the account key: per-device attribution, revocation semantics, no account key on the hot path. Readers verify against the device cert from `/v1/read/roster-identities`, with the leaf-verdict rules of §5.3.
- **Per-sender chain**: `chain_i = SHA-256(chain_{i-1} || message_id || rev0_c1_hash)` per `(sender device, channel)`, over revision 0 only so edits and tombstones do not break it. Detects interior omission (Section 8).
- **Live binding (anti-equivocation)**: the live MLS frame gets a new trailer after the real plaintext (same back-compat argument as the `0xF7` thread trailer in `framing.rs`: old clients never look past the length prefix), `0xF9 || SHA-256(C1) || chain_prev`. A receiver rebuilds `Body` from what it just decrypted plus `chain_prev`, recomputes `C1` (deterministic), and checks both the trailer and the `archive_c1_sha256` the DS returns with the envelope. Mismatch means the sender archived something other than what it said, or the DS substituted the entry; either is flagged on the Security page. Cost: 33 + 32 bytes per message, zero extra download. Implementer gaps: (1) `Body` must be a canonical encoding that the receiver can rebuild entirely from the live frame. Any sender-local field (clock-based `sent_at`, attachment JSON key order) breaks the recompute and raises false equivocation alarms. (2) On a thread reply the `0xF7` thread trailer must stay at offset `N` and `0xF9` must follow it. Putting `0xF9` at `N` makes thread-aware old clients (`framing.rs:417`) show the reply in the main channel. (3) Attachment envelopes are deliberately unpadded (`framing.rs:162` doc) and carry no trailer, so they need a different binding. (4) Joiners, the main consumers of shared history, never see the live frame and cannot run this check. They rely on members having flagged it.
- **Size**: ~256 B floor padded content + 2420 B signature + 2 tags + header, about 2.8 KB per text message. The live MLS message already carries a 2420 B leaf signature, so the archive roughly doubles per-message storage during the envelope's lifetime and keeps one copy after.
- Attachments: `Body.attachments` carries the same JSON the live message carries (hash, key, size); the blob stays in R2.

### 4.5 Write path

All writes ride existing message endpoints, atomically, so the archive cannot diverge from delivery:

| Operation | Endpoint | Archive effect |
|---|---|---|
| send | `/v1/messages/send` gains `archive: Option<ArchiveEntryIn>` and optional `share_key: Option<ShareKeyIn>` | Inserts `archive_entry` (id = envelope id), optional `group_share_key`, `archive_attachment_ref`, all in the envelope's transaction. DB trigger: an entry needs its envelope. |
| edit | `/v1/messages/edit` | Replaces the entry with `rev+1` if the request carries the #1086 delete token whose hash matches. |
| delete (self) | `/v1/messages/delete` with token | Replaces the entry with a signed tombstone (`kind = tombstone`, `rev+1`, keeps `rev0_c1_hash`), deletes the old ciphertext, releases attachment refs. |
| delete (admin) | `/v1/messages/delete`, admin branch | DS-authorized by role; replaces the blob with an unsigned DS tombstone marker (no key needed). Readers render "removed by an admin". |
| reseal | `messages/seal.rs::post_resealing` on 409 `epoch_behind` (#1041) | Entry is resealed at the new epoch together with the envelope; the I8 gate admits neither at a closed epoch. |

The delete-token capability is reused unchanged: same `HMAC(HKDF(account key, "pollis/envelope-delete/v1"), message_id)`, same `delete_token_hash` stored on the entry. The DS authorizes nobody, it compares a hash.

Receipts, typing, voice: never archived. Reactions: already stored server-side in plaintext (`message_reaction(id, message_id, user_id, emoji, created_at)`, `pollis-schema/migrations/000000_baseline.sql:98-105`, written by `pollis-delivery/src/messages.rs:1449`, kept for the life of the account per `docs/metadata-retention-policy.md:124`), so a joiner sees reactions regardless; nothing to archive (Section 8.4). Pins: separate store (#99), see Section 8.4. Threads: `thread_id` inside `Body`.

### 4.6 Read paths

| Reader | Keys from | Liveness needed |
|---|---|---|
| Current member, same device | own `archive_epoch_key` cache | none |
| Current member, recovering `stranded` / `failed` / `lineage` placeholders (#1312), or a device back after more than 12 months (#720 loss 3) | own cache (it replays commits and derives each `H_e`) | none |
| Own new device, approval or QR path | ring snapshot + delta | approver (online by definition), any sibling for the delta |
| Own new device, Secret Key path | none | loss (2) stands; D1 |
| Joiner by Welcome (invite, reservation) | `group_share_keystate` wrapped by the committer right after merge; adopted before the post-join self-update (the `try_adopt_pin_key` pattern) | committer (online by definition) |
| Joiner by external commit (invite link, `redeem_group_invite_link` → `external_join_group`) | wrap at the joiner's new epoch, written by the first holder that processes the join | one holder online after the join; event-driven (`membership_changed`, `new_message` pings, the joiner's own first send) |

DS read endpoint `POST /v1/read/archive/entries { conversation_id, before_seq, limit }`, membership-gated; defence in depth: the DS serves an entry only if its `(gen, epoch)` is in a share range for the channel or the reader's account was in the roster at that epoch (derivable from the commit log). Crypto remains the authority.

Hydration writes `message` rows with a new `origin = 'archive'` column and `archive_verified` flag; dedup by `message_id`, highest `rev` wins, a tombstone wins over content. Ordering by the envelope's DS `seq` stored on the entry.

### 4.7 Interaction (answers #1184)

- **Mechanism**: scroll-triggered fetch when scroll-back reaches the end of local history; a one-time banner on join in a sharing channel ("History since <date> is shared in this channel"). Lazy recovery on the next attempt, no polling.
- **Page**: 200 entries or 512 KiB, newest first.
- **States, never a silent empty scroll-back**: loading; "history before <date> is not shared in this channel" (explicit, like MSC4268 `withheld`); "history keys will arrive when another member is online" (external-join wait); "<n> messages could not be verified" (bad signature, unknown device); "history before <date> was removed by this group's retention".
- **Distinguishable**: archive-hydrated messages carry a subtle marker only when unverified or imported; verified archive messages render as ordinary history.

### 4.8 Deletion, edits and who may rewrite

There is no chunk to rewrite. Each entry is its own row and its own ciphertext; only the author (delete token) or an admin (role) can replace it, and replacing does not need the key for admin removal. Optional server-side packing of cold entries into R2 objects (Phase 5) concatenates opaque blobs; a delete rewrites the packed object on the server, which needs no key. The brief's "who can rewrite a chunk when only the sender may write" dissolves.

A joiner who fetched before a delete receives the live redaction frame like any current member. A device that fetched and then left keeps its copy, as with any member today.

### 4.9 Disappearing messages and lookback

- `ChannelHistory::Ephemeral` and `Archived` are exclusive by type; switching a channel to `Ephemeral` closes the share range and stops writes at that epoch. A message carrying a timer is never archived even if the mode flips later, because the ticket is minted from the mode at seal time. Signal excludes disappearing and view-once content from backups for the same reason [Signal support].
- Lookback (#150) keeps meaning "how much this device stores"; it no longer means "my history is gone". Hydration never refills past the lookback window. Preferences copy must say the archive, not the device, now bounds history in archived channels.

### 4.10 Retention, quota, #720

- **Retention**: group setting, `archive_retention` in {90 days, 1 year, 3 years}, default 1 year (open decision D3). The DS deletes entries by `created_at`, and clients delete `archive_epoch_key` and share-key cache rows past the same horizon, which gives the archive bounded forward secrecy. This is a TTL, and that is correct here: the archive is history, not delivery. Envelope retention stays watermark-gated (I3); the archive TTL never shortens it.
- **Quota**: per-group bytes by tier (#861/#870). Over quota, the oldest whole epochs are evicted with a visible "history before X is no longer available" row. Writes are never refused (refusing writes makes holes, which is "flaky"; evicting the oldest is "bounded").
- **#720**: a device silent past 12 months still recovers every message within archive retention, provided the group advanced at most 512 epochs while it was away, because it can then derive `H_e` from the retained commit log. Past the Tier-2 cap (`pollis-delivery/src/commit.rs:600`) it external-joins at head and cannot derive the gap's `H_e`, unless a sibling hands it the ring (not designed). For archived groups loss (3) shrinks to "silent past 12 months AND (past archive retention OR more than 512 epochs behind)". The 12 months is the deployed value (`wrangler.{dev,prod}.jsonc`); the code fallback when the variable is unset is 6 (`pollis-delivery/src/messages.rs:291`).
- **Attachments**: new `archive_attachment_ref` leg in `live_ref_exists!`, joined to a live `archive_entry`. Without it, attachment GC collects blobs that archive entries still name. Media may get a shorter horizon than text (Signal keeps 45 days of media free); open decision D5.

### 4.11 Metadata visible to the server

| Fact | Today | With the archive |
|---|---|---|
| A channel has archive / sharing on, since which epoch | no | yes (needed for gating; comparable to Matrix servers seeing `history_visibility`) |
| Per-message size bucket, time, channel | transient (until envelope GC) | **durable for the retention window**: a dump yields a full per-channel activity timeline |
| Sender of an entry | live DS sees it on the request (as for envelopes); not at rest | same |
| Which epochs had traffic in a sharing channel | derivable from envelopes, transient | durable (`group_share_key` rows) |
| A joiner bulk-downloaded history | no | yes (request timing) |
| Who deleted or edited | token-gated, not attributed | same; `revised_at` and `admin_removed` make edit and moderation timing durable |
| Device-to-entry linkage | none at rest | `user_device.archive_capable_at`, if set on a device's first entry, links that device to an exact entry time; set it only from an explicit capability publish, coarsened to the day |
| Size of a user's member-history footprint | no | `history_ring_transfer` blob size reveals how many group-epochs the user holds (pad it) |
| Already durable today, for comparison | commit log `sender_id` and timing (`commit.rs:366`), plaintext reactions with reactor and time, `pinned_by` | unchanged |

Padding uses the PADMÉ buckets already in `framing.rs`. The durable timeline is the main metadata regression and must be written into `docs/metadata-retention-policy.md` and whitepaper §1.2. Padding does nothing for timing. Reviewer: also coarsen `created_at` to the day (retention needs no more), avoid exposing a dense `seq` where an opaque sort key would do, and say in the channel's ambient marker that the server keeps an activity record. The whitepaper already concedes live activity timing as irreducible (§1.2, "when they are active"); what is new is that a single dump yields a year of it per channel.

### 4.12 The import archive (#1179/#1180 revised)

The admin's device writes entries with `kind = import`, signed by its device key, with `import_meta { source, source_author_hash, source_ts }`, under a random per-import key `K_imp` carried as a `group_share_key` row with `epoch = NULL, import_id` (the trigger allows import rows only for channels whose share range is open, so joiner exposure of imports follows the same switch). Members derive nothing; they unwrap `K_imp` with `S_g` like a joiner. Claims (brief item 4) are an overlay keyed by `source_author_hash`. The admin signature proves only "this admin imported this", never "this author said this"; the UI keeps the permanent "imported" badge. Bulk writes are the one exception to "entry needs its envelope": import entries need an `import_batch` row created by an admin, rate-limited and quota-counted.

### 4.13 Backward compatibility and rollout

- **Wire**: every new field is optional with `serde(default)`; an old client ignores `archive_c1_sha256` in fetch rows and never sends `archive`. Old clients render the `0xF9` trailer as an ordinary message (test like `old_client_ignores_receipt_frame`; must also cover thread-aware clients seeing `0xF9` at offset `N` and treating it as "no thread").
- **Holes**: messages from old clients are not archived. Readers cannot count them. Rollout control: a device advertises `archive_capable` (nullable column on `user_device`, measured: set by the DS when the device first sends an entry or publishes the capability). The settings page shows "N members' apps do not archive yet". Enabling is allowed regardless, since refusing sends from old clients would break messaging.
- **Mobile**: the archive lives in `pollis-core`, so phones get it only with a new store build; store builds lag, so old mobile builds will be the long tail of holes. Every DS change stays compatible with the oldest store build (all additive).
- **Order**: DS + migration first, then clients. A shipped DS without the new endpoints does **not** reject archive fields: `pollis-api` uses no `deny_unknown_fields` anywhere, so an old DS silently drops `archive` and `share_key` and returns success. The send response must therefore carry an explicit `archived: true` acknowledgement, and clients treat its absence as "archive unavailable", never as a send failure (the archive must never block a message). This matters for self-hosted DS instances (`docs/run-it-yourself.md`) that lag the client.
- **Old committers and approvers.** A Welcome joiner gets `S_g` only if the committer is archive-capable, and a new device gets a ring only if the approving sibling is. With old mobile builds in the long tail, both fall back to waiting for another holder, or to nothing.

## 5. Pollis changes

### 5.1 Migration sketch (`pollis-schema/migrations/000036_history_archive.sql`)

Additive only: new tables, indexes, triggers, one nullable column. A shipped client and a shipped DS never touch them.

```sql
-- One archived revision of one message. Opaque blob; no sender at rest.
CREATE TABLE IF NOT EXISTS archive_entry (
    message_id        TEXT PRIMARY KEY,         -- = message_envelope.id at first write
    conversation_id   TEXT NOT NULL,            -- channel or DM channel
    mls_group_id      TEXT NOT NULL,
    generation        INTEGER NOT NULL,
    epoch             INTEGER NOT NULL,
    kc                BLOB NOT NULL CHECK (length(kc) = 16),
    seq               INTEGER NOT NULL,         -- envelope's DS seq, ordering only
    rev               INTEGER NOT NULL DEFAULT 0 CHECK (rev >= 0),
    blob              BLOB NOT NULL CHECK (length(blob) <= 65536),
    c1_sha256         BLOB NOT NULL CHECK (length(c1_sha256) = 32),
    delete_token_hash TEXT,
    admin_removed     INTEGER NOT NULL DEFAULT 0 CHECK (admin_removed IN (0, 1)),
    created_at        TEXT NOT NULL DEFAULT (datetime('now')),
    revised_at        TEXT
);
CREATE INDEX IF NOT EXISTS idx_archive_entry_conv_seq ON archive_entry (conversation_id, seq DESC);
CREATE INDEX IF NOT EXISTS idx_archive_entry_epoch ON archive_entry (conversation_id, generation, epoch, kc);
CREATE INDEX IF NOT EXISTS idx_archive_entry_created ON archive_entry (created_at);

-- An entry cannot exist without the envelope it archives (import entries use import_batch instead).
CREATE TRIGGER IF NOT EXISTS archive_entry_needs_envelope
BEFORE INSERT ON archive_entry
WHEN NOT EXISTS (SELECT 1 FROM message_envelope WHERE id = NEW.message_id)
 AND NOT EXISTS (SELECT 1 FROM import_batch_entry WHERE message_id = NEW.message_id)
BEGIN SELECT RAISE(ABORT, 'archive entry without envelope'); END;

-- A revision only moves forward.
CREATE TRIGGER IF NOT EXISTS archive_entry_rev_monotone
BEFORE UPDATE OF rev ON archive_entry
WHEN NEW.rev <= OLD.rev
BEGIN SELECT RAISE(ABORT, 'archive revision must increase'); END;

-- Group and channel modes, mirrored from MLS-authenticated commits.
CREATE TABLE IF NOT EXISTS group_history_mode (
    group_id         TEXT PRIMARY KEY,
    member_history   INTEGER NOT NULL CHECK (member_history IN (0, 1)),
    retention_days   INTEGER NOT NULL CHECK (retention_days IN (90, 365, 1095)),
    since_generation INTEGER NOT NULL,
    since_epoch      INTEGER NOT NULL
);

-- Joiner-sharing ranges, one row per enable; at most one open range per channel.
CREATE TABLE IF NOT EXISTS channel_history_share (
    channel_id      TEXT NOT NULL,
    group_id        TEXT NOT NULL,
    from_generation INTEGER NOT NULL,
    from_epoch      INTEGER NOT NULL,
    to_generation   INTEGER,
    to_epoch        INTEGER,
    enabled_by      TEXT NOT NULL,
    PRIMARY KEY (channel_id, from_generation, from_epoch),
    CHECK ((to_generation IS NULL) = (to_epoch IS NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_share_one_open
    ON channel_history_share (channel_id) WHERE to_epoch IS NULL;

-- Current wrap of the group share master. Same CAS + I9 ceiling as pin_keystate.
CREATE TABLE IF NOT EXISTS group_share_keystate (
    group_id             TEXT PRIMARY KEY,
    key_version          INTEGER NOT NULL CHECK (key_version >= 0),
    wrapped              BLOB NOT NULL,
    nonce                BLOB NOT NULL,
    generation           INTEGER NOT NULL,
    epoch                INTEGER NOT NULL,
    rotated_generation   INTEGER NOT NULL,
    rotated_epoch        INTEGER NOT NULL,
    updated_at           TEXT NOT NULL DEFAULT (datetime('now')),
    updated_by_device_id TEXT NOT NULL
);

-- Version v's key under version v+1. Multiple candidates per version; insert-only.
CREATE TABLE IF NOT EXISTS group_share_key_link (
    group_id    TEXT NOT NULL,
    key_version INTEGER NOT NULL,
    link_id     TEXT NOT NULL,
    nonce       BLOB NOT NULL,
    wrapped     BLOB NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (group_id, key_version, link_id)
);
CREATE TRIGGER IF NOT EXISTS share_link_cap
BEFORE INSERT ON group_share_key_link
WHEN (SELECT COUNT(*) FROM group_share_key_link
       WHERE group_id = NEW.group_id AND key_version = NEW.key_version) >= 8
BEGIN SELECT RAISE(ABORT, 'too many link candidates'); END;

-- K(c,e) under S_g^v. Only inside a share range, only for a kc some entry uses.
CREATE TABLE IF NOT EXISTS group_share_key (
    channel_id  TEXT NOT NULL,
    generation  INTEGER NOT NULL,
    epoch       INTEGER NOT NULL,
    kc          BLOB NOT NULL CHECK (length(kc) = 16),
    key_version INTEGER NOT NULL,
    nonce       BLOB NOT NULL,
    wrapped     BLOB NOT NULL,
    PRIMARY KEY (channel_id, generation, epoch, kc)
);
CREATE TRIGGER IF NOT EXISTS share_key_in_range
BEFORE INSERT ON group_share_key
WHEN NOT EXISTS (
    SELECT 1 FROM channel_history_share s
     WHERE s.channel_id = NEW.channel_id
       AND (NEW.generation > s.from_generation
            OR (NEW.generation = s.from_generation AND NEW.epoch >= s.from_epoch))
       AND (s.to_generation IS NULL
            OR NEW.generation < s.to_generation
            OR (NEW.generation = s.to_generation AND NEW.epoch < s.to_epoch)))
  OR NOT EXISTS (
    SELECT 1 FROM archive_entry a
     WHERE a.conversation_id = NEW.channel_id AND a.generation = NEW.generation
       AND a.epoch = NEW.epoch AND a.kc = NEW.kc)
BEGIN SELECT RAISE(ABORT, 'share key outside share range or without entry'); END;

-- Attachment liveness leg for archived messages.
CREATE TABLE IF NOT EXISTS archive_attachment_ref (
    content_hash TEXT NOT NULL,
    message_id   TEXT NOT NULL,
    PRIMARY KEY (content_hash, message_id)
);
CREATE INDEX IF NOT EXISTS idx_archive_attachment_ref_msg ON archive_attachment_ref (message_id);

-- Ring transfer to the user's own new device. Deleted on pickup; TTL.
CREATE TABLE IF NOT EXISTS history_ring_transfer (
    id               TEXT PRIMARY KEY,
    user_id          TEXT NOT NULL,
    target_device_id TEXT NOT NULL,
    kind             TEXT NOT NULL CHECK (kind IN ('snapshot', 'delta')),
    nonce            BLOB NOT NULL,
    blob             BLOB NOT NULL CHECK (length(blob) <= 4194304),
    created_at       TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_ring_transfer_target ON history_ring_transfer (user_id, target_device_id);

-- Import batches (#1179/#1180).
CREATE TABLE IF NOT EXISTS import_batch (
    id TEXT PRIMARY KEY, group_id TEXT NOT NULL, created_by TEXT NOT NULL,
    entry_count INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE IF NOT EXISTS import_batch_entry (
    message_id TEXT PRIMARY KEY, batch_id TEXT NOT NULL
);

-- Rollout coverage; NULL = unknown = not capable.
ALTER TABLE user_device ADD COLUMN archive_capable_at TEXT;
```

The share-range trigger compares `(generation, epoch)` lexicographically, so a suite migration (§6.10) that opens `gen+1` at epoch 0 continues an open range without a gap. Teardown (`teardown.rs`) deletes every row of a destroyed group.

### 5.2 DS (`pollis-delivery`) and wire types (`pollis-api`)

| Endpoint | Auth | Notes |
|---|---|---|
| `/v1/messages/send`, `/edit`, `/delete` | device-signed (existing) | new optional archive fields; one transaction; I8 epoch gate applies to the entry |
| `/v1/commits` | existing | optional `history_setting`; DS checks admin role, writes `group_history_mode` / `channel_history_share` in the commit's transaction |
| `/v1/archive/share/keystate` | device-signed, member | mint (insert-only) or CAS upsert; `refuse_above_head` |
| `/v1/archive/share/link` | member | insert candidate |
| `/v1/archive/ring-transfer` (+ `/read/...`, `/ack`) | same account only | snapshot / delta; delete on ack; TTL sweep |
| `/v1/read/archive/entries` | member + least-privilege epoch filter | paged |
| `/v1/read/archive/share` | member | keystate, links, key rows for requested channels |
| `/v1/read/conversations/catch-up` fetch rows | existing | add `archive_c1_sha256` per envelope |
| `/v1/archive/import/*` | admin | batch create, bulk entries, rate-limited |

Retention sweep runs where envelope GC runs today (`sweep_envelope_gc`). That is a DS-side interval timer (`pollis-delivery/src/main.rs:146`, `POLLIS_DS_GC_SWEEP_SECS`, default 3600), not request traffic. The no-polling rule targets clients, so this is acceptable. Rate limits in `ratelimit.rs`, per device, not persisted.

### 5.3 `pollis-core`

New `commands/archive/`: `keys.rs` (H cache, K derivation, ring export/import), `entry.rs` (seal, open, verify, chain), `write.rs` (send/edit/delete hooks, reseal), `read.rs` (page, verify, hydrate, dedup), `share.rs` (`S_g` mint, wrap, rotate, adopt; a sibling of `pinned_messages.rs`), `transfer.rs` (enrollment hooks). Touch points: the epoch-change hook sites listed in `mls.md` "Pinned-message key state"; `messages/send.rs`, `edit_delete.rs`, `seal.rs`, `framing.rs` (`0xF9` trailer), `ingest.rs` (trailer and DS-hash check, chain heads); `device_enrollment.rs`, `device_link.rs`; `groups/invites.rs` (adopt after external join); export (#856).

Local schema additions: `archive_epoch_key(group_id, generation, epoch, key)`, `archive_share_cache(group_id, key_version, key)`, `archive_chain_head(sender_device_id, channel_id, head, last_message_id)`, `message.origin TEXT CHECK (origin IN ('live','archive'))`, `message.archive_verified INTEGER`. Added by `add_missing_columns` like `thread_id`.

Commands (desktop shim + `invoke_handler!` + test harness; mobile via uniffi): `set_group_history_mode`, `set_channel_history_share`, `get_history_status`, `load_archive_page`, `get_archive_coverage`.

### 5.4 UI, desktop and mobile in one PR per phase

- Group settings → History page (bar pattern, no modal): member-history switch, retention choice, coverage ("N members' apps do not archive yet"), storage use.
- Channel settings: "Share history with new members" switch. Enabling replaces the input bar with a confirmation bar stating "New members will be able to read messages sent from now on. Earlier messages stay private."
- Channel header: persistent ambient marker while sharing is on; a system row in the timeline on every mode change, rendered from the authenticated commit.
- Scroll-back states from Section 4.7; one-time join banner.
- Security page: archive equivocation and verification alerts.
- Mobile: same features through the bridge, native header marker and settings screens (`mobile/CLAUDE.md` patterns).
- E2E: Playwright `e2e/archive-share.spec.ts`, `e2e/archive-member-history.spec.ts`; Maestro `mobile/.maestro/flows/archive-share.yaml`, `archive-member-history.yaml` (two-client, `_two-client.md`).

### 5.5 Docs and principle changes (same PR as the code that makes them true)

- **CLAUDE.md**, "Messages must work": losses (1) and (2) gain "unless the group or channel has an archive mode on (§6.11), in which case the archive key schedule bounds them". Replace "no key backup, never add Megolm-style backup unless explicitly asked" with "no recoverable key backup: archive keys live only on member devices; adding any secret that opens history without a member device needs an explicit owner decision". State the owner's explicit decision with the date.
- **docs/backend-core-invariants.md**: rewrite the bounded-history caveats (a)/(b); add I11 and I12 (Section 6); add (c)'s archive interaction.
- **docs/security-whitepaper.md**: §1.2 (durable timeline, archive rows); §6.8 rewritten (it is already inaccurate: pins expose pre-join content); new §6.11 "History archive" with every label and key above; §12 key table (H_e, K, S_g, RK); §13 item 7 replaced; §14 comparisons (Matrix MSC4268, Signal backups).
- **docs/metadata-retention-policy.md**: archive retention, durable timeline, Turso point-in-time restore keeps deleted rows up to 30 days on paid plans [Turso PITR].
- **.codesight/wiki**: `mls.md` (archive key state section beside pins), `database.md`, `commands.md`, `testing.md`.

### 5.6 Phases (one ticket, one PR, a commit per phase)

P1 schema + DS endpoints + DS tests. P2 core: H cache, entry seal/verify, send/edit/delete/reseal, member reads and placeholder healing. P3 member history ring transfer. P4 joiner sharing (`S_g`, key rows, ranges, AAD settings). P5 UI desktop + mobile + e2e both tiers. P6 docs and principle edits. Optional P7: server-side packing to R2, import batches. The only legitimate split is the deploy boundary: the DS must be live before clients rely on it.

Effort estimate (by analogy to pins: 764 core + 443 DS + 176 api + ~700 test LOC): roughly 3.5k to 5k Rust, 1k TS desktop, 1k TS mobile, 2k tests. Reviewer: low confidence and probably optimistic. The store has several times the surface of pins (rotation, links, ring transfer, two trailers, chains, import, quota, AAD settings plus a GroupContext extension). 8k to 12k total is more plausible. Mobile verification per CLAUDE.md also needs Release builds on the Android emulator and the iOS simulator against the dev DS, not only Maestro flow files. The keeper path (headless runtime, pairing, unattended unlock, replay, multi-host coordination, VPS template) is larger and carries an operational surface the store does not.

## 6. Invariants and tests

| # | Invariant | Layer | Test that tries to create the invalid state |
|---|---|---|---|
| I-A1 | A reader can open an entry only if its account was in the tree at the entry's epoch, or the channel shared that epoch | key schedule + DB trigger | non-member-at-send leaf (left at e, rejoined at e+5) cannot decrypt entries at e+1..e+4 in a non-sharing channel; joiner of a sharing channel can |
| I-A2 | No share key row for an epoch outside a share range (honest-DS property; defends against DS bugs, not a malicious operator) | DB trigger | raw SQL insert below `from_epoch`, after `to_epoch`, and with no matching entry, all refused (DS handler bypassed) |
| I-A2b | A device never writes a share row from a range it did not verify | GroupContext extension (D2) | DS serves a forged range to a joiner; joiner writes no share row for that channel |
| I-A2c | No member can squat a share-key row, a link slot or the keystate wrap | candidate rows, atomic link write, commitment in authenticated mode | malicious member pre-writes garbage with the correct `kc`, fills link slots, CAS-writes a fake `S'`; a joiner still reads |
| I-A9b | An account-seed thief does not learn `H_e` minted after the leaf heals | delta sealed to a device KEM key | exfiltrate the account seed; victim self-updates; delta rows after that are undecryptable |
| I-A15b | Thread replies keep threading on thread-aware old clients | framing | `0xF7` stays at `N`, `0xF9` after it |
| I-A16 | Turning member history off purges `H_e` caches, and the UI states what survives | client discipline + test | disable; no `archive_epoch_key` rows remain on any device |
| I-A3 | No key exists for epochs before any archive mode was on | client discipline + test | after enabling at e, local DB has no `archive_epoch_key` below e on any device; exporter is gone |
| I-A4 | An entry cannot exist without its envelope (or an import batch) | DB trigger | raw insert refused |
| I-A5 | Archived and ephemeral are exclusive | Rust type | does not compile; flows: timer message never reaches `archive_entry` |
| I-A6 | No entry at a closed epoch | I8 gate extended | commit lands mid-send; DS refuses, client reseals both; no entry at e |
| I-A7 | Revisions only move forward; only the author's token or an admin replaces | DB trigger + token hash | replay of rev 0 after a tombstone refused |
| I-A8 | A removed member cannot read share keys written after its removal | `S_g` rotation | exfiltrate `S_g^v` before removal, read Turso directly after; new rows undecryptable |
| I-A9 | A stolen `S_g` heals at the victim's self-update | rotation rule | mirror of `a_stolen_leaf_is_locked_out_once_the_victim_rotates` |
| I-A10 | Equivocation is detected | live trailer + deterministic C1 | sender archives a different body; every receiver flags |
| I-A11 | Interior omission is detected | per-sender chain | DS drops one entry; reader flags a gap |
| I-A12 | `group_share_keystate` cannot be frozen | I9 ceiling | `generation = 2^62` refused, as `control_plane_head_bounds.rs` |
| I-A13 | Archive never blocks a send | protocol | DS without archive endpoints: send succeeds, archive marked unavailable |
| I-A14 | Archived attachments stay alive | liveness leg | envelope GC'd, entry live, blob not collected |
| I-A15 | Old clients render archive-trailer messages normally | framing | pre-thread and thread-aware decoders on `0xF9` frames |

Plus DS endpoint tests in `pollis-delivery/tests/archive.rs`, flows scenarios in `src-tauri/tests/flows/archive.rs`, and the model-based fuzzer extended with archive reads after churn.

## 7. Comparison

### 7.1 Table

| Property | Status quo | Keeper (#1181) | Store (this) | Megolm-style backup |
|---|---|---|---|---|
| Server at rest after delivery | no message ciphertext (envelopes collected); existing durable metadata (commit log sender/timing, plaintext reactions, pins) | same as status quo | history ciphertext + timeline for the retention window, on top of status quo | all ciphertext + wrapped keys |
| Server dump alone | nothing | nothing | nothing (keys never server-side; AES-256; PQ exporter on the PQ suite only) | nothing with a strong recovery key; offline brute force with a passphrase |
| Server dump + one member device | that device's local DB (bounded by its lookback, #150) | that device's DB; the device can also ask the keeper | every entry the account was entitled to within retention (up to 3 years, and with member history the ring is on every device of the account), every shared-channel entry back to the share-enable date even for the newest joiner's device, plus what that device evicted or never held | everything that account ever held |
| Devices whose compromise opens the full shared-channel history | n/a | the keeper hosts | every current member device | every device of every member |
| Keeper host compromise | n/a | full plaintext archive of opted-in channels; can also forge history | n/a | n/a |
| Forward secrecy of history | n/a | none on the keeper box | none inside retention; keys and rows deleted past it | none |
| PCS | MLS | MLS for transport; a compromised keeper stays compromised until removed | H_e heals with MLS (not yet for the account-seed-sealed delta, Section 4.3.7); `S_g` rotates on removal and self-update | none |
| Authenticity of history | MLS-signed at receipt | as specified, "keeper says Alice said X" (forwarder trust, the MSC4268 problem); fixable by forwarding the original MLS FramedContent and signature, which a reader verifies against the certified device key (leaf keys are the certified device keys, whitepaper §2.2, §5.3) | sender device signature, verifiable by any later reader | Megolm v1 unauthenticated, injectable |
| Completeness | n/a | complete if the keeper catches up within 12 months and 512 epochs (I3, Tier-2 cap); sender-agnostic, including old clients | complete for archiving senders; old clients (the mobile long tail) make holes | per client |
| Read availability | n/a | keeper online at request time | DS + keys: immediate for members and Welcome joiners; external joiners wait for one holder; own device needs the approver | recovery key |
| Lost every device | lost | recovered (keeper replays to the re-enrolled account) | lost, unless D1 | recovered |
| DMs | no | no, by policy (no admin); a user-owned always-on device could serve self-replay | yes, member history | yes |
| Consent prospective | n/a | physical | physical before member history is on; for sharing enabled later, client discipline + honest DS (Section 4.3.5) | no (Matrix needed a `shared_history` flag) |
| Protocol, wire and schema change | n/a | none for readers (ordinary members and messages) | new trailer, new tables, new endpoints, a GroupContext extension | new tables |
| Search | local | possible on the keeper host | client-side over hydrated history only | client-side |
| Operational burden | none | customer box, unattended unlock, VPS template, upgrades | none for customers; Pollis storage | Pollis storage + recovery UX |
| Cost to Pollis | none | none | ~2.8 KB per message; 1M messages ≈ 2.8 GB ≈ $0.04/month on R2 Standard at $0.015/GB-month [R2 pricing], more on Turso rows | similar |
| New security surface | none | unattended unlock (§3.5 limitation permanent) | durable server ciphertext, durable timeline, key caches | recovery secret |

### 7.2 Prose

The keeper's real advantages are more than three. It is sender-agnostic: it archives old-client messages, which matters for years given the mobile store long tail. It recovers "lost every device" without any new secret. It puts nothing durable on Pollis servers, so there is no metadata regression. It needs no protocol, wire or framing change, and works with the oldest shipped clients as readers. It keeps the MLS key schedule untouched (no retained per-epoch secrets on every device). Its storage sits on a box the customer controls rather than with Pollis. It can offer search. Its real disadvantages are that it concentrates plaintext on a box someone has to run unattended (the §3.5 limitation, permanently), that it adds operational work most groups will not do, and that DMs are excluded. The authenticity gap is a property of #1181's "replay as ordinary application messages" choice, not of keepers in general: forwarding the original signed MLS content would let a joiner verify the sender's certified device key. The reviewer treats this as fixable and not a decisive argument for the store.

The store's advantages are the inverse: no box, sender-authenticated history, DMs, exact entitlement for free, and availability tied to the DS, which is already a hard dependency. Its disadvantages are the durable server-side ciphertext and timeline, holes during the fleet transition, and no lost-every-device recovery by default.

### 7.3 Is this "backup with extra steps"?

Partly yes, and the docs must say so. Structurally it is server-held encrypted history, which CLAUDE.md forbids by default, and the owner is explicitly lifting that. What separates it from Megolm-style backup is specific: no secret outside member devices opens it (no recovery key, no passphrase, no brute-force target); entitlement is the MLS tree at send time, not "whatever keys some client uploaded"; entries are sender-signed, so the server and other members cannot inject history (MSC4048's problem); enabling is prospective and visible; retention is bounded. What it shares with Megolm backup: the server holds long-lived ciphertext, and any single compromised entitled device plus a server dump reads it. The first difference set is why it is acceptable; the shared set is why it must stay opt-in with an ambient marker. Precedent: the shipped vault (#107, `pollis-core/src/commands/vault.rs` header) is already a server-held store sealed under an account-key-derived key, which the Secret Key reopens. Pollis has crossed this line once, for a user's own notes.

Reviewer's answer: for member history (#1185) it is a backup with extra steps. The ring of `H_e` is copied to every device of the account and kept for the retention window. Functionally that is a server-held encrypted backup whose key lives on every device, and its main merits over Megolm backup are entitlement and signatures, not the absence of a long-lived key. A cheaper #1185 for the approval path is Signal-style device transfer: the approving sibling streams its local history, sealed under the enrollment channel, with no durable server store. The doc lists this as prior art but does not evaluate it as an option. For joiner sharing (#1186) it is not a backup; it is history sharing, and that is where the store's design earns its complexity.

### 7.4 Do they compose?

Yes, if the keeper is reduced to a **key-holder**. An always-on member device in member-history mode holds `H_e` for every epoch and the `S_g` cache. With no history-specific runtime it already (a) re-wraps `S_g` promptly, which removes the external-join wait, and (b) could, with one added capability, hand `K(c,e)` for the epochs a re-enrolled account was a member (entitlement from per-epoch roster snapshots it records at merge) to that account's recovered device, which closes "lost every device" without a recovery secret and without a plaintext cache. Its compromise then equals one member device's compromise, not a plaintext archive. This is a later, optional ticket under #1182's identity work.

## 8. Where this breaks

### 8.1 Attacks

| Attack | Outcome | Mitigation | Residual |
|---|---|---|---|
| Malicious member forges another sender's entry | fails | signature by the victim's certified device key inside the AEAD | none |
| Malicious member writes garbage entries | visible as its own messages; costs it a real envelope | 1:1 entry-to-envelope trigger, size cap, rate limit | spam, handled like spam |
| Sender equivocates (archive differs from live) | detected by every updated receiver | live trailer + deterministic C1 + DS hash | senders whose receivers are all old clients |
| Malicious member pre-empts a share key row with garbage | **succeeds as drafted**: the member knows `K(c,e)`, computes the right `kc`, and its `INSERT OR IGNORE` wins | candidate rows per key, validated by `kc` after unwrap (Section 4.3.4) | none once fixed |
| Malicious member writes a well-formed fake `S_g` wrap, or fills link slots | as drafted, joiners can adopt a fork, and link slots can be exhausted, including for future versions | commitment to `S_g` in the authenticated mode; link written atomically with the rotating CAS | delay only once fixed |
| Malicious DS serves a forged share range to a joiner | the joiner wraps keys for an unshared channel; later joiners read that member's messages there | D2 GroupContext extension mandatory; never write share rows from unverified ranges | none once fixed |
| Account-seed thief (stolen device, healed leaf) | as drafted, keeps reading ring deltas, so learns future `H_e` | seal deltas to a per-device KEM key | none once fixed |
| Malicious member withholds keys | cannot: `H_e` is derived, any one honest holder re-wraps | | if every holder is malicious or old, joiners get nothing |
| Removed member + server collusion | reads entries of its own membership epochs (it saw them live) and entries that the DS retained after deletion | honest DS hard-deletes; rotation of `S_g` | Turso point-in-time restore keeps deleted rows up to 30 days on paid plans [Turso PITR]; same exposure exists for envelopes today |
| Rotation races (commit during send, two rotators) | none | I8 409 + reseal; insert-only link versions; CAS keystate | |
| Sender deletes after a joiner fetched | joiner applies the live redaction as a member | existing redaction frames | a joiner who fetched and left keeps it, as any member |
| DS serves a deleted entry (rollback) to a later joiner | joiner shows deleted content | none cryptographic; tombstones and chains do not stop a full rollback of one entry | trust equal to today: the DS can already withhold redaction envelopes from members |
| DS truncates or omits entries | interior gaps detected by sender chains; tail truncation for a sender who never posts again is not | chains; optional archive heads in the verifiable log (D6) | silent tail truncation |
| DS serves a stale keystate | cannot be opened at the joiner's epoch | exporter-bound wrap, CAS, I9 ceiling | delay |
| Disappearing messages archived | cannot, by type | `ChannelHistory` enum, ticket minted at seal time | a malicious sender's own client can do anything with its own messages |
| Admin silently enables sharing | not silent: authenticated commit, system row, marker | old clients do not render the marker | members on old clients do not see it (their own messages are also not archived, so they are not exposed) |
| Quota exhaustion to evict history | old epochs evicted early | per-device rate limits, per-group quota visible to admins, removal of the abuser | a determined member can evict history faster than admins react |
| Traffic analysis | durable timeline at rest | padding, no sender at rest | this is the main regression; accept or do not ship |
| Stolen device, not yet revoked | reads everything that device was entitled to, plus all shared channels | revoke → removal commit → rotation | same as stealing its local DB, plus evicted and never-held entries |
| Account deletion | entries stay (no sender at rest, DS cannot find them) | client deletes its own entries by token before deleting the account (it has the ids locally) | users who lost all devices cannot erase their archived messages; document it |
| Mixed fleet | holes for old-client senders | coverage display, capability measurement | joiners cannot see holes they did not know about |
| Server-assigned roles | a DS that forges admin roles could accept a forged mode change | members verify the committer and the AAD; role itself comes from the DS (existing limitation) | same as every role-gated action today |
| Deniability | entries are transferable proof of authorship to any future joiner | none; MLS already signs | must be stated |

### 8.2 What it cannot do

- Lost every device: not without D1 (Megolm-shaped) or the key-holder composition (Section 7.4).
- History from before an archive mode was enabled: never, by construction.
- Archive messages from clients that predate it.
- Server-side search.

### 8.3 What the brief and the keeper tickets got wrong

1. **"History keys rotate periodically and on every member removal; new keys distributed over MLS."** Unnecessary for current members: per-epoch keys from the MLS exporter rotate with every commit and need no distribution. Only the joiner share master needs explicit rotation, and the repo already ships its shape (`pin_keystate`).
2. **"Any online member device that holds the keys hands them over."** Mostly free: Welcome joiners get keys from the committer, which is online by definition; own new devices get them from the approving sibling, also online by definition. Only external-join joiners need a holder later, which pins already accept.
3. **"Chunk the archive so deletes rewrite a chunk" and "who can rewrite a chunk when only the sender may write".** Per-entry rows remove the problem; packing, if wanted, is server-side over opaque entries.
4. **"Signed with their identity key."** Use the certified device key, and sign inside the encryption: a visible signature lets the DS identify the sender by trial verification against every member's public key, defeating sealed sender (#607).
5. **"Extra exposure is mostly what that device's local DB already holds, except deleted or disappearing messages if mishandled."** Incomplete: also lookback-evicted messages (#150), every shared-channel message for even the newest member, a durable server-side activity timeline, and deleted rows retained by Turso point-in-time restore.
6. **#1187 and the brief: "an offline keeper permanently loses that window."** Overstated, not simply wrong. Shipped invariant I3 (`docs/backend-core-invariants.md:141`; `CLEANUP_*`, `pollis-delivery/src/messages.rs:245-275`) retains envelopes until every live member device has reported a watermark past them, and catch-up interleaves decryption with commit replay (`pollis-core/src/commands/mls/sweep.rs:232-236`). So a keeper offline for a weekend loses nothing. It does lose messages in two shipped cases. (a) It is silent past `POLLIS_DS_WATERMARK_STALE_MONTHS`: 12 deployed (`wrangler.{dev,prod}.jsonc`), 6 as the code fallback (`messages.rs:291`). (b) The group advances more than `PRUNE_MAX_BEHIND_HEAD = 512` epochs while it is away (`commit.rs:600`). The keeper then external-joins at head and the gap is gone, which the commit-log docs call accepted loss (1). Self-updates run per device per group roughly weekly (whitepaper §6.7), so a group with about 100 active devices can pass 512 epochs in about five weeks: a vacation-length outage, not a weekend. The same cap bounds this store's own healing claims (4.3.1, 4.10). Multi-host is mostly availability, with an integrity component for long outages in busy groups. This weakens one argument for the store. Whether it changes the verdict depends on how much weight the authenticity argument carries, and Section 7.2 shows that argument is fixable on the keeper side.
7. **"Pre-join loss is a cryptographic property"** (CLAUDE.md, whitepaper §6.8). Already false for pins: any current member, including a joiner, reads every pin's content snapshot (`pinned_messages.rs` "What the honest guarantee is"). The vault already defeats loss (2) for vault entries. The principle text must be corrected regardless of this proposal.
8. **"The store does NOT fix lost every device."** True by default, but it is one design choice away (an account-key-sealed ring, the vault construction), which is exactly Megolm-style recovery. It is listed as D1 so it is decided, not drifted into.
9. **"Device-to-device transfer can reuse the §5.1 sealed channel."** Only for the snapshot at approval; §5.1's ephemeral key lives in memory and the new device's group joins happen later, so a second, account-sealed delta is required. The Secret Key path has no sibling at all.

### 8.4 Findings in the shipped code worth separate tickets

Reviewer verified both claims against the code at `ec22dec`.

- **Pins are forgeable. CONFIRMED; severity medium (integrity, insider only).** `PinSnapshot { v, message_id, conversation_id, sender_id, sent_at, content }` (`pollis-core/src/commands/pinned_messages.rs:109-116`) is AES-GCM sealed under `Kpin` with no signature. `pin_message` fills it from the local `message` row (`:486-519`), but nothing checks that on the reader side. `list_pinned_messages` checks only that `message_id` and `conversation_id` match the row (`:614`) and renders `snapshot.sender_id` and `snapshot.content`. The DS checks only membership and envelope shape (`pollis-delivery/src/pins.rs:273-311`); it does not check that the message exists or what it says. So a member running a modified client can pin arbitrary text attributed to any user id, and every current and future member sees it. Joiners cannot cross-check, because they lack the original. The desktop panel shows the snapshot's author, not the pinner (`frontend/src/components/Layout/RightPanel/PinnedPanel.tsx:65`). `pinned_by` is DS-authenticated and stored (`pins.rs:278, 303`) but not displayed. `ON CONFLICT(conversation_id, message_id) DO NOTHING` (`pins.rs:301`) also lets a forger squat a real message's pin slot until someone unpins it. The DS itself cannot forge (it has no `Kpin`). Fix independently of the archive: sign the snapshot with the pinner's device key and render "pinned by X", and, once an archive exists, reference a signed entry. Related, unverified by test: `pin_keystate` re-wrap is a first-writer-wins CAS (`pins.rs:217-230`), so a malicious member may be able to fork `Kpin` for later joiners. The "forks are unrepresentable" claim (`pinned_messages.rs:49-56`) holds against honest members only.
- **Pins expose pre-join content**, and `Kpin` never rotates, so a removed member who kept it plus a breach reads later pins (documented trade-off, `pinned_messages.rs:33-46`, `.codesight/wiki/mls.md:654-660`). CONFIRMED. Whitepaper §6.8 (`docs/security-whitepaper.md:393-395`) says pre-join messages are not visible and does not mention pins at all.
- **Reactions are server plaintext. CONFIRMED; severity low to medium (metadata plus small user content, durable).** `message_reaction(id, message_id, user_id, emoji, created_at)` (`pollis-schema/migrations/000000_baseline.sql:98-105`), inserted in plaintext by the DS (`pollis-delivery/src/messages.rs:1449`) and kept for the life of the account (`docs/metadata-retention-policy.md:124`). That links a named user to a message id and a time, durably, long after the envelope is collected, alongside an at-rest sealed sender that hides who *sent* the message. The emoji (including custom emoji ids) is user content. It is listed in the retention policy, but whitepaper §1.2's list of what Turso holds in plaintext (`docs/security-whitepaper.md:58-62`) omits it, and §1.2 says Turso "cannot recover message plaintext". It is a disclosure gap and a design-level metadata leak, not a key or ciphertext compromise. Fix: move reactions into MLS application messages (as edits and redactions already are), or at minimum disclose them in §1.2.

## 9. Open decisions

| # | Decision | Recommendation |
|---|---|---|
| D1 | Persistent account-sealed ring (Secret Key restores history on a lost-every-device recovery) | No for v1. It is Megolm-style backup: the Secret Key alone plus a dump opens all history. Revisit with the key-holder composition instead. |
| D2 | Mode in commit AAD only, or also a GroupContext `application_data` extension | Author: AAD for v1. Reviewer: the extension is **required** for v1 (Section 4.3.6, a forged DS range makes joiners leak), or v1 must forbid share-row writes by any device that did not process the enabling commit. Whether pinned openmls 0.8 (`docs/security-whitepaper.md:306`) supports custom GroupContext extensions and commit AAD is unverified. |
| D3 | Default retention | 1 year; options 90 days, 1 year, 3 years; no "forever". |
| D4 | DMs: who can enable member history | Either party, with a system row, Signal timer pattern; off by default. |
| D5 | Media retention | Shorter than text (Signal: 45 days free); attachments are the cost driver. |
| D6 | Publish per-channel archive heads in the verifiable log | Not in v1; chains catch interior omission; revisit if tail truncation matters. |
| D7 | Default for member history on new groups | Off until the fleet is mostly capable; then consider on for small groups. |
| D8 | Keep #1183 to #1187 open | Author: close #1184 to #1187 as superseded with a link here; rewrite #1183 as the optional key-holder (Section 7.4) or close. Reviewer: keep them open, labelled blocked on this decision. #1184's UX questions apply to either design. #1187's premise is partly right (Section 8.3 item 6). Closing a ticket before its replacement is accepted and specced conflicts with CLAUDE.md's one-ticket-one-PR rule, which forbids leaving work stranded. |
| D9 | Invite-link joiners: pre-wrap `S_g` to a single-use reservation secret so no holder must be online | Only for `max_uses = 1` reservations, never for shared links (a bearer link would leak history to non-joiners). |

## 10. Sources

External (some pages fetched directly; where DNS blocked a fetch, the claim is from the search-result text of the cited page):

- [MSC4268 PR] https://github.com/matrix-org/matrix-spec-proposals/pull/4268
- [MSC4268 text] https://raw.githubusercontent.com/matrix-org/matrix-spec-proposals/main/proposals/4268-encrypted-history-sharing.md
- [RoomKeyBundle docs] https://fractal-63a86d.pages.gitlab.gnome.org/matrix_sdk_crypto/types/room_history/index.html
- [MSC3061] https://git.sudo.is/matrix/matrix-doc/src/branch/main/proposals/3061-shareable-room-keys.md
- [CVE-2024-47080] https://osv.dev/vulnerability/CVE-2024-47080
- [Matrix 2024-10] https://matrix.org/blog/2024/10/security-disclosure-matrix-js-sdk-and-matrix-react-sdk/
- [Matrix Conference 2025 talk] https://media.ccc.de/v/matrix-conf-2025-72720-joining-the-conversation-balancing-privacy-with-usability-for-encrypted-messages
- [MSC4048] https://github.com/matrix-org/matrix-spec-proposals/pull/4048
- [matrix-sdk backups] https://fractal-1f518c.pages.gitlab.gnome.org/matrix_sdk_crypto/backups/index.html
- [Albrecht et al.] https://eprint.iacr.org/2023/485
- [Black Hat EU-22] https://i.blackhat.com/EU-22/Wednesday-Briefings/EU-22-Jones-Practically-exploitable-Cryptographic-Vulnerabilities-in-Matrix.pdf
- [CVE-2022-39251] https://nvd.nist.gov/vuln/detail/cve-2022-39251
- [CVE-2022-39248] https://nvd.nist.gov/vuln/detail/CVE-2022-39248
- [Matrix 2022 pre-disclosure] https://matrix.org/blog/2022/09/23/pre-disclosure-upcoming-critical-security-release-of-matrix-sd-ks-and-clients/
- [Signal blog] https://signal.org/blog/introducing-secure-backups/
- [Signal support] https://support.signal.org/hc/en-us/articles/9708267671322-Signal-Secure-Backup
- [BleepingComputer] https://bleepingcomputer.com/news/security/signal-adds-encypted-local-backup-support-to-ios-desktop-apps
- [CyberInsider 2025] https://cyberinsider.com/signal-adds-encrypting-message-history-sync-for-linked-devices/
- [SVR3 OSDI24] https://www.usenix.org/conference/osdi24/presentation/connell
- [Green 2020] https://blog.cryptographyengineering.com/2020/07/
- [WhatsApp multi-device] https://engineering.fb.com/2021/07/14/security/whatsapp-multi-device/
- [WhatsApp backups] https://engineering.fb.com/2021/09/10/security/whatsapp-e2ee-backups/
- [WeLiveSecurity] https://www.welivesecurity.com/2021/09/14/whatsapp-announces-end-to-end-encrypted-backups/
- [Wire support] https://support.wire.com/hc/en-us/articles/207834645-Why-can-t-I-see-my-conversation-history
- [RFC 9420] https://datatracker.ietf.org/doc/rfc9420/
- [mls-extensions] https://datatracker.ietf.org/doc/draft-ietf-mls-extensions/
- [appsync] https://rsync.nic.funet.fi/pub/doc/netinfo/internet-drafts/draft-barnes-mls-appsync-01.txt
- [RFC 8452 AES-GCM-SIV] https://www.rfc-editor.org/rfc/rfc8452
- [Apple security guide] https://support.apple.com/guide/security/sec2c21e7f49
- [Apple iCloud overview] https://support.apple.com/102651
- [Telegram FAQ] https://tsf.telegram.org/manuals/e2ee-simple
- [Discord forum] https://support.discord.com/hc/nl/community/posts/360046946331-Change-read-message-history-permission?page=2
- [R2 pricing] https://developers.cloudflare.com/r2/pricing
- [Turso PITR] https://docs.turso.tech/features/point-in-time-recovery

Repository (read for this design): `CLAUDE.md`; `docs/security-whitepaper.md` §1.2, §2, §5, §6; `docs/backend-core-invariants.md` (I3, I8, I9, I10, losses); `.codesight/wiki/mls.md` (pinned-message key state, epoch hooks); `pollis-core/src/db/local_schema.sql`; `pollis-core/src/commands/pinned_messages.rs`; `pollis-core/src/commands/vault.rs`; `pollis-core/src/commands/messages/{framing,edit_delete,reactions,retention}.rs`; `pollis-core/src/commands/groups/invites.rs`; `pollis-delivery/src/{messages,vault}.rs`; `pollis-schema/migrations/000000, 000012, 000013, 000014, 000019, 000020, 000027, 000028, 000031, 000032`; issues #1181 to #1187, #846, #150, #856, #1312.

## Review notes

Adversarial review against repo commit `ec22dec`. Each line is one change made above, or one unresolved concern.

### Changes made

- Summary: "sealed sender holds" now says sealed sender is at-rest only, since the live DS sees the signer (`docs/security-whitepaper.md:70`, `pollis-delivery/src/messages.rs:647-676`).
- Verdict relabelled as the author's. "Physical" prospective consent is limited to epochs before member history was enabled (Section 4.3.5).
- "Not a store today" is qualified. Durable metadata already exists: commit-log `sender_id` (`pollis-delivery/src/commit.rs:366-367`), plaintext reactions (`000000_baseline.sql:98-105`), and `pinned_by` (`pins.rs:299-303`).
- 4.3.1: PCS claim qualified, a "no FS inside retention" bullet added, and catch-up bounded by `PRUNE_MAX_BEHIND_HEAD = 512` (`pollis-delivery/src/commit.rs:600`, live via `pollis-delivery/src/lib.rs:774,878`).
- 4.3.3: `check` only detects random garbage. A well-formed fake `S'` wins the first-writer CAS, the same as `pin_keystate` (`pollis-delivery/src/pins.rs:217-230`).
- 4.3.3: the link cap of 8 per version can be exhausted by one malicious member, including for future versions (Section 5.1 `share_link_cap`).
- 4.3.4: the DB trigger is reframed as honest-DS defence in depth, not a cryptographic guarantee (CLAUDE.md "Untrusted = network, Turso, DS").
- 4.3.4 / 8.1: refuted "pre-empting a share key row is blocked". A member knows `K(c,e)`, so it can compute `kc`, and `INSERT OR IGNORE` lets it win.
- 4.3.5: since `Archived` requires `MemberHistory`, keys for pre-share epochs exist on every member device. Prospective joiner sharing is discipline, not physics.
- 4.3.6: refuted "forged range is harmless". A joiner becomes a writer and would wrap keys for unshared channels. D2 is made a v1 requirement.
- 4.3.7: the delta sealed under the account seed breaks PCS and is Secret-Key-openable in flight. The account seed is on every device and in `account_recovery` (`docs/security-whitepaper.md:101-104`).
- 4.4: added nonce analysis (GCM-SIV deterministic nonce is fine; C2 should take AAD) and exporter label separation (`pinned_messages.rs:81`).
- 4.4: added trailer gaps. The thread trailer must stay at `N` (`framing.rs:417`), attachments are unpadded (`framing.rs` `pad` doc), `Body` must be canonical, and joiners cannot run the check.
- 4.5: reaction evidence added (`pollis-delivery/src/messages.rs:1429-1449`, `docs/metadata-retention-policy.md:124`).
- 4.10: #720 healing is bounded by the 512-epoch cap. 12 months is the deployed value (`pollis-delivery/wrangler.prod.jsonc:42`, `wrangler.dev.jsonc:63`), and the code fallback is 6 (`pollis-delivery/src/messages.rs:291`). CLAUDE.md and the other agent were each half right.
- 4.11: added rows for device-to-entry linkage, ring size, edit/moderation timing and already-durable metadata, plus a recommendation to coarsen `created_at`.
- 4.13: refuted "an old DS rejects archive fields". `pollis-api` has no `deny_unknown_fields`, so the fields are silently dropped, and an explicit `archived` ack is needed.
- 4.13: added that old-build committers and approvers do not deliver `S_g` or the ring.
- 5.2: refuted "retention sweep is not a timer". `sweep_envelope_gc` runs on a tokio interval (`pollis-delivery/src/main.rs:138-151`).
- 5.6: effort estimate marked low confidence. The Release-build mobile verification CLAUDE.md requires is added.
- 6: invariants I-A2b, I-A2c, I-A9b, I-A15b and I-A16 added. I-A2 relabelled as an honest-DS property.
- 7.1: comparison table corrected (status-quo durability, blast-radius scope, device count, keeper authenticity fixability, 512-epoch completeness, protocol change, search, prospective nuance, PQ only on the PQ suite).
- 7.2: keeper steelman expanded (old-client coverage, no wire change, no retained epoch secrets, customer-controlled storage, search, fixable authenticity).
- 7.3: added the vault precedent (`pollis-core/src/commands/vault.rs:1-16`) and the reviewer's own answer.
- 8.3 item 6: #1187 is "overstated, not wrong". Long outages in busy groups lose messages via the 512 cap. Self-update cadence is about weekly per device per group (`docs/security-whitepaper.md` §6.7).
- 8.4: pin forgery CONFIRMED (medium) and reactions plaintext CONFIRMED (low to medium), with file:line evidence. A possible `Kpin` fork is noted as unverified.
- D2 and D8 carry the reviewer's dissent.

### Unresolved concerns

- Blast radius: the store moves "full shared-channel history" exposure from 1 to 2 keeper hosts to every member device, plus a dump. For member history it extends a device's exposure from its lookback window (#150) to the archive retention (up to 3 years), across all of the account's devices.
- `archive_epoch_key` caches H_e per group, not per channel. A stolen device opens every channel of every archived group within retention, a durable superset of what it holds locally.
- There is no deletion rule for `H_e` caches when member history is turned off, and no epoch-to-time mapping for the client-side retention purge (Section 4.10).
- Device certs and revocation: a later reader verifying an entry from a since-revoked device may get "could not be verified" under `leaf_verdict` rules (whitepaper §5.3). Historical-cert semantics are unspecified.
- Ring snapshot size: up to 4 MiB (`history_ring_transfer.blob`). A user in many large groups over 3 years can exceed it (about 32 to 50 B per group-epoch). Chunking is unspecified.
- Admin tombstones are DS-authored and unsigned (Section 4.5), so a DS that forges roles can censor history for joiners as "removed by an admin". This is the existing role-trust limitation, made durable.
- DM member history "either party enables" archives the other party's messages on Pollis servers for up to a year on one party's say-so. That consent question is not resolved (D4).
- The per-message ML-DSA-44 signature (2420 B) dominates entry size. Signing per-sender chain heads or batches is not evaluated.
- Turso cost is unquantified. `archive_entry.blob` lives in Turso rows, not R2, so the R2 price quoted in 7.1 is not the relevant one until P7 packing.
- openmls 0.8 at a pinned `main` revision (`docs/security-whitepaper.md:306`): whether it supports AAD on commits and custom GroupContext extensions is unverified. D2 depends on it.
- External citations (MSC4268 merge and spec dates, CVE-2024-47080 fix version, Turso PITR 30 days, R2 price) were not re-fetched by the reviewer and remain as cited.
- Signal-style sibling device transfer, for #1185 on the approval path with no durable server store, is not evaluated as an option.
- `pin_keystate` first-writer-wins CAS: a malicious member may fork `Kpin` for later joiners (`pollis-delivery/src/pins.rs:217-230`). This is unverified by test and worth its own ticket alongside pin signing.
- Reactions (`message_reaction`) are missing from whitepaper §1.2's plaintext list (`docs/security-whitepaper.md:58-62`). File a ticket regardless of this proposal.
- Whitepaper §6.8 (`docs/security-whitepaper.md:393-395`) and CLAUDE.md loss (1) are already inaccurate because of pins. Correct them regardless of this proposal.

### Reviewer verdict

I do not agree with the verdict as written. The store is a credible design for joiner sharing (#1186). Its sender-signed, per-entry, exporter-keyed core is sound, and it beats a keeper that replays unsigned re-sends. But four flaws in the draft are exploitable by a single malicious member or a malicious DS:

- forged DS ranges make joiners leak unshared channels;
- share-key rows can be squatted;
- link slots and the keystate wrap can be squatted or forked;
- account-seed-sealed deltas break PCS.

Each has a fix, and the doc's headline guarantees ("unrepresentable at the database", "prospective by construction, physically", "PCS is MLS PCS") were stronger than the design delivers. The comparison was also tilted. The keeper's authenticity gap is fixable by forwarding signed MLS content. The keeper is sender-agnostic through the multi-year mobile long tail, needs no wire change, and adds no metadata regression. #1187's premise survives for long outages because of the 512-epoch commit cap.

What I would change:

1. Do not close #1184 to #1187 yet; mark them blocked on an owner decision.
2. Serve #1185 first with Signal-style sibling transfer at enrollment, which needs no durable server store.
3. If the owner accepts the durable per-channel timeline in writing (with day-coarsened timestamps and disclosure in the ambient marker), build the store for #1186 only, with D2 mandatory and the four fixes plus the new invariants in Section 6.
4. Separately ticket pin signing and the reactions disclosure.
