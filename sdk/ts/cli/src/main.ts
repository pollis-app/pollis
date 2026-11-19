#!/usr/bin/env node
/**
 * `pollis`: the Pollis command line for App developers.
 *
 *   pollis app new <name> --lang ts|rust   a new App: one handler, a test, a README
 *   pollis app build                  compile to dist/app.wasm + dist/pollis-app.json, print the App hash
 *   pollis app test                   build, then run the tests offline in the real sandbox
 *   pollis app dev --installation ID  development installation: rebuild and sign on save, in pollis-bot
 *   pollis app run --installation ID  install the build in pollis-bot (prints the App hash) and run it
 */

import { parseArgs } from "node:util";

import { BuildError, buildApp } from "./build.js";
import { DevToolMissing } from "./devtool.js";
import { NewError, newApp } from "./new.js";
import { devApp, runApp } from "./runtime.js";
import { testApp } from "./test.js";

const HELP = `pollis app <command>

  new <name> --lang ts|rust
                         Create an App in TypeScript or Rust: one handler, a test and a README.
  build                  Compile the App to dist/app.wasm and write dist/pollis-app.json.
                         Prints the App hash an admin approves.
  test [files...]        Build, then run the App's tests offline in the real sandbox.
  dev --installation ID  Run the App in this machine's bot as a development installation:
                         prints your developer key (an admin approves it once), then
                         rebuilds and signs on every save. Needs pollis-bot.
  run --installation ID  Hand the App to this machine's bot and run it. Prints the App hash
                         an admin approves on the bot's page. Needs pollis-bot.

Options:
  --dir <path>           The App's directory (default: the current directory).
  --no-build             test: use the existing dist/ instead of building first.
  --sdk <path>           new: use the SDK from a Pollis checkout (its sdk/ts directory).
  --artifact <dir|url>   run: a built App (a directory or an https:// URL) instead of building this one.
  --data-dir <path>      dev, run: pollis-bot's data directory, if not its default.

Guide: https://github.com/pollis-app/pollis/blob/main/docs/bots/developer-guide.md`;

async function main(argv: string[]): Promise<number> {
  const { values, positionals } = parseArgs({
    args: argv,
    allowPositionals: true,
    options: {
      dir: { type: "string" },
      lang: { type: "string" },
      sdk: { type: "string" },
      "no-build": { type: "boolean" },
      installation: { type: "string" },
      artifact: { type: "string" },
      "data-dir": { type: "string" },
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
        console.error("usage: pollis app new <name> --lang ts|rust");
        return 2;
      }
      if (!values.lang) {
        console.error("say which language with --lang (TypeScript: --lang ts, Rust: --lang rust)");
        return 2;
      }
      const target = newApp({ name, lang: values.lang, parent: values.dir, sdk: values.sdk });
      const next = values.lang === "rust" ? "npx pollis app test" : "pnpm install\n  pnpm test";
      console.log(`Created ${target}\n\nNext:\n  cd ${name}\n  ${next}`);
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
      return devApp({ dir, installation: values.installation, dataDir: values["data-dir"] });
    case "run":
      return runApp({ dir, installation: values.installation, dataDir: values["data-dir"], artifact: values.artifact });
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
