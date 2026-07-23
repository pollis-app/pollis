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
use sha2::{Digest, Sha256};

use verifiable_log_serve::release::{verify_release, Layer, ReleaseReport};
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

/// Verdict of an in-app "verify this build" check. Serialized snake_case so the
/// renderer can switch on it directly, mirroring [`AuditStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildVerifyStatus {
    /// This build's payload fingerprint is published in the binaries transparency
    /// log for its release tag — the honest, publicly-attested build.
    Verified,
    /// The running release tag is not in the published binaries tree yet — the
    /// tree republishes after each release, so a very fresh build is invisible
    /// until then. Advisory; absence of the tag alone is NOT an alarm.
    Pending,
    /// The tag IS in the log but this build's payload fingerprint is absent, or
    /// the served tree failed verification / the served key does not match the
    /// pin. The loud, targeted-backdoor signal: the running binary is not one
    /// Pollis publicly attested.
    Mismatch,
    /// The log host was unreachable or the local payload could not be hashed.
    /// Advisory — "couldn't check", not "failed".
    Unavailable,
}

/// Result of [`verify_own_build`]: the running build's identity, the payload hash
/// this device computed, the verified release report, and the derived verdict.
#[derive(Debug, Clone, Serialize)]
pub struct BuildVerifyReport {
    pub status: BuildVerifyStatus,
    /// One-line, human-readable explanation of `status` (shown verbatim in UI).
    pub detail: String,
    /// This build's package version (`CARGO_PKG_VERSION`), e.g. `1.1.0`.
    pub version: String,
    /// The exact source commit, baked at build time; `None` if it wasn't baked
    /// (source checkout without git). Never fabricated.
    pub commit: Option<String>,
    /// The reproducible payload hash this device computed for its own binary,
    /// lowercase hex — the value compared against the log's payload leaves.
    pub my_payload_sha256: String,
    /// The verified release report from the binaries log, or `None` when
    /// unavailable / the served key was not the pinned one.
    pub report: Option<ReleaseReport>,
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
    let overlay = state.overlay_handle();
    match fetch_and_verify(overlay.as_deref(), &base, &my_user_id).await {
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
    let overlay = state.overlay_handle();
    match fetch_and_verify(overlay.as_deref(), &base, &peer_user_id).await {
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

/// Verify THIS build against the published binaries transparency log: compute
/// the running binary's reproducible payload hash, fetch + verify the release
/// tree for its tag (trusting only the pinned key), and report whether the hash
/// is publicly attested.
///
/// Reuses the EXACT `verifiable_log_serve::release::verify_release` the
/// `pollis-verify release` CLI runs — no verifier is reimplemented — run on the
/// blocking pool like [`self_audit_account_key`]. Advisory only: it never gates
/// launch or update, matching the account-key self-audit policy.
pub async fn verify_own_build(state: &Arc<AppState>) -> Result<BuildVerifyReport> {
    let version = env!("CARGO_PKG_VERSION").to_string();
    // Baked by `build.rs`; `None` when this build had no git checkout.
    let commit = option_env!("POLLIS_GIT_COMMIT").map(str::to_string);
    // The attest job tags leaves with the git tag (`github.ref_name`, e.g.
    // `v1.1.0`); the package version drops the `v`, so re-add it to match.
    let release_tag = format!("v{version}");

    let mk_unavailable = |detail: String, my_hash: String| BuildVerifyReport {
        status: BuildVerifyStatus::Unavailable,
        detail,
        version: version.clone(),
        commit: commit.clone(),
        my_payload_sha256: my_hash,
        report: None,
    };

    // Hash our own binary (blocking file IO) on the blocking pool.
    let my_payload = match tokio::task::spawn_blocking(compute_my_payload).await {
        Ok(Ok(payload)) => payload,
        Ok(Err(detail)) => return Ok(mk_unavailable(detail, String::new())),
        Err(e) => {
            return Ok(mk_unavailable(
                format!("hashing this build failed: {e}"),
                String::new(),
            ))
        }
    };
    let my_hash = my_payload.sha256().to_string();

    let base = transparency_base_url();

    // Pin the served binaries-log key FIRST. `verify_release` folds whatever key
    // the host serves into its verdict, so an unpinned key could sign a
    // self-consistent forged tree that "verifies" — the same trap the account
    // self-audit guards against. A served key ≠ the pin is the loud case.
    let overlay = state.overlay_handle();
    match fetch_served_binaries_public_key(overlay.as_deref(), &base).await {
        Ok(served) if !served_key_matches_pin(&served) => {
            return Ok(BuildVerifyReport {
                status: BuildVerifyStatus::Mismatch,
                detail: pin_mismatch_detail(&served),
                version,
                commit,
                my_payload_sha256: my_hash,
                report: None,
            });
        }
        Ok(_) => {}
        Err(detail) => return Ok(mk_unavailable(detail, my_hash)),
    }

    // The shared verifier — the SAME function `pollis-verify release` runs. Its
    // HTTP is blocking (ureq), so it runs on the blocking pool.
    let base_owned = base.clone();
    let tag_owned = release_tag.clone();
    match tokio::task::spawn_blocking(move || verify_release(&base_owned, &tag_owned)).await {
        Ok(Ok(report)) => Ok(derive_build_verify(
            &report,
            &my_payload,
            &version,
            commit.as_deref(),
        )),
        Ok(Err(e)) => Ok(mk_unavailable(
            format!("verifying the binaries log failed: {e}"),
            my_hash,
        )),
        Err(e) => Ok(mk_unavailable(
            format!("release verifier task failed: {e}"),
            my_hash,
        )),
    }
}

// ── Network (the only IO; everything below derive_* is pure) ──────────────────

/// Fetch the served public key and run the shared account verifier for
/// `user_id`. Returns `Err(detail)` on any transport/parse failure of the
/// prerequisites — the caller maps that to [`AuditStatus::Unavailable`].
async fn fetch_and_verify(
    overlay: Option<&pollis_relay::OverlayHandle>,
    base: &str,
    user_id: &str,
) -> std::result::Result<(AccountReport, String), String> {
    // The served public key — the input to the pinned-key cross-check. Fetched
    // separately because the shared verifier folds the key into its verdict but
    // does not surface it; a served key that differs from the pin is caught in
    // `derive_*` regardless of how the (forged) tree verifies under it.
    let served_key = fetch_served_public_key(overlay, base)
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
/// Routes through the overlay when on — this is the async reqwest fetch path
/// (§14.2); the `ureq` verifier below has no proxy seam and stays a documented
/// residual (§14.4).
async fn fetch_served_public_key(
    overlay: Option<&pollis_relay::OverlayHandle>,
    base: &str,
) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct PublicKeyDoc {
        public_key: String,
    }
    let url = format!(
        "{}/v1/account-keys/public_key.json",
        base.trim_end_matches('/')
    );
    let client = crate::net::overlay::http_client(overlay);
    let doc: PublicKeyDoc = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(doc.public_key.to_lowercase())
}

/// Fetch `/v1/binaries/public_key.json` and return its key (lowercase hex). The
/// binaries tree's key is the same pinned Ed25519 key as the account tree, but
/// served under the binaries subtree; the caller pin-checks it before trusting
/// any release verdict. Returns `Err(detail)` on transport/parse failure.
async fn fetch_served_binaries_public_key(
    overlay: Option<&pollis_relay::OverlayHandle>,
    base: &str,
) -> std::result::Result<String, String> {
    #[derive(serde::Deserialize)]
    struct PublicKeyDoc {
        public_key: String,
    }
    let url = format!("{}/v1/binaries/public_key.json", base.trim_end_matches('/'));
    let client = crate::net::overlay::http_client(overlay);
    let doc: PublicKeyDoc = client
        .get(&url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("could not fetch the binaries log's public key: {e}"))?
        .json()
        .await
        .map_err(|e| format!("could not parse the binaries log's public key: {e}"))?;
    Ok(doc.public_key.to_lowercase())
}

/// This build's own hash, paired with the leaf layer it is comparable against.
///
/// A hash means nothing without knowing which published leaf it should equal,
/// and the two shapes are NOT interchangeable: the `payload` leaf is a
/// `sha_tree` of an extracted directory (or the installer file), which an
/// installed process has no preimage for, while the `exe` leaf is the main
/// executable's own sha256, which is exactly what a running process can take.
/// Pairing hash with target layer in one type is what stops the wrong
/// comparison — which misses every time, and renders as "this binary is not one
/// Pollis publicly attested" on a perfectly genuine build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MyPayload {
    /// Compare against `payload` leaves: the shipped bytes ARE the logged
    /// payload (Linux AppImage — `$APPIMAGE` is the very file that was hashed).
    /// The stronger check of the two: it covers the whole payload, not just the
    /// main binary.
    Payload(String),
    /// Compare against `exe` leaves: the sha256 of the executable this process
    /// is running, which `scripts/attest-binaries.sh` logs per bundle.
    Exe(String),
}

impl MyPayload {
    /// The hex digest, for display in the report either way.
    fn sha256(&self) -> &str {
        match self {
            MyPayload::Payload(h) | MyPayload::Exe(h) => h,
        }
    }

    /// The leaf layer this hash is meaningful against.
    fn layer(&self) -> Layer {
        match self {
            MyPayload::Payload(_) => Layer::Payload,
            MyPayload::Exe(_) => Layer::Exe,
        }
    }
}

/// Compute this build's hash with the SAME method `scripts/attest-binaries.sh`
/// logs, and say which layer it is to be compared against.
///
/// - **Linux AppImage:** the shipped bytes ARE the reproducible payload (the
///   script's `sha_file` of the `.AppImage`). The AppImage runtime exports
///   `APPIMAGE` pointing at the file it mounted, so we hash exactly what was
///   logged and compare against the `payload` leaf.
/// - **Everything else** (macOS `.app` in a `.dmg`, Windows NSIS, Linux
///   deb/rpm): the `payload` leaf is unreachable from inside an install, so the
///   script also logs an `exe` leaf — the sha256 of `Contents/MacOS/pollis`,
///   `pollis.exe`, `usr/bin/pollis` as installed. `current_exe()` is that file.
fn compute_my_payload() -> std::result::Result<MyPayload, String> {
    #[cfg(target_os = "linux")]
    {
        if let Some(appimage) = std::env::var_os("APPIMAGE") {
            return Ok(MyPayload::Payload(sha256_file(std::path::Path::new(
                &appimage,
            ))?));
        }
    }
    let exe = std::env::current_exe()
        .map_err(|e| format!("could not locate the running executable: {e}"))?;
    Ok(MyPayload::Exe(sha256_file(&exe)?))
}

/// Stream a file through SHA-256, returning the lowercase-hex digest.
fn sha256_file(path: &std::path::Path) -> std::result::Result<String, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("could not open {} for hashing: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|e| format!("could not read {} for hashing: {e}", path.display()))?;
    Ok(hex::encode(hasher.finalize()))
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

/// Derive the build-verify verdict from a verified release report, this build's
/// computed payload hash, and its version/commit. Pure — no IO. The pin check on
/// the served key happens at the network boundary in [`verify_own_build`]; here
/// the report is already anchored to the pinned key.
///
/// Verdict:
/// - `chain_valid == false` → **Mismatch** (the published tree failed
///   verification under the trusted key — loud, like the self-audit alarm).
/// - tag not found (no artifacts for this release) → **Pending** (the tree
///   republishes after each release; a fresh build is invisible until then).
/// - tag present but carrying no leaf of the layer this install can compare
///   against → **Unavailable** ("couldn't check"). Comparing across layers would
///   miss every time and libel a genuine release; [`MyPayload`] makes choosing
///   the wrong layer unrepresentable, and this guard covers pre-`exe` tags.
/// - our hash present among the tag's comparable leaves → **Verified**.
/// - tag present, comparable leaves exist, ours absent → **Mismatch** (the
///   targeted-backdoor signal: the running binary is not one Pollis attested).
pub fn derive_build_verify(
    report: &ReleaseReport,
    my_payload: &MyPayload,
    version: &str,
    commit: Option<&str>,
) -> BuildVerifyReport {
    let mk = |status, detail: String| BuildVerifyReport {
        status,
        detail,
        version: version.to_string(),
        commit: commit.map(str::to_string),
        my_payload_sha256: my_payload.sha256().to_string(),
        report: Some(report.clone()),
    };

    // A head/proof/invariant we can't verify is worth nothing — loud.
    if !report.chain_valid {
        return mk(
            BuildVerifyStatus::Mismatch,
            first_release_violation(report, "the published binaries tree failed verification"),
        );
    }

    // Tree verifies, but this release tag has no artifacts published yet.
    if !report.found {
        return mk(
            BuildVerifyStatus::Pending,
            format!(
                "release {} is not published in the binaries transparency log yet — the log \
                 republishes after each release",
                report.release_tag
            ),
        );
    }

    // Compare only against leaves of the layer our hash actually means something
    // against (see `MyPayload`). `exe` leaves carry the executable's hash in
    // `artifact_sha256`; `payload` leaves carry the payload hash in
    // `payload_sha256` — and every signed leaf repeats its payload's
    // `payload_sha256`, so matching on that field keeps the AppImage behaviour
    // unchanged.
    //
    // Deliberately NOT filtered by platform/arch: a sha256 collision across
    // platforms is not a threat we need to model, whereas the leaves'
    // platform/arch labels are only as good as the attest script's. Matching on
    // the digest alone keeps a labelling mistake from ever becoming a false
    // tamper alarm, which is a much worse failure than an unfiltered compare.
    let want = my_payload.layer();
    let comparable: Vec<&str> = report
        .artifacts
        .iter()
        .filter(|a| a.layer == want)
        .map(|a| match want {
            Layer::Exe => a.artifact_sha256.as_str(),
            _ => a.payload_sha256.as_str(),
        })
        .collect();

    // A tag published before this platform's comparable layer existed has
    // nothing we can check against. That is "couldn't check", NOT "your binary
    // is unattested" — releases up to v1.5.2 carry no `exe` leaves at all, and
    // every macOS/Windows install would otherwise read as a tamper alarm.
    if comparable.is_empty() {
        return mk(
            BuildVerifyStatus::Unavailable,
            format!(
                "{} is published and its tree verifies, but it carries no fingerprint this \
                 install can compare itself against (releases before in-app verification \
                 shipped logged only the installer payload, which an installed app has no \
                 preimage for). Verify this build independently instead.",
                report.release_tag
            ),
        );
    }

    let my_payload_sha256 = my_payload.sha256();
    let published = comparable
        .iter()
        .any(|h| h.eq_ignore_ascii_case(my_payload_sha256));
    if published {
        mk(
            BuildVerifyStatus::Verified,
            format!(
                "this build's payload fingerprint is published in the public binaries \
                 transparency log for {}",
                report.release_tag
            ),
        )
    } else {
        mk(
            BuildVerifyStatus::Mismatch,
            format!(
                "this build's payload fingerprint is NOT in the published binaries log for {} — \
                 the running binary is not one Pollis publicly attested",
                report.release_tag
            ),
        )
    }
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

/// The first verifier-reported release violation, or a fallback if (somehow)
/// empty. Mirrors [`first_violation_detail`] for the account report.
fn first_release_violation(report: &ReleaseReport, fallback: &str) -> String {
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

    // ── build-verify ────────────────────────────────────────────────────────

    const HASH_MINE: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const HASH_OTHER: &str = "2222222222222222222222222222222222222222222222222222222222222222";
    const HASH_SIGNED: &str = "3333333333333333333333333333333333333333333333333333333333333333";

    // Build a fixture ReleaseReport by deserializing JSON. `payloads` become one
    // `payload` leaf each; the whole tree's validity and whether the tag is
    // found are set explicitly. Mirrors a pre-`exe` release (≤ v1.5.2).
    fn release_report(
        tag: &str,
        found: bool,
        chain_valid: bool,
        payloads: &[&str],
    ) -> ReleaseReport {
        let artifacts: Vec<serde_json::Value> = payloads
            .iter()
            .map(|h| {
                serde_json::json!({
                    "platform": "linux",
                    "arch": "x86_64",
                    "bundle": "appimage",
                    "layer": "payload",
                    "artifact_name": "pollis-linux.AppImage",
                    "payload_sha256": h,
                    "artifact_sha256": h,
                    "provenance_uri": "cdn.pollis.com/x.intoto.jsonl",
                    "included": true,
                })
            })
            .collect();
        let value = serde_json::json!({
            "release_tag": tag,
            "found": found,
            "sth_tree_size": 10,
            "root_hex": "deadbeef",
            "artifacts": artifacts,
            "chain_valid": chain_valid,
            "violations": if chain_valid {
                Vec::<String>::new()
            } else {
                vec!["binaries STH signature is invalid".to_string()]
            },
        });
        serde_json::from_value(value).expect("fixture ReleaseReport deserializes")
    }

    // A modern macOS-shaped release: the `.app` payload leaf, its `signed` dmg
    // leaf repeating that payload hash, and the `exe` leaf whose
    // `artifact_sha256` is the Mach-O a running app hashes.
    fn release_report_with_exe(
        tag: &str,
        chain_valid: bool,
        payload: &str,
        exes: &[&str],
    ) -> ReleaseReport {
        let mut artifacts = vec![
            serde_json::json!({
                "platform": "darwin", "arch": "aarch64", "bundle": "dmg",
                "layer": "payload", "artifact_name": "pollis-macos.dmg",
                "payload_sha256": payload, "artifact_sha256": payload,
                "provenance_uri": "cdn.pollis.com/x.intoto.jsonl", "included": true,
            }),
            serde_json::json!({
                "platform": "darwin", "arch": "aarch64", "bundle": "dmg",
                "layer": "signed", "artifact_name": "pollis-macos.dmg",
                "payload_sha256": payload, "artifact_sha256": HASH_SIGNED,
                "provenance_uri": "cdn.pollis.com/x.intoto.jsonl", "included": true,
            }),
        ];
        for e in exes {
            artifacts.push(serde_json::json!({
                "platform": "darwin", "arch": "aarch64", "bundle": "dmg",
                "layer": "exe", "artifact_name": "pollis-macos.dmg",
                "payload_sha256": payload, "artifact_sha256": e,
                "provenance_uri": "cdn.pollis.com/x.intoto.jsonl", "included": true,
            }));
        }
        let value = serde_json::json!({
            "release_tag": tag,
            "found": true,
            "sth_tree_size": 10,
            "root_hex": "deadbeef",
            "artifacts": artifacts,
            "chain_valid": chain_valid,
            "violations": if chain_valid {
                Vec::<String>::new()
            } else {
                vec!["binaries STH signature is invalid".to_string()]
            },
        });
        serde_json::from_value(value).expect("fixture ReleaseReport deserializes")
    }

    fn mine() -> MyPayload {
        MyPayload::Payload(HASH_MINE.to_string())
    }

    fn my_exe() -> MyPayload {
        MyPayload::Exe(HASH_MINE.to_string())
    }

    #[test]
    fn build_verified_when_my_hash_is_published() {
        let r = release_report("v1.1.0", true, true, &[HASH_OTHER, HASH_MINE]);
        let out = derive_build_verify(&r, &mine(), "1.1.0", Some("abc1234"));
        assert_eq!(out.status, BuildVerifyStatus::Verified);
        assert_eq!(out.commit.as_deref(), Some("abc1234"));
    }

    #[test]
    fn build_verified_is_case_insensitive_on_hash() {
        let r = release_report("v1.1.0", true, true, &[HASH_MINE]);
        let out = derive_build_verify(
            &r,
            &MyPayload::Payload(HASH_MINE.to_uppercase()),
            "1.1.0",
            None,
        );
        assert_eq!(out.status, BuildVerifyStatus::Verified);
        assert_eq!(out.commit, None);
    }

    #[test]
    fn build_mismatch_when_tag_present_but_hash_absent() {
        // The deliberately-wrong-local-hash case (design §6 Phase 4 acceptance):
        // the tag is in the log, but our payload hash is not among its leaves.
        let r = release_report("v1.1.0", true, true, &[HASH_OTHER]);
        let out = derive_build_verify(&r, &mine(), "1.1.0", Some("abc1234"));
        assert_eq!(out.status, BuildVerifyStatus::Mismatch);
    }

    #[test]
    fn build_pending_when_tag_not_in_log_yet() {
        let r = release_report("v1.1.0", false, true, &[]);
        let out = derive_build_verify(&r, &mine(), "1.1.0", None);
        assert_eq!(out.status, BuildVerifyStatus::Pending);
    }

    #[test]
    fn build_mismatch_when_tree_fails_verification() {
        // A tampered/forked tree (chain_valid == false) is loud, not pending —
        // even if our hash happens to appear among the untrusted leaves.
        let r = release_report("v1.1.0", true, false, &[HASH_MINE]);
        let out = derive_build_verify(&r, &mine(), "1.1.0", None);
        assert_eq!(out.status, BuildVerifyStatus::Mismatch);
    }

    #[test]
    fn exe_build_verified_against_exe_leaf() {
        // The macOS/Windows path: the running executable's hash matches the
        // tag's `exe` leaf. This is the case that was impossible before the
        // layer existed — every genuine build read as "not publicly attested".
        let r = release_report_with_exe("v1.1.0", true, HASH_OTHER, &[HASH_MINE]);
        let out = derive_build_verify(&r, &my_exe(), "1.1.0", Some("abc1234"));
        assert_eq!(out.status, BuildVerifyStatus::Verified);
        assert_eq!(out.my_payload_sha256, HASH_MINE);
    }

    #[test]
    fn exe_build_mismatch_when_exe_leaf_present_but_ours_absent() {
        // With a comparable leaf published, a miss IS the real signal again.
        let r = release_report_with_exe("v1.1.0", true, HASH_OTHER, &[HASH_SIGNED]);
        let out = derive_build_verify(&r, &my_exe(), "1.1.0", None);
        assert_eq!(out.status, BuildVerifyStatus::Mismatch);
    }

    #[test]
    fn layers_are_never_compared_across() {
        // THE invariant. Our exe hash equals the tag's *payload* leaf here — a
        // coincidence that says nothing, since the two hash different objects.
        // It must not read as Verified.
        let r = release_report_with_exe("v1.1.0", true, HASH_MINE, &[HASH_OTHER]);
        assert_eq!(
            derive_build_verify(&r, &my_exe(), "1.1.0", None).status,
            BuildVerifyStatus::Mismatch
        );

        // And symmetrically: an AppImage's payload hash must not be satisfied
        // by an `exe` leaf that happens to carry the same digest.
        let r = release_report_with_exe("v1.1.0", true, HASH_OTHER, &[HASH_MINE]);
        assert_eq!(
            derive_build_verify(&r, &mine(), "1.1.0", None).status,
            BuildVerifyStatus::Mismatch
        );
    }

    #[test]
    fn pre_exe_release_is_unavailable_not_mismatch() {
        // Releases up to v1.5.2 logged no `exe` leaves. A macOS install checking
        // itself against one has nothing comparable — "couldn't check", never
        // "your binary is unattested". This is the exact false alarm that
        // shipped: a genuine, signed, notarized v1.5.2 accused of being a
        // binary Pollis never published.
        let old = release_report("v1.5.2", true, true, &[HASH_OTHER]);
        let out = derive_build_verify(&old, &my_exe(), "1.5.2", Some("c3f8cf5"));
        assert_eq!(out.status, BuildVerifyStatus::Unavailable);
        assert_eq!(out.my_payload_sha256, HASH_MINE);

        // Not even if the exe hash coincidentally equals the payload leaf.
        let old = release_report("v1.5.2", true, true, &[HASH_MINE]);
        let out = derive_build_verify(&old, &my_exe(), "1.5.2", None);
        assert_eq!(out.status, BuildVerifyStatus::Unavailable);
    }

    #[test]
    fn missing_comparable_layer_still_reports_real_alarms() {
        // The Unavailable gate is scoped to the leaf comparison: a tree that
        // fails verification stays loud, and an unpublished tag stays pending,
        // on every platform and every release vintage.
        let broken = release_report("v1.5.2", true, false, &[HASH_MINE]);
        let out = derive_build_verify(&broken, &my_exe(), "1.5.2", None);
        assert_eq!(out.status, BuildVerifyStatus::Mismatch);

        let unpublished = release_report("v1.5.2", false, true, &[]);
        let out = derive_build_verify(&unpublished, &my_exe(), "1.5.2", None);
        assert_eq!(out.status, BuildVerifyStatus::Pending);
    }

    #[test]
    fn appimage_payload_path_is_unchanged_by_the_exe_layer() {
        // The AppImage keeps its stronger whole-payload match, and a tag that
        // also carries exe leaves does not disturb it.
        let r = release_report_with_exe("v1.1.0", true, HASH_MINE, &[HASH_OTHER]);
        assert_eq!(
            derive_build_verify(&r, &mine(), "1.1.0", None).status,
            BuildVerifyStatus::Verified
        );
    }
}
