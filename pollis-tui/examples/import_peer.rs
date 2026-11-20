//! Dev-DS check for the structure import: Discord (#1180) and Slack (#1179).
//!
//! Signs up (or signs in) `<email>` on the DS named by `POLLIS_DELIVERY_URL`
//! (the dev DS — it fixes the OTP), imports `<export>` with the same
//! `pollis_core::commands::import` calls (not shipped in any app; `import` feature), then runs it a
//! second time against the group the first run made and reads the group back:
//! categories and channels in order, the emoji, the invite links and, with
//! `--invite-by-email`, what became of each planned email. Prints the lines the
//! PR quotes; exits non-zero if the second run created anything, invited
//! anyone again, or the layout differs from the plan.
//!
//! `signup <email>` only signs an account up (an invitee for the email check).
//! Email invites refuse to run unless every planned address is `@pollis.test`.
//!
//! The flows suite covers the same path against the in-process DS, which has
//! no object storage; this is where the emoji and icon uploads are real.
//!
//!   POLLIS_DATA_DIR=$(mktemp -d) POLLIS_DELIVERY_URL=https://api-dev.pollis.com \
//!   POLLIS_DEV_ACCESS_KEY=… cargo run -p pollis-tui --features import-peer --example import_peer -- \
//!   slack <email> pollis-import/tests/fixtures/northwind --invite-by-email

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use pollis_core::commands::import::{self, EmailInviteOutcome, ImportApplyOptions, SourceKind};
use pollis_core::commands::{emoji, groups};
use pollis_core::config::Config;
use pollis_core::state::AppState;

/// The dev DS's fixed OTP (`DEV_OTP`); override with `POLLIS_DEV_OTP`.
const DEFAULT_OTP: &str = "000000";

const USAGE: &str = "usage: import_peer <discord|slack> <email> <export> [--invite-by-email] | import_peer signup <email>";

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().context(USAGE)?.as_str();
    let email = args.get(1).context(USAGE)?.clone();
    if !email.ends_with("@pollis.test") {
        bail!("refusing {email:?}: only @pollis.test addresses");
    }
    let otp = std::env::var("POLLIS_DEV_OTP").unwrap_or_else(|_| DEFAULT_OTP.to_string());
    let config = Config::from_env().context("POLLIS_DELIVERY_URL (the dev DS) is required")?;
    let ds = config.pollis_delivery_url.clone().unwrap_or_default();
    if !ds.contains("api-dev") && !ds.contains("localhost") {
        bail!("refusing to run against {ds:?} — dev DS only");
    }
    let state = Arc::new(AppState::new(config).await.context("AppState")?);
    pollis_tui::auth::request_otp(&state, &email).await?;
    let me = pollis_tui::auth::verify_otp(&state, &email, &otp).await?;
    pollis_tui::auth::set_pin_and_init(&state, &me.id, "0000").await?;
    if mode == "signup" {
        println!("SIGNED_UP={} ({})", me.username, me.id);
        return Ok(());
    }

    let source = match mode {
        "discord" => SourceKind::Discord,
        "slack" => SourceKind::Slack,
        _ => bail!("{USAGE}"),
    };
    let path = args.get(2).context(USAGE)?;
    let path = std::fs::canonicalize(path)?.to_string_lossy().into_owned();
    let invite_by_email = args.iter().any(|a| a == "--invite-by-email");
    println!("IMPORTER={} ({})", me.username, me.id);

    let preview = import::import_preview(source, path.clone(), me.id.clone(), None, None, &state).await?;
    println!(
        "PREVIEW {} target={:?} create: {} categories, {} channels, {} emoji; {} people, {} with email",
        source.name(),
        preview.target.as_ref().map(|t| &t.name),
        preview.plan.totals.to_create_categories,
        preview.plan.totals.to_create_channels,
        preview.plan.totals.to_upload_emoji,
        preview.plan.totals.people,
        preview.plan.totals.people_with_email,
    );
    if invite_by_email {
        // Never invite a real address, even on the dev DS.
        for p in &preview.plan.people {
            if let Some(e) = &p.email {
                if !e.ends_with("@pollis.test") {
                    bail!("refusing to invite {e:?}: only @pollis.test addresses");
                }
            }
        }
    }
    let options = Some(ImportApplyOptions { include_archived: false, invite_by_email });
    let target = preview.target.as_ref().map(|t| t.group_id.clone()).or(Some(import::TARGET_NEW.to_string()));
    let first = import::import_apply(source, path.clone(), me.id.clone(), target, options.clone(), &state).await?;
    report("FIRST", &first);

    let again = import::import_preview(source, path.clone(), me.id.clone(), None, None, &state).await?;
    let Some(t) = again.target.clone() else {
        bail!("the re-run did not find the imported group");
    };
    if t.group_id != first.group_id {
        bail!("the re-run picked another group");
    }
    let second = import::import_apply(source, path.clone(), me.id.clone(), Some(t.group_id.clone()), options, &state).await?;
    report("SECOND", &second);
    if second.categories_created + second.channels_created + second.emoji_uploaded > 0 || second.invite_link.is_some() {
        bail!("the second run created something");
    }
    if second.email_invites.iter().any(|i| i.outcome == EmailInviteOutcome::Invited) {
        bail!("the second run invited someone again");
    }

    // Read the group back from the DS.
    let all = groups::list_user_groups_with_channels(me.id.clone(), &state).await?;
    let g = all.iter().find(|g| g.id == first.group_id).context("imported group")?;
    let names: std::collections::BTreeMap<_, _> = g.categories.iter().map(|k| (k.id.as_str(), k.name.as_str())).collect();
    let layout: Vec<String> = g
        .channels
        .iter()
        .map(|c| {
            let voice = if c.channel_type == "voice" { " (voice)" } else { "" };
            match c.category_id.as_deref().and_then(|id| names.get(id)) {
                Some(k) => format!("{k} / {}{voice}", c.name),
                None => format!("{}{voice}", c.name),
            }
        })
        .collect();
    let planned: Vec<String> = preview
        .plan
        .channels
        .iter()
        .map(|c| {
            let voice = if c.kind.as_str() == "voice" { " (voice)" } else { "" };
            match &c.category {
                Some(k) => format!("{k} / {}{voice}", c.name),
                None => format!("{}{voice}", c.name),
            }
        })
        .collect();
    println!("CATEGORIES {:?}", g.categories.iter().map(|k| k.name.as_str()).collect::<Vec<_>>());
    for (line, c) in layout.iter().zip(&g.channels) {
        let desc = c.description.as_deref().map(|d| format!(" — {}", d.replace('\n', " / "))).unwrap_or_default();
        println!("CHANNEL {line}{desc}");
    }
    if preview.target.is_none() && layout != planned {
        bail!("the group's order differs from the plan");
    }
    let emoji = emoji::list_group_emoji(g.id.clone(), me.id.clone(), &state).await?;
    for e in &emoji {
        println!("EMOJI :{}: {} {} bytes animated={}", e.shortcode, e.content_type, e.size_bytes, e.animated);
    }
    let links = groups::list_group_invite_links(g.id.clone(), me.id.clone(), &state).await?;
    println!("INVITE_LINKS live={}", links.iter().filter(|l| l.is_live).count());
    println!("GROUP_ID={}", g.id);
    println!("DONE");
    Ok(())
}

fn report(label: &str, r: &import::ImportResult) {
    println!(
        "{label} group={} created_group={} icon={} +{} categories +{} channels (moved {}) +{} emoji; existing {}/{}/{}; invite_link={} existing_link={}",
        r.group_name,
        r.created_group,
        r.icon_set,
        r.categories_created,
        r.channels_created,
        r.channels_moved,
        r.emoji_uploaded,
        r.categories_existing,
        r.channels_existing,
        r.emoji_existing,
        r.invite_link.is_some(),
        r.invite_link_existing
    );
    for i in &r.email_invites {
        println!("{label} EMAIL_INVITE {} <{}> {:?}", i.username, i.email, i.outcome);
    }
    for f in &r.failures {
        println!("{label} FAILURE {} {}: {}", f.kind, f.name, f.error);
    }
}
