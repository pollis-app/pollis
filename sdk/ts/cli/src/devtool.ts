/**
 * Finding and running `pollis-app-dev`, the native half of `pollis app`: the
 * offline test runner (the real App host), the deterministic build step and
 * the App hash. Built from the Pollis repo (`cargo build --release -p
 * pollis-app-dev`).
 */

import { spawnSync } from "node:child_process";
import { accessSync, constants, existsSync } from "node:fs";
import { delimiter, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const EXE = process.platform === "win32" ? "pollis-app-dev.exe" : "pollis-app-dev";

export class DevToolMissing extends Error {}

function executable(path: string): boolean {
  try {
    accessSync(path, constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

/** `$POLLIS_APP_DEV_BIN`, else `pollis-app-dev` on PATH, else a build in the Pollis repo this CLI lives in. */
export function findDevTool(): string {
  const fromEnv = process.env.POLLIS_APP_DEV_BIN;
  if (fromEnv) {
    if (!executable(fromEnv)) {
      throw new DevToolMissing(`POLLIS_APP_DEV_BIN is ${fromEnv}, which is not an executable file`);
    }
    return fromEnv;
  }
  for (const dir of (process.env.PATH ?? "").split(delimiter)) {
    if (dir && executable(join(dir, EXE))) {
      return join(dir, EXE);
    }
  }
  // Inside the Pollis repo (sdk/ts/cli/dist/devtool.js): its cargo target dir.
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
  throw new DevToolMissing(
    "`pollis-app-dev` was not found. It runs your App in the real sandbox for `build` and `test`.\n" +
      "Build it from the Pollis repo with `cargo build --release -p pollis-app-dev`, then put\n" +
      "target/release on your PATH or set POLLIS_APP_DEV_BIN to the binary.",
  );
}

/** Run a `pollis-app-dev` subcommand; returns stdout, throws with stderr on failure. */
export function devTool(bin: string, args: string[]): string {
  const result = spawnSync(bin, args, { encoding: "utf8" });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    throw new Error((result.stderr || result.stdout || `pollis-app-dev ${args[0]} failed`).trim());
  }
  return result.stdout.trim();
}
