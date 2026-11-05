//! Join bookkeeping: the deadline/cancellation clock every join phase runs under,
//! error classification into [`VoiceJoinError`], and the in-session reconnect policy.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use livekit::DisconnectReason;
use livekit::RoomError;
use tokio::sync::Notify;

pub use crate::commands::voice_join_error::VoiceJoinError;

/// Whole-join budget. Generous: a cold join is ~1.5 s, a stalled DS or SFU is minutes.
pub const VOICE_JOIN_DEADLINE: Duration = Duration::from_secs(20);

/// How long a dropped session keeps trying to re-mint and re-connect before it
/// gives up and reports `Disconnected`. Starts at the SDK's own give-up, so the
/// total tolerated outage is this plus the SDK's ~45 s.
pub const VOICE_RECONNECT_WINDOW: Duration = Duration::from_secs(60);

/// Cancellation flag a `leave_voice_channel` raises while a join is in flight.
#[derive(Default)]
pub struct JoinCancel {
    flag: AtomicBool,
    notify: Notify,
}

impl JoinCancel {
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }
}

/// Runs each join phase under the shared deadline and the cancel flag.
pub struct JoinClock {
    deadline: Instant,
    cancel: Arc<JoinCancel>,
}

impl JoinClock {
    pub fn new(cancel: Arc<JoinCancel>, budget: Duration) -> Self {
        Self {
            deadline: Instant::now() + budget,
            cancel,
        }
    }

    /// Await `fut`, or stop early with `Cancelled` / `Deadline { phase }`.
    ///
    /// A cancelled phase's future is dropped; the caller still owns everything
    /// earlier phases produced and tears it down on the error path.
    pub async fn run<T, F>(&self, phase: &'static str, fut: F) -> Result<T, VoiceJoinError>
    where
        F: Future<Output = T>,
    {
        if self.cancel.is_cancelled() {
            return Err(VoiceJoinError::Cancelled);
        }
        // Registered before the flag re-check so a cancel between the two is not lost.
        let cancelled = self.cancel.notify.notified();
        tokio::pin!(cancelled);
        cancelled.as_mut().enable();
        if self.cancel.is_cancelled() {
            return Err(VoiceJoinError::Cancelled);
        }
        tokio::select! {
            biased;
            _ = &mut cancelled => Err(VoiceJoinError::Cancelled),
            _ = tokio::time::sleep_until(self.deadline.into()) => {
                Err(VoiceJoinError::Deadline { phase: phase.to_string() })
            }
            out = fut => Ok(out),
        }
    }
}

/// Map a failed DS call on the join path to the kind the renderer can act on.
pub fn classify_ds_error(e: &crate::error::Error) -> VoiceJoinError {
    use crate::error::Error;
    match e {
        Error::VoiceJoin(v) => v.clone(),
        Error::Network(_) => VoiceJoinError::DsUnreachable {
            detail: e.to_string(),
        },
        Error::Other(inner) => {
            if let Some(refused) =
                inner.downcast_ref::<crate::commands::mls::ds_client::DsRefused>()
            {
                return match refused.status.as_u16() {
                    401 | 403 => VoiceJoinError::NotMember {
                        detail: refused.to_string(),
                    },
                    _ => VoiceJoinError::DsUnreachable {
                        detail: refused.to_string(),
                    },
                };
            }
            let text = inner.to_string();
            if text.contains("MLS group not found") || text.contains("no 1:1 DM channel") {
                VoiceJoinError::NoMlsGroup { detail: text }
            } else if text.contains("error sending request")
                || text.contains("connection refused")
                || text.contains("timed out")
            {
                VoiceJoinError::DsUnreachable { detail: text }
            } else {
                VoiceJoinError::Internal { detail: text }
            }
        }
        other => VoiceJoinError::Internal {
            detail: other.to_string(),
        },
    }
}

/// Map a failed `Room::connect` to the kind the renderer can act on.
pub fn classify_connect_error(e: &RoomError) -> VoiceJoinError {
    use livekit::rtc_engine::EngineError;
    use livekit_signaling::SignalError;
    let detail = format!("LiveKit connect: {e}");
    match e {
        RoomError::Engine(EngineError::Signal(SignalError::Client(status, _)))
            if matches!(status.as_u16(), 401 | 403) =>
        {
            VoiceJoinError::TokenRejected { detail }
        }
        RoomError::Engine(EngineError::Signal(_))
        | RoomError::Engine(EngineError::Connection(_)) => {
            VoiceJoinError::LivekitUnreachable { detail }
        }
        _ => VoiceJoinError::Internal { detail },
    }
}

/// Whether a server-side disconnect is worth re-minting a token and connecting again for.
///
/// Anything that names us (duplicate identity, removed, rejected) or the room (deleted,
/// closed) is final; everything else is the link or the server, which comes back.
pub fn disconnect_is_retryable(reason: DisconnectReason) -> bool {
    !matches!(
        reason,
        DisconnectReason::ClientInitiated
            | DisconnectReason::DuplicateIdentity
            | DisconnectReason::ParticipantRemoved
            | DisconnectReason::RoomDeleted
            | DisconnectReason::RoomClosed
            | DisconnectReason::UserRejected
            | DisconnectReason::UserUnavailable
            | DisconnectReason::SipTrunkFailure
            | DisconnectReason::AgentError
    )
}

/// Delay before reconnect attempt `attempt` (1-based): 0.5 s doubling, capped at 8 s.
pub fn reconnect_delay(attempt: u32) -> Duration {
    let ms = 500u64.saturating_mul(1u64 << attempt.saturating_sub(1).min(4));
    Duration::from_millis(ms.min(8_000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_phase_that_finishes_in_time_passes_its_value_through() {
        let clock = JoinClock::new(Arc::new(JoinCancel::default()), Duration::from_secs(5));
        let got = clock.run("connect", async { 7 }).await.unwrap();
        assert_eq!(got, 7);
    }

    #[tokio::test]
    async fn a_phase_that_overruns_the_deadline_fails_as_deadline_naming_the_phase() {
        let clock = JoinClock::new(Arc::new(JoinCancel::default()), Duration::from_millis(50));
        let got = clock
            .run("connect", async {
                tokio::time::sleep(Duration::from_secs(30)).await;
            })
            .await;
        assert_eq!(
            got.unwrap_err(),
            VoiceJoinError::Deadline {
                phase: "connect".into()
            }
        );
    }

    #[tokio::test]
    async fn a_cancel_raised_mid_phase_aborts_it() {
        let cancel = Arc::new(JoinCancel::default());
        let clock = JoinClock::new(Arc::clone(&cancel), Duration::from_secs(20));
        let canceller = Arc::clone(&cancel);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            canceller.cancel();
        });
        let got = clock
            .run("mic", async {
                tokio::time::sleep(Duration::from_secs(30)).await;
            })
            .await;
        assert_eq!(got.unwrap_err(), VoiceJoinError::Cancelled);
    }

    #[tokio::test]
    async fn a_cancel_raised_between_phases_stops_the_next_one_before_it_starts() {
        let cancel = Arc::new(JoinCancel::default());
        let clock = JoinClock::new(Arc::clone(&cancel), Duration::from_secs(20));
        cancel.cancel();
        let ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran);
        let got = clock
            .run("publish", async move { flag.store(true, Ordering::SeqCst) })
            .await;
        assert_eq!(got.unwrap_err(), VoiceJoinError::Cancelled);
        assert!(
            !ran.load(Ordering::SeqCst),
            "the phase must not start after a cancel"
        );
    }

    #[test]
    fn only_link_and_server_failures_are_retried() {
        for retry in [
            DisconnectReason::UnknownReason,
            DisconnectReason::ServerShutdown,
            DisconnectReason::StateMismatch,
            DisconnectReason::JoinFailure,
            DisconnectReason::Migration,
            DisconnectReason::SignalClose,
            DisconnectReason::ConnectionTimeout,
            DisconnectReason::MediaFailure,
        ] {
            assert!(disconnect_is_retryable(retry), "{retry:?}");
        }
        for stop in [
            DisconnectReason::ClientInitiated,
            DisconnectReason::DuplicateIdentity,
            DisconnectReason::ParticipantRemoved,
            DisconnectReason::RoomDeleted,
            DisconnectReason::RoomClosed,
        ] {
            assert!(!disconnect_is_retryable(stop), "{stop:?}");
        }
    }

    #[test]
    fn reconnect_backoff_doubles_from_half_a_second_and_caps() {
        assert_eq!(reconnect_delay(1), Duration::from_millis(500));
        assert_eq!(reconnect_delay(2), Duration::from_millis(1000));
        assert_eq!(reconnect_delay(5), Duration::from_millis(8000));
        assert_eq!(reconnect_delay(40), Duration::from_millis(8000));
    }

    #[test]
    fn a_rejected_token_is_told_apart_from_an_unreachable_sfu() {
        use livekit::rtc_engine::EngineError;
        use livekit_signaling::SignalError;
        let rejected = RoomError::Engine(EngineError::Signal(SignalError::Client(
            reqwest::StatusCode::UNAUTHORIZED,
            "invalid token".into(),
        )));
        assert!(matches!(
            classify_connect_error(&rejected),
            VoiceJoinError::TokenRejected { .. }
        ));
        let down = RoomError::Engine(EngineError::Signal(SignalError::Timeout("validate".into())));
        assert!(matches!(
            classify_connect_error(&down),
            VoiceJoinError::LivekitUnreachable { .. }
        ));
        let other = RoomError::Internal("x".into());
        assert!(matches!(
            classify_connect_error(&other),
            VoiceJoinError::Internal { .. }
        ));
    }

    #[test]
    fn a_403_from_the_ds_is_not_member_and_a_missing_group_is_no_mls_group() {
        let refused = crate::commands::mls::ds_client::DsRefused {
            label: "ds_post".into(),
            path: "/v1/livekit/token".into(),
            status: reqwest::StatusCode::FORBIDDEN,
            body: String::new(),
        };
        let e = crate::error::Error::Other(anyhow::Error::new(refused));
        assert!(matches!(
            classify_ds_error(&e),
            VoiceJoinError::NotMember { .. }
        ));
        let e =
            crate::error::Error::Other(anyhow::anyhow!("MLS group not found for voice channel c1"));
        assert!(matches!(
            classify_ds_error(&e),
            VoiceJoinError::NoMlsGroup { .. }
        ));
        let e = crate::error::Error::Other(anyhow::anyhow!("error sending request for url (…)"));
        assert!(matches!(
            classify_ds_error(&e),
            VoiceJoinError::DsUnreachable { .. }
        ));
    }
}
