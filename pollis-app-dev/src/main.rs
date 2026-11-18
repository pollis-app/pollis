//! `pollis-app-dev`: the native half of the `pollis app` CLI (#2006).
//!
//!   pollis-app-dev check <app.wasm> [pollis-app.json]   load-check a component (and validate its manifest)
//!   pollis-app-dev hash <app.wasm> <pollis-app.json>    print the App hash
//!   pollis-app-dev run                                  one JSON test request on stdin, one JSON result on stdout
//!   pollis-app-dev serve                                newline-delimited JSON requests and results, until EOF
//!   pollis-app-dev --allow-wasi …                       Wizer-compatible deterministic pre-initializer (ComponentizeJS's wizerBin)
//!
//! `serve` compiles a component once and reuses it for every request with the
//! same bytes, which is what the TypeScript test helpers use.

use std::io::{BufRead, Read, Write};
use std::process::ExitCode;

use pollis_app_dev::{wizer, TestHost, TestRequest, TestResult};
use pollis_app_manifest::Manifest;

const USAGE: &str = "usage: pollis-app-dev check <app.wasm> [pollis-app.json] | hash <app.wasm> <pollis-app.json> | run | serve\n\
Developer tooling for Pollis Apps; the `pollis app` CLI drives it.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if wizer::is_wizer_invocation(&args) {
        return match wizer::main(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        };
    }
    let result = match args.first().map(String::as_str) {
        Some("check") => check(&args[1..]),
        Some("hash") => hash(&args[1..]),
        Some("run") => run(),
        Some("serve") => serve(),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn read(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("reading {path}: {e}"))
}

fn check(args: &[String]) -> Result<(), String> {
    let path = args.first().ok_or(USAGE)?;
    if let Some(manifest) = args.get(1) {
        Manifest::from_json(&read(manifest)?).map_err(|e| e.to_string())?;
    }
    TestHost::new()?.check(&read(path)?)?;
    println!("ok: the App host accepts {path}");
    Ok(())
}

fn hash(args: &[String]) -> Result<(), String> {
    let (Some(component), Some(manifest)) = (args.first(), args.get(1)) else {
        return Err(USAGE.to_string());
    };
    let manifest = read(manifest)?;
    Manifest::from_json(&manifest).map_err(|e| e.to_string())?;
    // The one App hash function, shared with the runtime and the apps (#2005).
    let hash =
        pollis_app_manifest::app_hash(&read(component)?, &manifest).map_err(|e| e.to_string())?;
    println!("{}", pollis_app_manifest::hex(&hash));
    Ok(())
}

fn handle(host: &TestHost, line: &str) -> TestResult {
    match serde_json::from_str::<TestRequest>(line) {
        Ok(request) => host.run(request),
        Err(e) => TestResult::setup(format!("bad test request: {e}")),
    }
}

fn run() -> Result<(), String> {
    let host = TestHost::new()?;
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| format!("reading stdin: {e}"))?;
    print_json(&handle(&host, &input));
    Ok(())
}

fn serve() -> Result<(), String> {
    let host = TestHost::new()?;
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        print_json(&handle(&host, &line));
    }
    Ok(())
}

fn print_json(result: &TestResult) {
    let line = serde_json::to_string(result).unwrap_or_else(|e| {
        format!(
            r#"{{"ok":false,"error":{{"kind":"setup","message":{}}},"actions":[],"stateWrites":[],"logs":[],"fetches":[]}}"#,
            serde_json::Value::String(e.to_string())
        )
    });
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}
