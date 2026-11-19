/**
 * `pollis app new <name> --lang ts|rust`: a new App from the template: one
 * handler, a test and a README.
 */

import { cpSync, existsSync, readFileSync, readdirSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export class NewError extends Error {}

const NAME = /^[a-z0-9][a-z0-9-]{0,63}$/;

export interface NewOptions {
  name: string;
  lang: string;
  /** Parent directory. Default: the current directory. */
  parent?: string;
  /**
   * Use local copies of the SDK packages (a path to the Pollis repo's
   * `sdk/ts`), for working before or beside the npm and crates.io releases.
   */
  sdk?: string;
}

function templates(): string {
  return join(dirname(dirname(fileURLToPath(import.meta.url))), "templates");
}

/** The SDK packages are released together, at this CLI's version. */
function sdkVersion(): string {
  const path = join(dirname(dirname(fileURLToPath(import.meta.url))), "package.json");
  return `^${(JSON.parse(readFileSync(path, "utf8")) as { version: string }).version}`;
}

/**
 * The `pollis-app` dependency line of a new Rust App. With `--sdk` the crate is
 * copied into the App: Cargo hashes the absolute path of a dependency outside
 * the project into the build, so a link would change the App hash with the
 * location of the checkout.
 */
function rustDependency(target: string, sdk: string | undefined): string {
  if (!sdk) {
    return JSON.stringify(sdkVersion());
  }
  const crate = join(resolve(sdk), "..", "rust", "pollis-app");
  if (!existsSync(join(crate, "Cargo.toml"))) {
    throw new NewError(`--sdk ${sdk}: expected the Pollis repo's sdk/ts directory (with ../rust/pollis-app beside it)`);
  }
  cpSync(crate, join(target, "vendor", "pollis-app"), {
    recursive: true,
    filter: (source) => !["target", "Cargo.lock"].includes(relative(crate, source)),
  });
  return '{ path = "vendor/pollis-app" }';
}

function newRustApp(target: string, options: NewOptions): void {
  cpSync(join(templates(), "rust"), target, { recursive: true });
  const dependency = rustDependency(target, options.sdk);
  renameSync(join(target, "gitignore"), join(target, ".gitignore"));
  const fill = (text: string) => text.replaceAll("{{name}}", options.name).replaceAll("{{pollis_app}}", dependency);
  const tmpl = join(target, "Cargo.toml.tmpl");
  writeFileSync(join(target, "Cargo.toml"), fill(readFileSync(tmpl, "utf8")));
  unlinkSync(tmpl);
  writeFileSync(join(target, "README.md"), fill(readFileSync(join(target, "README.md"), "utf8")));
}

export function newApp(options: NewOptions): string {
  if (options.lang !== "ts" && options.lang !== "rust") {
    throw new NewError(`--lang ${options.lang} is not available; use TypeScript (--lang ts) or Rust (--lang rust).`);
  }
  if (!NAME.test(options.name)) {
    throw new NewError(`"${options.name}" is not a valid App name: lowercase letters, digits and '-', starting with a letter or digit`);
  }
  const target = resolve(options.parent ?? ".", options.name);
  if (existsSync(target) && readdirSync(target).length > 0) {
    throw new NewError(`${target} already exists and is not empty`);
  }
  if (options.lang === "rust") {
    newRustApp(target, options);
    return target;
  }
  cpSync(join(templates(), "ts"), target, { recursive: true });
  renameSync(join(target, "gitignore"), join(target, ".gitignore"));

  let app = sdkVersion();
  let cli = sdkVersion();
  if (options.sdk) {
    const sdk = resolve(options.sdk);
    if (!existsSync(join(sdk, "app", "package.json")) || !existsSync(join(sdk, "cli", "package.json"))) {
      throw new NewError(`--sdk ${options.sdk}: expected the Pollis repo's sdk/ts directory (with app/ and cli/)`);
    }
    app = `link:${relative(target, join(sdk, "app"))}`;
    cli = `link:${relative(target, join(sdk, "cli"))}`;
  }
  const fill = (text: string) => text.replaceAll("{{name}}", options.name).replaceAll("{{app}}", app).replaceAll("{{cli}}", cli);
  const tmpl = join(target, "package.json.tmpl");
  writeFileSync(join(target, "package.json"), fill(readFileSync(tmpl, "utf8")));
  unlinkSync(tmpl);
  writeFileSync(join(target, "README.md"), fill(readFileSync(join(target, "README.md"), "utf8")));
  return target;
}
