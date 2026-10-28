//! What the detached `fuzz/` crate needs to stand up real groups the way this
//! crate does (#1537), behind the `fuzzing` feature: the one suite, the one
//! sender-ratchet window and the create / join configurations, plus the crates
//! a target builds them with — re-exported so the fuzz crate cannot pick up a
//! second, differently-featured openmls.

pub use openmls;
pub use openmls_basic_credential;
pub use openmls_traits;

pub use super::{make_credential, parse_credential_user_id, MlsDecryptor, PollisProvider};

/// The suite every group is born on.
pub const CIPHERSUITE: openmls::prelude::Ciphersuite = super::provider::CS_PQ;

/// The create configuration `group_state` births a group with.
pub fn create_config() -> openmls::prelude::MlsGroupCreateConfig {
    openmls::prelude::MlsGroupCreateConfig::builder()
        .ciphersuite(CIPHERSUITE)
        .use_ratchet_tree_extension(true)
        .sender_ratchet_configuration(super::provider::sender_ratchet_configuration())
        .build()
}

/// The configuration a Welcome is joined with.
pub fn join_config() -> openmls::prelude::MlsGroupJoinConfig {
    super::provider::join_config()
}
