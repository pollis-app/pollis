//! Account-key transparency: client-side self-audit and peer audit.
//!
//! This is the *client* half of the account-key tenant (issue #330). The log,
//! its signed Merkle tree, and the per-user `/verify/account/<id>` reports are
//! produced server-side and audited by the `pollis-verify` CLI; here we let the
//! running app perform the same verification for its OWN key and for a peer it
//! has TOFU-pinned (see [`crate::commands::safety`]) — reusing the EXACT shared
//! verifier the auditor uses ([`verifiable_log_serve::account::verify_account`])
//! so a client can never reach a different verdict than a third-party auditor.
//! No proof, Merkle, or signature logic is reimplemented here.
//!
//! **Pinned trust root.** The log's Ed25519 public key is pinned as a constant
//! ([`PINNED_LOG_PUBLIC_KEY`]). The served `public_key.json` MUST equal it; a
//! mismatch is a hard [`AuditStatus::Alarm`], never a warning — without the pin
//! a hostile host could serve its own key over its own self-consistent (but
//! forged) tree and every signature would "verify".
//!
//! **What lives here.** Only *status derivation* — comparing the verified
//! published chain against this device's local view of the key. That decision
//! is factored into pure functions ([`derive_self_audit`], [`derive_peer_audit`])
//! so it is unit-testable with no HTTP and no DB. Everything network/proof-shaped
//! goes through the shared verifier.
//!
//! **Policy: advisory, never blocking.** Every status is informational. The app
//! keeps working regardless; an `Alarm` raises the alert, it does not stop a
//! send. `Unavailable` means "couldn't check" (host down), not "failed".

use std::sync::Arc;

use serde::Serialize;

use verifiable_log_serve::{AccountKeyVersion, AccountReport};

use crate::error::{Error, Result};
use crate::state::AppState;

/// The transparency log's pinned Ed25519 public key (lowercase hex, 32 bytes).
///
/// Cross-checked against the auditor release notes / `docs/transparency.md` —
/// this is the key the static `/v1/account-keys/public_key.json` MUST carry. A
/// served key that differs is treated as a hostile host (hard ALARM), since any
/// key can sign a self-consistent forged tree. Pinning it here is what makes the
/// signature check mean anything.
pub const PINNED_LOG_PUBLIC_KEY: &str =
    "175ebfef98fc6b20c67c4cba9d4a36a4f85f05afa4e31f707e7d7e3c02227148";

/// Default base URL of the published transparency log.
const DEFAULT_TRANSPARENCY_URL: &str = "https://verify.pollis.com";

/// Env var that overrides [`DEFAULT_TRANSPARENCY_URL`] (dev/staging hosts, a
/// local `serve serve`). Read with the same compile-time-embed-then-runtime
/// fallback shape as the optional fields in [`crate::config`].
const TRANSPARENCY_URL_ENV: &str = "POLLIS_TRANSPARENCY_URL";

/// The base URL of the account-key transparency log to verify against.
fn transparency_base_url() -> String {
    option_env!("POLLIS_TRANSPARENCY_URL")
        .map(|s| s.to_string())
        .or_else(|| std::env::var(TRANSPARENCY_URL_ENV).ok())
        .unwrap_or_else(|| DEFAULT_TRANSPARENCY_URL.to_string())
}

/// Verdict of an account-key audit. Serialized snake_case so the renderer can
/// switch on it directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditStatus {
    /// Chain verifies and the published latest entry matches the local key.
    Ok,
    /// The local key/version is simply not in the published tree yet — the log
    /// publishes daily, so a recent signup/rotation is invisible until the next
    /// publish. Advisory; absence alone is NOT an alarm.
    Pending,
    /// A real discrepancy: the published chain disagrees with the local key at
    /// the same-or-higher version (selective targeting), the served key does not
    /// match the pinned key, or chain/proof verification failed.
    Alarm,
    /// The log host was unreachable or returned malformed prerequisites.
    /// Advisory — "couldn't check", not "failed".
    Unavailable,
}

/// Result of [`self_audit_account_key`]: the verified published report plus the
/// derived verdict against this device's own current key.
#[derive(Debug, Clone, Serialize)]
pub struct SelfAuditReport {
    pub status: AuditStatus,
    /// One-line, human-readable explanation of `status` (shown verbatim in UI).
    pub detail: String,
    /// This user's current key/version as this device sees it (`users` row).
    pub my_identity_version: i64,
    /// This user's current `account_id_pub`, lowercase hex.
    pub my_account_id_pub: String,
    /// The verified key-history report from the log, or `None` when unavailable.
    pub report: Option<AccountReport>,
}

/// Result of [`audit_peer_account_key`]: the verified published report plus the
/// derived verdict against the locally TOFU-pinned key for the peer.
#[derive(Debug, Clone, Serialize)]
pub struct PeerAuditReport {
    pub status: AuditStatus,
    pub detail: String,
    pub peer_user_id: String,
    /// The TOFU-pinned version we compared against (`None` if no local pin).
    pub pinned_identity_version: Option<i64>,
    /// True when the pinned key is present in the history AND a newer version
    /// has since been published (the peer rotated their key — still `Ok`).
    pub key_rotated: bool,
    /// The verified key-history report from the log, or `None` when unavailable.
    pub report: Option<AccountReport>,
}

// ── Commands ─────────────────────────────────────────────────────────────────

/// Self-audit: verify OWN account-key history against the published log, then
/// compare the chain's latest published version to this device's current key.
///
/// `my_user_id` is the current user (mirrors how [`crate::commands::safety`]
/// commands take the acting user id from the caller).
pub async fn self_audit_account_key(
    state: &Arc<AppState>,
    my_user_id: String,
) -> Result<SelfAuditReport> {
    // Local view of my own current key, from the same `users` row the safety
    // module reads.
    let conn = state.remote_db.conn().await?;
    let (my_pub, my_version) =
        crate::commands::safety::fetch_account_key(&conn, &my_user_id).await?;
    let my_pub_hex = hex::encode(&my_pub);

    let base = transparency_base_url();
    match fetch_and_verify(&base, &my_user_id).await {
        Ok((report, served_key)) => {
            Ok(derive_self_audit(&report, &served_key, &my_pub_hex, my_version))
        }
        Err(detail) => Ok(SelfAuditReport {
            status: AuditStatus::Unavailable,
            detail,
            my_identity_version: my_version,
            my_account_id_pub: my_pub_hex,
            report: None,
        }),
    }
}

/// Peer audit: verify a peer's account-key history against the published log,
/// then compare it to the key this device TOFU-pinned for that peer.
pub async fn audit_peer_account_key(
    state: &Arc<AppState>,
    peer_user_id: String,
) -> Result<PeerAuditReport> {
    // The locally-pinned key for this peer, from the same `contact_verification`
    // store the safety module maintains. `None` if we've never pinned them.
    let pinned = load_pinned_key(state, &peer_user_id).await?;
    let pinned_hex = pinned.as_ref().map(|(p, _)| hex::encode(p));
    let pinned_version = pinned.as_ref().map(|(_, v)| *v);

    let base = transparency_base_url();
    match fetch_and_verify(&base, &peer_user_id).await {
        Ok((report, served_key)) => {
            let pin = match (&pinned_hex, pinned_version) {
                (Some(h), Some(v)) => Some((h.as_str(), v)),
                _ => None,
            };
            Ok(derive_peer_audit(&report, &served_key, &peer_user_id, pin))
        }
        Err(detail) => Ok(PeerAuditReport {
            status: AuditStatus::Unavailable,
            detail,
            peer_user_id,
            pinned_identity_version: pinned_version,
            key_rotated: false,
            report: None,
        }),
    }
}

// ── Network (the only IO; everything below derive_* is pure) ──────────────────

/// Fetch the served public key and run the shared account verifier for
/// `user_id`. Returns `Err(detail)` on any transport/parse failure of the
/// prerequisites — the caller maps that to [`AuditStatus::Unavailable`].
async fn fetch_and_verify(
    base: &str,
    user_id: &str,
) -> std::result::Result<(AccountReport, String), String> {
    // The served public key — the input to the pinned-key cross-check. Fetched
    // separately because the shared verifier folds the key into its verdict but
    // does not surface it; a served key that differs from the pin is caught in
    // `derive_*` regardless of how the (forged) tree verifies under it.
    let served_key = fetch_served_public_key(base)
        .await
        .map_err(|e| format!("could not fetch the log's public key: {e}"))?;

    // The shared verifier — the SAME function `pollis-verify account` runs. Its
    // HTTP is blocking (ureq), so it runs on the blocking pool, matching how the
    // rest of pollis-core offloads blocking work (`spawn_blocking`).
    let base_owned = base.to_string();
    let user_owned = user_id.to_string();
    let report = tokio::task::spawn_blocking(move || {
        verifiable_log_serve::account::verify_account(&base_owned, &user_owned)
    })
    .await
    .map_err(|e| format!("account verifier task failed: {e}"))?
    .map_err(|e| format!("verifying the account-key log failed: {e}"))?;

    Ok((report, served_key))
}

/// Fetch `/v1/account-keys/public_key.json` and return its key (lowercase hex).
async fn fetch_served_public_key(base: &str) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct PublicKeyDoc {
        public_key: String,
    }
    let url = format!(
        "{}/v1/account-keys/public_key.json",
        base.trim_end_matches('/')
    );
    let client = reqwest::Client::new();
    let doc: PublicKeyDoc = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(doc.public_key.to_lowercase())
}

/// Read the TOFU-pinned `(account_id_pub, identity_version)` for a peer from the
/// local `contact_verification` table (same store the safety module owns).
async fn load_pinned_key(
    state: &Arc<AppState>,
    peer_user_id: &str,
) -> Result<Option<(Vec<u8>, i64)>> {
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    let pin = db
        .conn()
        .query_row(
            "SELECT account_id_pub, identity_version FROM contact_verification \
             WHERE peer_user_id = ?1",
            rusqlite::params![peer_user_id],
            |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?)),
        )
        .ok();
    Ok(pin)
}

// ── Pure status derivation (unit-tested without HTTP/DB) ──────────────────────

/// Derive the self-audit verdict from a verified report, the served public key,
/// and this device's own current key/version. Pure — no IO.
pub fn derive_self_audit(
    report: &AccountReport,
    served_public_key: &str,
    my_account_id_pub: &str,
    my_identity_version: i64,
) -> SelfAuditReport {
    let mk = |status, detail: String| SelfAuditReport {
        status,
        detail,
        my_identity_version,
        my_account_id_pub: my_account_id_pub.to_string(),
        report: Some(report.clone()),
    };

    // Pin first: an unpinned key invalidates every signature check below, so a
    // mismatch is a hard alarm regardless of how the served tree verifies.
    if !served_key_matches_pin(served_public_key) {
        return mk(AuditStatus::Alarm, pin_mismatch_detail(served_public_key));
    }

    // A head/proof we can't verify is worth nothing — alarm.
    if !report.chain_valid {
        return mk(
            AuditStatus::Alarm,
            first_violation_detail(report, "the published account-key chain failed verification"),
        );
    }

    // Chain verifies. Compare the chain's LATEST published version to ours.
    let Some(latest) = latest_version(report) else {
        return mk(
            AuditStatus::Pending,
            "your key history is not in the published log yet — the log publishes daily, so a \
             recent signup is invisible until the next publish"
                .to_string(),
        );
    };

    match (latest.identity_version as i64).cmp(&my_identity_version) {
        // Newest published version is older than ours: we just signed up or
        // rotated, and the daily publish hasn't run yet. Advisory.
        std::cmp::Ordering::Less => mk(
            AuditStatus::Pending,
            format!(
                "your current key version {my_identity_version} is not in the published log yet \
                 (newest published is version {}) — the log publishes daily",
                latest.identity_version
            ),
        ),
        // Same version: it must be the same key, or the server published a
        // different key for us at our version — selective targeting.
        std::cmp::Ordering::Equal => {
            if latest.account_id_pub.eq_ignore_ascii_case(my_account_id_pub) {
                mk(
                    AuditStatus::Ok,
                    format!(
                        "your account key is correctly published at version {my_identity_version}"
                    ),
                )
            } else {
                mk(
                    AuditStatus::Alarm,
                    format!(
                        "the log publishes a DIFFERENT key for you at version \
                         {my_identity_version} than this device holds — possible selective \
                         targeting"
                    ),
                )
            }
        }
        // A higher published version than this device knows about: the log shows
        // a key rotation this device never performed. The selective-targeting /
        // unauthorized-rotation catch.
        std::cmp::Ordering::Greater => mk(
            AuditStatus::Alarm,
            format!(
                "the log publishes a newer key version ({}) than this device holds \
                 ({my_identity_version}) — possible unauthorized key rotation",
                latest.identity_version
            ),
        ),
    }
}

/// Derive the peer-audit verdict from a verified report, the served public key,
/// the peer id, and the locally pinned `(account_id_pub_hex, version)` (if any).
/// Pure — no IO.
pub fn derive_peer_audit(
    report: &AccountReport,
    served_public_key: &str,
    peer_user_id: &str,
    pinned: Option<(&str, i64)>,
) -> PeerAuditReport {
    let mk = |status, detail: String, key_rotated| PeerAuditReport {
        status,
        detail,
        peer_user_id: peer_user_id.to_string(),
        pinned_identity_version: pinned.map(|(_, v)| v),
        key_rotated,
        report: Some(report.clone()),
    };

    if !served_key_matches_pin(served_public_key) {
        return mk(AuditStatus::Alarm, pin_mismatch_detail(served_public_key), false);
    }
    if !report.chain_valid {
        return mk(
            AuditStatus::Alarm,
            first_violation_detail(report, "the published account-key chain failed verification"),
            false,
        );
    }

    // No TOFU pin yet — there is nothing to audit the published history against.
    let Some((pinned_pub, pinned_version)) = pinned else {
        return mk(
            AuditStatus::Pending,
            "you have not pinned this peer's key yet, so there is nothing to audit against"
                .to_string(),
            false,
        );
    };

    // No published history for this peer (never signed up, or not yet published).
    if !report.found || report.keys.is_empty() {
        return mk(
            AuditStatus::Pending,
            "this peer has no published key history yet — the log publishes daily".to_string(),
            false,
        );
    }

    // The pinned key MUST appear in the verifying published history. Absent → the
    // server showed us a key it never published: alarm.
    let pinned_present = report
        .keys
        .iter()
        .any(|k| k.account_id_pub.eq_ignore_ascii_case(pinned_pub));
    if !pinned_present {
        return mk(
            AuditStatus::Alarm,
            "the key you pinned for this peer is ABSENT from the published log — the server \
             showed you a key it never published"
                .to_string(),
            false,
        );
    }

    // Present → the key history is accountable. Note a rotation to a newer
    // version (still Ok — the pinned key is in the history).
    let latest = latest_version(report);
    let key_rotated = latest.is_some_and(|l| (l.identity_version as i64) > pinned_version);
    let detail = if key_rotated {
        format!(
            "the key you pinned is in the published history; the peer has since rotated to a \
             newer version ({})",
            latest.map(|l| l.identity_version).unwrap_or_default()
        )
    } else {
        "the key you pinned for this peer is correctly published".to_string()
    };
    mk(AuditStatus::Ok, detail, key_rotated)
}

/// True iff the served key equals the pinned key (case-insensitive hex).
fn served_key_matches_pin(served: &str) -> bool {
    served.eq_ignore_ascii_case(PINNED_LOG_PUBLIC_KEY)
}

/// The chain's latest published key version (highest `identity_version`). `keys`
/// is already in `seq` order; `max_by_key` is defensive against source ordering.
fn latest_version(report: &AccountReport) -> Option<&AccountKeyVersion> {
    report.keys.iter().max_by_key(|k| k.identity_version)
}

/// The first verifier-reported violation, or a fallback if (somehow) empty.
fn first_violation_detail(report: &AccountReport, fallback: &str) -> String {
    report
        .violations
        .first()
        .cloned()
        .unwrap_or_else(|| fallback.to_string())
}

fn pin_mismatch_detail(served: &str) -> String {
    format!(
        "the log served public key {} but this build pins {} — refusing to trust the served tree",
        short_hex(served),
        short_hex(PINNED_LOG_PUBLIC_KEY)
    )
}

/// Abbreviate a long hex string for human-readable detail messages.
fn short_hex(s: &str) -> String {
    if s.len() <= 12 {
        s.to_string()
    } else {
        format!("{}\u{2026}{}", &s[..6], &s[s.len() - 4..])
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────
//
// Pure status-derivation tests only — no HTTP, no DB. The async command
// functions need an `AppState` + libsql + local DB and are exercised by the
// flows harness; these lock in the decision logic.

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: &str = "aa00000000000000000000000000000000000000000000000000000000000000";
    const KEY_B: &str = "bb00000000000000000000000000000000000000000000000000000000000000";
    const KEY_Z: &str = "zz_not_in_history_000000000000000000000000000000000000000000000";

    fn version(v: u64, seq: i64, key: &str) -> AccountKeyVersion {
        AccountKeyVersion {
            identity_version: v,
            seq,
            account_id_pub: key.to_string(),
            included: true,
        }
    }

    fn report(keys: Vec<AccountKeyVersion>, chain_valid: bool) -> AccountReport {
        AccountReport {
            user_id: "u1".to_string(),
            found: !keys.is_empty(),
            sth_tree_size: 10,
            root_hex: "deadbeef".to_string(),
            keys,
            chain_valid,
            violations: if chain_valid {
                Vec::new()
            } else {
                vec!["account STH signature is invalid".to_string()]
            },
        }
    }

    // ── self-audit ────────────────────────────────────────────────────────

    #[test]
    fn self_ok_when_latest_matches_my_key() {
        let r = report(vec![version(1, 0, KEY_A)], true);
        let out = derive_self_audit(&r, PINNED_LOG_PUBLIC_KEY, KEY_A, 1);
        assert_eq!(out.status, AuditStatus::Ok);
    }

    #[test]
    fn self_ok_is_case_insensitive_on_key() {
        let r = report(vec![version(1, 0, KEY_A)], true);
        let out = derive_self_audit(&r, PINNED_LOG_PUBLIC_KEY, &KEY_A.to_uppercase(), 1);
        assert_eq!(out.status, AuditStatus::Ok);
    }

    #[test]
    fn self_pending_when_history_empty() {
        let r = report(vec![], true);
        let out = derive_self_audit(&r, PINNED_LOG_PUBLIC_KEY, KEY_A, 1);
        assert_eq!(out.status, AuditStatus::Pending);
    }

    #[test]
    fn self_pending_when_my_version_newer_than_published() {
        // Published latest is v1; this device already rotated to v2.
        let r = report(vec![version(1, 0, KEY_A)], true);
        let out = derive_self_audit(&r, PINNED_LOG_PUBLIC_KEY, KEY_B, 2);
        assert_eq!(out.status, AuditStatus::Pending);
    }

    #[test]
    fn self_alarm_when_same_version_different_key() {
        // Selective targeting: log shows a different key at my current version.
        let r = report(vec![version(1, 0, KEY_A)], true);
        let out = derive_self_audit(&r, PINNED_LOG_PUBLIC_KEY, KEY_B, 1);
        assert_eq!(out.status, AuditStatus::Alarm);
    }

    #[test]
    fn self_alarm_when_published_version_higher() {
        // The log shows a rotation this device never performed.
        let r = report(vec![version(1, 0, KEY_A), version(2, 1, KEY_B)], true);
        let out = derive_self_audit(&r, PINNED_LOG_PUBLIC_KEY, KEY_A, 1);
        assert_eq!(out.status, AuditStatus::Alarm);
    }

    #[test]
    fn self_alarm_on_pin_mismatch_even_if_chain_valid() {
        let r = report(vec![version(1, 0, KEY_A)], true);
        let out = derive_self_audit(&r, KEY_B, KEY_A, 1);
        assert_eq!(out.status, AuditStatus::Alarm);
    }

    #[test]
    fn self_alarm_on_invalid_chain() {
        let r = report(vec![version(1, 0, KEY_A)], false);
        let out = derive_self_audit(&r, PINNED_LOG_PUBLIC_KEY, KEY_A, 1);
        assert_eq!(out.status, AuditStatus::Alarm);
    }

    // ── peer-audit ────────────────────────────────────────────────────────

    #[test]
    fn peer_ok_when_pinned_key_present() {
        let r = report(vec![version(1, 0, KEY_A)], true);
        let out = derive_peer_audit(&r, PINNED_LOG_PUBLIC_KEY, "peer", Some((KEY_A, 1)));
        assert_eq!(out.status, AuditStatus::Ok);
        assert!(!out.key_rotated);
    }

    #[test]
    fn peer_ok_notes_rotation_when_newer_version_exists() {
        let r = report(vec![version(1, 0, KEY_A), version(2, 1, KEY_B)], true);
        let out = derive_peer_audit(&r, PINNED_LOG_PUBLIC_KEY, "peer", Some((KEY_A, 1)));
        assert_eq!(out.status, AuditStatus::Ok);
        assert!(out.key_rotated);
    }

    #[test]
    fn peer_alarm_when_pinned_key_absent_from_history() {
        // The server showed us KEY_Z but the published history only has KEY_A.
        let r = report(vec![version(1, 0, KEY_A)], true);
        let out = derive_peer_audit(&r, PINNED_LOG_PUBLIC_KEY, "peer", Some((KEY_Z, 1)));
        assert_eq!(out.status, AuditStatus::Alarm);
    }

    #[test]
    fn peer_pending_when_no_local_pin() {
        let r = report(vec![version(1, 0, KEY_A)], true);
        let out = derive_peer_audit(&r, PINNED_LOG_PUBLIC_KEY, "peer", None);
        assert_eq!(out.status, AuditStatus::Pending);
    }

    #[test]
    fn peer_pending_when_peer_never_published() {
        let r = report(vec![], true);
        let out = derive_peer_audit(&r, PINNED_LOG_PUBLIC_KEY, "peer", Some((KEY_A, 1)));
        assert_eq!(out.status, AuditStatus::Pending);
    }

    #[test]
    fn peer_alarm_on_pin_mismatch() {
        let r = report(vec![version(1, 0, KEY_A)], true);
        let out = derive_peer_audit(&r, KEY_B, "peer", Some((KEY_A, 1)));
        assert_eq!(out.status, AuditStatus::Alarm);
    }

    #[test]
    fn peer_alarm_on_invalid_chain() {
        let r = report(vec![version(1, 0, KEY_A)], false);
        let out = derive_peer_audit(&r, PINNED_LOG_PUBLIC_KEY, "peer", Some((KEY_A, 1)));
        assert_eq!(out.status, AuditStatus::Alarm);
    }
}
