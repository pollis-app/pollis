/*
 * scripts/ota/lib.ts — manifest building, code signing, the store-build safety
 * checks, and the transparency leaves of an OTA update (#1250).
 *
 * Pinned: the manifest is the protocol-v1 shape with content-addressed URLs and
 * base64url sha256 hashes; a signature verifies against the certificate (the
 * way the app checks it) and fails for a tampered body, another key, another
 * key id or an expired/non-code-signing certificate; Expo's own signer and ours
 * agree; the bundle/env checks refuse api-dev and non-public EXPO_PUBLIC_*;
 * the leaves keep the frozen BinaryRecord field order.
 *
 *   cd mobile && pnpm test
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  ALG,
  KEY_ID,
  MANIFEST_URL,
  assertBundleTargetsProd,
  assertCodeSigningCertificate,
  assertKeyMatchesCertificate,
  assertManifestShape,
  assertNoSecretValues,
  assertOnlyPublicEnvReads,
  assertPublicEnv,
  assetListFile,
  assetRef,
  attestedFiles,
  buildDirective,
  buildManifest,
  findEnvReads,
  md5Hex,
  mergeRecords,
  parseSignatureHeader,
  sha256Hex,
  signBody,
  signatureHeader,
  verifyBody,
  type Plan,
} from "../scripts/ota/lib.ts";
import { expoSign, makeTestSigner } from "./ota-test-signer.ts";

const here = dirname(fileURLToPath(import.meta.url));
const signer = makeTestSigner();
const RV = "376466c81d9428da79f80525da44567b98aa4e4a";
const ID = "dd7c08cc-ce5e-4ea2-b437-b106ad5ec958";

const expoClient = {
  name: "Pollis",
  updates: { enabled: true, url: MANIFEST_URL, requestHeaders: { "expo-channel-name": "production" } },
  extra: { eas: { projectId: "74bee5ea-4d9b-4a1b-9fb3-5e1775f35990" } },
};

function sampleManifest(overrides: Partial<{ runtimeVersion: string; expoClient: Record<string, unknown> }> = {}): string {
  const bundle = Buffer.from("fake hermes bytecode naming https://api.pollis.com");
  return buildManifest({
    id: ID,
    createdAt: "2026-10-09T12:00:00.000Z",
    runtimeVersion: overrides.runtimeVersion ?? RV,
    launch: assetRef(bundle, "hbc", undefined, true),
    assets: [assetRef(Buffer.from("png bytes"), "png")],
    expoClient: overrides.expoClient ?? expoClient,
  });
}

test("the manifest is the protocol-v1 shape with content-addressed assets", () => {
  const m = JSON.parse(sampleManifest());
  assert.deepEqual(Object.keys(m), ["id", "createdAt", "runtimeVersion", "launchAsset", "assets", "metadata", "extra"]);
  assert.equal(m.launchAsset.contentType, "application/javascript");
  assert.equal(m.launchAsset.fileExtension, ".bundle");
  const png = Buffer.from("png bytes");
  assert.deepEqual(m.assets[0], {
    hash: Buffer.from(sha256Hex(png), "hex").toString("base64url"),
    key: md5Hex(png),
    contentType: "image/png",
    fileExtension: ".png",
    url: `https://api.pollis.com/updates/assets/${sha256Hex(png)}`,
  });
  assert.equal(m.extra.expoClient.extra.eas.projectId, expoClient.extra.eas.projectId);
  assert.doesNotThrow(() => assertManifestShape(sampleManifest(), { runtimeVersion: RV }));
});

test("buildManifest refuses a non-UUID id or an unsafe runtime version", () => {
  const launch = assetRef(Buffer.from("x"), "hbc", undefined, true);
  const base = { createdAt: "2026-10-09T12:00:00Z", launch, assets: [], expoClient };
  assert.throws(() => buildManifest({ ...base, id: "not-a-uuid", runtimeVersion: RV }), /UUID/);
  assert.throws(() => buildManifest({ ...base, id: ID, runtimeVersion: "../escape" }), /runtime version/);
});

test("assertManifestShape refuses a URL that is not its hash's content address", () => {
  const m = JSON.parse(sampleManifest());
  m.assets[0].url = "https://evil.example/asset";
  assert.throws(() => assertManifestShape(JSON.stringify(m), { runtimeVersion: RV }), /content address/);
});

test("assertManifestShape refuses a wrong runtime version or a non-prod expoClient", () => {
  assert.throws(() => assertManifestShape(sampleManifest(), { runtimeVersion: "other" }), /runtimeVersion/);
  const dev = sampleManifest({ expoClient: { ...expoClient, updates: { enabled: false } } });
  assert.throws(() => assertManifestShape(dev, { runtimeVersion: RV }), /production configuration/);
});

test("a signed manifest verifies against the certificate, exactly as the app checks it", () => {
  const body = sampleManifest();
  const header = signBody(body, signer.privateKeyPem);
  assert.match(header, /^sig="[A-Za-z0-9+/]+={0,2}", keyid="main", alg="rsa-v1_5-sha256"$/);
  assert.doesNotThrow(() => verifyBody(body, header, signer.certPem));
  assert.deepEqual(Object.keys(parseSignatureHeader(header)), ["sig", "keyid", "alg"]);
});

test("verification fails for a tampered body, another key, or another key id", () => {
  const body = sampleManifest();
  const header = signBody(body, signer.privateKeyPem);
  assert.throws(() => verifyBody(body.replace(ID, "00000000-0000-4000-8000-000000000000"), header, signer.certPem), /does not verify/);
  const other = makeTestSigner("someone else");
  assert.throws(() => verifyBody(body, signBody(body, other.privateKeyPem), signer.certPem), /does not verify/);
  const sig = parseSignatureHeader(header).sig;
  assert.throws(() => verifyBody(body, `sig="${sig}", keyid="root", alg="${ALG}"`, signer.certPem), /keyid/);
  assert.throws(() => verifyBody(body, "garbage", signer.certPem), /malformed/);
  assert.throws(() => assertKeyMatchesCertificate(other.privateKeyPem, signer.certPem), /does not verify/);
  assert.doesNotThrow(() => assertKeyMatchesCertificate(signer.privateKeyPem, signer.certPem));
});

test("Expo's own signer and ours produce signatures the other accepts", () => {
  const body = sampleManifest();
  const expoSig = expoSign(signer, body);
  assert.doesNotThrow(() => verifyBody(body, signatureHeader(expoSig, KEY_ID), signer.certPem));
  // RSASSA-PKCS1-v1_5 is deterministic: the same key and bytes give the same signature.
  assert.equal(parseSignatureHeader(signBody(body, signer.privateKeyPem)).sig, expoSig);
});

test("a certificate outside its validity window is refused", () => {
  assert.doesNotThrow(() => assertCodeSigningCertificate(signer.certPem));
  assert.throws(() => assertCodeSigningCertificate(signer.certPem, new Date(Date.now() + 7 * 24 * 3600_000)), /not valid now/);
});

test("a rollBackToEmbedded directive carries its commit time and is signable", () => {
  const body = buildDirective("rollBackToEmbedded", "2026-10-09T13:00:00.000Z");
  assert.deepEqual(JSON.parse(body), { type: "rollBackToEmbedded", parameters: { commitTime: "2026-10-09T13:00:00.000Z" } });
  assert.doesNotThrow(() => verifyBody(body, signBody(body, signer.privateKeyPem), signer.certPem));
  assert.deepEqual(JSON.parse(buildDirective("noUpdateAvailable", "")), { type: "noUpdateAvailable" });
});

test("the bundle check wants api.pollis.com and never api-dev", () => {
  assert.doesNotThrow(() => assertBundleTargetsProd(Buffer.from("…https://api.pollis.com…"), "ios"));
  assert.throws(() => assertBundleTargetsProd(Buffer.from("nothing"), "ios"), /does not name api.pollis.com/);
  assert.throws(
    () => assertBundleTargetsProd(Buffer.from("https://api.pollis.com https://api-dev.pollis.com"), "ios"),
    /names api-dev/,
  );
});

test("no secret value the publisher can see may sit in the bundle", () => {
  const secret = "R2SECRETVALUE-0123456789abcdef";
  assert.throws(() => assertNoSecretValues(Buffer.from(`x${secret}x`), { AWS_SECRET_ACCESS_KEY: secret }, "ios"), /AWS_SECRET_ACCESS_KEY/);
  assert.doesNotThrow(() => assertNoSecretValues(Buffer.from("clean"), { AWS_SECRET_ACCESS_KEY: secret }, "ios"));
});

test("only the four public EXPO_PUBLIC_* names may be set, and the DS must be prod", () => {
  const ok = { EXPO_PUBLIC_POLLIS_DELIVERY_URL: "https://api.pollis.com", EXPO_PUBLIC_LIVEKIT_URL: "wss://lk.example" };
  assert.doesNotThrow(() => assertPublicEnv(ok));
  assert.throws(() => assertPublicEnv({ ...ok, EXPO_PUBLIC_LIVEKIT_API_SECRET: "s" }), /not a public variable/);
  assert.throws(() => assertPublicEnv({ EXPO_PUBLIC_POLLIS_DELIVERY_URL: "https://api-dev.pollis.com" }), /api-dev/);
  assert.throws(() => assertPublicEnv({}), /must be exactly/);
});

test("the app's own sources read only public EXPO_PUBLIC_* names", () => {
  const files = ["app/_layout.tsx", "lib/realtime/client.ts"].map((p) => ({ path: p, text: readFileSync(join(here, "..", p), "utf8") }));
  assert.ok(findEnvReads(files).length >= 4, "the env reads are still where this test expects them");
  assert.doesNotThrow(() => assertOnlyPublicEnvReads(files));
  assert.throws(
    () => assertOnlyPublicEnvReads([{ path: "x.ts", text: "const k = process.env.EXPO_PUBLIC_RESEND_API_KEY;" }]),
    /EXPO_PUBLIC_RESEND_API_KEY/,
  );
  assert.throws(() => assertOnlyPublicEnvReads([{ path: "x.ts", text: 'process.env["EXPO_PUBLIC_TURSO_TOKEN"]' }]), /TURSO/);
});

function samplePlan(): Plan {
  const body = sampleManifest();
  const m = JSON.parse(body);
  return {
    version: 1,
    action: "publish",
    groupId: "39c30b86-563b-4f10-a062-f7820bbd70a8",
    createdAt: "2026-10-09T12:00:00.000Z",
    channel: "production",
    baseUrl: "https://api.pollis.com/updates",
    commit: "f".repeat(40),
    sourceDateEpoch: 1_760_000_000,
    toolchain: { rustc: "1.96.0", node: "22.20.0", pnpm: "10.25.0", runnerImage: "ubuntu24@20261005.1" },
    entries: [
      {
        platform: "ios",
        runtimeVersion: RV,
        kind: "manifest",
        body,
        updateId: ID,
        launchSha256: Buffer.from(m.launchAsset.hash, "base64url").toString("hex"),
        assets: [{ sha256: Buffer.from(m.assets[0].hash, "base64url").toString("hex"), key: m.assets[0].key, fileExtension: ".png", contentType: "image/png" }],
      },
      { platform: "android", runtimeVersion: "650428a8330d3d6e164e93a21f4fa73aa7088395", kind: "directive", body: buildDirective("rollBackToEmbedded", "2026-10-09T12:00:00.000Z") },
    ],
    files: {},
  };
}

test("an update becomes three payload leaves and a directive one, keyed by runtime version", () => {
  const plan = samplePlan();
  const files = attestedFiles(plan, () => Buffer.from("bundle bytes"));
  assert.deepEqual(
    files.map((f) => f.record.bundle),
    ["ota-manifest", "ota-bundle", "ota-assets", "ota-directive"],
  );
  for (const f of files) {
    assert.equal(f.record.release_tag, `mobile-ota-${plan.groupId}`);
    assert.equal(f.record.layer, "payload");
    assert.equal(f.record.artifact_sha256, sha256Hex(f.bytes));
    assert.equal(f.record.payload_sha256, f.record.artifact_sha256);
    assert.equal(f.record.provenance_uri, `cdn.pollis.com/releases/mobile-ota/${f.record.release_tag}/${f.artifactName}.intoto.jsonl`);
  }
  assert.equal(files[0].record.arch, RV);
  assert.equal(files[0].bytes.toString("utf8"), plan.entries[0].body);
  assert.equal(files[2].bytes.toString("utf8"), assetListFile(plan.entries[0]));
  assert.match(files[2].bytes.toString("utf8"), /^[0-9a-f]{64} {2}[0-9a-f]{32}\.png\n$/);
});

test("leaves keep the frozen BinaryRecord field order", () => {
  const rust = readFileSync(join(here, "..", "..", "verifiable-log-builder", "src", "binaries.rs"), "utf8");
  const struct = /pub struct BinaryRecord \{([\s\S]*?)\n\}/.exec(rust)?.[1] ?? "";
  const fields = [...struct.matchAll(/pub (\w+):/g)].map((m) => m[1]);
  const tool = /pub struct Toolchain \{([\s\S]*?)\n\}/.exec(rust)?.[1] ?? "";
  const toolFields = [...tool.matchAll(/pub (\w+):/g)].map((m) => m[1]);
  const record = attestedFiles(samplePlan(), () => Buffer.from("b"))[0].record;
  assert.deepEqual(Object.keys(record), fields);
  assert.deepEqual(Object.keys(record.toolchain), toolFields);
});

test("merging into the accumulator appends once and is idempotent per group", () => {
  const records = attestedFiles(samplePlan(), () => Buffer.from("b")).map((f) => f.record);
  const merged = mergeRecords([], records);
  assert.equal(merged?.length, 4);
  assert.equal(mergeRecords(merged ?? [], records), null);
  assert.throws(() => mergeRecords([], [...records, { ...records[0], release_tag: "mobile-ota-other" }]), /exactly one/);
});
