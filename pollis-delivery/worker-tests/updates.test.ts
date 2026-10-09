/*
 * worker/updates.ts — the Expo Updates protocol v1 responses the DS front-door
 * Worker gives under api.pollis.com/updates/ (mobile OTA, #1250).
 *
 * Pinned: a published pointer comes back as multipart/mixed with the protocol
 * headers and the stored signature passed through byte-for-byte on the signed
 * part; nothing published is a 204 (v1's "no update"); directives are served
 * as directive parts; malformed requests are 400s that never touch R2; a
 * malformed pointer is a 500, never a half-formed update; assets are served by
 * content address only; every other path/method is refused; the DS bindings
 * confine every read to the `ota/` prefix of the shared bucket, and an
 * environment with no bucket bound serves nothing; only /updates paths are
 * claimed from the container.
 *
 *   node --test pollis-delivery/worker-tests/*.test.ts   (or `pnpm test:worker` in pollis-delivery/)
 */

import test from "node:test";
import assert from "node:assert/strict";

import {
  OTA_KEY_PREFIX,
  handle,
  handleUpdates,
  isUpdatesPath,
  multipartBody,
  parsePointer,
  pointerKey,
  type BucketLike,
  type StoredObject,
} from "../worker/updates.ts";

const RV = "376466c81d9428da79f80525da44567b98aa4e4a";
const SIG = 'sig="c2lnbmF0dXJl", keyid="main", alg="rsa-v1_5-sha256"';
const SHA = "09f8db0f31a3a02acbf80a31f7e1ff83002b6a54b87aece45c50296673bd5072";

const MANIFEST = JSON.stringify({
  id: "dd7c08cc-ce5e-4ea2-b437-b106ad5ec958",
  createdAt: "2026-10-09T12:00:00.000Z",
  runtimeVersion: RV,
  launchAsset: { hash: "x", key: "k", contentType: "application/javascript", fileExtension: ".bundle", url: `https://api.pollis.com/updates/assets/${SHA}` },
  assets: [],
  metadata: {},
  extra: {},
});

function object(body: string | Uint8Array, contentType?: string): StoredObject {
  const bytes = typeof body === "string" ? new TextEncoder().encode(body) : body;
  return {
    async text() {
      return new TextDecoder().decode(bytes);
    },
    body: new Response(bytes).body,
    size: bytes.length,
    httpEtag: '"etag-1"',
    httpMetadata: contentType ? { contentType } : {},
  };
}

class FakeBucket implements BucketLike {
  reads: string[] = [];
  objects: Record<string, () => StoredObject>;
  constructor(objects: Record<string, () => StoredObject>) {
    this.objects = objects;
  }
  async get(key: string): Promise<StoredObject | null> {
    this.reads.push(key);
    const make = this.objects[key];
    return make ? make() : null;
  }
}

function env(objects: Record<string, () => StoredObject> = {}) {
  return { UPDATES: new FakeBucket(objects), CHANNEL: "production" };
}

function manifestRequest(headers: Record<string, string> = {}, query = ""): Request {
  return new Request(`https://api.pollis.com/updates/api/manifest${query}`, {
    headers: {
      "expo-protocol-version": "1",
      "expo-platform": "ios",
      "expo-runtime-version": RV,
      "expo-channel-name": "production",
      ...headers,
    },
  });
}

function pointer(kind: "manifest" | "directive", body: string, signature = SIG): () => StoredObject {
  return () => object(JSON.stringify({ kind, body, signature }));
}

function boundaryOf(res: Response): string {
  const m = /^multipart\/mixed; boundary=(\S+)$/.exec(res.headers.get("content-type") ?? "");
  assert.ok(m, `content-type ${res.headers.get("content-type")}`);
  return m[1];
}

test("a published manifest is served multipart with its signature passed through", async () => {
  const e = env({ [pointerKey("production", "ios", RV)]: pointer("manifest", MANIFEST) });
  const res = await handle(manifestRequest(), e);
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("expo-protocol-version"), "1");
  assert.equal(res.headers.get("expo-sfv-version"), "0");
  assert.equal(res.headers.get("cache-control"), "private, max-age=0");
  const b = boundaryOf(res);
  const text = await res.text();
  assert.equal(
    text,
    `--${b}\r\ncontent-disposition: form-data; name="manifest"\r\ncontent-type: application/json; charset=utf-8\r\n` +
      `expo-signature: ${SIG}\r\n\r\n${MANIFEST}\r\n` +
      `--${b}\r\ncontent-disposition: form-data; name="extensions"\r\ncontent-type: application/json\r\n\r\n{"assetRequestHeaders":{}}\r\n` +
      `--${b}--\r\n`,
  );
  assert.deepEqual(e.UPDATES.reads, [`current/production/ios/${RV}.json`]);
});

test("each response gets a fresh boundary", async () => {
  const e = env({ [pointerKey("production", "ios", RV)]: pointer("manifest", MANIFEST) });
  const a = boundaryOf(await handle(manifestRequest(), e));
  const b = boundaryOf(await handle(manifestRequest(), e));
  assert.notEqual(a, b);
});

test("platform and runtime version route to separate pointers", async () => {
  const androidRv = "650428a8330d3d6e164e93a21f4fa73aa7088395";
  const e = env();
  await handle(manifestRequest({ "expo-platform": "android", "expo-runtime-version": androidRv }), e);
  assert.deepEqual(e.UPDATES.reads, [`current/production/android/${androidRv}.json`]);
});

test("platform and runtime version may come from the query when headers are absent", async () => {
  const e = env();
  const res = await handle(
    manifestRequest({ "expo-platform": "", "expo-runtime-version": "" }, `?platform=android&runtime-version=${RV}`),
    e,
  );
  assert.equal(res.status, 204);
  assert.deepEqual(e.UPDATES.reads, [`current/production/android/${RV}.json`]);
});

test("nothing published is a protocol-v1 204", async () => {
  const res = await handle(manifestRequest(), env());
  assert.equal(res.status, 204);
  assert.equal(res.headers.get("expo-protocol-version"), "1");
  assert.equal(res.headers.get("expo-sfv-version"), "0");
  assert.equal(await res.text(), "");
});

test("a rollBackToEmbedded directive is served as a signed directive part, without extensions", async () => {
  const directive = JSON.stringify({ type: "rollBackToEmbedded", parameters: { commitTime: "2026-10-09T13:00:00.000Z" } });
  const e = env({ [pointerKey("production", "ios", RV)]: pointer("directive", directive) });
  const res = await handle(manifestRequest(), e);
  assert.equal(res.status, 200);
  const text = await res.text();
  assert.ok(text.includes(`content-disposition: form-data; name="directive"\r\n`));
  assert.ok(text.includes(`expo-signature: ${SIG}\r\n\r\n${directive}\r\n`));
  assert.ok(!text.includes('name="manifest"'));
  assert.ok(!text.includes('name="extensions"'));
});

test("a noUpdateAvailable directive is served too", async () => {
  const e = env({ [pointerKey("production", "ios", RV)]: pointer("directive", '{"type":"noUpdateAvailable"}') });
  const res = await handle(manifestRequest(), e);
  assert.equal(res.status, 200);
  assert.ok((await res.text()).includes('{"type":"noUpdateAvailable"}'));
});

test("malformed requests are 400s that never reach R2", async () => {
  const cases: Record<string, string>[] = [
    { "expo-protocol-version": "0" },
    { "expo-protocol-version": "" },
    { "expo-platform": "web" },
    { "expo-runtime-version": "../../assets/x" },
    { "expo-runtime-version": "a/b" },
    { "expo-channel-name": "staging" },
    { "expo-channel-name": "" },
  ];
  for (const headers of cases) {
    const e = env();
    const res = await handle(manifestRequest(headers), e);
    assert.equal(res.status, 400, JSON.stringify(headers));
    assert.deepEqual(e.UPDATES.reads, [], JSON.stringify(headers));
  }
});

test("a pointer the pipeline did not write correctly is a 500, never a partial update", async () => {
  const bad: (() => StoredObject)[] = [
    () => object("not json"),
    pointer("manifest", MANIFEST, "sig=unquoted"),
    pointer("manifest", MANIFEST, 'sig="abc", keyid="main", alg="rsa-v1_5-sha512"'),
    pointer("manifest", MANIFEST.replace(RV, "a-different-runtime")),
    pointer("directive", '{"type":"launchSomethingElse"}'),
    () => object(JSON.stringify({ kind: "other", body: MANIFEST, signature: SIG })),
  ];
  for (const make of bad) {
    const res = await handle(manifestRequest(), env({ [pointerKey("production", "ios", RV)]: make }));
    assert.equal(res.status, 500);
  }
});

test("parsePointer accepts exactly what the pipeline writes", () => {
  const p = parsePointer(JSON.stringify({ kind: "manifest", body: MANIFEST, signature: SIG }), RV);
  assert.deepEqual(p, { kind: "manifest", body: MANIFEST, signature: SIG });
});

test("multipartBody refuses a boundary that occurs inside the body", () => {
  assert.throws(() => multipartBody({ kind: "manifest", body: '{"x":"pollis-abc"}', signature: SIG }, "pollis-abc"), /collision/);
});

test("assets are served by content address with immutable caching", async () => {
  const bytes = new Uint8Array([1, 2, 3, 4]);
  const e = env({ [`assets/${SHA}`]: () => object(bytes, "image/png") });
  const res = await handle(new Request(`https://api.pollis.com/updates/assets/${SHA}`), e);
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("content-type"), "image/png");
  assert.equal(res.headers.get("content-length"), "4");
  assert.equal(res.headers.get("cache-control"), "public, max-age=31536000, immutable");
  assert.deepEqual(new Uint8Array(await res.arrayBuffer()), bytes);

  const head = await handle(new Request(`https://api.pollis.com/updates/assets/${SHA}`, { method: "HEAD" }), e);
  assert.equal(head.status, 200);
  assert.equal(await head.text(), "");
});

test("a missing asset is a 404 and a non-sha256 asset path never reaches R2", async () => {
  const e = env();
  assert.equal((await handle(new Request(`https://api.pollis.com/updates/assets/${SHA}`), e)).status, 404);
  const before = e.UPDATES.reads.length;
  for (const path of [
    "/updates/assets/abc",
    `/updates/assets/${SHA.toUpperCase()}`,
    `/updates/assets/${SHA}/x`,
    "/updates/assets/../current/production/ios/x.json",
    // The pre-fold path layout (updates.pollis.com) is not served.
    `/assets/${SHA}`,
  ]) {
    assert.equal((await handle(new Request(`https://api.pollis.com${path}`), e)).status, 404, path);
  }
  assert.equal(e.UPDATES.reads.length, before);
});

test("only GET/HEAD, and only the two routes", async () => {
  const e = env();
  assert.equal((await handle(new Request("https://api.pollis.com/updates/api/manifest", { method: "POST" }), e)).status, 405);
  assert.equal((await handle(new Request("https://api.pollis.com/updates/api/manifest", { method: "HEAD" }), e)).status, 405);
  for (const path of ["/updates/", "/updates/updates/x.json", "/updates/current/production/ios/x.json", "/updates/groups/x.json", "/api/manifest"]) {
    assert.equal((await handle(new Request(`https://api.pollis.com${path}`), e)).status, 404, path);
  }
  assert.deepEqual(e.UPDATES.reads, []);
});

test("the DS bindings read only under the ota/ prefix of the shared bucket", async () => {
  const bucket = new FakeBucket({
    [`${OTA_KEY_PREFIX}${pointerKey("production", "ios", RV)}`]: pointer("manifest", MANIFEST),
    [`${OTA_KEY_PREFIX}assets/${SHA}`]: () => object("bytes", "image/png"),
    // An unprefixed object — e.g. anything else in the release bucket — is invisible.
    [pointerKey("production", "android", RV)]: pointer("manifest", MANIFEST),
  });
  const bindings = { UPDATES: bucket, OTA_CHANNEL: "production" };
  assert.equal((await handleUpdates(manifestRequest(), bindings)).status, 200);
  assert.equal((await handleUpdates(new Request(`https://api.pollis.com/updates/assets/${SHA}`), bindings)).status, 200);
  assert.equal((await handleUpdates(manifestRequest({ "expo-platform": "android" }), bindings)).status, 204);
  assert.deepEqual(bucket.reads, [
    `ota/current/production/ios/${RV}.json`,
    `ota/assets/${SHA}`,
    `ota/current/production/android/${RV}.json`,
  ]);
});

test("an environment with no bucket or channel bound serves no updates", async () => {
  const bucket = new FakeBucket({});
  for (const bindings of [{}, { UPDATES: bucket }, { OTA_CHANNEL: "production" }]) {
    const res = await handleUpdates(manifestRequest(), bindings);
    assert.equal(res.status, 404, JSON.stringify(Object.keys(bindings)));
  }
  assert.deepEqual(bucket.reads, []);
});

test("only /updates paths are claimed from the container", () => {
  for (const path of ["/updates", "/updates/", "/updates/api/manifest", `/updates/assets/${SHA}`, "/updates/anything"]) {
    assert.equal(isUpdatesPath(path), true, path);
  }
  for (const path of ["/", "/version", "/v1/config", "/api/manifest", `/assets/${SHA}`, "/updatesx", "/v1/updates"]) {
    assert.equal(isUpdatesPath(path), false, path);
  }
});
