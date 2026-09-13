# State-derivation audit (repeatable scan)

How to run: ask an agent to "run the state-derivation audit in docs/state-derivation-audit.md" (or to re-verify the epic "State-derivation audit: MLS / membership / session state"). The agent follows this file verbatim, reads the code, and produces a report only. Tracking epic: GitHub issue #1078. Baseline from the first run: `docs/state-derivation-audit-baseline-2026-09-13.md`.


# State-derivation audit

A repeatable, read-only scan of every stateful structure that represents group,
membership, or session state (or a view derived from it), classifying each as
pure-derived, wrongly-incremental, or legitimately-incremental, and checking
whether incremental state can silently drift.

The first run of this scan is the epic "State-derivation audit: MLS / membership /
session state" (search the issue tracker for the `epic` label and that title).
Its baseline table is copied in `docs/state-derivation-audit-baseline-2026-09-13.md`. A re-run
must produce the same table shape and diff itself against the baseline.

## Rules

- Read-only. Do not modify, refactor, or "fix" any code. Report only.
- Cite every finding with `file:line` and the function signature. Assert nothing
  you did not read in the source. Record the commit SHA the line numbers refer to
  (`git rev-parse --short HEAD`).
- Pull the pinned openmls rev from the workspace `Cargo.toml` (`openmls = { git = ..., rev = ... }`)
  and read it before claiming any openmls semantics (a shallow `git fetch --depth 1 origin <rev>`
  into the scratchpad works). Never quote openmls behaviour from memory.

## Buckets

- **A. Should be pure-derived and is.** `fn compute_x(current_inputs) -> X`,
  rebuilt from the full current input set on every use.
- **B. Should be pure-derived but is maintained incrementally.** Live state is a
  running sum of diffs (`on_added { insert } / on_removed { remove }`); correctness
  depends on catching every case in the right order. This is the bug surface.
  Show the mutation sites and say what should be recomputed instead.
- **C. Legitimately incremental by design.** The MLS ratchet tree's cryptographic
  state (node/path secrets, key schedule) AND the tree structure itself (leaf
  assignment, blanking), plus any other state the spec makes history-dependent
  (pending commits, suite-generation pointer, self-update clock, TOFU pins).
  Do NOT flag these as anti-patterns. Verify the incremental logic matches the
  spec and flag any place the public state could silently diverge from what the
  operation history implies.

## Step 1: inventory

Walk every file below and list each stateful structure: type, definition site,
and every function that writes it. The list is the minimum; add anything new
`grep` turns up.

Client (`pollis-core/src`):
- `commands/mls/reconcile.rs` (roster diff, publish/resolve, pending-commit rollback)
- `commands/mls/group_state.rs` (replay, apply, recovery gates, GroupInfo heal, encrypt/decrypt)
- `commands/mls/invariants.rs` (pure Kani-proved decision cores)
- `commands/mls/generation.rs`, `self_update.rs`, `welcomes.rs`, `sweep.rs`, `migrate.rs`, `delivery.rs`, `provider.rs`, `device.rs`, `key_packages.rs`
- `signal/mls_storage.rs` (openmls StorageProvider over `mls_kv`)
- `commands/messages/{seal,send,edit_delete,receipts,ingest,watermark,read_state}.rs`
- `commands/{voice_e2ee,pinned_messages,safety,dm}.rs`, `commands/groups/*.rs`
- `commands/livekit/{realtime,participants,identity}.rs`, `realtime.rs`, `state.rs`
- `db/local_schema.sql` (`mls_kv`, `mls_self_update`, `mls_generation`, `pin_key_cache`, `read_cursor`, `message_receipt`)

Server (`pollis-delivery/src`):
- `commit.rs` (head arithmetic, CAS, retention floors, `mls_commit_since`)
- `directory.rs` (`desired_roster`, catch-up head ordering, the `user_groups`/`user_dms` note)
- `writes.rs` (`is_member`), `groups.rs`, `profile.rs`, `account.rs`, `teardown.rs`, `messages.rs` (watermark, roster parity test)
- `pollis-schema/migrations/` (membership tables, `000009` directory index, `000016`/`000017` conversation registry)

Other: `pollis-tui/src/home.rs` (sidebar rebuild), `net/overlay.rs` (relay pool refetch).

Useful greps (run from the repo root):

```bash
grep -rn 'merge_pending_commit\|clear_pending_commit\|pending_commit()' pollis-core/src --include=*.rs
grep -rn 'mls_group_lock' pollis-core/src --include=*.rs
grep -rn 'try_mls_encrypt\|MlsDecryptor::open\|load_group_with_signer' pollis-core/src --include=*.rs
grep -rn 'INSERT INTO group_member\|DELETE FROM group_member\|dm_channel_member\|group_invite' pollis-delivery/src
grep -rn 'unchecked_transaction\|\.transaction()' pollis-core/src --include=*.rs
grep -rn 'x3dh\|double.ratchet' --include=*.rs -il .
```

## Step 2: classify

One bucket per structure, with evidence. For B, show the mutation sites and the
recompute that should replace them.

## Step 3: derivation properties (A and B)

For each: determinism (same inputs, same output), order independence (does the
result depend on event arrival order, and should it), idempotence (does applying
the same input twice change it), hidden reads (does producing the new state read
the previous derived state). Hidden reads are the smell for A/B; expected for C.

## Step 4: drift checks (B and C)

For every incremental structure: is there an assertion, invariant check, test, or
reconciliation that would catch it diverging from a full recompute or from the
spec? If not, say so explicitly. Specifically re-check these known drift points
from the baseline:

1. Every `merge_pending_commit` call site: is it inside `publish_staged_commit` /
   `apply_one_commit`, or under the per-conversation `mls_group_lock` after an
   ingesting catch-up? Any site reachable without the lock (the baseline found
   `try_mls_encrypt` via `receipts::emit_receipt`) is a drift point.
2. The commit add-metadata (`added_user_id`, `added_device_ids`): is the receiver's
   cert verification driven by the full `(user, device)` add set, or by one user id?
3. Leaver eviction: after `leave_group` / `leave_dm_channel`, who commits the
   removal and when? Is it only the cold-launch sweep backstop?
4. `registered_devices` returning an empty set: is there a client-side sanity check
   (e.g. the actor's own device must be present) before the diff treats "empty" as
   "revoke everyone"?
5. `sweep::local_tree_has_stale_leaf` vs `reconcile::desired_set`: one rule or two copies?
6. The `RosterChanged` banner diff: snapshot-plus-diff or a fresh tree walk after merge?
7. `user_groups` / `user_dms`: still written-to-but-never-read?

## Output

1. A table with one row per structure: `#`, structure, type/where, every writer,
   bucket, verdict + evidence (`file:line`). Keep the row numbering from the
   baseline where the structure still exists; append new rows at the end.
2. A "Derivation properties and drift checks" section.
3. A ranked "Highest-risk findings" section, with a concrete recommendation for
   every B item and correctness notes only for C.
4. A "Diff against baseline" section: for each baseline row, `unchanged`,
   `fixed` (cite the fix), `regressed`, or `new`. Cross-reference the epic's
   sub-issues by number when a row corresponds to one.

Deliver the report in the terminal reply (and, if asked, as a comment on the epic).
Do not open PRs or edit code as part of this audit.
