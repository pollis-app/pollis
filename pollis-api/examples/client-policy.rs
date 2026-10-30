//! The publisher's validator for `client-policy.json` (#1611), run by `scripts/client-policy.mjs`.
//!
//! `cargo run -q -p pollis-api --example client-policy -- validate <file>` parses the file with the
//! strict parser (the same code clients and the DS run, minus their tolerance for unknown keys)
//! and prints its canonical JSON on stdout. `-- check <file> <platform> <version>` prints
//! `allowed` or `blocked <minimum>` by the one rule. Exit 1 on an invalid file.

use pollis_api::client_policy::{ClientPlatform, ClientPolicy, Verdict};

fn read(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("client-policy: read {path}: {e}");
        std::process::exit(2);
    })
}

fn parse(json: &str) -> ClientPolicy {
    ClientPolicy::parse_strict(json).unwrap_or_else(|e| {
        eprintln!("client-policy: invalid: {e}");
        std::process::exit(1);
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["validate", path] => {
            let policy = parse(&read(path));
            println!("{}", serde_json::to_string_pretty(&policy).expect("serialize"));
        }
        ["check", path, platform, version] => {
            let policy = parse(&read(path));
            let Some(platform) = ClientPlatform::parse(platform) else {
                eprintln!("client-policy: unknown platform {platform:?}");
                std::process::exit(2);
            };
            match policy.evaluate(platform, version) {
                Verdict::Allowed => println!("allowed"),
                Verdict::Blocked { minimum } => println!("blocked {minimum}"),
            }
        }
        _ => {
            eprintln!("usage: client-policy validate <file> | check <file> <platform> <version>");
            std::process::exit(2);
        }
    }
}
