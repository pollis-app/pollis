//! Join-path invariants that need no SFU: the re-entry guard and the leave-cancels-join contract.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::config::Config;
use crate::error::Error;
use crate::keystore::InMemoryKeystore;
use crate::state::AppState;

use super::join::{JoinCancel, VoiceJoinError};
use super::lifecycle::{begin_warm, join_voice_channel, leave_voice_channel, warm_inflight};

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

/// One warm per channel at a time; a join that arrives mid-warm can wait on it, and
/// the guard's drop wakes that waiter whichever way the warm ended.
#[tokio::test]
async fn a_warm_in_flight_is_visible_to_a_join_and_released_when_it_ends() {
    let guard = begin_warm("warm-chan").expect("first warm starts");
    assert!(
        begin_warm("warm-chan").is_none(),
        "a second warm for the same channel is a no-op"
    );
    assert!(
        begin_warm("other-chan").is_some(),
        "other channels are independent"
    );
    let inflight = warm_inflight("warm-chan").expect("visible while in flight");
    let waiter = tokio::spawn(async move { inflight.notified().await });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    drop(guard);
    tokio::time::timeout(std::time::Duration::from_secs(2), waiter)
        .await
        .expect("the waiter is woken by the guard's drop")
        .unwrap();
    assert!(warm_inflight("warm-chan").is_none());
}
