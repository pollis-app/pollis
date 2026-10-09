// The Expo Updates protocol v1 (https://docs.expo.dev/technical-specs/expo-updates-1/),
// server side, for Pollis's over-the-air JS updates (#1250).
//
// Served by the DS front-door Worker (index.ts) under `/updates/` on
// api.pollis.com, and answered HERE, in the Worker, straight from R2: an
// update request never reaches the DS container or its Durable Object, so app
// launches and multi-MB bundle downloads never queue behind message traffic on
// the single serialized instance.
//
//   GET /updates/api/manifest          the live manifest or directive
//   GET|HEAD /updates/assets/<sha256>  content-addressed bundles and assets
//
// This module is a DUMB PIPE on purpose. It never holds the code-signing key
// and never builds, edits or re-serializes a manifest: the publish pipeline
// (mobile/scripts/ota-publish.ts) signs each manifest or directive behind an
// approval gate and stores the exact bytes plus their `expo-signature` value in
// R2, and this file passes both through unchanged. A compromised Worker can
// therefore withhold an update or serve an old signed one, but it cannot make
// a phone run code that the signing key did not sign — the app refuses any
// manifest whose signature does not verify against the certificate compiled
// into it.
//
// R2 layout (the public release bucket `pollis`, under OTA_KEY_PREFIX; written
// only by the publish pipeline, which uses the same prefix):
//
//   ota/current/<channel>/<platform>/<runtimeVersion>.json   the live pointer:
//       { "kind": "manifest" | "directive", "body": "<exact signed JSON>",
//         "signature": "sig=\"…\", keyid=\"main\", alg=\"rsa-v1_5-sha256\"" }
//   ota/assets/<sha256 hex>                                  content-addressed
//       bundles and assets, immutable
//   ota/updates/…, ota/groups/…                              the publish archive
//       (rollback/republish source); never served here
//
// The bucket is public (cdn.pollis.com), so these objects are also readable
// there. That is fine: nothing under ota/ is secret (the bundle is the same JS
// every store build ships, and every manifest is signed and logged), and the
// app only ever asks api.pollis.com/updates.
//
// Privacy: the only request data read is the protocol headers below. Nothing
// is logged, and the DS wrangler configs keep Workers Logs and Logpush off, so
// no client IP is recorded anywhere (worker-tests/updates-no-client-ip.test.ts).
//
// This module imports nothing Cloudflare-specific so it runs under `node
// --test` exactly as it runs in workerd.

// Every key this module reads lives under this prefix of the shared bucket.
// The publish pipeline writes under the same one (mobile/scripts/ota/lib.ts
// OTA_KEY_PREFIX; mobile/tests/ota-worker.test.ts pins that they agree).
export const OTA_KEY_PREFIX = "ota/";

// Every route this module owns. index.ts hands any path under it here and
// never forwards one to the container.
export const UPDATES_PATH_PREFIX = "/updates/";

export interface StoredObject {
  text(): Promise<string>;
  body: ReadableStream | null;
  size: number;
  httpEtag?: string;
  httpMetadata?: { contentType?: string };
}

export interface BucketLike {
  get(key: string): Promise<StoredObject | null>;
}

// What the protocol handlers see: a bucket whose keys are already relative to
// OTA_KEY_PREFIX, and the one channel this deployment serves.
export interface UpdatesEnv {
  UPDATES: BucketLike;
  // Requests naming any other channel are refused, so a build configured for
  // some other channel gets nothing.
  CHANNEL: string;
}

// The DS Worker's bindings this module needs. Both optional: an environment
// that binds no bucket (dev) serves no updates at all.
export interface UpdatesBindings {
  UPDATES?: BucketLike;
  OTA_CHANNEL?: string;
}

export type PointerKind = "manifest" | "directive";

export interface Pointer {
  kind: PointerKind;
  body: string;
  signature: string;
}

export const PLATFORMS = ["ios", "android"] as const;
export type Platform = (typeof PLATFORMS)[number];

// A fingerprint runtime version is 40 hex chars; anything key-safe and short is
// accepted so the policy can change without a Worker deploy, but nothing that
// could walk the R2 key space (no `/`, no `..`).
const RUNTIME_VERSION_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const ASSET_PATH_RE = /^\/updates\/assets\/([0-9a-f]{64})$/;
// Exactly what the publish pipeline writes: three sf-string members, the
// algorithm the app's codeSigningMetadata names.
const SIGNATURE_RE = /^sig="[A-Za-z0-9+/]+={0,2}", keyid="[A-Za-z0-9_-]{1,64}", alg="rsa-v1_5-sha256"$/;
const DIRECTIVE_TYPES = new Set(["noUpdateAvailable", "rollBackToEmbedded"]);

// Every protocol response says which protocol it speaks; a v1 client treats a
// 204 carrying these as "no update" and anything else as an error.
const PROTOCOL_HEADERS: Record<string, string> = {
  "expo-protocol-version": "1",
  "expo-sfv-version": "0",
  "cache-control": "private, max-age=0",
  "x-content-type-options": "nosniff",
};

function textResponse(status: number, message: string, extra: Record<string, string> = {}): Response {
  return new Response(message + "\n", {
    status,
    headers: { "content-type": "text/plain; charset=utf-8", "x-content-type-options": "nosniff", ...extra },
  });
}

export function pointerKey(channel: string, platform: Platform, runtimeVersion: string): string {
  return `current/${channel}/${platform}/${runtimeVersion}.json`;
}

// Validate a stored pointer before serving it. A pointer the pipeline did not
// write correctly is a 500, never a half-formed protocol response: the publish
// job's live re-fetch then fails loudly instead of phones quietly erroring.
export function parsePointer(raw: string, runtimeVersion: string): Pointer {
  const value = JSON.parse(raw) as Partial<Pointer>;
  if (value.kind !== "manifest" && value.kind !== "directive") {
    throw new Error("pointer kind");
  }
  if (typeof value.body !== "string" || typeof value.signature !== "string") {
    throw new Error("pointer fields");
  }
  if (!SIGNATURE_RE.test(value.signature)) {
    throw new Error("pointer signature");
  }
  const body = JSON.parse(value.body) as Record<string, unknown>;
  if (value.kind === "manifest") {
    // A manifest filed under the wrong runtime version would be refused by the
    // app anyway; refusing it here makes the mistake visible at publish time.
    if (body.runtimeVersion !== runtimeVersion) {
      throw new Error("pointer runtimeVersion");
    }
    const launch = body.launchAsset as { url?: unknown } | undefined;
    if (typeof body.id !== "string" || typeof launch?.url !== "string") {
      throw new Error("pointer manifest shape");
    }
  } else {
    if (typeof body.type !== "string" || !DIRECTIVE_TYPES.has(body.type)) {
      throw new Error("pointer directive type");
    }
  }
  return { kind: value.kind, body: value.body, signature: value.signature };
}

function randomBoundary(): string {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  let hex = "";
  for (const b of bytes) {
    hex += b.toString(16).padStart(2, "0");
  }
  return `pollis-${hex}`;
}

// multipart/mixed per the protocol: the signed part carries its signature in a
// part header, byte-for-byte as the pipeline stored it.
export function multipartBody(pointer: Pointer, boundary: string): string {
  if (pointer.body.includes(boundary)) {
    throw new Error("boundary collision");
  }
  const name = pointer.kind;
  const parts = [
    `--${boundary}\r\n` +
      `content-disposition: form-data; name="${name}"\r\n` +
      `content-type: application/json; charset=utf-8\r\n` +
      `expo-signature: ${pointer.signature}\r\n` +
      `\r\n` +
      `${pointer.body}\r\n`,
  ];
  if (pointer.kind === "manifest") {
    parts.push(
      `--${boundary}\r\n` +
        `content-disposition: form-data; name="extensions"\r\n` +
        `content-type: application/json\r\n` +
        `\r\n` +
        `{"assetRequestHeaders":{}}\r\n`,
    );
  }
  return parts.join("") + `--${boundary}--\r\n`;
}

function header(request: Request, name: string, query: string): string | null {
  const fromHeader = request.headers.get(name);
  if (fromHeader !== null && fromHeader !== "") {
    return fromHeader;
  }
  return new URL(request.url).searchParams.get(query);
}

export async function handleManifest(request: Request, env: UpdatesEnv): Promise<Response> {
  const protocol = request.headers.get("expo-protocol-version");
  if (protocol !== "1") {
    return textResponse(400, "expo-protocol-version 1 required");
  }
  const platform = header(request, "expo-platform", "platform");
  if (platform !== "ios" && platform !== "android") {
    return textResponse(400, "expo-platform must be ios or android");
  }
  const runtimeVersion = header(request, "expo-runtime-version", "runtime-version");
  if (runtimeVersion === null || !RUNTIME_VERSION_RE.test(runtimeVersion)) {
    return textResponse(400, "invalid expo-runtime-version");
  }
  const channel = request.headers.get("expo-channel-name");
  if (channel !== env.CHANNEL) {
    return textResponse(400, "unknown channel");
  }

  const object = await env.UPDATES.get(pointerKey(channel, platform, runtimeVersion));
  if (object === null) {
    // Nothing published for this binary: protocol v1's "no update available".
    return new Response(null, { status: 204, headers: PROTOCOL_HEADERS });
  }

  let pointer: Pointer;
  try {
    pointer = parsePointer(await object.text(), runtimeVersion);
  } catch {
    return textResponse(500, "invalid update pointer", PROTOCOL_HEADERS);
  }

  const boundary = randomBoundary();
  return new Response(multipartBody(pointer, boundary), {
    status: 200,
    headers: {
      ...PROTOCOL_HEADERS,
      "content-type": `multipart/mixed; boundary=${boundary}`,
      vary: "expo-platform, expo-runtime-version, expo-channel-name",
    },
  });
}

export async function handleAsset(request: Request, env: UpdatesEnv, sha256: string): Promise<Response> {
  const object = await env.UPDATES.get(`assets/${sha256}`);
  if (object === null) {
    return textResponse(404, "not found");
  }
  const headers: Record<string, string> = {
    "content-type": object.httpMetadata?.contentType ?? "application/octet-stream",
    "content-length": String(object.size),
    // Content-addressed: the URL names the bytes, so they never change.
    "cache-control": "public, max-age=31536000, immutable",
    "x-content-type-options": "nosniff",
  };
  if (object.httpEtag) {
    headers.etag = object.httpEtag;
  }
  return new Response(request.method === "HEAD" ? null : object.body, { status: 200, headers });
}

export async function handle(request: Request, env: UpdatesEnv): Promise<Response> {
  if (request.method !== "GET" && request.method !== "HEAD") {
    return textResponse(405, "method not allowed", { allow: "GET, HEAD" });
  }
  const { pathname } = new URL(request.url);
  if (pathname === "/updates/api/manifest") {
    if (request.method !== "GET") {
      return textResponse(405, "method not allowed", { allow: "GET" });
    }
    return handleManifest(request, env);
  }
  const asset = ASSET_PATH_RE.exec(pathname);
  if (asset !== null) {
    return handleAsset(request, env, asset[1]);
  }
  return textResponse(404, "not found");
}

// True for every path this module owns (and so the container never sees).
export function isUpdatesPath(pathname: string): boolean {
  return pathname === "/updates" || pathname.startsWith(UPDATES_PATH_PREFIX);
}

// Confine every read to OTA_KEY_PREFIX. The handlers' key shapes already
// cannot leave it (runtime versions and hashes admit no `/`), and R2 keys are
// literal strings with no path normalisation; this makes the bound structural.
export function prefixedBucket(bucket: BucketLike, prefix: string): BucketLike {
  return {
    get(key: string) {
      return bucket.get(prefix + key);
    },
  };
}

// Entry point from index.ts.
export async function handleUpdates(request: Request, bindings: UpdatesBindings): Promise<Response> {
  if (!bindings.UPDATES || !bindings.OTA_CHANNEL) {
    return textResponse(404, "updates are not served by this deployment");
  }
  return handle(request, {
    UPDATES: prefixedBucket(bindings.UPDATES, OTA_KEY_PREFIX),
    CHANNEL: bindings.OTA_CHANNEL,
  });
}
