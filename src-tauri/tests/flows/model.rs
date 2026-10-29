//! Model-based property fuzz layer for the adversarial MLS recovery suite.
//!
//! Generates random op/fault/offline sequences, forces convergence, and checks a shadow oracle.
//! A continuous member since a send MUST decrypt it; a non-member at send time must NOT.
//! Half the cases are born on `SUITE_LEGACY` and cross the suite boundary on `Upgrade`.
//! MLS RNG is not seedable, so shrinking is best-effort and the printed op sequence is the repro.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};
use serial_test::serial;

use crate::harness::{
    arm_ds_fault, clear_ds_fault, commit_budget_violation, ds_head_epoch, ds_head_generation,
    local_generation_of, republish_key_packages, set_current_suite, wipe, DsFault, TestClient,
    SUITE_LEGACY,
};

/// Index 0 is `alice`, owner and committer; the rest are the churn pool.
const NACTORS: usize = 4;

/// Modest CI default. Override for a local soak: `PROPTEST_CASES=2000 …`.
const DEFAULT_CASES: u32 = 32;

/// Bounded generated-sequence length. Kept short so a single case stays cheap.
const MIN_OPS: usize = 4;
const MAX_OPS: usize = 12;

/// Rounds of poll + process + fetch for every ever-member.
const CONVERGE_ROUNDS: usize = 4;
/// The probe adds no epoch, so fewer rounds suffice.
const PROBE_ROUNDS: usize = 2;

/// Crossings, so a run where none happened fails instead of passing vacuously.
static BOUNDARY_CROSSINGS: AtomicUsize = AtomicUsize::new(0);

fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        // `invoke` wraps `spawn_blocking`, which needs worker threads.
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime for the model proptest")
    })
}

#[derive(Clone, Debug)]
enum Op {
    Add(u8),
    Remove(u8),
    Send(u8),
    Sync(u8),
    /// Current member `a` rotates its own leaf (#666).
    Rotate(u8),
    /// Cold-launch sweep: the only op that drives a suite migration.
    Sweep(u8),
    /// The pin comes off and every actor republishes; legacy-born cases only.
    Upgrade,
    /// Arm a landing DS fault for the next commit-producing op.
    Fault(u8),
}

fn fault_variant(v: u8) -> DsFault {
    match v {
        0 => DsFault::Fail500PostWrite,
        1 => DsFault::DropResponse,
        _ => DsFault::DropWelcome,
    }
}

fn op_strategy(npool: u8) -> impl Strategy<Value = Op> {
    prop_oneof![
        // Weighted toward Send/Sync.
        3 => (1u8..npool).prop_map(Op::Add),
        2 => (1u8..npool).prop_map(Op::Remove),
        6 => (0u8..npool).prop_map(Op::Send),
        4 => (0u8..npool).prop_map(Op::Sync),
        3 => (0u8..npool).prop_map(Op::Rotate),
        3 => (0u8..npool).prop_map(Op::Sweep),
        2 => Just(Op::Upgrade),
        2 => (0u8..3u8).prop_map(Op::Fault),
    ]
}

/// `(start_legacy, ops)`: half the cases are born on the retired code point.
fn ops_strategy() -> impl Strategy<Value = (bool, Vec<Op>)> {
    (
        any::<bool>(),
        proptest::collection::vec(op_strategy(NACTORS as u8), MIN_OPS..=MAX_OPS),
    )
}

/// Invite, accept, drain the Welcome and replay commits.
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

async fn contents(client: &TestClient, channel_id: &str) -> Vec<String> {
    client
        .fetch_channel_messages(channel_id)
        .await
        .iter()
        .filter_map(|m| m["content"].as_str().map(str::to_string))
        .collect()
}

/// Poll, process and fetch for every ever-member.
async fn converge(clients: &[TestClient], ever: &BTreeSet<usize>, channel_id: &str, rounds: usize) {
    for _ in 0..rounds {
        for &i in ever {
            clients[i].poll().await;
            clients[i].process_commits_for(channel_id).await;
            let _ = clients[i].fetch_channel_messages(channel_id).await;
        }
    }
}

/// The detail plus the full op sequence, the authoritative repro.
fn fail_msg(ops: &[Op], detail: &str) -> String {
    format!("{detail}\n  op sequence ({} ops): {ops:?}", ops.len())
}

async fn run_case(ops: &[Op], nactors: usize, start_legacy: bool) -> Result<(), String> {
    // `wipe` also clears any suite pin a previous case left.
    wipe().await;
    clear_ds_fault();

    // Legacy-born: everyone publishes in `SUITE_LEGACY` and the group is born there.
    if start_legacy {
        set_current_suite(Some(SUITE_LEGACY));
    }

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

    // Shadow oracle: current/ever membership, stint start clocks, and sends.
    let mut current: BTreeSet<usize> = BTreeSet::from([0]);
    let mut ever: BTreeSet<usize> = BTreeSet::from([0]);
    let mut joined_at: HashMap<usize, usize> = HashMap::from([(0usize, 0usize)]);
    let mut messages: Vec<(String, BTreeSet<usize>, usize)> = Vec::new();
    let mut pending_fault: Option<DsFault> = None;
    let mut msg_seq: usize = 0;
    // Once shipped, the group must migrate at the next sweep.
    let mut upgraded = !start_legacy;
    let mut clock: usize = 0;
    // Landed Add/Remove ops; all-skipped sequences rightly stay at epoch 0.
    let mut commit_ops: usize = 0;

    for op in ops {
        clock += 1;
        match *op {
            Op::Add(t) => {
                let t = t as usize;
                if current.contains(&t) {
                    continue;
                }
                // Armed right before the consuming commit, so nothing is left armed.
                if let Some(f) = pending_fault.take() {
                    arm_ds_fault(f);
                }
                join_member(&clients[0], &clients[t], &group_id, &channel_id, &usernames[t]).await;
                commit_ops += 1;
                current.insert(t);
                ever.insert(t);
                // Overwrites a prior stint, so pre-removal messages aren't "continuous".
                joined_at.insert(t, clock);
            }
            Op::Remove(t) => {
                let t = t as usize;
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
                if !current.contains(&a) {
                    continue;
                }
                // Sync first so the send is at the live epoch, keeping the oracle sound.
                clients[a].poll().await;
                clients[a].process_commits_for(&channel_id).await;
                let body = format!("m{msg_seq}");
                msg_seq += 1;
                clients[a].send_channel_message(&channel_id, &body).await;
                messages.push((body, current.clone(), clock));
            }
            Op::Rotate(a) => {
                let a = a as usize;
                if !current.contains(&a) {
                    continue;
                }
                if let Some(f) = pending_fault.take() {
                    arm_ds_fault(f);
                }
                // Catch up first: a commit staged at a stale epoch loses the CAS.
                clients[a].poll().await;
                clients[a].process_commits_for(&channel_id).await;
                // A lost race produces no epoch; an error means a fault didn't recover.
                match clients[a].try_self_update(&group_id).await {
                    Ok(true) => commit_ops += 1,
                    Ok(false) => {}
                    Err(e) => {
                        return Err(fail_msg(
                            ops,
                            &format!("ROTATION FAILED: actor {a} could not rotate its own leaf: {e}"),
                        ));
                    }
                }
            }
            Op::Sync(a) => {
                let a = a as usize;
                clients[a].poll().await;
                if ever.contains(&a) {
                    clients[a].process_commits_for(&channel_id).await;
                    let _ = clients[a].fetch_channel_messages(&channel_id).await;
                }
            }
            Op::Sweep(a) => {
                let a = a as usize;
                clients[a].poll().await;
                clients[a].sweep().await;
                if ever.contains(&a) {
                    let _ = clients[a].fetch_channel_messages(&channel_id).await;
                }
            }
            Op::Upgrade => {
                if upgraded {
                    continue;
                }
                // Republishing drops retired-suite packages, so sweep immediately (twice: drain, then CAS).
                set_current_suite(None);
                for c in &clients {
                    republish_key_packages(c).await;
                }
                for _ in 0..2 {
                    clients[0].poll().await;
                    clients[0].sweep().await;
                }
                upgraded = true;
            }
            Op::Fault(v) => {
                pending_fault = Some(fault_variant(v));
            }
        }
    }

    clear_ds_fault();

    // Drive every member's sweep after an upgrade, so every such case crosses.
    if upgraded && start_legacy {
        for _ in 0..2 {
            for &i in &ever {
                clients[i].poll().await;
                clients[i].sweep().await;
            }
        }
    }
    converge(&clients, &ever, &channel_id, CONVERGE_ROUNDS).await;

    // Probe: every current member must decrypt a fresh alice message.
    clients[0].poll().await;
    clients[0].process_commits_for(&channel_id).await;
    let probe = format!("probe{msg_seq}");
    clients[0].send_channel_message(&channel_id, &probe).await;
    let probe_at = clock + 1;
    messages.push((probe.clone(), current.clone(), probe_at));
    converge(&clients, &ever, &channel_id, PROBE_ROUNDS).await;

    let mut view: HashMap<usize, Vec<String>> = HashMap::new();
    for &x in &ever {
        view.insert(x, contents(&clients[x], &channel_id).await);
    }

    // Assertions 1 + 3: positive delivery and blackout.
    for (body, snap, sent_at) in &messages {
        for &x in &ever {
            let has = view[&x].contains(body);
            let in_snap = snap.contains(&x);
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

    // Assertion 2: no current member is wedged (all decrypt the probe).
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
    // Demand a head past genesis only if a commit actually landed.
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

    // Assertion 4: each roster equals the shadow model's.
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

    // Assertion 5: migrate exactly when every roster device republished, all onto one generation.
    let head_generation = ds_head_generation(&group_id).await;
    let expected_generation = i64::from(start_legacy && upgraded);
    if expected_generation > 0 {
        BOUNDARY_CROSSINGS.fetch_add(1, Ordering::Relaxed);
    }
    if head_generation != expected_generation {
        return Err(fail_msg(
            ops,
            &format!(
                "SUITE GENERATION: head is {head_generation}, expected {expected_generation} \
                 (born {}, build {})",
                if start_legacy { "legacy" } else { "current" },
                if upgraded { "shipped" } else { "not shipped" },
            ),
        ));
    }
    for &x in &current {
        let local = local_generation_of(&clients[x], &group_id).await;
        if local != head_generation {
            return Err(fail_msg(
                ops,
                &format!(
                    "STRANDED ON A RETIRED LINEAGE: current member {x} is on generation {local} \
                     while the group's head is {head_generation}. The predecessor takes no \
                     further commits, so this member is wedged forever."
                ),
            ));
        }
    }

    // Assertion 6: payload size (§4.3).
    if let Some(detail) = commit_budget_violation(&group_id).await {
        return Err(fail_msg(
            ops,
            &format!(
                "COMMIT BALLOON: {detail}. A commit that size is carrying something that does not \
                 belong in an UpdatePath."
            ),
        ));
    }

    // Keep clients alive through the assertions.
    drop(clients);
    // Don't leave the last case's pin on for later tests.
    set_current_suite(None);
    Ok(())
}

/// Property fuzz over the shared singleton world, hence `#[serial]`.
#[test]
#[serial]
fn model_based_convergence_is_bulletproof() {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(DEFAULT_CASES);

    let mut config = Config::default();
    config.cases = cases;
    // MLS RNG isn't seedable, so a persisted seed could mislead.
    config.failure_persistence = None;

    let rt = runtime();
    let mut runner = TestRunner::new(config);
    let result = runner.run(&ops_strategy(), |(start_legacy, ops)| {
        rt.block_on(run_case(&ops, NACTORS, start_legacy))
            .map_err(|detail| TestCaseError::fail(detail))?;
        Ok(())
    });

    if let Err(err) = result {
        panic!("model-based proptest found a failing case: {err}");
    }

    // Zero crossings is a generator regression, so fail.
    let crossings = BOUNDARY_CROSSINGS.swap(0, Ordering::Relaxed);
    eprintln!("[model] {cases} cases, {crossings} crossed the suite boundary");
    assert!(
        crossings > 0,
        "no generated case crossed the suite boundary in {cases} cases — the migration \
         path went completely untested and assertion 5 passed vacuously"
    );
}

/// Marathon soak (`#[ignore]`, nightly in mls-tests.yml): one long case; reproduces #442. Env: MARATHON_OPS, MARATHON_ACTORS.
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
    // Always LEGACY: one long run gets one shot at the boundary.
    let strat = proptest::collection::vec(op_strategy(actors as u8), ops_n..=ops_n);
    let result = runner.run(&strat, |ops| {
        rt.block_on(run_case(&ops, actors, true))
            .map_err(|detail| TestCaseError::fail(detail))?;
        Ok(())
    });

    if let Err(err) = result {
        panic!("marathon soak found a failing case: {err}");
    }
}
