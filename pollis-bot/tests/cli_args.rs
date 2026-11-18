//! The `pollis-bot` binary's command line, run as a real process with an empty
//! environment; none of these touch a data directory.

use std::process::{Command, Output, Stdio};

fn bot(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pollis-bot"))
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .output()
        .expect("run pollis-bot")
}

#[test]
fn help_lists_every_command_and_the_defaults() {
    let out = bot(&["--help"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for word in ["pair", "add-host", "run", "post", "status", "rotate-secret", "max_installations", "127.0.0.1:8787"] {
        assert!(text.contains(word), "{word} missing from:\n{text}");
    }
}

#[test]
fn version_prints_the_crate_version() {
    let out = bot(&["--version"]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), format!("pollis-bot {}", env!("CARGO_PKG_VERSION")));
}

#[test]
fn a_bad_command_line_exits_2_and_names_the_mistake() {
    let out = bot(&["frobnicate"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("frobnicate"));
}
