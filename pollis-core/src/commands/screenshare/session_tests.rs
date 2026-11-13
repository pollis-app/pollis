//! A share or camera whose capture dies mid-session must end through the stop path and
//! say why; a reader that outlived its session must not touch the next one.

use std::sync::{Arc, Mutex};

use crate::{sink::EventSink, state::AppState};

use super::{HelperSession, ScreenShareEvent};

pub(crate) struct CapturingSink<T>(pub Arc<Mutex<Vec<T>>>);

impl<T: Send + Sync + Clone> EventSink<T> for CapturingSink<T> {
    fn send(&self, event: T) -> Result<(), String> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
}

pub(crate) fn test_state() -> Arc<AppState> {
    Arc::new(AppState::new_with_parts(
        crate::config::Config::for_test().expect("test config"),
        Arc::new(crate::keystore::InMemoryKeystore::new()),
    ))
}

/// A parked session with a real child process, so a kill is observable.
pub(crate) async fn fake_helper_session() -> HelperSession {
    let child = tokio::process::Command::new("sleep")
        .arg("60")
        .kill_on_drop(true)
        .spawn()
        .expect("spawn sleep");
    let (a, _b) = tokio::net::UnixStream::pair().expect("socketpair");
    // The far end is dropped on purpose: the reader half is never read in these tests.
    let (read_half, write_half) = a.into_split();
    HelperSession {
        child,
        reader: tokio::io::BufReader::new(read_half),
        writer: write_half,
    }
}

pub(crate) async fn wait_until<F: Fn() -> bool>(what: &str, f: F) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !f() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting: {what}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

async fn park_share(state: &Arc<AppState>) -> (u64, u32) {
    let session = fake_helper_session().await;
    let pid = session.child.id().expect("child pid");
    let mut ss = state.screenshare.lock().await;
    ss.local_helper = Some(session.child);
    ss.local_writer = Some(session.writer);
    ss.local_reader_task = Some(tokio::spawn(std::future::pending()));
    (ss.begin_session(), pid)
}

fn pid_alive(pid: u32) -> bool {
    // A killed child stays a zombie until reaped; `kill -0` on a zombie still succeeds,
    // so read the process state letter instead: `ps` prints it on Linux and macOS
    // alike (`/proc` exists only on Linux), and prints nothing once the pid is gone.
    std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .map(|out| {
            let stat = String::from_utf8_lossy(&out.stdout);
            let stat = stat.trim();
            !stat.is_empty() && !stat.starts_with('Z')
        })
        .unwrap_or(false)
}

#[tokio::test]
async fn a_dead_reader_ends_the_share_and_says_why() {
    let state = test_state();
    let events = Arc::new(Mutex::new(Vec::new()));
    state.screenshare.lock().await.events = Some(Arc::new(CapturingSink(Arc::clone(&events))));
    let (token, pid) = park_share(&state).await;
    assert!(pid_alive(pid));

    super::start_unix::on_reader_exit(Arc::clone(&state), token, "source ended".into());

    wait_until("stop + error events", || events.lock().unwrap().len() >= 2).await;
    let got = events.lock().unwrap().clone();
    assert!(
        matches!(got[0], ScreenShareEvent::LocalStopped),
        "the share must be gone before the renderer hears why: {got:?}"
    );
    assert!(
        matches!(&got[1], ScreenShareEvent::LocalError { message } if message == "source ended"),
        "{got:?}"
    );
    let ss = state.screenshare.lock().await;
    assert!(
        ss.local_helper.is_none() && ss.local_writer.is_none() && ss.local_reader_task.is_none()
    );
    assert!(!ss.is_session(token));
    drop(ss);
    wait_until("helper killed", || !pid_alive(pid)).await;
}

#[tokio::test]
async fn a_reader_from_an_earlier_session_cannot_end_the_current_one() {
    let state = test_state();
    let events = Arc::new(Mutex::new(Vec::new()));
    state.screenshare.lock().await.events = Some(Arc::new(CapturingSink(Arc::clone(&events))));

    let (old_token, _) = park_share(&state).await;
    super::stop_screen_share(&state).await.unwrap();
    assert!(matches!(
        events.lock().unwrap().as_slice(),
        [ScreenShareEvent::LocalStopped]
    ));
    events.lock().unwrap().clear();
    let (new_token, new_pid) = park_share(&state).await;

    super::start_unix::on_reader_exit(Arc::clone(&state), old_token, "stale".into());
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let ss = state.screenshare.lock().await;
    assert!(
        ss.is_session(new_token),
        "the new session must still be live"
    );
    assert!(ss.local_helper.is_some());
    assert!(
        events.lock().unwrap().is_empty(),
        "{:?}",
        events.lock().unwrap()
    );
    assert!(pid_alive(new_pid));
}

#[tokio::test]
async fn a_stop_during_the_handshake_invalidates_the_start_token() {
    let state = test_state();
    let (token, _) = park_share(&state).await;
    assert!(state.screenshare.lock().await.is_session(token));

    super::stop_screen_share(&state).await.unwrap();
    assert!(
        !state.screenshare.lock().await.is_session(token),
        "the start that is still awaiting the helper must see its session as ended"
    );

    // Nothing live: a no-op stop must not disturb a token issued afterwards.
    let later = state.screenshare.lock().await.begin_session();
    super::stop_screen_share(&state).await.unwrap();
    assert!(state.screenshare.lock().await.is_session(later));
}

mod camera {
    use std::sync::{Arc, Mutex};

    use super::{fake_helper_session, pid_alive, test_state, wait_until, CapturingSink};
    use crate::commands::camera::{self, CameraEvent};
    use crate::state::AppState;

    async fn park_camera(state: &Arc<AppState>) -> (u64, u32) {
        let session = fake_helper_session().await;
        let pid = session.child.id().expect("child pid");
        let mut cam = state.camera.lock().await;
        cam.local_helper = Some(session.child);
        cam.local_writer = Some(session.writer);
        cam.local_reader_task = Some(tokio::spawn(std::future::pending()));
        (cam.begin_session(), pid)
    }

    #[tokio::test]
    async fn a_dead_camera_reader_ends_the_capture_and_says_why() {
        let state = test_state();
        let events = Arc::new(Mutex::new(Vec::new()));
        state.camera.lock().await.events = Some(Arc::new(CapturingSink(Arc::clone(&events))));
        let (token, pid) = park_camera(&state).await;

        camera::on_reader_exit_for_tests(Arc::clone(&state), token, "device went away".into());

        wait_until("stop + error events", || events.lock().unwrap().len() >= 2).await;
        let got = events.lock().unwrap().clone();
        assert!(matches!(got[0], CameraEvent::LocalStopped), "{got:?}");
        assert!(
            matches!(&got[1], CameraEvent::LocalError { message } if message == "device went away"),
            "{got:?}"
        );
        let cam = state.camera.lock().await;
        assert!(cam.local_helper.is_none() && cam.local_reader_task.is_none());
        assert!(!cam.is_session(token));
        drop(cam);
        wait_until("helper killed", || !pid_alive(pid)).await;
    }

    #[tokio::test]
    async fn a_stale_camera_reader_cannot_end_the_current_capture() {
        let state = test_state();
        let events = Arc::new(Mutex::new(Vec::new()));
        state.camera.lock().await.events = Some(Arc::new(CapturingSink(Arc::clone(&events))));
        let (old_token, _) = park_camera(&state).await;
        camera::stop_camera(&state).await.unwrap();
        events.lock().unwrap().clear();
        let (new_token, new_pid) = park_camera(&state).await;

        camera::on_reader_exit_for_tests(Arc::clone(&state), old_token, "stale".into());
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let cam = state.camera.lock().await;
        assert!(cam.is_session(new_token));
        assert!(cam.local_helper.is_some());
        assert!(events.lock().unwrap().is_empty());
        assert!(pid_alive(new_pid));
    }
}
