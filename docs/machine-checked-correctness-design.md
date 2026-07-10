# Machine-Checked Correctness — Design

> **Status:** Partly shipped. **M0–M3 are SHIPPED** — supply-chain gating
> (`cargo-deny` + `cargo-vet`, `deny.toml` / `supply-chain/`); the Kani watermark
> proofs on the real production `next_watermark`
> (`pollis-core/src/commands/messages/watermark.rs`, I3); the Kani gate +
> canonicalization proofs `may_rejoin` / `resolve` / `classify`
> (`pollis-core/src/commands/mls/invariants.rs`, I5/I2/I1), each paired with a
> `should_panic` mutant certifying teeth; and the nightly `schedule:` marathon
> soak with one-step failing-op-sequence → regression promotion. **M4/M5 have now
> landed (#481):** TLA+ exhaustive models for both the delivery/retention machine
> (Spec B, `specs/tla/Delivery.tla`, I3+I4) and the commit-log epoch machine
> (Spec A, `specs/tla/CommitLog.tla`, I1+I2) — each sound + a refuting teeth
> config, gated in `.github/workflows/tla.yml` — plus Track-B `cargo-fuzz` targets
> over the pure fns in a detached `fuzz/` crate (nightly, out of band). Only
> optional OSS-Fuzz onboarding remains open. Governed by
> [`backend-core-invariants.md`](backend-core-invariants.md) ("invalid states are
> unrepresentable") and composes with the verifiable-builds / transparency work
> ([`transparency.md`](transparency.md)). Scope: the Pollis MLS control-plane and
> delivery/retention state machine (`pollis-core`, `pollis-delivery`, the remote
> schema). Zero user burden — this is internal assurance whose *artifacts* are
> auditable by third parties.

## 0. The thesis in one paragraph

Pollis already asserts a strong doctrine: every state that affects message
delivery or group membership must be modeled so an invalid configuration
*cannot be expressed*
([`backend-core-invariants.md:9`](backend-core-invariants.md)). Today that
doctrine is defended by DB constraints, Rust types, a single serialization
chokepoint (the Delivery Service), and a genuinely strong test suite: hand-picked
adversarial recovery scenarios (`src-tauri/tests/flows/adversarial.rs`), a
model-based proptest fuzzer with a shadow oracle (`src-tauri/tests/flows/model.rs`),
and a marathon soak. Those are *sampling* defenses — they explore a randomized
subset of the state space and assert an oracle. This document turns the doctrine
from "we tested a lot of orderings and it held" into "we *proved* a property over
the whole small-configuration state space, and proved key pure functions correct
on the real Rust." That is the difference between *unlikely* and
*impossible-to-ship*, and it is exactly the source-side complement to verifiable
builds: **verifiable builds prove the binary matches the source; machine-checked
correctness proves the source is correct.**

The honest headline: formal methods are expensive and most of Pollis's state
machine is I/O-bound async glue that is not worth modeling. The ROI is
concentrated in a handful of **pure functions** that are load-bearing for the
bulletproof-membership invariant, plus **one abstract model** of the epoch/commit
log/delivery machine. We recommend spending there and nowhere else.

---

## 1. What we are protecting (grounding)

The guarantee we engineer for
([`backend-core-invariants.md:24`](backend-core-invariants.md)):

> A member added at epoch *N* must — no matter how long they are away, how many
> epochs pass, how many members are added/removed — (1) reach the current epoch by
> replaying every commit *N → current* (the commit chain has no gaps, ever), and
> (2) receive and decrypt every message sent in any epoch they were a member.

with exactly two accepted losses: (a) messages sent before you joined the MLS
tree (a cryptographic property of MLS), and (b) a brand-new device starts empty
(no key backup). The `model.rs` shadow oracle already encodes precisely this and
nothing weaker (`.codesight/wiki/testing.md:266-284`): positive delivery for
continuous members, forward-secrecy negative for non-members, no-wedge, roster
consistency.

The failure taxonomy F1–F7 (`backend-core-invariants.md:48`) is the list of
*invalid states that were once representable*. The invariants I1–I6 are the fixes.
The current enforcement is split across three physical writers/checkers:

| Layer | Mechanism | Where |
|---|---|---|
| DB constraint | `UNIQUE(conversation_id, epoch)` | `pollis-core/src/db/migrations/000003_mls_commit_log_unique_epoch.sql:22` |
| Serialization chokepoint | DS conditional-insert = head → append-only, gapless, one-per-epoch *by construction* | `pollis-delivery/src/commit.rs:137` (`submit_commit`) |
| Client state machine | gap detection, own-commit adoption, external-join recovery, `#418` epoch-interleaved catch-up | `pollis-core/src/commands/mls/group_state.rs:770` (`process_pending_commits_locked_impl`) |
| Client pure logic | watermark advance / stop-at ceiling — **Kani-proved (I3)** | `pollis-core/src/commands/messages/watermark.rs` (`next_watermark`; P1/P2/P3 harnesses), called by `ingest.rs:337` |

The doctrine explicitly prefers the lowest layer, but note the architecture
decision in [`mls-reconcile-hardening.md:16`](mls-reconcile-hardening.md): the
invariants live in **DS code, not DB triggers** (the `000007` trigger migration
was reverted). That is the right call *for enforcement* — but it means the
gap-free / append-only / one-per-epoch guarantee is now a property of **one SQL
statement and the Rust around it**, exactly the kind of "load-bearing pure logic"
that machine checking pays for.

---

## 2. Property coverage map (the gap analysis)

Each invariant, its *strongest current defense*, and the *target* machine-checked
defense. This is the map that drives the roadmap; the "Gap" column is where the
work is.

| Inv | Property | Strongest current defense | Target machine-checked defense | Gap |
|---|---|---|---|---|
| **I1** | commit log gapless, append-only, one-per-epoch | `UNIQUE` index + DS `submit_commit` conditional-insert (`commit.rs:137`); proptest never forks | **TLA+**: model the DS `submit_commit` as an atomic action over N clients racing at a head; model-check "no two distinct commits at one epoch ∧ no gap ∧ head monotone" exhaustively. **Kani** on `head_epoch` arithmetic + the accept/reject decision extracted as a pure fn. | ✅ **Modelled (M4 Spec A, #481).** `specs/tla/CommitLog.tla` exhaustively TLC-checks `OnePerEpoch ∧ Gapless ∧ HeadMonotone` over the `Submit` conditional-insert-at-head under N-client concurrency; teeth: `CommitLogBroken.cfg` drops the head-guard → a real `OnePerEpoch` fork counterexample. The Kani arithmetic half (`head_epoch_of`/`accepts`) already shipped in M2 (`commit.rs`). |
| **I2** | commits are a verifiable chain | MLS confirmation tag chains epochs (openmls); `our_commit_is_canonical` byte-compares (`reconcile.rs:35`) | Kani on the *canonicalization decision* (own-commit adoption vs rollback): given (submit outcome, stored bytes, our bytes), the adopt/rollback choice never adopts a foreign commit and never rolls back our own landed commit. | ✅ **Covered from both sides (#481).** `NoForeignAdopt` is TLC-checked in TLA+ Spec A (`CommitLog.tla`, M4); the pure own-commit `resolve` decision is Kani-proved (M2) and now continuously fuzzed (Track B `fuzz/resolve`). |
| **I3** | delivery = monotonic per-(member,device) cursor; retention ≥ slowest member (no TTL) | **Kani-proved** — `next_watermark` (`messages/watermark.rs`) called by the real ingest path (`ingest.rs:337`); plus the `envelope_cleanup_ttl_or_watermark` flow test | **Kani** on the watermark function: prove *monotonicity* (advance never regresses) and the **safety property** — the watermark never advances to/past an un-handled envelope's `sent_at` (the F3 message-loss guard). This is the single highest-ROI target. | ✅ **Proved (M1, #467/#468).** `next_watermark` extracted + wired into production; harnesses `p1_no_skip` (anti-F3), `p2_monotone`, `p3_handled_liveness`, each with a `should_panic` mutant (`p{1,2,3}_mutant_refuted`) certifying teeth. Retention floor (I4) still deferred — see that row. |
| **I4** | commits + welcomes retained until slowest member consumed | commit-log pruning disabled; DS is sole writer, never deletes (`commit.rs:17`) | TLA+ retention model: with the cursor model, GC-below-floor is unreachable. Kani on the floor computation once it exists. | ✅ **Design modelled (M5 Spec B, #481).** `specs/tla/Delivery.tla` (+ `Delivery.cfg`) exhaustively TLC-checks `NoLossForCurrentMember` (GC never removes a message a current member-device still needs) with `DeliveryBroken.cfg` refuting it as teeth — authored **before** the floor code (#539) per the doctrine. Kani on the floor computation still pending until that code lands (30-day envelope TTL still live per `mls-reconcile-hardening.md:141`). |
| **I5** | historical membership derivable, not guessed | DS `is_member` / client `local_user_is_member` (`group_state.rs:679`) gate recovery | Kani on the recovery-gate decision: a revoked *or* removed device can never take a recovery path that re-enters the tree (fuzzer finding #2). | Gate is fail-closed prose (`group_state.rs:679-728`); proptest samples it. Provable as a pure predicate over (registered?, member?). |
| **I6** | one schema, one apply path | harness embeds migrations + `apply_drift_fixups` (`testing.md:87-89`) | Property test / CI assertion: test schema == prod schema (structural diff), plus `schema_migrations` contiguity check. Not really formal-methods territory — a mechanical equality check. | Divergence caught only if someone looks; make it a gate. |

**Gaps that machine checking closes, ranked by ROI:**

1. **I3 watermark safety** (highest) — ✅ **done (M1)**. A pure function, a subtle
   safety property, a *history of a real false-alarm* (#442) that ate real time.
   Kani proves the real code (`next_watermark`), small bounded loop, no external
   deps.
2. **I5 recovery-gate** — a two-bit predicate guarding a membership *leak*.
   Trivial to prove, high consequence.
3. **I2 own-commit canonicalization** — the #411 adopt/rollback logic. Extractable
   to a pure decision function; proves "never adopt a foreign commit."
4. **I1 epoch machine** — TLA+ of `submit_commit` under concurrency. Exhaustive
   over small N; catches the ordering bugs proptest can only sample.
5. **Everything else** — leave to the existing (strong) proptest + adversarial
   suite. Do **not** try to formally model async I/O, openmls internals, or the DB
   driver.

**Milestone status (roadmap §8).** The recommended M0–M3 cut is now **shipped**:

- ✅ **M0 — supply chain**: `cargo-deny` + `cargo-vet` gated in CI (`deny.toml`,
  `supply-chain/`).
- ✅ **M1 — Kani watermark (I3)**: `next_watermark` proofs
  (`pollis-core/src/commands/messages/watermark.rs`).
- ✅ **M2 — Kani gate + canonicalization (I5/I2/I1)**: `may_rejoin` / `resolve` /
  `classify` proofs (`pollis-core/src/commands/mls/invariants.rs`).
- ✅ **M3 — continuous soak (Track A)**: the marathon runs nightly on a
  `schedule:` and a failing op sequence is captured as a durable CI artifact for
  one-step promotion to a deterministic regression — see §5.4.

**M4 and M5 are now landed (#481).** M5 Spec B (TLA+ Delivery / retention, I3+I4,
`specs/tla/Delivery.tla`) was authored ahead of the #539 retention-floor code per
the "model before you ship the floor" rule; the Track-B `cargo-fuzz` targets over
the §4 pure fns ship in the detached `fuzz/` crate; and M4 Spec A (TLA+ CommitLog,
I1/I2, `specs/tla/CommitLog.tla`) model-checks the `submit_commit` epoch machine
under concurrency. Both specs (sound + teeth) run in `.github/workflows/tla.yml`;
the fuzz smoke runs out of band on nightly. Only **optional OSS-Fuzz onboarding**
remains.

---

## 3. Layer 1 — Formal model of the epoch/delivery invariants (TLA+)

### What to model

A **TLA+** (PlusCal) spec of the abstract state machine, *not* the Rust. Two
specs, kept small:

**Spec A — `CommitLog` (I1/I2).** State: `log[conv]` = a sequence of commits, each
`[epoch, seq, author]`; `localEpoch[client]`. Actions:
- `Submit(c, basedOn)` — the DS action, atomic: `IF basedOn = Head(log) THEN
  append at Head ELSE reject`. This is the exact
  `commit.rs:137` semantics (`?2 = COALESCE(MAX(epoch),-1)+1`) lifted to a
  spec-level atomic step. Concurrency = interleaving of multiple clients'
  `Submit` steps.
- `Apply(client)` — advance `localEpoch[client]` by one if `log` has the next
  commit; model the gap detector (`group_state.rs:887`) as: if the next epoch is
  missing but a higher one exists, take the `ExternalJoin` action to jump to head.
- `ExternalJoin(client)` — set `localEpoch[client] := Head(log)` (models the
  recovery jump), guarded by a `member[client]` flag (I5).

**Invariants to check (TLC exhaustive):**
- `OnePerEpoch`: `∀ conv, e: |{c ∈ log[conv] : c.epoch = e}| ≤ 1`.
- `Gapless`: `log[conv]` epochs are `0..Head-1` with no hole.
- `HeadMonotone`: `Head(log[conv])` never decreases.
- `NoForeignAdopt`: a client's adopted commit at epoch `e` byte-equals `log[e]`
  (abstracted as author/nonce equality) — the I2 property.

**Forward-compatibility note (PQ hybrid MLS).** The PQ suite-migration program
([`pq-hybrid-mls-design.md`](pq-hybrid-mls-design.md)) will introduce a
suite-generation lineage: the per-conversation monotone key extends from
`(conversation, epoch)` to `(conversation, generation, epoch)`, with epoch 0
accepted only as the first commit of a newly-opened generation. When that
ships, Spec A's invariants — and the transparency-log verifier — must be
restated over that generation-keyed head. Parameterize the spec's head key from
day one (model `Head` over an abstract key rather than hard-coding the
per-conversation epoch) so the extension is a config change, not a rewrite.

**Spec B — `Delivery` (I3/I4).** State: per-conversation ordered `msgs` each with
`[epoch, sentAt]`; per-(member,device) `cursor`; `member` set with continuous
`joinEpoch`. Actions: `Send`, `Advance(cursor)` (the watermark step, guarded by
"all handled up to here"), `GC(floor)` (retention). Invariants:
- `NoLossForCurrentMember`: no `GC` removes a msg below any current member-device
  cursor (I3/I4 — the anti-F3 property).
- `CursorMonotone`: cursors never regress.
- `AcceptedLossesOnly`: a member decrypts `m` iff continuously present since
  `m.epoch` — encodes the two accepted losses and nothing weaker (mirrors the
  `model.rs` oracle at `.codesight/wiki/testing.md:266`).

### Why model checking catches what proptest samples

TLC is *exhaustive* over a bounded configuration: for `N=3` clients, `K=4`
commits, all offline/fault interleavings, it visits **every reachable state** and
proves the invariant or produces a minimal counterexample *trace*. `model.rs`
draws `DEFAULT_CASES = 32` random sequences of 4–12 ops
(`.codesight/wiki/testing.md:341`) — excellent for surfacing classes of bug (it
found #440, the committer strand), but it *samples*. A fork that only manifests
under one specific 3-way interleave at a specific epoch is a needle proptest may
never draw and TLC always finds. The two are complementary: TLC proves the
*abstract design* is sound; proptest + Kani prove the *implementation* matches it.

### Stateright alternative

If we prefer to stay in Rust, **Stateright** (a Rust model checker) lets the model
live in the repo as `#[test]`-runnable code, sharing types with `pollis-core`
enums, and is `cargo`-native (no separate TLA+ toolchain, runs headless in-box).
Trade-off: TLA+/TLC is more mature, has better state-space reduction, and the spec
reads as math (better as a third-party-auditable artifact); Stateright keeps
everything in one language and one CI. **Recommendation: TLA+ for the two specs**
(the artifact value — a `.tla` file an auditor can read and re-check with the
public TLC — is worth more here than language uniformity), with Stateright as a
fallback if the team won't maintain TLA+.

### Effort & in-box testability

- Spec A + B: ~1.5–2.5 weeks for someone comfortable with TLA+ (most of it is
  getting the abstraction boundary right, not the invariants).
- TLC runs headless in-box (JVM); a small-config run is seconds-to-minutes. **Fully
  in-box.** The `.tla`/`.cfg` files are committed; CI runs TLC on the small config
  as a fast gate, and a larger config on a schedule.
- Maintenance cost is the real risk: a spec that drifts from the code is worse
  than none. Mitigate by keeping the specs *tiny* and pinning them to the two
  functions they abstract (`submit_commit`, the watermark step), with a comment in
  each Rust function pointing at its spec action.

---

## 4. Layer 2 — Kani (bounded model checking on the real Rust)

Kani proves properties on **actual `pollis-core` code** by symbolic/bounded model
checking (CBMC backend). It is the highest-value layer because it closes the gap
between "the model is right" and "the code implements the model." It runs
headless, `cargo kani`, entirely in-box.

The strategy is to **extract the load-bearing decisions into small pure functions**
(most already are, or nearly) and prove properties over symbolic inputs. Targets,
in ROI order:

### 4.1 Watermark safety (I3) — the flagship harness

The watermark logic in `ingest_group_envelopes_interleaved`
(`ingest.rs:319-361`) is nearly pure already: `is_handled(epoch, type,
max_fired_epoch)` + the stop-at ceiling + the candidate-advance loop. Refactor the
computation into a free function:

```rust
// pollis-core/src/commands/messages/watermark.rs
pub fn next_watermark(
    envs: &[(SentAt, EnvKind, Option<Epoch>)], // sent_at-ordered
    max_fired_epoch: Option<Epoch>,
) -> Option<SentAt>;
```

Kani proof harnesses (`#[kani::proof]`, bounded `envs.len() ≤ 6`, symbolic
epochs/kinds via `kani::any()`):

- **P1 (no-skip / anti-F3):** the returned watermark is strictly less than the
  `sent_at` of the first *un-handled* envelope. ⇒ the next `sent_at > watermark`
  fetch cannot drop an un-decrypted message. This is the exact property that #442
  was a *false alarm* about — proving it retires the whole class.
- **P2 (monotone):** `next_watermark` over a prefix ≤ `next_watermark` over the
  full slice (feeding it a superset never regresses the cursor).
- **P3 (handled-liveness):** if *every* envelope is handled, the watermark equals
  the max `sent_at` (no message is retried forever once decryptable).

This is the single best use of the budget: real code, a subtle safety property, a
documented near-miss, a tiny bounded loop, zero external dependencies.

> **Status: shipped (M1, #467/#468).** `next_watermark` lives in
> `pollis-core/src/commands/messages/watermark.rs` and is the exact function the
> production ingest path calls (`ingest.rs:337`) — no forked copy. The harnesses
> `p1_no_skip` / `p2_monotone` / `p3_handled_liveness` are bounded to
> `envs.len() ≤ 4` (CBMC models `Vec`/`String` heap at ruinous cost, so inputs are
> fixed-size stack arrays over a `0..=3` domain; the tie/no-skip counterexamples
> are 2–3-element phenomena, so len-4 finds them). Each proof is paired with a
> `#[kani::should_panic]` mutant harness (`p1_mutant_refuted`: `>` instead of `>=`
> on a `sent_at` tie; `p2_mutant_refuted`: bail to `None` on the first un-handled
> envelope, discarding the handled prefix; `p3_mutant_refuted`: an off-by-one that
> never advances onto the final handled envelope) proving each property has teeth.

### 4.2 Gap detection + head arithmetic (I1)

`head_epoch` = `MAX(epoch)+1` (`commit.rs:98`) and the gap detector's
`commit.epoch != current_epoch` branch (`group_state.rs:887`). Extract the pure
decision:

```rust
pub enum ReplayStep { Apply, GapRecover, Wait }
pub fn classify(current_epoch: u64, next_row_epoch: Option<u64>, head: u64) -> ReplayStep;
```

Kani proofs: **never `Apply` across a gap** (`next_row_epoch != current+... `
handled), **`head` arithmetic never underflows** (the `COALESCE(...,-1)+1`
translated to `u64`/`i64` — prove no wrap for the empty-log case), and the DS
accept decision `based_on == head ⟺ accept` is total and never accepts two
distinct epochs. Pairs directly with TLA+ Spec A: TLA+ proves the *design*, Kani
proves the *arithmetic in the code*.

### 4.3 Own-commit canonicalization (I2)

The #411 adopt/rollback decision (`group_state.rs:403-440`, `reconcile.rs:35`).
Extract:

```rust
pub enum Resolution { Adopt, Rollback }
pub fn resolve(outcome: SubmitOutcome, ours: &[u8], stored_at_epoch: Option<&[u8]>) -> Resolution;
```

Kani proof over symbolic small byte vectors: **`Adopt` ⟹ `stored_at_epoch ==
Some(ours)`** (never adopt a foreign commit → no phantom epoch, no fork) and
**`Rollback` ⟹ `stored_at_epoch != Some(ours)`** (never discard a landed own
commit → no wedge). Both directions are the #411 correctness argument, mechanized.

### 4.4 Recovery-gate predicate (I5)

`may_rejoin_via_external_join` = `local_device_registered ∧ local_user_is_member`
(`group_state.rs:708`). Trivially pure once lifted:

```rust
pub fn may_rejoin(registered: bool, is_member: bool) -> bool { registered && is_member }
```

Kani proof: **a revoked (`!registered`) or removed (`!is_member`) device never
rejoins** — the fuzzer-finding-#2 leak, proved as a two-bit truth table. (Low
effort; included because the *consequence* is a membership/forward-secrecy leak
and a proof is nearly free.)

### Kani caveats specific to Pollis

- **`panic = "abort"`** in the release profile (`Cargo.toml:24`) is fine — Kani
  builds its own harness profile.
- Kani cannot see through openmls / libsql / async. That is *why* we extract pure
  functions: the harnesses take the already-computed epochs/bytes/flags as
  symbolic inputs, so no I/O or crypto is in scope. The extraction is a small,
  behavior-preserving refactor of code that is already structured this way (the
  functions above literally exist as inline closures / helpers today).
- Bound loops explicitly (`kani::unwind`/slice-length bounds ≤ 6–8). The
  properties are all "no reachable bad state in small configs," matching the
  doctrine's "small N, exhaustive."

### Effort & in-box testability

- 4.1: ~3–4 days incl. the refactor + wiring the flow test to call the extracted
  fn (so we don't lose the integration coverage). 4.2–4.4: ~1 day each.
- All run headless via `cargo kani` in-box. Add a `kani` job to `mls-tests.yml`
  (fast — these are tiny harnesses). **Fully in-box + cheap CI.**

---

## 5. Layer 3 — Continuous large-scale fuzzing

The `model.rs` marathon soak already exists and *runs headless in-box* — this
session's memory notes confirm the full recipe to build+run the flows harness
headless locally is established. The task is to turn a manual `#[ignore]`d run
into a **continuous soak** and to make failures reproducible despite un-seedable
MLS RNG.

### 5.1 What "continuous" means here

MLS keygen uses the OS RNG and is **not seedable** (`.codesight/wiki/testing.md:333,366`),
so classic corpus-replay coverage-guided fuzzing (libFuzzer/AFL/OSS-Fuzz on a
`fuzz_target!` with a byte seed) does **not** fit the crypto layer — a replayed
seed won't reproduce the same keys. This is a real constraint and the design must
respect it, not paper over it. Two tracks:

**Track A — scheduled deep marathon (the primary, fits Pollis today).** A
`schedule:` cron in `mls-tests.yml` (and/or an in-box `loop`) that runs the
marathon with large knobs:

```bash
MARATHON_OPS=5000 MARATHON_ACTORS=12 cargo test --features test-harness \
  --test flows -- --ignored --nocapture model_marathon_convergence
```

The **repro of record is the printed op sequence**, not a seed
(`.codesight/wiki/testing.md:367`) — the harness already embeds the full
op/fault/offline sequence in every failure message. Design addition: on failure,
**persist that op sequence as a committed regression test** (a new
`adversarial.rs` scenario or a `model.rs` fixed-sequence case). The op sequence
*is* deterministic to replay at the semantic level (same adds/removes/sends/faults
in the same order) even though the bytes differ — which is exactly what the oracle
checks, so a semantic replay is a valid regression. This closes the feedback loop:
soak finds → op sequence → deterministic scenario → permanent guard.

**Track B — coverage-guided fuzzing of the PURE layer (where seeds DO work).** The
Kani targets from §4 are also perfect `cargo-fuzz`/libFuzzer targets: `next_watermark`,
`classify`, `resolve`, `may_rejoin` are pure, seedable, and fast. A `fuzz/`
directory with these targets gives coverage-guided exploration *between* Kani's
bounded proof and the marathon's system-level soak — and *these* can go to
**OSS-Fuzz** (Rust is supported) because they have no un-seedable RNG. This is the
honest split: OSS-Fuzz-style continuous fuzzing for the pure functions; scheduled
semantic-op soak for the crypto/state-machine whole.

### 5.2 Corpus & coverage story

- **Pure-fn corpus (Track B):** seed with the boundary cases the flow tests
  already construct (empty log, single epoch, gap, tie `sent_at`, pre-join epoch,
  unparseable bytes). Coverage measured with `cargo llvm-cov` on the fuzz targets;
  target 100% branch coverage of the extracted decision functions (they're small
  enough that this is achievable and meaningful).
- **System soak coverage (Track A):** coverage is *behavioral*, not line-based —
  measured by the diversity of op/fault/offline interleavings the oracle validates.
  Track the histogram of op-mix and max-epoch-reached per run in the soak log so a
  regression that quietly stops exercising deep churn is visible.

### 5.3 Failure feedback

```
soak/OSS-Fuzz failure
  → Track A: printed op sequence  → new deterministic scenario in adversarial.rs / model.rs fixed case
  → Track B: minimized byte input → new #[test] over the pure fn + (usually) a Kani harness for the class
  → root-cause the invariant it violated (I1..I6) → update the coverage map (§2)
```

### 5.4 M3 — the continuous soak + one-step promotion (IMPLEMENTED)

Track A is now wired into CI. The marathon soak
(`model_marathon_convergence`, `src-tauri/tests/flows/model.rs`) runs on a
**nightly `schedule:`** in `.github/workflows/mls-tests.yml` (07:00 UTC) at a
meaningful size — **`MARATHON_OPS=500`, `MARATHON_ACTORS=8`** — in addition to
the existing manual `workflow_dispatch`. The scheduled run has no dispatch
inputs, so the step's `${{ github.event.inputs.marathon_ops || '500' }}` /
`|| '8'` fallbacks pin the nightly size; a manual dispatch still uses its own
inputs (300/6 defaults). A marathon failure fails the job **red** (the run is
`set -o pipefail`-guarded so the `tee` can't swallow the non-zero exit) — no
silent green.

**What is captured.** The soak's stdout/stderr is teed to `marathon-soak.log`.
On failure a follow-up step distils the panic's embedded op sequence (the
`op sequence (N ops): [...]` line — the repro of record, since MLS keygen uses
the OS RNG and is **not** seedable, so a byte seed would not replay) into
`marathon-failing-op-sequence.txt`, and both files are uploaded as the
`marathon-failing-op-sequence` artifact (`actions/upload-artifact`,
`if: failure()`, 90-day retention).

**Scope, stated honestly.** This is a *replayable uploaded artifact + a
documented promotion*, **not** an auto-PR bot. Nothing opens a regression PR by
itself; a human copies the captured sequence into a deterministic test. That is
the deliberate M3 cut — the feedback loop is closed by a one-step manual
promotion, not automation.

**One-step promotion (soak failure → permanent regression).**

1. Open the failed nightly run, download the `marathon-failing-op-sequence`
   artifact, and read `marathon-failing-op-sequence.txt`. It contains the exact
   `Op` vector, e.g. `op sequence (500 ops): [Send(0), Add(1), Fault(1),
   Remove(1), Sync(2), ...]`.
2. In `src-tauri/tests/flows/model.rs`, add a fixed-sequence regression that
   reuses the existing case body — no new harness surface:

   ```rust
   #[tokio::test(flavor = "multi_thread")]
   #[serial]
   async fn regression_soak_2026_07_07() {
       // Pasted verbatim from the marathon-failing-op-sequence artifact of
       // <run URL>. `nactors` = the pool the soak used (MARATHON_ACTORS=8).
       let ops = vec![Op::Send(0), Op::Add(1), Op::Fault(1), Op::Remove(1) /* … */];
       run_case(&ops, 8).await.expect("regression must converge");
   }
   ```

   Because the oracle asserts *semantic* delivery/membership (not bitwise key
   equality), replaying the same adds/removes/sends/faults **in the same order**
   is a valid regression even though the MLS bytes differ run to run — the
   op sequence, not a seed, is the deterministic repro. (A hand-authored
   `adversarial.rs` scenario is the alternative when the sequence shrinks to a
   handful of ops worth naming.)
3. Confirm it reproduces (fails), fix the invariant it violated, confirm it
   passes, and keep it committed as a permanent guard. Update the §2 coverage
   map with the invariant (I1..I6) it exercised.

### Effort & in-box testability

- Track A continuous: ~1–2 days (cron/loop + the "persist op sequence as
  regression" helper). Runs headless in-box today; the scheduled variant is
  free CI (`workflow_dispatch` already exists at `mls-tests.yml:24`, promote to
  `schedule:`).
- Track B (`cargo-fuzz` targets + OSS-Fuzz integration): ~3–5 days, and depends on
  the §4 extraction landing first (shared targets). OSS-Fuzz onboarding is a
  separate PR to their repo but the targets live here.

---

## 6. Layer 4 — Supply-chain / dependency assurance

The MLS crypto and the DS pull a deep dependency tree (openmls, libsql, axum,
libwebrtc, …). "The source is correct" is only meaningful if the *dependency*
source is trusted. This ties directly into the verifiable-builds story: a
reproducible build of a tree full of unvetted crates proves you reproduced
*someone's* code, not *trustworthy* code.

Three tools, wired into CI as a distinct fast job (no native deps → cheap):

- **`cargo-deny`** (advisories + licenses + bans + sources): fail CI on any
  RUSTSEC advisory in the tree, on a disallowed/again-unknown license, on
  duplicate/yanked crates, and on any crate sourced outside crates.io/our vendor.
  A `deny.toml` at the workspace root; `cargo deny check` in CI. **Start here —
  highest value per hour, near-zero maintenance.**
- **`cargo-vet`** (human review provenance): record that each dependency (and
  version) has been reviewed — either by us or via an imported trusted registry
  (the Mozilla/Google shared audit set). `supply-chain/` audits committed;
  `cargo vet` in CI fails on any unvetted/updated crate, forcing a review or an
  explicit exemption. This is the "verify, don't trust" story applied to deps: an
  auditor can read `supply-chain/audits.toml` and see exactly what was reviewed by
  whom.
- **`cargo-crev`** (optional, community web-of-trust): complements vet with
  external reviews. Lower priority; vet's imported audit sets cover most of the
  value with less ceremony.

### Composition with verifiable builds

The transparency/verifiable-builds work
([`transparency.md`](transparency.md), the `verifiable-log*` crates) proves the
**shipped binary corresponds to a specific source tree**. `cargo-vet` +
`cargo-deny` prove that **source tree's dependencies were reviewed and are
advisory-clean**. Machine-checked correctness (§3–§5) proves **our own source
implements the invariants**. The three together are the full chain:

```
reviewed deps (vet/deny)  →  correct source (TLA+/Kani/fuzz)  →  reproducible binary (verifiable builds)  →  transparent log (Merkle STH)
        └──────────────────────────── "verify, don't trust" end to end ────────────────────────────┘
```

Every link is an artifact a third party can re-check with only public inputs. No
single link is trusted on our say-so.

### Effort & in-box testability

- `cargo-deny`: <1 day (config + CI job). Runs in-box, no native deps.
- `cargo-vet`: 1–2 days to bootstrap + import audit sets; then ongoing per-bump
  review cost (real but bounded, and the point). In-box.

---

## 7. Threat model / value — impossible-to-ship vs merely unlikely

What each layer *upgrades* from "tested" to "proved":

| Bug class | Today (unlikely) | After (impossible-to-ship) | By |
|---|---|---|---|
| Two commits at one epoch / fork | `UNIQUE` + DS conditional-insert + proptest | Exhaustively unreachable in the model; arithmetic proved | TLA+ Spec A + Kani §4.2 |
| Commit-log gap wedges a member | gap detector + external-join + one adversarial scenario | `classify` never `Apply`s across a gap (proved); design has no gap-reachable state | Kani §4.2 + TLA+ |
| **Undelivered message dropped (F3)** | watermark stop-at + one flow test + prose (and a *false alarm*, #442) | watermark provably never advances past an un-handled envelope | **Kani §4.1** |
| Phantom-epoch / wedge from lost response (#411) | comment-argued adopt/rollback + proptest faults | `resolve` provably never adopts foreign / never rolls back own | Kani §4.3 |
| Removed/revoked device re-enters tree (leak, finding #2) | fail-closed gate + proptest | two-bit predicate proved | Kani §4.4 |
| Deep-churn divergence | marathon soak (manual) | continuously soaked; every found sequence becomes a permanent regression | §5 Track A |
| Vulnerable/malicious dependency | none (implicit trust) | advisory-clean + provenance-reviewed, gated | §6 |

The distinction that matters for the doctrine: proptest and the adversarial suite
make these bugs **unlikely to reach a release** (you'd have to draw the needle and
not notice). Kani + TLA+ make a *specific, enumerated* subset **impossible to
merge** — the proof fails in CI. We are not claiming the whole system is verified
(it is not — openmls, libsql, the async glue are trusted); we are claiming the
*load-bearing decisions* of the bulletproof-membership invariant are, and the
coverage map (§2) is the honest ledger of which is which.

---

## 8. Phased roadmap

Ordered by ROI. Each phase is independently shippable and independently valuable;
**nothing depends on a phase after it.** All of it runs headless in-box; the "CI"
column is only where we also want it gated on every PR.

| Phase | Work | Effort | In-box | CI |
|---|---|---|---|---|
| **M0 — supply chain** | `cargo-deny` (`deny.toml` + job); bootstrap `cargo-vet` + import audit sets | ~2–3 d | ✅ | ✅ new fast job |
| **M1 — Kani watermark (I3)** | extract `next_watermark`; P1 no-skip / P2 monotone / P3 liveness; rewire flow test to the extracted fn | ~4 d | ✅ | ✅ `cargo kani` job |
| **M2 — Kani gate + canonicalization (I5/I2/I1)** | extract `may_rejoin`, `resolve`, `classify`; prove leak-freedom + no-foreign-adopt + no-gap-apply | ~3 d | ✅ | ✅ same job |
| **M3 — continuous soak (Track A)** | promote marathon to `schedule:`/`loop`; "persist failing op sequence as regression" helper | ~2 d | ✅ | ✅ scheduled |
| **M4 — TLA+ epoch model (I1/I2)** | ✅ **Done** (#481): `specs/tla/CommitLog.tla` + sound/teeth cfgs model-check `OnePerEpoch ∧ Gapless ∧ HeadMonotone ∧ NoForeignAdopt` over `Submit`/`Apply`/`ExternalJoin`; cross-ref comments in `submit_commit` (`commit.rs`) + the client apply/gap path (`group_state.rs`); wired into the `tla.yml` gate | ~1.5–2.5 wk | ✅ (JVM) | ✅ TLC small-config gate |
| **M5 — TLA+ delivery model + fuzz targets (I3/I4 + Track B)** | ✅ **Spec B done** (`specs/tla/Delivery.tla` + teeth cfg, TLC gate `.github/workflows/tla.yml`, #481) — landed before the #539 floor code. ✅ **Track-B `cargo-fuzz` done**: the detached `fuzz/` crate carries a target per §4 pure fn (`next_watermark`/`classify`/`resolve`/`may_rejoin`), each asserting its Kani property with a `--cfg fuzz_mutant` teeth variant + a seed corpus (OSS-Fuzz-eligible), run via `scripts/fuzz-check.sh`. ⏳ **Remaining:** optional OSS-Fuzz onboarding | ~1.5–2 wk | ✅ | ✅ fuzz smoke + TLC |

**Recommended cut if budget is tight:** M0 + M1 + M2 + M3 deliver ~80% of the
value for ~2 weeks of work — supply-chain assurance, the three highest-consequence
pure-function proofs on real code, and a self-reinforcing continuous soak. M4/M5
(TLA+) are higher-effort, higher-ceremony, and best justified once the pure-fn
proofs have shown the team the ROI and the retention-floor (I4) code is about to
be written (model it first).

---

## 9. Acceptance criteria

- **M0:** `cargo deny check` is green and gates PRs; a seeded RUSTSEC advisory
  fails CI; `cargo vet` reports a fully-vetted tree (or explicit, reviewed
  exemptions). Artifact: `deny.toml`, `supply-chain/audits.toml` committed and
  auditable.
- **M1:** `cargo kani` proves P1–P3 on `next_watermark`; a deliberately-broken
  watermark (advance past an un-handled envelope) makes Kani produce a
  counterexample. The extracted fn is the one production calls (no drift).
- **M2:** Kani proves `may_rejoin` leak-freedom, `resolve` no-foreign-adopt +
  no-own-rollback, `classify` no-gap-apply; each has a negative test (break the
  fn → Kani fails).
- **M3:** the marathon runs on a schedule headless; a synthetic injected
  divergence is caught *and* its op sequence is auto-emitted in a form that drops
  straight into `adversarial.rs`/`model.rs` as a regression.
- **M4:** TLC exhaustively checks `OnePerEpoch ∧ Gapless ∧ HeadMonotone ∧
  NoForeignAdopt` for N=3, K=4 with all fault interleavings and reports no
  violation; deliberately removing the head-guard from the `Submit` action
  produces a fork counterexample. `.tla` files re-checkable by a third party with
  public TLC.
- **M5:** TLC checks `NoLossForCurrentMember ∧ CursorMonotone ∧
  AcceptedLossesOnly` on the delivery spec; `cargo-fuzz` targets build and run to a
  coverage plateau with 100% branch coverage of the extracted decision functions.
- **Overarching (the doctrine's own acceptance test):** the "joined 4 years / 300
  commits ago, comes back, receives every in-window message" guarantee is now
  defended at *three* levels for its load-bearing steps — a deterministic
  adversarial scenario (have), an exhaustive small-config model (M4/M5), and a
  proof on the real gap/watermark/canonicalization code (M1/M2) — with the coverage
  map (§2) kept current as the ledger of what is proved vs sampled.

---

## 10. Recommendation

Spend the budget where the state machine is *pure and load-bearing*, not where it
is *async and glue*. Concretely: **do M0 → M1 → M2 → M3 now** (~2 weeks:
supply-chain gating + Kani on watermark/gate/canonicalization + a continuous
soak). These are all in-box, cheap in CI, and each retires a *named* production
incident class (#442 message-loss false-alarm, #411 phantom-epoch, fuzzer-finding-#2
leak, F1 gaps). Defer the TLA+ specs (M4/M5) until the pure-fn proofs prove the ROI
and the retention-floor code (I4) is imminent — then model it before writing it.
Do **not** attempt to formally verify openmls, libsql, or the async command layer;
keep them behind the trust boundary and let the (already strong) proptest +
adversarial suite guard the integration. The whole program composes with
verifiable builds and the transparency log to make "verify, don't trust" auditable
end to end: reviewed deps → proved source → reproducible binary → transparent log.

---

## Appendix — GitHub-issue-ready summary

> **Title:** Machine-checked correctness for the MLS backend — prove the "invalid
> states unrepresentable" doctrine, don't just test it
>
> **Problem.** Our bulletproof-membership invariant
> (`docs/backend-core-invariants.md`) is defended by a strong but *sampling* suite:
> the adversarial recovery scenarios, the model-based proptest fuzzer + shadow
> oracle, and the marathon soak. These make delivery/membership bugs *unlikely* but
> not *impossible-to-ship* — the load-bearing decisions (commit-log gap/head
> arithmetic, delivery watermark, own-commit canonicalization, recovery gate) are
> proved only by prose and by drawing enough random orderings. #442 (a message-loss
> *false alarm* traced to oracle logic) and #411 (phantom-epoch) show the reasoning
> is subtle. We want the source *proved* correct, as the complement to verifiable
> builds proving the binary matches the source.
>
> **Approach.** Extract the load-bearing pure functions (watermark advance, gap
> classification, own-commit adopt/rollback, recovery gate — they already exist as
> helpers/closures) and (1) prove them with **Kani** on the real Rust, (2) fuzz them
> continuously (`cargo-fuzz`, OSS-Fuzz-eligible since they're seedable), (3) model
> the abstract epoch/delivery machine in **TLA+** and exhaustively check it for
> small configs, (4) turn the marathon soak into a continuous scheduled run whose
> failing op-sequences auto-become regressions, and (5) gate the dependency tree
> with **cargo-deny** + **cargo-vet**. All of it runs headless **in-box**; CI gates
> are cheap. Full coverage map (invariant → current defense → target proof → gap) in
> `docs/machine-checked-correctness-design.md` §2.
>
> **Milestones.**
> - **M0 — supply chain** (~2–3 d): `cargo-deny` (advisories/licenses) + `cargo-vet`
>   (review provenance), gated in CI.
> - **M1 — Kani watermark / I3** (~4 d): prove the watermark never advances past an
>   un-handled envelope (the F3 / #442 class), + monotonicity + liveness, on the
>   extracted production function.
> - **M2 — Kani gate + canonicalization / I5+I2+I1** (~3 d): prove a
>   revoked/removed device never rejoins (finding-#2 leak); prove own-commit
>   resolution never adopts a foreign commit / never rolls back a landed own one
>   (#411); prove replay never applies across a gap.
> - **M3 — continuous soak** (~2 d): scheduled marathon; failing op-sequences
>   auto-emitted as `adversarial.rs`/`model.rs` regressions.
> - **M4 — TLA+ epoch model / I1+I2** (~1.5–2.5 wk): exhaustive check of
>   one-per-epoch ∧ gapless ∧ head-monotone ∧ no-foreign-adopt.
> - **M5 — TLA+ delivery model + fuzz targets / I3+I4** (~1.5–2 wk): retention-floor
>   model (*before* the floor code ships) + `cargo-fuzz`/OSS-Fuzz targets on the
>   pure fns.
>
> **Recommended cut:** M0–M3 (~2 wk) deliver ~80% of the value; M4/M5 once the
> pure-fn proofs prove ROI and the I4 retention floor is imminent.
>
> **Acceptance.** Each Kani/TLA+ target has a *negative* test (break the code/spec →
> the check fails). `cargo deny`/`vet` gate PRs. The soak auto-produces regressions.
> The doctrine's own acceptance test ("joined 4y/300 commits ago, receives every
> in-window message") is defended at three levels — deterministic scenario,
> exhaustive small-config model, and a proof on the real gap/watermark/canonicalization
> code — with the §2 coverage map kept current as the proved-vs-sampled ledger.
> Composes with verifiable builds + the transparency log for end-to-end "verify,
> don't trust."
