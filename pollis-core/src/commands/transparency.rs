//! Account-key transparency, client side (#330): self-audit of this device's own
//! key, audit of a TOFU-pinned peer (see [`crate::commands::safety`]), and
//! verification of this build against the binaries log. All proof, Merkle and
//! signature checks go through the same `verifiable_log_serve` verifier the
//! `pollis-verify` CLI uses, so the app can't reach a different verdict than an
//! auditor; only status derivation lives here, in pure functions.
//!
//! The log's public key is pinned ([`PINNED_LOG_PUBLIC_KEYS`]): a served key that
//! matches no pin is a hard alarm, since any key can sign a self-consistent
//! forged tree. Every status is advisory and never blocks the app;
//! `Unavailable` means "couldn't check".

use std::sync::Arc;

use serde::Serialize;
use sha2::{Digest, Sha256};

use verifiable_log_serve::release::{verify_release_via, Layer, ReleaseReport};
// The real served type: a local struct keeping only `public_key` silently drops
// the rotation-overlap `keys` array (#875).
use verifiable_log_serve::{AccountKeyVersion, AccountReport, PublicKeyDoc};

use crate::error::{Error, Result};
use crate::state::AppState;

/// One key this build accepts from the transparency log.
///
/// A set so rotation isn't a flag day: ship a release pinning both current key
/// and successor, wait out the overlap, then move the log. The reverse order
/// makes every rotation a fleet-wide false alarm. Design:
/// `docs/sth-signing-key-custody.md` §5.
pub struct PinnedKey {
    /// `key_id_for` of `public_key`, for diagnostics only; never a trust input.
    pub key_id: &'static str,
    /// ML-DSA-44 public key, lowercase hex (1312 bytes).
    pub public_key: &'static str,
    /// Milliseconds since epoch after which this key is rejected; `None` is the
    /// current key. Enforced locally, so a retiring key expires even on a client
    /// that never updates.
    pub not_after: Option<u64>,
}

/// The transparency log's pinned ML-DSA-44 keys. One must be the key
/// `/v1/account-keys/public_key.json` serves. This entry is fresh material from
/// the #732 rotation. An empty set can only withhold trust ([`check_pin`] gives
/// `Unavailable`), never `Ok` or `Alarm`.
pub const PINNED_LOG_PUBLIC_KEYS: &[PinnedKey] = &[PinnedKey {
    key_id: "6cbd4b2aed5c4bf1",
    public_key: "56ab128f3f10107382802e69d3de8659d0127c711feb9c849f5b213c6f2d0af3b5fe41f581b202b385906fc42e4421747e84939054d160c551536131e41508a82b1f3ff0a07bcc4cee5e2eae8e85155d5c9e0dbc6e7683811649fb9e3b1f18c7ed070dbf61f2a058915b33f8ad3edcd135dd18770053e5ac971b13d17d95e16e98f47a852d600c47cbc0349354af2898803cfec7112660076d20027cb67870e18fb25ee327a36743fa812ccf93ba0769ddbd3d42ab40849ac8c98357b64eaf1ffc242abb12fddef4d8cdfa02448b4d99546b448e589657f898a47c6f30ddd88edd3f4456470e0a151e5fd601750c8b0489d3471897cfa78e0d7a00d938dfe876ef243117c972e041fdb00aa7af30d34184153cfd7b1e3b481dc562bbfc82bc20fe8ac4d9845f41de49fc33b6f94494df7088b06c7cb9ae35db86ac0fd293ca403046cec46ca9b12c755670d3d9b14c300b11ec292cd5e37d9f9e5e5d1729222a33bf1e13440f44dbf1b4d4104c612db4e269760868be5ff99f9ed269625fa4f39e21713a14293285e95f8a8e8cecd9db8e6a70c36340280322eab3490270ac640f706a23e81d79111dead641eaf7b926582ed0b0422f9addc0091d731a4fe1b9079be8bd75df23f5f9bf287beab7f67f763e04f0245bf9c705136d04eb8391fb4b4f12bfba44ae49bb6f32ddb0d539e59cd0159120b2fb1718f57e12a846638dbe0b650bfcd5a6cc74cd315b49136ea4e13d431a7f3a4c38fc783a82ca2b4c44a2f379c8aa9704d4639de3f94466662c97fbbd834db97a90405c382b5039803f4e4ed5c6b57487c8d23ad9e4d319df3466c49ef1e1cef526ddad1db5fa14f3b067b40580e068582dc428e21dbdc3df848e8e00fe1181f8e0d1409ab9a8757aef008b67191f4368f37cbd587ff65acdf07adbb989d09cc3318e346ca71c029557f2c523c204defab472b3dcb09bfbb95d5d1665a360a00faeb09b660f13fdc00f7b53fbfeaa58f87a208ad4551bcbe4307bf4d8451e027f4cc33cd55700016795c3164b1bc90d9dd1737b49d2e9e4b190128d2e62a44a80c1375c616aa2871ae7ad4a914102551380a8f8edb68c2df02bdf52607a7432ea7026f6a1efcdb37ecc11ecf1623ec6979e5d65c2812a997121010cd5fd9a98b9ed34edc17b667bfd37ef2be6dfe67fbdde03fa95bb80d0e1c7336263042ef44c4f9d28f1bf959bdc24c09cf8269378705022ff476fce91dbba6c8ffec00b27572eaa4835b59948d7a625ccc84ff4ac062176f4972f5131a961b17c7ff0010d2f2f3f8c12b7bf05fb9771d64a24fdab058f4bf3a155ade6a496b9a09d43a7673b5d8fb6519e01bf911ca78cc23f95943f63db72883d522fe24d4b7c7a26c7fd43b4f6f7496acf9ea2cab2e3cd6fc274964b576084c820bae79dbaa331d11751ec718660cd8e7847b7bacf31180803f681fb349b96338c98c791f74bc95e0d37b2810632159bc3175fed2e16038d45d35e4628250e8c9fb66c5bb2238f6456901f657e9655d3d5a09ff4952a0b9eb9c614f78c27626a136ef281f7099f68e898628530ef690851c179ef6a02448d498e49b2c362c839832100f4a9bf4abf17d496c71bfb5263da345d952b275f04707b31b9f6575da6dd2be799b90cc615f52ec32b4833a7e619d7f34f91f16edc38bc0a869c7211473f3ab90255446e0b7efbb2b97e8111d43b039ec0469b020f38925aad61e229836c96fad5bf3c3cad8f2c1c8b56cd819e8972d108dbfa8cd518177feaa7f4e0b547584a9a5d39ad4f1e8010cfead998ec18991cb89031a11c03cbd1ee7e0a1436da10ef154db13d4850c687c0a668215c9c8b7b1c",
    not_after: None,
}];

/// A pinned offline root key that vouches for signer keys (#754). With the root
/// pinned, a leaked signing seed is repaired by a root-signed statement the
/// online signer can't forge, rather than by a release verified through the
/// tree the leaked key signs.
#[derive(Debug, Clone, Copy)]
pub struct PinnedRoot {
    pub key_id: &'static str,
    /// ML-DSA-44 public key, lowercase hex (1312 bytes).
    pub public_key: &'static str,
    /// Milliseconds since epoch after which this root is rejected; `None` is current.
    pub not_after: Option<u64>,
}

/// The pinned offline roots, minted by the root ceremony (#754). The private half
/// is offline and absent from CI, Doppler and this repo, so a build-system
/// compromise can't re-authorise a signer. Rotation lists both, with the outgoing
/// one carrying `not_after`.
pub const PINNED_LOG_ROOT_KEYS: &[PinnedRoot] = &[PinnedRoot {
    key_id: "511fed0e9f262467",
    public_key: "c415c83da3d9035a0cd86087d7f4850299e45213f4e28d6b979c791165fdeeb4ca8430eb91c8106f65075649455525a902ce1b10a661d7129d6aa7d25d99ab5033fdadf902d89fc9a5d5740f46c2665e33868336e4002cca6d94359eca45d4dd83e0f6509986d3959fe0a0990ab8b5f7dd6c60b4def0c2a3122e03f654332db10c41d1b9e19d88d659add64f9b3485687bd7fab5fe0b01242f9d74d0ebfa8d5436b3180f72cfdf78873ec5d04251ed25f898b37db5c9b71e0bf1bd15c1cf71e84db053462b55c80870f8aec1351196b6c0459fd5e1683d00d1a27f33b21b195bd0e3238d76ea9f613b46556e3b86a6df71e64af8a0a78327fe248f158a902152413b1d6e0e8bfb43b0186d4b34b34dc3e71c6bacc6770e88ac52f5f5989b550025e016091efa3245fedd7b12997288b0b3e1f2b01e0924f2c80044198ad90a6f595a0d06ffe851f33bd4487f293c6a63970c2432c538180700495ba755bdbaf525fce59f6c75fcfbced8e0df1b16e00c185438d437cd4fc6fe8aa1dccc52dc6757958766ecce9064a19ff6f441cb823601cc20a2bf27e0b59499f7e4a5d5c0ed117012ede65982ffc160d20256eb42f5eaa06230a91b999afaa4514b74fb2896d049251d912016c5dae3767f3a08f079ce96ce34dbf7a8bbd677a9008ebcd45f214438788db8dfde3276ff2de38a42217025f3e86da8afe2278357f0fd80ae10c010e1f1bf74acb774f3bfe8d26cefe9004e16d7ee2267ee0358664853c0d5ae71c8f9a0e5376cee3ccb5f0d956f7db7253ce1f035bd446bda49dbf3461aae23ce685a4c7d4dfb9c14429685e55d84dc0f819f0bcb3ee7b02b4901f8c2fb87f3bd10606096716fb9862787717e2e3c3930340ce7d9c2a8bd591297130f55edb6398e6b8243012b22dd8d7ff9937c651bca6cb45373a433b500f9a219d488ac2320742e89128f0799a834e5e01dacf500e2b0998954edc87dfffc3cf85bebe6bca9608a34e4efc4092de97b2ddef75a2669060fe2522ad5ac8157d15e0e983055145f824b4a4705a6a5a6952d6c3b234932505d9274157bed8aa77cfa836e63e79f41a881da33257dec31b5aa85be39746310db532b65639bae569e56fbbf05019bdb8ab1f7ef0cdc6adb3854d73534d78d96dcd6e1d9d357fa9a7ac5d90c5c4680c08e46cf3f9103c3b4aaa2e12115bf9d428e6754ec11430846025a4e03abf3df748e1cb446d518d0bb57584f94bf14dc407cce3321d2545b7d03d35f247149ca842d29796f6de0f0bb61b18143dbe1bac184eefaca43fad62abd7443eeb8d55f6640701e34373b1417c36bb0caee24762efc85f26eb4b369cdf9cb281d825ed924eb83b48d1f00586d0e893d0f87345b31bdb2efca8ae77200accc33f8292ea927af781f3de0f5ad761c1f3adb83d78067a79f78fb816e22877aa6caf565bfa6493de957b89d48c840018dcaa9856880648d55f6b341fe8caf5cb5c611ba6b5d0554c368017cac85bdd06989631ceb9d976bbdce89804584bc9b92678dee148631e1805ae1155798e9d6c2c223f27297fd1ca7510d14f9f4ccd47a032a0ffc02840cf84ee2286c7b5db863b8ca1ea86f08f29798a2be748ecf283ad5e11304c72a6254737b69c278951ae5b0611ac01576edda99fca114e2f5a3b4dc913d07bb7313a8a907b5544e22cfffd35166357a0e0cfafc70d1ef8f9eb22fc96bd3f00a165c0f64d0f01d2f050445570d7d292502ca37004633b401796d2372f1881d64bc86cf9722e4a34e8bd97e36fa129e58cec74fe87d7ef6b568549013d7aacff4086ccce0dfab996dcd613a7b920187f05efae94711972372aebd7a7bcea19e",
    not_after: None,
}];

/// What a fetched key set told us about who may sign a given tree.
#[derive(Debug, PartialEq, Eq)]
pub enum KeySetOutcome {
    /// Verified under a pinned root: these signer keys (lowercase hex) are
    /// acceptable for the tree now.
    Signers(Vec<String>),
    /// No root pinned, so nothing to check against: withhold trust, never alarm.
    NoRootPinned,
    /// Not verifiable under any live pinned root, malformed, or expired: alarm.
    Untrusted(String),
}

/// Derive the acceptable signer keys for `context` from a served key-set
/// document. Pure; `now_ms` is passed in. Every key-set trust decision lives
/// here.
pub fn signers_from_key_set(
    document: &str,
    roots: &[PinnedRoot],
    context: &[u8],
    now_ms: u64,
) -> KeySetOutcome {
    let live: Vec<&PinnedRoot> = roots
        .iter()
        .filter(|r| r.not_after.is_none_or(|exp| now_ms <= exp))
        .collect();
    if live.is_empty() {
        return KeySetOutcome::NoRootPinned;
    }

    let statement: verifiable_log_serve::KeySetStatement = match serde_json::from_str(document) {
        Ok(s) => s,
        Err(e) => return KeySetOutcome::Untrusted(format!("key set is not readable: {e}")),
    };

    let mut verified = false;
    for r in &live {
        let Ok(vk) = verifiable_log_serve::root_key_from_hex(r.public_key) else {
            // A malformed pin is our bug, not the server misbehaving.
            continue;
        };
        if statement.verify(&vk).is_ok() {
            verified = true;
            break;
        }
    }
    if !verified {
        return KeySetOutcome::Untrusted(
            "the served key set is not signed by any root this build pins".into(),
        );
    }

    let active = statement.active_signers_for(context, now_ms);
    if active.is_empty() {
        // Verified but grants nothing (expired, or no signer for this tree): distinct
        // from a forgery.
        return KeySetOutcome::Untrusted(
            "the key set verified but authorises no signer for this tree right now".into(),
        );
    }
    KeySetOutcome::Signers(
        active
            .iter()
            .map(|s| s.public_key.to_ascii_lowercase())
            .collect(),
    )
}

/// Default base URL of the published transparency log.
const DEFAULT_TRANSPARENCY_URL: &str = "https://verify.pollis.com";

/// Env var overriding [`DEFAULT_TRANSPARENCY_URL`] (compile-time embed, then
/// runtime).
const TRANSPARENCY_URL_ENV: &str = "POLLIS_TRANSPARENCY_URL";

fn transparency_base_url() -> String {
    option_env!("POLLIS_TRANSPARENCY_URL")
        .map(|s| s.to_string())
        .or_else(|| std::env::var(TRANSPARENCY_URL_ENV).ok())
        .unwrap_or_else(|| DEFAULT_TRANSPARENCY_URL.to_string())
}

/// Verdict of an account-key audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditStatus {
    Ok,
    /// The local key/version isn't published yet (the log publishes daily). Absence
    /// alone is not an alarm.
    Pending,
    /// A real discrepancy: the chain disagrees with the local key at the same or a
    /// higher version, the served key matches no pin, or verification failed.
    Alarm,
    /// The log host was unreachable or served malformed prerequisites.
    Unavailable,
}

/// Result of [`self_audit_account_key`].
#[derive(Debug, Clone, Serialize)]
pub struct SelfAuditReport {
    pub status: AuditStatus,
    /// Shown verbatim in the UI.
    pub detail: String,
    pub my_identity_version: i64,
    /// Lowercase hex.
    pub my_account_id_pub: String,
    /// `None` when unavailable.
    pub report: Option<AccountReport>,
}

/// Result of [`audit_peer_account_key`].
#[derive(Debug, Clone, Serialize)]
pub struct PeerAuditReport {
    pub status: AuditStatus,
    pub detail: String,
    pub peer_user_id: String,
    /// `None` if there is no local pin.
    pub pinned_identity_version: Option<i64>,
    /// The pinned key is in the history and a newer version has since been
    /// published: the peer rotated (still `Ok`).
    pub key_rotated: bool,
    pub report: Option<AccountReport>,
}

/// Verdict of the in-app "verify this build" check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildVerifyStatus {
    /// This build's fingerprint is published for its release tag.
    Verified,
    /// The release tag isn't in the binaries tree yet (it republishes after each
    /// release). Not an alarm.
    Pending,
    /// The tag is published without this build's fingerprint, or the tree failed
    /// verification or pin check: the running binary is not one Pollis attested.
    Mismatch,
    /// The log host was unreachable or the local payload couldn't be hashed.
    Unavailable,
}

/// Result of [`verify_own_build`].
#[derive(Debug, Clone, Serialize)]
pub struct BuildVerifyReport {
    pub status: BuildVerifyStatus,
    /// Shown verbatim in the UI.
    pub detail: String,
    pub version: String,
    /// The source commit baked at build time; `None` if not baked, never fabricated.
    pub commit: Option<String>,
    /// This device's hash of its own binary, lowercase hex.
    pub my_payload_sha256: String,
    /// `None` when unavailable or the served key wasn't pinned.
    pub report: Option<ReleaseReport>,
}

/// Self-audit: verify this account's key history against the log and compare the
/// latest published version with this device's current key.
pub async fn self_audit_account_key(
    state: &Arc<AppState>,
    my_user_id: String,
) -> Result<SelfAuditReport> {
    // The log must stay on a separate origin from the DS: the audit compares what
    // the DS says our key is against an independent log. Served by the DS, it would
    // be the DS attesting to itself.
    let (my_pub, my_version) =
        crate::commands::ds_reads::account_key(state, &my_user_id).await?;
    let my_pub_hex = hex::encode(&my_pub);

    let base = transparency_base_url();
    let overlay = state.overlay_handle();
    match fetch_and_verify(overlay.as_deref(), &base, &my_user_id).await {
        Ok((report, served_key)) => {
            Ok(derive_self_audit(
                &report,
                &served_key,
                PINNED_LOG_PUBLIC_KEYS,
                &my_pub_hex,
                my_version,
            ))
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
    // `None` if this peer was never pinned.
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
            Ok(derive_peer_audit(
                &report,
                &served_key,
                PINNED_LOG_PUBLIC_KEYS,
                &peer_user_id,
                pin,
            ))
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

/// Verify this build against the binaries log: hash the running binary, verify
/// the release tree for its tag (pinned key only) and report whether the hash is
/// attested. Same `verify_release` as `pollis-verify release`. Advisory; never
/// gates launch or update.
pub async fn verify_own_build(state: &Arc<AppState>) -> Result<BuildVerifyReport> {
    let version = env!("CARGO_PKG_VERSION").to_string();
    let commit = option_env!("POLLIS_GIT_COMMIT").map(str::to_string);
    // Attest leaves use the git tag (`v1.1.0`); the package version lacks the `v`.
    let release_tag = format!("v{version}");

    let mk_unavailable = |detail: String, my_hash: String| BuildVerifyReport {
        status: BuildVerifyStatus::Unavailable,
        detail,
        version: version.clone(),
        commit: commit.clone(),
        my_payload_sha256: my_hash,
        report: None,
    };

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

    // Pin-check the served key first: `verify_release` trusts whatever key the host
    // serves.
    let overlay = state.overlay_handle();
    match fetch_served_binaries_public_key(overlay.as_deref(), &base).await {
        Ok(served) => match check_pin(&served, PINNED_LOG_PUBLIC_KEYS) {
            PinCheck::Match => {}
            PinCheck::Mismatch => {
                return Ok(BuildVerifyReport {
                    status: BuildVerifyStatus::Mismatch,
                    detail: pin_mismatch_detail(&served, PINNED_LOG_PUBLIC_KEYS),
                    version,
                    commit,
                    my_payload_sha256: my_hash,
                    report: None,
                });
            }
            PinCheck::RotationPending => {
                return Ok(mk_unavailable(PIN_PENDING_DETAIL.to_string(), my_hash));
            }
        },
        Err(detail) => return Ok(mk_unavailable(detail, my_hash)),
    }

    // Blocking `ureq`, so on the blocking pool. The proxy is computed first so the
    // fetch rides the overlay when it is on; a proxied failure is `Unavailable`,
    // never a silent direct fetch.
    let proxy = overlay_proxy(overlay.as_deref());
    let base_owned = base.clone();
    let tag_owned = release_tag.clone();
    match tokio::task::spawn_blocking(move || {
        verify_release_via(&base_owned, &tag_owned, proxy.as_deref())
    })
    .await
    {
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

/// SOCKS5 proxy URL for the blocking `ureq` verifier, or `None` with the overlay
/// off. The transparency host is first-party, so it is always routed when the
/// overlay runs.
///
/// `socks5://`, not `socks5h://`: `ureq` doesn't accept the `socks5h` token, but
/// for a hostname it already sends `ATYP=DOMAIN`, resolving proxy-side, so no
/// local DNS lookup leaks.
fn overlay_proxy(overlay: Option<&pollis_relay::OverlayHandle>) -> Option<String> {
    overlay.map(|h| format!("socks5://{}", h.socks_addr()))
}

/// Fetch the served public key and run the shared account verifier for
/// `user_id`. `Err(detail)` maps to [`AuditStatus::Unavailable`].
async fn fetch_and_verify(
    overlay: Option<&pollis_relay::OverlayHandle>,
    base: &str,
    user_id: &str,
) -> std::result::Result<(AccountReport, String), String> {
    // Fetched separately: the verifier folds the key into its verdict without
    // surfacing it, and `derive_*` pin-checks it.
    let served_key = fetch_served_public_key(overlay, base)
        .await
        .map_err(|e| format!("could not fetch the log's public key: {e}"))?;

    // Same function as `pollis-verify account`, on the blocking pool. The proxy is
    // computed before spawning; a proxied failure is `Unavailable`, never a direct
    // fallback that would leak the IP.
    let proxy = overlay_proxy(overlay);
    let base_owned = base.to_string();
    let user_owned = user_id.to_string();
    let report = tokio::task::spawn_blocking(move || {
        verifiable_log_serve::account::verify_account_via(
            &base_owned,
            &user_owned,
            proxy.as_deref(),
        )
    })
    .await
    .map_err(|e| format!("account verifier task failed: {e}"))?
    .map_err(|e| format!("verifying the account-key log failed: {e}"))?;

    Ok((report, served_key))
}

/// Fetch `/v1/account-keys/public_key.json` and return its key (lowercase hex),
/// through the overlay when on.
async fn fetch_served_public_key(
    overlay: Option<&pollis_relay::OverlayHandle>,
    base: &str,
) -> Result<String> {
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
    Ok(active_served_key(&doc))
}

/// The key a served [`PublicKeyDoc`] says is signing now, lowercase hex: what
/// [`check_pin`] compares.
///
/// Deliberately not the served overlap set: that is the log's claim, while the
/// pin is this build's. Matching against the set would let a hostile host list
/// the genuine pinned key as retired, sign heads with its own key, and pass.
/// Consequently a build pinning only a retired key reports a mismatch; the
/// rotation ordering (`docs/sth-signing-key-custody.md` §5) prevents that.
fn active_served_key(doc: &PublicKeyDoc) -> String {
    doc.public_key.to_lowercase()
}

/// Fetch `/v1/binaries/public_key.json` and return its active key (lowercase
/// hex); the caller pin-checks it. `Err(detail)` on transport/parse failure.
async fn fetch_served_binaries_public_key(
    overlay: Option<&pollis_relay::OverlayHandle>,
    base: &str,
) -> std::result::Result<String, String> {
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
    Ok(active_served_key(&doc))
}

/// This build's hash paired with the leaf layer it is comparable against. A
/// `payload` leaf is a tree hash an installed app has no preimage for, while an
/// `exe` leaf is the executable's own sha256; comparing across them always
/// misses and accuses a genuine build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MyPayload {
    /// Compare against `payload` leaves (Linux AppImage: `$APPIMAGE` is the logged
    /// file). The stronger check.
    Payload(String),
    /// Compare against `exe` leaves: the running executable's sha256, logged per
    /// bundle by `scripts/attest-binaries.sh`.
    Exe(String),
}

impl MyPayload {
    fn sha256(&self) -> &str {
        match self {
            MyPayload::Payload(h) | MyPayload::Exe(h) => h,
        }
    }

    fn layer(&self) -> Layer {
        match self {
            MyPayload::Payload(_) => Layer::Payload,
            MyPayload::Exe(_) => Layer::Exe,
        }
    }
}

/// This build's hash, computed the way `scripts/attest-binaries.sh` logs it: the
/// AppImage file itself (`payload` leaf) on Linux AppImage, else
/// `current_exe()` (`exe` leaf).
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

fn sha256_file(path: &std::path::Path) -> std::result::Result<String, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("could not open {} for hashing: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|e| format!("could not read {} for hashing: {e}", path.display()))?;
    Ok(hex::encode(hasher.finalize()))
}

/// The TOFU-pinned `(account_id_pub, identity_version)` for a peer.
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

/// Derive the self-audit verdict. Pure.
pub fn derive_self_audit(
    report: &AccountReport,
    served_public_key: &str,
    log_pin: &[PinnedKey],
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

    // Pin first: an unpinned key invalidates every signature check below.
    match check_pin(served_public_key, log_pin) {
        PinCheck::Match => {}
        PinCheck::Mismatch => {
            return mk(
                AuditStatus::Alarm,
                pin_mismatch_detail(served_public_key, log_pin),
            );
        }
        PinCheck::RotationPending => {
            return mk(AuditStatus::Unavailable, PIN_PENDING_DETAIL.to_string());
        }
    }

    if !report.chain_valid {
        return mk(
            AuditStatus::Alarm,
            first_violation_detail(report, "the published account-key chain failed verification"),
        );
    }

    let Some(latest) = latest_version(report) else {
        return mk(
            AuditStatus::Pending,
            "your key history is not in the published log yet — the log publishes daily, so a \
             recent signup is invisible until the next publish"
                .to_string(),
        );
    };

    match (latest.identity_version as i64).cmp(&my_identity_version) {
        // We just signed up or rotated and the daily publish hasn't run.
        std::cmp::Ordering::Less => mk(
            AuditStatus::Pending,
            format!(
                "your current key version {my_identity_version} is not in the published log yet \
                 (newest published is version {}) — the log publishes daily",
                latest.identity_version
            ),
        ),
        // Same version must be the same key, or this is selective targeting.
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
        // A rotation this device never performed: unauthorized rotation or targeting.
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

/// Derive the peer-audit verdict against the locally pinned key, if any. Pure.
pub fn derive_peer_audit(
    report: &AccountReport,
    served_public_key: &str,
    log_pin: &[PinnedKey],
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

    match check_pin(served_public_key, log_pin) {
        PinCheck::Match => {}
        PinCheck::Mismatch => {
            return mk(
                AuditStatus::Alarm,
                pin_mismatch_detail(served_public_key, log_pin),
                false,
            );
        }
        PinCheck::RotationPending => {
            return mk(AuditStatus::Unavailable, PIN_PENDING_DETAIL.to_string(), false);
        }
    }
    if !report.chain_valid {
        return mk(
            AuditStatus::Alarm,
            first_violation_detail(report, "the published account-key chain failed verification"),
            false,
        );
    }

    let Some((pinned_pub, pinned_version)) = pinned else {
        return mk(
            AuditStatus::Pending,
            "you have not pinned this peer's key yet, so there is nothing to audit against"
                .to_string(),
            false,
        );
    };

    if !report.found || report.keys.is_empty() {
        return mk(
            AuditStatus::Pending,
            "this peer has no published key history yet — the log publishes daily".to_string(),
            false,
        );
    }

    // The pinned key must be in the published history, or the server showed us a
    // key it never published.
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

/// Derive the build-verify verdict. Pure; the served key was pin-checked in
/// [`verify_own_build`].
///
/// Invalid chain is `Mismatch`; tag not found is `Pending`; no leaf of this
/// install's comparable layer (tags before `exe` leaves) is `Unavailable`; our
/// hash among comparable leaves is `Verified`; otherwise `Mismatch`, the
/// targeted-backdoor signal.
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

    if !report.chain_valid {
        return mk(
            BuildVerifyStatus::Mismatch,
            first_release_violation(report, "the published binaries tree failed verification"),
        );
    }

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

    // Compare only against this install's layer (`exe` leaves carry the hash in
    // `artifact_sha256`, `payload` leaves in `payload_sha256`). Not filtered by
    // platform/arch: a labelling mistake must never become a false tamper alarm.
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

    // No comparable leaf is "couldn't check", not "unattested": releases up to
    // v1.5.2 have no `exe` leaves.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinCheck {
    Match,
    /// Served key differs: hostile host, hard alarm.
    Mismatch,
    /// No live pin: withhold trust; "couldn't check", never "failed".
    RotationPending,
}

/// Compare the served key against the pinned set (case-insensitive), ignoring
/// pins whose window closed before `now_ms`. Any live pin matching is `Match`, so
/// moving the log within the overlap is invisible. Expiry is enforced locally.
/// An empty or wholly expired set withholds trust.
fn check_pin_at(served: &str, pins: &[PinnedKey], now_ms: u64) -> PinCheck {
    let mut any_live = false;
    for p in pins {
        if p.not_after.is_some_and(|exp| now_ms > exp) {
            continue;
        }
        any_live = true;
        if served.eq_ignore_ascii_case(p.public_key) {
            return PinCheck::Match;
        }
    }
    if any_live {
        PinCheck::Mismatch
    } else {
        PinCheck::RotationPending
    }
}

fn check_pin(served: &str, pins: &[PinnedKey]) -> PinCheck {
    check_pin_at(served, pins, now_ms())
}

/// Wall-clock milliseconds. A clock wrong into the future expires a key early,
/// degrading to `Unavailable`, never a false alarm.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Shown when no log pin is live; reachable only mid-rotation.
const PIN_PENDING_DETAIL: &str =
    "this build pins no transparency-log key, so the published tree cannot be checked against \
     anything — treating it as unverified rather than trusted";

/// Highest `identity_version` in the chain.
fn latest_version(report: &AccountReport) -> Option<&AccountKeyVersion> {
    report.keys.iter().max_by_key(|k| k.identity_version)
}

fn first_violation_detail(report: &AccountReport, fallback: &str) -> String {
    report
        .violations
        .first()
        .cloned()
        .unwrap_or_else(|| fallback.to_string())
}

fn first_release_violation(report: &ReleaseReport, fallback: &str) -> String {
    report
        .violations
        .first()
        .cloned()
        .unwrap_or_else(|| fallback.to_string())
}

fn pin_mismatch_detail(served: &str, pins: &[PinnedKey]) -> String {
    let pinned = if pins.is_empty() {
        "(none)".to_string()
    } else {
        pins.iter()
            .map(|p| short_hex(p.public_key))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "the log served public key {} but this build pins {} — refusing to trust the served tree",
        short_hex(served),
        pinned
    )
}

fn short_hex(s: &str) -> String {
    if s.len() <= 12 {
        s.to_string()
    } else {
        format!("{}\u{2026}{}", &s[..6], &s[s.len() - 4..])
    }
}

// Pure status-derivation tests; the async commands are covered by the flows
// harness.

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in pin, so the derivation logic is tested independent of the shipped
    /// constant.
    const LOG_PIN: &str = "10900000000000000000000000000000000000000000000000000000000000ff";
    const PIN_SET: &[PinnedKey] = &[PinnedKey {
        key_id: "0000000000000000",
        public_key: LOG_PIN,
        not_after: None,
    }];
    const NO_PINS: &[PinnedKey] = &[];

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

    #[test]
    fn self_ok_when_latest_matches_my_key() {
        let r = report(vec![version(1, 0, KEY_A)], true);
        let out = derive_self_audit(&r, LOG_PIN, PIN_SET, KEY_A, 1);
        assert_eq!(out.status, AuditStatus::Ok);
    }

    #[test]
    fn self_ok_is_case_insensitive_on_key() {
        let r = report(vec![version(1, 0, KEY_A)], true);
        let out = derive_self_audit(&r, LOG_PIN, PIN_SET, &KEY_A.to_uppercase(), 1);
        assert_eq!(out.status, AuditStatus::Ok);
    }

    #[test]
    fn self_pending_when_history_empty() {
        let r = report(vec![], true);
        let out = derive_self_audit(&r, LOG_PIN, PIN_SET, KEY_A, 1);
        assert_eq!(out.status, AuditStatus::Pending);
    }

    #[test]
    fn self_pending_when_my_version_newer_than_published() {
        let r = report(vec![version(1, 0, KEY_A)], true);
        let out = derive_self_audit(&r, LOG_PIN, PIN_SET, KEY_B, 2);
        assert_eq!(out.status, AuditStatus::Pending);
    }

    #[test]
    fn self_alarm_when_same_version_different_key() {
        let r = report(vec![version(1, 0, KEY_A)], true);
        let out = derive_self_audit(&r, LOG_PIN, PIN_SET, KEY_B, 1);
        assert_eq!(out.status, AuditStatus::Alarm);
    }

    #[test]
    fn self_alarm_when_published_version_higher() {
        let r = report(vec![version(1, 0, KEY_A), version(2, 1, KEY_B)], true);
        let out = derive_self_audit(&r, LOG_PIN, PIN_SET, KEY_A, 1);
        assert_eq!(out.status, AuditStatus::Alarm);
    }

    #[test]
    fn self_alarm_on_pin_mismatch_even_if_chain_valid() {
        let r = report(vec![version(1, 0, KEY_A)], true);
        let out = derive_self_audit(&r, KEY_B, PIN_SET, KEY_A, 1);
        assert_eq!(out.status, AuditStatus::Alarm);
    }

    #[test]
    fn self_alarm_on_invalid_chain() {
        let r = report(vec![version(1, 0, KEY_A)], false);
        let out = derive_self_audit(&r, LOG_PIN, PIN_SET, KEY_A, 1);
        assert_eq!(out.status, AuditStatus::Alarm);
    }

    /// No live pin must withhold trust: `Unavailable` whether the served tree is
    /// perfect or broken.
    #[test]
    fn no_pin_withholds_trust_without_alarming() {
        let good = report(vec![version(1, 0, KEY_A)], true);
        let out = derive_self_audit(&good, KEY_A, NO_PINS, KEY_A, 1);
        assert_eq!(out.status, AuditStatus::Unavailable);

        let bad = report(vec![version(1, 0, KEY_A)], false);
        let out = derive_self_audit(&bad, KEY_B, NO_PINS, KEY_A, 1);
        assert_eq!(out.status, AuditStatus::Unavailable);

        let out = derive_peer_audit(&good, KEY_A, NO_PINS, "peer", Some((KEY_A, 1)));
        assert_eq!(out.status, AuditStatus::Unavailable);
    }

    /// Shipped pins must be full 1312-byte ML-DSA-44 keys.
    #[test]
    fn shipping_pins_are_ml_dsa_sized_with_derived_ids() {
        for pin in PINNED_LOG_PUBLIC_KEYS {
            assert_eq!(pin.public_key.len(), verifiable_log_serve::STH_PUB_LEN * 2);
            assert!(pin.public_key.chars().all(|c| c.is_ascii_hexdigit()));
            // The id must be derived from the key, so a copy-paste slip can't name another.
            let vk = verifiable_log_serve::verifying_key_from_hex(pin.public_key)
                .expect("pinned key must parse as ML-DSA-44");
            assert_eq!(pin.key_id, verifiable_log_serve::key_id_for(&vk));
        }
    }

    const OLD_KEY: &str = "aa900000000000000000000000000000000000000000000000000000000000ff";
    const NEW_KEY: &str = "bb900000000000000000000000000000000000000000000000000000000000ff";
    const WINDOW_CLOSES: u64 = 2_000;

    const OVERLAP: &[PinnedKey] = &[
        PinnedKey { key_id: "new", public_key: NEW_KEY, not_after: None },
        PinnedKey { key_id: "old", public_key: OLD_KEY, not_after: Some(WINDOW_CLOSES) },
    ];

    /// Inside the window both keys verify, so moving the log is invisible.
    #[test]
    fn overlap_window_accepts_both_keys() {
        assert_eq!(check_pin_at(OLD_KEY, OVERLAP, 1_000), PinCheck::Match);
        assert_eq!(check_pin_at(NEW_KEY, OVERLAP, 1_000), PinCheck::Match);
    }

    /// Once the window closes the retired key is rejected, even though it's listed.
    #[test]
    fn retired_key_stops_being_accepted_after_its_window() {
        assert_eq!(check_pin_at(OLD_KEY, OVERLAP, WINDOW_CLOSES), PinCheck::Match);
        assert_eq!(
            check_pin_at(OLD_KEY, OVERLAP, WINDOW_CLOSES + 1),
            PinCheck::Mismatch
        );
        assert_eq!(
            check_pin_at(NEW_KEY, OVERLAP, WINDOW_CLOSES + 1),
            PinCheck::Match
        );
    }

    /// An unknown key is still a hard alarm.
    #[test]
    fn overlap_still_alarms_on_an_unpinned_key() {
        assert_eq!(check_pin_at(LOG_PIN, OVERLAP, 1_000), PinCheck::Mismatch);
    }

    /// Every pin expired withholds trust: aged-out pins aren't evidence of a hostile
    /// host.
    #[test]
    fn wholly_expired_set_withholds_trust_instead_of_alarming() {
        const ALL_EXPIRED: &[PinnedKey] = &[PinnedKey {
            key_id: "old",
            public_key: OLD_KEY,
            not_after: Some(WINDOW_CLOSES),
        }];
        assert_eq!(
            check_pin_at(OLD_KEY, ALL_EXPIRED, WINDOW_CLOSES + 1),
            PinCheck::RotationPending
        );
        assert_eq!(check_pin_at(LOG_PIN, NO_PINS, 1_000), PinCheck::RotationPending);
    }

    #[test]
    fn pin_comparison_is_case_insensitive() {
        assert_eq!(
            check_pin_at(&NEW_KEY.to_uppercase(), OVERLAP, 1_000),
            PinCheck::Match
        );
    }

    /// At most one pin may be open-ended, or a retired key is trusted forever.
    #[test]
    fn at_most_one_pin_has_no_expiry() {
        let open = PINNED_LOG_PUBLIC_KEYS
            .iter()
            .filter(|p| p.not_after.is_none())
            .count();
        assert!(open <= 1, "{open} pins have no not_after; at most one may");
    }

    // #875: these parse the real `PublicKeyDoc`, which a hand-rolled struct would
    // silently strip of its rotation key set.

    fn served_doc(active: &str, retiring: &str, not_after: u64) -> String {
        format!(
            r#"{{"public_key":"{active}","keys":[
                 {{"key_id":"new","algorithm":"ML-DSA-44","public_key":"{active}"}},
                 {{"key_id":"old","algorithm":"ML-DSA-44","public_key":"{retiring}","not_after":{not_after}}}
               ]}}"#
        )
    }

    #[test]
    fn a_served_key_set_is_parsed_not_discarded() {
        let doc: PublicKeyDoc =
            serde_json::from_str(&served_doc(NEW_KEY, OLD_KEY, WINDOW_CLOSES)).unwrap();

        assert_eq!(doc.keys.len(), 2, "the rotation key set must survive parsing");
        assert_eq!(doc.public_key, NEW_KEY);
        assert!(doc.keys[0].not_after.is_none());
        assert_eq!(doc.keys[1].public_key, OLD_KEY);
        assert_eq!(doc.keys[1].not_after, Some(WINDOW_CLOSES));

        assert_eq!(doc.active_keys(WINDOW_CLOSES).len(), 2);
        assert_eq!(doc.active_keys(WINDOW_CLOSES + 1).len(), 1);
    }

    /// A pre-key-set document still parses, as every shipped verifier relies on.
    #[test]
    fn a_legacy_served_document_without_a_key_set_still_parses() {
        let doc: PublicKeyDoc =
            serde_json::from_str(&format!(r#"{{"public_key":"{NEW_KEY}"}}"#)).unwrap();

        assert!(doc.keys.is_empty());
        assert_eq!(active_served_key(&doc), NEW_KEY);
    }

    /// The pin is checked against the key signing now, never the served set; see
    /// `active_served_key`.
    #[test]
    fn publishing_the_pinned_key_as_retired_does_not_launder_a_hostile_active_key() {
        let doc: PublicKeyDoc =
            serde_json::from_str(&served_doc(LOG_PIN, NEW_KEY, WINDOW_CLOSES)).unwrap();

        assert!(doc.keys.iter().any(|k| k.public_key == NEW_KEY));
        assert_eq!(active_served_key(&doc), LOG_PIN);
        assert_eq!(
            check_pin_at(&active_served_key(&doc), OVERLAP, 1_000),
            PinCheck::Mismatch
        );
    }

    #[test]
    fn an_active_key_this_build_pins_matches() {
        let doc: PublicKeyDoc =
            serde_json::from_str(&served_doc(NEW_KEY, OLD_KEY, WINDOW_CLOSES)).unwrap();
        assert_eq!(
            check_pin_at(&active_served_key(&doc), OVERLAP, 1_000),
            PinCheck::Match
        );
    }

    #[test]
    fn the_active_served_key_is_lowercased() {
        let doc: PublicKeyDoc =
            serde_json::from_str(&format!(r#"{{"public_key":"{}"}}"#, NEW_KEY.to_uppercase()))
                .unwrap();
        assert_eq!(active_served_key(&doc), NEW_KEY);
    }

    #[test]
    fn peer_ok_when_pinned_key_present() {
        let r = report(vec![version(1, 0, KEY_A)], true);
        let out = derive_peer_audit(&r, LOG_PIN, PIN_SET, "peer", Some((KEY_A, 1)));
        assert_eq!(out.status, AuditStatus::Ok);
        assert!(!out.key_rotated);
    }

    #[test]
    fn peer_ok_notes_rotation_when_newer_version_exists() {
        let r = report(vec![version(1, 0, KEY_A), version(2, 1, KEY_B)], true);
        let out = derive_peer_audit(&r, LOG_PIN, PIN_SET, "peer", Some((KEY_A, 1)));
        assert_eq!(out.status, AuditStatus::Ok);
        assert!(out.key_rotated);
    }

    #[test]
    fn peer_alarm_when_pinned_key_absent_from_history() {
        let r = report(vec![version(1, 0, KEY_A)], true);
        let out = derive_peer_audit(&r, LOG_PIN, PIN_SET, "peer", Some((KEY_Z, 1)));
        assert_eq!(out.status, AuditStatus::Alarm);
    }

    #[test]
    fn peer_pending_when_no_local_pin() {
        let r = report(vec![version(1, 0, KEY_A)], true);
        let out = derive_peer_audit(&r, LOG_PIN, PIN_SET, "peer", None);
        assert_eq!(out.status, AuditStatus::Pending);
    }

    #[test]
    fn peer_pending_when_peer_never_published() {
        let r = report(vec![], true);
        let out = derive_peer_audit(&r, LOG_PIN, PIN_SET, "peer", Some((KEY_A, 1)));
        assert_eq!(out.status, AuditStatus::Pending);
    }

    #[test]
    fn peer_alarm_on_pin_mismatch() {
        let r = report(vec![version(1, 0, KEY_A)], true);
        let out = derive_peer_audit(&r, KEY_B, PIN_SET, "peer", Some((KEY_A, 1)));
        assert_eq!(out.status, AuditStatus::Alarm);
    }

    #[test]
    fn peer_alarm_on_invalid_chain() {
        let r = report(vec![version(1, 0, KEY_A)], false);
        let out = derive_peer_audit(&r, LOG_PIN, PIN_SET, "peer", Some((KEY_A, 1)));
        assert_eq!(out.status, AuditStatus::Alarm);
    }

    const HASH_MINE: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const HASH_OTHER: &str = "2222222222222222222222222222222222222222222222222222222222222222";
    const HASH_SIGNED: &str = "3333333333333333333333333333333333333333333333333333333333333333";

    // Every real leaf carries a build recipe (#944), so fixtures do too.
    fn fixture_toolchain() -> serde_json::Value {
        serde_json::json!({
            "rustc": "1.96.0",
            "node": "20.11.1",
            "pnpm": "10.26.1",
            "runner_image": "ubuntu22@20260810.260.1+helper:ubuntu24@20260810.260.1",
            "source_date_epoch": 1_786_661_317_u64,
        })
    }

    // One `payload` leaf per hash; mirrors a pre-`exe` release (<= v1.5.2).
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
                    "toolchain": fixture_toolchain(),
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

    // A modern macOS-shaped release: payload leaf, `signed` dmg leaf, and `exe`
    // leaves carrying the Mach-O hash.
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
                "provenance_uri": "cdn.pollis.com/x.intoto.jsonl",
                "toolchain": fixture_toolchain(), "included": true,
            }),
            serde_json::json!({
                "platform": "darwin", "arch": "aarch64", "bundle": "dmg",
                "layer": "signed", "artifact_name": "pollis-macos.dmg",
                "payload_sha256": payload, "artifact_sha256": HASH_SIGNED,
                "provenance_uri": "cdn.pollis.com/x.intoto.jsonl",
                "toolchain": fixture_toolchain(), "included": true,
            }),
        ];
        for e in exes {
            artifacts.push(serde_json::json!({
                "platform": "darwin", "arch": "aarch64", "bundle": "dmg",
                "layer": "exe", "artifact_name": "pollis-macos.dmg",
                "payload_sha256": payload, "artifact_sha256": e,
                "provenance_uri": "cdn.pollis.com/x.intoto.jsonl",
                "toolchain": fixture_toolchain(), "included": true,
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
        // A failed tree is loud even if our hash appears among its leaves.
        let r = release_report("v1.1.0", true, false, &[HASH_MINE]);
        let out = derive_build_verify(&r, &mine(), "1.1.0", None);
        assert_eq!(out.status, BuildVerifyStatus::Mismatch);
    }

    #[test]
    fn exe_build_verified_against_exe_leaf() {
        let r = release_report_with_exe("v1.1.0", true, HASH_OTHER, &[HASH_MINE]);
        let out = derive_build_verify(&r, &my_exe(), "1.1.0", Some("abc1234"));
        assert_eq!(out.status, BuildVerifyStatus::Verified);
        assert_eq!(out.my_payload_sha256, HASH_MINE);
    }

    #[test]
    fn exe_build_mismatch_when_exe_leaf_present_but_ours_absent() {
        let r = release_report_with_exe("v1.1.0", true, HASH_OTHER, &[HASH_SIGNED]);
        let out = derive_build_verify(&r, &my_exe(), "1.1.0", None);
        assert_eq!(out.status, BuildVerifyStatus::Mismatch);
    }

    #[test]
    fn layers_are_never_compared_across() {
        // The invariant: an exe hash equal to the payload leaf must not read as
        // Verified.
        let r = release_report_with_exe("v1.1.0", true, HASH_MINE, &[HASH_OTHER]);
        assert_eq!(
            derive_build_verify(&r, &my_exe(), "1.1.0", None).status,
            BuildVerifyStatus::Mismatch
        );

        let r = release_report_with_exe("v1.1.0", true, HASH_OTHER, &[HASH_MINE]);
        assert_eq!(
            derive_build_verify(&r, &mine(), "1.1.0", None).status,
            BuildVerifyStatus::Mismatch
        );
    }

    #[test]
    fn pre_exe_release_is_unavailable_not_mismatch() {
        // Releases up to v1.5.2 logged no `exe` leaves: "couldn't check", never
        // "unattested" (the false alarm that shipped on v1.5.2).
        let old = release_report("v1.5.2", true, true, &[HASH_OTHER]);
        let out = derive_build_verify(&old, &my_exe(), "1.5.2", Some("c3f8cf5"));
        assert_eq!(out.status, BuildVerifyStatus::Unavailable);
        assert_eq!(out.my_payload_sha256, HASH_MINE);

        let old = release_report("v1.5.2", true, true, &[HASH_MINE]);
        let out = derive_build_verify(&old, &my_exe(), "1.5.2", None);
        assert_eq!(out.status, BuildVerifyStatus::Unavailable);
    }

    #[test]
    fn missing_comparable_layer_still_reports_real_alarms() {
        // The `Unavailable` gate covers only the leaf comparison.
        let broken = release_report("v1.5.2", true, false, &[HASH_MINE]);
        let out = derive_build_verify(&broken, &my_exe(), "1.5.2", None);
        assert_eq!(out.status, BuildVerifyStatus::Mismatch);

        let unpublished = release_report("v1.5.2", false, true, &[]);
        let out = derive_build_verify(&unpublished, &my_exe(), "1.5.2", None);
        assert_eq!(out.status, BuildVerifyStatus::Pending);
    }

    #[test]
    fn appimage_payload_path_is_unchanged_by_the_exe_layer() {
        let r = release_report_with_exe("v1.1.0", true, HASH_MINE, &[HASH_OTHER]);
        assert_eq!(
            derive_build_verify(&r, &mine(), "1.1.0", None).status,
            BuildVerifyStatus::Verified
        );
    }
}

#[cfg(test)]
mod key_set_client_tests {
    use super::*;

    const CONTEXT: &[u8] = b"pollis-verifiable-log:sth:v2";
    const NOW: u64 = 1_700_000_000_000;

    /// A real root-signed statement, not a stub.
    fn fixture(root_seed: u8, signer_seed: u8, stmt_expiry: Option<u64>) -> (String, String) {
        use ml_dsa::{Keypair, MlDsa44, SigningKey};
        let root = SigningKey::<MlDsa44>::from_seed(&[root_seed; 32].into());
        let signer = SigningKey::<MlDsa44>::from_seed(&[signer_seed; 32].into());
        let svk = signer.verifying_key();
        let entry = verifiable_log_serve::SignerEntry {
            key_id: verifiable_log_serve::key_id_for(&svk),
            public_key: hex::encode(svk.encode()),
            trees: vec![String::from_utf8_lossy(CONTEXT).to_string()],
            not_after_ms: None,
        };
        let st = verifiable_log_serve::KeySetStatement::create(&root, NOW, stmt_expiry, vec![entry])
            .expect("create");
        (
            serde_json::to_string(&st).expect("json"),
            hex::encode(root.verifying_key().encode()),
        )
    }

    fn pins(hex_key: &str, not_after: Option<u64>) -> Vec<PinnedRoot> {
        vec![PinnedRoot {
            key_id: "test",
            public_key: Box::leak(hex_key.to_string().into_boxed_str()),
            not_after,
        }]
    }

    #[test]
    fn a_statement_signed_by_the_pinned_root_yields_its_signers() {
        let (doc, root_hex) = fixture(1, 2, None);
        match signers_from_key_set(&doc, &pins(&root_hex, None), CONTEXT, NOW) {
            KeySetOutcome::Signers(keys) => assert_eq!(keys.len(), 1),
            other => panic!("expected signers, got {other:?}"),
        }
    }

    // The attack this stops: whoever holds the online signing seed authorising
    // themselves.
    #[test]
    fn a_statement_signed_by_anyone_else_is_untrusted() {
        let (doc, _) = fixture(1, 2, None);
        let (_, other_root) = fixture(9, 3, None);
        assert!(matches!(
            signers_from_key_set(&doc, &pins(&other_root, None), CONTEXT, NOW),
            KeySetOutcome::Untrusted(_)
        ));
    }

    // No root pinned has learned nothing; it must not accuse the server.
    #[test]
    fn a_build_with_no_root_pinned_withholds_trust_rather_than_alarming() {
        let (doc, _) = fixture(1, 2, None);
        assert_eq!(
            signers_from_key_set(&doc, &[], CONTEXT, NOW),
            KeySetOutcome::NoRootPinned
        );
    }

    #[test]
    fn an_expired_root_pin_is_treated_as_no_pin() {
        let (doc, root_hex) = fixture(1, 2, None);
        assert_eq!(
            signers_from_key_set(&doc, &pins(&root_hex, Some(NOW - 1)), CONTEXT, NOW + 1),
            KeySetOutcome::NoRootPinned
        );
    }

    // Anti-freeze: blocking fresh statements can't keep a compromised signer
    // authorised forever.
    #[test]
    fn an_expired_statement_authorises_nobody() {
        let (doc, root_hex) = fixture(1, 2, Some(NOW + 1000));
        assert!(matches!(
            signers_from_key_set(&doc, &pins(&root_hex, None), CONTEXT, NOW + 5000),
            KeySetOutcome::Untrusted(_)
        ));
    }

    #[test]
    fn a_signer_authorised_for_another_tree_does_not_count_for_this_one() {
        let (doc, root_hex) = fixture(1, 2, None);
        assert!(matches!(
            signers_from_key_set(&doc, &pins(&root_hex, None), b"pollis-verifiable-log:sth:v2:binaries", NOW),
            KeySetOutcome::Untrusted(_)
        ));
    }

    #[test]
    fn an_unreadable_document_is_untrusted_not_a_panic() {
        let (_, root_hex) = fixture(1, 2, None);
        assert!(matches!(
            signers_from_key_set("{not json", &pins(&root_hex, None), CONTEXT, NOW),
            KeySetOutcome::Untrusted(_)
        ));
    }

    #[test]
    fn tampering_with_a_served_signer_key_is_untrusted() {
        let (doc, root_hex) = fixture(1, 2, None);
        let mut v: serde_json::Value = serde_json::from_str(&doc).expect("json");
        v["signers"][0]["public_key"] = serde_json::json!("00".repeat(1312));
        assert!(matches!(
            signers_from_key_set(&v.to_string(), &pins(&root_hex, None), CONTEXT, NOW),
            KeySetOutcome::Untrusted(_)
        ));
    }
}

#[cfg(test)]
mod pinned_root_tests {
    use super::*;

    /// A typo in the pin would look like a hostile key set at runtime.
    #[test]
    fn every_pinned_root_is_a_wellformed_key() {
        for r in PINNED_LOG_ROOT_KEYS {
            assert!(
                verifiable_log_serve::root_key_from_hex(r.public_key).is_ok(),
                "pinned root {} is not a valid ML-DSA-44 key",
                r.key_id
            );
            assert_eq!(r.public_key, r.public_key.to_ascii_lowercase());
        }
    }

    #[test]
    fn each_pinned_root_key_id_matches_its_key() {
        for r in PINNED_LOG_ROOT_KEYS {
            let vk = verifiable_log_serve::root_key_from_hex(r.public_key).expect("valid key");
            assert_eq!(r.key_id, verifiable_log_serve::key_id_for(&vk));
        }
    }

    /// The committed statement must verify under a pinned root, or every log check
    /// degrades to "cannot verify".
    #[test]
    fn the_committed_key_set_verifies_under_a_pinned_root() {
        let doc = include_str!("../../../key-set.json");
        match signers_from_key_set(doc, PINNED_LOG_ROOT_KEYS, b"pollis-verifiable-log:sth:v2", 1_786_400_000_000) {
            KeySetOutcome::Signers(keys) => assert!(!keys.is_empty()),
            other => panic!("committed key set is not usable by this build: {other:?}"),
        }
    }
}
