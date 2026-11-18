//! The one verifier of "signed by a current admin's device", shared by #1182
//! host approvals and #2005 App approvals, so both hold an admin signature to
//! the same rule, checked from the verifier's own state:
//!
//! 1. [`verify_signer`]: the signing device is a leaf in the verifier's own
//!    MLS view of the group, its leaf key is `Certified` under the signer's
//!    account key (`IdentityDirectory`, TOFU-pinned), and the signature over
//!    `domain ‖ 0x00 ‖ payload` verifies under that leaf key.
//! 2. [`verify_signer_is_admin`]: the transparency log does not disagree with
//!    the signer's account key, and the roster read lists the signer as an
//!    admin of the group. The MLS view (step 1) and the roster must agree.

use std::sync::Arc;

use super::approval;
use super::host::AccountKeyAudit;
use crate::commands::transparency::AuditStatus;
use crate::error::Result;
use crate::state::AppState;

/// Why an admin signature was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignerRefusal {
    /// The signing device has no leaf in the verifier's view of the group.
    NotInGroup,
    /// The signing device is not certified by its account key, or is revoked.
    Uncertified(String),
    /// The signature does not verify under the device's certified leaf key.
    BadSignature,
    /// The transparency log disagrees with the signer's account key.
    TransparencyAlarm,
    /// The roster does not list the signer as an admin of the group.
    NotAdmin(String),
}

/// Step 1: the signer's device is a certified leaf in `viewer`'s view of
/// `group_id`, and `signature` verifies under its leaf key.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn verify_signer(
    state: &Arc<AppState>,
    group_id: &str,
    viewer_user_id: &str,
    signer_user_id: &str,
    signer_device_id: &str,
    domain: &[u8],
    payload: &[u8],
    signature: &[u8],
) -> Result<std::result::Result<(), SignerRefusal>> {
    let Some((leaf_key, scheme)) =
        crate::commands::mls::local_leaf_signature_key(state, group_id, signer_user_id, signer_device_id).await
    else {
        return Ok(Err(SignerRefusal::NotInGroup));
    };
    if scheme != openmls::prelude::SignatureScheme::MLDSA44 {
        return Ok(Err(SignerRefusal::Uncertified(format!(
            "the group signs with {scheme:?}, not ML-DSA-44"
        ))));
    }
    let directory = crate::commands::mls::load_pinned_identities(
        state,
        std::slice::from_ref(&signer_user_id.to_string()),
        viewer_user_id,
    )
    .await?;
    match directory.leaf_verdict(signer_user_id, signer_device_id, &leaf_key, scheme) {
        crate::commands::mls::LeafVerdict::Certified => {}
        crate::commands::mls::LeafVerdict::Unverifiable(why) => {
            return Ok(Err(SignerRefusal::Uncertified(why.to_string())))
        }
        crate::commands::mls::LeafVerdict::Uncertified(why) => return Ok(Err(SignerRefusal::Uncertified(why))),
    }
    if approval::verify_signature_in(domain, &leaf_key, payload, signature).is_err() {
        return Ok(Err(SignerRefusal::BadSignature));
    }
    Ok(Ok(()))
}

/// Step 2: the transparency log does not alarm on the signer, and the roster
/// read lists them as an admin of `group_id`.
pub(crate) async fn verify_signer_is_admin(
    state: &Arc<AppState>,
    group_id: &str,
    viewer_user_id: &str,
    signer_user_id: &str,
    audit: &dyn AccountKeyAudit,
) -> Result<std::result::Result<(), SignerRefusal>> {
    if audit.audit(state, signer_user_id).await == AuditStatus::Alarm {
        return Ok(Err(SignerRefusal::TransparencyAlarm));
    }
    if let Some(why) = not_admin_in_roster(state, group_id, viewer_user_id, signer_user_id).await? {
        return Ok(Err(SignerRefusal::NotAdmin(why)));
    }
    Ok(Ok(()))
}

/// `Some(reason)` unless the roster read lists `user_id` as an admin of the group.
pub(crate) async fn not_admin_in_roster(
    state: &Arc<AppState>,
    group_id: &str,
    viewer_user_id: &str,
    user_id: &str,
) -> Result<Option<String>> {
    let resp = crate::commands::ds_reads::group(state, group_id, viewer_user_id, false).await?;
    if !resp.authorized {
        return Ok(Some("cannot read the group's roster".to_string()));
    }
    Ok(match resp.members.iter().find(|m| m.user_id == user_id) {
        Some(m) if m.role == "admin" => None,
        Some(_) => Some("roster: member, not admin".to_string()),
        None => Some("roster: not a member".to_string()),
    })
}
