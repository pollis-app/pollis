//! #875 — a REGRESSION GUARD, not a proof: no DS write body leaves its actor
//! field unset.
//!
//! With DS `require_auth` off, the body's actor field is required; a `None`
//! there works against production and fails on any unsigned deployment.

use std::path::{Path, PathBuf};

/// Field names `pollis-delivery` reads as "who is acting".
const ACTOR_FIELDS: &[&str] = &[
    "actor_id",
    "approver_id",
    "blocker_id",
    "claimer_id",
    "created_by",
    "creator_id",
    "inviter_id",
    "owner_id",
    "requester_id",
    "sender_id",
    "user_id",
];

/// Call sites allowed to pass `None`, each with the reason it is not an actor.
const ALLOWED: &[(&str, &str, &str)] = &[];

fn command_sources() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands");
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read_dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Walk every `pollis_api::…` struct literal in the command layer and refuse a
/// bare `None` on any field the DS reads as the actor.
#[test]
fn no_ds_write_body_sets_its_actor_field_to_none() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();

    for path in command_sources() {
        let rel = path
            .strip_prefix(root)
            .expect("under the manifest dir")
            .to_string_lossy()
            .replace('\\', "/");
        let body = std::fs::read_to_string(&path).expect("read");

        // Only inside a `pollis_api::…Body { … }` literal.
        let mut depth: i32 = 0;
        for (i, line) in body.lines().enumerate() {
            if depth == 0 {
                if line.contains("pollis_api::") && line.trim_end().ends_with('{') {
                    depth = 1;
                }
                continue;
            }

            let trimmed = line.trim();
            for field in ACTOR_FIELDS {
                if trimmed == format!("{field}: None,") {
                    let allowed = ALLOWED
                        .iter()
                        .any(|(f, fld, _)| *f == rel && fld == field);
                    if !allowed {
                        offenders.push(format!("{rel}:{}: {field}: None", i + 1));
                    }
                }
            }

            depth += line.matches('{').count() as i32;
            depth -= line.matches('}').count() as i32;
            if depth <= 0 {
                depth = 0;
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "these DS write bodies do not name their acting user, so they will be \
         refused on any deployment running with POLLIS_DS_REQUIRE_AUTH off:\n  {}\n\n\
         Send `Some(current_user_id(state).await?)` (or the command's own user_id). \
         Naming it is never a privilege escalation — with auth ON the DS requires it \
         to EQUAL the signer and refuses otherwise.",
        offenders.join("\n  ")
    );
}

/// Control: the scanner can actually see an offending line.
#[test]
fn the_scanner_recognises_an_offending_line() {
    let sample = "    let b = pollis_api::emoji::RemoveEmojiBody {\n\
                  \x20       group_id: g,\n\
                  \x20       actor_id: None,\n\
                  \x20   };\n";
    let mut depth: i32 = 0;
    let mut hits = 0;
    for line in sample.lines() {
        if depth == 0 {
            if line.contains("pollis_api::") && line.trim_end().ends_with('{') {
                depth = 1;
            }
            continue;
        }
        if ACTOR_FIELDS
            .iter()
            .any(|f| line.trim() == format!("{f}: None,"))
        {
            hits += 1;
        }
        depth += line.matches('{').count() as i32;
        depth -= line.matches('}').count() as i32;
        if depth <= 0 {
            depth = 0;
        }
    }
    assert_eq!(
        hits, 1,
        "the scan must flag `actor_id: None` inside a pollis_api literal"
    );
}

#[test]
fn the_actor_field_list_is_populated() {
    assert!(
        ACTOR_FIELDS.len() >= 10,
        "ACTOR_FIELDS lost entries; the scan is only as good as this list"
    );
}
