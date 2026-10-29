// The Expo Updates protocol v1 server for mobile OTA JS updates (#1250),
// answered in the Worker straight from R2 so update traffic never queues behind
// the single DS instance. A DUMB PIPE on purpose: it holds no signing key and
// passes the pipeline's pre-signed bytes and `expo-signature` through
// unchanged, so a compromised Worker can withhold an update but not make a
// phone run unsigned code. Nothing is logged, so no client IP is recorded.
// R2 layout and pipeline: .codesight/wiki/ota-updates.md.
//
// Imports nothing Cloudflare-specific so it runs under `node --test`.

// Must equal mobile/scripts/ota/lib.ts OTA_KEY_PREFIX (pinned by
// mobile/tests/ota-worker.test.ts).
export const OTA_KEY_PREFIX = "ota/";

// index.ts never forwards a path under this to the container.
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

// Keys already relative to OTA_KEY_PREFIX.
export interface UpdatesEnv {
  UPDATES: BucketLike;
  // Requests naming any other channel are refused.
  CHANNEL: string;
}

// Both optional: an environment that binds no bucket (dev) serves no updates.
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

// Looser than the 40-hex fingerprint so the policy can change without a deploy,
// but nothing that could walk the R2 key space (no `/`, no `..`).
const RUNTIME_VERSION_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const ASSET_PATH_RE = /^\/updates\/assets\/([0-9a-f]{64})$/;
// Exactly what the publish pipeline writes.
const SIGNATURE_RE = /^sig="[A-Za-z0-9+/]+={0,2}", keyid="[A-Za-z0-9_-]{1,64}", alg="rsa-v1_5-sha256"$/;
const DIRECTIVE_TYPES = new Set(["noUpdateAvailable", "rollBackToEmbedded"]);

// A v1 client treats a 204 carrying these as "no update".
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

// A malformed pointer is a 500, never a half-formed protocol response, so the
// publish job's live re-fetch fails loudly.
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
    // The app would refuse it anyway; refusing here surfaces it at publish time.
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

// The signed part carries its signature byte-for-byte as the pipeline stored it.
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

export function isUpdatesPath(pathname: string): boolean {
  return pathname === "/updates" || pathname.startsWith(UPDATES_PATH_PREFIX);
}

// Makes confinement to OTA_KEY_PREFIX structural, not just a property of the
// handlers' key shapes.
export function prefixedBucket(bucket: BucketLike, prefix: string): BucketLike {
  return {
    get(key: string) {
      return bucket.get(prefix + key);
    },
  };
}

export async function handleUpdates(request: Request, bindings: UpdatesBindings): Promise<Response> {
  if (!bindings.UPDATES || !bindings.OTA_CHANNEL) {
    return textResponse(404, "updates are not served by this deployment");
  }
  return handle(request, {
    UPDATES: prefixedBucket(bindings.UPDATES, OTA_KEY_PREFIX),
    CHANNEL: bindings.OTA_CHANNEL,
  });
}
