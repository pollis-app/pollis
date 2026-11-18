//! `pollis-import plan <export-dir> [--json] [--drop-categories]`
//!
//! Reads a DiscordChatExporter JSON export and prints what an import would
//! create and drop. Read-only: it writes nothing and makes no network calls,
//! so it is safe to point at a real export before any Pollis import exists.

use std::path::PathBuf;
use std::process::ExitCode;

use pollis_import::discord;
use pollis_import::plan::{plan, CategoryStrategy, ImportOptions};
use pollis_import::report;

const USAGE: &str = "usage: pollis-import plan <export-dir> [--json] [--drop-categories]

Reads every DiscordChatExporter JSON file in <export-dir> (one server) and
prints a dry-run import plan. Nothing is written and nothing leaves this machine.

Export with DiscordChatExporter using a bot token, --format Json and --media,
so images and emoji are saved beside the JSON (Discord's links expire).";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("plan") {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let mut dir: Option<PathBuf> = None;
    let mut json = false;
    let mut options = ImportOptions::default();
    for arg in &args[1..] {
        match arg.as_str() {
            "--json" => json = true,
            "--drop-categories" => options.categories = CategoryStrategy::Drop,
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => {
                eprintln!("unknown option {other}\n\n{USAGE}");
                return ExitCode::from(2);
            }
            other => dir = Some(PathBuf::from(other)),
        }
    }
    let Some(dir) = dir else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };

    let export = match discord::read_export_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("could not read {}: {e}", dir.display());
            return ExitCode::FAILURE;
        }
    };
    let plan = plan(&export, &options);
    if json {
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
