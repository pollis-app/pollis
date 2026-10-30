#!/usr/bin/env node
// Build, validate, diff and (with --apply) publish the minimum-client-version policy (#1611).
//
// The policy is a static file on the CDN, next to releases/latest.json, uploaded to R2 the same
// way: `aws s3 cp` to the `pollis` bucket. This script is its ONLY writer — never hand-edit it in
// R2. Runbook: docs/deployments.md → "Client minimum version". Format and rule:
// pollis-api/src/client_policy.rs, which this script runs to validate (one implementation).
//
//   node scripts/client-policy.mjs --env dev                        # show what is published
//   node scripts/client-policy.mjs --env prod --set macos=1.17.0    # dry run: validate + diff
//   node scripts/client-policy.mjs --env prod --set macos=1.17.0 --apply   # asks, then uploads
//
// Options:
//   --env prod|dev            prod → cdn.pollis.com/client-policy.json, dev → …/dev/client-policy.json
//   --set <platform>=<ver>    set a minimum (repeatable). Platforms: ios, android-play,
//                             android-sideload, macos, windows, linux, cli
//   --unset <platform>        remove a minimum (repeatable)
//   --message <text>          the blocked screen's text instead of the built-in one
//   --clear-message           back to the built-in text
//   --from <file>             start from this file instead of what is published
//   --out <file>              also write the result here
//   --apply                   upload (asks you to type the env name; --yes skips the prompt)
//   --force                   allow a minimum above the latest published release, one that cannot
//                             be checked (unreadable release manifest), or replacing an invalid
//                             published file from empty
//
// Upload credentials (only with --apply): R2_ACCESS_KEY_ID, R2_SECRET_KEY, CLOUDFLARE_ACCOUNT_ID,
// e.g. `doppler run -p pollis -c prd_prod -- node scripts/client-policy.mjs … --apply`.
//
// Test hooks (scripts/test-client-policy.sh): CLIENT_POLICY_CDN_BASE and
// CLIENT_POLICY_RELEASES_BASE may be local directories; CLIENT_POLICY_VALIDATOR names a built
// `client-policy` example binary instead of `cargo run`.

import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createInterface } from "node:readline";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const PLATFORMS = ["ios", "android-play", "android-sideload", "macos", "windows", "linux", "cli"];
const KEYS = { prod: "client-policy.json", dev: "dev/client-policy.json" };
const CDN_BASE = process.env.CLIENT_POLICY_CDN_BASE || "https://cdn.pollis.com";
const RELEASES_BASE = process.env.CLIENT_POLICY_RELEASES_BASE || "https://cdn.pollis.com/releases";
const BUCKET = "pollis";
// The `[workspace.package] version` in Cargo.toml that only CI's bump-version.sh replaces.
const DEV_PLACEHOLDER_VERSION = "1.1.0";
const DESKTOP_AND_CLI = ["macos", "windows", "linux", "cli"];

function die(msg, code = 1) {
  console.error(`client-policy: ${msg}`);
  process.exit(code);
}

function parseArgs(argv) {
  const opts = { env: null, set: [], unset: [], message: undefined, from: null, out: null, apply: false, yes: false, force: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const next = () => {
      if (i + 1 >= argv.length) {
        die(`${a} needs a value`, 2);
      }
      return argv[++i];
    };
    switch (a) {
      case "--env": opts.env = next(); break;
      case "--set": opts.set.push(next()); break;
      case "--unset": opts.unset.push(next()); break;
      case "--message": opts.message = next(); break;
      case "--clear-message": opts.message = null; break;
      case "--from": opts.from = next(); break;
      case "--out": opts.out = next(); break;
      case "--apply": opts.apply = true; break;
      case "--yes": opts.yes = true; break;
      case "--force": opts.force = true; break;
      case "-h": case "--help":
        console.log(readFileSync(fileURLToPath(import.meta.url), "utf8").split("\n").slice(1, 30).map((l) => l.replace(/^\/\/ ?/, "")).join("\n"));
        process.exit(0);
      default: die(`unknown argument ${a} (see --help)`, 2);
    }
  }
  if (!KEYS[opts.env]) {
    die("--env prod|dev is required", 2);
  }
  return opts;
}

// A URL is fetched; anything else is a local path (tests). `null` for a 404 / missing file.
async function load(location) {
  if (!/^https?:\/\//.test(location)) {
    return existsSync(location) ? readFileSync(location, "utf8") : null;
  }
  const sep = location.includes("?") ? "&" : "?";
  const resp = await fetch(`${location}${sep}nocache=${Date.now()}`, { headers: { "cache-control": "no-cache" } });
  if (resp.status === 404 || resp.status === 403) {
    return null;
  }
  if (!resp.ok) {
    die(`GET ${location}: HTTP ${resp.status}`);
  }
  return resp.text();
}

function joinLoc(base, path) {
  return /^https?:\/\//.test(base) ? `${base.replace(/\/$/, "")}/${path}` : join(base, path);
}

// The canonical form clients read: fixed key order, minimums sorted.
function canonical(policy) {
  const minimum = {};
  for (const k of Object.keys(policy.minimum ?? {}).sort()) {
    minimum[k] = policy.minimum[k];
  }
  return JSON.stringify({ v: policy.v ?? 1, minimum, message: policy.message ?? null }, null, 2) + "\n";
}

let workDir = null;
function tmpFile(name, contents) {
  workDir ??= mkdtempSync(join(tmpdir(), "client-policy-"));
  const p = join(workDir, name);
  writeFileSync(p, contents);
  return p;
}

// pollis-api's strict parser and the rule, so the script cannot disagree with clients or the DS.
function validator(args) {
  const bin = process.env.CLIENT_POLICY_VALIDATOR;
  const [cmd, cmdArgs] = bin
    ? [bin, args]
    : ["cargo", ["run", "-q", "-p", "pollis-api", "--example", "client-policy", "--", ...args]];
  return spawnSync(cmd, cmdArgs, { cwd: ROOT, encoding: "utf8" });
}

function validate(json, { fatal = true } = {}) {
  const r = validator(["validate", tmpFile("candidate.json", json)]);
  if (r.status !== 0) {
    const why = (r.stderr || r.error?.message || "validator failed").trim().replace(/^client-policy: /, "");
    if (!fatal) {
      console.log(`  ! the published file is invalid (${why}); clients ignore it. Pass --from to replace it.`);
      return null;
    }
    die(why);
  }
  return JSON.parse(r.stdout);
}

// Would a build at `version` be blocked by `minimum`? Answered by the rule itself.
function below(version, minimum, platform) {
  const probe = tmpFile("probe.json", JSON.stringify({ v: 1, minimum: { [platform]: minimum } }));
  const r = validator(["check", probe, platform, version]);
  if (r.status !== 0) {
    die((r.stderr || "validator failed").trim());
  }
  return r.stdout.trim().startsWith("blocked");
}

// The newest published release per platform, where a manifest says it.
async function latestReleases() {
  const read = async (path) => {
    const raw = await load(joinLoc(RELEASES_BASE, path)).catch(() => null);
    try {
      return raw ? String(JSON.parse(raw).version ?? "").replace(/^v/, "") || null : null;
    } catch {
      return null;
    }
  };
  const desktop = await read("latest.json");
  const cli = await read("cli/latest.json");
  const sideload = await read("android/latest.json");
  // Mobile ships one version to TestFlight/App Store, Play and the APK (mobile/package.json), and
  // only the APK has a manifest, so it bounds all three. Store review lags: confirm by hand.
  return { macos: desktop, windows: desktop, linux: desktop, cli, "android-sideload": sideload, ios: sideload, "android-play": sideload };
}

async function confirm(env) {
  if (!process.stdin.isTTY) {
    die("--apply needs a terminal to confirm, or --yes", 2);
  }
  const rl = createInterface({ input: process.stdin, output: process.stdout });
  const answer = await new Promise((r) => rl.question(`Type "${env}" to publish: `, r));
  rl.close();
  return answer.trim() === env;
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const key = KEYS[opts.env];
  const url = joinLoc(CDN_BASE, key);

  const publishedRaw = await load(url);
  let published = null;
  if (publishedRaw !== null) {
    published = validate(publishedRaw, { fatal: false });
    // Starting from empty would silently drop whatever minimums the broken file meant to hold.
    if (!published && !opts.force) {
      die("the published file is invalid; fix it with --from <file> (or --force to start from empty)");
    }
  }
  console.log(`${opts.env}: ${url}`);
  console.log(published ? "published:" : "published: nothing yet (clients treat that as no minimums)");

  let next;
  if (opts.from) {
    next = validate(readFileSync(opts.from, "utf8"));
  } else {
    next = structuredClone(published ?? { v: 1, minimum: {}, message: null });
  }
  for (const s of opts.set) {
    const m = /^([a-z-]+)=(.+)$/.exec(s);
    if (!m) {
      die(`--set ${s}: expected <platform>=<version>`, 2);
    }
    if (!PLATFORMS.includes(m[1])) {
      die(`--set ${s}: unknown platform (known: ${PLATFORMS.join(", ")})`, 2);
    }
    next.minimum[m[1]] = m[2];
  }
  for (const p of opts.unset) {
    if (!PLATFORMS.includes(p)) {
      die(`--unset ${p}: unknown platform (known: ${PLATFORMS.join(", ")})`, 2);
    }
    delete next.minimum[p];
  }
  if (opts.message !== undefined) {
    next.message = opts.message;
  }
  const nextJson = canonical(validate(canonical(next)));
  next = JSON.parse(nextJson);

  // The diff, per platform.
  const before = published ?? { minimum: {}, message: null };
  const rows = PLATFORMS.map((p) => [p, before.minimum[p] ?? "-", next.minimum[p] ?? "-"]);
  const width = Math.max(...PLATFORMS.map((p) => p.length));
  let changed = false;
  for (const [p, a, b] of rows) {
    const mark = a === b ? " " : "*";
    changed ||= a !== b;
    console.log(`  ${mark} ${p.padEnd(width)}  ${a.padEnd(10)} → ${b}`);
  }
  const msgBefore = before.message ?? null;
  if (msgBefore !== next.message) {
    changed = true;
    console.log(`  * message  ${JSON.stringify(msgBefore)} → ${JSON.stringify(next.message)}`);
  } else {
    console.log(`    message  ${JSON.stringify(next.message)}`);
  }
  if (publishedRaw !== null && (!published || canonical(published) !== publishedRaw)) {
    // A file not in canonical form was hand-edited or written by something else.
    changed = true;
    console.log("  * the published file is not in canonical form; this rewrites it");
  }
  if (opts.out) {
    writeFileSync(opts.out, nextJson);
  }
  if (!changed && publishedRaw !== null) {
    console.log("no change.");
    return;
  }

  // Never set a floor above what users can install: that blocks everyone, newest build included.
  const latest = await latestReleases();
  let refused = false;
  for (const p of PLATFORMS) {
    const min = next.minimum[p];
    if (!min || min === before.minimum[p]) {
      continue;
    }
    if (!latest[p]) {
      console.log(`  ! ${p}: the release manifest for ${p} could not be read, so ${min} cannot be checked`);
      refused = true;
      continue;
    }
    if (below(latest[p], min, p)) {
      console.log(`  ! ${p}: ${min} is above the latest release (${latest[p]}); every ${p} install would be blocked`);
      refused = true;
      continue;
    }
    if (p === "ios" || p === "android-play") {
      console.log(`  ! ${p}: bounded by the sideload APK (${latest[p]}); confirm ${min} is live in the store, not just uploaded`);
    }
    // Unstamped dev desktop/CLI builds report the workspace placeholder; a dev floor above it blocks them all.
    if (opts.env === "dev" && DESKTOP_AND_CLI.includes(p) && below(DEV_PLACEHOLDER_VERSION, min, p)) {
      console.log(`  ! ${p}: dev desktop/CLI builds report the unstamped ${DEV_PLACEHOLDER_VERSION}; ${min} blocks every one of them`);
    }
  }
  if (refused && !opts.force) {
    die("refusing a minimum that cannot be checked or is above the latest release (--force to override)");
  }

  if (!opts.apply) {
    console.log(`dry run: nothing uploaded. Re-run with --apply to publish to ${url}`);
    return;
  }
  if (!opts.yes && !(await confirm(opts.env))) {
    die("not confirmed; nothing uploaded");
  }
  for (const v of ["R2_ACCESS_KEY_ID", "R2_SECRET_KEY", "CLOUDFLARE_ACCOUNT_ID"]) {
    if (!process.env[v]) {
      die(`${v} is not set (run under doppler; see the header)`);
    }
  }
  const file = tmpFile("client-policy.json", nextJson);
  // The release workflow's exact mechanism (desktop-release.yml → "Publish latest.json to R2").
  execFileSync(
    "aws",
    [
      "s3", "cp", file, `s3://${BUCKET}/${key}`,
      "--endpoint-url", `https://${process.env.CLOUDFLARE_ACCOUNT_ID}.r2.cloudflarestorage.com`,
      "--content-type", "application/json",
      "--cache-control", "no-cache",
    ],
    {
      stdio: "inherit",
      env: {
        ...process.env,
        AWS_ACCESS_KEY_ID: process.env.R2_ACCESS_KEY_ID,
        AWS_SECRET_ACCESS_KEY: process.env.R2_SECRET_KEY,
        AWS_DEFAULT_REGION: "auto",
      },
    },
  );
  const live = await load(url);
  if (live !== nextJson) {
    die(`uploaded, but ${url} does not serve it yet — check it by hand`);
  }
  console.log(`published and verified: ${url}`);
  console.log(`the DS mirror (GET /v1/client-policy) follows within its refresh window`);
}

main().catch((e) => die(e?.stack || String(e)));
