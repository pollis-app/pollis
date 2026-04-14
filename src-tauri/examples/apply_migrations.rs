/// Iterate every `000NNN_*.sql` in `src-tauri/src/db/migrations/` in
/// numeric order and apply it to Turso.
///
/// Modes:
///   (default) `--track-only` — only run the tail `INSERT INTO
///     schema_migrations` row from each file. Use this when the
///     schema itself is intact but the tracking table got wiped.
///     Also (re)creates `schema_migrations` if missing.
///
///   `--full` — run every statement in every migration file. Keeps
///     going on per-statement errors so "duplicate column" /
///     "table already exists" don't abort the run. Use only when
///     you know what you're doing — migrations are not idempotent.
///
/// Reads TURSO_URL and TURSO_TOKEN from env (or `.env.development`).
///
/// Usage:
///   cargo run --manifest-path src-tauri/Cargo.toml \
///     --example apply_migrations
///   cargo run … --example apply_migrations -- --full

use std::fs;
use std::path::{Path, PathBuf};

use libsql::Builder;

#[derive(Clone, Copy)]
enum Mode {
    TrackOnly,
    Full,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::from_filename(".env.development").ok();

    let mode = match std::env::args().nth(1).as_deref() {
        Some("--full") => Mode::Full,
        Some("--track-only") | None => Mode::TrackOnly,
        Some(other) => anyhow::bail!(
            "unknown flag {other:?}. use --track-only (default) or --full"
        ),
    };

    let url = std::env::var("TURSO_URL")
        .map_err(|_| anyhow::anyhow!("TURSO_URL not set"))?;
    let token = std::env::var("TURSO_TOKEN")
        .map_err(|_| anyhow::anyhow!("TURSO_TOKEN not set"))?;

    // Resolve relative to the crate's manifest dir (src-tauri/) so the
    // caller's CWD doesn't matter — `pnpm db:track` runs from the repo
    // root, but direct cargo invocations can come from anywhere.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let dir = PathBuf::from(manifest_dir).join("src/db/migrations");
    if !dir.is_dir() {
        anyhow::bail!(
            "migrations directory not found at {} (manifest dir: {}, cwd: {:?})",
            dir.display(),
            manifest_dir,
            std::env::current_dir().ok(),
        );
    }
    let dir = dir.as_path();

    let files = discover_migrations(dir)?;
    println!("found {} migration file(s) in {}", files.len(), dir.display());
    for (n, f) in &files {
        println!("  {:>3}  {}", n, f.file_name().and_then(|s| s.to_str()).unwrap_or("?"));
    }

    let db = Builder::new_remote(url.clone(), token.clone()).build().await?;
    let conn = db.connect()?;

    // Always ensure the tracking table exists before attempting the
    // INSERTs that every migration file ends with. Migration 1 creates
    // this table in the normal flow, so this CREATE is how we recover
    // when that row was specifically deleted.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version     INTEGER PRIMARY KEY,
             description TEXT NOT NULL,
             applied_at  TEXT NOT NULL DEFAULT (datetime('now'))
         )",
        (),
    ).await?;
    println!("\nensured schema_migrations table exists\n");

    let (mut ok, mut skipped, mut errs) = (0u32, 0u32, 0u32);

    for (version, path) in &files {
        let sql = fs::read_to_string(path)?;
        let label = path.file_name().and_then(|s| s.to_str()).unwrap_or("?");
        println!("── migration {version:03} — {label} ──");

        match mode {
            Mode::TrackOnly => {
                let insert = extract_tracking_insert(&sql);
                let Some(stmt) = insert else {
                    println!("  no schema_migrations INSERT found — skipped");
                    skipped += 1;
                    continue;
                };
                match conn.execute(&stmt, ()).await {
                    Ok(_) => {
                        println!("  ok: tracked v{version}");
                        ok += 1;
                    }
                    Err(e) => {
                        // PK conflict = already tracked at this
                        // version. Not a real error.
                        let s = e.to_string();
                        if s.contains("UNIQUE") || s.contains("PRIMARY KEY") {
                            println!("  already tracked (skipped)");
                            skipped += 1;
                        } else {
                            println!("  error: {s}");
                            errs += 1;
                        }
                    }
                }
            }
            Mode::Full => {
                let (s_ok, s_err) = run_statements(&conn, &sql).await;
                println!("  {s_ok} stmt(s) ok, {s_err} failed (errors logged above)");
                ok += s_ok as u32;
                errs += s_err as u32;
            }
        }
    }

    println!(
        "\ndone — ok: {ok}, skipped: {skipped}, errors: {errs}",
    );
    Ok(())
}

/// Collect files matching `NNNNNN_*.sql`, sorted ascending by
/// numeric prefix. Non-numbered files (e.g. `remote_schema.sql`,
/// `local_schema.sql`) are excluded.
fn discover_migrations(dir: &Path) -> anyhow::Result<Vec<(u32, PathBuf)>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else { continue };
        if !name.ends_with(".sql") {
            continue;
        }
        // Expect `NNNNNN_...sql` where the prefix parses as an int.
        let Some(prefix) = name.split('_').next() else { continue };
        let Ok(version) = prefix.parse::<u32>() else { continue };
        out.push((version, path));
    }
    out.sort_by_key(|(v, _)| *v);
    Ok(out)
}

/// Pull just the `INSERT INTO schema_migrations … VALUES (…)`
/// statement out of a migration file. Returns None if the file
/// doesn't end with one (shouldn't happen with our convention,
/// but handled gracefully).
fn extract_tracking_insert(sql: &str) -> Option<String> {
    // Simple approach: find the INSERT, read until the terminating
    // `;` (or EOF). Migration files only contain one such INSERT
    // at the very end.
    let lower = sql.to_ascii_lowercase();
    let start = lower.find("insert into schema_migrations")?;
    let after = &sql[start..];
    let end = after.find(';').map(|i| i + 1).unwrap_or(after.len());
    Some(after[..end].trim().to_string())
}

/// Run every `;`-terminated statement in `sql`, logging and
/// counting failures without aborting.
async fn run_statements(conn: &libsql::Connection, sql: &str) -> (usize, usize) {
    let mut ok = 0;
    let mut err = 0;
    for raw in sql.split(';') {
        // Strip line comments.
        let stmt: String = raw
            .lines()
            .filter(|l| !l.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        let stmt = stmt.trim();
        if stmt.is_empty() {
            continue;
        }
        match conn.execute(stmt, ()).await {
            Ok(_) => ok += 1,
            Err(e) => {
                err += 1;
                let preview: String = stmt.chars().take(80).collect();
                println!("  error on `{preview}…`: {e}");
            }
        }
    }
    (ok, err)
}
