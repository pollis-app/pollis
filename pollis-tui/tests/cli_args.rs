//! The `pollis` binary's command line, run as a real process (#1337).
//!
//! Every case runs with an EMPTY environment and stdin/stdout that are not a
//! terminal — exactly the situation in which the binary used to say nothing:
//! `--help` died on `missing env var: R2_S3_ENDPOINT` in a dev build, and a bare
//! `pollis` exited 1 with `entering raw mode: Device not configured` only in its
//! log. One `#[test]` per file is the crate rule for tests that touch the data
//! dir; this one never does (it never gets past argument parsing / the TTY
//! check), so several are fine.

use std::process::{Command, Output, Stdio};

fn pollis(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pollis"))
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run pollis")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn version_prints_the_crate_version_with_no_env_and_no_tty() {
    for flag in ["--version", "-V"] {
        let out = pollis(&[flag]);
        assert!(out.status.success(), "{flag}: {out:?}");
        assert_eq!(
            text(&out.stdout).trim(),
            format!("pollis {}", env!("CARGO_PKG_VERSION")),
            "{flag}"
        );
    }
}

#[test]
fn help_prints_usage_with_no_env_and_no_tty() {
    for flag in ["--help", "-h"] {
        let out = pollis(&[flag]);
        assert!(out.status.success(), "{flag}: {out:?}");
        let stdout = text(&out.stdout);
        assert!(stdout.contains("USAGE"), "{flag}: {stdout}");
        assert!(stdout.contains("--version"), "{flag}: {stdout}");
        assert!(stdout.contains("POLLIS_DATA_DIR"), "{flag}: {stdout}");
    }
}

#[test]
fn an_unknown_argument_is_refused_on_stderr() {
    let out = pollis(&["--frobnicate"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = text(&out.stderr);
    assert!(stderr.contains("--frobnicate"), "{stderr}");
    assert!(stderr.contains("pollis --help"), "{stderr}");
}

#[test]
fn without_a_terminal_it_says_so_instead_of_exiting_silently() {
    let out = pollis(&[]);
    assert!(!out.status.success(), "{out:?}");
    let stderr = text(&out.stderr);
    assert!(stderr.contains("not a terminal"), "{stderr}");
    // It must fail on the terminal check, before reading any config.
    assert!(!stderr.contains("missing env var"), "{stderr}");
}
