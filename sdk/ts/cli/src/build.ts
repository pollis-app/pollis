/**
 * `pollis app build`: an App's source in, a sandboxed component and its manifest
 * out. Rust Apps compile in `rust.ts`; both languages end at the same check.
 *
 * For TypeScript:
 *
 * 1. Bundle the App with esbuild (refusing Node built-ins) and import it in
 *    Node to read `app({ manifest })`; check it and write `pollis-app.json`.
 * 2. Bundle it again for the sandbox, behind a generated entry that exports
 *    the six `pollis:app@0.1.0` handlers.
 * 3. Compile that with ComponentizeJS (StarlingMonkey), pre-initialized by
 *    `pollis-app-dev` with fixed randomness and clocks, in a fixed staging
 *    directory: the same source and lockfile give the same bytes.
 * 4. Load the component in the real App host (`pollis-app-dev check`), so an
 *    App the runtime would refuse is refused here, and print its App hash.
 */

import { builtinModules, createRequire } from "node:module";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, relative, resolve } from "node:path";
import { pathToFileURL } from "node:url";

import { build as esbuild, type Plugin } from "esbuild";
import { isApp, type AppDefinition } from "@pollis-inc/app";
import { HANDLER_FOR, ManifestError, resolveManifest, type ResolvedManifest } from "@pollis-inc/app/manifest";

import { devTool, findDevTool } from "./devtool.js";
import { BuildError } from "./errors.js";
import { compileRust, isRustApp } from "./rust.js";

export { BuildError };

export interface BuildOptions {
  /** Project directory (holds package.json). */
  dir: string;
  /** App entry, relative to `dir`. Default: package.json `pollis.entry`, else src/app.ts. */
  entry?: string;
  /** Output directory, relative to `dir`. Default dist. */
  out?: string;
  quiet?: boolean;
}

export interface BuildResult {
  component: string;
  manifestPath: string;
  manifest: ResolvedManifest;
  appHash: string;
  bytes: number;
}

/**
 * Packages are resolved as for a browser: the sandbox has web basics
 * (`crypto.getRandomValues`, TextEncoder, timers) and no Node built-ins, so a
 * package's browser build is usually the one that works. HTTP is ctx.fetch.
 */
const MAIN_FIELDS = ["browser", "module", "main"];
const CONDITIONS = ["browser", "import", "default"];

const BUILTINS = new Set(builtinModules.flatMap((m) => [m, `node:${m}`]));

/** What each Node built-in is usually reached for, and what to use instead. */
function builtinAdvice(name: string): string {
  const bare = name.replace(/^node:/, "").split("/")[0];
  switch (bare) {
    case "fs":
    case "path":
      return "Apps have no files. Keep data in ctx.state (sealed key-value state, `state.quota` in the manifest).";
    case "net":
    case "http":
    case "https":
    case "http2":
    case "dgram":
    case "dns":
    case "tls":
      return "Apps have no sockets. Use ctx.fetch, which reaches only the hosts listed in `egress`.";
    case "child_process":
    case "worker_threads":
    case "cluster":
      return "Apps cannot start processes or threads.";
    case "crypto":
      return "Use the web's crypto.getRandomValues() (built in) or a pure-JS library.";
    default:
      return "Apps run in a sandbox with no files, sockets or processes; only pure-JS packages work.";
  }
}

/** Refuses every Node built-in, naming the importer and saying why. */
export function noNodeBuiltins(): Plugin {
  return {
    name: "pollis-no-node-builtins",
    setup(build) {
      build.onResolve({ filter: /.*/ }, (args) => {
        if (!BUILTINS.has(args.path)) {
          return undefined;
        }
        return {
          errors: [
            {
              text: `\`${args.path}\` is a Node built-in, and Pollis Apps cannot use Node built-ins. ${builtinAdvice(args.path)}`,
            },
          ],
        };
      });
    },
  };
}

function readJson(path: string): Record<string, unknown> {
  return JSON.parse(readFileSync(path, "utf8")) as Record<string, unknown>;
}

function formatEsbuildError(e: unknown): string {
  const errors = (e as { errors?: { text: string; location?: { file: string; line: number; column: number } | null }[] }).errors;
  if (!errors?.length) {
    return (e as Error).message ?? String(e);
  }
  return errors
    .map((err) => (err.location ? `${err.location.file}:${err.location.line}:${err.location.column}: ${err.text}` : err.text))
    .join("\n");
}

/** Bundle for Node and import it, to read the App's definition. */
async function loadDefinition(dir: string, entry: string, scratch: string): Promise<AppDefinition> {
  const outfile = join(scratch, "manifest-probe.mjs");
  try {
    await esbuild({
      absWorkingDir: dir,
      entryPoints: [entry],
      bundle: true,
      platform: "neutral",
      mainFields: MAIN_FIELDS,
      conditions: CONDITIONS,
      format: "esm",
      target: "es2022",
      outfile,
      logLevel: "silent",
      plugins: [noNodeBuiltins()],
    });
  } catch (e) {
    throw new BuildError(formatEsbuildError(e));
  }
  const mod = (await import(pathToFileURL(outfile).href + `?t=${Date.now()}`)) as { default?: unknown };
  if (!isApp(mod.default)) {
    throw new BuildError(`${entry} must \`export default app({ manifest, ...handlers })\` from "@pollis-inc/app"`);
  }
  return mod.default;
}

/** The generated module the component is built from. */
function entrySource(entry: string): string {
  const handlers = Object.values(HANDLER_FOR);
  return [
    `import definition from ${JSON.stringify("./" + entry.replace(/\\/g, "/"))};`,
    `import { exportsFor } from "@pollis-inc/app/runtime";`,
    `const handlers = exportsFor(definition);`,
    ...handlers.map((h) => `export function ${h}(arg) { return handlers.${h}(arg); }`),
    "",
  ].join("\n");
}

/** Where ComponentizeJS runs. Fixed, because its paths end up in the component. */
function stageDir(): string {
  return process.platform === "win32" ? join(tmpdir(), "pollis-app-build") : "/tmp/pollis-app-build";
}

/** Hold the staging directory for one build at a time. */
function lockStage(stage: string): () => void {
  const lock = `${stage}.lock`;
  try {
    mkdirSync(lock);
  } catch {
    let owner = 0;
    try {
      owner = Number(readFileSync(join(lock, "pid"), "utf8"));
    } catch {
      // Lock directory without a pid: a build that crashed while starting.
    }
    let alive = false;
    if (owner > 0) {
      try {
        process.kill(owner, 0);
        alive = true;
      } catch {
        alive = false;
      }
    }
    if (alive) {
      throw new BuildError(`another \`pollis app build\` is running (pid ${owner}); run this one when it finishes`);
    }
    rmSync(lock, { recursive: true, force: true });
    mkdirSync(lock);
  }
  writeFileSync(join(lock, "pid"), String(process.pid));
  return () => rmSync(lock, { recursive: true, force: true });
}

/** A seeded `Math.random`, for ComponentizeJS's work-directory name. */
function seededRandom(): () => number {
  let seed = 1;
  return () => {
    seed = (seed * 16807) % 2147483647;
    return seed / 2147483647;
  };
}

async function componentize(bundle: string, wit: string, wizerBin: string): Promise<Uint8Array> {
  const { componentize: run } = await import("@bytecodealliance/componentize-js");
  const stage = stageDir();
  const unlock = lockStage(stage);
  const cwd = process.cwd();
  const tmp = process.env.TMPDIR;
  const random = Math.random;
  try {
    rmSync(stage, { recursive: true, force: true });
    mkdirSync(join(stage, "src"), { recursive: true });
    writeFileSync(join(stage, "src", "app.js"), bundle);
    process.chdir(join(stage, "src"));
    process.env.TMPDIR = stage;
    Math.random = seededRandom();
    // `witWorld` (the WIT as text) is supported but missing from the package's
    // types. A path (`witPath`) would be read through ComponentizeJS's WASI
    // shim, which cannot see arbitrary host paths.
    const { component } = await run({
      sourcePath: "app.js",
      witWorld: wit,
      worldName: "app",
      // No wasi:http: the App host refuses it. Network access is ctx.fetch.
      disableFeatures: ["http", "fetch-event"],
      wizerBin,
    } as unknown as { sourcePath: string });
    return component;
  } finally {
    Math.random = random;
    process.chdir(cwd);
    if (tmp === undefined) {
      delete process.env.TMPDIR;
    } else {
      process.env.TMPDIR = tmp;
    }
    rmSync(stage, { recursive: true, force: true });
    unlock();
  }
}

function log(quiet: boolean | undefined, line: string): void {
  if (!quiet) {
    console.log(line);
  }
}

/**
 * Write the build out and load it in the real App host, so an App the runtime
 * would refuse is refused here, then compute its App hash.
 */
function finishBuild(bin: string, out: string, manifest: ResolvedManifest, component: Uint8Array): BuildResult {
  const manifestPath = join(out, "pollis-app.json");
  writeFileSync(manifestPath, JSON.stringify(manifest, null, 2) + "\n");
  const componentPath = join(out, "app.wasm");
  writeFileSync(componentPath, component);
  // A development signature from an earlier `pollis app dev` signs another App hash. Left beside
  // this build it makes a development installation refuse the build as tampered instead of
  // offering it to an admin; `pollis app dev` signs each new build itself.
  rmSync(`${componentPath}.sig.json`, { force: true });

  try {
    devTool(bin, ["check", componentPath, manifestPath]);
  } catch (e) {
    rmSync(componentPath, { force: true });
    throw new BuildError(`the App host refuses this build: ${(e as Error).message}`);
  }
  const appHash = devTool(bin, ["hash", componentPath, manifestPath]);
  return { component: componentPath, manifestPath, manifest, appHash, bytes: component.length };
}

async function buildRustApp(dir: string, options: BuildOptions): Promise<BuildResult> {
  const out = resolve(dir, options.out ?? "dist");
  const bin = findDevTool();
  mkdirSync(out, { recursive: true });
  const scratch = join(out, ".build");
  rmSync(scratch, { recursive: true, force: true });
  mkdirSync(scratch, { recursive: true });
  try {
    log(options.quiet, `compiling ${relative(process.cwd(), dir) || "."} to a component...`);
    const { manifest, component } = compileRust(dir, scratch);
    return finishBuild(bin, out, manifest, component);
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

export async function buildApp(options: BuildOptions): Promise<BuildResult> {
  const dir = resolve(options.dir);
  if (isRustApp(dir)) {
    return buildRustApp(dir, options);
  }
  const pkgPath = join(dir, "package.json");
  if (!existsSync(pkgPath)) {
    throw new BuildError(`no App in ${dir} (no package.json, or Cargo.toml that depends on pollis-app); run this in your App's directory`);
  }
  const pkg = readJson(pkgPath);
  const configured = (pkg.pollis as { entry?: string } | undefined)?.entry;
  const entry = options.entry ?? configured ?? "src/app.ts";
  if (!existsSync(join(dir, entry))) {
    throw new BuildError(`no App entry at ${entry} (set "pollis": { "entry": "..." } in package.json)`);
  }
  const out = resolve(dir, options.out ?? "dist");
  const bin = findDevTool();
  mkdirSync(out, { recursive: true });
  const scratch = join(out, ".build");
  rmSync(scratch, { recursive: true, force: true });
  mkdirSync(scratch, { recursive: true });

  try {
    // 1. The manifest, from the code.
    const definition = await loadDefinition(dir, entry, scratch);
    const handlers = Object.keys(definition).filter(
      (k) => (Object.values(HANDLER_FOR) as string[]).includes(k) && typeof (definition as unknown as Record<string, unknown>)[k] === "function",
    );
    let manifest: ResolvedManifest;
    try {
      manifest = resolveManifest(definition.manifest, handlers, {
        name: pkg.name as string | undefined,
        version: pkg.version as string | undefined,
        description: pkg.description as string | undefined,
      });
    } catch (e) {
      if (e instanceof ManifestError) {
        throw new BuildError(`manifest: ${e.message}`);
      }
      throw e;
    }

    // 2. The sandbox bundle.
    let bundle: string;
    try {
      const result = await esbuild({
        absWorkingDir: dir,
        stdin: { contents: entrySource(entry), resolveDir: dir, sourcefile: "pollis-app-entry.js", loader: "js" },
        bundle: true,
        write: false,
        platform: "neutral",
        mainFields: MAIN_FIELDS,
        conditions: CONDITIONS,
        format: "esm",
        target: "es2022",
        charset: "utf8",
        legalComments: "none",
        // Drops esbuild's "// path/to/file.ts" comments, whose paths depend on
        // where the dependencies are installed; names stay readable.
        minifyWhitespace: true,
        external: ["pollis:app/*"],
        logLevel: "silent",
        plugins: [noNodeBuiltins()],
      });
      bundle = result.outputFiles[0].text;
    } catch (e) {
      throw new BuildError(formatEsbuildError(e));
    }
    writeFileSync(join(scratch, "app.js"), bundle);

    // 3. The component.
    const require = createRequire(join(dir, "package.json"));
    let witPath: string;
    try {
      witPath = require.resolve("@pollis-inc/app/wit/app.wit");
    } catch {
      throw new BuildError("@pollis-inc/app is not installed in this project; add it with `pnpm add @pollis-inc/app`");
    }
    log(options.quiet, `compiling ${relative(process.cwd(), join(dir, entry)) || entry} to a component...`);
    let component: Uint8Array;
    try {
      component = await componentize(bundle, readFileSync(witPath, "utf8"), bin);
    } catch (e) {
      throw new BuildError(`compiling to a component failed:\n${(e as Error).message}`);
    }

    // 4. The real host's verdict, and the App hash.
    return finishBuild(bin, out, manifest, component);
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}
