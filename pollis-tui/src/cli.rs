//! Command-line arguments (#1337).
//!
//! Parsed BEFORE anything else in `main` — before `Config::from_env`, before
//! the terminal check — so `pollis --help` and `pollis --version` answer with no
//! TTY and no environment at all. A dev build without its env used to be unable
//! to print help (`missing env var: R2_S3_ENDPOINT`), and every argument was
//! silently ignored.
//!
//! Pure and hand-rolled: three flags do not justify an argument-parser
//! dependency, and keeping it a function of an iterator is what lets the unit
//! tests below pin it.

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
    // Help wins over everything, the way most tools behave: `pollis --bogus -h`
    // is someone asking how to use it.
    if args.iter().any(|a| matches!(a.as_ref(), "-h" | "--help" | "help")) {
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
/// ends. `pollis` draws a full-screen UI and reads raw keystrokes; with stdin or
/// stdout redirected it used to exit 1 with nothing on screen, the reason
/// (`entering raw mode: Device not configured`) only in the log.
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
        // Help anywhere on the line is a request for help.
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
        // A flag that is valid alone is not silently accepted with extras.
        assert!(matches!(parse(["--version", "x"]), Command::Invalid(_)));
    }

    #[test]
    fn version_is_the_crate_version() {
        assert_eq!(version_text(), format!("pollis {}", env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn help_names_the_flags_and_the_data_dir() {
        let help = help_text();
        for needle in ["--help", "--version", "POLLIS_DATA_DIR", "pollis-tui.log"] {
            assert!(help.contains(needle), "help is missing {needle}:\n{help}");
        }
    }

    #[test]
    fn the_not_a_terminal_message_says_which_end() {
        assert!(not_a_terminal_text(false, true).contains("stdin is not"));
        assert!(not_a_terminal_text(true, false).contains("stdout is not"));
        assert!(not_a_terminal_text(false, false).contains("stdin and stdout are not"));
    }
}
