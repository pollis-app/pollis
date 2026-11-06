//! Abuse reports (#1213).
//!
//! No message content arrives here: neither the wire type nor `user_report` has a field for it.

use axum::{extract::State, response::IntoResponse, response::Response};
use libsql::Connection;
use pollis_api::reports::ReportUserBody;

use crate::error::{AppError, AuthRejection};
use crate::writes::{gate_and_parse, ok_response, resolve_actor, RawRequest, WriteOutcome};
use crate::AppState;

/// Reports one account may file in a rolling 24 hours.
pub const REPORTS_PER_DAY: i64 = 20;

/// What [`apply_report_user`] decided.
#[derive(Debug, PartialEq, Eq)]
pub enum ReportOutcome {
    Stored {
        report_id: String,
        reporter_id: String,
        prior_reports_against: i64,
    },
    /// Not allowed: reporting yourself, filing as someone else, or pointing at
    /// a conversation the reporter is not in.
    Forbidden,
    UnknownAccount,
    RateLimited,
}

pub async fn report_user(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<ReportUserBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    match apply_report_user(&conn, authed.as_deref(), &parsed).await? {
        ReportOutcome::Stored {
            report_id,
            reporter_id,
            prior_reports_against,
        } => {
            // Notifying the operator never fails the report.
            if let Some(key) = state.otp_config.resend_api_key.clone() {
                let notice = ReportNotice {
                    report_id,
                    reporter_id,
                    reported_id: parsed.reported_id.clone(),
                    reason: parsed.reason.as_str(),
                    conversation_id: parsed.conversation_id.clone(),
                    message_id: parsed.message_id.clone(),
                    prior_reports_against,
                    dev: state.otp_config.dev_otp.is_some(),
                };
                tokio::spawn(async move {
                    if let Err(e) = send_report_notice(&key, &notice).await {
                        tracing::warn!(error = %e, report = %notice.report_id, "report: operator email failed");
                    }
                });
            }
            Ok(ok_response::<ReportUserBody>(pollis_api::StatusOk::Ok))
        }
        ReportOutcome::Forbidden => Ok(AuthRejection::Forbidden.into_response()),
        ReportOutcome::UnknownAccount => Ok(crate::writes::bad_request("unknown account")),
        ReportOutcome::RateLimited => Ok(crate::ratelimit::too_many_requests()),
    }
}

/// Authorize and store one report; the table's CHECKs back up the self-report rules.
pub async fn apply_report_user(
    conn: &Connection,
    authed: Option<&str>,
    body: &ReportUserBody,
) -> anyhow::Result<ReportOutcome> {
    let reporter = match resolve_actor(authed, Some(body.reporter_id.as_str())) {
        Ok(r) => r,
        Err(WriteOutcome::Forbidden) => return Ok(ReportOutcome::Forbidden),
        Err(_) => return Ok(ReportOutcome::Forbidden),
    };
    if reporter == body.reported_id {
        return Ok(ReportOutcome::Forbidden);
    }
    if body.message_id.is_some() && body.conversation_id.is_none() {
        return Ok(ReportOutcome::Forbidden);
    }
    // A given conversation must be one the reporter is in.
    if let Some(conversation_id) = &body.conversation_id {
        if !crate::writes::is_member(conn, conversation_id, &reporter).await? {
            return Ok(ReportOutcome::Forbidden);
        }
    }
    let mut rows = conn
        .query("SELECT 1 FROM users WHERE id = ?1", libsql::params![body.reported_id.clone()])
        .await?;
    if rows.next().await?.is_none() {
        return Ok(ReportOutcome::UnknownAccount);
    }
    // A shadowed `rows` is not dropped: its unfinished SELECT would hold a read
    // snapshot into the INSERT, which then fails `database is locked` if another
    // connection wrote in between (#1618). Drop each one before writing.
    drop(rows);
    let recent = count(
        conn,
        "SELECT COUNT(*) FROM user_report \
          WHERE reporter_id = ?1 AND created_at > datetime('now', '-1 day')",
        &reporter,
    )
    .await?;
    if recent >= REPORTS_PER_DAY {
        return Ok(ReportOutcome::RateLimited);
    }
    let mut rows = conn
        .query(
            "SELECT COUNT(DISTINCT reporter_id) FROM user_report \
              WHERE reported_id = ?1 AND reporter_id <> ?2",
            libsql::params![body.reported_id.clone(), reporter.clone()],
        )
        .await?;
    let prior_reports_against: i64 = match rows.next().await? {
        Some(row) => row.get(0)?,
        None => 0,
    };
    drop(rows);
    let report_id = ulid::Ulid::new().to_string();
    conn.execute(
        "INSERT INTO user_report (id, reporter_id, reported_id, reason, conversation_id, message_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        libsql::params![
            report_id.clone(),
            reporter.clone(),
            body.reported_id.clone(),
            body.reason.as_str(),
            body.conversation_id.clone(),
            body.message_id.clone()
        ],
    )
    .await?;
    Ok(ReportOutcome::Stored {
        report_id,
        reporter_id: reporter,
        prior_reports_against,
    })
}

async fn count(conn: &Connection, sql: &str, id: &str) -> anyhow::Result<i64> {
    let mut rows = conn.query(sql, libsql::params![id.to_string()]).await?;
    Ok(match rows.next().await? {
        Some(row) => row.get::<i64>(0)?,
        None => 0,
    })
}

/// What the operator email says: ids only.
struct ReportNotice {
    report_id: String,
    reporter_id: String,
    reported_id: String,
    reason: &'static str,
    conversation_id: Option<String>,
    message_id: Option<String>,
    prior_reports_against: i64,
    /// Dev and prod mail the same inbox; a DS that accepts `DEV_OTP` is never
    /// prod (the prod deploy never syncs it), so that is what marks dev.
    dev: bool,
}

/// Subject and body of the operator email.
fn report_email(n: &ReportNotice) -> (String, String) {
    let (tag, env, doppler) = if n.dev {
        ("[dev] ", "Environment: dev (these accounts are in the dev database, not prod)\n\n", "DOPPLER_CONFIG=dev ")
    } else {
        ("", "", "")
    };
    let subject = format!("{tag}Pollis abuse report: {} ({})", n.reported_id, n.reason);
    let text = format!(
        "{env}Report {report}\n\n\
         Reported account: {reported}\n\
         Reason: {reason}\n\
         Reported by: {reporter}\n\
         Conversation: {conversation}\n\
         Message: {message}\n\
         Other accounts that reported this one before: {prior}\n\n\
         No message content is included or available.\n\
         To suspend the account: {doppler}scripts/suspend-account.sh {reported} \"<reason>\"",
        report = n.report_id,
        reported = n.reported_id,
        reason = n.reason,
        reporter = n.reporter_id,
        conversation = n.conversation_id.as_deref().unwrap_or("none"),
        message = n.message_id.as_deref().unwrap_or("none"),
        prior = n.prior_reports_against,
    );
    (subject, text)
}

async fn send_report_notice(api_key: &str, n: &ReportNotice) -> anyhow::Result<()> {
    let (subject, text) = report_email(n);
    let body = serde_json::json!({
        "from": "Pollis <noreply@mail.pollis.com>",
        "to": ["support@pollis.com"],
        "subject": subject,
        "text": text,
    });
    let resp = crate::util::http_post(crate::util::Upstream::Resend, "https://api.resend.com/emails")
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let txt = resp.text().await.unwrap_or_default();
        anyhow::bail!("Resend non-success: {txt}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(dev: bool) -> ReportNotice {
        ReportNotice {
            report_id: "R".into(),
            reporter_id: "A".into(),
            reported_id: "B".into(),
            reason: "spam",
            conversation_id: None,
            message_id: None,
            prior_reports_against: 0,
            dev,
        }
    }

    #[test]
    fn dev_report_email_is_labelled_and_points_the_script_at_dev() {
        let (subject, text) = report_email(&notice(true));
        assert!(subject.starts_with("[dev] "));
        assert!(text.starts_with("Environment: dev"));
        assert!(text.contains("DOPPLER_CONFIG=dev scripts/suspend-account.sh B \"<reason>\""));
    }

    #[test]
    fn prod_report_email_is_unlabelled_and_targets_prod() {
        let (subject, text) = report_email(&notice(false));
        assert_eq!(subject, "Pollis abuse report: B (spam)");
        assert!(!text.contains("dev"));
        assert!(text.contains("\nTo suspend the account: scripts/suspend-account.sh B \"<reason>\""));
    }
}
