//! The `pollis` binary's command line, run as a real process (#1337).
//!
//! Each case runs with an empty environment and no terminal; it never touches the data dir, so
//! several tests are fine.

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
    assert!(!stderr.contains("missing env var"), "{stderr}");
}
