//! The voice-join path must not issue one DS round trip per person in the room
//! (#1045).
//!
//! Seeding the participant roster used to call `lookup_avatar_url_for_identity`
//! once per existing participant, awaited one at a time, as the LAST step before
//! `join_voice_channel` returned. Every request is a real HTTPS round trip to the
//! Delivery Service since #988, so a ten-person channel paid eleven of them in
//! series while the user stared at a dead call — and the busier the channel, the
//! slower the join. The case that should feel fastest was the slowest.
//!
//! The fix batches them into one `lookup_avatar_urls_for_identities`. This guard
//! exists because the regression is **invisible in a passing test**: a loop of
//! awaits is correct, returns the right avatars, and shows up only as latency
//! that grows with a room size no unit test sets up. A reviewer would have to
//! notice the `.await` inside the loop.
//!
//! ## How it decides
//!
//! It reads the roster-seeding phase — the span between the `roster_start` and
//! `roster_ms` timing markers — and fails on a per-identity avatar lookup there,
//! or on any Delivery Service read awaited inside a per-participant loop. It also
//! asserts the batched call is still present, so the guard cannot pass by the
//! phase being deleted.
//!
//! Reading source text is a lower bound, not a proof — a fan-out assembled
//! through a helper would slip past — but it is exactly the shape the defect had,
//! and the shape the obvious "just add the avatar here" edit would reintroduce.
//! Verified by mutation: restoring the loop fails both tests.
//!
//! Deliberately NOT banned: the single `lookup_avatar_url_for_identity` in the
//! participant-connected event handler. That is one lookup for one arriving
//! person, which is O(1) per event and correct.

use std::path::PathBuf;

/// The join path: everything `join_voice_channel` runs before it returns.
fn lifecycle_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/commands/voice/lifecycle.rs");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The roster-seeding region: `let roster_start` … `let roster_ms`.
///
/// Scoped to that span rather than to the whole function on purpose. The room
/// event-handler closure is *registered* inside `join_voice_channel`, and it
/// legitimately looks up one avatar for one arriving participant — banning the
/// call across the whole function would forbid the correct thing along with the
/// wrong one. The N+1 lived in this span, and this span is where it would come
/// back. Both markers are the timing instrumentation the phase already has, so
/// they cannot be deleted without the timings noticing.
fn roster_seeding_region(source: &str) -> String {
    let start = source
        .find("let roster_start")
        .expect("the roster phase is timed with `let roster_start` in lifecycle.rs");
    let end = source
        .find("let roster_ms")
        .expect("the roster phase is timed with `let roster_ms` in lifecycle.rs");
    assert!(end > start, "roster_ms precedes roster_start — markers moved");
    source[start..end].to_string()
}

/// Source lines with `//` comments stripped, so prose describing the old defect
/// does not read as the defect.
fn code_lines(region: &str) -> Vec<(usize, String)> {
    region
        .lines()
        .enumerate()
        .map(|(i, line)| {
            let code = match line.find("//") {
                Some(at) => &line[..at],
                None => line,
            };
            (i + 1, code.trim().to_string())
        })
        .filter(|(_, code)| !code.is_empty())
        .collect()
}

#[test]
fn the_join_path_does_not_look_up_avatars_one_participant_at_a_time() {
    let source = lifecycle_source();
    let region = roster_seeding_region(&source);

    let offenders: Vec<(usize, String)> = code_lines(&region)
        .into_iter()
        .filter(|(_, code)| code.contains("lookup_avatar_url_for_identity"))
        .collect();

    assert!(
        offenders.is_empty(),
        "the roster-seeding phase calls lookup_avatar_url_for_identity, which is one \
         DS round trip per person and turns a busy channel into a slow join. Seed the \
         roster with lookup_avatar_urls_for_identities instead — one request for \
         everyone. Offending lines (relative to the phase):\n{}",
        offenders
            .iter()
            .map(|(n, l)| format!("  {n}: {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );

    // And the batched call must actually be the one in use, so this test cannot
    // pass by the roster seeding being deleted or quietly moved elsewhere.
    assert!(
        region.contains("lookup_avatar_urls_for_identities"),
        "the roster-seeding phase no longer makes one batched avatar request — if \
         that moved, move this guard with it",
    );
}

/// The `.await`-in-a-loop shape, stated generally enough to catch the next one.
///
/// The avatar fan-out was not special: any per-participant DS read on the join
/// path costs a round trip per person. This checks the roster-seeding region for
/// an `await` inside a `for` over participants.
#[test]
fn the_join_path_does_not_await_a_ds_read_per_participant() {
    let source = lifecycle_source();
    let body = roster_seeding_region(&source);

    let mut in_participant_loop = false;
    let mut depth_at_loop = 0usize;
    let mut depth = 0usize;
    let mut offenders: Vec<String> = Vec::new();

    for line in body.lines() {
        let trimmed = line.trim();

        if in_participant_loop && trimmed.contains(".await") {
            // `ds_` names the Delivery Service client; `lookup_` is the avatar/
            // pseudonym family that goes through it.
            if trimmed.contains("ds_") || trimmed.contains("lookup_") {
                offenders.push(trimmed.to_string());
            }
        }

        depth += line.matches('{').count();
        depth = depth.saturating_sub(line.matches('}').count());

        if in_participant_loop && depth <= depth_at_loop {
            in_participant_loop = false;
        }
        if trimmed.starts_with("for ")
            && (trimmed.contains("participant") || trimmed.contains("identit"))
        {
            in_participant_loop = true;
            depth_at_loop = depth.saturating_sub(1);
        }
    }

    assert!(
        offenders.is_empty(),
        "the roster-seeding phase awaits a Delivery Service read inside a \
         per-participant loop — that is one network round trip per person in the room, in series, \
         between the user and a live call. Batch it. Offending lines:\n{}",
        offenders
            .iter()
            .map(|l| format!("  {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}
