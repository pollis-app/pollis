//! The WASI stubs (#2004): an ordinary std Rust App loads and runs, with an
//! empty environment, stdio to the runtime log, host time, no filesystem,
//! `exit` that traps, and no sockets or wasi:http.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use pollis_app_host::*;

const STD_HASH: AppHash = [5u8; 32];

fn run_std(script: &str) -> Run {
    run_component(
        &std_component(),
        STD_HASH,
        &host(),
        Arc::new(Fake::default()),
        on_message(script),
    )
}

#[test]
fn an_ordinary_std_rust_app_loads_and_runs() {
    let r = run_std("print hello; env; time; hash; sleep 20; post c-general hi");
    let outcome = r.ok();
    assert_eq!(
        outcome.actions,
        vec![Action::Post {
            channel: "c-general".into(),
            text: "hi".into()
        }]
    );
    let lines = r.lines();
    // stdout and stderr reach the runtime log, never the group.
    assert!(lines.contains(&"stdout: hello".to_string()), "{lines:?}");
    assert!(
        lines.contains(&"stderr: err hello".to_string()),
        "{lines:?}"
    );
    // No environment and no arguments; wasi-libc's default "/" names no
    // directory the App can open (see the_filesystem_is_empty).
    assert_eq!(r.line("ok env"), "ok env: vars=0 args=0 cwd=Some(\"/\")");
    // Wall-clock time is the runtime's.
    assert_eq!(r.line("ok time"), format!("ok time: {NOW}"));
    assert_eq!(r.line("ok hash"), "ok hash: 1");
    assert_eq!(r.line("ok sleep"), "ok sleep: true");
}

#[test]
fn the_filesystem_is_empty() {
    let r = run_std(
        "fs-read /etc/passwd; fs-read pollis-app.json; fs-write /tmp/x; fs-dir /; fs-dir .",
    );
    r.ok();
    for step in ["err fs-read", "err fs-write", "err fs-dir"] {
        assert!(
            r.lines().iter().any(|l| l.starts_with(step)),
            "{step}: {:?}",
            r.lines()
        );
    }
    assert!(
        !r.lines().iter().any(|l| l.starts_with("ok fs")),
        "{:?}",
        r.lines()
    );
}

#[test]
fn exit_traps_the_job_and_discards_everything() {
    let r = run_std("post c-general queued; exit");
    assert_eq!(
        r.result.as_ref().unwrap_err(),
        &JobError::Trap("the App called `exit`; return from the handler instead".into())
    );
}

#[test]
fn a_panic_traps_the_job() {
    let r = run_std("post c-general queued; panic");
    assert!(matches!(r.result, Err(JobError::Trap(_))), "{:?}", r.result);
}

#[test]
fn sleeping_past_the_deadline_times_out() {
    let limits = JobLimits {
        deadline: Duration::from_millis(200),
        ..JobLimits::default()
    };
    let r = run_component(
        &std_component(),
        STD_HASH,
        &host_with(limits),
        Arc::new(Fake::default()),
        on_message("sleep 5000"),
    );
    assert!(
        matches!(r.result, Err(JobError::Timeout(_))),
        "{:?}",
        r.result
    );
}

#[test]
fn only_the_listed_wasi_interfaces_are_importable() {
    for ok in [
        "pollis:app/host@0.1.0",
        "wasi:cli/environment@0.2.0",
        "wasi:io/streams@0.2.6",
        "wasi:clocks/wall-clock@0.2.3",
        "wasi:random/random@0.2.9",
        "wasi:filesystem/preopens@0.2.6",
    ] {
        assert!(check_import(ok).is_ok(), "{ok}");
    }
    for (bad, want) in [
        (
            "wasi:sockets/tcp@0.2.6",
            "use `http-fetch` with hosts listed in `egress`",
        ),
        ("wasi:sockets/ip-name-lookup@0.2.0", "use `http-fetch`"),
        ("wasi:http/outgoing-handler@0.2.0", "use `http-fetch`"),
        ("wasi:cli/environment@0.3.0", "may import only"),
        ("wasi:keyvalue/store@0.2.0", "may import only"),
        ("pollis:app/host@0.2.0", "may import only"),
    ] {
        let err = check_import(bad).unwrap_err();
        assert!(
            err.starts_with(&format!("import `{bad}` refused")) && err.contains(want),
            "{bad}: {err}"
        );
    }
}
