//! Push the current schema to Turso, wiping all existing data.
//! Edit src-tauri/src/db/migrations/remote_schema.sql, then run: pnpm db:push

#[tokio::main]
async fn main() {
    let _ = dotenvy::from_filename(".env.development");

    let url = std::env::var("TURSO_URL").unwrap_or_else(|_| {
        eprintln!("error: TURSO_URL is not set");
        std::process::exit(1);
    });
    let token = std::env::var("TURSO_TOKEN").unwrap_or_else(|_| {
        eprintln!("error: TURSO_TOKEN is not set");
        std::process::exit(1);
    });

    println!("Pushing schema to {url} (all data will be wiped)...");

    if let Err(e) = push(&url, &token).await {
        eprintln!("Failed: {e}");
        std::process::exit(1);
    }

    println!("Done.");
}

async fn push(url: &str, token: &str) -> anyhow::Result<()> {
    use libsql::{Builder, Connection};

    let db = Builder::new_remote(url.to_string(), token.to_string())
        .build()
        .await?;
    let conn = db.connect()?;

    // Drop every index and table that currently exists (reverse order for FK deps)
    drop_all(&conn).await?;

    // Apply the schema
    let schema = include_str!("../src/db/migrations/remote_schema.sql");
    run_statements(&conn, schema).await?;

    Ok(())
}

/// Query sqlite_master for all user-defined indexes and tables, then drop them.
async fn drop_all(conn: &libsql::Connection) -> anyhow::Result<()> {
    // Collect names first to avoid holding the cursor while executing drops
    let mut indexes: Vec<String> = Vec::new();
    let mut tables: Vec<String> = Vec::new();

    let mut rows = conn
        .query(
            "SELECT type, name FROM sqlite_master WHERE type IN ('index','table') AND name NOT LIKE 'sqlite_%'",
            (),
        )
        .await?;

    while let Some(row) = rows.next().await? {
        let kind: String = row.get(0)?;
        let name: String = row.get(1)?;
        if kind == "index" {
            indexes.push(name);
        } else {
            tables.push(name);
        }
    }

    for name in indexes {
        conn.execute(&format!("DROP INDEX IF EXISTS \"{name}\""), ()).await?;
    }

    // Drop tables in reverse creation order so child tables (FK deps) are
    // dropped before their parents — Turso ignores PRAGMA foreign_keys=OFF.
    for name in tables.iter().rev() {
        conn.execute(&format!("DROP TABLE IF EXISTS \"{name}\""), ()).await?;
    }

    Ok(())
}

async fn run_statements(conn: &libsql::Connection, sql: &str) -> anyhow::Result<()> {
    for raw in sql.split(';') {
        let stmt: String = raw
            .lines()
            .filter(|l| !l.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        let stmt = stmt.trim();
        if !stmt.is_empty() {
            conn.execute(stmt, ()).await.map_err(|e| {
                anyhow::anyhow!("Migration failed on statement:\n{stmt}\n\nError: {e}")
            })?;
        }
    }
    Ok(())
}
