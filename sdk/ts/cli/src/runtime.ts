/**
 * `pollis app run` and `pollis app dev`: hand a built App to the bot runtime,
 * `pollis-bot`, on this machine.
 *
 * - `run`: `pollis-bot app install --artifact <dir|url>` (prints the App hash
 *   an admin approves on the bot's page), then `pollis-bot run`.
 * - `dev`: for a development installation, whose approval names a developer
 *   key instead of one hash. `pollis-bot app dev-key` (created once, printed
 *   for the admin to approve), `app install --artifact dist`, then
 *   `pollis-bot run`, rebuilding and `pollis-bot app sign --artifact dist`
 *   on every save. The runtime checks the file and its signature before each
 *   job, so the next job runs the new build.
 */

import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { accessSync, constants, existsSync, watch } from "node:fs";
import { delimiter, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { BuildError, buildApp } from "./build.js";

const EXE = process.platform === "win32" ? "pollis-bot.exe" : "pollis-bot";

function executable(path: string): boolean {
  try {
    accessSync(path, constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

/** `$POLLIS_BOT_BIN`, else `pollis-bot` on PATH, else a build in the Pollis repo this CLI lives in. */
export function findBot(): string {
  const fromEnv = process.env.POLLIS_BOT_BIN;
  if (fromEnv) {
    if (!executable(fromEnv)) {
      throw new BuildError(`POLLIS_BOT_BIN is ${fromEnv}, which is not an executable file`);
    }
    return fromEnv;
  }
  for (const dir of (process.env.PATH ?? "").split(delimiter)) {
    if (dir && executable(join(dir, EXE))) {
      return join(dir, EXE);
    }
  }
  let dir = dirname(fileURLToPath(import.meta.url));
  for (let i = 0; i < 6; i++) {
    for (const profile of ["release", "debug"]) {
      const candidate = join(dir, "target", profile, EXE);
      if (existsSync(join(dir, "Cargo.toml")) && executable(candidate)) {
        return candidate;
      }
    }
    dir = dirname(dir);
  }
  throw new BuildError(
    "`pollis-bot` was not found. It is the runtime that runs your App for a group.\n" +
      "Install it (docs/bots/self-hosting.md), put it on your PATH, or set POLLIS_BOT_BIN.",
  );
}

export interface RuntimeOptions {
  dir: string;
  installation?: string;
  /** `pollis-bot --data-dir`, if not the default. */
  dataDir?: string;
}

function botArgs(options: RuntimeOptions, args: string[]): string[] {
  return options.dataDir ? ["--data-dir", options.dataDir, ...args] : args;
}

function installation(options: RuntimeOptions): string {
  if (!options.installation) {
    throw new BuildError("say which installation with --installation <id> (`pollis-bot status` lists them)");
  }
  return options.installation;
}

/** Run a `pollis-bot` step to completion with its output on this terminal. */
function step(bot: string, options: RuntimeOptions, args: string[]): void {
  const result = spawnSync(bot, botArgs(options, args), { stdio: "inherit" });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    throw new BuildError(`\`pollis-bot ${args.slice(0, 2).join(" ")}\` failed (exit ${result.status})`);
  }
}

/** `pollis-bot run` in the foreground; resolves with its exit code. */
function runBot(bot: string, options: RuntimeOptions): { child: ChildProcess; done: Promise<number> } {
  const child = spawn(bot, botArgs(options, ["run"]), { stdio: "inherit" });
  const done = new Promise<number>((resolveDone, reject) => {
    child.on("error", reject);
    child.on("exit", (code, signal) => resolveDone(code ?? (signal ? 1 : 0)));
  });
  for (const sig of ["SIGINT", "SIGTERM"] as const) {
    process.once(sig, () => child.kill(sig));
  }
  return { child, done };
}

/**
 * `pollis app run`: install an artifact (default: build this App and use
 * `dist/`) for an installation, then run the runtime.
 */
export async function runApp(options: RuntimeOptions & { artifact?: string }): Promise<number> {
  const bot = findBot();
  const id = installation(options);
  let artifact = options.artifact;
  if (!artifact) {
    const built = await buildApp({ dir: options.dir, quiet: true });
    console.log(`built ${built.manifest.name} ${built.manifest.version} (App hash ${built.appHash})`);
    artifact = dirname(built.component);
  } else if (!/^https:\/\//.test(artifact)) {
    artifact = resolve(artifact);
  }
  step(bot, options, ["app", "install", "--installation", id, "--artifact", artifact]);
  return runBot(bot, options).done;
}

/**
 * `pollis app dev`: a development installation that follows your edits.
 * Rebuilds and re-signs on every save under the App's source; the runtime
 * picks the new build up at its next job.
 */
export async function devApp(options: RuntimeOptions): Promise<number> {
  const bot = findBot();
  const id = installation(options);
  const dir = resolve(options.dir);

  console.log("Your developer key (an admin approves it once, with Development installation on, on the bot's page):");
  step(bot, options, ["app", "dev-key"]);

  const build = async (): Promise<boolean> => {
    try {
      const built = await buildApp({ dir, quiet: true });
      step(bot, options, ["app", "sign", "--artifact", dirname(built.component)]);
      console.log(`[pollis app dev] built and signed ${built.manifest.name} ${built.manifest.version} (App hash ${built.appHash})`);
      return true;
    } catch (e) {
      console.error(`[pollis app dev] build failed; the bot keeps the last good build:\n${(e as Error).message}`);
      return false;
    }
  };
  if (!(await build())) {
    return 1;
  }
  step(bot, options, ["app", "install", "--installation", id, "--artifact", join(dir, "dist")]);

  // Rebuild on save: one build at a time, and edits made during a build
  // trigger exactly one more.
  let building = false;
  let again = false;
  const rebuild = async (): Promise<void> => {
    if (building) {
      again = true;
      return;
    }
    building = true;
    do {
      again = false;
      await build();
    } while (again);
    building = false;
  };
  const watched = ["src", "package.json", "Cargo.toml", "Cargo.lock"].map((p) => join(dir, p)).filter((p) => existsSync(p));
  const watchers = watched.map((p) =>
    watch(p, { recursive: true }, () => {
      void rebuild();
    }),
  );
  console.log(`[pollis app dev] watching ${watched.map((p) => p.slice(dir.length + 1)).join(", ")}; Ctrl+C stops`);

  const { done } = runBot(bot, options);
  const code = await done;
  for (const w of watchers) {
    w.close();
  }
  return code;
}
