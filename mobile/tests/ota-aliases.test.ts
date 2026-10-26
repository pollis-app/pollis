/*
 * Runtime-version aliases (#1446). The sideload APKs 1.0.14–1.0.16 baked a
 * runtime version polluted by build-time state (#1429) while their native code
 * is the Play build's of the same commit. The ledger records that with
 * `nativeIdenticalTo`, and every update built for the Play runtime version is
 * also issued, re-signed, under the sideload one. Pinned:
 *
 *   - the shipped ledger validates and records exactly the three aliases;
 *   - an alias must name a recorded runtime version of the same platform and
 *     commit, never itself, never another alias; a runtime version is never
 *     both an alias and real;
 *   - aliases follow only the update's own runtime version (an update for
 *     today's 8f1befed reaches sideload 1.0.16, never 1.0.14/1.0.15);
 *   - an alias manifest is the original with only id + runtimeVersion changed,
 *     and the signer's check refuses anything else;
 *   - through the Worker, a sideload-runtime request gets a manifest naming
 *     its own runtime version that verifies against the certificate — the
 *     original's bytes filed under the alias would not be served.
 *
 *   cd mobile && pnpm test
 */

import test from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { handle, type BucketLike, type StoredObject } from "../../pollis-delivery/worker/updates.ts";
import {
  MANIFEST_URL,
  aliasManifest,
  aliasesOf,
  assertAliasEntries,
  assetRef,
  buildDirective,
  buildManifest,
  expandAliases,
  parseMultipart,
  parseStoreBuilds,
  partName,
  pointerFor,
  pointerKey,
  reachOf,
  signBody,
  verifyBody,
  type PlanEntry,
  type StoreBuild,
} from "../scripts/ota/lib.ts";
import { makeTestSigner } from "./ota-test-signer.ts";

const here = dirname(fileURLToPath(import.meta.url));
const ledger = parseStoreBuilds(readFileSync(join(here, "..", "store", "runtime-versions.json"), "utf8"));

const PLAY_14 = "5466ba191d93be2e4fae2ca99c6ccdc2c964ffe4";
const SIDE_14 = "cd2015f48976c765d0cf6c3495f5e3d29ef1f2f5";
const PLAY_15 = "3d24cb87e81195301172ddbc218ced95b3ba89c4";
const SIDE_15 = "a2ebce84423d3f01e2a1f4805b5da1d1c16d1697";
const PLAY_16 = "8f1befed077744ecff198dc91a0366852132f4a8";
const SIDE_16 = "27b59ee394cc3224b66c6bb87420560a6415eadc";

// ---------------------------------------------------------------------------
// The ledger
// ---------------------------------------------------------------------------

test("the shipped ledger records each sideload APK as native-identical to Play of its commit", () => {
  const aliased = ledger.filter((b) => b.nativeIdenticalTo !== undefined);
  assert.deepEqual(
    aliased.map((b) => [b.channel, b.version, b.runtimeVersion, b.nativeIdenticalTo]),
    [
      ["sideload", "1.0.14", SIDE_14, PLAY_14],
      ["sideload", "1.0.15", SIDE_15, PLAY_15],
      ["sideload", "1.0.16", SIDE_16, PLAY_16],
    ],
  );
  for (const b of aliased) {
    const target = ledger.find((t) => t.runtimeVersion === b.nativeIdenticalTo);
    assert.equal(target?.channel, "play");
    assert.equal(target?.commit, b.commit);
    assert.equal(target?.platform, b.platform);
  }
});

const base: StoreBuild = { platform: "android", channel: "play", version: "1", commit: "c1", runtimeVersion: "a".repeat(40) };
const ledgerOf = (...builds: object[]): string => JSON.stringify({ builds });

test("an alias must name a recorded runtime version of the same platform and commit", () => {
  const alias = { ...base, channel: "sideload", runtimeVersion: "b".repeat(40), nativeIdenticalTo: "a".repeat(40) };
  assert.equal(parseStoreBuilds(ledgerOf(base, alias)).length, 2);
  // Unknown target.
  assert.throws(() => parseStoreBuilds(ledgerOf(base, { ...alias, nativeIdenticalTo: "c".repeat(40) })), /not a recorded android runtime version of commit c1/);
  // Same runtime version, another commit: different native sources.
  assert.throws(() => parseStoreBuilds(ledgerOf({ ...base, commit: "c0" }, alias)), /not a recorded android runtime version of commit c1/);
  // Same runtime version, another platform.
  assert.throws(() => parseStoreBuilds(ledgerOf({ ...base, platform: "ios" }, alias)), /not a recorded android runtime version/);
  // Itself.
  assert.throws(() => parseStoreBuilds(ledgerOf(base, { ...alias, nativeIdenticalTo: alias.runtimeVersion })), /its own runtime version/);
  // Malformed.
  assert.throws(() => parseStoreBuilds(ledgerOf(base, { ...alias, nativeIdenticalTo: "8f1befed" })), /nativeIdenticalTo must be 40 lowercase hex/);
});

test("no alias chains, and a runtime version is never both an alias and real", () => {
  const b = { ...base, channel: "sideload", runtimeVersion: "b".repeat(40), nativeIdenticalTo: "a".repeat(40) };
  const c = { ...base, channel: "other", runtimeVersion: "c".repeat(40), nativeIdenticalTo: "b".repeat(40) };
  assert.throws(() => parseStoreBuilds(ledgerOf(base, b, c)), /is itself an alias/);
  // A Play build recorded with the alias's value would take the target's updates
  // without the ledger saying its native code matches.
  const realB = { ...base, channel: "play-again", runtimeVersion: "b".repeat(40) };
  assert.throws(() => parseStoreBuilds(ledgerOf(base, b, realB)), /both as an alias/);
});

// ---------------------------------------------------------------------------
// Reach
// ---------------------------------------------------------------------------

test("aliases follow only the update's own runtime version", () => {
  assert.deepEqual(aliasesOf("android", PLAY_16, ledger), [SIDE_16]);
  assert.deepEqual(aliasesOf("android", PLAY_15, ledger), [SIDE_15]);
  assert.deepEqual(aliasesOf("android", PLAY_14, ledger), [SIDE_14]);
  // Never the reverse, never across platforms, never for an unknown value.
  assert.deepEqual(aliasesOf("android", SIDE_16, ledger), []);
  assert.deepEqual(aliasesOf("ios", PLAY_16, ledger), []);
  assert.deepEqual(aliasesOf("android", "d".repeat(40), ledger), []);
});

test("an update for today's runtime version reaches Play and sideload 1.0.16 and nothing older", () => {
  const [r] = reachOf({ android: PLAY_16 }, ledger);
  assert.deepEqual(r.aliases, [SIDE_16]);
  assert.deepEqual(
    r.binaries.map((b) => `${b.channel} ${b.version}`),
    ["play 1.0.16", "sideload 1.0.16"],
  );
});

// ---------------------------------------------------------------------------
// The plan: expansion and the signer's check
// ---------------------------------------------------------------------------

function primaryEntry(rv: string = PLAY_16): PlanEntry {
  const bundle = Buffer.from("hermes bytecode … https://api.pollis.com …");
  const icon = Buffer.from("\x89PNG fake icon");
  const launch = assetRef(bundle, "hbc", undefined, true);
  const png = assetRef(icon, "png");
  const updateId = randomUUID();
  const body = buildManifest({
    id: updateId,
    createdAt: "2026-10-26T12:00:00.000Z",
    runtimeVersion: rv,
    launch,
    assets: [png],
    expoClient: { updates: { enabled: true, url: MANIFEST_URL } },
  });
  return {
    platform: "android",
    runtimeVersion: rv,
    kind: "manifest",
    body,
    updateId,
    launchSha256: launch.sha256,
    assets: [{ sha256: png.sha256, key: png.key, fileExtension: png.fileExtension, contentType: png.contentType }],
  };
}

test("expansion adds one re-built manifest per alias, differing only in id and runtimeVersion", () => {
  const primary = primaryEntry();
  const entries = expandAliases([primary], ledger, randomUUID);
  assert.equal(entries.length, 2);
  const [, alias] = entries;
  assert.equal(alias.runtimeVersion, SIDE_16);
  assert.equal(alias.aliasOf, PLAY_16);
  assert.notEqual(alias.updateId, primary.updateId);
  assert.equal(alias.launchSha256, primary.launchSha256);
  assert.deepEqual(alias.assets, primary.assets);
  const a = JSON.parse(alias.body);
  const p = JSON.parse(primary.body);
  assert.equal(a.runtimeVersion, SIDE_16);
  assert.equal(a.id, alias.updateId);
  assert.deepEqual({ ...a, id: p.id, runtimeVersion: p.runtimeVersion }, p);
  // Same key order, so the body is exactly what aliasManifest builds.
  assert.equal(alias.body, aliasManifest(primary.body, SIDE_16, alias.updateId as string));
  assert.doesNotThrow(() => assertAliasEntries(entries, ledger));
  // Idempotent: a plan that already covers the alias gains nothing.
  assert.equal(expandAliases(entries, ledger, randomUUID).length, 2);
});

test("a rollback directive is issued for the alias too", () => {
  const directive: PlanEntry = { platform: "android", runtimeVersion: PLAY_16, kind: "directive", body: buildDirective("rollBackToEmbedded", "2026-10-26T12:00:00.000Z") };
  const entries = expandAliases([directive], ledger, randomUUID);
  assert.deepEqual(entries.map((e) => [e.runtimeVersion, e.aliasOf, e.body]), [
    [PLAY_16, undefined, directive.body],
    [SIDE_16, PLAY_16, directive.body],
  ]);
  assert.doesNotThrow(() => assertAliasEntries(entries, ledger));
});

test("the signer refuses an alias the ledger does not record, a doctored alias, or a missing one", () => {
  const primary = primaryEntry();
  const [, alias] = expandAliases([primary], ledger, randomUUID);

  // The ledger has no alias 1.0.15-sideload -> 8f1befed: different native code.
  const wrong = { ...alias, runtimeVersion: SIDE_15, body: aliasManifest(primary.body, SIDE_15, alias.updateId as string) };
  assert.throws(() => assertAliasEntries([primary, alias, wrong], ledger), /does not record/);

  // Other bytes under the alias.
  const m = JSON.parse(alias.body);
  m.launchAsset.url = "https://api.pollis.com/updates/assets/" + "0".repeat(64);
  assert.throws(() => assertAliasEntries([primary, { ...alias, body: JSON.stringify(m) }], ledger), /beyond id and runtimeVersion/);
  assert.throws(() => assertAliasEntries([primary, { ...alias, launchSha256: "0".repeat(64) }], ledger), /files differ/);

  // Reusing the original's update id.
  const sameId = { ...alias, updateId: primary.updateId, body: aliasManifest(primary.body, SIDE_16, primary.updateId as string) };
  assert.throws(() => assertAliasEntries([primary, sameId], ledger), /its own update id/);

  // An alias whose target is not in the plan.
  assert.throws(() => assertAliasEntries([alias], ledger), /not a manifest entry of this plan/);

  // A plan that leaves an aliased binary behind.
  assert.throws(() => assertAliasEntries([primary], ledger), /no entry for it/);
});

// ---------------------------------------------------------------------------
// Through the Worker, as the sideload 1.0.16 binary asks
// ---------------------------------------------------------------------------

class MemoryBucket implements BucketLike {
  objects = new Map<string, Buffer>();
  async get(key: string): Promise<StoredObject | null> {
    const o = this.objects.get(key);
    if (!o) {
      return null;
    }
    return {
      async text() {
        return o.toString("utf8");
      },
      body: new Response(new Uint8Array(o)).body,
      size: o.length,
    };
  }
}

async function ask(bucket: MemoryBucket, runtimeVersion: string): Promise<Response> {
  return handle(
    new Request("https://api.pollis.com/updates/api/manifest", {
      headers: {
        "expo-protocol-version": "1",
        "expo-platform": "android",
        "expo-runtime-version": runtimeVersion,
        "expo-channel-name": "production",
        "expo-expect-signature": 'sig, keyid="main", alg="rsa-v1_5-sha256"',
      },
    }),
    { UPDATES: bucket, CHANNEL: "production" },
  );
}

test("a sideload-runtime request gets the same update, naming its own runtime version, signed", async () => {
  const signer = makeTestSigner();
  const entries = expandAliases([primaryEntry()], ledger, randomUUID);
  const bucket = new MemoryBucket();
  for (const e of entries) {
    e.signature = signBody(e.body, signer.privateKeyPem);
    bucket.objects.set(pointerKey("production", e.platform, e.runtimeVersion), Buffer.from(JSON.stringify(pointerFor(e))));
  }

  for (const rv of [PLAY_16, SIDE_16]) {
    const res = await ask(bucket, rv);
    assert.equal(res.status, 200, rv);
    const parts = parseMultipart(res.headers.get("content-type") ?? "", Buffer.from(await res.arrayBuffer()));
    const manifest = parts.find((p) => partName(p) === "manifest");
    assert.ok(manifest);
    assert.doesNotThrow(() => verifyBody(manifest.body, manifest.headers["expo-signature"], signer.certPem));
    // expo-updates launches only an update whose runtimeVersion equals the
    // binary's own (LauncherSelectionPolicyFilterAware), so this is what makes
    // the sideload binary take it.
    const m = JSON.parse(manifest.body.toString("utf8"));
    assert.equal(m.runtimeVersion, rv);
    assert.equal(m.launchAsset.hash, JSON.parse(entries[0].body).launchAsset.hash);
  }

  // Older sideload builds get nothing from today's update.
  assert.equal((await ask(bucket, SIDE_15)).status, 204);
  assert.equal((await ask(bucket, SIDE_14)).status, 204);

  // Copying the original's signed bytes under the alias instead would not be
  // served (and a phone would not launch it).
  bucket.objects.set(pointerKey("production", "android", SIDE_16), Buffer.from(JSON.stringify(pointerFor(entries[0]))));
  assert.equal((await ask(bucket, SIDE_16)).status, 500);
});
