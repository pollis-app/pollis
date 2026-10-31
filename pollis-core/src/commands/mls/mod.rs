//! MLS commands, re-exported at `pollis_core::commands::mls::*`.

mod delivery;
mod device;
pub(crate) mod ds_client;
pub(crate) mod ds_reads;
pub(crate) mod generation;
mod group_state;
pub mod invariants;
mod join_branch;
mod key_packages;
mod migrate;
// `pub(crate)`: the media-only voice key export builds its own provider.
pub(crate) mod provider;
#[cfg(feature = "fuzzing")]
pub mod fuzz_surface;
mod reconcile;
mod self_update;
mod sweep;
mod welcomes;

pub use provider::{
    make_credential, parse_credential_device_id, parse_credential_user_id, PollisProvider,
};
// Voice key export; media builds only.
#[cfg(feature = "media")]
pub(crate) use provider::MlsProvider;
pub(crate) use join_branch::{
    entitled_since, first_join, parse_ds_instant, record_first_join, Entitlement,
};
// Suite-migration seam for the flows harness (#669).
#[cfg(feature = "test-harness")]
pub use provider::set_current_suite_override;

pub use device::{
    ensure_device_cert, load_device_cert_pubs, load_device_pq_signing_key,
    load_or_create_device_signer, resign_stale_device_certs, stale_cert_candidates,
    AddedLeaf, IdentityDirectory, LeafVerdict,
};

pub(crate) use ds_client::{
    current_user_id, decode_response, ds_claim_key_package, ds_livekit_send_data,
    ds_livekit_token, ds_post, ds_post_envelope, ds_post_json, ds_post_ok, ds_post_plain,
    ds_post_session_ok, ds_post_signed_or_session, ds_post_signed_or_session_json,
    ds_post_signed_or_session_ok, EnvelopePost,
};
// Desktop-only (voice roster); mobile has no Rust-side participants path.
#[cfg(feature = "media")]
pub(crate) use ds_client::ds_livekit_participants;
// #836 identity resolution: desktop's realtime loop, and mobile's JS data channel (#1635).
pub(crate) use ds_client::ds_livekit_identities;

pub use key_packages::{ensure_mls_key_package, validate_key_package};
// An uncertified-key KeyPackage: the flows suite's malicious-DS model.
#[cfg(feature = "test-harness")]
pub use key_packages::forge_key_package_for;

pub use welcomes::{
    apply_welcome, poll_mls_welcomes, poll_mls_welcomes_inner, reset_welcome_delivery,
};

pub use group_state::{
    envelope_lineage, external_join_group, forget_local_mls_group, has_local_group, init_mls_group,
    process_pending_commits, process_pending_commits_inner, process_pending_commits_inner_with_hook,
    publish_group_info, try_mls_decrypt, try_mls_decrypt_why, try_mls_encrypt, DecryptFailure,
    DecryptFailureKind, MlsDecryptor, ReplayBound,
};
// Welcome/external-join interleavings for the flows suite (#1041).
#[cfg(feature = "test-harness")]
pub use group_state::rendezvous;

pub use sweep::catch_up_all_mls_groups;
pub use sweep::{backstop_before_seal, reconcile_backstop_all, reconcile_backstop_one, BackstopReport};
pub use sweep::apply_membership_wake;

pub use self_update::{self_update_group, self_update_if_due};

pub use reconcile::{
    reconcile_group_mls_core, reconcile_group_mls_core_staged, reconcile_group_mls_impl,
    ReconcileCommitData, ReconcileOutcome,
};
// Leaf cross-signing seams for the flows suite.
#[cfg(feature = "test-harness")]
pub use reconcile::{local_epoch_and_pending, local_tree_members, set_skip_committer_leaf_check};

#[cfg(test)]
mod tests;

/// Where an epoch may be advanced, enforced on the source (#1079).
#[cfg(test)]
mod merge_site_tests {
    use std::path::{Path, PathBuf};

    /// The `(file, enclosing fn)` pairs allowed to call `merge_pending_commit`:
    ///
    /// | site | why it may merge |
    /// |---|---|
    /// | `reconcile::merge_pending_in_suite` | reached only from `finalize_won_commit` — the log said we WON this epoch |
    /// | `reconcile::reconcile_group_mls_core` | merges the commit it just built, having already published it |
    /// | `reconcile::stage_reconcile_commit` | resolves a dangling commit under the MLS lock, after a catch-up, before staging a new one |
    /// | `self_update::stage_self_update` | same, for the own-leaf rotation |
    /// | `group_state::apply_one_commit` | replaying a commit the log holds — that IS the log's answer |
    const ALLOWED: &[(&str, &str)] = &[
        ("commands/mls/reconcile.rs", "merge_pending_in_suite"),
        ("commands/mls/reconcile.rs", "reconcile_group_mls_core"),
        ("commands/mls/reconcile.rs", "stage_reconcile_commit"),
        ("commands/mls/self_update.rs", "stage_self_update"),
        ("commands/mls/group_state.rs", "apply_one_commit"),
    ];

    /// Merging a staged commit advances the epoch, so only a site that knows the log's answer may do it (#1079).
    #[test]
    fn no_new_site_merges_a_pending_commit() {
        let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rs(&src_root, &mut files);
        assert!(
            files.len() > 50,
            "walked only {} files under {} — the walk is broken, not clean",
            files.len(),
            src_root.display()
        );

        // Split so this file does not match itself.
        let needle = concat!("merge_pending", "_commit(");

        let mut offenders = Vec::new();
        for file in &files {
            let rel = file
                .strip_prefix(&src_root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/");
            // `tests.rs` is `#[cfg(test)]`.
            if rel.ends_with("/tests.rs") || rel.ends_with("tests.rs") {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(file) else {
                continue;
            };
            let lines: Vec<&str> = src.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if !line.contains(needle) {
                    continue;
                }
                let t = line.trim_start();
                if t.starts_with("//") || t.starts_with("///") {
                    continue;
                }
                let enclosing = enclosing_fn(&lines, i).unwrap_or_else(|| "<none>".to_string());
                if ALLOWED
                    .iter()
                    .any(|(f, fun)| rel == *f && enclosing == *fun)
                {
                    continue;
                }
                offenders.push(format!("{rel}:{} in fn {enclosing}\n      {t}", i + 1));
            }
        }

        assert!(
            offenders.is_empty(),
            "merging a pending commit advances this device's epoch onto a branch the commit log \
             may never hold (#1079). Only a site that already knows the log's answer may do it — \
             see `ALLOWED` above. New sites:\n  {}",
            offenders.join("\n  ")
        );

        // And the allowlist must not rot: every entry has to still exist, or a
        // deleted site would leave a permanent licence behind.
        for (f, fun) in ALLOWED {
            let path = src_root.join(f);
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("allowlisted file {f} is unreadable: {e}"));
            let lines: Vec<&str> = src.lines().collect();
            let found = lines.iter().enumerate().any(|(i, l)| {
                l.contains(concat!("merge_pending", "_commit("))
                    && !l.trim_start().starts_with("//")
                    && enclosing_fn(&lines, i).as_deref() == Some(fun)
            });
            assert!(
                found,
                "allowlist entry ({f}, {fun}) no longer merges a pending commit — drop it rather \
                 than leaving a standing licence"
            );
        }
    }

    /// Each merge must first keep the epoch's file KEK (`file_keys::keep_epoch_kek`, #1382), or a device yet to adopt the ring is stranded.
    #[test]
    fn every_merge_site_keeps_the_file_kek() {
        let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rs(&src_root, &mut files);
        let needles = [concat!("merge_pending", "_commit("), concat!("merge_staged", "_commit(")];
        let keep = concat!("keep_epoch", "_kek(");
        let mut checked = 0;
        let mut offenders = Vec::new();
        for file in &files {
            let rel = file
                .strip_prefix(&src_root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/");
            if rel.ends_with("tests.rs") {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(file) else {
                continue;
            };
            let lines: Vec<&str> = src.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                let t = line.trim_start();
                if t.starts_with("//") || !needles.iter().any(|n| line.contains(n)) {
                    continue;
                }
                checked += 1;
                let Some(fun) = enclosing_fn(&lines, i) else {
                    continue;
                };
                // Walk back to the enclosing fn's declaration line.
                let kept = lines[..i]
                    .iter()
                    .rev()
                    .take_while(|l| enclosing_fn(&[l], 0).as_deref() != Some(fun.as_str()))
                    .any(|l| l.contains(keep));
                if !kept {
                    offenders.push(format!("{rel}:{} in fn {fun}", i + 1));
                }
            }
        }
        assert!(checked >= 5, "found only {checked} merge sites — the walk is broken, not clean");
        assert!(
            offenders.is_empty(),
            "these merges leave an epoch without keeping its file KEK \
             (`crate::commands::file_keys::keep_epoch_kek`):\n  {}",
            offenders.join("\n  ")
        );
    }

    /// The name of the nearest `fn` at or above `line`. Textual, matching the
    /// declaration forms this crate actually uses.
    fn enclosing_fn(lines: &[&str], line: usize) -> Option<String> {
        for l in lines[..=line].iter().rev() {
            let t = l.trim_start();
            let t = t.strip_prefix("pub ").unwrap_or(t);
            let t = match t.find(") ") {
                // `pub(super) fn` / `pub(crate) fn`
                Some(i) if t.starts_with("pub(") => &t[i + 2..],
                _ => t,
            };
            let t = t.strip_prefix("async ").unwrap_or(t);
            if let Some(rest) = t.strip_prefix("fn ") {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    return Some(name);
                }
            }
        }
        None
    }

    fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }
}
