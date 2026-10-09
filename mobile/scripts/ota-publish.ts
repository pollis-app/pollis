#!/usr/bin/env node
// Over-the-air JS update pipeline for the mobile app (#1250).
//
//   node scripts/ota-publish.ts <command> [options]          (from mobile/)
//
// Each command is one step of .github/workflows/mobile-ota-release.yml and can
// be run by hand with the same result (runbook: mobile/CLAUDE.md "OTA updates").
//
//   build                 expo export with prod EXPO_PUBLIC values, runtime
//                         versions from the fingerprint, safety checks, unsigned
//                         manifests            → <dir>/plan.json + <dir>/files/
//   prepare-republish     re-serve an earlier group's bytes under new update ids
//                         (rollback to a known-good update)
//   prepare-rollback      rollBackToEmbedded directives (rollback to the store
//                         binary's own bundle)
//   sign                  re-check everything, then sign with OTA_CODE_SIGNING_KEY
//                         (the only step that sees the key)
//   upload                assets, archive, then the live pointers, to R2
//   attest                stage the files + BinaryRecord leaves for the log
//   log-append            merge the leaves into the transparency accumulator
//   verify                re-fetch through the Worker; check signature + hashes
//
// Only node: built-ins at the top level. `build` loads Expo from node_modules
// lazily, so `sign` runs from a bare checkout.

import { spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync, mkdtempSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir, platform as osPlatform, release as osRelease } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  CHANNEL,
  MANIFEST_URL,
  OTA_KEY_PREFIX,
  PLATFORMS,
  UPDATES_BASE_URL,
  assertBundleTargetsProd,
  assertCodeSigningCertificate,
  assertKeyMatchesCertificate,
  assertManifestShape,
  assertNoSecretValues,
  assertOnlyPublicEnvReads,
  assertPublicEnv,
  assetRef,
  attestedFiles,
  base64UrlToHex,
  buildDirective,
  buildManifest,
  mergeRecords,
  parseMultipart,
  partName,
  pointerFor,
  pointerKey,
  releaseTag,
  sha256Base64Url,
  sha256Hex,
  signBody,
  verifyBody,
  type BinaryRecord,
  type Plan,
  type PlanAction,
  type PlanEntry,
  type Platform,
} from "./ota/lib.ts";

const MOBILE = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const REPO = resolve(MOBILE, "..");
const CERT_PATH = join(MOBILE, "store", "ota-code-signing.pem");
const ACCUMULATOR_KEY = "internal/binary-records.json";

// ---------------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------------

function fail(message: string): never {
  console.error(`ota-publish: ${message}`);
  process.exit(1);
}

function parseArgs(argv: string[]): { command: string; opts: Record<string, string> } {
  const [command, ...rest] = argv;
  const opts: Record<string, string> = {};
  for (let i = 0; i < rest.length; i++) {
    const a = rest[i];
    if (!a.startsWith("--")) {
      fail(`unexpected argument ${a}`);
    }
    const eq = a.indexOf("=");
    if (eq > 0) {
      opts[a.slice(2, eq)] = a.slice(eq + 1);
    } else if (i + 1 < rest.length && !rest[i + 1].startsWith("--")) {
      opts[a.slice(2)] = rest[++i];
    } else {
      opts[a.slice(2)] = "true";
    }
  }
  return { command: command ?? "help", opts };
}

function need(opts: Record<string, string>, name: string): string {
  const v = opts[name];
  if (v === undefined || v === "") {
    fail(`--${name} is required`);
  }
  return v;
}

function run(cmd: string, args: string[], options: { cwd?: string; env?: NodeJS.ProcessEnv; capture?: boolean } = {}): string {
  const r = spawnSync(cmd, args, {
    cwd: options.cwd ?? MOBILE,
    env: options.env ?? process.env,
    encoding: "utf8",
    stdio: options.capture === false ? "inherit" : ["ignore", "pipe", "pipe"],
    maxBuffer: 256 * 1024 * 1024,
  });
  if (r.status !== 0) {
    const err = typeof r.stderr === "string" ? r.stderr.trim() : "";
    throw new Error(`${cmd} ${args.join(" ")} exited ${r.status}${err ? `: ${err}` : ""}`);
  }
  return typeof r.stdout === "string" ? r.stdout : "";
}

function readPlan(dir: string): Plan {
  return JSON.parse(readFileSync(join(dir, "plan.json"), "utf8")) as Plan;
}

function writePlan(dir: string, plan: Plan): void {
  writeFileSync(join(dir, "plan.json"), JSON.stringify(plan, null, 2) + "\n");
}

function fileBytes(dir: string, sha256: string): Buffer {
  const p = join(dir, "files", sha256);
  if (!existsSync(p)) {
    throw new Error(`missing file ${sha256}`);
  }
  const bytes = readFileSync(p);
  if (sha256Hex(bytes) !== sha256) {
    throw new Error(`file ${sha256} does not hash to its name`);
  }
  return bytes;
}

function readCertificate(): string {
  if (!existsSync(CERT_PATH)) {
    fail(
      `${relative(REPO, CERT_PATH)} is missing — the owner generates it with ` +
        "mobile/scripts/generate-ota-signing-key.sh and commits it; no OTA can be built or signed without it",
    );
  }
  const pem = readFileSync(CERT_PATH, "utf8");
  assertCodeSigningCertificate(pem);
  return pem;
}

function gitHead(): { commit: string; epoch: number; dirty: boolean } {
  const commit = run("git", ["rev-parse", "HEAD"], { cwd: REPO }).trim();
  const epoch = Number(run("git", ["log", "-1", "--format=%ct"], { cwd: REPO }).trim());
  const dirty = run("git", ["status", "--porcelain"], { cwd: REPO }).trim() !== "";
  return { commit, epoch, dirty };
}

function toolchain(): Plan["toolchain"] {
  const rt = readFileSync(join(REPO, "rust-toolchain.toml"), "utf8");
  const rustc = /channel\s*=\s*"([^"]+)"/.exec(rt)?.[1] ?? fail("rust-toolchain.toml has no channel");
  const pm = (JSON.parse(readFileSync(join(REPO, "package.json"), "utf8")) as { packageManager?: string }).packageManager;
  const pnpm = /^pnpm@(.+)$/.exec(pm ?? "")?.[1] ?? fail("root package.json has no pnpm packageManager");
  // GitHub-hosted runners export ImageOS + ImageVersion (the dated image), the
  // same identity desktop leaves record; a workstation records itself as such.
  const image = process.env.ImageOS && process.env.ImageVersion
    ? `${process.env.ImageOS}@${process.env.ImageVersion}`
    : `local-${osPlatform()}-${osRelease()}`;
  return { rustc, node: process.versions.node, pnpm, runnerImage: image };
}

function sourceFiles(): { path: string; text: string }[] {
  const roots = ["app", "components", "hooks", "lib", "i18n", "stores", "theme", "types", "modules/pollis-native/src"];
  const out: { path: string; text: string }[] = [];
  const walk = (dir: string): void => {
    if (!existsSync(dir)) {
      return;
    }
    for (const name of readdirSync(dir)) {
      const p = join(dir, name);
      if (statSync(p).isDirectory()) {
        walk(p);
      } else if (/\.(tsx?|jsx?|mjs|cjs)$/.test(name)) {
        out.push({ path: relative(MOBILE, p), text: readFileSync(p, "utf8") });
      }
    }
  };
  for (const r of roots) {
    walk(join(MOBILE, r));
  }
  return out;
}

function summary(plan: Plan): string {
  const lines = [
    `### OTA ${plan.action} — group \`${plan.groupId}\``,
    "",
    `- release tag (transparency log): \`${releaseTag(plan)}\``,
    `- source commit: \`${plan.commit}\``,
    `- channel: \`${plan.channel}\`, base URL: ${plan.baseUrl}`,
    "",
    "| platform | runtime version | kind | update id | launch bundle sha256 | assets |",
    "|---|---|---|---|---|---|",
  ];
  for (const e of plan.entries) {
    lines.push(
      `| ${e.platform} | \`${e.runtimeVersion}\` | ${e.kind} | ${e.updateId ?? "-"} | ${e.launchSha256 ? `\`${e.launchSha256}\`` : "-"} | ${e.assets?.length ?? 0} |`,
    );
  }
  return lines.join("\n") + "\n";
}

function emitSummary(plan: Plan, dir: string): void {
  const text = summary(plan);
  writeFileSync(join(dir, "summary.md"), text);
  if (process.env.GITHUB_STEP_SUMMARY) {
    writeFileSync(process.env.GITHUB_STEP_SUMMARY, text, { flag: "a" });
  }
  console.log(text);
}

// ---------------------------------------------------------------------------
// build
// ---------------------------------------------------------------------------

interface ExportMetadata {
  fileMetadata: Record<string, { bundle: string; assets: { path: string; ext: string }[] }>;
}

async function cmdBuild(opts: Record<string, string>): Promise<void> {
  const out = resolve(need(opts, "out"));
  const platforms = (opts.platforms ?? PLATFORMS.join(",")).split(",") as Platform[];
  for (const p of platforms) {
    if (!(PLATFORMS as readonly string[]).includes(p)) {
      fail(`unknown platform ${p}`);
    }
  }
  if (existsSync(join(out, "plan.json"))) {
    fail(`${out} already holds a plan; use a fresh directory`);
  }

  // Same checks as a store build, before spending a minute on Metro.
  assertPublicEnv(process.env);
  assertOnlyPublicEnvReads(sourceFiles());
  readCertificate();
  const head = gitHead();
  if (head.dirty && opts["allow-dirty"] !== "true") {
    fail("the working tree has uncommitted changes; the transparency log records a commit, so publish from a clean tree");
  }

  // The OTA build is a prod build: app.config.js must evaluate exactly as it
  // does for a store build (updates on, code-signed). EXPO_NO_DOTENV keeps a
  // workstation's mobile/.env (often api-dev) out of the bundle — the values
  // come from this process's environment only.
  process.env.POLLIS_OTA = "production";
  process.env.EXPO_NO_DOTENV = "1";
  process.env.NODE_ENV = "production";

  const req = createRequire(join(MOBILE, "package.json"));
  const { resolveRuntimeVersionAsync } = req("expo-updates/utils/build/resolveRuntimeVersionAsync") as {
    resolveRuntimeVersionAsync: (root: string, p: Platform, f: object, o: object) => Promise<{ runtimeVersion: string | null }>;
  };
  const { getConfig } = req("expo/config") as {
    getConfig: (root: string, o: object) => { exp: Record<string, unknown> };
  };
  const runtimeVersions = async (): Promise<Record<string, string>> => {
    const rv: Record<string, string> = {};
    for (const p of platforms) {
      const r = await resolveRuntimeVersionAsync(MOBILE, p, { silent: true }, {});
      if (!r.runtimeVersion) {
        fail(`no runtime version for ${p}`);
      }
      rv[p] = r.runtimeVersion;
    }
    return rv;
  };

  const before = await runtimeVersions();
  const expoClient = getConfig(MOBILE, { isPublicConfig: true, skipSDKVersionRequirement: true }).exp;

  const exportDir = join(out, "export");
  mkdirSync(join(out, "files"), { recursive: true });
  const args = ["export", "--output-dir", exportDir, "--clear"];
  for (const p of platforms) {
    args.push("--platform", p);
  }
  run(join(MOBILE, "node_modules", ".bin", "expo"), args, { capture: false });

  // Nothing the export did may move the fingerprint: if it did, the bundle was
  // built from a tree whose native layer is not the one the runtime version names.
  const after = await runtimeVersions();
  for (const p of platforms) {
    if (before[p] !== after[p]) {
      fail(`${p} runtime version moved during the export (${before[p]} -> ${after[p]})`);
    }
  }

  const metadata = JSON.parse(readFileSync(join(exportDir, "metadata.json"), "utf8")) as ExportMetadata;
  const plan: Plan = {
    version: 1,
    action: "publish",
    groupId: randomUUID(),
    createdAt: new Date().toISOString(),
    channel: CHANNEL,
    baseUrl: UPDATES_BASE_URL,
    commit: head.commit,
    sourceDateEpoch: head.epoch,
    toolchain: toolchain(),
    entries: [],
    files: {},
  };

  const store = (bytes: Buffer, contentType: string): string => {
    const sha = sha256Hex(bytes);
    writeFileSync(join(out, "files", sha), bytes);
    plan.files[sha] = { contentType, size: bytes.length };
    return sha;
  };

  for (const p of platforms) {
    const meta = metadata.fileMetadata[p];
    if (!meta) {
      fail(`expo export produced no ${p} bundle`);
    }
    const bundle = readFileSync(join(exportDir, meta.bundle));
    assertBundleTargetsProd(bundle, p);
    assertNoSecretValues(bundle, process.env, p);
    const launch = assetRef(bundle, "hbc", plan.baseUrl, true);
    store(bundle, launch.contentType);

    const seen = new Set<string>();
    const assets = [];
    for (const a of meta.assets) {
      const bytes = readFileSync(join(exportDir, a.path));
      const ref = assetRef(bytes, a.ext, plan.baseUrl);
      if (seen.has(ref.key)) {
        continue;
      }
      seen.add(ref.key);
      assertNoSecretValues(bytes, process.env, `${p} ${a.path}`);
      store(bytes, ref.contentType);
      assets.push(ref);
    }

    const updateId = randomUUID();
    const body = buildManifest({
      id: updateId,
      createdAt: plan.createdAt,
      runtimeVersion: after[p],
      launch,
      assets,
      expoClient,
    });
    assertManifestShape(body, { runtimeVersion: after[p], baseUrl: plan.baseUrl });
    plan.entries.push({
      platform: p,
      runtimeVersion: after[p],
      kind: "manifest",
      body,
      updateId,
      launchSha256: launch.sha256,
      assets: assets.map((a) => ({ sha256: a.sha256, key: a.key, fileExtension: a.fileExtension, contentType: a.contentType })),
    });
  }

  writePlan(out, plan);
  emitSummary(plan, out);
}

// ---------------------------------------------------------------------------
// R2 (S3 API through the aws CLI, exactly as the release workflows do it)
// ---------------------------------------------------------------------------

interface R2 {
  bucket: string;
  endpoint: string;
}

function r2From(opts: Record<string, string>): R2 {
  return { bucket: need(opts, "bucket"), endpoint: need(opts, "endpoint") };
}

const AWS_ENV = {
  ...process.env,
  AWS_DEFAULT_REGION: process.env.AWS_DEFAULT_REGION ?? "auto",
  // R2 rejects the integrity checksums newer aws-cli v2 adds by default.
  AWS_REQUEST_CHECKSUM_CALCULATION: "when_required",
  AWS_RESPONSE_CHECKSUM_VALIDATION: "when_required",
};

function aws(args: string[]): string {
  return run("aws", args, { env: AWS_ENV });
}

// The update store is the `ota/` prefix of the release bucket. These three
// helpers (and the live-pointer listing in prepare-rollback) are the only
// update-store I/O, and they take keys relative to that prefix — the same keys
// the Worker reads (pollis-delivery/worker/updates.ts). The transparency
// accumulator (log-append) lives in its own bucket and is not prefixed.
function storeKey(key: string): string {
  return `${OTA_KEY_PREFIX}${key}`;
}

function r2Exists(r2: R2, key: string): boolean {
  const res = spawnSync("aws", ["s3api", "head-object", "--bucket", r2.bucket, "--key", storeKey(key), "--endpoint-url", r2.endpoint], {
    env: AWS_ENV,
    encoding: "utf8",
  });
  return res.status === 0;
}

function r2Put(r2: R2, key: string, file: string, contentType: string, cacheControl: string): void {
  aws(["s3", "cp", file, `s3://${r2.bucket}/${storeKey(key)}`, "--endpoint-url", r2.endpoint, "--content-type", contentType, "--cache-control", cacheControl, "--only-show-errors"]);
}

function r2Get(r2: R2, key: string): Buffer {
  const tmp = join(mkdtempSync(join(tmpdir(), "ota-")), "obj");
  aws(["s3", "cp", `s3://${r2.bucket}/${storeKey(key)}`, tmp, "--endpoint-url", r2.endpoint, "--only-show-errors"]);
  return readFileSync(tmp);
}

function writeTemp(contents: string | Buffer): string {
  const p = join(mkdtempSync(join(tmpdir(), "ota-")), "obj");
  writeFileSync(p, contents);
  return p;
}

// ---------------------------------------------------------------------------
// republish / rollback
// ---------------------------------------------------------------------------

interface UpdateArchive {
  platform: Platform;
  runtimeVersion: string;
  groupId: string;
  commit: string;
  sourceDateEpoch: number;
  toolchain: Plan["toolchain"];
  createdAt: string;
  body: string;
  signature: string;
  launchSha256: string;
  assets: NonNullable<PlanEntry["assets"]>;
}

interface GroupArchive {
  action: PlanAction;
  groupId: string;
  createdAt: string;
  commit: string;
  entries: { platform: Platform; runtimeVersion: string; kind: string; updateId?: string }[];
}

function cmdPrepareRepublish(opts: Record<string, string>): void {
  const r2 = r2From(opts);
  const from = need(opts, "from-group");
  const out = resolve(need(opts, "out"));
  mkdirSync(join(out, "files"), { recursive: true });
  const group = JSON.parse(r2Get(r2, `groups/${from}.json`).toString("utf8")) as GroupArchive;
  const manifests = group.entries.filter((e) => e.kind === "manifest" && e.updateId);
  if (manifests.length === 0) {
    fail(`group ${from} has no updates to republish`);
  }
  const createdAt = new Date().toISOString();
  let first: UpdateArchive | undefined;
  const plan: Plan = {
    version: 1,
    action: "republish",
    groupId: randomUUID(),
    createdAt,
    channel: CHANNEL,
    baseUrl: UPDATES_BASE_URL,
    commit: "",
    sourceDateEpoch: 0,
    toolchain: { rustc: "", node: "", pnpm: "", runnerImage: "" },
    fromGroupId: from,
    entries: [],
    files: {},
  };
  for (const e of manifests) {
    const archive = JSON.parse(r2Get(r2, `updates/${e.updateId}.json`).toString("utf8")) as UpdateArchive;
    first ??= archive;
    // A republish is a NEW update (new id, new createdAt) carrying the old
    // bytes: the client only moves to an update newer than the one it runs,
    // so re-serving the old manifest unchanged would roll nobody back.
    const manifest = JSON.parse(archive.body) as Record<string, unknown>;
    manifest.id = randomUUID();
    manifest.createdAt = createdAt;
    const body = JSON.stringify(manifest);
    for (const sha of [archive.launchSha256, ...archive.assets.map((a) => a.sha256)]) {
      const bytes = r2Get(r2, `assets/${sha}`);
      if (sha256Hex(bytes) !== sha) {
        fail(`R2 object assets/${sha} does not hash to its name`);
      }
      writeFileSync(join(out, "files", sha), bytes);
      const ct = sha === archive.launchSha256 ? "application/javascript" : (archive.assets.find((a) => a.sha256 === sha)?.contentType ?? "application/octet-stream");
      plan.files[sha] = { contentType: ct, size: bytes.length };
    }
    plan.entries.push({
      platform: archive.platform,
      runtimeVersion: archive.runtimeVersion,
      kind: "manifest",
      body,
      updateId: manifest.id as string,
      launchSha256: archive.launchSha256,
      assets: archive.assets,
    });
  }
  // The leaves describe bytes built from the original commit, so they record it.
  plan.commit = first!.commit;
  plan.sourceDateEpoch = first!.sourceDateEpoch;
  plan.toolchain = first!.toolchain;
  writePlan(out, plan);
  emitSummary(plan, out);
}

function cmdPrepareRollback(opts: Record<string, string>): void {
  const r2 = r2From(opts);
  const out = resolve(need(opts, "out"));
  mkdirSync(out, { recursive: true });
  const which = need(opts, "platform");
  const platforms = (which === "all" ? [...PLATFORMS] : [which]) as Platform[];
  const rvOpt = need(opts, "runtime-version");
  const head = gitHead();
  const createdAt = new Date().toISOString();
  const plan: Plan = {
    version: 1,
    action: "rollback",
    groupId: randomUUID(),
    createdAt,
    channel: CHANNEL,
    baseUrl: UPDATES_BASE_URL,
    commit: head.commit,
    sourceDateEpoch: head.epoch,
    toolchain: toolchain(),
    entries: [],
    files: {},
  };
  for (const p of platforms) {
    if (!(PLATFORMS as readonly string[]).includes(p)) {
      fail(`unknown platform ${p}`);
    }
    let rvs: string[];
    if (rvOpt === "all") {
      // Every runtime version that currently has a live pointer: the panic button.
      const listing = aws(["s3", "ls", `s3://${r2.bucket}/${storeKey(`current/${CHANNEL}/${p}/`)}`, "--endpoint-url", r2.endpoint]);
      rvs = [...listing.matchAll(/\s(\S+)\.json\s*$/gm)].map((m) => m[1]);
    } else {
      rvs = [rvOpt];
    }
    for (const rv of rvs) {
      plan.entries.push({ platform: p, runtimeVersion: rv, kind: "directive", body: buildDirective("rollBackToEmbedded", createdAt) });
    }
  }
  if (plan.entries.length === 0) {
    fail("nothing to roll back: no live pointer matches");
  }
  writePlan(out, plan);
  emitSummary(plan, out);
}

// ---------------------------------------------------------------------------
// sign — the approval-gated step
// ---------------------------------------------------------------------------

function cmdSign(opts: Record<string, string>): void {
  const dir = resolve(need(opts, "in"));
  const key = process.env.OTA_CODE_SIGNING_KEY;
  if (!key) {
    fail("OTA_CODE_SIGNING_KEY is not set (it exists only in the protected ota-signing environment)");
  }
  const cert = readCertificate();
  assertKeyMatchesCertificate(key, cert);
  const plan = readPlan(dir);
  if (plan.version !== 1 || plan.channel !== CHANNEL || plan.baseUrl !== UPDATES_BASE_URL) {
    fail("plan does not target the production channel at the production update URL");
  }
  const slots = new Set<string>();
  for (const e of plan.entries) {
    const slot = `${e.platform}/${e.runtimeVersion}`;
    if (slots.has(slot)) {
      fail(`two entries for ${slot}`);
    }
    slots.add(slot);
    if (e.kind === "manifest") {
      // Re-derive every claim from the bytes, independently of the build job:
      // that job ran Metro and the whole npm tree, this one ran neither.
      const m = assertManifestShape(e.body, { runtimeVersion: e.runtimeVersion, baseUrl: plan.baseUrl });
      if (m.id !== e.updateId) {
        fail(`${slot}: manifest id does not match the plan`);
      }
      const launchHex = base64UrlToHex(m.launchAsset.hash);
      if (launchHex !== e.launchSha256) {
        fail(`${slot}: launch asset hash does not match the plan`);
      }
      assertBundleTargetsProd(fileBytes(dir, launchHex), slot);
      const planned = new Set((e.assets ?? []).map((a) => a.sha256));
      for (const a of m.assets) {
        const hex = base64UrlToHex(a.hash);
        if (!planned.has(hex)) {
          fail(`${slot}: manifest names an asset the plan does not`);
        }
        fileBytes(dir, hex);
      }
      if (planned.size !== m.assets.length) {
        fail(`${slot}: plan and manifest disagree on the asset set`);
      }
    } else {
      const d = JSON.parse(e.body) as { type?: string };
      if (d.type !== "rollBackToEmbedded" && d.type !== "noUpdateAvailable") {
        fail(`${slot}: unknown directive ${d.type}`);
      }
    }
    e.signature = signBody(e.body, key);
    verifyBody(e.body, e.signature, cert);
  }
  writePlan(dir, plan);
  console.log(`signed ${plan.entries.length} entr${plan.entries.length === 1 ? "y" : "ies"} of group ${plan.groupId}`);
}

// ---------------------------------------------------------------------------
// upload
// ---------------------------------------------------------------------------

// `--phase content` uploads the bytes and the archive (nothing a phone sees);
// `--phase live` flips the pointers. The workflow appends to the transparency
// log BETWEEN the two, so no update is ever live that is not in the log.
function cmdUpload(opts: Record<string, string>): void {
  const r2 = r2From(opts);
  const dir = resolve(need(opts, "in"));
  const phase = opts.phase ?? "all";
  if (phase !== "content" && phase !== "live" && phase !== "all") {
    fail("--phase must be content, live or all");
  }
  const plan = readPlan(dir);
  const cert = readCertificate();
  for (const e of plan.entries) {
    // Never put a pointer up that a phone would refuse.
    verifyBody(e.body, pointerFor(e).signature, cert);
  }
  if (phase !== "live") {
    uploadContent(r2, dir, plan);
  }
  if (phase !== "content") {
    uploadPointers(r2, plan);
  }
}

function uploadContent(r2: R2, dir: string, plan: Plan): void {
  // 1. Content-addressed bytes. Immutable, so an existing object is skipped.
  for (const [sha, meta] of Object.entries(plan.files)) {
    const key = `assets/${sha}`;
    if (r2Exists(r2, key)) {
      continue;
    }
    fileBytes(dir, sha);
    r2Put(r2, key, join(dir, "files", sha), meta.contentType, "public, max-age=31536000, immutable");
  }

  // 2. The archive (republish source + audit trail). Never served.
  for (const e of plan.entries) {
    if (e.kind !== "manifest") {
      continue;
    }
    const archive: UpdateArchive = {
      platform: e.platform,
      runtimeVersion: e.runtimeVersion,
      groupId: plan.groupId,
      commit: plan.commit,
      sourceDateEpoch: plan.sourceDateEpoch,
      toolchain: plan.toolchain,
      createdAt: plan.createdAt,
      body: e.body,
      signature: e.signature as string,
      launchSha256: e.launchSha256 as string,
      assets: e.assets ?? [],
    };
    r2Put(r2, `updates/${e.updateId}.json`, writeTemp(JSON.stringify(archive)), "application/json", "no-cache");
  }
  const group: GroupArchive = {
    action: plan.action,
    groupId: plan.groupId,
    createdAt: plan.createdAt,
    commit: plan.commit,
    entries: plan.entries.map((e) => ({ platform: e.platform, runtimeVersion: e.runtimeVersion, kind: e.kind, updateId: e.updateId })),
  };
  r2Put(r2, `groups/${plan.groupId}.json`, writeTemp(JSON.stringify(group)), "application/json", "no-cache");
}

function uploadPointers(r2: R2, plan: Plan): void {
  // 3. Live pointers LAST, so no phone is ever told about bytes not yet there.
  for (const e of plan.entries) {
    if (e.kind === "manifest") {
      for (const sha of [e.launchSha256 as string, ...(e.assets ?? []).map((a) => a.sha256)]) {
        if (!r2Exists(r2, `assets/${sha}`)) {
          fail(`assets/${sha} is not in the bucket; run --phase content first`);
        }
      }
    }
    r2Put(r2, pointerKey(plan.channel, e.platform, e.runtimeVersion), writeTemp(JSON.stringify(pointerFor(e))), "application/json", "no-cache");
    console.log(`live: ${e.platform} ${e.runtimeVersion} -> ${e.kind}${e.updateId ? ` ${e.updateId}` : ""}`);
  }
}

// ---------------------------------------------------------------------------
// transparency log
// ---------------------------------------------------------------------------

function cmdAttest(opts: Record<string, string>): void {
  const dir = resolve(need(opts, "in"));
  const out = resolve(need(opts, "out"));
  const plan = readPlan(dir);
  mkdirSync(out, { recursive: true });
  const files = attestedFiles(plan, (e) => fileBytes(dir, e.launchSha256 as string));
  for (const f of files) {
    writeFileSync(join(out, f.artifactName), f.bytes);
  }
  writeFileSync(join(dir, "records.json"), JSON.stringify(files.map((f) => f.record), null, 2) + "\n");
  console.log(`staged ${files.length} artifacts for ${releaseTag(plan)} in ${out}`);
}

function cmdLogAppend(opts: Record<string, string>): void {
  const r2 = r2From(opts);
  const records = JSON.parse(readFileSync(resolve(need(opts, "records")), "utf8")) as BinaryRecord[];
  // Optimistic concurrency on the shared accumulator: desktop releases append
  // to the same object. Read with its ETag, write only if it is unchanged.
  for (let attempt = 1; attempt <= 5; attempt++) {
    const tmp = join(mkdtempSync(join(tmpdir(), "ota-")), "acc.json");
    const got = spawnSync(
      "aws",
      ["s3api", "get-object", "--bucket", r2.bucket, "--key", ACCUMULATOR_KEY, "--endpoint-url", r2.endpoint, tmp],
      { env: AWS_ENV, encoding: "utf8" },
    );
    let accumulator: BinaryRecord[] = [];
    let condition: string[];
    if (got.status === 0) {
      const etag = (JSON.parse(got.stdout) as { ETag: string }).ETag;
      const raw = readFileSync(tmp, "utf8").trim();
      accumulator = raw === "" ? [] : (JSON.parse(raw) as BinaryRecord[]);
      condition = ["--if-match", etag];
    } else if (/NoSuchKey|Not Found|404/.test(got.stderr)) {
      condition = ["--if-none-match", "*"];
    } else {
      fail(`reading the accumulator failed: ${got.stderr.trim()}`);
    }
    const merged = mergeRecords(accumulator, records);
    if (merged === null) {
      console.log(`${records[0].release_tag} already in the accumulator — nothing to append`);
      return;
    }
    const body = writeTemp(JSON.stringify(merged));
    const put = spawnSync(
      "aws",
      [
        "s3api", "put-object", "--bucket", r2.bucket, "--key", ACCUMULATOR_KEY, "--endpoint-url", r2.endpoint,
        "--body", body, "--content-type", "application/json", "--cache-control", "no-cache", ...condition,
      ],
      { env: AWS_ENV, encoding: "utf8" },
    );
    if (put.status === 0) {
      console.log(`accumulator: ${accumulator.length} + ${records.length} = ${merged.length} records`);
      return;
    }
    if (!/PreconditionFailed|412/.test(put.stderr)) {
      fail(`writing the accumulator failed: ${put.stderr.trim()}`);
    }
    console.log(`accumulator changed underneath us (attempt ${attempt}); retrying`);
  }
  fail("could not append to the accumulator after 5 attempts");
}

// ---------------------------------------------------------------------------
// verify — through the Worker, as a phone would
// ---------------------------------------------------------------------------

async function fetchUpdate(baseUrl: string, platform: Platform, runtimeVersion: string): Promise<{ status: number; headers: Headers; body: Buffer }> {
  const res = await fetch(`${baseUrl}/api/manifest`, {
    headers: {
      "expo-protocol-version": "1",
      "expo-platform": platform,
      "expo-runtime-version": runtimeVersion,
      "expo-channel-name": CHANNEL,
      "expo-expect-signature": 'sig, keyid="main", alg="rsa-v1_5-sha256"',
      accept: "multipart/mixed,application/expo+json,application/json",
    },
  });
  return { status: res.status, headers: res.headers, body: Buffer.from(await res.arrayBuffer()) };
}

async function verifyOne(baseUrl: string, cert: string, platform: Platform, runtimeVersion: string, expect?: PlanEntry): Promise<void> {
  const label = `${platform}/${runtimeVersion}`;
  const res = await fetchUpdate(baseUrl, platform, runtimeVersion);
  if (res.status !== 200) {
    throw new Error(`${label}: HTTP ${res.status}`);
  }
  if (res.headers.get("expo-protocol-version") !== "1" || res.headers.get("expo-sfv-version") !== "0") {
    throw new Error(`${label}: missing protocol headers`);
  }
  const parts = parseMultipart(res.headers.get("content-type") ?? "", res.body);
  const signed = parts.find((p) => partName(p) === "manifest" || partName(p) === "directive");
  if (!signed) {
    throw new Error(`${label}: no manifest or directive part`);
  }
  const kind = partName(signed);
  verifyBody(signed.body, signed.headers["expo-signature"] ?? "", cert);
  if (expect) {
    if (kind !== expect.kind || signed.body.toString("utf8") !== expect.body) {
      throw new Error(`${label}: the Worker serves different bytes than were signed`);
    }
  }
  if (kind === "manifest") {
    const m = JSON.parse(signed.body.toString("utf8")) as { id: string; launchAsset: { url: string; hash: string }; assets: { url: string; hash: string }[] };
    for (const a of [m.launchAsset, ...m.assets]) {
      const got = await fetch(a.url);
      if (got.status !== 200) {
        throw new Error(`${label}: ${a.url} -> HTTP ${got.status}`);
      }
      if (sha256Base64Url(Buffer.from(await got.arrayBuffer())) !== a.hash) {
        throw new Error(`${label}: ${a.url} does not match its manifest hash`);
      }
    }
    console.log(`ok ${label}: update ${m.id}, manifest sha256 ${sha256Hex(signed.body)}, ${m.assets.length + 1} files verified`);
  } else {
    console.log(`ok ${label}: ${signed.body.toString("utf8")} (sha256 ${sha256Hex(signed.body)})`);
  }
}

async function cmdVerify(opts: Record<string, string>): Promise<void> {
  const cert = readCertificate();
  const baseUrl = opts["base-url"] ?? UPDATES_BASE_URL;
  if (opts.in) {
    const plan = readPlan(resolve(opts.in));
    for (const e of plan.entries) {
      await verifyOne(baseUrl, cert, e.platform, e.runtimeVersion, e);
    }
    return;
  }
  await verifyOne(baseUrl, cert, need(opts, "platform") as Platform, need(opts, "runtime-version"));
}

// ---------------------------------------------------------------------------

const HELP = `usage: node scripts/ota-publish.ts <command> [options]

  build              --out <dir> [--platforms ios,android] [--allow-dirty]
  prepare-republish  --from-group <groupId> --out <dir> --bucket <b> --endpoint <url>
  prepare-rollback   --platform ios|android|all --runtime-version <rv>|all --out <dir> --bucket <b> --endpoint <url>
  sign               --in <dir>                     (needs OTA_CODE_SIGNING_KEY)
  upload             --in <dir> --bucket <b> --endpoint <url> [--phase content|live|all]
  attest             --in <dir> --out <dir>
  log-append         --records <file> --bucket <b> --endpoint <url>
  verify             --in <dir> | --platform <p> --runtime-version <rv>  [--base-url <url>]

See mobile/CLAUDE.md "OTA updates" for the runbook. Manifest URL: ${MANIFEST_URL}
`;

async function main(): Promise<void> {
  const { command, opts } = parseArgs(process.argv.slice(2));
  switch (command) {
    case "build":
      return cmdBuild(opts);
    case "prepare-republish":
      return cmdPrepareRepublish(opts);
    case "prepare-rollback":
      return cmdPrepareRollback(opts);
    case "sign":
      return cmdSign(opts);
    case "upload":
      return cmdUpload(opts);
    case "attest":
      return cmdAttest(opts);
    case "log-append":
      return cmdLogAppend(opts);
    case "verify":
      return cmdVerify(opts);
    default:
      process.stdout.write(HELP);
      if (command !== "help") {
        process.exit(1);
      }
  }
}

main().catch((e: unknown) => {
  fail(e instanceof Error ? e.message : String(e));
});
