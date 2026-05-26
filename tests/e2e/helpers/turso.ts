// Wipe the shared disposable test Turso DB between scenarios.
//
// Mirrors `wipe_remote` in `src-tauri/src/test_harness.rs`: order matters
// because Turso enforces FK constraints when `PRAGMA foreign_keys = ON` is
// set on a per-connection basis (it isn't on the libsql HTTP path today,
// but ordering correctly keeps this resilient if it ever gets flipped on).
// Keep this list in lockstep with the harness `tables` array; the two are
// the source of truth for every scenario-level reset across both test
// surfaces.

import { createClient, type Client } from "@libsql/client";
import * as fs from "node:fs";
import * as path from "node:path";

const TABLES_DELETE_ORDER = [
  "message_reaction",
  "group_invite",
  "group_join_request",
  "user_preferences",
  "user_block",
  "dm_channel_member",
  "dm_channel",
  "message_envelope",
  "conversation_watermark",
  "mls_commit_log",
  "mls_welcome",
  "mls_key_package",
  "mls_group_info",
  "device_enrollment_request",
  "security_event",
  "account_recovery",
  "user_device",
  "channels",
  "group_member",
  "groups",
  "attachment_object",
  "users",
];

let cachedClient: Client | null = null;

function envFromFile(file: string): Record<string, string> {
  // Lightweight `.env` parser — only handles `KEY=value` and `KEY="value"`
  // lines. Good enough for our purpose; we don't want to pull in dotenv
  // as a workspace dep just for two values.
  const out: Record<string, string> = {};
  if (!fs.existsSync(file)) {
    return out;
  }
  const raw = fs.readFileSync(file, "utf8");
  for (const line of raw.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) {
      continue;
    }
    const eq = trimmed.indexOf("=");
    if (eq < 0) {
      continue;
    }
    const key = trimmed.slice(0, eq).trim();
    let value = trimmed.slice(eq + 1).trim();
    if (
      (value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'"))
    ) {
      value = value.slice(1, -1);
    }
    out[key] = value;
  }
  return out;
}

/** Resolve the TURSO_URL / TURSO_TOKEN for the test DB. Reads
 *  `.env.test` at the repo root — the same file the Rust integration
 *  harness consumes. */
export function loadTestTursoEnv(): {
  url: string;
  token: string;
} {
  const envPath = path.resolve(__dirname, "..", "..", "..", ".env.test");
  const env = envFromFile(envPath);
  const url = process.env.TURSO_URL || env.TURSO_URL;
  const token = process.env.TURSO_TOKEN || env.TURSO_TOKEN;
  if (!url || !token) {
    throw new Error(
      `Missing TURSO_URL / TURSO_TOKEN — checked process.env and ${envPath}. ` +
        "The E2E suite reuses the Rust integration harness's disposable test " +
        "Turso instance.",
    );
  }
  return { url, token };
}

function getClient(): Client {
  if (cachedClient !== null) {
    return cachedClient;
  }
  const { url, token } = loadTestTursoEnv();
  cachedClient = createClient({ url, authToken: token });
  return cachedClient;
}

/** Delete every row from every Pollis table in the disposable test Turso
 *  DB. Call from a `test.beforeEach` (or `test.beforeAll` for a suite
 *  that wants to share state across scenarios).
 *
 *  Retries each table up to 2 times on transient libsql errors (HTTP 502
 *  / 503 / 504, Hrana stream eviction). The Rust integration harness
 *  has the same dance in `wipe_remote`; without it, a suite occasionally
 *  trips on a Turso edge restart and aborts before the first assertion. */
export async function wipeTestTurso(): Promise<void> {
  for (const table of TABLES_DELETE_ORDER) {
    let attempt = 0;
    while (true) {
      try {
        const client = getClient();
        await client.execute(`DELETE FROM ${table}`);
        break;
      } catch (e) {
        if (attempt >= 2 || !isTransientLibsqlError(e)) {
          throw new Error(`wipe ${table}: ${stringifyError(e)}`);
        }
        attempt += 1;
        // Drop the cached client so the next attempt opens a fresh
        // HTTP connection — Hrana stream-gone errors stick to the
        // socket otherwise.
        await closeTursoClient();
        await new Promise((r) => setTimeout(r, 500 * attempt));
      }
    }
  }
}

function isTransientLibsqlError(e: unknown): boolean {
  const msg = stringifyError(e).toLowerCase();
  return (
    msg.includes("502") ||
    msg.includes("503") ||
    msg.includes("504") ||
    msg.includes("stream not found") ||
    msg.includes("stream is closed") ||
    msg.includes("connection reset") ||
    msg.includes("econnreset") ||
    msg.includes("etimedout") ||
    msg.includes("server_error")
  );
}

function stringifyError(e: unknown): string {
  if (e instanceof Error) {
    return `${e.message}${e.cause ? ` (cause: ${stringifyError(e.cause)})` : ""}`;
  }
  return String(e);
}

/** Drop the cached client. Useful between worker boundaries. */
export async function closeTursoClient(): Promise<void> {
  if (cachedClient !== null) {
    cachedClient.close();
    cachedClient = null;
  }
}
