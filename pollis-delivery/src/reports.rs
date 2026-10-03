//! Abuse reports (#1213).
//!
//! `POST /v1/reports` records that one account reported another, with a reason
//! and optionally the conversation and message ids, and tells the operator by
//! email. Messages are end-to-end encrypted, so no content ever arrives here:
//! the wire type has no field for it and `user_report` has no column for it.
//! The operator acts on accounts and on patterns (several unrelated reporters,
//! a new account messaging many strangers), and the one action available is
//! account suspension (`account_suspension`, enforced in `auth.rs`).
//!
//! Shape as in [`crate::profile`]: a pure `apply_*` on a bare connection holds
//! the authorization and the write, so the flows harness and the axum handler
//! run the same checks.

use axum::{extract::State, response::IntoResponse, response::Response};
use libsql::Connection;
use pollis_api::reports::ReportUserBody;

use crate::error::{AppError, AuthRejection};
use crate::writes::{gate_and_parse, ok_response, resolve_actor, RawRequest, WriteOutcome};
use crate::AppState;

/// Reports one account may file in a rolling 24 hours. Generous for a real
/// person dealing with a harassment campaign; small enough that a script
/// cannot flood the operator's inbox.
pub const REPORTS_PER_DAY: i64 = 20;

/// What [`apply_report_user`] decided.
#[derive(Debug, PartialEq, Eq)]
pub enum ReportOutcome {
    /// Stored. The fields are what the operator email needs.
    Stored {
        report_id: String,
        reporter_id: String,
        prior_reports_against: i64,
    },
    /// Not allowed: reporting yourself, filing as someone else, or pointing at
    /// a conversation the reporter is not in.
    Forbidden,
    /// The reported account does not exist.
    UnknownAccount,
    /// The reporter is over [`REPORTS_PER_DAY`].
    RateLimited,
}

// ── POST /v1/reports ─────────────────────────────────────────────────────────

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
            // Telling the operator never holds up or fails the report: it is
            // stored either way, and the inbox is a convenience over the table.
            if let Some(key) = state.otp_config.resend_api_key.clone() {
                let notice = ReportNotice {
                    report_id,
                    reporter_id,
                    reported_id: parsed.reported_id.clone(),
                    reason: parsed.reason.as_str(),
                    conversation_id: parsed.conversation_id.clone(),
                    message_id: parsed.message_id.clone(),
                    prior_reports_against,
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

/// Authorize and store one report. The reporter is the authenticated user; the
/// table's CHECKs refuse a self-report and a message id without a conversation
/// even if a check here were ever removed.
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
    // A conversation, when given, must be one the reporter is in: you can
    // point at where it happened, not at somebody else's conversation.
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

/// What the operator email says. Ids only, the same as the stored row.
struct ReportNotice {
    report_id: String,
    reporter_id: String,
    reported_id: String,
    reason: &'static str,
    conversation_id: Option<String>,
    message_id: Option<String>,
    prior_reports_against: i64,
}

async fn send_report_notice(api_key: &str, n: &ReportNotice) -> anyhow::Result<()> {
    let body = serde_json::json!({
        "from": "Pollis <noreply@mail.pollis.com>",
        "to": ["support@pollis.com"],
        "subject": format!("Pollis abuse report: {} ({})", n.reported_id, n.reason),
        "text": format!(
            "Report {report}\n\n\
             Reported account: {reported}\n\
             Reason: {reason}\n\
             Reported by: {reporter}\n\
             Conversation: {conversation}\n\
             Message: {message}\n\
             Other accounts that reported this one before: {prior}\n\n\
             No message content is included or available.\n\
             To suspend the account: scripts/suspend-account.sh {reported} \"<reason>\"",
            report = n.report_id,
            reported = n.reported_id,
            reason = n.reason,
            reporter = n.reporter_id,
            conversation = n.conversation_id.as_deref().unwrap_or("none"),
            message = n.message_id.as_deref().unwrap_or("none"),
            prior = n.prior_reports_against,
        ),
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
