//! State logging. Every line is scoped (the runtime, or one installation), and
//! every failure carries one concrete fix.

use std::fmt::Display;

fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

pub fn info(scope: &str, message: impl Display) {
    eprintln!("{} pollis-bot [{scope}] {message}", now());
}

pub fn failure(scope: &str, what: impl Display, fix: &str) {
    eprintln!("{} pollis-bot [{scope}] FAILED: {what}. Fix: {fix}", now());
}

/// The scope label for one installation.
pub fn scope(installation_id: &str) -> String {
    format!("installation {installation_id}")
}
