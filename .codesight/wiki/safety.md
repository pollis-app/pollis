# Safety Numbers & TOFU Pinning

Signal-style contact verification layered on top of MLS. The cryptographic anchor
for *who someone is* in Pollis is `users.account_id_pub` (32-byte Ed25519,
per-user, never rotates unless the user resets their identity). Every device
cert chains to it, so verifying that one value out-of-band transitively covers
every device the user owns now or in the future.

Turso is untrusted. The server can write any row, including swapping a peer's
`account_id_pub` for one it controls — at which point MLS would happily admit
the attacker's device into a group, because MLS only checks that the *device*
cert signs against the `account_id_pub` it's currently told to use. The defence
against this is at a layer above MLS: Trust-On-First-Use pinning of the
`account_id_pub` itself, with a comparable safety number for explicit
human verification.

## Surfaces

### Safety number derivation

`pollis-core/src/commands/safety.rs::get_safety_number`. 60 decimal digits,
displayed as twelve 5-digit blocks. Per-user 30-digit fingerprint:

```
fingerprint(account_id_pub) = SHA-512^5200 over (version || pubkey || stable_id)
  → take 6 disjoint 5-byte windows → each rendered as 5 decimal digits
```

Iteration count matches Signal's `NumericFingerprintGenerator` default — slow
on purpose so brute-forcing against a truncated display is infeasible.
`FP_VERSION = 0`. Bump it if the derivation ever changes so old and new clients
never display matching numbers under different schemes.

The combined number is the sorted concatenation of both parties' 30-digit
fingerprints. Order independence is enforced by sorting before joining, so
Alice asking about Bob and Bob asking about Alice produce the byte-identical
string. QR payload is `hex(sorted_pubkeys).join(":")` so a scanner can verify
without branching on "is this my key or theirs."

### TOFU pin store

Local DB table `contact_verification`:

| Column | Notes |
|---|---|
| `peer_user_id` (PK) | Pin is per-USER, not per-conversation. Verifying once propagates everywhere. |
| `account_id_pub` (BLOB) | The pinned key — what we compare future server values against. |
| `identity_version` (INTEGER) | Server's claimed version at pin time. Bumps on account reset. |
| `verified` (INTEGER 0/1) | User explicitly compared safety numbers out-of-band. |
| `updated_at` | Touched on every pin/update for audit. |

The local user's own row is never inserted — `batch_check_and_pin_account_keys`
explicitly skips `actor_user_id`.

### Detection hooks

Two call sites guarantee every conversation kind is covered:

1. **DM message ingest** — `pollis-core/src/commands/messages/ingest.rs:317`
   calls `check_and_pin_account_key` for each peer in the DM on every incoming
   message. Single-peer, single-Turso-query.

2. **Group MLS reconcile** — `pollis-core/src/commands/mls/reconcile.rs` calls
   `batch_check_and_pin_account_keys` after the roster fetch and before the
   device-pair fetch (around the `1b.` numbered step). One Turso query for the
   entire roster, batched for performance — a 50-person group costs one
   round-trip, not 50. Refs #277 closed the historical hole where only DMs ran
   TOFU and groups had no automatic detection.

### Mismatch behaviour

On detected mismatch, the helper:

1. `UPDATE contact_verification SET account_id_pub = ?, identity_version = ?,
   verified = 0, updated_at = datetime('now')` — the pin is refreshed in place
   to the new key, and the verified flag is cleared.
2. Emits `RealtimeEvent::KeyChanged { peer_user_id, peer_identity_version }`
   on the local sink. Frontend listeners (`useLiveKitRealtime.ts` →
   `useKeyChangeStore`) flag the peer, invalidate the
   `peerVerificationKeys.all` query, and render the inline banner in every
   open conversation.
3. Returns success. **Sends are not blocked.** The policy is
   advisory-with-acknowledge: the banner is the user-facing signal; the user
   chooses whether to re-verify before continuing. Hard-blocking a 50-person
   group on one rotated key would wreck UX without adding security (the
   warning is the security).

The user-visible signal in the UI is the cleared `verified` flag, not the
`key_changed` boolean from `list_peer_verifications`. The boolean only fires
when the local pin is *stale* (server moved, local hasn't synced yet); after
the helper runs, both sides match and the boolean returns to false. The
cleared verified flag is what persists as evidence the swap happened.

## Verification UX

- Profile page (`/user/$userId`) — safety number display + Verify button.
  Reachable from DMs (avatar click), from group member lists (row click — wired
  in `frontend/src/pages/Members.tsx`), and from channel author labels.
- Shield-check (verified) / shield-alert (key changed) badges render in the
  same component shape across surfaces:
  - DM sidebar (`Sidebar.tsx`)
  - Group member roster (`Members.tsx`)
  - Channel author labels — wherever a peer's name appears
- All three read from the same `usePeerVerifications` query so the cache is
  one source of truth.

## Roster-change banners

Separate from key-change banners. When `reconcile_group_mls_impl` produces a
non-empty commit (epoch bump + at least one add/remove), it emits
`RealtimeEvent::RosterChanged` with a user-level diff:

```rust
RosterChanged {
    conversation_id, epoch_before, epoch_after,
    joined_user_ids: Vec<String>,
    left_user_ids: Vec<String>,
    devices_added: Vec<(user_id, device_id)>,
    devices_removed: Vec<(user_id, device_id)>,
}
```

The diff is computed in the reconcile against the pre-state `already_in_tree`
snapshot: a user with no prior leaves who gains one is a *join*; a user with
prior leaves who gains one is a *device add*; the inverse for removes.

Locally the reconciler emits via the LiveKit sink so its own UI picks the
banner up immediately with the full per-user diff. It also broadcasts via
`publish_to_room_server` so other already-connected room members refetch — but
that cleartext `roster_changed` wake-up ping now carries **only** the
`conversation_id` + `epoch_before`/`epoch_after`, **not** the
`joined`/`left`/device-id lists (metadata minimization #331 v2 §5.3,
`livekit_signalling.rs::roster_changed_payload`). Remote peers re-derive the diff
from the authenticated MLS commit + a member refetch rather than trusting a
cleartext identity list on the wire. New joiners don't see banners for themselves
(the Welcome path doesn't go through this hook), and the frontend filters
self-actions defensively. See [mls.md](./mls.md#metadata-minimized-signalling-331-v2).

Renderer plumbing:

- `frontend/src/stores/rosterChangeStore.ts` — per-conversation queue of
  `RosterBanner` items, capped at 200 to bound memory.
- `frontend/src/hooks/useLiveKitRealtime.ts` (search for `roster_changed`) —
  parses the event, splits into per-user banner records, pushes to the store,
  invalidates `groupQueryKeys.members(conversation_id)`.
- `frontend/src/components/Message/MessageList.tsx` — interleaves banners with
  messages by wall-clock timestamp, renders inline as a centered divider
  (same shape as the existing `DayDivider`).

## Threat model

| Attack | Detection | Action |
|---|---|---|
| Turso swaps a peer's `account_id_pub` | Next DM ingest OR next group reconcile via TOFU helper; permanently visible in the account-key transparency log (#330) | Pin refreshed, verified cleared, KeyChanged event surfaces inline banner; swap is absent from / accountable in the published log (`pollis-verify account`, `audit_peer_account_key`) |
| Turso adds a rogue device under an existing user (account-key unchanged) | Cross-signing check on inbound MLS commit (`mls.rs`) | Logs warning; commit currently proceeds (advisory — known gap, see whitepaper §13.2) |
| Turso adds a rogue device under a *swapped* account-key | Both layers fire: TOFU detects the key swap; cross-sign detects the cert mismatch | Banner + warning |
| Network MITM between two clients (no Turso write) | MLS cipher integrity (AES-128-GCM AEAD) | Decryption fails — cannot impersonate anyone |
| Local DB tampering on attacker's own machine | Out of scope — that machine's user can do whatever they want to their own DB |

## Key transparency (verifiable logs)

Implemented in #330 (was the top Roadmap item below). Append-only,
Ed25519-signed Merkle trees (RFC 6962/9162) published at
**https://verify.pollis.com**, domain-separated by STH context so a head for one
tree can never stand in for another:

- **Commit-log tenant** (`pollis-verifiable-log:sth:v1`) — one leaf per MLS
  commit. Closes server-side fork / epoch-regression / replay on the MLS commit
  stream: replaying under its invariant proves no two commits share a
  `(conversation_id, epoch)` and epochs only increase. See `docs/transparency.md`.
- **Account-key tenant** (`pollis-verifiable-log:sth:v1:account-keys`) — one leaf
  per account identity-key version, sourced from the append-only
  `account_key_log` table (dual-written with `users.account_id_pub` at signup and
  `reset_identity`). Closes selective targeting + key-history accountability: a
  user's whole key history is publicly checkable and `identity_version` is proven
  monotonic, so a server-swapped `account_id_pub` can no longer hide — it is
  either absent from the published history (caught) or a visible, accountable
  rotation.

- **Binaries tenant** (`pollis-verifiable-log:sth:v1:binaries`, #453) — one leaf
  per released build artifact on the SAME log infrastructure, a third independent
  tenant with its own tree and domain-separated STH context (so a binaries head
  can never be presented as a commit-log or account-key head). Each leaf commits
  to an artifact's reproducible pre-signature payload hash + its signed sha256
  (`BinaryRecord` / `BinaryInvariant` in `verifiable-log-builder`). The release
  pipeline attests-and-logs every artifact; `pollis-verify release <tag>` verifies
  that every published artifact for a tag is provably included in the signed
  binaries tree at verify.pollis.com (`/v1/binaries`).

  **Honest scope.** This is P2 — leaf structure, hashes, and pipeline wiring. It
  is **not** full bit-for-bit reproducibility, cosign, or in-app verification;
  those are tracked in #484. Logging that a signed artifact was published is a
  transparency/accountability property, not (yet) a proof that the artifact was
  built from the published source.

This is the scalable backstop the TOFU layer above always wanted: TOFU catches a
swap only on the next message and only for keys *this* device has seen; the log
makes every user's full history auditable by anyone.

### Client integration

`pollis-core/src/commands/transparency.rs`:

- `self_audit_account_key` — verifies OWN published key history, reusing the same
  `verifiable_log_serve::account::verify_account` the `pollis-verify` CLI runs
  (no re-implementation), and compares the chain's latest version to this
  device's current key.
- `audit_peer_account_key` — the same against a TOFU-pinned peer's pinned key:
  pinned key present in the published history → `ok` (notes a newer version as a
  rotation); pinned key absent from a verifying history → `alarm` (the server
  showed a key it never published).

Statuses are `ok` / `pending` / `alarm` / `unavailable`. The log's public key is
pinned in the client; a served key ≠ the pin is a hard `alarm` (any key can sign
a self-consistent forged tree). All checks are **advisory** — they alert, they
never block a send, the same policy as the key-change banner above.

**UI surfacing.** Both commands are wrapped by React Query hooks in
`frontend/src/hooks/queries/useTransparency.ts` (`useSelfAuditAccountKey`,
`usePeerAuditAccountKey`) and rendered by the shared, deliberately quiet
`components/Security/AccountKeyAuditLine.tsx` — a single advisory status line
(no modal, no blocker), warning-toned only on `alarm` (amber, where it appends
the report's reason). The **peer** audit shows on the user profile page
(`pages/UserProfile.tsx`, `/user/$userId`) beside the safety number; the
**self** audit shows in its own "Account key" section on the security page
(`pages/SecurityPage.tsx`).

### Auditing infrastructure

`pollis-verify account <user_id>` (released CLI), precomputed
`/verify/account/<id>` reports, a CI post-publish self-audit, and an equivocation
tripwire comparing each new head against the previously-published one.

### Honest limits

Daily publish lag (a rotation is invisible until the next build — `pending`, not
alarm); advisory-only client checks; enumerable `user_id`s / keys (no VRF private
lookups — keys are public by design; VRF is the upgrade path); single first-party
log + auditor (anyone can run their own via the released `pollis-verify`);
CI/GitHub in the publishing TCB (signing key in Actions secrets). Full
threat-model writeup: whitepaper §6.9 / §13 item 10.

## Account reset (pre-enrollment soft reset, #492)

A user who has lost their Secret Key resets from a **pre-enrollment** device — one
sitting on the login/OTP gate that has no device signing key yet, so it cannot
produce a signature the DS would accept. The identity-reset writes it drives —
`POST /v1/account/rotate-identity` (bump `identity_version`, append to
`account_key_log`, rewrap `account_recovery`), `POST /v1/account/reset-recover`
(the membership/device wipe), and the accompanying `POST /v1/welcomes/purge` —
therefore accept **either** a device signature **or** a verified-OTP session
(`gate_or_session` in `pollis-delivery`). Every op stays SELF-scoped: the target
user is bound from the session record, never re-derived from a client-supplied id.
The `account_key_log` append is still CAS-guarded (one head per user, no fork/gap),
so a reset produces a visible, accountable rotation in the account-key tenant
above rather than a hidden key swap. Properties: `pollis-delivery/tests/reset_session.rs`.

## Roadmap

- **VRF private lookups for key transparency.** The shipped log (#330, see "Key
  transparency" above) is enumerable by design — `user_id`s and public keys are
  listable, since the keys are public anyway. A CONIKS / Signal-KT-style
  VRF-backed private-lookup layer would hide the user set and rotation cadence
  while keeping the same auditability. This is the remaining future upgrade, not
  a quick follow-up.
- **Hard-block-on-mismatch policy.** Today's stance is advisory across
  the board. A user-configurable per-conversation "block sends until
  acknowledged" mode is an open product call — would make sense for
  high-stakes DMs, would be UX-hostile in large groups.
- **Member/device-change event richness.** The current `RosterChanged`
  event covers join / leave / device add / device remove. Future
  additions could include explicit "role changed" or "owner transferred"
  events, but those go through other code paths today.

## Tests

`src-tauri/tests/flows/auth.rs`:

- `safety_number_lifecycle_and_key_change_detection` — full DM lifecycle:
  pin via DM, verify, swap on Turso, observe `status="changed"` on
  `get_safety_number`.
- `group_reconcile_tofu_detects_key_swap` — same attack via the group
  path: create group → invite → swap server-side → trigger reconcile
  with a second invite → assert `verified` flag was cleared and the pin
  was refreshed in place. This is the test that locks in the #277 fix.
