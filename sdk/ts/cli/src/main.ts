#!/usr/bin/env node
/**
 * `pollis`: the Pollis command line for App developers.
 *
 *   pollis app new <name> --lang ts   a new App: one handler, a test, a README
 *   pollis app build                  compile to dist/app.wasm + dist/pollis-app.json, print the App hash
 *   pollis app test                   build, then run the tests offline in the real sandbox
 *   pollis app dev                    (not yet available)
 *   pollis app run                    (not yet available)
 */

import { parseArgs } from "node:util";

import { BuildError, buildApp } from "./build.js";
import { DevToolMissing } from "./devtool.js";
import { NewError, newApp } from "./new.js";
import { testApp } from "./test.js";

const HELP = `pollis app <command>

  new <name> --lang ts   Create an App: one handler, a test and a README.
  build                  Compile the App to dist/app.wasm and write dist/pollis-app.json.
                         Prints the App hash an admin approves.
  test [files...]        Build, then run the App's tests offline in the real sandbox.
  dev                    Run the App against a test group, rebuilding on save. Not yet available.
  run                    Run an approved App for a real group. Not yet available.

Options:
  --dir <path>           The App's directory (default: the current directory).
  --no-build             test: use the existing dist/ instead of building first.
  --sdk <path>           new: use the SDK from a Pollis checkout (its sdk/ts directory).

Guide: https://github.com/pollis-app/pollis/blob/main/docs/bots/developer-guide.md`;

const NOT_YET: Record<string, string> = {
  dev:
    "`pollis app dev` is not available yet. It runs your App in a local bot against a test group,\n" +
    "rebuilding on save, as a development installation; that needs App approval in the Pollis apps,\n" +
    "which is still being built. Until then, `pollis app test` runs your handlers offline in the real sandbox.",
  run:
    "`pollis app run` is not available yet. It runs an App for a real group once an admin approves its\n" +
    "App hash in the Pollis app; that approval step is still being built. `pollis app build` already\n" +
    "prints the App hash an admin will approve.",
};

async function main(argv: string[]): Promise<number> {
  const { values, positionals } = parseArgs({
    args: argv,
    allowPositionals: true,
    options: {
      dir: { type: "string" },
      lang: { type: "string" },
      sdk: { type: "string" },
      "no-build": { type: "boolean" },
      help: { type: "boolean", short: "h" },
    },
  });
  const [group, command, ...rest] = positionals;
  if (values.help || group !== "app" || !command) {
    console.log(HELP);
    return values.help ? 0 : 2;
  }
  const dir = values.dir ?? ".";
  switch (command) {
    case "new": {
      const name = rest[0];
      if (!name) {
        console.error("usage: pollis app new <name> --lang ts");
        return 2;
      }
      if (!values.lang) {
        console.error("say which language with --lang (TypeScript: --lang ts)");
        return 2;
      }
      const target = newApp({ name, lang: values.lang, parent: values.dir, sdk: values.sdk });
      console.log(`Created ${target}\n\nNext:\n  cd ${name}\n  pnpm install\n  pnpm test`);
      return 0;
    }
    case "build": {
      const built = await buildApp({ dir });
      console.log(
        `built ${built.manifest.name} ${built.manifest.version}\n` +
          `  dist/app.wasm         ${(built.bytes / 1024 / 1024).toFixed(1)} MB\n` +
          `  dist/pollis-app.json\n` +
          `App hash: ${built.appHash}`,
      );
      return 0;
    }
    case "test":
      return testApp({ dir, build: !values["no-build"], patterns: rest });
    case "dev":
    case "run":
      console.error(NOT_YET[command]);
      return 2;
    default:
      console.error(`unknown command \`pollis app ${command}\`\n\n${HELP}`);
      return 2;
  }
}

main(process.argv.slice(2)).then(
  (code) => {
    process.exitCode = code;
  },
  (e: unknown) => {
    if (e instanceof BuildError || e instanceof NewError || e instanceof DevToolMissing) {
      console.error(`error: ${e.message}`);
    } else {
      console.error(e);
    }
    process.exitCode = 1;
  },
);
