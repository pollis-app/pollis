//! Model-based property fuzz layer for the adversarial MLS recovery suite.
//!
//! Governing theme (`docs/backend-core-invariants.md`, I1–I6 / F1–F7): *make
//! invalid states unrepresentable*, and *group-membership encryption must be
//! bulletproof — we can't rely on timeliness or best-case; any member may be
//! offline arbitrarily long and must pick up seamlessly.* The hand-picked
//! scenarios in `flows/adversarial.rs` prove specific failure-recovery
//! orderings. This layer is the "beyond reasonable doubt" complement: instead of
//! curated sequences, it GENERATES random op/fault/offline sequences over a small
//! actor pool, forces the group to converge, and asserts the SAME bulletproof
//! invariant holds for *every* generated sequence.
//!
//! ## What makes it model-based (not just fuzzing)
//!
//! Alongside execution we maintain a plain-Rust **shadow oracle**:
//! - the current membership SET of the channel as ops apply, and
//! - for every Send, `(message_body, snapshot_of_membership_at_send_time)`.
//!
//! After convergence the oracle predicts, for each actor `X` and message `M`
//! with membership snapshot `S` (sent at logical clock `t_M`):
//! - **Positive (delivery must work):** if `X` has been a member *continuously*
//!   since `M` was sent — i.e. `X ∈ current_now` AND `X`'s most-recent (re-)join
//!   clock `≤ t_M` — then `X` MUST decrypt `M`. This is the bulletproof case: a
//!   continuously-present member, even one offline for the whole window, replays
//!   the commit chain on return and the interleave hook decrypts every in-window
//!   envelope at its epoch.
//! - **Negative (the two accepted losses + forward secrecy):** `X ∉ S` ⟹ `X`
//!   does NOT decrypt `M`. This encodes *exactly* the two accepted losses from
//!   CLAUDE.md — (a) messages sent before you joined, and (b) messages sent while
//!   you were removed (before a re-add) — and nothing weaker.
//!
//! Deliberately NOT asserted (neither too strong nor too weak):
//! - A removed member that cached a message it decrypted while a member
//!   (`X ∈ S` but `X ∉ current_now`) — retained local history is fine to keep.
//! - A message sent while `X` was a member but during a stint `X` was later
//!   removed from and re-added after (`X ∈ S` but `X`'s current-join clock `> t_M`).
//!   Removal forgets MLS state and re-add gives a fresh leaf, so if `X` never
//!   fetched `M` before removal it is *cryptographically* gone — and MLS has no
//!   key backup (Megolm-style backup is explicitly forbidden by CLAUDE.md). The
//!   deterministic `eviction_then_readd_has_provable_blackout` scenario covers the
//!   flip side (a *cached* pre-removal message survives). Asserting delivery here
//!   would be a false failure.
//!
//! ## Ops → real client commands (NO invented seams)
//!
//! Every op maps to a method already exercised by the green flows suite, so this
//! layer invents no new client surface (there is deliberately no rotate/self-update
//! op — `self_update` does not exist anywhere in this repo):
//! - `Op::Add(t)`      → `join_member` (invite → accept → poll → process), i.e.
//!                       `send_group_invite` / `accept_group_invite` /
//!                       `poll_mls_welcomes` / `process_pending_commits`.
//! - `Op::Remove(t)`   → `TestClient::remove_member` (`remove_member_from_group`)
//!                       + committer `process_pending_commits`.
//! - `Op::Send(a)`     → `TestClient::send_channel_message` (`send_message`) from
//!                       a CURRENT member (the sender syncs first — a real client
//!                       processes pending commits when it acts — so the send is at
//!                       the live epoch, which keeps the oracle sound).
//! - `Op::Sync(a)`     → `poll_mls_welcomes` + `process_pending_commits` +
//!                       `get_channel_messages` (models "come back online"; an
//!                       actor that hasn't Synced in a while is effectively offline).
//! - `Op::Fault(v)`    → `arm_ds_fault` before the NEXT commit-producing op.
//!
//! Ops are guarded against the shadow model (no Send from a non-member, no Remove
//! of an absent actor, no Add of a present one) by **skipping** them in execution
//! and NOT recording them in the oracle.
//!
//! ## Fault set (see `harness::DsFault`)
//!
//! The fuzzer draws from the three *landing* faults — `Fail500PostWrite`,
//! `DropResponse`, `DropWelcome` — all of which LEAVE THE COMMIT DURABLE and force
//! the client through a recovery path (adopt-own-canonical / external-join /
//! duplicate-leaf prune). They recover *internally* (the client command returns
//! `Ok`), so the membership change still lands and the shadow model applies it
//! normally — the difference from clean fuzzing is purely the recovery ordering.
//! `Fail500PreWrite` (the CLEAN, no-op rollback) is deliberately excluded: it
//! surfaces a client-side error (needs the non-panicking `invoke_try` path) and
//! makes the op a membership no-op the model would have to predict — both are
//! covered deterministically by `fail500_pre_write_persists_nothing_and_does_not_wedge`
//! in `adversarial.rs`. The `drop_commit_row` interior-gap fault is likewise left
//! to the deterministic `epoch_gap_recovers_via_external_join` scenario: sequencing
//! a durable interior gap requires knowing which epoch has a higher epoch appended
//! above it, which is awkward to guarantee from a random generator.
//!
//! ## Async/proptest bridge, determinism, and CI budget
//!
//! - **Async bridge:** proptest closures are synchronous; the harness is async
//!   tokio. We build ONE process-wide multi-thread `tokio::runtime::Runtime`
//!   (`runtime()`) and `block_on` the async case body inside the proptest closure.
//!   The whole test is `#[serial]` because it shares the singleton `WORLD`
//!   `OnceCell`, the one in-process DS, and the global `NEXT_DS_FAULT`; each case
//!   `wipe()`s + `clear_ds_fault()`s at the start so cases can't bleed.
//! - **Determinism / shrinking:** MLS key generation uses the OS RNG and is NOT
//!   seeded from the harness (there is no seam to seed OpenMLS's provider RNG
//!   without new production surface), so replays of the same op sequence are not
//!   bitwise-identical. We therefore do NOT claim deterministic shrinking, and we
//!   DISABLE failure persistence (`failure_persistence = None`) so no misleading
//!   "regression" seed — which might not reproduce — is written. Shrinking is
//!   best-effort; the counterexample **op sequence** printed on failure is the
//!   authoritative repro, so every failure message embeds it.
//! - **CI time budget:** each case spins real MLS crypto + a real in-process DS,
//!   so cases are EXPENSIVE. CI runs a MODEST count (`DEFAULT_CASES`, short
//!   bounded sequences, 4 actors) so this adds only a couple of minutes. Deep
//!   fuzzing is a LOCAL / MANUAL soak: crank the count with
//!   `PROPTEST_CASES=2000 cargo test --features test-harness --test flows model`.
//!   This is not silent under-coverage — the modest CI count is documented here
//!   and logged nowhere else because there is no runtime seam to hide it behind.

use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;

use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};
use serial_test::serial;

use crate::harness::{
    arm_ds_fault, clear_ds_fault, ds_head_epoch, wipe, DsFault, TestClient,
};

// ─── tunables (CI budget knobs) ──────────────────────────────────────────────

/// Actors in the pool: index 0 is `alice`, the fixed group owner + committer for
/// every Add/Remove; indices 1..NACTORS are the churn pool (bob, carol, dave).
const NACTORS: usize = 4;

/// Modest CI default. Override for a local soak: `PROPTEST_CASES=2000 …`.
const DEFAULT_CASES: u32 = 32;

/// Bounded generated-sequence length. Kept short so a single case stays cheap.
const MIN_OPS: usize = 4;
const MAX_OPS: usize = 12;

/// Convergence rounds — how many times every ever-member polls + processes +
/// fetches to drain welcomes, replay all pending commits (decrypting each epoch's
/// envelopes via the interleave hook), and settle external-join recoveries. A
/// handful of rounds covers the recovering-member ↔ committer ping-pong for a
/// pool this small.
const CONVERGE_ROUNDS: usize = 4;
/// Fewer rounds suffice after the final probe: it is an application message (no
/// new epoch), so members already at the head only need to fetch its envelope.
const PROBE_ROUNDS: usize = 2;

// ─── one process-wide runtime for the sync→async bridge ──────────────────────

fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        // Multi-thread, matching the deterministic scenarios'
        // `#[tokio::test(flavor = "multi_thread")]`: the harness's `invoke`
        // wraps `spawn_blocking`, which needs worker threads.
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime for the model proptest")
    })
}

// ─── the generated op ────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
enum Op {
    /// Add pool actor `t` (1..NACTORS) via the invite/accept/poll/process flow.
    Add(u8),
    /// Remove pool actor `t` (1..NACTORS) via `remove_member_from_group`.
    Remove(u8),
    /// Actor `a` (0..NACTORS) sends a channel message.
    Send(u8),
    /// Actor `a` (0..NACTORS) comes online (poll + process + fetch).
    Sync(u8),
    /// Arm a landing DS fault (index into `fault_variant`) for the next
    /// commit-producing op.
    Fault(u8),
}

/// Map the generated fault index to one of the three *landing* faults.
fn fault_variant(v: u8) -> DsFault {
    match v {
        0 => DsFault::Fail500PostWrite,
        1 => DsFault::DropResponse,
        _ => DsFault::DropWelcome,
    }
}

fn op_strategy(npool: u8) -> impl Strategy<Value = Op> {
    prop_oneof![
        // Weighted toward Send/Sync so most coverage is message delivery across
        // membership churn; Add/Remove/Fault salt in the adversarial orderings.
        3 => (1u8..npool).prop_map(Op::Add),
        2 => (1u8..npool).prop_map(Op::Remove),
        6 => (0u8..npool).prop_map(Op::Send),
        4 => (0u8..npool).prop_map(Op::Sync),
        2 => (0u8..3u8).prop_map(Op::Fault),
    ]
}

fn ops_strategy() -> impl Strategy<Value = Vec<Op>> {
    proptest::collection::vec(op_strategy(NACTORS as u8), MIN_OPS..=MAX_OPS)
}

// ─── shared helpers (mirrors `flows/adversarial.rs`; kept local to avoid
// widening the harness surface for two tiny functions) ───────────────────────

/// Invite `member` to `group_id`, accept, drain the Welcome, and replay commits
/// so the member is a fully-joined participant of the channel's MLS group.
async fn join_member(
    inviter: &TestClient,
    member: &TestClient,
    group_id: &str,
    channel_id: &str,
    member_username: &str,
) {
    inviter.invite(group_id, member_username).await;
    let invite_id = member
        .first_pending_invite()
        .await
        .expect("member should have a pending invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    member.accept_invite(&invite_id).await;
    member.poll().await;
    inviter.process_commits_for(channel_id).await;
    member.process_commits_for(channel_id).await;
}

/// The decrypted plaintext bodies visible to `client` in `channel_id`.
async fn contents(client: &TestClient, channel_id: &str) -> Vec<String> {
    client
        .fetch_channel_messages(channel_id)
        .await
        .iter()
        .filter_map(|m| m["content"].as_str().map(str::to_string))
        .collect()
}

/// Bring every ever-member online: poll welcomes, replay pending commits, and
/// fetch (the fetch is what drives the interleaved replay+decrypt + external-join
/// recovery, exactly as the deterministic scenarios drive it via `contents`).
async fn converge(clients: &[TestClient], ever: &BTreeSet<usize>, channel_id: &str, rounds: usize) {
    for _ in 0..rounds {
        for &i in ever {
            clients[i].poll().await;
            clients[i].process_commits_for(channel_id).await;
            let _ = clients[i].fetch_channel_messages(channel_id).await;
        }
    }
}

/// Format a failure so a shrunk counterexample is actionable: the human detail
/// PLUS the full op sequence (the authoritative repro given non-seedable MLS RNG).
fn fail_msg(ops: &[Op], detail: &str) -> String {
    format!("{detail}\n  op sequence ({} ops): {ops:?}", ops.len())
}

// ─── the generated-case body ─────────────────────────────────────────────────

async fn run_case(ops: &[Op], nactors: usize) -> Result<(), String> {
    // Fresh remote + no armed fault so cases can't bleed across the shared world.
    wipe().await;
    clear_ds_fault();

    // A pool of `nactors` pre-signed-up clients (index 0 = owner/committer) + one
    // group channel. Emails are generated so the pool can scale (the marathon
    // soak runs with a larger pool than the modest fuzzer default).
    let mut clients: Vec<TestClient> = Vec::with_capacity(nactors);
    for _ in 0..nactors {
        clients.push(TestClient::new().await);
    }
    let mut ids: Vec<String> = Vec::with_capacity(nactors);
    let mut usernames: Vec<String> = Vec::with_capacity(nactors);
    for i in 0..nactors {
        let email = format!("user{i}@test.local");
        let p = clients[i].sign_up(&email).await;
        ids.push(p.id.clone());
        usernames.push(p.username.clone());
    }

    let group_id = clients[0].create_group("Model").await;
    let channel_id = clients[0].general_channel_id(&group_id).await;

    // ── shadow oracle ─────────────────────────────────────────────────────────
    // Current membership (alice always in), the set that has EVER been a member
    // (only these may safely `fetch` / be asserted on), the clock at which each
    // current member's continuous stint began (`joined_at`, alice at clock 0),
    // the recorded sends `(body, snapshot, sent_at_clock)`, and the intent to arm
    // a fault before the next commit-producing op.
    let mut current: BTreeSet<usize> = BTreeSet::from([0]);
    let mut ever: BTreeSet<usize> = BTreeSet::from([0]);
    let mut joined_at: HashMap<usize, usize> = HashMap::from([(0usize, 0usize)]);
    let mut messages: Vec<(String, BTreeSet<usize>, usize)> = Vec::new();
    let mut pending_fault: Option<DsFault> = None;
    let mut msg_seq: usize = 0;
    // A logical clock that ticks once per op, so `joined_at` and each message's
    // `sent_at` are ordered and "continuous membership since M" is decidable.
    let mut clock: usize = 0;
    // How many commit-producing ops (Add/Remove) actually LANDED — used to decide
    // whether the DS head *must* have advanced past genesis. A degenerate sequence
    // whose every membership op is skipped by a well-formedness guard (e.g. all
    // Remove of a never-added actor) leaves the group correctly at epoch 0.
    let mut commit_ops: usize = 0;

    for op in ops {
        clock += 1;
        match *op {
            Op::Add(t) => {
                let t = t as usize;
                // Well-formed guard: don't add someone already present.
                if current.contains(&t) {
                    continue;
                }
                // Arm the fault (if any) immediately before the add commit that
                // will consume it — arm and consume are adjacent, so nothing is
                // ever left armed.
                if let Some(f) = pending_fault.take() {
                    arm_ds_fault(f);
                }
                join_member(&clients[0], &clients[t], &group_id, &channel_id, &usernames[t]).await;
                commit_ops += 1;
                current.insert(t);
                ever.insert(t);
                // This stint's continuous membership starts now (overwrites any
                // prior stint, so a message from before a removal-and-re-add is
                // correctly no longer "continuous" for `t`).
                joined_at.insert(t, clock);
            }
            Op::Remove(t) => {
                let t = t as usize;
                // Well-formed guard: don't remove someone not present. Alice
                // (index 0) is never a target of the generator (range starts at 1).
                if !current.contains(&t) {
                    continue;
                }
                if let Some(f) = pending_fault.take() {
                    arm_ds_fault(f);
                }
                clients[0].remove_member(&group_id, &ids[t]).await;
                clients[0].process_commits_for(&channel_id).await;
                commit_ops += 1;
                current.remove(&t);
            }
            Op::Send(a) => {
                let a = a as usize;
                // Well-formed guard: only a current member may send.
                if !current.contains(&a) {
                    continue;
                }
                // A real client processes pending commits when it acts, so the
                // send is at the live epoch — this keeps the oracle sound (no
                // stale-epoch sends that current members legitimately couldn't
                // decrypt, which is out of scope here).
                clients[a].poll().await;
                clients[a].process_commits_for(&channel_id).await;
                let body = format!("m{msg_seq}");
                msg_seq += 1;
                clients[a].send_channel_message(&channel_id, &body).await;
                // ORACLE: sealed at the membership snapshot + clock at send time.
                messages.push((body, current.clone(), clock));
            }
            Op::Sync(a) => {
                let a = a as usize;
                // Poll welcomes for anyone (harmless if none); only replay
                // commits for actors that have a group to catch up on.
                clients[a].poll().await;
                if ever.contains(&a) {
                    clients[a].process_commits_for(&channel_id).await;
                    let _ = clients[a].fetch_channel_messages(&channel_id).await;
                }
            }
            Op::Fault(v) => {
                pending_fault = Some(fault_variant(v));
            }
        }
    }

    // Defensive: faults are armed only immediately before a consuming commit, so
    // nothing should be armed here — but clear any stray arm so it can't bleed.
    clear_ds_fault();

    // ── force convergence ───────────────────────────────────────────────────
    converge(&clients, &ever, &channel_id, CONVERGE_ROUNDS).await;

    // Probe: a fresh alice-authored message every CURRENT member must decrypt —
    // the load-bearing "everyone caught up to the head" check (a wedged or forked
    // member fails it). Alice is always current.
    clients[0].poll().await;
    clients[0].process_commits_for(&channel_id).await;
    let probe = format!("probe{msg_seq}");
    clients[0].send_channel_message(&channel_id, &probe).await;
    // Sent after every op, so its clock is beyond every current member's join —
    // every current member is "continuous since the probe" and must decrypt it.
    let probe_at = clock + 1;
    messages.push((probe.clone(), current.clone(), probe_at));
    converge(&clients, &ever, &channel_id, PROBE_ROUNDS).await;

    // Snapshot each ever-member's decrypted view once (O(actors) fetches).
    let mut view: HashMap<usize, Vec<String>> = HashMap::new();
    for &x in &ever {
        view.insert(x, contents(&clients[x], &channel_id).await);
    }

    // ── Assertion 1 (positive delivery) + Assertion 3 (blackout / forward
    // secrecy) — the two directions of the bulletproof invariant ──────────────
    for (body, snap, sent_at) in &messages {
        for &x in &ever {
            let has = view[&x].contains(body);
            let in_snap = snap.contains(&x);
            // Continuous membership since M: currently a member, and this stint's
            // join clock is at or before M's send clock.
            let continuous =
                current.contains(&x) && joined_at.get(&x).is_some_and(|&j| j <= *sent_at);

            if continuous && !has {
                return Err(fail_msg(
                    ops,
                    &format!(
                        "LOST MESSAGE (delivery broken): actor {x} has been a member continuously \
                         since {body:?} was sent, but cannot decrypt it. view={:?}",
                        view[&x]
                    ),
                ));
            }
            if !in_snap && has {
                return Err(fail_msg(
                    ops,
                    &format!(
                        "MEMBERSHIP LEAK: actor {x} decrypted {body:?} but was NOT a member when \
                         it was sent (an accepted-loss violation — before-join or during-eviction). \
                         view={:?}",
                        view[&x]
                    ),
                ));
            }
        }
    }

    // ── Assertion 2: no current member is wedged — all agree on the head, proved
    // by every current member decrypting the final probe (stronger than reading a
    // server-side epoch integer, and per-client) ─────────────────────────────
    for &x in &current {
        if !view[&x].contains(&probe) {
            return Err(fail_msg(
                ops,
                &format!(
                    "WEDGED: current member {x} did not converge to the head — it cannot decrypt \
                     the final probe {probe:?}. view={:?}",
                    view[&x]
                ),
            ));
        }
    }
    // Only demand the DS advanced past genesis if a membership commit actually
    // landed: a sequence whose every Add/Remove was skipped by a well-formedness
    // guard (e.g. all-Remove of a never-added actor) leaves the group correctly at
    // epoch 0, which is not a wedge. When commits DID land, a genesis head would be
    // a real bug (the DS silently dropped every commit).
    if commit_ops > 0 {
        let head = ds_head_epoch(&group_id).await;
        if head <= 0 {
            return Err(fail_msg(
                ops,
                &format!(
                    "DS head epoch is {head} after {commit_ops} committed membership op(s); the \
                     group should have advanced past creation"
                ),
            ));
        }
    }

    // ── Assertion 4: roster consistency — every current member's roster equals
    // the shadow model's current set ──────────────────────────────────────────
    let expected: BTreeSet<String> = current.iter().map(|&i| ids[i].clone()).collect();
    for &x in &current {
        let got: BTreeSet<String> =
            clients[x].group_member_ids(&group_id).await.into_iter().collect();
        if got != expected {
            return Err(fail_msg(
                ops,
                &format!(
                    "ROSTER DIVERGENCE: current member {x} sees {got:?}, shadow model expects \
                     {expected:?}"
                ),
            ));
        }
    }

    // Keep clients alive through the assertions (see harness docs: an early drop
    // can close a client's local DB mid-assertion).
    drop(clients);
    Ok(())
}

// ─── the proptest driver ─────────────────────────────────────────────────────

/// Generate random op/fault/offline sequences, force convergence, and assert the
/// bulletproof-membership invariant for every one of them. `#[serial]` because it
/// shares the singleton `WORLD` / in-process DS / `NEXT_DS_FAULT` with the rest of
/// the flows suite.
#[test]
#[serial]
fn model_based_convergence_is_bulletproof() {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(DEFAULT_CASES);

    let mut config = Config::default();
    config.cases = cases;
    // See the module header: MLS RNG isn't seedable from the harness, so a
    // persisted "regression" seed could fail to reproduce and mislead. The
    // printed op sequence is the repro of record.
    config.failure_persistence = None;

    let rt = runtime();
    let mut runner = TestRunner::new(config);
    let result = runner.run(&ops_strategy(), |ops| {
        rt.block_on(run_case(&ops, NACTORS))
            .map_err(|detail| TestCaseError::fail(detail))?;
        Ok(())
    });

    if let Err(err) = result {
        // `err`'s Display carries the minimal failing input (Op: Debug) plus our
        // embedded op-sequence detail — actionable even when shrinking is only
        // best-effort.
        panic!("model-based proptest found a failing case: {err}");
    }
}

/// Marathon soak: ONE crazy-long generated sequence — hundreds of ops over a
/// larger actor pool, with messages, membership churn, offline/online, and DS
/// faults all flying — run through the SAME shadow-oracle convergence check as
/// the property fuzzer. Where `model_based_convergence_is_bulletproof` runs many
/// SHORT cases, this runs ONE very LONG case, exercising deep interleavings of
/// the single convergence gate end to end.
///
/// KNOWN RESIDUAL (issue #442): at high op counts this soak still surfaces a
/// fork-recovery strand — a continuous member forced to external-join-rebuild
/// under a heavy fault storm loses the un-ingested backlog whose keys were on the
/// abandoned branch. That needs runtime tracing to pin (which fault forks the
/// member) and is deliberately NOT gated here; the per-PR fuzzer + deterministic
/// recovery repros are the gate. This soak is the reproduction tool for #442.
///
/// `#[ignore]`d by default (it's a multi-minute soak, too heavy for the per-PR
/// gate); run explicitly. Tunable via env for an even crazier run:
///   MARATHON_OPS (default 300), MARATHON_ACTORS (default 6).
/// e.g. `MARATHON_OPS=800 MARATHON_ACTORS=8 cargo test --features test-harness \
///        --test flows -- --ignored --nocapture model_marathon_convergence`
///
/// No shrinking (`max_shrink_iters = 0`): shrinking a several-hundred-op MLS
/// sequence is prohibitively slow and, per the module header, MLS RNG isn't
/// seedable — the printed op sequence on failure is the repro of record.
#[test]
#[serial]
#[ignore = "multi-minute soak; run explicitly (-- --ignored model_marathon_convergence)"]
fn model_marathon_convergence() {
    let ops_n: usize = std::env::var("MARATHON_OPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    let actors: usize = std::env::var("MARATHON_ACTORS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(6)
        .max(2);

    let mut config = Config::default();
    config.cases = 1;
    config.failure_persistence = None;
    config.max_shrink_iters = 0;

    let rt = runtime();
    let mut runner = TestRunner::new(config);
    let strat = proptest::collection::vec(op_strategy(actors as u8), ops_n..=ops_n);
    let result = runner.run(&strat, |ops| {
        rt.block_on(run_case(&ops, actors))
            .map_err(|detail| TestCaseError::fail(detail))?;
        Ok(())
    });

    if let Err(err) = result {
        panic!("marathon soak found a failing case: {err}");
    }
}
