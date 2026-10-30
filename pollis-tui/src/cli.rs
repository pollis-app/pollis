//! Command-line arguments (#1337).
//!
//! Parsed before anything else, so `--help` and `--version` need no TTY or env. Hand-rolled and
//! pure, so it is unit-testable.

/// What the command line asked for.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// No arguments: start the interactive client.
    Run,
    /// `-h` / `--help`.
    Help,
    /// `-V` / `--version`.
    Version,
    /// Anything else. Carries the message for stderr.
    Invalid(String),
}

/// Classify the arguments that follow the program name.
pub fn parse<I, S>(args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<S> = args.into_iter().collect();
    // Help wins over everything: `pollis --bogus -h` is asking how to use it.
    if args
        .iter()
        .any(|a| matches!(a.as_ref(), "-h" | "--help" | "help"))
    {
        return Command::Help;
    }
    match args.first().map(|a| a.as_ref()) {
        None => Command::Run,
        Some("-V" | "--version" | "version") if args.len() == 1 => Command::Version,
        Some(other) => Command::Invalid(format!(
            "unrecognized argument '{other}'\n\nRun 'pollis --help' for usage."
        )),
    }
}

/// The exit status of a build below the published minimum (#1611): distinct from a failure (1)
/// and a bad argument (2), so a script can tell "update pollis" from "something broke".
pub const EXIT_UPGRADE_REQUIRED: u8 = 3;

/// How a user updates the terminal client.
pub const UPDATE_COMMAND: &str = "curl -fsSL https://cdn.pollis.com/releases/cli-install.sh | bash";

/// What a blocked build prints instead of starting (#1611): what happened, and the one thing to do.
pub fn upgrade_required_text(status: &pollis_core::client_policy::ClientPolicyStatus) -> String {
    let installed = status.version.as_deref().unwrap_or(env!("CARGO_PKG_VERSION"));
    let required = match status.minimum.as_deref() {
        Some(min) => format!(" Update to {min} or later to keep using it."),
        None => " Update to the latest version to keep using it.".to_string(),
    };
    let mut out = format!("pollis: this version ({installed}) is no longer supported.{required}\n");
    if let Some(message) = status.message.as_deref() {
        out.push_str(message);
        out.push('\n');
    }
    out.push_str(&format!(
        "\nUpdate:\n    {UPDATE_COMMAND}\n\nYour keys and messages stay on this device.\nhttps://pollis.com"
    ));
    out
}

/// `pollis --version`.
pub fn version_text() -> String {
    format!("pollis {}", env!("CARGO_PKG_VERSION"))
}

/// `pollis --help`.
pub fn help_text() -> String {
    format!(
        "\
pollis {version} — the Pollis terminal client

USAGE:
    pollis              Start the interactive client (needs a terminal)
    pollis --help       Show this help
    pollis --version    Show the version

The client is full-screen and keyboard-driven. Sign in with your email and the
one-time code it receives, then set a 4-digit PIN for this device. Press Tab to
move between the conversation list and the message pane; the bottom bar always
lists the keys for the pane that has focus.

ENVIRONMENT:
    POLLIS_DATA_DIR      Where this device keeps its keys, local database and
                         log (pollis-tui.log). Currently:
                         {data_dir}
    POLLIS_DELIVERY_URL  The Pollis Delivery Service (built into release
                         binaries; required for a source build).

Docs: https://pollis.com",
        version = env!("CARGO_PKG_VERSION"),
        data_dir = pollis_core::db::local::dirs_path().display(),
    )
}

/// The message for running the interactive client without a terminal on both
/// ends; `pollis` draws a full-screen UI and reads raw keystrokes.
pub fn not_a_terminal_text(stdin_tty: bool, stdout_tty: bool) -> String {
    let which = match (stdin_tty, stdout_tty) {
        (false, false) => "stdin and stdout are",
        (false, true) => "stdin is",
        _ => "stdout is",
    };
    format!(
        "pollis: {which} not a terminal.\n\
         pollis is an interactive, full-screen client: run it directly in a terminal\n\
         (over SSH, use `ssh -t`). See 'pollis --help'."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_arguments_runs_the_client() {
        assert_eq!(parse(Vec::<String>::new()), Command::Run);
    }

    #[test]
    fn help_and_version_flags_are_recognized() {
        for flag in ["-h", "--help", "help"] {
            assert_eq!(parse([flag]), Command::Help, "{flag}");
        }
        for flag in ["-V", "--version", "version"] {
            assert_eq!(parse([flag]), Command::Version, "{flag}");
        }
        assert_eq!(parse(["--bogus", "--help"]), Command::Help);
    }

    #[test]
    fn anything_else_is_refused_with_a_pointer_to_help() {
        match parse(["--frobnicate"]) {
            Command::Invalid(msg) => {
                assert!(msg.contains("--frobnicate"), "{msg}");
                assert!(msg.contains("pollis --help"), "{msg}");
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
        assert!(matches!(parse(["--version", "x"]), Command::Invalid(_)));
    }

    #[test]
    fn version_is_the_crate_version() {
        assert_eq!(
            version_text(),
            format!("pollis {}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn help_names_the_flags_and_the_data_dir() {
        let help = help_text();
        for needle in ["--help", "--version", "POLLIS_DATA_DIR", "pollis-tui.log"] {
            assert!(help.contains(needle), "help is missing {needle}:\n{help}");
        }
    }

    fn status(minimum: Option<&str>, message: Option<&str>) -> pollis_core::client_policy::ClientPolicyStatus {
        pollis_core::client_policy::ClientPolicyStatus {
            blocked: true,
            platform: Some(pollis_api::client_policy::ClientPlatform::Cli),
            version: Some("1.16.0".into()),
            minimum: minimum.map(str::to_string),
            message: message.map(str::to_string),
            source: pollis_core::client_policy::PolicySource::Network,
        }
    }

    #[test]
    fn the_upgrade_message_says_what_happened_and_what_to_do() {
        let text = upgrade_required_text(&status(Some("1.17.0"), None));
        assert!(text.contains("this version (1.16.0) is no longer supported"), "{text}");
        assert!(text.contains("Update to 1.17.0 or later"), "{text}");
        assert!(text.contains(UPDATE_COMMAND), "{text}");
        let text = upgrade_required_text(&status(None, Some("1.16 corrupts drafts.")));
        assert!(text.contains("Update to the latest version"), "{text}");
        assert!(text.contains("1.16 corrupts drafts."), "{text}");
        assert_ne!(EXIT_UPGRADE_REQUIRED, 1);
        assert_ne!(EXIT_UPGRADE_REQUIRED, 2);
    }

    #[test]
    fn the_not_a_terminal_message_says_which_end() {
        assert!(not_a_terminal_text(false, true).contains("stdin is not"));
        assert!(not_a_terminal_text(true, false).contains("stdout is not"));
        assert!(not_a_terminal_text(false, false).contains("stdin and stdout are not"));
    }
}
