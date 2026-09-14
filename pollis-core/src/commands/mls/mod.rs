//! MLS commands — split into cohesive submodules. Public surface is
//! preserved via the `pub use` re-exports below so every external caller
//! (Tauri shims, sibling `commands::*` modules, integration tests) keeps
//! resolving names at `pollis_core::commands::mls::*`.

mod delivery;
mod device;
pub(crate) mod ds_client;
pub(crate) mod ds_reads;
pub(crate) mod generation;
mod group_state;
pub mod invariants;
mod key_packages;
mod migrate;
// `pub(crate)` because the media-only voice key export builds its own provider
// outside this module.
pub(crate) mod provider;
mod reconcile;
mod self_update;
mod sweep;
mod welcomes;

// ── Provider / credential helpers ────────────────────────────────────────────
pub use provider::{
    make_credential, parse_credential_device_id, parse_credential_user_id, PollisProvider,
};
// Out-of-module MLS crypto (voice key export). Only the `media` build has such
// a call site.
#[cfg(feature = "media")]
pub(crate) use provider::MlsProvider;
// The one seam the flows harness needs to exercise suite migration now that
// production ships a single suite (#669) — see `provider::current_suite`.
#[cfg(feature = "test-harness")]
pub use provider::set_current_suite_override;

// ── Per-device signing keys + cross-signing ──────────────────────────────────
pub use device::{
    ensure_device_cert, load_device_cert_pubs, load_device_pq_signing_key,
    load_or_create_device_signer, resign_stale_device_certs, stale_cert_candidates,
    AddedLeaf, IdentityDirectory, LeafVerdict,
};

// ── Signed Delivery-Service write client (4 `X-Pollis-*` headers) ────────────
pub(crate) use ds_client::{
    current_user_id, decode_response, ds_claim_key_package, ds_livekit_send_data,
    ds_livekit_token, ds_post, ds_post_envelope, ds_post_json, ds_post_ok, ds_post_plain,
    ds_post_session_ok, ds_post_signed_or_session, ds_post_signed_or_session_ok, EnvelopePost,
};
// Desktop-only (voice roster); mobile has no Rust-side participants path.
#[cfg(feature = "media")]
pub(crate) use ds_client::ds_livekit_participants;
// #836 identity resolution is media-only: the headless/mobile build drives
// LiveKit through the native SDK and never sees a Rust-side participant
// identity, so neither the resolver nor its cache exists there.
#[cfg(feature = "media")]
pub(crate) use ds_client::ds_livekit_identities;

// ── Key packages ─────────────────────────────────────────────────────────────
pub use key_packages::{ensure_mls_key_package, validate_key_package};
// A KeyPackage that claims a `user:device` credential with a key the user never
// certified — the flows suite's model of a DS substituting an attacker's package.
#[cfg(feature = "test-harness")]
pub use key_packages::forge_key_package_for;

// ── Welcomes ─────────────────────────────────────────────────────────────────
pub use welcomes::{
    apply_welcome, poll_mls_welcomes, poll_mls_welcomes_inner, reset_welcome_delivery,
};

// ── Group lifecycle / encrypt / decrypt / commit processing ──────────────────
pub use group_state::{
    envelope_lineage, external_join_group, forget_local_mls_group, has_local_group, init_mls_group,
    process_pending_commits, process_pending_commits_inner, process_pending_commits_inner_with_hook,
    publish_group_info, try_mls_decrypt, try_mls_encrypt, MlsDecryptor, ReplayBound,
};
// Deterministic interleavings of the Welcome and external-join paths for the
// flows suite (#1041) — see `group_state::rendezvous`.
#[cfg(feature = "test-harness")]
pub use group_state::rendezvous;

// ── Cold-launch / post-reconnect sweep ──────────────────────────────────────
pub use sweep::catch_up_all_mls_groups;
pub use sweep::apply_membership_wake;

// ── Own-leaf rotation (post-join merge + periodic PCS) ───────────────────────
pub use self_update::{self_update_group, self_update_if_due};

// ── Reconcile + self-repair ──────────────────────────────────────────────────
pub use reconcile::{
    reconcile_group_mls_core, reconcile_group_mls_core_staged, reconcile_group_mls_impl,
    ReconcileCommitData, ReconcileOutcome,
};
// Leaf cross-signing seams for the flows suite: play a pre-fix committer, and
// read the local tree back to assert what was admitted or evicted.
#[cfg(feature = "test-harness")]
pub use reconcile::{local_epoch_and_pending, local_tree_members, set_skip_committer_leaf_check};

#[cfg(test)]
mod tests;

/// Where an epoch may be advanced, enforced on the source (#1079).
#[cfg(test)]
mod merge_site_tests {
    use std::path::{Path, PathBuf};

    /// The `(file, enclosing fn)` pairs allowed to call
    /// `merge_pending_commit`. Each one is a place where the commit log's answer
    /// is already known, or where the caller holds the per-conversation MLS lock
    /// and is about to stage its own commit:
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

    /// Merging a staged commit advances this device's epoch, so it is a decision
    /// about which branch of history this device is on — and only the commit log
    /// gets to make it.
    ///
    /// `load_group_with_signer` used to merge unconditionally, which put that
    /// decision behind a function whose job is "give me this group". The
    /// consequence was #1079: `try_mls_encrypt`, reached from
    /// `receipts::emit_receipt` (which by design takes no MLS lock and runs at
    /// the end of every DM catch-up), merged a committer's staged commit
    /// mid-flight. On a lost race the device sat at a phantom epoch on a branch
    /// the log never held, invisible to `invariants::resolve`; on a won one
    /// `sweep_before_merge` opened its decryptor at the already-advanced lineage
    /// and skipped every envelope sealed at the closing epoch — the exact
    /// committer-arm loss #1041 exists to prevent.
    ///
    /// The fix was to move the merge to the callers that have the log's answer.
    /// This test is what keeps it there: a refactor that reintroduces a merge
    /// anywhere else fails here rather than in production, six months later, as
    /// a message that silently never arrived.
    ///
    /// A source-shape guard, not a proof — it cannot see a merge reached through
    /// a function pointer or a macro. It closes the case that actually happened.
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

        // Split so this file's own source does not contain the needle and match
        // itself. `concat!` is compile-time, so the check is exact.
        let needle = concat!("merge_pending", "_commit(");

        let mut offenders = Vec::new();
        for file in &files {
            let rel = file
                .strip_prefix(&src_root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/");
            // `tests.rs` is `#[cfg(test)]`: a test driving openmls directly is
            // not a production epoch advance.
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
