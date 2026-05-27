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
banner up immediately. It also broadcasts via `publish_to_room_server` so
other already-connected room members see the same banner without a refetch.
New joiners don't see banners for themselves (the Welcome path doesn't go
through this hook), and the frontend filters self-actions defensively.

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
| Turso swaps a peer's `account_id_pub` | Next DM ingest OR next group reconcile via TOFU helper | Pin refreshed, verified cleared, KeyChanged event surfaces inline banner |
| Turso adds a rogue device under an existing user (account-key unchanged) | Cross-signing check on inbound MLS commit (`mls.rs`) | Logs warning; commit currently proceeds (advisory — known gap, see whitepaper §13.2) |
| Turso adds a rogue device under a *swapped* account-key | Both layers fire: TOFU detects the key swap; cross-sign detects the cert mismatch | Banner + warning |
| Network MITM between two clients (no Turso write) | MLS cipher integrity (AES-128-GCM AEAD) | Decryption fails — cannot impersonate anyone |
| Local DB tampering on attacker's own machine | Out of scope — that machine's user can do whatever they want to their own DB |

## Roadmap

- **Key transparency** (CONIKS / Signal-KT style append-only log). The
  scalable fix for an untrusted server swapping `account_id_pub` is an
  auditable log that one or more auditors watch globally, so individual
  users don't have to manually compare numbers. Tracked in #277 as a
  separate piece of work, not a quick follow-up.
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
