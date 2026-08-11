//! Client-side **relay revocation** enforcement — issue #813 Phase B, consuming
//! Phase C's producer half.
//!
//! The pool publishes a short-lived, Ed25519-signed `revocations.json` beside the
//! signed directory (same key), and the directory carries a `revocation` anchor
//! (`{ seq, path, count }`) binding the two together. Without the client half
//! below, the pool would publish revocations that shipped clients ignore — the
//! feature would be decoration.
//!
//! **The verification and matching rules are not re-derived here.** They live in
//! `pollis_relay::policy` (`verify_revocations`, `RevocationStore`,
//! `RelayIdentity`, `Admission`), with `infra/relay-hydra/lib/revocation-verify.mjs`
//! as the byte-exact JS reference. This module is the *admission policy* around
//! them: when enforcement is required, and what "cannot evaluate" means.
//!
//! ## Fail closed
//!
//! [`Admission::Unevaluable`] is not an admission. Every one of these resolves
//! identically to "revoked", leaving an **empty** relay pool rather than the
//! unfiltered one (`Prefer` then dials direct, `Strict` surfaces a degrade —
//! both already handled downstream):
//!
//! - no list has ever been installed;
//! - the held list has expired **at use time**, not merely at fetch time;
//! - the held list's `seq` is below the sequence the signed directory anchors
//!   (a stale-but-unexpired list paired with a fresh directory — the pairing
//!   attack the anchor exists to detect);
//! - verification failed for any reason.
//!
//! ## The one carve-out: deploy order
//!
//! Publish-then-enforce. A client that *requires* the list must not reach users
//! before the reconciler that publishes it is applied, or every overlay-on client
//! fails closed on day one. So while the directory says there is nothing to
//! enforce — no `revocation` anchor at all, or an anchor whose `count` is `0` —
//! an unreachable list means "no revocations known" and the pool is used. The
//! moment the anchor advertises `count > 0` (or a count this build cannot read),
//! inability to evaluate is fatal.
//!
//! Note what this carve-out is **not**: it never downgrades a *positive*
//! revocation. A verified list that revokes a relay is honoured regardless of the
//! anchor, and a `seq` below the anchor is still a rejection.

use std::sync::Mutex;

use pollis_relay::{Admission, RelayIdentity, RevocationError, RevocationStore};

use super::directory::{Directory, DirectoryError};
use super::path::RelayEndpoint;

/// What the currently-held signed directory says about revocation.
#[derive(Debug, Clone, Copy)]
struct AnchorState {
    /// The sequence the directory anchors; a held list below this is stale.
    seq: u64,
    /// Whether inability to evaluate revocation must empty the pool.
    required: bool,
}

impl Default for AnchorState {
    fn default() -> Self {
        AnchorState {
            seq: 0,
            required: false,
        }
    }
}

/// The client's revocation admission gate for the relay pool.
///
/// One instance is shared across pool rebuilds (the store's high-water mark must
/// survive them, or a rollback would become possible at every directory refresh).
pub(crate) struct RelayAdmission {
    store: RevocationStore,
    anchor: Mutex<AnchorState>,
}

impl RelayAdmission {
    /// The enforcing gate: verifies against the same pinned key as the directory
    /// (`POLLIS_OVERLAY_DIRECTORY_KEY`) — Phase C deliberately did not mint a
    /// second key, because a second key is a second thing to rotate.
    pub(crate) fn enforcing(pinned_pubkey_b64: &str) -> RelayAdmission {
        RelayAdmission {
            store: RevocationStore::enforcing(pinned_pubkey_b64),
            anchor: Mutex::new(AnchorState::default()),
        }
    }

    /// The gate for a pool that has no signed directory at all (the static v0
    /// `POLLIS_OVERLAY_RELAY` config): there is no anchor to require anything, so
    /// nothing is enforced and nothing is filtered. This is not a bypass of the
    /// rule above — it is the absence of the artifact the rule is about.
    pub(crate) fn unenforced() -> RelayAdmission {
        RelayAdmission {
            store: RevocationStore::unconfigured(),
            anchor: Mutex::new(AnchorState::default()),
        }
    }

    /// Adopt what a freshly-verified directory says about revocation.
    ///
    /// `count == Some(0)` is the only value that keeps enforcement optional. An
    /// anchor whose `count` this build cannot read is treated as "there may be
    /// revocations" — ambiguity resolves closed, not open.
    pub(crate) fn observe_directory(&self, dir: &Directory) {
        let next = match dir.revocation.as_ref() {
            None => AnchorState::default(),
            Some(anchor) => AnchorState {
                seq: anchor.seq,
                required: anchor.count != Some(0),
            },
        };
        *self.anchor.lock().unwrap() = next;
    }

    /// Whether the pool must have usable revocation evidence right now.
    pub(crate) fn enforcement_required(&self) -> bool {
        self.anchor.lock().unwrap().required
    }

    /// Fetch and install the revocation list named by `dir`'s anchor, resolved
    /// against `directory_url`. A no-op when the directory carries no anchor.
    ///
    /// Errors are returned for logging only — the *consequence* of a failed
    /// refresh is expressed by [`RelayAdmission::admits`] refusing every relay,
    /// never by a caller remembering to react to a `Result`.
    pub(crate) async fn refresh(
        &self,
        directory_url: &str,
        dir: &Directory,
        now_secs: i64,
    ) -> Result<u64, RevocationRefreshError> {
        let Some(anchor) = dir.revocation.as_ref() else {
            return Ok(0);
        };
        let url = revocation_url(directory_url, anchor.resolved_path());
        let bytes = super::directory::fetch_signed(&url).await?;
        Ok(self.install(&bytes, now_secs)?)
    }

    /// Verify and adopt a revocation envelope, floored at the sequence the
    /// currently-held directory anchors (and at the store's own high-water mark,
    /// which `RevocationStore::install` applies on top). Rejection leaves the
    /// previously-held list in place — and if that list is itself unusable, every
    /// relay stays inadmissible.
    pub(crate) fn install(
        &self,
        envelope_bytes: &[u8],
        now_secs: i64,
    ) -> Result<u64, RevocationError> {
        let floor = self.anchor.lock().unwrap().seq;
        self.store.install(envelope_bytes, now_secs, floor)
    }

    /// May this relay be dialed right now?
    ///
    /// The single place the fail-closed rule is applied to the pool. Callers ask
    /// per candidate at *dial* time (not once at fetch time) so an expiring list
    /// closes the pool the moment it goes stale.
    pub(crate) fn admits(&self, endpoint: &RelayEndpoint, now_secs: i64) -> bool {
        let anchor = *self.anchor.lock().unwrap();
        let id = RelayIdentity {
            addr: &endpoint.addr,
            cert_der: endpoint.cert.as_ref(),
        };
        match self.store.admit(&id, now_secs) {
            // A positive revocation is honoured unconditionally — the deploy-order
            // carve-out below only ever excuses *missing* evidence, never
            // contradicts evidence we hold.
            Admission::Revoked => false,
            Admission::Admit => {
                // Freshness binding: a list that verified but sits below the
                // sequence the signed directory anchors is stale evidence, and
                // "stale evidence" is the same as "no evidence".
                if self.store.seq().is_some_and(|seq| seq >= anchor.seq) {
                    true
                } else {
                    !anchor.required
                }
            }
            Admission::Unevaluable => !anchor.required,
        }
    }
}

/// Why a revocation refresh failed. Both halves are fail-closed at the use site.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RevocationRefreshError {
    #[error("revocation list fetch failed: {0}")]
    Fetch(#[from] DirectoryError),
    #[error("revocation list rejected: {0}")]
    Rejected(#[from] RevocationError),
}

/// Resolve the anchor's `path` against the directory URL.
///
/// Three shapes, matching how the reconciler may name the artifact: an absolute
/// URL, a root-relative path, or (the normal case) a sibling filename published
/// next to the directory in the same CloudFront distribution.
pub(crate) fn revocation_url(directory_url: &str, path: &str) -> String {
    let path = path.trim();
    if path.starts_with("https://") || path.starts_with("http://") {
        return path.to_string();
    }
    let (scheme, rest) = match directory_url.split_once("://") {
        Some((s, r)) => (s, r),
        None => ("https", directory_url),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if let Some(abs) = path.strip_prefix('/') {
        return format!("{scheme}://{authority}/{abs}");
    }
    // Sibling of the directory object: drop everything after the last '/'.
    let dir_path = rest
        .strip_prefix(authority)
        .unwrap_or("")
        .split(['?', '#'])
        .next()
        .unwrap_or("");
    let base = match dir_path.rfind('/') {
        Some(i) => &dir_path[..=i],
        None => "/",
    };
    format!("{scheme}://{authority}{base}{path}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use ed25519_dalek::{Signer, SigningKey};
    use pollis_relay::{REVOCATION_TYPE, REVOCATION_VERSION};

    use crate::net::path::RelayEndpoint;
    use pollis_relay::CertificateDer;

    const RELAY_A: &str = "203.0.113.7:9444";
    const RELAY_B: &str = "203.0.113.8:9444";

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn pinned_b64(sk: &SigningKey) -> String {
        base64::engine::general_purpose::STANDARD.encode(sk.verifying_key().to_bytes())
    }

    fn endpoint(addr: &str) -> RelayEndpoint {
        RelayEndpoint::first_party(
            addr.to_string(),
            "us-west-2".to_string(),
            CertificateDer::from(vec![1u8; 48]),
        )
    }

    /// Sign an arbitrary payload with the pool key (the envelope shape is shared
    /// with the directory: `{payload_b64, signature_b64}`).
    fn envelope(sk: &SigningKey, payload: &str) -> Vec<u8> {
        let sig = sk.sign(payload.as_bytes());
        serde_json::to_vec(&serde_json::json!({
            "payload_b64": base64::engine::general_purpose::STANDARD.encode(payload.as_bytes()),
            "signature_b64": base64::engine::general_purpose::STANDARD.encode(sig.to_bytes()),
        }))
        .unwrap()
    }

    fn revocation_payload(seq: u64, expires_at: i64, revoked_addrs: &[&str]) -> String {
        let revoked: Vec<serde_json::Value> = revoked_addrs
            .iter()
            .map(|a| serde_json::json!({ "addr": a }))
            .collect();
        serde_json::to_string(&serde_json::json!({
            "version": REVOCATION_VERSION,
            "type": REVOCATION_TYPE,
            "seq": seq,
            "issued_at": 1_000,
            "expires_at": expires_at,
            "revoked": revoked,
        }))
        .unwrap()
    }

    fn directory_with_anchor(seq: u64, count: Option<u32>) -> Directory {
        let anchor = match count {
            Some(c) => format!(r#","revocation":{{"seq":{seq},"count":{c}}}"#),
            None => format!(r#","revocation":{{"seq":{seq}}}"#),
        };
        let json = format!(
            r#"{{"version":1,"issued_at":1,"expires_at":9999999999,"relays":[{{"addr":"{RELAY_A}","cert_b64":"QUJD"}}]{anchor}}}"#
        );
        serde_json::from_str(&json).expect("directory parses")
    }

    fn directory_without_anchor() -> Directory {
        serde_json::from_str(
            r#"{"version":1,"issued_at":1,"expires_at":9999999999,"relays":[{"addr":"x:1","cert_b64":"QUJD"}]}"#,
        )
        .unwrap()
    }

    // ── URL resolution ─────────────────────────────────────────────────────

    #[test]
    fn resolves_the_revocation_url_three_ways() {
        let dir = "https://relays.pollis.com/v1/directory.json";
        assert_eq!(
            revocation_url(dir, "revocations.json"),
            "https://relays.pollis.com/v1/revocations.json"
        );
        assert_eq!(
            revocation_url(dir, "/other/revocations.json"),
            "https://relays.pollis.com/other/revocations.json"
        );
        assert_eq!(
            revocation_url(dir, "https://elsewhere.example/r.json"),
            "https://elsewhere.example/r.json"
        );
        // Directory at the root still resolves a sibling.
        assert_eq!(
            revocation_url("https://relays.pollis.com/directory.json", "revocations.json"),
            "https://relays.pollis.com/revocations.json"
        );
    }

    // ── Fail-closed admission ──────────────────────────────────────────────

    /// A revoked relay is never admitted — the base case the feature exists for.
    #[test]
    fn a_revoked_relay_is_refused() {
        let sk = signing_key();
        let gate = RelayAdmission::enforcing(&pinned_b64(&sk));
        gate.observe_directory(&directory_with_anchor(3, Some(1)));
        let list = envelope(&sk, &revocation_payload(3, 5_000, &[RELAY_A]));
        gate.store.install(&list, 1_500, 3).expect("installs");

        assert!(!gate.admits(&endpoint(RELAY_A), 1_500), "revoked relay admitted");
        assert!(gate.admits(&endpoint(RELAY_B), 1_500), "unrevoked relay refused");
    }

    /// No list at all, while the anchor advertises revocations ⇒ nothing is
    /// admitted. An empty pool, never the unfiltered pool.
    #[test]
    fn no_list_with_revocations_advertised_admits_nothing() {
        let gate = RelayAdmission::enforcing(&pinned_b64(&signing_key()));
        gate.observe_directory(&directory_with_anchor(3, Some(2)));
        assert!(!gate.admits(&endpoint(RELAY_A), 1_500));
        assert!(!gate.admits(&endpoint(RELAY_B), 1_500));
    }

    /// Expiry is evaluated at USE time. A list that was fresh when installed
    /// stops being evidence the second it expires, and the pool closes — not on
    /// the next refresh tick, immediately.
    #[test]
    fn an_expired_list_fails_closed_at_use_time() {
        let sk = signing_key();
        let gate = RelayAdmission::enforcing(&pinned_b64(&sk));
        gate.observe_directory(&directory_with_anchor(3, Some(1)));
        let list = envelope(&sk, &revocation_payload(3, 5_000, &[RELAY_A]));
        gate.store.install(&list, 1_500, 3).expect("installs");

        assert!(gate.admits(&endpoint(RELAY_B), 4_999), "fresh: B is usable");
        assert!(!gate.admits(&endpoint(RELAY_B), 5_000), "expired: nothing is usable");
        assert!(!gate.admits(&endpoint(RELAY_A), 5_000));
    }

    /// A list below the directory's anchored `seq` is a stale-list/fresh-directory
    /// pairing. It verifies, it is unexpired, and it is still not evidence.
    #[test]
    fn a_list_behind_the_anchor_fails_closed() {
        let sk = signing_key();
        let gate = RelayAdmission::enforcing(&pinned_b64(&sk));
        // The directory anchors seq 9; the best list we can install is seq 4.
        gate.observe_directory(&directory_with_anchor(4, Some(1)));
        let list = envelope(&sk, &revocation_payload(4, 5_000, &[]));
        gate.store.install(&list, 1_500, 4).expect("installs at the old floor");
        assert!(gate.admits(&endpoint(RELAY_B), 1_500));

        // A newer directory raises the floor; the held list is now behind it.
        gate.observe_directory(&directory_with_anchor(9, Some(1)));
        assert!(
            !gate.admits(&endpoint(RELAY_B), 1_500),
            "a list behind the anchor must not admit anything"
        );
        // And installing it again is refused outright.
        assert!(gate.store.install(&list, 1_500, 9).is_err());
    }

    /// A verification failure leaves the store empty, which is fail-closed, not
    /// fail-open.
    #[test]
    fn a_forged_list_is_rejected_and_admits_nothing() {
        let sk = signing_key();
        let attacker = SigningKey::from_bytes(&[9u8; 32]);
        let gate = RelayAdmission::enforcing(&pinned_b64(&sk));
        gate.observe_directory(&directory_with_anchor(1, Some(1)));
        let forged = envelope(&attacker, &revocation_payload(1, 5_000, &[]));
        assert!(gate.store.install(&forged, 1_500, 1).is_err());
        assert!(!gate.admits(&endpoint(RELAY_A), 1_500));
    }

    // ── The deploy-order carve-out, and its limits ─────────────────────────

    /// A directory published before revocation existed carries no anchor: the
    /// pool stays usable. This is the case that would otherwise have taken every
    /// overlay-on client offline the day the client shipped ahead of the infra.
    #[test]
    fn a_directory_without_an_anchor_does_not_require_a_list() {
        let gate = RelayAdmission::enforcing(&pinned_b64(&signing_key()));
        gate.observe_directory(&directory_without_anchor());
        assert!(!gate.enforcement_required());
        assert!(gate.admits(&endpoint(RELAY_A), 1_500));
    }

    /// `count == 0` — revocation is live but nothing is revoked — also tolerates
    /// an unreachable list.
    #[test]
    fn an_empty_anchor_does_not_require_a_list() {
        let gate = RelayAdmission::enforcing(&pinned_b64(&signing_key()));
        gate.observe_directory(&directory_with_anchor(7, Some(0)));
        assert!(!gate.enforcement_required());
        assert!(gate.admits(&endpoint(RELAY_A), 1_500));
    }

    /// An anchor whose `count` this build cannot read resolves CLOSED. Ambiguity
    /// is not permission.
    #[test]
    fn an_anchor_without_a_count_requires_a_list() {
        let gate = RelayAdmission::enforcing(&pinned_b64(&signing_key()));
        gate.observe_directory(&directory_with_anchor(7, None));
        assert!(gate.enforcement_required());
        assert!(!gate.admits(&endpoint(RELAY_A), 1_500));
    }

    /// The carve-out excuses MISSING evidence only. With `count == 0` — the
    /// most permissive anchor there is — a list we actually hold that revokes a
    /// relay still refuses it.
    #[test]
    fn the_carve_out_never_overrides_a_positive_revocation() {
        let sk = signing_key();
        let gate = RelayAdmission::enforcing(&pinned_b64(&sk));
        gate.observe_directory(&directory_with_anchor(2, Some(0)));
        let list = envelope(&sk, &revocation_payload(2, 5_000, &[RELAY_A]));
        gate.store.install(&list, 1_500, 2).expect("installs");
        assert!(!gate.enforcement_required());
        assert!(!gate.admits(&endpoint(RELAY_A), 1_500), "held revocation ignored");
        assert!(gate.admits(&endpoint(RELAY_B), 1_500));
    }

    /// The static (no-directory) pool has no anchor and no key: unenforced, and
    /// explicitly so, rather than accidentally enforcing and wedging.
    #[test]
    fn the_static_pool_is_unenforced() {
        let gate = RelayAdmission::unenforced();
        assert!(!gate.enforcement_required());
        assert!(gate.admits(&endpoint(RELAY_A), 1_500));
    }
}
