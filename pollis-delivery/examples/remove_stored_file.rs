//! Operator removal of a stored file (#2109). Operator only; driven by
//! `scripts/remove-stored-file.sh`, which runs it under the DS's own Doppler
//! config. No route, no token: it uses exactly the database and R2
//! credentials the Delivery Service itself runs with (`TURSO_URL` /
//! `TURSO_TOKEN`, `R2_*`), and the same `messages::remove_stored_object` the
//! DS tests cover.
//!
//! Because a file is stored once, removing it takes it from every message and
//! Vault entry that holds it: the tombstone is written first (the id is then
//! refused everywhere — claim, registration, presign — so the identical file
//! cannot be stored again), then the R2 object, its row, every reference and
//! every claim go. Running it again finishes a removal a bucket failure cut
//! short.
//!
//!   cargo run -p pollis-delivery --example remove_stored_file -- [--dry-run] <object id>

use anyhow::{bail, Context, Result};
use pollis_delivery::broker::BrokerConfig;
use pollis_delivery::db::Db;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let dry_run = args.first().is_some_and(|a| a == "--dry-run");
    if dry_run {
        args.remove(0);
    }
    let [id] = args.as_slice() else {
        bail!("usage: remove_stored_file [--dry-run] <object id>");
    };
    let v1 = id.len() == 64 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !v1 && !pollis_api::messages::is_shared_object_id(id) {
        bail!("{id:?} is not an attachment id (64 hex, or v2- + 64 hex)");
    }

    let url = std::env::var("TURSO_URL").context("TURSO_URL (the DS's database) must be set")?;
    let token = std::env::var("TURSO_TOKEN").context("TURSO_TOKEN must be set")?;
    let db = Db::connect_remote(&url, &token).await.context("connect to the DS database")?;
    let conn = db.conn().await?;
    let broker = BrokerConfig::from_env();
    if broker.r2_ready().is_none() {
        bail!("R2 is not configured (R2_S3_ENDPOINT, R2_BUCKET, R2_ACCESS_KEY_ID, R2_SECRET_KEY)");
    }

    let count = |sql: &'static str| {
        let conn = conn.clone();
        let id = id.clone();
        async move {
            let mut rows = conn.query(sql, libsql::params![id]).await?;
            Ok::<i64, anyhow::Error>(match rows.next().await? {
                Some(row) => row.get::<i64>(0)?,
                None => 0,
            })
        }
    };
    let object = count("SELECT COUNT(*) FROM attachment_object WHERE content_hash = ?1").await?;
    let refs = count("SELECT COUNT(*) FROM attachment_ref WHERE content_hash = ?1").await?
        + count("SELECT COUNT(*) FROM vault_attachment_ref WHERE content_hash = ?1").await?;
    let removed = count("SELECT COUNT(*) FROM attachment_removed WHERE content_hash = ?1").await?;
    println!("object row: {object}, references: {refs}, already removed: {}", removed > 0);
    if dry_run {
        println!("dry run: nothing changed");
        return Ok(());
    }

    let report = pollis_delivery::messages::remove_stored_object(&conn, &broker, id)
        .await
        .context("removal did not finish; run it again")?;
    println!("removed {id}: {} reference(s) dropped; the id is refused from now on", report.references);
    Ok(())
}
