/*
 * End to end, minus the network: what the publish pipeline signs, the update
 * routes in the DS front-door Worker (pollis-delivery/worker/updates.ts) serve,
 * and a client can verify with nothing but the certificate compiled into the
 * app.
 *
 * Pinned: the bytes a phone receives are byte-for-byte the bytes that were
 * signed; the signature verifies against the certificate; every asset URL in
 * the manifest resolves through the Worker to bytes matching its manifest
 * hash; a rollback directive round-trips the same way; and a Worker that
 * altered the manifest would be caught. The pipeline and the Worker also agree
 * on where the update store is (bucket + key prefix) and where it is served.
 *
 *   cd mobile && pnpm test
 */

import test from "node:test";
import assert from "node:assert/strict";

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  OTA_KEY_PREFIX as WORKER_KEY_PREFIX,
  handle,
  handleUpdates,
  isUpdatesPath,
  type BucketLike,
  type StoredObject,
} from "../../pollis-delivery/worker/updates.ts";
import {
  MANIFEST_URL,
  OTA_BUCKET,
  OTA_KEY_PREFIX,
  UPDATES_BASE_URL,
  assetRef,
  buildDirective,
  buildManifest,
  parseMultipart,
  partName,
  pointerFor,
  pointerKey,
  sha256Base64Url,
  signBody,
  verifyBody,
  type PlanEntry,
} from "../scripts/ota/lib.ts";
import { makeTestSigner } from "./ota-test-signer.ts";

const signer = makeTestSigner();
const RV = "376466c81d9428da79f80525da44567b98aa4e4a";

class MemoryBucket implements BucketLike {
  objects = new Map<string, { bytes: Buffer; contentType?: string }>();
  put(key: string, bytes: Buffer | string, contentType?: string): void {
    this.objects.set(key, { bytes: Buffer.from(bytes), contentType });
  }
  async get(key: string): Promise<StoredObject | null> {
    const o = this.objects.get(key);
    if (!o) {
      return null;
    }
    return {
      async text() {
        return o.bytes.toString("utf8");
      },
      body: new Response(new Uint8Array(o.bytes)).body,
      size: o.bytes.length,
      httpMetadata: { contentType: o.contentType },
    };
  }
}

function publish(bucket: MemoryBucket): PlanEntry {
  const bundle = Buffer.from("hermes bytecode … https://api.pollis.com …");
  const icon = Buffer.from("\x89PNG fake icon");
  const launch = assetRef(bundle, "hbc", undefined, true);
  const png = assetRef(icon, "png");
  bucket.put(`assets/${launch.sha256}`, bundle, launch.contentType);
  bucket.put(`assets/${png.sha256}`, icon, png.contentType);
  const body = buildManifest({
    id: "dd7c08cc-ce5e-4ea2-b437-b106ad5ec958",
    createdAt: "2026-10-09T12:00:00.000Z",
    runtimeVersion: RV,
    launch,
    assets: [png],
    expoClient: { updates: { enabled: true, url: MANIFEST_URL } },
  });
  const entry: PlanEntry = { platform: "ios", runtimeVersion: RV, kind: "manifest", body, signature: signBody(body, signer.privateKeyPem) };
  bucket.put(pointerKey("production", "ios", RV), JSON.stringify(pointerFor(entry)), "application/json");
  return entry;
}

async function fetchFromWorker(bucket: MemoryBucket): Promise<{ name: string | null; body: Buffer; signature: string }> {
  const res = await handle(
    new Request("https://api.pollis.com/updates/api/manifest", {
      headers: {
        "expo-protocol-version": "1",
        "expo-platform": "ios",
        "expo-runtime-version": RV,
        "expo-channel-name": "production",
        "expo-expect-signature": 'sig, keyid="main", alg="rsa-v1_5-sha256"',
      },
    }),
    { UPDATES: bucket, CHANNEL: "production" },
  );
  assert.equal(res.status, 200);
  const parts = parseMultipart(res.headers.get("content-type") ?? "", Buffer.from(await res.arrayBuffer()));
  const signed = parts.find((p) => partName(p) === "manifest" || partName(p) === "directive");
  assert.ok(signed);
  return { name: partName(signed), body: signed.body, signature: signed.headers["expo-signature"] };
}

test("the served manifest is the signed bytes and verifies against the certificate", async () => {
  const bucket = new MemoryBucket();
  const entry = publish(bucket);
  const got = await fetchFromWorker(bucket);
  assert.equal(got.name, "manifest");
  assert.equal(got.body.toString("utf8"), entry.body);
  assert.equal(got.signature, entry.signature);
  assert.doesNotThrow(() => verifyBody(got.body, got.signature, signer.certPem));
});

test("every asset in the manifest resolves through the Worker to bytes matching its hash", async () => {
  const bucket = new MemoryBucket();
  publish(bucket);
  const m = JSON.parse((await fetchFromWorker(bucket)).body.toString("utf8"));
  for (const a of [m.launchAsset, ...m.assets]) {
    const res = await handle(new Request(a.url), { UPDATES: bucket, CHANNEL: "production" });
    assert.equal(res.status, 200, a.url);
    assert.equal(res.headers.get("content-type"), a.contentType);
    assert.equal(sha256Base64Url(Buffer.from(await res.arrayBuffer())), a.hash);
  }
});

test("a manifest altered after signing is refused by the client check", async () => {
  const bucket = new MemoryBucket();
  const entry = publish(bucket);
  const tampered = entry.body.replace("https://api.pollis.com/updates/assets/", "https://evil.example/assets/");
  bucket.put(pointerKey("production", "ios", RV), JSON.stringify({ kind: "manifest", body: tampered, signature: entry.signature }));
  const got = await fetchFromWorker(bucket);
  assert.throws(() => verifyBody(got.body, got.signature, signer.certPem), /does not verify/);
});

test("a signed rollBackToEmbedded directive round-trips", async () => {
  const bucket = new MemoryBucket();
  publish(bucket);
  const body = buildDirective("rollBackToEmbedded", "2026-10-09T13:00:00.000Z");
  const entry: PlanEntry = { platform: "ios", runtimeVersion: RV, kind: "directive", body, signature: signBody(body, signer.privateKeyPem) };
  bucket.put(pointerKey("production", "ios", RV), JSON.stringify(pointerFor(entry)));
  const got = await fetchFromWorker(bucket);
  assert.equal(got.name, "directive");
  assert.equal(got.body.toString("utf8"), body);
  assert.doesNotThrow(() => verifyBody(got.body, got.signature, signer.certPem));
});

const repo = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

function jsonc(path: string): Record<string, unknown> {
  const raw = readFileSync(path, "utf8");
  const stripped = raw.replace(/("(?:[^"\\]|\\.)*")|\/\/[^\n]*/g, (_m, str: string | undefined) => str ?? "");
  return JSON.parse(stripped);
}

test("the pipeline and the DS Worker agree on the update store and its URL", async () => {
  // Same key prefix on both sides.
  assert.equal(OTA_KEY_PREFIX, WORKER_KEY_PREFIX);
  // The prod DS config binds the bucket the pipeline writes, and every workflow
  // step that touches the update store names it.
  const prod = jsonc(join(repo, "pollis-delivery", "wrangler.prod.jsonc"));
  assert.deepEqual(prod.r2_buckets, [{ binding: "UPDATES", bucket_name: OTA_BUCKET }]);
  const workflow = readFileSync(join(repo, ".github", "workflows", "mobile-ota-release.yml"), "utf8");
  const buckets = [...workflow.matchAll(/--bucket (\S+)/g)].map((m) => m[1]);
  assert.ok(buckets.length >= 4, buckets.join(","));
  for (const b of buckets) {
    assert.ok(b === OTA_BUCKET || b === '"$R2_BUCKET"', `unexpected --bucket ${b}`);
  }
  assert.doesNotMatch(workflow, /R2_UPDATES_|updates\.pollis\.com/);
  // The URLs the app and the manifests use are routes the DS claims.
  for (const url of [MANIFEST_URL, `${UPDATES_BASE_URL}/assets/${"0".repeat(64)}`]) {
    const u = new URL(url);
    assert.equal(u.host, "api.pollis.com");
    assert.ok(isUpdatesPath(u.pathname), url);
  }
  // And a pointer the pipeline files under OTA_KEY_PREFIX is what the DS
  // bindings serve.
  const bucket = new MemoryBucket();
  const entry = publish(bucket);
  const prefixed = new MemoryBucket();
  for (const [k, v] of bucket.objects) {
    prefixed.objects.set(OTA_KEY_PREFIX + k, v);
  }
  const res = await handleUpdates(
    new Request(MANIFEST_URL, {
      headers: { "expo-protocol-version": "1", "expo-platform": "ios", "expo-runtime-version": RV, "expo-channel-name": "production" },
    }),
    { UPDATES: prefixed, OTA_CHANNEL: "production" },
  );
  assert.equal(res.status, 200);
  const parts = parseMultipart(res.headers.get("content-type") ?? "", Buffer.from(await res.arrayBuffer()));
  assert.equal(parts.find((p) => partName(p) === "manifest")?.body.toString("utf8"), entry.body);
});
