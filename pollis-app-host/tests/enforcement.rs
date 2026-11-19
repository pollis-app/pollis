//! Every enforcement rule in #2004, each against the checked-in fixture App
//! (`fixtures/test-app`). Each test fails if the rule it names is removed.

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::*;
use pollis_app_host::*;
use pollis_app_manifest::{ActionKind, ChannelGrant, EventKind};

// ---------------------------------------------------------------------------
// The App runs through every trigger and every host function.
// ---------------------------------------------------------------------------

#[test]
fn every_trigger_reaches_its_handler() {
    let m = msg("m9", "c-ops", "u-alice", "now", NOW - 5);
    let triggers = [
        (Trigger::Message(m.clone()), "trigger message m9"),
        (Trigger::Mention(m.clone()), "trigger mention m9"),
        (
            Trigger::Command(Command {
                name: "triage".into(),
                args: "now".into(),
                message: m.clone(),
            }),
            "trigger command triage m9",
        ),
        (
            Trigger::Schedule(Schedule {
                name: "now".into(),
                slot: 42,
            }),
            "trigger schedule 42",
        ),
        (
            Trigger::Request(Request {
                id: "r1".into(),
                requester: author("u-alice"),
                body: "now".into(),
            }),
            "trigger request r1",
        ),
        (
            Trigger::Webhook(Webhook {
                delivery_id: "d1".into(),
                headers: vec![],
                body: b"now".to_vec(),
            }),
            "trigger webhook d1",
        ),
    ];
    for (trigger, want) in triggers {
        let run = run(trigger);
        run.ok();
        assert_eq!(
            run.lines(),
            vec![want.to_string(), format!("ok now: {NOW}")]
        );
    }
}

#[test]
fn every_host_function_works_within_the_grants() {
    let fake = Fake::default();
    fake.messages
        .lock()
        .unwrap()
        .push(msg("m-bot", "c-general", BOT, "earlier", NOW - DAY));
    fake.http.lock().unwrap().insert(
        "https://api.x.com/v1".into(),
        HttpResponse {
            status: 200,
            headers: vec![],
            body: b"pong".to_vec(),
        },
    );
    let run = run_with(
        fake,
        on_message(
            "post c-ops hello; reply m1 thanks; react m1 +1; edit m-bot fixed; delete m-bot; \
             query; kv-set k v1; kv-get k; kv-set gone x; kv-del gone; fetch https://api.x.com/v1; random",
        ),
    );
    let outcome = run.ok().clone();
    assert_eq!(
        outcome.actions,
        vec![
            Action::Post {
                channel: "c-ops".into(),
                text: "hello".into()
            },
            Action::Reply {
                channel: "c-general".into(),
                message: "m1".into(),
                text: "thanks".into()
            },
            Action::React {
                channel: "c-general".into(),
                message: "m1".into(),
                emoji: "+1".into()
            },
            Action::EditOwn {
                channel: "c-general".into(),
                message: "m-bot".into(),
                text: "fixed".into()
            },
            Action::DeleteOwn {
                channel: "c-general".into(),
                message: "m-bot".into()
            },
        ]
    );
    assert_eq!(
        outcome.kv_writes,
        vec![
            KvWrite {
                key: "gone".into(),
                value: None
            },
            KvWrite {
                key: "k".into(),
                value: Some(b"v1".to_vec())
            },
        ]
    );
    assert_eq!(run.line("ok query"), "ok query: m-bot");
    assert_eq!(run.line("ok kv-get"), "ok kv-get k: v1");
    assert_eq!(
        run.line("ok fetch"),
        "ok fetch https://api.x.com/v1: 200 pong"
    );
    assert!(run.line("ok random").starts_with("ok random: "));
}

// ---------------------------------------------------------------------------
// An App never receives a message from a channel outside its grants, nor an
// event kind it did not declare.
// ---------------------------------------------------------------------------

#[test]
fn message_from_an_ungranted_channel_is_not_delivered() {
    let run = run(Trigger::Message(msg(
        "m1", "c-secret", "u-alice", "now", NOW,
    )));
    assert_eq!(
        run.result.as_ref().unwrap_err(),
        &JobError::NotDelivered(
            "message in `c-secret` not delivered: not in `read.channels`".into()
        )
    );
    assert!(run.lines().is_empty(), "the App ran");
}

#[test]
fn channel_the_admin_granted_but_the_manifest_did_not_request_is_not_delivered() {
    let mut admin = admin();
    admin.channels.push(pollis_app_manifest::ChannelGrant {
        id: "c-random".into(),
        name: "random".into(),
    });
    let mut manifest = manifest();
    manifest.read.channels.push("#random".into());
    // Admin grants random, manifest does not ask for it: refused outright.
    let r = run_on(
        &host(),
        Arc::new(Fake::default()),
        "inst-a",
        &common::manifest(),
        &admin,
        on_message("now"),
    );
    assert!(matches!(r.result, Err(JobError::GrantsRefused(ref e)) if e.contains("#random")));
    // Manifest asks for random, admin did not grant it: not delivered.
    let r = run_on(
        &host(),
        Arc::new(Fake::default()),
        "inst-a",
        &manifest,
        &common::admin(),
        Trigger::Message(msg("m1", "c-random", "u-alice", "now", NOW)),
    );
    assert!(matches!(r.result, Err(JobError::NotDelivered(_))));
}

#[test]
fn undeclared_trigger_kind_is_not_delivered() {
    let mut manifest = manifest();
    manifest
        .triggers
        .remove(&pollis_app_manifest::TriggerKind::Webhook);
    let r = run_on(
        &host(),
        Arc::new(Fake::default()),
        "inst-a",
        &manifest,
        &admin(),
        Trigger::Webhook(Webhook {
            delivery_id: "d1".into(),
            headers: vec![],
            body: b"now".to_vec(),
        }),
    );
    assert_eq!(
        r.result.as_ref().unwrap_err(),
        &JobError::NotDelivered("`webhook` trigger not delivered: not in `triggers`".into())
    );
    assert!(r.lines().is_empty());
}

#[test]
fn undeclared_event_kind_is_not_delivered() {
    let mut admin = admin();
    admin.events = BTreeSet::from([EventKind::Created]);
    let mut edited = msg("m1", "c-general", "u-alice", "now", NOW - 10);
    edited.edited_at = Some(NOW);
    let r = run_on(
        &host(),
        Arc::new(Fake::default()),
        "inst-a",
        &manifest(),
        &admin,
        Trigger::Message(edited),
    );
    assert_eq!(
        r.result.unwrap_err(),
        JobError::NotDelivered("`edited` message not delivered: not in `read.events`".into())
    );
}

#[test]
fn the_bots_own_message_is_not_delivered() {
    let r = run(Trigger::Message(msg(
        "m1",
        "c-general",
        BOT,
        "post c-general loop",
        NOW,
    )));
    assert!(matches!(r.result, Err(JobError::NotDelivered(_))));
}

#[test]
fn query_returns_only_granted_channels_events_and_history() {
    let fake = Fake::default();
    {
        let mut m = fake.messages.lock().unwrap();
        m.push(msg("in-window", "c-general", "u-bob", "x", NOW - 2 * DAY));
        m.push(msg("other-channel", "c-secret", "u-bob", "x", NOW - DAY));
        m.push(msg("too-old", "c-general", "u-bob", "x", NOW - 8 * DAY));
        let mut edited = msg("edited", "c-ops", "u-bob", "x", NOW - DAY);
        edited.edited_at = Some(NOW - 10);
        m.push(edited);
    }
    let mut admin = admin();
    admin.events = BTreeSet::from([EventKind::Created]);
    let fake = Arc::new(fake);
    let r = run_on(
        &host(),
        fake.clone(),
        "inst-a",
        &manifest(),
        &admin,
        on_message("query; query c-secret"),
    );
    r.ok();
    // The fake returned everything; the host kept only what the grants allow.
    assert_eq!(r.line("ok query"), "ok query: in-window");
    assert_eq!(
        r.line("err query c-secret"),
        "err query c-secret: `query-messages` on `c-secret` refused: not in `read.channels`"
    );
    // And it asked the runtime only for granted channels inside the window.
    let asked = fake.queries.lock().unwrap()[0].clone();
    assert_eq!(
        asked.channels,
        vec!["c-general".to_string(), "c-ops".to_string()]
    );
    assert_eq!(asked.since, NOW - 7 * DAY);
}

// ---------------------------------------------------------------------------
// Every action outside its grants is refused and reported to the App as an
// error, never applied.
// ---------------------------------------------------------------------------

#[test]
fn actions_outside_the_grants_are_refused_with_the_manifest_field() {
    let fake = Fake::default();
    {
        let mut m = fake.messages.lock().unwrap();
        m.push(msg("m-secret", "c-secret", "u-bob", "x", NOW));
        m.push(msg("m-bob", "c-general", "u-bob", "x", NOW));
        m.push(msg("m-bot-secret", "c-secret", BOT, "x", NOW));
    }
    let mut admin = admin();
    admin.actions.remove(&ActionKind::React);
    let r = run_on(
        &host(),
        Arc::new(fake),
        "inst-a",
        &manifest(),
        &admin,
        on_message(
            "post c-secret hi; reply m-secret hi; react m1 +1; edit m-bob hi; delete m-bob; \
             delete m-bot-secret; reply nope hi",
        ),
    );
    let outcome = r.ok();
    assert!(outcome.actions.is_empty(), "applied: {:?}", outcome.actions);
    for (step, want) in [
        (
            "err post c-secret",
            "`post` to `c-secret` refused: not in `read.channels`",
        ),
        (
            "err reply m-secret",
            "`reply` on `m-secret` refused: not a message in `read.channels`",
        ),
        ("err react m1", "`react` refused: not in `actions`"),
        (
            "err edit m-bob",
            "`edit-own` on `m-bob` refused: not a message this bot sent",
        ),
        (
            "err delete m-bob",
            "`delete-own` on `m-bob` refused: not a message this bot sent",
        ),
        (
            "err delete m-bot-secret",
            "`delete-own` on `m-bot-secret` refused: not a message in `read.channels`",
        ),
        (
            "err reply nope",
            "`reply` on `nope` refused: not a message in `read.channels`",
        ),
    ] {
        assert!(r.line(step).ends_with(want), "{step}: {}", r.line(step));
    }
}

#[test]
fn state_beyond_the_quota_is_refused() {
    let big = "x".repeat(70);
    let r = run(on_message(&format!("kv-set small v; kv-set big {big}")));
    assert!(r
        .line("err kv-set big")
        .ends_with("`kv-set` refused: would exceed `state.quota` (64 bytes)"));
    assert_eq!(r.ok().kv_writes.len(), 1);
}

#[test]
fn state_without_a_quota_is_refused() {
    let mut manifest = manifest();
    manifest.state.quota = 0;
    let r = run_on(
        &host(),
        Arc::new(Fake::default()),
        "inst-a",
        &manifest,
        &admin(),
        on_message("kv-get k"),
    );
    assert!(r
        .line("err kv-get")
        .ends_with("`kv-get` refused: no `state.quota` declared"));
}

// ---------------------------------------------------------------------------
// http-fetch to an undeclared host, a non-HTTPS URL, a redirect off-host or an
// IP literal is refused.
// ---------------------------------------------------------------------------

#[test]
fn http_fetch_refusals_never_reach_the_network() {
    let fake = Arc::new(Fake::default());
    let r = run_on(
        &host(),
        fake.clone(),
        "inst-a",
        &manifest(),
        &admin(),
        on_message(
            "fetch https://api.y.com/; fetch http://api.x.com/; fetch https://93.184.216.34/; \
             fetch https://[::1]/; fetch https://api.x.com:8443/",
        ),
    );
    r.ok();
    assert_eq!(
        r.line("err fetch https://api.y.com"),
        "err fetch https://api.y.com/: `http-fetch` to `api.y.com` refused: not in `egress`"
    );
    assert!(r
        .line("err fetch http://")
        .ends_with("`http-fetch` to `api.x.com` refused: only https is allowed"));
    assert!(r
        .line("err fetch https://93")
        .ends_with("IP literals are not allowed; list a host name in `egress`"));
    assert!(r
        .line("err fetch https://[")
        .ends_with("IP literals are not allowed; list a host name in `egress`"));
    assert!(r
        .line("err fetch https://api.x.com:8443")
        .ends_with("only the default https port is allowed"));
    assert!(fake.fetched.lock().unwrap().is_empty());
}

#[test]
fn http_fetch_with_no_egress_is_refused() {
    let mut admin = admin();
    admin.egress.clear();
    let r = run_on(
        &host(),
        Arc::new(Fake::default()),
        "inst-a",
        &manifest(),
        &admin,
        on_message("fetch https://api.x.com/"),
    );
    assert!(r
        .line("err fetch")
        .ends_with("`http-fetch` to `api.x.com` refused: not in `egress`"));
}

#[test]
fn http_redirect_off_host_is_refused_and_same_host_is_followed() {
    let fake = Fake::default();
    {
        let mut http = fake.http.lock().unwrap();
        let redirect = |to: &str| HttpResponse {
            status: 302,
            headers: vec![("Location".into(), to.into())],
            body: vec![],
        };
        http.insert(
            "https://api.x.com/off".into(),
            redirect("https://evil.example/steal"),
        );
        http.insert(
            "https://api.x.com/ip".into(),
            redirect("https://127.0.0.1/"),
        );
        http.insert("https://api.x.com/same".into(), redirect("/final"));
        http.insert(
            "https://api.x.com/final".into(),
            HttpResponse {
                status: 200,
                headers: vec![],
                body: b"done".to_vec(),
            },
        );
    }
    let fake = Arc::new(fake);
    let r = run_on(
        &host(),
        fake.clone(),
        "inst-a",
        &manifest(),
        &admin(),
        on_message(
            "fetch https://api.x.com/off; fetch https://api.x.com/ip; fetch https://api.x.com/same",
        ),
    );
    r.ok();
    assert!(r.line("err fetch https://api.x.com/off").ends_with(
        "`http-fetch` redirect from `api.x.com` to `evil.example` refused: redirects may not leave the requested host"
    ));
    assert!(r
        .line("err fetch https://api.x.com/ip")
        .contains("refused: redirects may not leave the requested host"));
    assert_eq!(
        r.line("ok fetch"),
        "ok fetch https://api.x.com/same: 200 done"
    );
    let urls: Vec<String> = fake
        .fetched
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.url.clone())
        .collect();
    assert!(
        urls.iter().all(|u| u.starts_with("https://api.x.com/")),
        "{urls:?}"
    );
}

#[test]
fn http_response_over_the_cap_is_refused() {
    let fake = Fake::default();
    fake.http.lock().unwrap().insert(
        "https://api.x.com/big".into(),
        HttpResponse {
            status: 200,
            headers: vec![],
            body: vec![0; 2048],
        },
    );
    let limits = JobLimits {
        max_http_response_bytes: 1024,
        ..JobLimits::default()
    };
    let r = run_on(
        &host_with(limits),
        Arc::new(fake),
        "inst-a",
        &manifest(),
        &admin(),
        on_message("fetch https://api.x.com/big"),
    );
    assert!(r
        .line("err fetch")
        .ends_with("`http-fetch` to `api.x.com` refused: response exceeds 1024 bytes"));
}

// ---------------------------------------------------------------------------
// A trap, timeout or memory overrun discards all of that job's queued actions
// and leaves the store unchanged.
// ---------------------------------------------------------------------------

fn assert_discarded(run: &Run) {
    assert!(run.result.is_err(), "job succeeded: {:?}", run.result);
    // Queued before the failure, so the App saw them accepted...
    assert!(
        run.lines()
            .contains(&"ok post c-general queued: ".to_string()),
        "{:?}",
        run.lines()
    );
    assert!(run.lines().contains(&"ok kv-set k v: ".to_string()));
    // ...but nothing reached the store.
    assert!(run.fake.kv.lock().unwrap().is_empty());
}

#[test]
fn a_trap_discards_everything() {
    let r = run(on_message("post c-general queued; kv-set k v; trap"));
    assert_discarded(&r);
    assert!(matches!(r.result, Err(JobError::Trap(_))), "{:?}", r.result);
}

#[test]
fn a_timeout_discards_everything() {
    let r = run_on(
        &host_with(short_deadline()),
        Arc::new(Fake::default()),
        "inst-a",
        &manifest(),
        &admin(),
        on_message("post c-general queued; kv-set k v; spin"),
    );
    assert_discarded(&r);
    assert!(
        matches!(r.result, Err(JobError::Timeout(_))),
        "{:?}",
        r.result
    );
}

#[test]
fn a_timeout_in_one_job_does_not_stop_another() {
    let host = Arc::new(host_with(short_deadline()));
    let h = host.clone();
    let spinner = std::thread::spawn(move || {
        run_on(
            &h,
            Arc::new(Fake::default()),
            "inst-a",
            &manifest(),
            &admin(),
            on_message("spin"),
        )
        .result
    });
    // Starts while the spinner runs and calls the host thousands of times,
    // crossing the spinner's epoch bump.
    let busy = run_on(
        &host,
        Arc::new(Fake::default()),
        "inst-b",
        &manifest(),
        &admin(),
        on_message("calls 5000"),
    );
    assert!(matches!(spinner.join().unwrap(), Err(JobError::Timeout(_))));
    busy.ok();
}

#[test]
fn a_memory_overrun_discards_everything() {
    let limits = JobLimits {
        memory_bytes: 16 << 20,
        ..JobLimits::default()
    };
    let r = run_on(
        &host_with(limits),
        Arc::new(Fake::default()),
        "inst-a",
        &manifest(),
        &admin(),
        on_message("post c-general queued; kv-set k v; oom"),
    );
    assert_discarded(&r);
    assert_eq!(
        r.result.as_ref().unwrap_err(),
        &JobError::MemoryExceeded(16 << 20)
    );
}

#[test]
fn exceeding_the_host_call_cap_discards_everything() {
    let limits = JobLimits {
        max_host_calls: 100,
        ..JobLimits::default()
    };
    let r = run_on(
        &host_with(limits),
        Arc::new(Fake::default()),
        "inst-a",
        &manifest(),
        &admin(),
        on_message("post c-general queued; kv-set k v; calls 1000"),
    );
    assert_discarded(&r);
    assert_eq!(
        r.result.unwrap_err(),
        JobError::HostCallLimit("`now` exceeded the per-job limit of 100 host calls".into())
    );
}

#[test]
fn exceeding_the_action_and_fetch_caps_traps() {
    let limits = JobLimits {
        max_actions: 2,
        ..JobLimits::default()
    };
    let r = run_on(
        &host_with(limits),
        Arc::new(Fake::default()),
        "inst-a",
        &manifest(),
        &admin(),
        on_message("post c-general a; post c-general b; post c-general c"),
    );
    assert_eq!(
        r.result.unwrap_err(),
        JobError::HostCallLimit("`post` exceeded the per-job limit of 2 actions".into())
    );

    let fake = Fake::default();
    fake.http.lock().unwrap().insert(
        "https://api.x.com/".into(),
        HttpResponse {
            status: 200,
            headers: vec![],
            body: vec![],
        },
    );
    let limits = JobLimits {
        max_http_fetches: 1,
        ..JobLimits::default()
    };
    let r = run_on(
        &host_with(limits),
        Arc::new(fake),
        "inst-a",
        &manifest(),
        &admin(),
        on_message("fetch https://api.x.com/; fetch https://api.x.com/"),
    );
    assert_eq!(
        r.result.unwrap_err(),
        JobError::HostCallLimit("`http-fetch` exceeded the per-job limit of 1 HTTP fetches".into())
    );
}

#[test]
fn a_handler_error_discards_everything() {
    let r = run(on_message("post c-general queued; kv-set k v; fail boom"));
    assert_discarded(&r);
    assert_eq!(
        r.result.unwrap_err(),
        JobError::HandlerFailed("boom".into())
    );
}

// ---------------------------------------------------------------------------
// Two installations of the same App share nothing.
// ---------------------------------------------------------------------------

#[test]
fn two_installations_of_the_same_app_share_nothing() {
    let host = host();
    let fake = Arc::new(Fake::default());
    let a = run_on(
        &host,
        fake.clone(),
        "inst-a",
        &manifest(),
        &admin(),
        on_message("kv-set k secret-a; static; static"),
    );
    fake.commit("inst-a", a.ok());
    assert!(
        host.is_cached(&APP_HASH),
        "compiled component should be shared"
    );

    fake.logs.lock().unwrap().clear();
    let b = run_on(
        &host,
        fake.clone(),
        "inst-b",
        &manifest(),
        &admin(),
        on_message("kv-get k; static"),
    );
    b.ok();
    assert_eq!(b.line("ok kv-get"), "ok kv-get k: none");
    // Instance memory does not carry over: a fresh instance per job.
    assert_eq!(b.line("ok static"), "ok static: 1");

    fake.logs.lock().unwrap().clear();
    let a2 = run_on(
        &host,
        fake.clone(),
        "inst-a",
        &manifest(),
        &admin(),
        on_message("kv-get k; static"),
    );
    a2.ok();
    assert_eq!(a2.line("ok kv-get"), "ok kv-get k: secret-a");
    assert_eq!(a2.line("ok static"), "ok static: 1");
}

// ---------------------------------------------------------------------------
// Bots never moderate: there is no host function for deleting others'
// messages, removing members or changing roles.
// ---------------------------------------------------------------------------

/// The function names declared in the WIT `host` interface.
fn wit_host_functions() -> BTreeSet<String> {
    let wit = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../pollis-app-wit/wit/app.wit"
    ))
    .unwrap();
    let start = wit.find("interface host {").expect("host interface");
    let body = &wit[start..];
    let body = &body[..body.find("\n}").unwrap()];
    body.lines()
        .filter_map(|l| l.trim().split_once(": func"))
        .map(|(name, _)| name.trim().to_string())
        .collect()
}

#[test]
fn there_is_no_moderation_host_function() {
    let functions = wit_host_functions();
    // Exactly these; adding a host function is a conscious change here.
    assert_eq!(
        functions,
        HOST_IMPORTS
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<_>>()
    );
    for f in &functions {
        for banned in [
            "remove", "kick", "ban", "role", "member", "admin", "moderat", "pin", "mute",
        ] {
            assert!(
                !f.contains(banned),
                "host function `{f}` looks like moderation"
            );
        }
        // kv-delete touches only the installation's own state.
        if (f.contains("delete") || f.contains("edit")) && !f.starts_with("kv-") {
            assert!(
                f.ends_with("-own"),
                "`{f}` must be limited to the bot's own messages"
            );
        }
    }
    // And ActionKind has no moderation variant either.
    for a in ActionKind::ALL {
        assert!(!["remove", "kick", "ban", "role"]
            .iter()
            .any(|b| a.as_str().contains(b)));
    }
}

// ---------------------------------------------------------------------------
// The engine: no WASI, cache by App hash, grants never wider than the manifest.
// ---------------------------------------------------------------------------

#[test]
fn a_component_importing_sockets_is_refused() {
    let host = host();
    let err = host.prepare([9; 32], &net_component()).unwrap_err();
    match err {
        JobError::ImportRefused(msg) => {
            assert!(msg.starts_with("import `wasi:sockets/"), "{msg}");
            assert!(
                msg.contains("use `http-fetch` with hosts listed in `egress`"),
                "{msg}"
            );
        }
        other => panic!("expected ImportRefused, got {other:?}"),
    }
    assert!(!host.is_cached(&[9; 32]));
}

#[test]
fn the_fixture_app_imports_only_the_pollis_host() {
    host().prepare(APP_HASH, &component()).unwrap();
}

#[test]
fn different_bytes_under_a_cached_app_hash_are_refused() {
    let host = host();
    host.prepare(APP_HASH, &component()).unwrap();
    let err = host.prepare(APP_HASH, &net_component()).unwrap_err();
    assert!(matches!(err, JobError::AppHashMismatch(_)), "{err:?}");
}

#[test]
fn garbage_is_refused() {
    assert!(matches!(
        host().prepare([1; 32], b"not wasm"),
        Err(JobError::InvalidComponent(_))
    ));
}

#[test]
fn grants_wider_than_the_manifest_are_refused() {
    let mut admin = admin();
    admin.egress.insert("evil.example".into());
    let r = run_on(
        &host(),
        Arc::new(Fake::default()),
        "inst-a",
        &manifest(),
        &admin,
        on_message("now"),
    );
    assert_eq!(
        r.result.unwrap_err(),
        JobError::GrantsRefused(
            "grants are wider than the manifest: host `evil.example` is not in `egress`".into()
        )
    );
}

// ---------------------------------------------------------------------------
// `post` names a granted channel by id or by name, and nothing else (#2007).
// ---------------------------------------------------------------------------

fn posted_to(outcome: &JobOutcome) -> Vec<String> {
    outcome
        .actions
        .iter()
        .map(|action| match action {
            Action::Post { channel, .. } => channel.clone(),
            other => panic!("not a post: {other:?}"),
        })
        .collect()
}

#[test]
fn post_resolves_a_granted_channel_by_name_to_its_id() {
    let r = run(on_message("post c-ops a; post ops b; post #ops c; post OPS d; post #Ops e"));
    assert_eq!(posted_to(r.ok()), vec!["c-ops"; 5]);
}

#[test]
fn post_by_a_name_that_was_not_granted_is_refused() {
    let mut narrow = admin();
    narrow.channels.retain(|c| c.name == "general");
    let r = run_on(
        &host(),
        Arc::new(Fake::default()),
        "inst-a",
        &manifest(),
        &narrow,
        on_message("post ops hi; post #secret hi"),
    );
    let outcome = r.ok();
    assert!(outcome.actions.is_empty(), "applied: {:?}", outcome.actions);
    assert_eq!(r.line("err post ops"), "err post ops hi: `post` to `ops` refused: not in `read.channels`");
    assert_eq!(r.line("err post #secret"), "err post #secret hi: `post` to `#secret` refused: not in `read.channels`");
}

#[test]
fn post_by_a_name_two_granted_channels_share_is_refused() {
    let mut twins = admin();
    twins.channels.push(ChannelGrant { id: "c-ops-2".into(), name: "ops".into() });
    let r = run_on(
        &host(),
        Arc::new(Fake::default()),
        "inst-a",
        &manifest(),
        &twins,
        on_message("post ops hi; post c-ops-2 by-id"),
    );
    assert_eq!(posted_to(r.ok()), vec!["c-ops-2"]);
    assert_eq!(r.line("err post ops"), "err post ops hi: `post` to `ops` refused: not in `read.channels`");
}
