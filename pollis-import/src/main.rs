//! `pollis-import plan [--slack] <export> [--json | --sheet] [--include-archived]`
//!
//! Reads a DiscordChatExporter JSON export or a Slack workspace export and
//! prints what an import would create and drop. Read-only: it writes nothing
//! and makes no network calls, so it is safe to point at a real export before
//! any Pollis import exists.

use std::path::PathBuf;
use std::process::ExitCode;

use pollis_import::plan::{plan, ImportOptions};
use pollis_import::{read_export, report, SourceKind};

const USAGE: &str = "usage: pollis-import plan [--slack] <export> [--json | --sheet] [--include-archived]

Reads an export and prints a dry-run import plan (--json: the plan as JSON;
--sheet: the invite sheet as CSV). Nothing is written and nothing leaves this
machine. The Pollis desktop app applies it.

Discord (the default): <export> is a folder of DiscordChatExporter JSON
files, and a discord-guild.json snapshot if there is one. Export with a bot
token, --format Json and --media, so images and emoji are saved beside the
JSON (Discord's links expire).

Slack (--slack, or any .zip): <export> is a workspace export as downloaded
(the .zip) or the folder it unpacks to, with a slack-workspace.json snapshot
beside it if there is one. Archived channels are left out unless
--include-archived.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("plan") {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let mut path: Option<PathBuf> = None;
    let mut json = false;
    let mut sheet = false;
    let mut slack = false;
    let mut options = ImportOptions::default();
    for arg in &args[1..] {
        match arg.as_str() {
            "--json" => json = true,
            "--sheet" => sheet = true,
            "--slack" => slack = true,
            "--include-archived" => options.include_archived = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => {
                eprintln!("unknown option {other}\n\n{USAGE}");
                return ExitCode::from(2);
            }
            other => path = Some(PathBuf::from(other)),
        }
    }
    let Some(path) = path else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let zipped = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip"));
    let kind = if slack || zipped { SourceKind::Slack } else { SourceKind::Discord };

    let export = match read_export(kind, &path) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("could not read {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let plan = plan(&export, &options);
    if sheet {
        print!("{}", report::invite_sheet(&plan));
    } else if json {
        match serde_json::to_string_pretty(&plan) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("could not serialise the plan: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        print!("{}", report::render(&plan));
    }
    ExitCode::SUCCESS
}
