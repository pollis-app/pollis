//! MLS StorageProvider backed by the local SQLite `mls_kv` table.
//!
//! `MlsStore` wraps a `&rusqlite::Connection` and fully implements
//! `openmls_traits::storage::StorageProvider<CURRENT_VERSION>`.
//!
//! Key layout (mirrors openmls_memory_storage):
//!   scope = the entity-type label (e.g. b"KeyPackage", b"Tree", …)
//!   key   = serde_json-serialised lookup key + VERSION as 2-byte big-endian suffix
//!   value = serde_json-serialised entity (or JSON array of byte-arrays for lists)

use openmls_traits::storage::{traits, Entity, StorageProvider, CURRENT_VERSION};
use rusqlite::Connection;
use serde::{de::DeserializeOwned, Serialize};

// ── label constants (match openmls_memory_storage) ───────────────────────────

const KEY_PACKAGE_LABEL: &[u8] = b"KeyPackage";
const PSK_LABEL: &[u8] = b"Psk";
const ENCRYPTION_KEY_PAIR_LABEL: &[u8] = b"EncryptionKeyPair";
const SIGNATURE_KEY_PAIR_LABEL: &[u8] = b"SignatureKeyPair";
const EPOCH_KEY_PAIRS_LABEL: &[u8] = b"EpochKeyPairs";
const TREE_LABEL: &[u8] = b"Tree";
const GROUP_CONTEXT_LABEL: &[u8] = b"GroupContext";
const INTERIM_TRANSCRIPT_HASH_LABEL: &[u8] = b"InterimTranscriptHash";
const CONFIRMATION_TAG_LABEL: &[u8] = b"ConfirmationTag";
const JOIN_CONFIG_LABEL: &[u8] = b"MlsGroupJoinConfig";
const OWN_LEAF_NODES_LABEL: &[u8] = b"OwnLeafNodes";
const GROUP_STATE_LABEL: &[u8] = b"GroupState";
const QUEUED_PROPOSAL_LABEL: &[u8] = b"QueuedProposal";
const PROPOSAL_QUEUE_REFS_LABEL: &[u8] = b"ProposalQueueRefs";
const OWN_LEAF_NODE_INDEX_LABEL: &[u8] = b"OwnLeafNodeIndex";
const EPOCH_SECRETS_LABEL: &[u8] = b"EpochSecrets";
const RESUMPTION_PSK_STORE_LABEL: &[u8] = b"ResumptionPsk";
const MESSAGE_SECRETS_LABEL: &[u8] = b"MessageSecrets";

// ── Error type ───────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum MlsStorageError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
}

// ── key helpers ──────────────────────────────────────────────────────────────

/// Builds the composite storage key: label || serialised_key || VERSION (2 bytes BE).
fn build_key<const V: u16>(label: &[u8], key_bytes: Vec<u8>) -> Vec<u8> {
    let mut out = label.to_vec();
    out.extend_from_slice(&key_bytes);
    out.extend_from_slice(&V.to_be_bytes());
    out
}

fn epoch_key_pairs_id<const V: u16>(
    group_id: &impl Serialize,
    epoch: &impl Serialize,
    leaf_index: u32,
) -> Result<Vec<u8>, MlsStorageError> {
    let mut key = serde_json::to_vec(group_id)?;
    key.extend_from_slice(&serde_json::to_vec(epoch)?);
    key.extend_from_slice(&serde_json::to_vec(&leaf_index)?);
    Ok(key)
}

// ── MlsStore ─────────────────────────────────────────────────────────────────

/// Thin wrapper around a `rusqlite::Connection` that maps all MLS storage
/// operations to the `mls_kv(scope, key, value)` table.
///
/// `MlsStore` borrows `&Connection` so it must not outlive the local DB guard
/// that provides it.  This is enforced by the lifetime `'a`.
pub struct MlsStore<'a> {
    conn: &'a Connection,
}

impl<'a> MlsStore<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    // ── primitive KV ops ─────────────────────────────────────────────────────

    fn raw_write(&self, storage_key: Vec<u8>, value: Vec<u8>) -> Result<(), MlsStorageError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO mls_kv (scope, key, value) VALUES (?1, ?2, ?3)",
            rusqlite::params![b"" as &[u8], storage_key, value],
        )?;
        Ok(())
    }

    fn raw_read(&self, storage_key: &[u8]) -> Result<Option<Vec<u8>>, MlsStorageError> {
        match self.conn.query_row(
            "SELECT value FROM mls_kv WHERE scope = ?1 AND key = ?2",
            rusqlite::params![b"" as &[u8], storage_key],
            |row| row.get::<_, Vec<u8>>(0),
        ) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn raw_delete(&self, storage_key: &[u8]) -> Result<(), MlsStorageError> {
        self.conn.execute(
            "DELETE FROM mls_kv WHERE scope = ?1 AND key = ?2",
            rusqlite::params![b"" as &[u8], storage_key],
        )?;
        Ok(())
    }

    // ── typed write/read/delete for single values ─────────────────────────

    fn write<const V: u16>(
        &self,
        label: &[u8],
        key: &[u8],
        value: Vec<u8>,
    ) -> Result<(), MlsStorageError> {
        let storage_key = build_key::<V>(label, key.to_vec());
        self.raw_write(storage_key, value)
    }

    fn read<const V: u16, Ent: Entity<V>>(
        &self,
        label: &[u8],
        key: &[u8],
    ) -> Result<Option<Ent>, MlsStorageError> {
        let storage_key = build_key::<V>(label, key.to_vec());
        match self.raw_read(&storage_key)? {
            Some(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            None => Ok(None),
        }
    }

    fn delete<const V: u16>(
        &self,
        label: &[u8],
        key: &[u8],
    ) -> Result<(), MlsStorageError> {
        let storage_key = build_key::<V>(label, key.to_vec());
        self.raw_delete(&storage_key)
    }

    // ── typed write/read for lists ────────────────────────────────────────
    // Lists are persisted as JSON arrays of byte-arrays (Vec<Vec<u8>>),
    // matching the openmls_memory_storage format exactly.

    fn read_list<const V: u16, Ent: Entity<V>>(
        &self,
        label: &[u8],
        key: &[u8],
    ) -> Result<Vec<Ent>, MlsStorageError> {
        let mut storage_key = label.to_vec();
        storage_key.extend_from_slice(key);
        storage_key.extend_from_slice(&V.to_be_bytes());
        match self.raw_read(&storage_key)? {
            None => Ok(vec![]),
            Some(bytes) => {
                let list: Vec<Vec<u8>> = serde_json::from_slice(&bytes)?;
                list.iter()
                    .map(|item| Ok(serde_json::from_slice(item)?))
                    .collect()
            }
        }
    }

    fn append<const V: u16>(
        &self,
        label: &[u8],
        key: &[u8],
        value: Vec<u8>,
    ) -> Result<(), MlsStorageError> {
        let mut storage_key = label.to_vec();
        storage_key.extend_from_slice(key);
        storage_key.extend_from_slice(&V.to_be_bytes());

        let mut list: Vec<Vec<u8>> = match self.raw_read(&storage_key)? {
            Some(bytes) => serde_json::from_slice(&bytes)?,
            None => vec![],
        };
        list.push(value);
        self.raw_write(storage_key, serde_json::to_vec(&list)?)
    }

    fn remove_item<const V: u16>(
        &self,
        label: &[u8],
        key: &[u8],
        value: Vec<u8>,
    ) -> Result<(), MlsStorageError> {
        let mut storage_key = label.to_vec();
        storage_key.extend_from_slice(key);
        storage_key.extend_from_slice(&V.to_be_bytes());

        let mut list: Vec<Vec<u8>> = match self.raw_read(&storage_key)? {
            Some(bytes) => serde_json::from_slice(&bytes)?,
            None => return Ok(()),
        };
        if let Some(pos) = list.iter().position(|item| item == &value) {
            list.remove(pos);
        }
        self.raw_write(storage_key, serde_json::to_vec(&list)?)
    }
}

// ── StorageProvider impl ──────────────────────────────────────────────────────

impl StorageProvider<CURRENT_VERSION> for MlsStore<'_> {
    type Error = MlsStorageError;

    // --- writes ---

    fn write_mls_join_config<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        MlsGroupJoinConfig: traits::MlsGroupJoinConfig<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        config: &MlsGroupJoinConfig,
    ) -> Result<(), Self::Error> {
        self.write::<CURRENT_VERSION>(
            JOIN_CONFIG_LABEL,
            &serde_json::to_vec(group_id)?,
            serde_json::to_vec(config)?,
        )
    }

    fn append_own_leaf_node<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        LeafNode: traits::LeafNode<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        leaf_node: &LeafNode,
    ) -> Result<(), Self::Error> {
        self.append::<CURRENT_VERSION>(
            OWN_LEAF_NODES_LABEL,
            &serde_json::to_vec(group_id)?,
            serde_json::to_vec(leaf_node)?,
        )
    }

    fn queue_proposal<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ProposalRef: traits::ProposalRef<CURRENT_VERSION>,
        QueuedProposal: traits::QueuedProposal<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        proposal_ref: &ProposalRef,
        proposal: &QueuedProposal,
    ) -> Result<(), Self::Error> {
        // Write (group_id, proposal_ref) → proposal
        self.write::<CURRENT_VERSION>(
            QUEUED_PROPOSAL_LABEL,
            &serde_json::to_vec(&(group_id, proposal_ref))?,
            serde_json::to_vec(proposal)?,
        )?;
        // Append proposal_ref to the per-group ref list
        self.append::<CURRENT_VERSION>(
            PROPOSAL_QUEUE_REFS_LABEL,
            &serde_json::to_vec(group_id)?,
            serde_json::to_vec(proposal_ref)?,
        )
    }

    fn write_tree<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        TreeSync: traits::TreeSync<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        tree: &TreeSync,
    ) -> Result<(), Self::Error> {
        self.write::<CURRENT_VERSION>(
            TREE_LABEL,
            &serde_json::to_vec(group_id)?,
            serde_json::to_vec(tree)?,
        )
    }

    fn write_interim_transcript_hash<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        InterimTranscriptHash: traits::InterimTranscriptHash<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        interim_transcript_hash: &InterimTranscriptHash,
    ) -> Result<(), Self::Error> {
        self.write::<CURRENT_VERSION>(
            INTERIM_TRANSCRIPT_HASH_LABEL,
            &serde_json::to_vec(group_id)?,
            serde_json::to_vec(interim_transcript_hash)?,
        )
    }

    fn write_context<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        GroupContext: traits::GroupContext<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        group_context: &GroupContext,
    ) -> Result<(), Self::Error> {
        self.write::<CURRENT_VERSION>(
            GROUP_CONTEXT_LABEL,
            &serde_json::to_vec(group_id)?,
            serde_json::to_vec(group_context)?,
        )
    }

    fn write_confirmation_tag<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ConfirmationTag: traits::ConfirmationTag<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        confirmation_tag: &ConfirmationTag,
    ) -> Result<(), Self::Error> {
        self.write::<CURRENT_VERSION>(
            CONFIRMATION_TAG_LABEL,
            &serde_json::to_vec(group_id)?,
            serde_json::to_vec(confirmation_tag)?,
        )
    }

    fn write_group_state<
        GroupState: traits::GroupState<CURRENT_VERSION>,
        GroupId: traits::GroupId<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        group_state: &GroupState,
    ) -> Result<(), Self::Error> {
        self.write::<CURRENT_VERSION>(
            GROUP_STATE_LABEL,
            &serde_json::to_vec(group_id)?,
            serde_json::to_vec(group_state)?,
        )
    }

    fn write_message_secrets<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        MessageSecrets: traits::MessageSecrets<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        message_secrets: &MessageSecrets,
    ) -> Result<(), Self::Error> {
        self.write::<CURRENT_VERSION>(
            MESSAGE_SECRETS_LABEL,
            &serde_json::to_vec(group_id)?,
            serde_json::to_vec(message_secrets)?,
        )
    }

    fn write_resumption_psk_store<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ResumptionPskStore: traits::ResumptionPskStore<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        resumption_psk_store: &ResumptionPskStore,
    ) -> Result<(), Self::Error> {
        self.write::<CURRENT_VERSION>(
            RESUMPTION_PSK_STORE_LABEL,
            &serde_json::to_vec(group_id)?,
            serde_json::to_vec(resumption_psk_store)?,
        )
    }

    fn write_own_leaf_index<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        LeafNodeIndex: traits::LeafNodeIndex<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        own_leaf_index: &LeafNodeIndex,
    ) -> Result<(), Self::Error> {
        self.write::<CURRENT_VERSION>(
            OWN_LEAF_NODE_INDEX_LABEL,
            &serde_json::to_vec(group_id)?,
            serde_json::to_vec(own_leaf_index)?,
        )
    }

    fn write_group_epoch_secrets<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        GroupEpochSecrets: traits::GroupEpochSecrets<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        group_epoch_secrets: &GroupEpochSecrets,
    ) -> Result<(), Self::Error> {
        self.write::<CURRENT_VERSION>(
            EPOCH_SECRETS_LABEL,
            &serde_json::to_vec(group_id)?,
            serde_json::to_vec(group_epoch_secrets)?,
        )
    }

    fn write_signature_key_pair<
        SignaturePublicKey: traits::SignaturePublicKey<CURRENT_VERSION>,
        SignatureKeyPair: traits::SignatureKeyPair<CURRENT_VERSION>,
    >(
        &self,
        public_key: &SignaturePublicKey,
        signature_key_pair: &SignatureKeyPair,
    ) -> Result<(), Self::Error> {
        self.write::<CURRENT_VERSION>(
            SIGNATURE_KEY_PAIR_LABEL,
            &serde_json::to_vec(public_key)?,
            serde_json::to_vec(signature_key_pair)?,
        )
    }

    fn write_encryption_key_pair<
        EncryptionKey: traits::EncryptionKey<CURRENT_VERSION>,
        HpkeKeyPair: traits::HpkeKeyPair<CURRENT_VERSION>,
    >(
        &self,
        public_key: &EncryptionKey,
        key_pair: &HpkeKeyPair,
    ) -> Result<(), Self::Error> {
        self.write::<CURRENT_VERSION>(
            ENCRYPTION_KEY_PAIR_LABEL,
            &serde_json::to_vec(public_key)?,
            serde_json::to_vec(key_pair)?,
        )
    }

    fn write_encryption_epoch_key_pairs<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        EpochKey: traits::EpochKey<CURRENT_VERSION>,
        HpkeKeyPair: traits::HpkeKeyPair<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        epoch: &EpochKey,
        leaf_index: u32,
        key_pairs: &[HpkeKeyPair],
    ) -> Result<(), Self::Error> {
        let key = epoch_key_pairs_id::<CURRENT_VERSION>(group_id, epoch, leaf_index)?;
        self.write::<CURRENT_VERSION>(EPOCH_KEY_PAIRS_LABEL, &key, serde_json::to_vec(key_pairs)?)
    }

    fn write_key_package<
        HashReference: traits::HashReference<CURRENT_VERSION>,
        KeyPackage: traits::KeyPackage<CURRENT_VERSION>,
    >(
        &self,
        hash_ref: &HashReference,
        key_package: &KeyPackage,
    ) -> Result<(), Self::Error> {
        self.write::<CURRENT_VERSION>(
            KEY_PACKAGE_LABEL,
            &serde_json::to_vec(hash_ref)?,
            serde_json::to_vec(key_package)?,
        )
    }

    fn write_psk<
        PskId: traits::PskId<CURRENT_VERSION>,
        PskBundle: traits::PskBundle<CURRENT_VERSION>,
    >(
        &self,
        psk_id: &PskId,
        psk: &PskBundle,
    ) -> Result<(), Self::Error> {
        self.write::<CURRENT_VERSION>(
            PSK_LABEL,
            &serde_json::to_vec(psk_id)?,
            serde_json::to_vec(psk)?,
        )
    }

    // --- reads ---

    fn mls_group_join_config<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        MlsGroupJoinConfig: traits::MlsGroupJoinConfig<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<MlsGroupJoinConfig>, Self::Error> {
        self.read::<CURRENT_VERSION, _>(JOIN_CONFIG_LABEL, &serde_json::to_vec(group_id)?)
    }

    fn own_leaf_nodes<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        LeafNode: traits::LeafNode<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<LeafNode>, Self::Error> {
        self.read_list::<CURRENT_VERSION, _>(OWN_LEAF_NODES_LABEL, &serde_json::to_vec(group_id)?)
    }

    fn queued_proposal_refs<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ProposalRef: traits::ProposalRef<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<ProposalRef>, Self::Error> {
        self.read_list::<CURRENT_VERSION, _>(
            PROPOSAL_QUEUE_REFS_LABEL,
            &serde_json::to_vec(group_id)?,
        )
    }

    fn queued_proposals<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ProposalRef: traits::ProposalRef<CURRENT_VERSION>,
        QueuedProposal: traits::QueuedProposal<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<(ProposalRef, QueuedProposal)>, Self::Error> {
        let refs: Vec<ProposalRef> = self.read_list::<CURRENT_VERSION, _>(
            PROPOSAL_QUEUE_REFS_LABEL,
            &serde_json::to_vec(group_id)?,
        )?;
        refs.into_iter()
            .map(|proposal_ref| {
                let key = serde_json::to_vec(&(group_id, &proposal_ref))?;
                let proposal: QueuedProposal = self
                    .read::<CURRENT_VERSION, _>(QUEUED_PROPOSAL_LABEL, &key)?
                    .ok_or_else(|| {
                        serde_json::Error::io(std::io::Error::other("missing queued proposal"))
                    })?;
                Ok((proposal_ref, proposal))
            })
            .collect()
    }

    fn tree<GroupId: traits::GroupId<CURRENT_VERSION>, TreeSync: traits::TreeSync<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<TreeSync>, Self::Error> {
        self.read::<CURRENT_VERSION, _>(TREE_LABEL, &serde_json::to_vec(group_id)?)
    }

    fn group_context<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        GroupContext: traits::GroupContext<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<GroupContext>, Self::Error> {
        self.read::<CURRENT_VERSION, _>(GROUP_CONTEXT_LABEL, &serde_json::to_vec(group_id)?)
    }

    fn interim_transcript_hash<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        InterimTranscriptHash: traits::InterimTranscriptHash<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<InterimTranscriptHash>, Self::Error> {
        self.read::<CURRENT_VERSION, _>(
            INTERIM_TRANSCRIPT_HASH_LABEL,
            &serde_json::to_vec(group_id)?,
        )
    }

    fn confirmation_tag<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ConfirmationTag: traits::ConfirmationTag<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<ConfirmationTag>, Self::Error> {
        self.read::<CURRENT_VERSION, _>(CONFIRMATION_TAG_LABEL, &serde_json::to_vec(group_id)?)
    }

    fn group_state<
        GroupState: traits::GroupState<CURRENT_VERSION>,
        GroupId: traits::GroupId<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<GroupState>, Self::Error> {
        self.read::<CURRENT_VERSION, _>(GROUP_STATE_LABEL, &serde_json::to_vec(group_id)?)
    }

    fn message_secrets<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        MessageSecrets: traits::MessageSecrets<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<MessageSecrets>, Self::Error> {
        self.read::<CURRENT_VERSION, _>(MESSAGE_SECRETS_LABEL, &serde_json::to_vec(group_id)?)
    }

    fn resumption_psk_store<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ResumptionPskStore: traits::ResumptionPskStore<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<ResumptionPskStore>, Self::Error> {
        self.read::<CURRENT_VERSION, _>(
            RESUMPTION_PSK_STORE_LABEL,
            &serde_json::to_vec(group_id)?,
        )
    }

    fn own_leaf_index<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        LeafNodeIndex: traits::LeafNodeIndex<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<LeafNodeIndex>, Self::Error> {
        self.read::<CURRENT_VERSION, _>(OWN_LEAF_NODE_INDEX_LABEL, &serde_json::to_vec(group_id)?)
    }

    fn group_epoch_secrets<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        GroupEpochSecrets: traits::GroupEpochSecrets<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<GroupEpochSecrets>, Self::Error> {
        self.read::<CURRENT_VERSION, _>(EPOCH_SECRETS_LABEL, &serde_json::to_vec(group_id)?)
    }

    fn signature_key_pair<
        SignaturePublicKey: traits::SignaturePublicKey<CURRENT_VERSION>,
        SignatureKeyPair: traits::SignatureKeyPair<CURRENT_VERSION>,
    >(
        &self,
        public_key: &SignaturePublicKey,
    ) -> Result<Option<SignatureKeyPair>, Self::Error> {
        self.read::<CURRENT_VERSION, _>(
            SIGNATURE_KEY_PAIR_LABEL,
            &serde_json::to_vec(public_key)?,
        )
    }

    fn encryption_key_pair<
        HpkeKeyPair: traits::HpkeKeyPair<CURRENT_VERSION>,
        EncryptionKey: traits::EncryptionKey<CURRENT_VERSION>,
    >(
        &self,
        public_key: &EncryptionKey,
    ) -> Result<Option<HpkeKeyPair>, Self::Error> {
        self.read::<CURRENT_VERSION, _>(
            ENCRYPTION_KEY_PAIR_LABEL,
            &serde_json::to_vec(public_key)?,
        )
    }

    fn encryption_epoch_key_pairs<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        EpochKey: traits::EpochKey<CURRENT_VERSION>,
        HpkeKeyPair: traits::HpkeKeyPair<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        epoch: &EpochKey,
        leaf_index: u32,
    ) -> Result<Vec<HpkeKeyPair>, Self::Error> {
        let key = epoch_key_pairs_id::<CURRENT_VERSION>(group_id, epoch, leaf_index)?;
        let storage_key = build_key::<CURRENT_VERSION>(EPOCH_KEY_PAIRS_LABEL, key);
        match self.raw_read(&storage_key)? {
            None => Ok(vec![]),
            Some(bytes) => Ok(serde_json::from_slice(&bytes)?),
        }
    }

    fn key_package<
        KeyPackageRef: traits::HashReference<CURRENT_VERSION>,
        KeyPackage: traits::KeyPackage<CURRENT_VERSION>,
    >(
        &self,
        hash_ref: &KeyPackageRef,
    ) -> Result<Option<KeyPackage>, Self::Error> {
        self.read::<CURRENT_VERSION, _>(KEY_PACKAGE_LABEL, &serde_json::to_vec(hash_ref)?)
    }

    fn psk<
        PskBundle: traits::PskBundle<CURRENT_VERSION>,
        PskId: traits::PskId<CURRENT_VERSION>,
    >(
        &self,
        psk_id: &PskId,
    ) -> Result<Option<PskBundle>, Self::Error> {
        self.read::<CURRENT_VERSION, _>(PSK_LABEL, &serde_json::to_vec(psk_id)?)
    }

    // --- deletes ---

    fn remove_proposal<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ProposalRef: traits::ProposalRef<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        proposal_ref: &ProposalRef,
    ) -> Result<(), Self::Error> {
        // Remove from refs list
        self.remove_item::<CURRENT_VERSION>(
            PROPOSAL_QUEUE_REFS_LABEL,
            &serde_json::to_vec(group_id)?,
            serde_json::to_vec(proposal_ref)?,
        )?;
        // Delete the proposal itself
        self.delete::<CURRENT_VERSION>(
            QUEUED_PROPOSAL_LABEL,
            &serde_json::to_vec(&(group_id, proposal_ref))?,
        )
    }

    fn delete_own_leaf_nodes<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.delete::<CURRENT_VERSION>(OWN_LEAF_NODES_LABEL, &serde_json::to_vec(group_id)?)
    }

    fn delete_group_config<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.delete::<CURRENT_VERSION>(JOIN_CONFIG_LABEL, &serde_json::to_vec(group_id)?)
    }

    fn delete_tree<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.delete::<CURRENT_VERSION>(TREE_LABEL, &serde_json::to_vec(group_id)?)
    }

    fn delete_confirmation_tag<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.delete::<CURRENT_VERSION>(CONFIRMATION_TAG_LABEL, &serde_json::to_vec(group_id)?)
    }

    fn delete_group_state<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.delete::<CURRENT_VERSION>(GROUP_STATE_LABEL, &serde_json::to_vec(group_id)?)
    }

    fn delete_context<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.delete::<CURRENT_VERSION>(GROUP_CONTEXT_LABEL, &serde_json::to_vec(group_id)?)
    }

    fn delete_interim_transcript_hash<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.delete::<CURRENT_VERSION>(
            INTERIM_TRANSCRIPT_HASH_LABEL,
            &serde_json::to_vec(group_id)?,
        )
    }

    fn delete_message_secrets<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.delete::<CURRENT_VERSION>(MESSAGE_SECRETS_LABEL, &serde_json::to_vec(group_id)?)
    }

    fn delete_all_resumption_psk_secrets<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.delete::<CURRENT_VERSION>(RESUMPTION_PSK_STORE_LABEL, &serde_json::to_vec(group_id)?)
    }

    fn delete_own_leaf_index<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.delete::<CURRENT_VERSION>(OWN_LEAF_NODE_INDEX_LABEL, &serde_json::to_vec(group_id)?)
    }

    fn delete_group_epoch_secrets<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.delete::<CURRENT_VERSION>(EPOCH_SECRETS_LABEL, &serde_json::to_vec(group_id)?)
    }

    fn clear_proposal_queue<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ProposalRef: traits::ProposalRef<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        let refs: Vec<ProposalRef> = self.read_list::<CURRENT_VERSION, _>(
            PROPOSAL_QUEUE_REFS_LABEL,
            &serde_json::to_vec(group_id)?,
        )?;
        for proposal_ref in &refs {
            let key = serde_json::to_vec(&(group_id, proposal_ref))?;
            self.delete::<CURRENT_VERSION>(QUEUED_PROPOSAL_LABEL, &key)?;
        }
        self.delete::<CURRENT_VERSION>(PROPOSAL_QUEUE_REFS_LABEL, &serde_json::to_vec(group_id)?)
    }

    fn delete_signature_key_pair<
        SignaturePublicKey: traits::SignaturePublicKey<CURRENT_VERSION>,
    >(
        &self,
        public_key: &SignaturePublicKey,
    ) -> Result<(), Self::Error> {
        self.delete::<CURRENT_VERSION>(
            SIGNATURE_KEY_PAIR_LABEL,
            &serde_json::to_vec(public_key)?,
        )
    }

    fn delete_encryption_key_pair<EncryptionKey: traits::EncryptionKey<CURRENT_VERSION>>(
        &self,
        public_key: &EncryptionKey,
    ) -> Result<(), Self::Error> {
        self.delete::<CURRENT_VERSION>(
            ENCRYPTION_KEY_PAIR_LABEL,
            &serde_json::to_vec(public_key)?,
        )
    }

    fn delete_encryption_epoch_key_pairs<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        EpochKey: traits::EpochKey<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        epoch: &EpochKey,
        leaf_index: u32,
    ) -> Result<(), Self::Error> {
        let key = epoch_key_pairs_id::<CURRENT_VERSION>(group_id, epoch, leaf_index)?;
        self.delete::<CURRENT_VERSION>(EPOCH_KEY_PAIRS_LABEL, &key)
    }

    fn delete_key_package<KeyPackageRef: traits::HashReference<CURRENT_VERSION>>(
        &self,
        hash_ref: &KeyPackageRef,
    ) -> Result<(), Self::Error> {
        self.delete::<CURRENT_VERSION>(KEY_PACKAGE_LABEL, &serde_json::to_vec(hash_ref)?)
    }

    fn delete_psk<PskKey: traits::PskId<CURRENT_VERSION>>(
        &self,
        psk_id: &PskKey,
    ) -> Result<(), Self::Error> {
        self.delete::<CURRENT_VERSION>(PSK_LABEL, &serde_json::to_vec(psk_id)?)
    }
}
