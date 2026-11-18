//! App approval records (#2005): what an admin signs so an installation runs
//! exactly one App with exactly the grants the admin chose, and the pure checks
//! the runtime makes before every load.
//!
//! Two record types, never interchangeable:
//!
//! * [`AppApproval`] (production) pins one App hash and its manifest hash,
//!   signed over [`APP_APPROVAL_DOMAIN`].
//! * [`DevApproval`] (development, for `pollis app dev`) names a developer
//!   signing key instead of a hash, signed over [`APP_DEV_APPROVAL_DOMAIN`]. The
//!   runtime accepts any build signed by that key ([`DevBuildSignature`], over
//!   [`APP_DEV_BUILD_DOMAIN`]) whose manifest does not widen the approved one.
//!
//! Each is canonical JSON (RFC 8785, the App hash's own canonicalization) with
//! no unknown fields, signed by the approving admin's device ML-DSA-44 key, and
//! travels in the host approval's `{payload, signature}` wire shape. A record
//! of one type does not parse as the other (different required fields, no
//! unknown fields) and its signature does not verify under the other's domain.
//!
//! Who signed it and whether they are an admin is checked against the
//! verifier's own MLS view and roster (`signer.rs`, shared with #1182); that
//! needs an open installation, so it lives in `apps.rs`. Everything here is pure.

use base64::Engine as _;
use ml_dsa::{Keypair as _, MlDsa44, Signer as _, SigningKey};
use pollis_api::bot_apps::AppApprovalKind;
use pollis_app_manifest::{AdminGrants, Manifest};
use serde::{Deserialize, Serialize};

use super::approval::{self, ApprovalDecodeError};
use super::signer::SignerRefusal;

/// Domain separation for a production approval's signature.
pub const APP_APPROVAL_DOMAIN: &[u8] = b"pollis-app-approval-v1";
/// Domain separation for a development approval's signature.
pub const APP_DEV_APPROVAL_DOMAIN: &[u8] = b"pollis-app-dev-approval-v1";
/// Domain separation for a developer's signature on one build.
pub const APP_DEV_BUILD_DOMAIN: &[u8] = b"pollis-app-dev-build-v1";
/// The record format version.
pub const APP_APPROVAL_VERSION: u32 = 1;
/// The file a development build's signature is shipped in, beside the component
/// (`<component>.sig.json`).
pub const DEV_BUILD_SIGNATURE_SUFFIX: &str = ".sig.json";

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// A production approval: this installation may run the App whose hash is
/// `app_hash`, with `grants`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppApproval {
    /// The App hash, lowercase hex (`pollis_app_manifest::app_hash`).
    pub app_hash: String,
    pub approver_device_id: String,
    pub approver_user_id: String,
    /// Non-widening updates from `source` are approved by any admin's app
    /// without asking (each still gets its own signed record).
    pub auto_update: bool,
    pub bot_user_id: String,
    /// The admin's narrowing of the manifest. Never wider than it.
    pub grants: AdminGrants,
    pub group_id: String,
    /// The bot's user id while a bot has one installation.
    pub installation_id: String,
    /// Unix seconds.
    pub issued_at: i64,
    /// `pollis_app_manifest::manifest_hash` of the approved manifest, lowercase hex.
    pub manifest_hash: String,
    /// One more than the installation's previous approval.
    pub seq: i64,
    /// Where the runtime said it loads the artifact from, as given at setup.
    /// Shown to admins; the approval binds the hash, not this location.
    pub source: Option<String>,
    pub v: u32,
}

/// A development approval: this installation may run any build signed by
/// `developer_key` whose manifest does not widen the approved manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevApproval {
    pub approver_device_id: String,
    pub approver_user_id: String,
    pub bot_user_id: String,
    /// The developer's ML-DSA-44 public key, base64 (STANDARD).
    pub developer_key: String,
    pub grants: AdminGrants,
    pub group_id: String,
    pub installation_id: String,
    pub issued_at: i64,
    /// The manifest builds are held to: none may widen it.
    pub manifest_hash: String,
    pub seq: i64,
    pub v: u32,
}

/// Either record, as one stored row holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppApprovalRecord {
    Production(AppApproval),
    Development(DevApproval),
}

/// The fields both records share.
pub struct RecordCommon<'a> {
    pub installation_id: &'a str,
    pub bot_user_id: &'a str,
    pub group_id: &'a str,
    pub seq: i64,
    pub manifest_hash: &'a str,
    pub grants: &'a AdminGrants,
    pub approver_user_id: &'a str,
    pub approver_device_id: &'a str,
    pub issued_at: i64,
    pub v: u32,
}

fn canonical<T: Serialize>(value: &T) -> Vec<u8> {
    let v = serde_json::to_value(value).expect("an approval always serializes");
    pollis_app_manifest::app_hash::jcs(&v)
        .expect("an approval holds no floats")
        .into_bytes()
}

impl AppApprovalRecord {
    pub fn kind(&self) -> AppApprovalKind {
        match self {
            AppApprovalRecord::Production(_) => AppApprovalKind::Production,
            AppApprovalRecord::Development(_) => AppApprovalKind::Development,
        }
    }

    pub fn domain(&self) -> &'static [u8] {
        domain_of(self.kind())
    }

    /// The canonical bytes that are signed.
    pub fn canonical(&self) -> Vec<u8> {
        match self {
            AppApprovalRecord::Production(a) => canonical(a),
            AppApprovalRecord::Development(d) => canonical(d),
        }
    }

    pub fn common(&self) -> RecordCommon<'_> {
        match self {
            AppApprovalRecord::Production(a) => RecordCommon {
                installation_id: &a.installation_id,
                bot_user_id: &a.bot_user_id,
                group_id: &a.group_id,
                seq: a.seq,
                manifest_hash: &a.manifest_hash,
                grants: &a.grants,
                approver_user_id: &a.approver_user_id,
                approver_device_id: &a.approver_device_id,
                issued_at: a.issued_at,
                v: a.v,
            },
            AppApprovalRecord::Development(d) => RecordCommon {
                installation_id: &d.installation_id,
                bot_user_id: &d.bot_user_id,
                group_id: &d.group_id,
                seq: d.seq,
                manifest_hash: &d.manifest_hash,
                grants: &d.grants,
                approver_user_id: &d.approver_user_id,
                approver_device_id: &d.approver_device_id,
                issued_at: d.issued_at,
                v: d.v,
            },
        }
    }

    /// Sign with the admin's device key and encode for the DS.
    pub fn sign(&self, device_key: &SigningKey<MlDsa44>) -> String {
        approval::sign_payload(self.domain(), &self.canonical(), device_key)
    }

    /// Parse a stored record as the kind its row says, WITHOUT checking the
    /// signature. Returns the record, the signed payload and the signature.
    /// A record of the other kind is refused: it is never read as this one.
    pub fn decode(kind: AppApprovalKind, encoded: &str) -> Result<(Self, Vec<u8>, Vec<u8>), AppApprovalRefusal> {
        let (payload, signature) = approval::decode_wire(encoded).map_err(|e| match e {
            ApprovalDecodeError::Malformed(why) => AppApprovalRefusal::Malformed(why.to_string()),
            ApprovalDecodeError::BadSignature => AppApprovalRefusal::BadSignature,
        })?;
        let record = match kind {
            AppApprovalKind::Production => serde_json::from_slice::<AppApproval>(&payload)
                .map(AppApprovalRecord::Production)
                .map_err(|_| AppApprovalRefusal::WrongKind)?,
            AppApprovalKind::Development => serde_json::from_slice::<DevApproval>(&payload)
                .map(AppApprovalRecord::Development)
                .map_err(|_| AppApprovalRefusal::WrongKind)?,
        };
        if record.canonical() != payload {
            return Err(AppApprovalRefusal::Malformed("payload is not canonical".to_string()));
        }
        Ok((record, payload, signature))
    }
}

pub fn domain_of(kind: AppApprovalKind) -> &'static [u8] {
    match kind {
        AppApprovalKind::Production => APP_APPROVAL_DOMAIN,
        AppApprovalKind::Development => APP_DEV_APPROVAL_DOMAIN,
    }
}

/// Why an installation does not run. Every case is logged by the runtime and
/// reported to the group's admins; [`AppApprovalRefusal::awaits_approval`] says
/// which ones an admin resolves by approving the artifact the runtime has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppApprovalRefusal {
    /// No approval exists for this installation yet.
    NotApproved,
    /// Not a well-formed, canonical record of a supported version.
    Malformed(String),
    /// The record is not of the kind its row says (e.g. a development approval
    /// served as a production one).
    WrongKind,
    /// The record names another installation, bot or group, or another seq than its row.
    WrongInstallation,
    /// The approving device has no leaf in this verifier's view of the group.
    ApproverNotInGroup,
    /// The approving device is not certified by its account key.
    UncertifiedApprover(String),
    /// The signature does not verify under the approving device's key.
    BadSignature,
    /// The transparency log disagrees with the approver's account key.
    TransparencyAlarm,
    /// The approver is not a current admin of the group.
    NotAdmin(String),
    /// The manifest served beside the record does not hash to `manifest_hash`.
    ManifestMismatch,
    /// The grants ask for more than the manifest requested.
    WidenedGrants(String),
    /// The DS served an older approval than this installation already accepted.
    RolledBack { accepted: i64, served: i64 },
    /// The artifact is not the approved App (a first install, an update, or a swap).
    HashMismatch { approved: String, artifact: String },
    /// A development build signed by another key than the approved one, or not signed.
    DeveloperKeyMismatch,
    /// A development build whose signature does not verify.
    BadBuildSignature,
    /// A development build, or an update under `auto_update`, whose manifest widens the approved one.
    BuildWidens(String),
    /// An artifact this host already moved past under the same approval (an
    /// automatic update cannot be undone by serving the older file again).
    ArtifactRolledBack,
}

impl From<SignerRefusal> for AppApprovalRefusal {
    fn from(r: SignerRefusal) -> Self {
        match r {
            SignerRefusal::NotInGroup => AppApprovalRefusal::ApproverNotInGroup,
            SignerRefusal::Uncertified(why) => AppApprovalRefusal::UncertifiedApprover(why),
            SignerRefusal::BadSignature => AppApprovalRefusal::BadSignature,
            SignerRefusal::TransparencyAlarm => AppApprovalRefusal::TransparencyAlarm,
            SignerRefusal::NotAdmin(why) => AppApprovalRefusal::NotAdmin(why),
        }
    }
}

impl AppApprovalRefusal {
    /// The machine code reported to admins (the apps translate it).
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotApproved => "not_approved",
            Self::Malformed(_) => "malformed",
            Self::WrongKind => "wrong_kind",
            Self::WrongInstallation => "wrong_installation",
            Self::ApproverNotInGroup => "approver_not_in_group",
            Self::UncertifiedApprover(_) => "uncertified_approver",
            Self::BadSignature => "bad_signature",
            Self::TransparencyAlarm => "transparency_alarm",
            Self::NotAdmin(_) => "not_admin",
            Self::ManifestMismatch => "manifest_mismatch",
            Self::WidenedGrants(_) => "widened_grants",
            Self::RolledBack { .. } => "rolled_back",
            Self::HashMismatch { .. } => "hash_mismatch",
            Self::DeveloperKeyMismatch => "developer_key_mismatch",
            Self::BadBuildSignature => "bad_build_signature",
            Self::BuildWidens(_) => "build_widens",
            Self::ArtifactRolledBack => "artifact_rolled_back",
        }
    }

    /// An admin resolves this by approving the artifact the runtime has.
    pub fn awaits_approval(&self) -> bool {
        matches!(
            self,
            Self::NotApproved | Self::HashMismatch { .. } | Self::DeveloperKeyMismatch | Self::BuildWidens(_)
        )
    }
}

impl std::fmt::Display for AppApprovalRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotApproved => write!(f, "no admin has approved an App for this installation yet"),
            Self::Malformed(why) => write!(f, "malformed approval: {why}"),
            Self::WrongKind => write!(f, "the approval is not of the kind it is stored as"),
            Self::WrongInstallation => write!(f, "the approval is for another installation"),
            Self::ApproverNotInGroup => SignerRefusal::NotInGroup.fmt(f),
            Self::UncertifiedApprover(why) => SignerRefusal::Uncertified(why.clone()).fmt(f),
            Self::BadSignature => SignerRefusal::BadSignature.fmt(f),
            Self::TransparencyAlarm => SignerRefusal::TransparencyAlarm.fmt(f),
            Self::NotAdmin(why) => SignerRefusal::NotAdmin(why.clone()).fmt(f),
            Self::ManifestMismatch => write!(f, "the manifest served with the approval is not the approved one"),
            Self::WidenedGrants(why) => write!(f, "the approval grants more than the manifest asks for: {why}"),
            Self::RolledBack { accepted, served } => write!(
                f,
                "the server served approval {served}, older than approval {accepted} this host already accepted"
            ),
            Self::HashMismatch { approved, artifact } => {
                write!(f, "this artifact's App hash {artifact} is not the approved {approved}")
            }
            Self::DeveloperKeyMismatch => write!(f, "this build is not signed by the approved developer key"),
            Self::BadBuildSignature => write!(f, "this build's developer signature does not verify"),
            Self::BuildWidens(why) => write!(f, "this build's manifest asks for more than was approved: {why}"),
            Self::ArtifactRolledBack => {
                write!(f, "this artifact is older than one this host already ran under the same approval")
            }
        }
    }
}

/// An approval record that verified (signature, signer, admin, manifest,
/// grants, seq), ready to be held against an artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedAppApproval {
    pub record: AppApprovalRecord,
    /// The approved manifest's JSON text, as served beside the record.
    pub manifest_json: String,
    pub manifest: Manifest,
}

impl VerifiedAppApproval {
    pub fn seq(&self) -> i64 {
        self.record.common().seq
    }

    pub fn grants(&self) -> &AdminGrants {
        self.record.common().grants
    }
}

/// A developer's signature on one build: ML-DSA-44 over
/// `APP_DEV_BUILD_DOMAIN ‖ 0x00 ‖ app_hash` (the 32 raw bytes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevBuildSignature {
    /// base64 (STANDARD) ML-DSA-44 public key.
    pub developer_key: String,
    /// base64 (STANDARD).
    pub signature: String,
}

/// A developer key's public half, base64, from its 32-byte seed.
pub fn developer_key_public(seed: &[u8; 32]) -> String {
    let key = SigningKey::<MlDsa44>::from_seed(&(*seed).into());
    b64().encode(key.verifying_key().encode())
}

/// A fresh developer key seed.
pub fn new_developer_seed() -> [u8; 32] {
    let mut seed = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut seed);
    seed
}

/// Sign one build (its App hash) with a developer key.
pub fn sign_dev_build(seed: &[u8; 32], app_hash: &[u8; 32]) -> DevBuildSignature {
    let key = SigningKey::<MlDsa44>::from_seed(&(*seed).into());
    let sig = key.sign(&approval::signed_message_in(APP_DEV_BUILD_DOMAIN, app_hash)).encode().to_vec();
    DevBuildSignature { developer_key: developer_key_public(seed), signature: b64().encode(sig) }
}

/// What the runtime may load: the approved App and the grants it runs under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadableApp {
    pub app_hash: [u8; 32],
    pub manifest: Manifest,
    pub grants: AdminGrants,
    pub development: bool,
    /// A newer artifact from the approval's pinned source, accepted by the
    /// runtime under the record's `auto_update` because it widens nothing.
    pub auto_updated: bool,
}

/// The artifact the runtime is about to load.
pub struct Artifact<'a> {
    pub component: &'a [u8],
    pub manifest_json: &'a [u8],
    pub dev_signature: Option<&'a DevBuildSignature>,
}

/// Hold an artifact against a verified approval (run before every load).
///
/// Production: the artifact must hash to exactly the approved App hash (and
/// so its manifest to the approved manifest hash); grants are the record's.
/// Development: the build must be signed by the approved developer key and its
/// manifest must not widen the approved one; grants are the record's, carried
/// to the build's manifest (never wider).
pub fn check_artifact(approved: &VerifiedAppApproval, artifact: &Artifact<'_>) -> Result<LoadableApp, AppApprovalRefusal> {
    let malformed = |e: pollis_app_manifest::ManifestError| AppApprovalRefusal::Malformed(e.to_string());
    let app_hash = pollis_app_manifest::app_hash(artifact.component, artifact.manifest_json).map_err(malformed)?;
    let manifest = Manifest::from_json(artifact.manifest_json).map_err(malformed)?;
    match &approved.record {
        AppApprovalRecord::Production(a) => {
            let artifact_hex = pollis_app_manifest::hex(&app_hash);
            if !pollis_device_cert::constant_time_eq(artifact_hex.as_bytes(), a.app_hash.as_bytes()) {
                return Err(AppApprovalRefusal::HashMismatch { approved: a.app_hash.clone(), artifact: artifact_hex });
            }
            let manifest_hash = pollis_app_manifest::manifest_hash(artifact.manifest_json).map_err(malformed)?;
            if pollis_app_manifest::hex(&manifest_hash) != a.manifest_hash {
                return Err(AppApprovalRefusal::ManifestMismatch);
            }
            a.grants.check_not_wider(&manifest).map_err(|e| AppApprovalRefusal::WidenedGrants(e.to_string()))?;
            Ok(LoadableApp { app_hash, manifest, grants: a.grants.clone(), development: false, auto_updated: false })
        }
        AppApprovalRecord::Development(d) => {
            let Some(sig) = artifact.dev_signature else {
                return Err(AppApprovalRefusal::DeveloperKeyMismatch);
            };
            if sig.developer_key != d.developer_key {
                return Err(AppApprovalRefusal::DeveloperKeyMismatch);
            }
            let key = b64().decode(&sig.developer_key).map_err(|_| AppApprovalRefusal::BadBuildSignature)?;
            let signature = b64().decode(&sig.signature).map_err(|_| AppApprovalRefusal::BadBuildSignature)?;
            approval::verify_signature_in(APP_DEV_BUILD_DOMAIN, &key, &app_hash, &signature)
                .map_err(|_| AppApprovalRefusal::BadBuildSignature)?;
            refuse_widening(&approved.manifest, &manifest)?;
            Ok(LoadableApp { app_hash, grants: d.grants.carried_to(&manifest), manifest, development: true, auto_updated: false })
        }
    }
}

/// Refuse a manifest that asks for anything `approved` did not, naming each widening.
fn refuse_widening(approved: &Manifest, manifest: &Manifest) -> Result<(), AppApprovalRefusal> {
    let widens = pollis_app_manifest::widenings_between(approved, manifest);
    if widens.is_empty() {
        return Ok(());
    }
    let why = widens
        .iter()
        .map(|c| format!("{:?} {}", c.field, c.value).to_lowercase())
        .collect::<Vec<_>>()
        .join(", ");
    Err(AppApprovalRefusal::BuildWidens(why))
}

/// [`check_artifact`], plus the record's standing permission for updates: a
/// production approval with `auto_update` lets the runtime itself run a newer
/// artifact from exactly the pinned `source` (the location the runtime loads
/// from, compared as given) whose manifest widens nothing against the approved
/// one, with the approved grants carried to it. No admin needs to be online;
/// the admin's signed record already granted it. A widening update, or any
/// other location, still waits for a new approval. The caller must also pass
/// the result through the installation's accepted-hash ledger
/// (`apps::accept_app_hash`), which refuses going back to a superseded hash.
pub fn check_artifact_from(
    approved: &VerifiedAppApproval,
    artifact: &Artifact<'_>,
    runtime_source: &str,
) -> Result<LoadableApp, AppApprovalRefusal> {
    let refusal = match check_artifact(approved, artifact) {
        Ok(app) => return Ok(app),
        Err(r @ AppApprovalRefusal::HashMismatch { .. }) => r,
        Err(other) => return Err(other),
    };
    let AppApprovalRecord::Production(a) = &approved.record else {
        return Err(refusal);
    };
    if !a.auto_update || a.source.as_deref() != Some(runtime_source) {
        return Err(refusal);
    }
    let malformed = |e: pollis_app_manifest::ManifestError| AppApprovalRefusal::Malformed(e.to_string());
    let manifest = Manifest::from_json(artifact.manifest_json).map_err(malformed)?;
    refuse_widening(&approved.manifest, &manifest)?;
    let app_hash = pollis_app_manifest::app_hash(artifact.component, artifact.manifest_json).map_err(malformed)?;
    Ok(LoadableApp { app_hash, grants: a.grants.carried_to(&manifest), manifest, development: false, auto_updated: true })
}

/// The checks on a decoded record that need no network: version, that it is
/// for this installation and row, that the served manifest is the approved one,
/// and that the grants are not wider than it. The signature and the signer are
/// checked by the caller (`apps.rs`), from its own MLS view and roster.
pub fn check_record(
    record: &AppApprovalRecord,
    row_seq: i64,
    bot_user_id: &str,
    group_id: &str,
    manifest_json: &str,
) -> Result<Manifest, AppApprovalRefusal> {
    let c = record.common();
    if c.v != APP_APPROVAL_VERSION {
        return Err(AppApprovalRefusal::Malformed(format!("unsupported version {}", c.v)));
    }
    if c.installation_id != bot_user_id || c.bot_user_id != bot_user_id || c.group_id != group_id || c.seq != row_seq {
        return Err(AppApprovalRefusal::WrongInstallation);
    }
    let served = pollis_app_manifest::manifest_hash(manifest_json.as_bytes())
        .map_err(|e| AppApprovalRefusal::Malformed(e.to_string()))?;
    if pollis_app_manifest::hex(&served) != c.manifest_hash {
        return Err(AppApprovalRefusal::ManifestMismatch);
    }
    let manifest =
        Manifest::from_json(manifest_json.as_bytes()).map_err(|e| AppApprovalRefusal::Malformed(e.to_string()))?;
    c.grants.check_not_wider(&manifest).map_err(|e| AppApprovalRefusal::WidenedGrants(e.to_string()))?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pollis_app_manifest::{ActionKind, ChannelGrant, EventKind};
    use std::collections::BTreeSet;

    const MANIFEST: &str = r##"{"name":"triage","version":"1.0.0","triggers":["messages"],
        "read":{"channels":["#support"],"history":30},"actions":["reply"],"egress":["api.linear.app"]}"##;

    fn grants() -> AdminGrants {
        AdminGrants {
            channels: vec![ChannelGrant { id: "c1".into(), name: "support".into() }],
            history: 7,
            events: BTreeSet::from([EventKind::Created]),
            actions: BTreeSet::from([ActionKind::Reply]),
            egress: BTreeSet::new(),
        }
    }

    fn component() -> Vec<u8> {
        b"\0asm\x0d\0\x01\0 the app".to_vec()
    }

    fn production() -> AppApproval {
        AppApproval {
            app_hash: pollis_app_manifest::hex(&pollis_app_manifest::app_hash(&component(), MANIFEST.as_bytes()).unwrap()),
            approver_device_id: "DEV".into(),
            approver_user_id: "ADMIN".into(),
            auto_update: false,
            bot_user_id: "BOT".into(),
            grants: grants(),
            group_id: "GROUP".into(),
            installation_id: "BOT".into(),
            issued_at: 1_700_000_000,
            manifest_hash: pollis_app_manifest::hex(&pollis_app_manifest::manifest_hash(MANIFEST.as_bytes()).unwrap()),
            seq: 1,
            source: Some("https://apps.example/triage.wasm".into()),
            v: APP_APPROVAL_VERSION,
        }
    }

    fn development(dev_seed: &[u8; 32]) -> DevApproval {
        let p = production();
        DevApproval {
            approver_device_id: p.approver_device_id,
            approver_user_id: p.approver_user_id,
            bot_user_id: p.bot_user_id,
            developer_key: developer_key_public(dev_seed),
            grants: p.grants,
            group_id: p.group_id,
            installation_id: p.installation_id,
            issued_at: p.issued_at,
            manifest_hash: p.manifest_hash,
            seq: 1,
            v: APP_APPROVAL_VERSION,
        }
    }

    fn admin_key() -> SigningKey<MlDsa44> {
        SigningKey::<MlDsa44>::from_seed(&[1u8; 32].into())
    }

    fn verified(record: AppApprovalRecord) -> VerifiedAppApproval {
        VerifiedAppApproval { record, manifest_json: MANIFEST.into(), manifest: Manifest::from_json(MANIFEST.as_bytes()).unwrap() }
    }

    #[test]
    fn a_production_record_round_trips_and_verifies_under_its_own_domain_only() {
        let record = AppApprovalRecord::Production(production());
        let encoded = record.sign(&admin_key());
        let (got, payload, sig) = AppApprovalRecord::decode(AppApprovalKind::Production, &encoded).unwrap();
        assert_eq!(got, record);
        let vk = admin_key().verifying_key().encode();
        approval::verify_signature_in(APP_APPROVAL_DOMAIN, &vk, &payload, &sig).unwrap();
        assert!(approval::verify_signature_in(APP_DEV_APPROVAL_DOMAIN, &vk, &payload, &sig).is_err());
        assert!(approval::verify_signature(&vk, &payload, &sig).is_err(), "not a host approval either");
    }

    #[test]
    fn a_development_approval_can_never_be_read_as_production_or_back() {
        let dev = AppApprovalRecord::Development(development(&[9u8; 32])).sign(&admin_key());
        assert_eq!(
            AppApprovalRecord::decode(AppApprovalKind::Production, &dev).unwrap_err(),
            AppApprovalRefusal::WrongKind
        );
        let prod = AppApprovalRecord::Production(production()).sign(&admin_key());
        assert_eq!(
            AppApprovalRecord::decode(AppApprovalKind::Development, &prod).unwrap_err(),
            AppApprovalRefusal::WrongKind
        );
    }

    #[test]
    fn a_non_canonical_record_is_refused() {
        let pretty = serde_json::to_vec_pretty(&production()).unwrap();
        let encoded = approval::sign_payload(APP_APPROVAL_DOMAIN, &pretty, &admin_key());
        assert!(matches!(
            AppApprovalRecord::decode(AppApprovalKind::Production, &encoded),
            Err(AppApprovalRefusal::Malformed(_))
        ));
        // A missing optional field is not canonical either (it re-serializes as null).
        let mut v = serde_json::to_value(production()).unwrap();
        v.as_object_mut().unwrap().remove("source");
        let bytes = pollis_app_manifest::app_hash::jcs(&v).unwrap().into_bytes();
        let encoded = approval::sign_payload(APP_APPROVAL_DOMAIN, &bytes, &admin_key());
        assert!(matches!(
            AppApprovalRecord::decode(AppApprovalKind::Production, &encoded),
            Err(AppApprovalRefusal::Malformed(_))
        ));
    }

    #[test]
    fn the_exact_artifact_loads_with_the_records_grants() {
        let v = verified(AppApprovalRecord::Production(production()));
        let app = check_artifact(&v, &Artifact { component: &component(), manifest_json: MANIFEST.as_bytes(), dev_signature: None })
            .unwrap();
        assert_eq!(app.grants, grants());
        assert!(!app.development);
        // Reformatted manifest text: same hash, still loads.
        let reformatted = serde_json::to_vec_pretty(&serde_json::from_str::<serde_json::Value>(MANIFEST).unwrap()).unwrap();
        check_artifact(&v, &Artifact { component: &component(), manifest_json: &reformatted, dev_signature: None }).unwrap();
    }

    #[test]
    fn one_changed_byte_is_a_hash_mismatch_never_close_enough() {
        let v = verified(AppApprovalRecord::Production(production()));
        let mut other = component();
        *other.last_mut().unwrap() ^= 1;
        let err =
            check_artifact(&v, &Artifact { component: &other, manifest_json: MANIFEST.as_bytes(), dev_signature: None })
                .unwrap_err();
        assert!(matches!(err, AppApprovalRefusal::HashMismatch { .. }));
        assert!(err.awaits_approval());
        let widened = MANIFEST.replace("\"reply\"", "\"reply\",\"post\"");
        assert!(matches!(
            check_artifact(&v, &Artifact { component: &component(), manifest_json: widened.as_bytes(), dev_signature: None }),
            Err(AppApprovalRefusal::HashMismatch { .. })
        ));
    }

    #[test]
    fn record_checks_refuse_another_installation_a_swapped_manifest_and_widened_grants() {
        let p = production();
        let rec = AppApprovalRecord::Production(p.clone());
        assert!(check_record(&rec, 1, "BOT", "GROUP", MANIFEST).is_ok());
        assert_eq!(check_record(&rec, 2, "BOT", "GROUP", MANIFEST), Err(AppApprovalRefusal::WrongInstallation));
        assert_eq!(check_record(&rec, 1, "OTHER", "GROUP", MANIFEST), Err(AppApprovalRefusal::WrongInstallation));
        assert_eq!(check_record(&rec, 1, "BOT", "ELSEWHERE", MANIFEST), Err(AppApprovalRefusal::WrongInstallation));
        let swapped = MANIFEST.replace("api.linear.app", "evil.example");
        assert_eq!(check_record(&rec, 1, "BOT", "GROUP", &swapped), Err(AppApprovalRefusal::ManifestMismatch));
        let mut wide = p;
        wide.grants.egress.insert("evil.example".into());
        assert!(matches!(
            check_record(&AppApprovalRecord::Production(wide), 1, "BOT", "GROUP", MANIFEST),
            Err(AppApprovalRefusal::WidenedGrants(_))
        ));
    }

    #[test]
    fn auto_update_takes_a_non_widening_artifact_from_the_pinned_source_only() {
        let mut p = production();
        p.auto_update = true;
        let v = verified(AppApprovalRecord::Production(p));
        let source = "https://apps.example/triage.wasm";
        let newer = b"\0asm\x0d\0\x01\0 v2".to_vec();
        let narrower = MANIFEST.replace("\"history\":30", "\"history\":3");
        let app = check_artifact_from(&v, &Artifact { component: &newer, manifest_json: narrower.as_bytes(), dev_signature: None }, source)
            .expect("a non-widening update from the pinned source runs");
        assert!(app.auto_updated);
        assert_eq!(app.grants.history, 3);
        // Another location: waits for an admin.
        assert!(matches!(
            check_artifact_from(&v, &Artifact { component: &newer, manifest_json: MANIFEST.as_bytes(), dev_signature: None }, "/opt/elsewhere/app.wasm"),
            Err(AppApprovalRefusal::HashMismatch { .. })
        ));
        // Widening: waits for an admin.
        let wider = MANIFEST.replace("[\"reply\"]", "[\"reply\",\"post\"]");
        assert!(matches!(
            check_artifact_from(&v, &Artifact { component: &newer, manifest_json: wider.as_bytes(), dev_signature: None }, source),
            Err(AppApprovalRefusal::BuildWidens(_))
        ));
        // Without auto_update: waits for an admin.
        let v = verified(AppApprovalRecord::Production(production()));
        assert!(matches!(
            check_artifact_from(&v, &Artifact { component: &newer, manifest_json: MANIFEST.as_bytes(), dev_signature: None }, source),
            Err(AppApprovalRefusal::HashMismatch { .. })
        ));
    }

    #[test]
    fn a_development_installation_runs_any_build_its_key_signs_that_does_not_widen() {
        let dev_seed = [9u8; 32];
        let v = verified(AppApprovalRecord::Development(development(&dev_seed)));
        let build = b"\0asm\x0d\0\x01\0 build 42".to_vec();
        let hash = pollis_app_manifest::app_hash(&build, MANIFEST.as_bytes()).unwrap();
        let sig = sign_dev_build(&dev_seed, &hash);
        let app = check_artifact(&v, &Artifact { component: &build, manifest_json: MANIFEST.as_bytes(), dev_signature: Some(&sig) })
            .unwrap();
        assert!(app.development);

        // Unsigned, signed by someone else, or a signature over another build.
        assert_eq!(
            check_artifact(&v, &Artifact { component: &build, manifest_json: MANIFEST.as_bytes(), dev_signature: None }),
            Err(AppApprovalRefusal::DeveloperKeyMismatch)
        );
        let stranger = sign_dev_build(&[8u8; 32], &hash);
        assert_eq!(
            check_artifact(&v, &Artifact { component: &build, manifest_json: MANIFEST.as_bytes(), dev_signature: Some(&stranger) }),
            Err(AppApprovalRefusal::DeveloperKeyMismatch)
        );
        let other_build = sign_dev_build(&dev_seed, &[0u8; 32]);
        assert_eq!(
            check_artifact(&v, &Artifact { component: &build, manifest_json: MANIFEST.as_bytes(), dev_signature: Some(&other_build) }),
            Err(AppApprovalRefusal::BadBuildSignature)
        );

        // A build that asks for a new egress host is refused, even when signed.
        let wider = MANIFEST.replace("[\"api.linear.app\"]", "[\"api.linear.app\",\"evil.example\"]");
        let hash = pollis_app_manifest::app_hash(&build, wider.as_bytes()).unwrap();
        let sig = sign_dev_build(&dev_seed, &hash);
        assert!(matches!(
            check_artifact(&v, &Artifact { component: &build, manifest_json: wider.as_bytes(), dev_signature: Some(&sig) }),
            Err(AppApprovalRefusal::BuildWidens(_))
        ));

        // A narrower build runs with the grants carried to it.
        let narrower = MANIFEST.replace("\"history\":30", "\"history\":3");
        let hash = pollis_app_manifest::app_hash(&build, narrower.as_bytes()).unwrap();
        let sig = sign_dev_build(&dev_seed, &hash);
        let app = check_artifact(&v, &Artifact { component: &build, manifest_json: narrower.as_bytes(), dev_signature: Some(&sig) })
            .unwrap();
        assert_eq!(app.grants.history, 3);
    }
}
