//! The client must not be able to reach a database at all (#987) — neither
//! `pollis-core`, nor `pollis`, the crate that becomes the installed binary.
//!
//! The sibling of `no_client_side_remote_writes.rs`, and strictly stronger. That
//! one scans source text for `INSERT`/`UPDATE`/`DELETE` against a remote table —
//! a lower bound, because SQL assembled at runtime slips past it. This one does
//! not read source: it asserts the crate has no way to run a statement in the
//! first place, because no database driver is in its dependency graph.
//!
//! # Why it needs a test
//!
//! Before #987 the client held a whole-database read token and 102 call sites
//! opened `state.remote_db` to use it. Removing them is not the guarantee — the
//! guarantee is that a *new* one cannot be added, and the only thing that makes
//! that true is the absence of the dependency. A dependency comes back quietly:
//! one `libsql = "0.9"` line, or — the case that actually happened here — a
//! TRANSITIVE edge through a crate that has nothing to do with databases.
//! `pollis-core` depends on `verifiable-log-serve` for release verification, and
//! `verifiable-log-serve` depended on `verifiable-log-builder`, which reads the
//! commit log out of Turso. Nothing in the client called it; `libsql` linked
//! anyway. Both edges are cut (`verifiable-log-serve`'s `live` feature and
//! `verifiable-log-builder`'s `db-source`), and this is what keeps them cut.
//!
//! # How it checks
//!
//! `cargo tree -p <crate> -e normal`, which resolves features FOR THAT
//! PACKAGE — not the workspace union, which is why `cargo metadata` is the wrong
//! tool here: its `resolve` graph reports every feature any workspace member
//! turns on, so `verifiable-log-builder` appears with its database reader
//! enabled because `pollis-delivery` needs one.
//!
//! `-e normal` excludes dev-dependencies on purpose: a test fixture may
//! legitimately seed a libsql file and never ships. Run twice — default features
//! and `--all-features` — so a driver cannot hide behind an optional feature the
//! default build happens not to select.

use std::process::Command;

/// Crates a shipped client must never link.
///
/// `libsql` is the driver; `libsql-hrana` is its wire protocol and would appear
/// alone if someone reached for the transport without the driver.
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
        // `{p}` renders "name version (source)"; the dedup marker "(*)" can
        // follow. The name is the first whitespace-delimited token.
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

    // The verifier crate is the specific edge that needs a feature gate, so
    // assert it is actually present: if a refactor dropped the dependency
    // entirely, the check below would pass for the wrong reason.
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

/// The library is only half the answer — `pollis` is the crate that becomes the
/// installed binary, and it can declare a driver of its own that `pollis-core`
/// never sees. It did: the flows harness needs `libsql` to stand up its
/// process-local "remote", and the dependency was unconditional, so every
/// shipped desktop build linked the driver (and the h2 0.3 / hyper 0.14 stack
/// behind it) to serve a test. It is now optional and reached only through
/// `test-harness`, which no release build selects.
///
/// Default features only, deliberately: `--all-features` turns on
/// `test-harness`, where the driver is supposed to be.
#[test]
fn the_shipping_desktop_binary_does_not_link_a_database_driver() {
    assert_no_driver("pollis", "default features", &[]);
}

/// The same, with EVERY feature on. A driver behind an optional feature is still
/// a driver a build can select, and `media` / `test-harness` are both selected
/// by real builds.
#[test]
fn no_feature_of_pollis_core_pulls_in_a_database_driver() {
    assert_no_driver("pollis-core", "--all-features", &["--all-features"]);
}
