//! The voice-join path must not issue one DS round trip per person in the room
//! (#1045).
//!
//! Reads source text, so it is a lower bound: a fan-out through a helper slips past.

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
/// Not the whole function: its event handler rightly looks up one arriving avatar.
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

/// Source lines with `//` comments stripped.
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

    // So the test cannot pass by the phase being deleted.
    assert!(
        region.contains("lookup_avatar_urls_for_identities"),
        "the roster-seeding phase no longer makes one batched avatar request — if \
         that moved, move this guard with it",
    );
}

/// The `.await`-in-a-loop shape, stated generally enough to catch the next one.
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
