/**
 * `pollis app build` and `test` for an App written in Rust.
 *
 * 1. Run the App's generated `__pollis_app_describe` test natively, which
 *    writes what `pollis_app::app!` declares, and resolve the manifest with the
 *    same code a TypeScript App uses.
 * 2. `cargo build --target wasm32-wasip2`: rustc links a component directly, so
 *    an App developer needs no extra tool. Source paths are remapped out of the
 *    binary and the lockfile is enforced, so the same source, lockfile and
 *    toolchain give the same bytes wherever they are built.
 */

import { spawnSync, type SpawnSyncReturns } from "node:child_process";
import { existsSync, readFileSync, realpathSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";

import { ManifestError, resolveManifest, type Manifest, type ResolvedManifest } from "@pollis-inc/app/manifest";

import { BuildError } from "./errors.js";

const TARGET = "wasm32-wasip2";
const NO_CARGO = "`cargo` was not found. Rust Apps need Rust: install it from https://rustup.rs";

/** Whether `dir` holds a Rust App: a crate that depends on `pollis-app`. */
export function isRustApp(dir: string): boolean {
  const path = join(dir, "Cargo.toml");
  return existsSync(path) && /^\s*(pollis-app\s*[=.]|\[dependencies\.pollis-app\])/m.test(readFileSync(path, "utf8"));
}

function cargo(dir: string, args: string[], env: NodeJS.ProcessEnv = {}): SpawnSyncReturns<string> {
  const result = spawnSync("cargo", args, {
    cwd: dir,
    encoding: "utf8",
    maxBuffer: 256 * 1024 * 1024,
    env: { ...process.env, ...env },
  });
  if ((result.error as NodeJS.ErrnoException | undefined)?.code === "ENOENT") {
    throw new BuildError(NO_CARGO);
  }
  if (result.error) {
    throw result.error;
  }
  return result;
}

/** Cargo's error, with the fix for the mistakes a new App developer is likely to hit. */
function cargoFailure(what: string, stderr: string): BuildError {
  let advice = "";
  if (/no matching package named `pollis-app`/.test(stderr)) {
    advice =
      "\n\nThe `pollis-app` crate could not be downloaded. To use the copy in a Pollis checkout, create the App " +
      "with `pollis app new <name> --lang rust --sdk <checkout>/sdk/ts`.";
  } else if (/can't find crate for `(core|std)`|target may not be installed/.test(stderr)) {
    advice = `\n\nThe WebAssembly target is missing. Install it with: rustup target add ${TARGET}`;
  } else if (/lock file .* needs to be updated|--locked was passed/.test(stderr)) {
    advice = "\n\nCargo.lock does not match Cargo.toml. Run `cargo update --workspace`, then commit Cargo.lock.";
  }
  return new BuildError(`${what} failed:\n${stderr.trim()}${advice}`);
}

interface CargoPackage {
  name: string;
  source: string | null;
  manifest_path: string;
  targets: { name: string; crate_types: string[] }[];
}

interface CargoMetadata {
  packages: CargoPackage[];
  target_directory: string;
}

function metadata(dir: string): { root: CargoPackage; packages: CargoPackage[]; targetDir: string } {
  const result = cargo(dir, ["metadata", "--format-version", "1", "--locked"]);
  if (result.status !== 0) {
    throw cargoFailure("`cargo metadata`", result.stderr);
  }
  const meta = JSON.parse(result.stdout) as CargoMetadata;
  const manifest = join(realpathSync(dir), "Cargo.toml");
  const root = meta.packages.find((p) => p.manifest_path === manifest);
  if (!root) {
    throw new BuildError(`no package in ${manifest}; a Rust App is a single crate, not a virtual workspace`);
  }
  return { root, packages: meta.packages, targetDir: meta.target_directory };
}

/** Handler names as Rust spells them, in a message written for TypeScript. */
function rustNames(message: string): string {
  return message.replace(/\bon[A-Z][a-z]+\b/g, (name) => name.replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`));
}

interface Description {
  manifest: Manifest;
  handlers: string[];
  package: { name: string; version: string; description: string };
}

/** Read the manifest out of the code, by running the App's describe test natively. */
function describe(dir: string, scratch: string): ResolvedManifest {
  const out = join(scratch, "describe.json");
  const result = cargo(dir, ["test", "--locked", "--lib", "--quiet", "--", "__pollis_app_describe"], {
    POLLIS_APP_DESCRIBE_OUT: out,
  });
  if (result.status !== 0) {
    throw cargoFailure("compiling the App", result.stderr || result.stdout);
  }
  if (!existsSync(out)) {
    throw new BuildError("src/lib.rs must call `pollis_app::app! { manifest: ..., on_command: ... }` at the root of the crate");
  }
  const described = JSON.parse(readFileSync(out, "utf8")) as Description;
  try {
    return resolveManifest(described.manifest, described.handlers, described.package);
  } catch (e) {
    if (e instanceof ManifestError) {
      throw new BuildError(`manifest: ${rustNames(e.message)}`);
    }
    throw e;
  }
}

/**
 * The flags that keep machine-specific paths out of the component. Later
 * remaps win, so the more specific paths come after the App's own directory.
 */
function rustFlags(dir: string, packages: CargoPackage[], targetDir: string): string {
  const cargoHome = process.env.CARGO_HOME ?? join(homedir(), ".cargo");
  const remaps: [string, string][] = [
    [realpathSync(dir), "/app"],
    [cargoHome, "/cargo"],
    ...packages
      .filter((p) => p.source === null)
      .map((p): [string, string] => [dirname(p.manifest_path), `/path/${p.name}`]),
    [targetDir, "/target"],
  ];
  const flags = ["-Cdebuginfo=0", "-Cstrip=symbols", ...remaps.map(([from, to]) => `--remap-path-prefix=${from}=${to}`)];
  // CARGO_ENCODED_RUSTFLAGS replaces anything set in RUSTFLAGS or .cargo/config.toml.
  return flags.join("\x1f");
}

export interface RustBuild {
  manifest: ResolvedManifest;
  component: Uint8Array;
}

/** Compile the Rust App in `dir` to a component and read its manifest. */
export function compileRust(dir: string, scratch: string): RustBuild {
  if (!existsSync(join(dir, "Cargo.lock"))) {
    const locked = cargo(dir, ["generate-lockfile"]);
    if (locked.status !== 0) {
      throw cargoFailure("`cargo generate-lockfile`", locked.stderr);
    }
  }
  const { root, packages, targetDir } = metadata(dir);
  const lib = root.targets.find((t) => t.crate_types.includes("cdylib"));
  if (!lib) {
    throw new BuildError('Cargo.toml needs a component library: add `[lib]` with `crate-type = ["cdylib"]`');
  }
  const manifest = describe(dir, scratch);
  const result = cargo(dir, ["build", "--locked", "--release", "--lib", "--target", TARGET], {
    CARGO_ENCODED_RUSTFLAGS: rustFlags(dir, packages, targetDir),
    SOURCE_DATE_EPOCH: "0",
  });
  if (result.status !== 0) {
    throw cargoFailure("compiling to a component", result.stderr);
  }
  const wasm = join(targetDir, TARGET, "release", `${lib.name.replaceAll("-", "_")}.wasm`);
  return { manifest, component: readFileSync(wasm) };
}

/** Run the App's Rust tests against the build in `dist/`. */
export function testRust(dir: string, patterns: string[], env: NodeJS.ProcessEnv): number {
  const filters = patterns.length > 0 ? ["--", ...patterns] : [];
  const result = spawnSync("cargo", ["test", "--locked", ...filters], {
    cwd: dir,
    stdio: "inherit",
    env: { ...process.env, ...env },
  });
  if ((result.error as NodeJS.ErrnoException | undefined)?.code === "ENOENT") {
    throw new BuildError(NO_CARGO);
  }
  return result.status ?? 1;
}
