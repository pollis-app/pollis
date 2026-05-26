// Spawn an Electron instance pointed at the built `dist/main.js`, with
// the per-instance isolation the Rust integration harness gets via
// `POLLIS_DATA_DIR`.
//
// Why we don't go through `pnpm dev`: that script depends on Vite at
// :5173, which Playwright would have to wait on before launching the
// Electron process. For tests we use the production Electron bundle
// against the static built frontend instead — same loadFile path
// production uses, no dev server in the loop.

import { _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { loadTestTursoEnv } from "./turso";

const REPO_ROOT = path.resolve(__dirname, "..", "..", "..");
const ELECTRON_DIR = path.join(REPO_ROOT, "electron");
const ELECTRON_MAIN = path.join(ELECTRON_DIR, "dist", "main.js");

export interface LaunchedApp {
  app: ElectronApplication;
  page: Page;
  /** Per-instance data dir. Owned by this LaunchedApp — `dispose()`
   *  removes it. */
  dataDir: string;
}

export interface LaunchOptions {
  /** When set, the Rust auth path short-circuits get_session() and logs in
   *  as this user without going through OTP. The user is created lazily
   *  on first call. Use a fresh per-scenario value (e.g.
   *  `alice-${uniqueSuffix()}@e2e.local`) so tests don't collide with
   *  state from other workers or earlier runs that escaped the wipe. */
  devEmail?: string;
  /** Override the data-dir leaf name. Defaults to a `pollis-e2e-${pid}-
   *  ${counter}` tmpdir. The leaf name also becomes the OS-keystore
   *  namespace prefix (see `pollis-core/src/keystore.rs::namespaced`),
   *  so two LaunchedApps with different dataDir leaves stay isolated. */
  dataDirLabel?: string;
}

let counter = 0;

/** Launch one Electron instance. Returns a handle to its first window
 *  plus a `dispose()` you should call from `test.afterEach`. */
export async function launchPollis(opts: LaunchOptions = {}): Promise<LaunchedApp> {
  if (!fs.existsSync(ELECTRON_MAIN)) {
    throw new Error(
      `Electron main bundle not found at ${ELECTRON_MAIN}. ` +
        "Run `pnpm build:pollis-node:debug && pnpm build:electron && pnpm --filter frontend build` first " +
        "(the `test:e2e` script does this automatically).",
    );
  }

  const label = opts.dataDirLabel ?? `pollis-e2e-${process.pid}-${++counter}`;
  const dataDir = path.join(os.tmpdir(), label);
  fs.mkdirSync(dataDir, { recursive: true });

  const { url: tursoUrl, token: tursoToken } = loadTestTursoEnv();

  // Pollis's `Config::from_env` requires R2 + Resend env vars at process
  // start even when those services are never touched. Fill them with
  // syntactically-valid placeholders; any test that actually exercises
  // an attachment upload or real OTP email will need to provide real
  // credentials. LiveKit is optional — leave blank and the realtime
  // subscribe path becomes a no-op (DM delivery still works via the
  // polling fallback).
  const env: NodeJS.ProcessEnv = {
    ...process.env,
    POLLIS_DATA_DIR: dataDir,
    DEV_OTP: process.env.DEV_OTP ?? "000000",
    TURSO_URL: tursoUrl,
    TURSO_TOKEN: tursoToken,
    R2_S3_ENDPOINT: process.env.R2_S3_ENDPOINT ?? "http://127.0.0.1:1/r2-placeholder",
    R2_ACCESS_KEY_ID: process.env.R2_ACCESS_KEY_ID ?? "e2e-placeholder",
    R2_SECRET_KEY: process.env.R2_SECRET_KEY ?? "e2e-placeholder",
    R2_PUBLIC_URL: process.env.R2_PUBLIC_URL ?? "http://127.0.0.1:1/r2-placeholder",
    RESEND_API_KEY: process.env.RESEND_API_KEY ?? "e2e-placeholder",
  };
  if (opts.devEmail !== undefined) {
    env.DEV_EMAIL = opts.devEmail;
  }

  const app = await electron.launch({
    args: [ELECTRON_MAIN],
    cwd: ELECTRON_DIR,
    env,
    timeout: 30_000,
  });

  // Hide every BrowserWindow the moment it's created and suppress the
  // macOS dock icon. Electron has no native headless mode, but the
  // renderer keeps painting whether or not the window is on screen,
  // and `page.screenshot()` uses CDP under the hood so it sees the
  // offscreen surface fine. Done entirely from the test side via
  // `app.evaluate` so the application code stays unaware of tests.
  //
  // Why move offscreen + hide rather than just `.hide()`: some
  // Chromium scheduling paths (rAF, IntersectionObserver) throttle for
  // hidden windows, which can starve the React mount enough to slow a
  // test. Off-screen but "shown" keeps the scheduler happy.
  //
  // Set `POLLIS_E2E_HEADED=1` in the env to disable this and watch the
  // Electron windows live — useful for debugging selector failures or
  // running under `playwright test --ui`.
  if (process.env.POLLIS_E2E_HEADED !== "1") {
    await app.evaluate(({ app: electronApp, BrowserWindow }) => {
    if (process.platform === "darwin" && electronApp.dock) {
      electronApp.dock.hide();
    }
    const stash = (w: Electron.BrowserWindow) => {
      try {
        w.setPosition(-10000, -10000);
        w.setSkipTaskbar(true);
        w.setAlwaysOnTop(false);
        // Belt: if the window is in the process of being shown, push
        // it back off-screen after the show event lands.
        w.on("show", () => {
          try {
            w.setPosition(-10000, -10000);
          } catch {
            /* window may have been destroyed */
          }
        });
      } catch {
        /* window may have been destroyed */
      }
    };
    for (const w of BrowserWindow.getAllWindows()) {
      stash(w);
    }
    electronApp.on("browser-window-created", (_event, win) => {
      stash(win);
    });
    });
  }

  // Main.ts in dev mode opens DevTools as a separate detached window
  // alongside the main app window. `firstWindow()` is racey across the
  // two; wait for the one matching our renderer URL instead. Once
  // packaged builds get added to the matrix this predicate widens to
  // match `file://` too.
  const page = await app.waitForEvent("window", {
    predicate: (p) => p.url().startsWith("http://localhost:5173"),
    timeout: 30_000,
  });
  await page.waitForLoadState("domcontentloaded");

  return { app, page, dataDir };
}

/** Close the Electron process and remove its data-dir. Safe to call
 *  more than once. */
export async function dispose(handle: LaunchedApp): Promise<void> {
  try {
    await handle.app.close();
  } catch {
    // Best-effort — app may have already exited.
  }
  try {
    fs.rmSync(handle.dataDir, { recursive: true, force: true });
  } catch {
    // Best-effort — Windows occasionally holds an handle for a second
    // longer than the process exit.
  }
}

/** Short, lowercase, URL-safe random suffix for per-scenario test
 *  identifiers (emails, dataDir labels). 6 chars of base36 is plenty
 *  given each suite wipes the test Turso. */
export function uniqueSuffix(): string {
  return Math.floor(Math.random() * 36 ** 6)
    .toString(36)
    .padStart(6, "0");
}
