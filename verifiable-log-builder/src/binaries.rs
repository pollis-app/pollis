//! The binaries tenant: the leaf encoding for one released artifact
//! ([`BinaryRecord`]) and [`BinaryInvariant`]. Its own tree, signed under
//! [`STH_CONTEXT`] (`docs/verifiable-builds-design.md` §2). Pure: no IO, DB or
//! clock.

use serde::{Deserialize, Serialize};
use verifiable_log::{Entry, InvariantViolation, TenantInvariant};

pub const TENANT: &str = "binaries";

/// The binaries tree's STH context, re-exported from the core so it can't drift.
pub use verifiable_log::STH_CONTEXT_BINARIES as STH_CONTEXT;

/// Which layer of a shipped file a [`BinaryRecord`] commits to.
///
/// Signed artifacts embed non-reproducible signatures, so they are logged as a
/// derived [`Self::Signed`] leaf bound to a reproducible [`Self::Payload`] leaf
/// via `payload_sha256`. A closed enum, so an unknown layer fails to parse.
/// Adding a variant leaves existing leaf bytes untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    /// The reproducible pre-signature payload (`.app` contents, AppImage
    /// squashfs, unsigned exe+resources). `artifact_sha256 == payload_sha256`.
    Payload,
    /// The shipped, signed/notarized bytes.
    Signed,
    /// The installed main executable, hashed on its own (e.g.
    /// `Contents/MacOS/pollis`). Exists so a running app can verify itself: it
    /// has no preimage of the payload, but can hash the file it is executing.
    Exe,
}

/// The toolchain a rebuilder installs to reproduce `payload_sha256`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Toolchain {
    pub rustc: String,
    pub node: String,
    pub pnpm: String,
    /// Ideally digest-pinned, e.g. `"macos-14@sha256:…"`.
    pub runner_image: String,
    /// The release tag's commit timestamp, in Unix seconds.
    pub source_date_epoch: u64,
}

/// Frozen leaf for one released artifact (per platform × arch × bundle ×
/// layer): hashes plus the reproducibility recipe, never the bytes.
///
/// Encoded as compact JSON in declared field order (design doc §2.2); do not
/// reorder fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinaryRecord {
    pub release_tag: String,
    /// 40-hex git sha.
    pub commit: String,
    /// `darwin` | `windows` | `linux`.
    pub platform: String,
    /// `aarch64` | `x86_64`.
    pub arch: String,
    /// `dmg` | `app` | `nsis` | `appimage` | `deb` | `rpm`.
    pub bundle: String,
    pub artifact_name: String,
    pub layer: Layer,
    pub payload_sha256: String,
    /// `== payload_sha256` when `layer == payload`.
    pub artifact_sha256: String,
    pub toolchain: Toolchain,
    /// SLSA/in-toto provenance attestation URI.
    pub provenance_uri: String,
}

impl BinaryRecord {
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    pub fn to_entry(&self) -> Result<Entry, serde_json::Error> {
        Ok(Entry::new(TENANT, self.encode()?))
    }

    /// Leaves sharing this tuple are the same released unit and must agree on
    /// `artifact_sha256`.
    fn fork_key(&self) -> (&str, &str, &str, &str, Layer) {
        (
            &self.release_tag,
            &self.platform,
            &self.arch,
            &self.bundle,
            self.layer,
        )
    }
}

/// Binary transparency's per-release rules (design doc §2.3):
///
/// * **(a) no silent re-issue**: equal [`BinaryRecord::fork_key`] with a
///   different `artifact_sha256` is a fork; a re-release needs a new tag.
/// * **(b) monotonic releases**: once a newer tag appears, an earlier one never
///   reappears. (No git-ancestry check: an offline verifier has no git graph.)
/// * **(c) derived-layer pairing**: every non-`payload` leaf needs an earlier
///   `payload` leaf with equal `payload_sha256`. Stated over "not payload" so a
///   new layer inherits it.
pub struct BinaryInvariant;

impl BinaryInvariant {
    /// An unparseable leaf is a violation, never silently accepted.
    fn parse(entry: &Entry) -> Result<BinaryRecord, InvariantViolation> {
        BinaryRecord::decode(&entry.data)
            .map_err(|e| InvariantViolation::new(TENANT, format!("malformed binary leaf: {e}")))
    }
}

impl TenantInvariant for BinaryInvariant {
    fn check(&self, existing: &[&Entry], candidate: &Entry) -> Result<(), InvariantViolation> {
        let cand = Self::parse(candidate)?;

        let mut cand_tag_seen = false;
        let mut last_tag: Option<String> = None;
        let mut payload_seen = false;

        for prev_entry in existing {
            let prev = Self::parse(prev_entry)?;

            // (a)
            if prev.fork_key() == cand.fork_key() && prev.artifact_sha256 != cand.artifact_sha256 {
                return Err(InvariantViolation::new(
                    TENANT,
                    format!(
                        "fork for {}/{}/{}/{} ({:?}): artifact_sha256 {} conflicts with earlier {}",
                        cand.release_tag,
                        cand.platform,
                        cand.arch,
                        cand.bundle,
                        cand.layer,
                        cand.artifact_sha256,
                        prev.artifact_sha256,
                    ),
                ));
            }

            if prev.release_tag == cand.release_tag {
                cand_tag_seen = true;
            }
            if prev.layer == Layer::Payload && prev.payload_sha256 == cand.payload_sha256 {
                payload_seen = true;
            }
            // `existing` is in publish order, so this ends as the tree's current
            // tag. Moved, not cloned, to avoid an allocation per prior leaf.
            last_tag = Some(prev.release_tag);
        }

        // (b)
        if cand_tag_seen && last_tag.as_deref().is_some_and(|t| t != cand.release_tag) {
            return Err(InvariantViolation::new(
                TENANT,
                format!(
                    "release_tag `{}` reappears out of publish order (current tag is `{}`)",
                    cand.release_tag,
                    last_tag.as_deref().unwrap_or_default(),
                ),
            ));
        }

        // (c)
        if cand.layer != Layer::Payload && !payload_seen {
            return Err(InvariantViolation::new(
                TENANT,
                format!(
                    "{:?} artifact `{}` ({}) has no matching payload leaf with payload_sha256 {}",
                    cand.layer, cand.artifact_name, cand.release_tag, cand.payload_sha256,
                ),
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toolchain() -> Toolchain {
        Toolchain {
            rustc: "1.83.0".to_string(),
            node: "20.11.1".to_string(),
            pnpm: "9.1.0".to_string(),
            runner_image: "ubuntu-24.04@sha256:abc".to_string(),
            source_date_epoch: 1_700_000_000,
        }
    }

    fn record(tag: &str, platform: &str, bundle: &str, layer: Layer, payload: u8, artifact: u8) -> BinaryRecord {
        BinaryRecord {
            release_tag: tag.to_string(),
            commit: "f".repeat(40),
            platform: platform.to_string(),
            arch: "aarch64".to_string(),
            bundle: bundle.to_string(),
            artifact_name: format!("pollis-{tag}-{platform}.{bundle}"),
            layer,
            payload_sha256: hex::encode([payload; 32]),
            artifact_sha256: hex::encode([artifact; 32]),
            toolchain: toolchain(),
            provenance_uri: format!("cdn.pollis.com/releases/{tag}/{platform}.intoto.jsonl"),
        }
    }

    #[test]
    fn encode_is_stable_and_roundtrips() {
        let r = record("v1.0.0", "darwin", "dmg", Layer::Payload, 0x11, 0x11);
        let bytes = r.encode().unwrap();
        let s = String::from_utf8(bytes.clone()).unwrap();
        assert!(s.starts_with("{\"release_tag\":"));
        assert!(s.find("\"release_tag\":").unwrap() < s.find("\"commit\":").unwrap());
        assert!(s.find("\"layer\":").unwrap() < s.find("\"payload_sha256\":").unwrap());
        assert!(s.find("\"toolchain\":").unwrap() < s.find("\"provenance_uri\":").unwrap());
        assert!(s.find("\"rustc\":").unwrap() < s.find("\"source_date_epoch\":").unwrap());
        assert!(s.contains("\"layer\":\"payload\""));
        assert_eq!(BinaryRecord::decode(&bytes).unwrap(), r);
    }

    #[test]
    fn accepts_payload_then_signed_pair_and_multiple_platforms() {
        let inv = BinaryInvariant;
        let mac_payload = record("v1.0.0", "darwin", "dmg", Layer::Payload, 0x11, 0x11).to_entry().unwrap();
        let mac_signed = record("v1.0.0", "darwin", "dmg", Layer::Signed, 0x11, 0x22).to_entry().unwrap();
        let lin_payload = record("v1.0.0", "linux", "appimage", Layer::Payload, 0x33, 0x33).to_entry().unwrap();

        assert!(inv.check(&[], &mac_payload).is_ok());
        assert!(inv.check(&[&mac_payload], &mac_signed).is_ok());
        assert!(inv.check(&[&mac_payload, &mac_signed], &lin_payload).is_ok());
    }

    #[test]
    fn accepts_new_tag_after_prior_tag() {
        let inv = BinaryInvariant;
        let v1 = record("v1.0.0", "linux", "appimage", Layer::Payload, 0x11, 0x11).to_entry().unwrap();
        let v2 = record("v1.1.0", "linux", "appimage", Layer::Payload, 0x22, 0x22).to_entry().unwrap();
        assert!(inv.check(&[&v1], &v2).is_ok());
    }

    #[test]
    fn rejects_fork_same_tuple_different_hash() {
        let inv = BinaryInvariant;
        let a = record("v1.0.0", "darwin", "dmg", Layer::Payload, 0x11, 0x11).to_entry().unwrap();
        let forked = record("v1.0.0", "darwin", "dmg", Layer::Payload, 0x99, 0x99).to_entry().unwrap();
        let err = inv.check(&[&a], &forked).unwrap_err();
        assert!(err.message.contains("fork"), "got: {}", err.message);
    }

    #[test]
    fn rejects_signed_without_payload() {
        let inv = BinaryInvariant;
        let signed = record("v1.0.0", "windows", "nsis", Layer::Signed, 0x44, 0x55).to_entry().unwrap();
        let err = inv.check(&[], &signed).unwrap_err();
        assert!(err.message.contains("no matching payload"), "got: {}", err.message);
    }

    #[test]
    fn rejects_exe_without_payload() {
        let inv = BinaryInvariant;
        let exe = record("v1.0.0", "darwin", "dmg", Layer::Exe, 0x44, 0x55).to_entry().unwrap();
        let err = inv.check(&[], &exe).unwrap_err();
        assert!(err.message.contains("no matching payload"), "got: {}", err.message);
    }

    #[test]
    fn accepts_exe_leaf_bound_to_its_payload() {
        let inv = BinaryInvariant;
        let payload = record("v1.0.0", "darwin", "dmg", Layer::Payload, 0x11, 0x11).to_entry().unwrap();
        let signed = record("v1.0.0", "darwin", "dmg", Layer::Signed, 0x11, 0x22).to_entry().unwrap();
        let exe = record("v1.0.0", "darwin", "dmg", Layer::Exe, 0x11, 0x33).to_entry().unwrap();
        assert!(inv.check(&[&payload, &signed], &exe).is_ok());
        // `exe` is its own fork_key slot, so it doesn't collide with `signed`.
        assert!(inv.check(&[&payload, &exe], &signed).is_ok());
    }

    #[test]
    fn exe_layer_serialises_as_exe_and_roundtrips() {
        let r = record("v1.0.0", "windows", "nsis", Layer::Exe, 0x11, 0x33);
        let bytes = r.encode().unwrap();
        let s = String::from_utf8(bytes.clone()).unwrap();
        assert!(s.contains("\"layer\":\"exe\""), "got: {s}");
        assert_eq!(BinaryRecord::decode(&bytes).unwrap(), r);
    }

    #[test]
    fn pre_exe_leaf_bytes_still_decode_unchanged() {
        let r = record("v1.0.0", "darwin", "dmg", Layer::Payload, 0x11, 0x11);
        let bytes = r.encode().unwrap();
        assert_eq!(BinaryRecord::decode(&bytes).unwrap(), r);
        assert_eq!(BinaryRecord::decode(&bytes).unwrap().encode().unwrap(), bytes);
    }

    #[test]
    fn rejects_tag_out_of_publish_order() {
        let inv = BinaryInvariant;
        let v1 = record("v1.0.0", "linux", "appimage", Layer::Payload, 0x11, 0x11).to_entry().unwrap();
        let v2 = record("v1.1.0", "linux", "appimage", Layer::Payload, 0x22, 0x22).to_entry().unwrap();
        let back = record("v1.0.0", "darwin", "dmg", Layer::Payload, 0x33, 0x33).to_entry().unwrap();
        let err = inv.check(&[&v1, &v2], &back).unwrap_err();
        assert!(err.message.contains("out of publish order"), "got: {}", err.message);
    }

    #[test]
    fn malformed_leaf_is_a_violation() {
        let inv = BinaryInvariant;
        let bad = Entry::new(TENANT, b"not json".to_vec());
        assert!(inv.check(&[], &bad).is_err());
    }
}
