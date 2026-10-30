//! End-to-end integration harness for Pollis.
//! Run with:
//! ```
//! cargo test --features test-harness --test flows
//! ```

#[path = "flows/harness.rs"]
mod harness;

#[path = "flows/adversarial.rs"]
mod adversarial;
#[path = "flows/auth.rs"]
mod auth;
#[path = "flows/channel_mute.rs"]
mod channel_mute;
#[path = "flows/cross_channel_ingest.rs"]
mod cross_channel_ingest;
#[path = "flows/cross_signing.rs"]
mod cross_signing;
#[path = "flows/device_link.rs"]
mod device_link;
#[path = "flows/device_outside_tree.rs"]
mod device_outside_tree;
#[path = "flows/dms.rs"]
mod dms;
#[path = "flows/ds_surface.rs"]
mod ds_surface;
#[path = "flows/groups.rs"]
mod groups;
#[path = "flows/heavy_churn.rs"]
mod heavy_churn;
#[path = "flows/key_package_exhaustion.rs"]
mod key_package_exhaustion;
// Tests the harness's own sqlite3, not the product.
#[path = "flows/linked_sqlite.rs"]
mod linked_sqlite;
#[path = "flows/lost_messages.rs"]
mod lost_messages;
#[path = "flows/messages.rs"]
mod messages;
#[path = "flows/model.rs"]
mod model;
#[path = "flows/outbox.rs"]
mod outbox;
#[path = "flows/own_envelopes.rs"]
mod own_envelopes;
#[path = "flows/file_keys.rs"]
mod file_keys;
#[path = "flows/pinning.rs"]
mod pinning;
#[path = "flows/pq_key_packages.rs"]
mod pq_key_packages;
#[path = "flows/pq_migration.rs"]
mod pq_migration;
#[path = "flows/push_dismiss.rs"]
mod push_dismiss;
#[path = "flows/rejoin.rs"]
mod rejoin;
#[path = "flows/rich_push.rs"]
mod rich_push;
#[path = "flows/reports.rs"]
mod reports;
#[path = "flows/sealed_sender.rs"]
mod sealed_sender;
#[path = "flows/security.rs"]
mod security;
#[path = "flows/vault.rs"]
mod vault;
#[path = "flows/zero_loss.rs"]
mod zero_loss;
// Needs the media command surface.
#[cfg(feature = "media")]
#[path = "flows/voice.rs"]
mod voice;
