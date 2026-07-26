# TLA+ specs — machine-checked correctness

Abstract TLA+ models of the Pollis control-plane / delivery invariants, checked
exhaustively by TLC over small configurations. These are the design-level
complement to the Kani proofs on the real Rust pure functions
(`pollis-core/src/commands/messages/watermark.rs`,
`pollis-core/src/commands/mls/invariants.rs`). See
[`docs/machine-checked-correctness-design.md`](../../docs/machine-checked-correctness-design.md)
§3 for the coverage rationale.

## Specs

| File | Spec | Invariants | Status |
|---|---|---|---|
| `CommitLog.tla` + `CommitLog.cfg` | **Spec A — CommitLog / epoch machine** | I1 (`OnePerEpoch`, `Gapless`, `LineageMonotone`, `OpeningClosesTheHead`) + I2 (`NoForeignAdopt`) | authored + TLC-checked |
| `Delivery.tla` + `Delivery.cfg` | **Spec B — Delivery / retention** | I3 (`NoLossForCurrentMember`, `CursorMonotone`) + I4 (retention floor) + `AcceptedLossesOnly` | authored + TLC-checked |

## Running TLC

Fast, headless, JVM-only. From the repo root:

```bash
scripts/tlc-check.sh            # both sound specs — all invariants must PASS
scripts/tlc-check.sh --broken   # every teeth config — each must produce a counterexample
```

The script downloads a pinned `tla2tools.jar` if absent and needs only a JRE
(`java` on `PATH` or `JAVA_HOME`). A third party can re-check the `.tla`/`.cfg`
files with the public [TLA+ tools](https://github.com/tlaplus/tlaplus) — nothing
here is Pollis-specific tooling.

## CommitLog (Spec A) at a glance

State: per-key append-only `log` of commits (`[gen, epoch, seq, author, closes]`,
`seq` a globally-unique byte-identity nonce), per-`(key, client)` `localGen` and
`localEpoch` (which lineage the client is on and how far it has applied), the
`member` I5 gate flag, `adopted` (the `seq` a client installed at each
`(gen, epoch)`, for `NoForeignAdopt`), and `pending` (a migrator's prepared-but-
unlanded opening commit). Actions: `Submit` (the DS `submit_commit`
conditional-insert — append at the head of the client's own generation iff
`based_on = head`, else the stale client is rejected and must catch up), `Apply`
(replay the next commit, the `classify` decision), `ExternalJoin` (recovery jump
to the head, gated by `member`), `Remove` (eviction), and the #454 P4 suite
migration split into `MigratePrepare` / `MigrateCommit` / `MigrateAbandon` so the
window between reading the old head and landing the successor is a real
interleaving point, plus `AdoptGeneration` (a device hops to the successor).

Invariants:

- **`OnePerEpoch`** (I1): no two distinct commits share an epoch *within a
  generation* — no fork.
- **`Gapless`** (I1): within each generation present, the epochs are exactly
  `0..Head-1`, no hole.
- **`LineageMonotone`** (I1): the `(generation, epoch)` head never decreases
  lexicographically — append-only, across migrations as well as within one.
- **`OpeningClosesTheHead`** (I1, #454 P4): a commit opening generation `g > 0`
  at epoch 0 names a `closes` epoch strictly above every epoch in generation
  `g - 1` — the compare-and-swap that makes a lineage's start unforgeable and
  stops a migration orphaning a commit that landed on the predecessor.
- **`NoForeignAdopt`** (I2): a commit a client adopted at `(g, e)` byte-equals
  (`seq`-equals) the log's commit at `(g, e)` — no phantom epoch, no fork
  adopted.

The DS enforces exactly two accepting branches, and `pollis_delivery::commit::
accepts()` (Kani-proved) is their Rust image: continue the head lineage at
`head + 1`, or open generation `N+1` at epoch 0 having closed the old lineage's
current head.

**Teeth.** Two mirror-image negative configs, each isolating one guard:

- `CommitLogBroken.cfg` flips `SoundSubmit` to `FALSE`, dropping the
  conditional-insert guard so a stale client appends a second commit at an
  already-occupied epoch. TLC reports a concrete `OnePerEpoch` counterexample
  (two clients landing distinct commits at one epoch).
- `CommitLogMigrateBroken.cfg` flips `SoundMigrate` to `FALSE`, dropping the
  migration compare-and-swap so a migrator may open the successor naming a head
  epoch that has since moved on. TLC reports an `OpeningClosesTheHead`
  counterexample in which a racing `Submit` is orphaned on the abandoned
  lineage — note both lineages stay individually well-formed, which is why
  "the generation must increase" is not sufficient on its own.

## Delivery (Spec B) at a glance

State: per-key ordered `msgs` (`[epoch, sentAt]`), per-`(key, device)` `cursor`
(the delivery watermark), the current `member` set with per-device `joinEpoch`
and replayed `replay` epoch, `delivered` (decrypted sentAts), and the retention
`gcFloor`. Actions: `Commit` (advance epoch), `Send`, `Join`/`Leave`,
`ReplayCommit`, `Advance` (the watermark step, abstracting `next_watermark`), and
`GC` (raise the retention floor).

Invariants:

- **`NoLossForCurrentMember`** (I3 + I4, anti-F3): retention never removes a
  message a current member-device still needs.
- **`CursorMonotone`**: cursors never regress.
- **`AcceptedLossesOnly`**: a device has decrypted `m` only if continuously
  present since `m`'s epoch — exactly the two accepted losses (pre-join
  messages; a fresh device starts empty), nothing weaker.

**Teeth.** `DeliveryBroken.cfg` flips `SoundGC` to `FALSE`, guarding the
retention floor by the *fastest* member instead of the slowest. TLC then reports
a concrete `NoLossForCurrentMember` counterexample, proving the invariant is not
vacuously true.

**Suite generations (PQ hybrid MLS, #454 P4).** Spec B keeps `Keys` a singleton,
and that is deliberate: read `k1` as one *lineage*, a `(conversation, generation)`
pair. An earlier note here promised the PQ extension would be "a config change,
not a spec rewrite" — that was wrong. Spec B has no action and no invariant that
relates two keys (every variable is `[Keys -> …]`, every invariant `\A k \in
Keys`, the only shared state the global `clock`), so a two-key config is a
product of two independent copies: sound, but it proves nothing the one-key
config already proves, at >10 minutes of TLC (measured) instead of seconds. The
safety content of P4 is the *relationship between* lineages, so it lives in Spec
A, which was rewritten for it. Retention inside a lineage is untouched by P4
because a successor restarts at epoch 0 on a fresh chain; the generation-aware
retention floor (`(generation, since_epoch)`, so a prune cannot strand a device
still catching up on a retired lineage) belongs to #539's commit-log pruning and
should be modelled against that code.
