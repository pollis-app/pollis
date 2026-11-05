//! Join-path invariants that need no SFU: the re-entry guard and the leave-cancels-join contract.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::config::Config;
use crate::error::Error;
use crate::keystore::InMemoryKeystore;
use crate::state::AppState;

use super::join::{JoinCancel, VoiceJoinError};
use super::lifecycle::{join_voice_channel, leave_voice_channel};

fn state() -> Arc<AppState> {
    Arc::new(AppState::new_with_parts(
        Config::for_test().expect("test config"),
        Arc::new(InMemoryKeystore::new()),
    ))
}

async fn join(state: &Arc<AppState>) -> Result<(), Error> {
    join_voice_channel(
        "chan-1".into(),
        "u1".into(),
        "U1".into(),
        None,
        None,
        crate::commands::voice_apm::ApmConfig::default(),
        None,
        state,
    )
    .await
}

/// Two same-identity joins would trip DuplicateIdentity on the SFU; the second is refused before any I/O.
#[tokio::test]
async fn a_second_join_while_one_is_in_flight_is_refused_typed() {
    let state = state();
    state
        .voice
        .lock()
        .await
        .joining
        .store(true, Ordering::Release);
    match join(&state).await {
        Err(Error::VoiceJoin(VoiceJoinError::JoinInProgress)) => {}
        other => panic!("expected JoinInProgress, got {other:?}"),
    }
    // The refused join must not clear the flag the live join owns.
    assert!(state.voice.lock().await.joining.load(Ordering::Acquire));
}

/// `leave_voice_channel` during a join raises the join's cancel flag, so the join
/// stops at its next phase instead of finishing into a room nobody wants.
#[tokio::test]
async fn leaving_during_a_join_cancels_it() {
    let state = state();
    let cancel = Arc::new(JoinCancel::default());
    {
        let mut voice = state.voice.lock().await;
        voice.joining.store(true, Ordering::Release);
        voice.join_cancel = Some(Arc::clone(&cancel));
    }
    assert!(!cancel.is_cancelled());
    leave_voice_channel(&state)
        .await
        .expect("leave with no room is a no-op");
    assert!(
        cancel.is_cancelled(),
        "leave must cancel the in-flight join"
    );
    assert!(state.voice.lock().await.room.is_none());
}

/// A leave with no join in flight leaves no stale cancel behind for the next join to trip on.
#[tokio::test]
async fn a_leave_outside_a_join_cancels_nothing() {
    let state = state();
    let cancel = Arc::new(JoinCancel::default());
    state.voice.lock().await.join_cancel = Some(Arc::clone(&cancel));
    leave_voice_channel(&state).await.expect("leave");
    assert!(!cancel.is_cancelled());
}
