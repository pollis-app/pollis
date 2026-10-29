//! #1337: every message shape the other clients produce, rendered on the REAL
//! UI, and every message key (reply / edit / delete / react / thread) driven
//! through it — two clients over the in-process Delivery Service.
//!
//! Bob writes through `pollis_tui::send` with the fields the other clients set; Alice uses only
//! keystrokes and every assertion is on her rendered screen.
//!
//! One `#[tokio::test]` per file: the data dir is process-global (see the
//! crate wiki, "every test file here holds exactly one").

mod common;

use std::time::Duration;

use common::{spawn_world, Driver};
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::style::Color;

/// Hang guards, not latency assertions — see `ui_e2e.rs`.
const SURFACE_TIMEOUT: Duration = Duration::from_secs(120);
const SIDEBAR_TIMEOUT: Duration = Duration::from_secs(60);

async fn press(d: &mut Driver, code: KeyCode) {
    d.press(code, KeyModifiers::NONE).await;
}

async fn key(d: &mut Driver, c: char) {
    d.press(KeyCode::Char(c), KeyModifiers::NONE).await;
}

/// The rendered line containing `needle`.
fn line_with<'a>(screen: &'a str, needle: &str) -> &'a str {
    screen
        .lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} not on screen:\n{screen}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn other_clients_message_shapes_render_and_message_keys_work() {
    let world = spawn_world().await;
    let mut alice = Driver::new(&world, "alice-device");
    let mut bob = Driver::new(&world, "bob-device");
    alice.signup_dev("alice@e2e.local").await;
    bob.signup_dev("bob@e2e.local").await;

    // The key hints are on screen from Home's first frame.
    let first = alice.buffer_text();
    assert!(
        first.contains("You're signed in.")
            && first.contains("Enter open")
            && first.contains("d DM"),
        "the hint bar must show on first entry to Home:\n{first}"
    );

    // Alice starts the DM through the UI and focus lands on it.
    let bob_name = bob.username();
    key(&mut alice, 'd').await;
    alice.send_keys(&bob_name).await;
    alice.enter().await;
    let landed = alice.buffer_text();
    assert!(
        landed.contains("DM started.") && landed.contains("r reply") && landed.contains("t thread"),
        "starting a DM must open it with the message pane focused:\n{landed}"
    );
    assert_eq!(
        alice.app().home.open.as_ref().map(|o| o.name.as_str()),
        Some(bob_name.as_str())
    );

    bob.wait_for("pending", SIDEBAR_TIMEOUT).await;
    key(&mut bob, 'a').await;
    bob.wait_for("Direct Messages", SIDEBAR_TIMEOUT).await;
    bob.enter().await;

    let dm_id = alice.app().home.open.as_ref().unwrap().conv_id.clone();
    let bob_state = bob.state();
    let bob_id = bob.user_id();
    let alice_name = alice.username();
    let send = |text: String, reply_to: Option<String>, thread: Option<String>| {
        let state = bob_state.clone();
        let bob_id = bob_id.clone();
        let bob_name = bob_name.clone();
        let dm_id = dm_id.clone();
        async move {
            pollis_tui::send::send_text_with(
                &state,
                &bob_id,
                Some(bob_name),
                &dm_id,
                &text,
                reply_to,
                thread,
            )
            .await
            .expect("bob send")
            .id
        }
    };
    let root = send(
        "ROOT first line\nsecond line\nthird line".into(),
        None,
        None,
    )
    .await;
    let long_words = "word ".repeat(40);
    send(format!("LONGSTART {long_words}LONGEND"), None, None).await;
    send(
        r#"{"_att":[{"key":"media/abc123","hash":"abc123","name":"photo.png","ct":"image/png","size":2048,"w":4,"h":4}],"_txt":"CAPTION_HERE"}"#.into(),
        None,
        None,
    )
    .await;
    send(format!("MENTION hey @{alice_name} look"), None, None).await;
    send("THREAD_REPLY_ONE".into(), None, Some(root.clone())).await;
    send("QUOTE_REPLY_BODY".into(), Some(root.clone()), None).await;
    pollis_tui::send::toggle_reaction(&bob_state, &root, &bob_id, "🎉", false)
        .await
        .expect("bob reacts");

    alice.wait_for("QUOTE_REPLY_BODY", SURFACE_TIMEOUT).await;
    let screen = alice.buffer_text();

    assert!(screen.contains("CAPTION_HERE"), "{screen}");
    assert!(screen.contains("[image: photo.png, 2 KB]"), "{screen}");
    assert!(
        !screen.contains("_att") && !screen.contains("media/abc123"),
        "{screen}"
    );

    let second = line_with(&screen, "second line");
    assert!(
        !second.contains("ROOT") && !second.contains("third line"),
        "{screen}"
    );
    assert!(line_with(&screen, "third line").contains("third line"));

    assert!(
        screen.contains("LONGSTART") && screen.contains("LONGEND"),
        "{screen}"
    );
    assert_ne!(
        line_with(&screen, "LONGSTART"),
        line_with(&screen, "LONGEND"),
        "a long message must wrap onto several lines:\n{screen}"
    );

    assert!(!screen.contains("THREAD_REPLY_ONE"), "{screen}");
    assert!(screen.contains("↪ 1 reply"), "{screen}");

    assert!(
        line_with(&screen, "↳").contains(&format!("{bob_name}: ROOT first line")),
        "{screen}"
    );

    assert!(screen.contains("── Today"), "{screen}");
    let header = line_with(&screen, "ROOT first line").replace(['┃', '│'], " ");
    let hhmm = header.split_whitespace().find(|w| {
        w.len() == 5
            && w.as_bytes()[2] == b':'
            && w.chars().filter(char::is_ascii_digit).count() == 4
    });
    assert!(hhmm.is_some(), "a message must carry its time: {header:?}");

    assert_eq!(
        alice.bg_of(&format!("@{alice_name}")),
        Some(Color::Yellow),
        "{screen}"
    );

    // Reactions are read once per completed sync round.
    alice.wait_for("🎉", SURFACE_TIMEOUT).await;

    // Select the root: ↑ walks up from the newest displayed message.
    for _ in 0..5 {
        press(&mut alice, KeyCode::Up).await;
    }
    assert_eq!(
        alice
            .app()
            .home
            .open
            .as_ref()
            .and_then(|o| o.selected.clone()),
        Some(root.clone()),
        "five ↑ from the newest should select the root"
    );
    assert_ne!(
        alice.bg_of("ROOT first line"),
        None,
        "the selected message is highlighted"
    );

    key(&mut alice, 'e').await;
    assert!(
        alice.buffer_text().contains("only edit your own"),
        "{}",
        alice.buffer_text()
    );

    key(&mut alice, 't').await;
    let thread = alice.buffer_text();
    assert!(
        thread.contains("Thread ·") && thread.contains("THREAD_REPLY_ONE"),
        "{thread}"
    );
    assert!(thread.contains("Esc close thread"), "{thread}");

    key(&mut alice, 'i').await;
    alice.send_keys("ALICE_IN_THREAD").await;
    alice.enter().await;
    assert!(
        alice.buffer_text().contains("ALICE_IN_THREAD"),
        "{}",
        alice.buffer_text()
    );
    press(&mut alice, KeyCode::Esc).await;
    press(&mut alice, KeyCode::Esc).await;
    let back = alice.buffer_text();
    assert!(
        !back.contains("ALICE_IN_THREAD") && back.contains("↪ 2 replies"),
        "{back}"
    );

    bob.wait_for("2 replies", SURFACE_TIMEOUT).await;
    assert!(
        !bob.buffer_text().contains("ALICE_IN_THREAD"),
        "{}",
        bob.buffer_text()
    );

    key(&mut alice, '+').await;
    alice.send_keys(":+1:").await;
    alice.enter().await;
    alice.wait_for("👍", SURFACE_TIMEOUT).await;
    bob.wait_for("👍", SURFACE_TIMEOUT).await;

    key(&mut alice, 'r').await;
    assert!(
        alice
            .buffer_text()
            .contains(&format!("Reply to {bob_name}")),
        "{}",
        alice.buffer_text()
    );
    alice.send_keys("ALICE_QUOTES").await;
    alice.enter().await;
    bob.wait_for("ALICE_QUOTES", SURFACE_TIMEOUT).await;
    assert!(
        line_with(&bob.buffer_text(), "ALICE_QUOTES").contains(&alice.username())
            || bob.buffer_text().matches("↳").count() >= 2,
        "{}",
        bob.buffer_text()
    );

    // ↑ selects the newest, which is hers.
    press(&mut alice, KeyCode::Up).await;
    key(&mut alice, 'e').await;
    alice.send_keys(" EDITED").await;
    alice.enter().await;
    alice
        .wait_for("ALICE_QUOTES EDITED (edited)", SURFACE_TIMEOUT)
        .await;
    bob.wait_for("ALICE_QUOTES EDITED", SURFACE_TIMEOUT).await;

    // An edit leaves it selected: x asks in the input bar, y deletes.
    key(&mut alice, 'x').await;
    assert!(
        alice.buffer_text().contains("y delete"),
        "{}",
        alice.buffer_text()
    );
    key(&mut alice, 'y').await;
    let after = alice.buffer_text();
    assert!(!after.contains("ALICE_QUOTES EDITED"), "{after}");
    assert!(!after.contains("Delete failed"), "{after}");
}
