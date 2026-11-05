//! The voice-join path must not issue one DS round trip per person in the room
//! (#1045).
//!
//! Reads source text, so it is a lower bound: a fan-out through a helper slips past.

use std::path::PathBuf;

/// The join path: everything `join_voice_channel` runs before it returns, plus
/// the helpers that seed the roster for it.
fn lifecycle_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/commands/voice/lifecycle.rs");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The body of `fn name(…) { … }` (first brace after the signature to its match), or `None`.
fn fn_body(source: &str, name: &str) -> Option<String> {
    let needle = format!("fn {name}(");
    let start = source.find(&needle)?;
    let open = start + source[start..].find('{')?;
    let mut depth = 0usize;
    for (i, c) in source[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(source[open..open + i + 1].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// The functions that seed the roster at join, in whichever shape this revision has:
/// the join itself, the snapshot helper(s), and the avatar follow-up if avatars left the join.
const SEED_PATH: &[&str] = &[
    "join_inner",
    "roster_snapshot",
    "roster_identities",
    "deliver_avatars",
    "reseed_roster",
];

fn seed_path(source: &str) -> Vec<(&'static str, String)> {
    let found: Vec<(&'static str, String)> = SEED_PATH
        .iter()
        .filter_map(|name| fn_body(source, name).map(|body| (*name, body)))
        .collect();
    assert!(
        found.iter().any(|(n, _)| *n == "join_inner"),
        "lifecycle.rs no longer has `join_inner`; point SEED_PATH at the join's new shape"
    );
    found
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
    let mut offenders: Vec<String> = Vec::new();
    let mut batched = 0usize;
    for (name, body) in seed_path(&source) {
        for (n, code) in code_lines(&body) {
            if code.contains("lookup_avatar_url_for_identity(") {
                offenders.push(format!("  {name}:{n}: {code}"));
            }
            batched += code.matches("lookup_avatar_urls_for_identities(").count();
        }
    }

    assert!(
        offenders.is_empty(),
        "the roster-seeding path calls lookup_avatar_url_for_identity, which is one \
         DS round trip per person and turns a busy channel into a slow join. Seed the \
         roster with lookup_avatar_urls_for_identities instead — one request for \
         everyone. Offending lines:\n{}",
        offenders.join("\n"),
    );

    // Exactly one batched request for the whole seed roster — on the join, or in the
    // follow-up that delivers avatars after it returned. Zero means the phase was
    // deleted; two means someone is asking twice.
    assert_eq!(
        batched, 1,
        "the roster-seeding path must make exactly ONE batched avatar request \
         (lookup_avatar_urls_for_identities) — if that moved, move this guard with it"
    );
}

/// The `.await`-in-a-loop shape, stated generally enough to catch the next one.
#[test]
fn the_join_path_does_not_await_a_ds_read_per_participant() {
    let source = lifecycle_source();
    let mut offenders: Vec<String> = Vec::new();

    for (name, body) in seed_path(&source) {
        let mut in_participant_loop = false;
        let mut depth_at_loop = 0usize;
        let mut depth = 0usize;

        for line in body.lines() {
            let trimmed = line.trim();

            if in_participant_loop && trimmed.contains(".await") {
                // `ds_` is the DS client; `lookup_` goes through it.
                if trimmed.contains("ds_") || trimmed.contains("lookup_") {
                    offenders.push(format!("  {name}: {trimmed}"));
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
    }

    assert!(
        offenders.is_empty(),
        "the roster-seeding path awaits a Delivery Service read inside a \
         per-participant loop — that is one network round trip per person in the room, in series, \
         between the user and a live call. Batch it. Offending lines:\n{}",
        offenders.join("\n"),
    );
}
