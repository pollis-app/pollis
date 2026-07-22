//! The **binaries** tenant: the canonical leaf encoding for one released build
//! artifact ([`BinaryRecord`]) and the [`BinaryInvariant`] that makes binary
//! transparency's per-release rules globally auditable.
//!
//! This is the third tenant of the transparency log (the first is
//! [`crate::commit_log`], the second [`crate::account_key`]). Per the verifiable
//! -builds design (`docs/verifiable-builds-design.md` §2) it gets its **own**
//! Merkle tree and its **own** STH — binary entries are never interleaved into
//! the commit-log or account-key trees — and that tree's STHs are signed under a
//! domain-separated context ([`STH_CONTEXT`]) so a binaries head can never be
//! presented as a commit-log or account-key head even though the same Ed25519
//! key signs all three.
//!
//! Like [`crate::commit_log`] / [`crate::account_key`] this module is pure — no
//! IO, no DB, no clock. Phase 1 reads [`BinaryRecord`]s from a JSON file (see the
//! builder binary's `build-binaries` mode); everything here operates on
//! already-read values so the encoding and invariant can be unit-tested and
//! reused (e.g. by `pollis-verify`) without any source of truth attached.

use serde::{Deserialize, Serialize};
use verifiable_log::{Entry, InvariantViolation, TenantInvariant};

/// Tenant id for the released-binaries tree in the shared verifiable log.
pub const TENANT: &str = "binaries";

/// Domain-separation context for the binaries tree's Signed Tree Heads.
///
/// It extends the commit-log's frozen `pollis-verifiable-log:sth:v1` with a
/// `:binaries` suffix, exactly as the account-key tree uses `:account-keys`. The
/// commit-log and account-key contexts must NOT change (continuity of
/// already-published STHs); this distinct context guarantees an STH signed for
/// one tree fails verification against the others even though all three use the
/// same Ed25519 key. Verified via [`verifiable_log::Sth::verify_with_context`].
pub const STH_CONTEXT: &[u8] = b"pollis-verifiable-log:sth:v1:binaries";

/// Which layer of a shipped file a [`BinaryRecord`] commits to.
///
/// A signed artifact (`.dmg` / `.exe` / updater bundle) is inherently
/// non-reproducible (embedded notarization / Authenticode / minisign
/// signatures), so it is logged as a *derived* [`Self::Signed`] leaf bound to a
/// reproducible [`Self::Payload`] leaf via `payload_sha256`. Modelling this as a
/// closed enum (rather than a free string) makes an invalid `layer` an
/// unrepresentable state: decoding a leaf with any other value is a parse error,
/// which the invariant treats as a violation.
///
/// Adding a variant is backward-compatible in both directions that matter: old
/// leaves carry `payload`/`signed` and still decode, and their canonical bytes
/// (and therefore every published inclusion proof) are untouched, because the
/// variant list is not part of any leaf's encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    /// The reproducible pre-signature payload (`.app` contents, AppImage
    /// squashfs, unsigned exe+resources). `artifact_sha256 == payload_sha256`.
    Payload,
    /// The shipped, signed/notarized bytes — a derived wrapper around a
    /// `Payload` leaf sharing the same `payload_sha256`.
    Signed,
    /// The **main executable as installed**, hashed on its own:
    /// `Contents/MacOS/pollis` inside the `.app`, `pollis.exe` inside the NSIS
    /// tree, `usr/bin/pollis` inside the AppImage/deb/rpm. `artifact_sha256` is
    /// that file's sha256; `payload_sha256` binds it to the enclosing
    /// [`Self::Payload`] leaf.
    ///
    /// This layer exists so a *running* app can verify itself. The `payload`
    /// leaf is the rebuilder's unit — a `sha_tree` of an extracted directory or
    /// the installer file — and an installed process has neither preimage, so
    /// it can never match one. It CAN hash the one file it is executing, which
    /// is also the precise claim the in-app check makes: that these running
    /// bytes are bytes Pollis published.
    Exe,
}

/// The reproducibility recipe pinned into each leaf. A rebuilder installs these
/// exact toolchain versions to reproduce `payload_sha256` byte-for-byte. Encoded
/// in serde declaration order as a nested object (see [`BinaryRecord::encode`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Toolchain {
    /// Exact rustc version, e.g. `"1.83.0"`.
    pub rustc: String,
    /// Exact Node version, e.g. `"20.11.1"`.
    pub node: String,
    /// Exact pnpm version, e.g. `"9.1.0"`.
    pub pnpm: String,
    /// CI runner image identity, ideally digest-pinned, e.g. `"macos-14@sha256:…"`.
    pub runner_image: String,
    /// `SOURCE_DATE_EPOCH` (Unix seconds) used for the build — the release tag's
    /// commit timestamp, independently recoverable from `git`.
    pub source_date_epoch: u64,
}

/// The canonical, frozen leaf payload committing to a single released artifact
/// (one per platform × arch × bundle × layer): a content hash plus the full
/// reproducibility recipe, **never** the binary bytes — the same "hash and drop
/// the blob" discipline as the commit-log leaf.
///
/// The on-the-wire leaf encoding is **compact JSON of this struct with fields in
/// exactly this declared order** (matching `docs/verifiable-builds-design.md`
/// §2.2). serde emits struct fields in declaration order with no insignificant
/// whitespace, so the encoding is deterministic and stable. This is a frozen
/// contract extension of `verifiable-log`'s leaf encoding — see the builder
/// README.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinaryRecord {
    /// The git tag that produced this artifact, e.g. `"v1.3.0"`.
    pub release_tag: String,
    /// The exact source revision (40-hex git sha).
    pub commit: String,
    /// `darwin` | `windows` | `linux`.
    pub platform: String,
    /// `aarch64` | `x86_64`.
    pub arch: String,
    /// `dmg` | `app` | `nsis` | `appimage` | `deb` | `rpm`.
    pub bundle: String,
    /// The shipped file name, e.g. `"pollis-v1.3.0-macos.dmg"`.
    pub artifact_name: String,
    /// Which layer this leaf commits to (reproducible payload vs signed wrapper).
    pub layer: Layer,
    /// Hash of the reproducible pre-signature payload, lowercase hex.
    pub payload_sha256: String,
    /// Hash of the *shipped* artifact, lowercase hex
    /// (`== payload_sha256` when `layer == payload`).
    pub artifact_sha256: String,
    /// The pinned reproducibility recipe.
    pub toolchain: Toolchain,
    /// URI of this artifact's SLSA/in-toto provenance attestation.
    pub provenance_uri: String,
}

impl BinaryRecord {
    /// Canonical leaf bytes: compact JSON in the struct's declared field order.
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// Parse leaf bytes produced by [`Self::encode`] back into a `BinaryRecord`.
    pub fn decode(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    /// Build the tenant-tagged [`Entry`] for this leaf.
    pub fn to_entry(&self) -> Result<Entry, serde_json::Error> {
        Ok(Entry::new(TENANT, self.encode()?))
    }

    /// The fork-identity tuple: two leaves sharing this tuple describe the *same*
    /// released unit, so they must agree on `artifact_sha256` — see
    /// [`BinaryInvariant`].
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

/// The globally-auditable form of binary transparency's per-release rules
/// (`docs/verifiable-builds-design.md` §2.3). Registered for [`TENANT`] on the
/// builder's log, it is consulted on every append and enforces:
///
/// * **No silent re-issue (fork)** — two leaves with equal
///   `(release_tag, platform, arch, bundle, layer)` but different
///   `artifact_sha256` are a fork (mirrors the commit-log "no fork" rule); a
///   legitimate re-release must use a new tag.
/// * **Monotonic releases** — `release_tag` is append-only in publish order: once
///   a new tag has begun appearing, an earlier tag can never appear again (a
///   leaf cannot reference a tag out of publish order). The git-ancestry half of
///   the design rule is intentionally omitted here — a pure, offline verifier
///   has no git graph — leaving the cheap, self-contained tag-order check.
/// * **Derived-layer pairing** — every non-`payload` leaf (`signed`, `exe`) must
///   have a matching `layer:"payload"` leaf with equal `payload_sha256` earlier
///   in the tree, so the reproducible unit a derived leaf describes is always
///   itself published and independently reproducible. Stated over "not payload"
///   rather than per-variant so a future layer inherits the rule instead of
///   silently escaping it.
///
/// Because this runs inside `verifiable_log`'s replay, `pollis-verify` re-checks
/// it independently — the app and CLI can never disagree, the same guarantee the
/// other two trees enjoy.
pub struct BinaryInvariant;

impl BinaryInvariant {
    /// A leaf whose bytes don't parse as a `BinaryRecord` can't be reasoned
    /// about, so it's treated as a violation rather than silently accepted.
    fn parse(entry: &Entry) -> Result<BinaryRecord, InvariantViolation> {
        BinaryRecord::decode(&entry.data)
            .map_err(|e| InvariantViolation::new(TENANT, format!("malformed binary leaf: {e}")))
    }
}

impl TenantInvariant for BinaryInvariant {
    fn check(&self, existing: &[&Entry], candidate: &Entry) -> Result<(), InvariantViolation> {
        let cand = Self::parse(candidate)?;

        // Walk the prior leaves once, gathering what the tag-order and pairing
        // rules need: whether the candidate's tag has appeared before, the tag of
        // the most recent existing leaf, and whether a matching payload exists.
        let mut cand_tag_seen = false;
        let mut last_tag: Option<String> = None;
        let mut payload_seen = false;

        for prev_entry in existing {
            let prev = Self::parse(prev_entry)?;

            // (a) No silent re-issue: same released unit, different bytes.
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
            // (c) Track the matching payload for the pairing check below.
            if prev.layer == Layer::Payload && prev.payload_sha256 == cand.payload_sha256 {
                payload_seen = true;
            }
            // `existing` is in insertion (publish) order, so the last iteration
            // leaves `last_tag` holding the current tag of the tree.
            last_tag = Some(prev.release_tag.clone());
        }

        // (b) Monotonic releases: if the candidate's tag was published earlier
        //     but the tree has since moved on to a newer tag, this leaf jumped
        //     back out of publish order.
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

        // (c) Derived-layer pairing: a signed/exe leaf must be bound to an
        //     already-logged reproducible payload with the same payload_sha256.
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
        // Field order is frozen (matches §2.2 declaration order).
        assert!(s.starts_with("{\"release_tag\":"));
        assert!(s.find("\"release_tag\":").unwrap() < s.find("\"commit\":").unwrap());
        assert!(s.find("\"layer\":").unwrap() < s.find("\"payload_sha256\":").unwrap());
        assert!(s.find("\"toolchain\":").unwrap() < s.find("\"provenance_uri\":").unwrap());
        // The nested toolchain keeps its own declared order too.
        assert!(s.find("\"rustc\":").unwrap() < s.find("\"source_date_epoch\":").unwrap());
        // Layer serialises as a bare lowercase string.
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
        // signed wraps the earlier payload (same payload_sha256) — accepted.
        assert!(inv.check(&[&mac_payload], &mac_signed).is_ok());
        // a different platform's payload is independent — accepted.
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
        // same (tag, platform, arch, bundle, layer) but a different artifact hash.
        let forked = record("v1.0.0", "darwin", "dmg", Layer::Payload, 0x99, 0x99).to_entry().unwrap();
        let err = inv.check(&[&a], &forked).unwrap_err();
        assert!(err.message.contains("fork"), "got: {}", err.message);
    }

    #[test]
    fn rejects_signed_without_payload() {
        let inv = BinaryInvariant;
        // A signed leaf whose payload_sha256 was never logged as a payload leaf.
        let signed = record("v1.0.0", "windows", "nsis", Layer::Signed, 0x44, 0x55).to_entry().unwrap();
        let err = inv.check(&[], &signed).unwrap_err();
        assert!(err.message.contains("no matching payload"), "got: {}", err.message);
    }

    #[test]
    fn rejects_exe_without_payload() {
        let inv = BinaryInvariant;
        // The pairing rule is stated over "not payload", so `exe` inherits it:
        // an exe leaf must be bound to a published reproducible payload.
        let exe = record("v1.0.0", "darwin", "dmg", Layer::Exe, 0x44, 0x55).to_entry().unwrap();
        let err = inv.check(&[], &exe).unwrap_err();
        assert!(err.message.contains("no matching payload"), "got: {}", err.message);
    }

    #[test]
    fn accepts_exe_leaf_bound_to_its_payload() {
        let inv = BinaryInvariant;
        let payload = record("v1.0.0", "darwin", "dmg", Layer::Payload, 0x11, 0x11).to_entry().unwrap();
        let signed = record("v1.0.0", "darwin", "dmg", Layer::Signed, 0x11, 0x22).to_entry().unwrap();
        // Same payload_sha256 binds it to the .app payload; artifact_sha256 is
        // the main executable's own hash — what a running app can recompute.
        let exe = record("v1.0.0", "darwin", "dmg", Layer::Exe, 0x11, 0x33).to_entry().unwrap();
        assert!(inv.check(&[&payload, &signed], &exe).is_ok());
        // `exe` is its own fork_key slot, so it doesn't collide with `signed`
        // despite sharing (tag, platform, arch, bundle) and differing in hash.
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
        // Adding the variant must not disturb leaves already in the published
        // tree — their bytes (and every inclusion proof over them) are fixed.
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
        // After v1.1.0 has begun, a v1.0.0 leaf reappears — out of publish order.
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
