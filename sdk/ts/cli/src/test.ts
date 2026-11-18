/**
 * `pollis app test`: build the App, then run its `*.test.ts` files with Node's
 * test runner, wired to the build through `@pollis-inc/app/testing`.
 */

import { spawnSync } from "node:child_process";
import { globSync } from "node:fs";
import { join, resolve } from "node:path";

import { buildApp } from "./build.js";
import { findDevTool } from "./devtool.js";

export interface TestOptions {
  dir: string;
  build: boolean;
  /** Test file globs, relative to `dir`. */
  patterns: string[];
}

export async function testApp(options: TestOptions): Promise<number> {
  const dir = resolve(options.dir);
  const bin = findDevTool();
  const component = join(dir, "dist", "app.wasm");
  const manifest = join(dir, "dist", "pollis-app.json");
  if (options.build) {
    const built = await buildApp({ dir, quiet: true });
    console.log(`built ${built.manifest.name} ${built.manifest.version} (App hash ${built.appHash})`);
  }
  const patterns = options.patterns.length > 0 ? options.patterns : ["src/**/*.test.ts", "test/**/*.test.ts"];
  const files = patterns.flatMap((p) => globSync(p, { cwd: dir })).sort();
  if (files.length === 0) {
    console.error(`no test files matching ${patterns.join(", ")}`);
    return 1;
  }
  const result = spawnSync(process.execPath, ["--test", "--test-concurrency=1", ...files], {
    cwd: dir,
    stdio: "inherit",
    env: {
      ...process.env,
      // Report to this terminal even when `pollis app test` itself runs under
      // a test runner.
      NODE_TEST_CONTEXT: undefined,
      POLLIS_APP_COMPONENT: component,
      POLLIS_APP_MANIFEST: manifest,
      POLLIS_APP_DEV_BIN: bin,
    },
  });
  return result.status ?? 1;
}
