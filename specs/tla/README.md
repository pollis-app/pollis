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
| `CommitLog.tla` + `CommitLog.cfg` | **Spec A — CommitLog / epoch machine** | I1 (`OnePerEpoch`, `Gapless`, `HeadMonotone`) + I2 (`NoForeignAdopt`) | authored + TLC-checked |
| `Delivery.tla` + `Delivery.cfg` | **Spec B — Delivery / retention** | I3 (`NoLossForCurrentMember`, `CursorMonotone`) + I4 (retention floor) + `AcceptedLossesOnly` | authored + TLC-checked |

## Running TLC

Fast, headless, JVM-only. From the repo root:

```bash
scripts/tlc-check.sh            # both sound specs — all invariants must PASS
scripts/tlc-check.sh --broken   # both teeth configs — each must produce a counterexample
```

The script downloads a pinned `tla2tools.jar` if absent and needs only a JRE
(`java` on `PATH` or `JAVA_HOME`). A third party can re-check the `.tla`/`.cfg`
files with the public [TLA+ tools](https://github.com/tlaplus/tlaplus) — nothing
here is Pollis-specific tooling.

## CommitLog (Spec A) at a glance

State: per-key append-only `log` of commits (`[epoch, seq, author]`, `seq` a
globally-unique byte-identity nonce), per-`(key, client)` `localEpoch` (how far
the client has applied), the `member` I5 gate flag, and `adopted` (the `seq` a
client installed at each epoch, for `NoForeignAdopt`). Actions: `Submit` (the DS
`submit_commit` conditional-insert — append at the head iff `based_on = head`,
else the stale client is rejected and must catch up), `Apply` (replay the next
commit, the `classify` decision), `ExternalJoin` (recovery jump to the head,
gated by `member`), and `Remove` (eviction).

Invariants:

- **`OnePerEpoch`** (I1): no two distinct commits share an epoch — no fork.
- **`Gapless`** (I1): the epochs present are exactly `0..Head-1`, no hole.
- **`HeadMonotone`** (I1): the head epoch never decreases — append-only.
- **`NoForeignAdopt`** (I2): a commit a client adopted at epoch `e` byte-equals
  (`seq`-equals) the log's commit at `e` — no phantom epoch, no fork adopted.

**Teeth.** `CommitLogBroken.cfg` flips `SoundSubmit` to `FALSE`, dropping the
conditional-insert guard so a stale client appends a second commit at an
already-occupied epoch. TLC then reports a concrete `OnePerEpoch` counterexample
(two clients landing distinct commits at one epoch), proving the invariant is not
vacuously true.

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

**Forward-compat (PQ hybrid MLS).** Everything is keyed by an abstract
`k \in Keys`. The PQ program's `(conversation, generation, epoch)` lineage is
modelled by enlarging `Keys` — a config change, not a spec rewrite
(design doc §3 note).
