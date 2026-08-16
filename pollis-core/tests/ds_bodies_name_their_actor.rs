//! #875 — a REGRESSION GUARD, not a proof: no DS write body leaves its actor
//! field unset.
//!
//! ## The failure this guards
//!
//! `pollis-delivery` resolves the acting user for every write through one of
//! three helpers (`writes::resolve_actor`, `broker::resolve_user`,
//! `writes::resolve_recipient`), all with the same rule:
//!
//! | DS `require_auth` | actor comes from | body's actor field |
//! | --- | --- | --- |
//! | **on** (production) | the device-signed `X-Pollis-User` | must EQUAL the signer, else `403` |
//! | **off** | the body's actor field | **required** — missing/empty is `403`/`400` |
//!
//! So a body without it is perfectly healthy against a signed deployment and a
//! hard refusal against an unsigned one — invisible to anyone running with auth
//! on, and a total outage for anyone running with it off. It has been found and
//! fixed three times: `/v1/messages/delete` (×2) and `/v1/emoji/*` earlier in
//! #875, then twelve account/enrollment/welcome endpoints and the four broker
//! endpoints when typing the client surfaced them all at once.
//!
//! ## What typing did and did not fix
//!
//! Typed bodies (`pollis-api`) made OMITTING the field impossible: a Rust struct
//! literal must name every field, so a forgotten actor is a compile error. But
//! `user_id: None` is a perfectly legal way to name it, and it reproduces the
//! outage exactly. The type system cannot tell a deliberate `None` from a lazy
//! one — which is what this scan is for.
//!
//! ## Why a source scan
//!
//! The property is about a VALUE at every call site, so nothing in the type
//! system can carry it, and a runtime test would need every command driven
//! against a no-auth DS. `pollis-delivery/tests/schema_is_not_reinvented.rs`
//! establishes the pattern: when the failure mode is "someone adds a convenient
//! line that no assertion elsewhere would notice", grep for the line.
//!
//! This is a guard. It proves no call site currently sets an actor field to a
//! bare `None`; it does NOT prove the values sent are correct, and it cannot see
//! a body built through a helper that returns `None` from elsewhere.

use std::path::{Path, PathBuf};

/// Field names `pollis-delivery` reads as "who is acting", across all three
/// resolver helpers. Taken from the `*Body` definitions in `pollis-api` that
/// document a field as the no-auth actor/recipient.
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
///
/// Empty, and that is the point: every DS write the client makes currently names
/// its acting user. Adding an entry here is a deliberate claim that the field is
/// not the DS's actor for that endpoint — check the handler's `resolve_*` line
/// before you make it.
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

        // Track whether we are inside a `pollis_api::…Body { … }` literal, so a
        // `None` in unrelated code (a local struct, a match arm) is not flagged.
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
///
/// Without this, `no_ds_write_body_sets_its_actor_field_to_none` would pass just
/// as happily if the literal-tracking were broken and it inspected nothing at
/// all — which is the most likely way a source scan silently stops working.
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

/// The field list is not allowed to quietly empty out — an empty `ACTOR_FIELDS`
/// would make the scan above pass against anything.
#[test]
fn the_actor_field_list_is_populated() {
    assert!(
        ACTOR_FIELDS.len() >= 10,
        "ACTOR_FIELDS lost entries; the scan is only as good as this list"
    );
}
