//! The client must not be able to reach a database at all (#987) — neither
//! `pollis-core`, nor `pollis`, the crate that becomes the installed binary.
//!
//! Uses `cargo tree -p` (per-package features, unlike `cargo metadata`'s union).
//! `-e normal` excludes dev-dependencies: a test fixture may seed a libsql file.

use std::process::Command;

/// Crates a shipped client must never link.
const BANNED: [&str; 2] = ["libsql", "libsql-hrana"];

/// Every package in `pollis-core`'s NORMAL dependency graph, one name per line.
fn normal_deps(package: &str, extra_args: &[&str]) -> Vec<String> {
    let mut cmd = Command::new(env!("CARGO"));
    cmd.args([
        "tree",
        "-p",
        package,
        "-e",
        "normal",
        "--prefix",
        "none",
        "--format",
        "{p}",
        "--manifest-path",
    ])
    .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
    .args(extra_args);

    let out = cmd.output().expect("run cargo tree");
    assert!(
        out.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        // `{p}` renders "name version (source)".
        .filter_map(|l| l.split_whitespace().next())
        .map(|s| s.to_string())
        .collect()
}

fn assert_no_driver(package: &str, label: &str, extra_args: &[&str]) {
    let deps = normal_deps(package, extra_args);

    // Sanity: the walk found a real graph. Without this, a `cargo tree` output
    // change makes the test pass by seeing nothing.
    assert!(
        deps.iter().any(|d| d == "pollis-api") && deps.iter().any(|d| d == "reqwest"),
        "[{package} / {label}] cargo tree did not resolve the real graph — it is \
         the whole basis of this test. Saw {} line(s).",
        deps.len()
    );

    // If this dependency vanished, the check below would pass vacuously.
    assert!(
        deps.iter().any(|d| d == "verifiable-log-serve"),
        "[{package} / {label}] verifiable-log-serve is no longer a dependency, so this test's \
         most important case — a database driver arriving through it — has stopped \
         being checked. Re-point the assertion or delete it deliberately."
    );

    let offenders: Vec<&str> = BANNED
        .iter()
        .copied()
        .filter(|c| deps.iter().any(|d| d == c))
        .collect();

    assert!(
        offenders.is_empty(),
        "[{package} / {label}] {package} links {offenders:?}. Since #987 the client holds NO \
         database credential and speaks only to the Delivery Service; a driver in \
         its graph means either a direct dependency came back or a transitive edge \
         reintroduced one — which is how it happened the first time \
         (verifiable-log-serve -> verifiable-log-builder -> libsql, with nothing in \
         the client calling it). Feature-gate the database half of whatever pulled \
         it in, and take that dependency with `default-features = false`."
    );
}

#[test]
fn pollis_core_does_not_link_a_database_driver() {
    assert_no_driver("pollis-core", "default features", &[]);
}

/// Default features only: `--all-features` turns on `test-harness`, which needs libsql.
#[test]
fn the_shipping_desktop_binary_does_not_link_a_database_driver() {
    assert_no_driver("pollis", "default features", &[]);
}

#[test]
fn no_feature_of_pollis_core_pulls_in_a_database_driver() {
    assert_no_driver("pollis-core", "--all-features", &["--all-features"]);
}
