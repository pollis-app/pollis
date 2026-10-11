// Pure pieces of the OTA publish pipeline (#1250): manifest building, code
// signing and verification, the bundle/env safety checks, multipart parsing,
// and the transparency-log records. No I/O beyond what callers pass in, and
// ONLY `node:` built-ins — the signing job runs this file from a bare checkout
// with no node_modules, so it cannot be handed a poisoned dependency tree.
//
// The CLI that drives it is mobile/scripts/ota-publish.ts; tests are
// mobile/tests/ota-*.test.ts.

import { createHash, sign, verify, X509Certificate, createPrivateKey } from "node:crypto";

export const PROD_DS = "https://api.pollis.com";
export const PROD_DS_HOST = "api.pollis.com";
export const DEV_DS_HOST = "api-dev.pollis.com";
// The update server is the DS front-door Worker (pollis-delivery/worker/updates.ts),
// answering under /updates/ on the prod DS host without touching the container.
export const UPDATES_BASE_URL = `${PROD_DS}/updates`;
export const MANIFEST_URL = `${UPDATES_BASE_URL}/api/manifest`;
// The update store is the `ota/` prefix of the public release bucket (`pollis`,
// the one behind cdn.pollis.com). Every update-store key this pipeline reads or
// writes is relative to it, exactly as the Worker reads them
// (pollis-delivery/worker/updates.ts OTA_KEY_PREFIX; mobile/tests/ota-worker.test.ts
// pins that the two agree).
export const OTA_BUCKET = "pollis";
export const OTA_KEY_PREFIX = "ota/";
export const CHANNEL = "production";
export const KEY_ID = "main";
export const ALG = "rsa-v1_5-sha256";
// id-kp-codeSigning: expo-updates refuses a certificate without it.
export const CODE_SIGNING_EKU = "1.3.6.1.5.5.7.3.3";

export const PLATFORMS = ["ios", "android"] as const;
export type Platform = (typeof PLATFORMS)[number];

// The EXPO_PUBLIC_* names the app reads, all public endpoints. Anything else
// with that prefix is refused outright: Expo inlines every referenced
// EXPO_PUBLIC_* value into the bundle, and an OTA skips store review.
export const PUBLIC_EXPO_VARS = [
  "EXPO_PUBLIC_POLLIS_DELIVERY_URL",
  "EXPO_PUBLIC_LIVEKIT_URL",
  "EXPO_PUBLIC_R2_ENDPOINT",
  "EXPO_PUBLIC_R2_PUBLIC_URL",
] as const;

// Read by the app in DEV builds only (#1242: the dev DS gate key). The read is
// allowed, but an OTA must never be built with one set: assertPublicEnv refuses
// any of these in the environment, so the bundle inlines `undefined`.
export const DEV_ONLY_EXPO_VARS = ["EXPO_PUBLIC_POLLIS_DEV_ACCESS_KEY"] as const;

export function sha256Hex(bytes: Buffer | string): string {
  return createHash("sha256").update(bytes).digest("hex");
}

export function sha256Base64Url(bytes: Buffer | string): string {
  return createHash("sha256").update(bytes).digest("base64url");
}

export function md5Hex(bytes: Buffer | string): string {
  return createHash("md5").update(bytes).digest("hex");
}

export function base64UrlToHex(b64url: string): string {
  return Buffer.from(b64url, "base64url").toString("hex");
}

// ---------------------------------------------------------------------------
// Same checks as a store build
// ---------------------------------------------------------------------------

export function assertPublicEnv(env: Record<string, string | undefined>): void {
  const problems: string[] = [];
  for (const [name, value] of Object.entries(env)) {
    if (!name.startsWith("EXPO_PUBLIC_") || value === undefined) {
      continue;
    }
    if ((DEV_ONLY_EXPO_VARS as readonly string[]).includes(name)) {
      if (value !== "") {
        problems.push(`${name} is dev-only and must not be set for an OTA; unset it`);
      }
      continue;
    }
    if (!(PUBLIC_EXPO_VARS as readonly string[]).includes(name)) {
      problems.push(`${name} is not a public variable the app reads; unset it`);
      continue;
    }
    if (value.includes(DEV_DS_HOST)) {
      problems.push(`${name} names ${DEV_DS_HOST}`);
    }
  }
  if (env.EXPO_PUBLIC_POLLIS_DELIVERY_URL !== PROD_DS) {
    problems.push(`EXPO_PUBLIC_POLLIS_DELIVERY_URL must be exactly ${PROD_DS}`);
  }
  if (problems.length > 0) {
    throw new Error(`refusing to build an OTA update:\n  - ${problems.join("\n  - ")}`);
  }
}

// Every `process.env.EXPO_PUBLIC_X` read in the app's sources. A read is what
// inlines a value (an unread .env entry is inert), so this is the check that
// would have caught #995.
export function findEnvReads(files: { path: string; text: string }[]): { name: string; path: string }[] {
  const out: { name: string; path: string }[] = [];
  const re = /process\.env(?:\.|\[\s*["'])(EXPO_PUBLIC_[A-Z0-9_]+)/g;
  for (const file of files) {
    for (const m of file.text.matchAll(re)) {
      out.push({ name: m[1], path: file.path });
    }
  }
  return out;
}

export function assertOnlyPublicEnvReads(files: { path: string; text: string }[]): void {
  const allowed: readonly string[] = [...PUBLIC_EXPO_VARS, ...DEV_ONLY_EXPO_VARS];
  const bad = findEnvReads(files).filter((r) => !allowed.includes(r.name));
  if (bad.length > 0) {
    throw new Error(
      `the app reads EXPO_PUBLIC_* names that are not on the public allowlist:\n  - ${bad
        .map((b) => `${b.name} (${b.path})`)
        .join("\n  - ")}`,
    );
  }
}

// The bundle is Hermes bytecode, where `strings` can miss a literal, so the
// raw bytes are searched — exactly what mobile-apk-release.yml does.
export function assertBundleTargetsProd(bundle: Buffer, label: string): void {
  if (!bundle.includes(Buffer.from(PROD_DS_HOST))) {
    throw new Error(`${label}: bundle does not name ${PROD_DS_HOST}`);
  }
  if (bundle.includes(Buffer.from(DEV_DS_HOST))) {
    throw new Error(`${label}: bundle names ${DEV_DS_HOST}`);
  }
}

// Belt and braces: no secret the publishing process can see may appear in the
// bundle verbatim. Names that look like credentials, values long enough not to
// match by accident.
export function assertNoSecretValues(bundle: Buffer, env: Record<string, string | undefined>, label: string): void {
  for (const [name, value] of Object.entries(env)) {
    if (value === undefined || value.length < 16) {
      continue;
    }
    if (!/(SECRET|TOKEN|PASSWORD|PRIVATE|_KEY$|_KEY_)/.test(name)) {
      continue;
    }
    if (bundle.includes(Buffer.from(value))) {
      throw new Error(`${label}: bundle contains the value of ${name}`);
    }
  }
}

// ---------------------------------------------------------------------------
// Manifests and directives (Expo Updates protocol v1)
// ---------------------------------------------------------------------------

const CONTENT_TYPES: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  svg: "image/svg+xml",
  ttf: "font/ttf",
  otf: "font/otf",
  woff: "font/woff",
  woff2: "font/woff2",
  json: "application/json",
  xml: "application/xml",
  mp4: "video/mp4",
  wav: "audio/wav",
  mp3: "audio/mpeg",
};

export function contentTypeFor(ext: string): string {
  return CONTENT_TYPES[ext.toLowerCase()] ?? "application/octet-stream";
}

export interface AssetRef {
  // Lowercase hex sha256 — the R2 key and the URL path, so a URL names its bytes.
  sha256: string;
  // base64url sha256 — what expo-updates checks every download against.
  hash: string;
  // md5 hex — the asset key `expo export` and the embedded manifest use, so a
  // client can reuse an asset it already has instead of downloading it.
  key: string;
  contentType: string;
  fileExtension: string;
  url: string;
}

export function assetRef(bytes: Buffer, ext: string, baseUrl: string = UPDATES_BASE_URL, isLaunch = false): AssetRef {
  const sha256 = sha256Hex(bytes);
  return {
    sha256,
    hash: sha256Base64Url(bytes),
    key: md5Hex(bytes),
    contentType: isLaunch ? "application/javascript" : contentTypeFor(ext),
    fileExtension: isLaunch ? ".bundle" : `.${ext}`,
    url: `${baseUrl}/assets/${sha256}`,
  };
}

export interface ManifestInput {
  id: string;
  createdAt: string;
  runtimeVersion: string;
  launch: AssetRef;
  assets: AssetRef[];
  expoClient: Record<string, unknown>;
}

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const RUNTIME_VERSION_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;

function wireAsset(a: AssetRef): Record<string, string> {
  return { hash: a.hash, key: a.key, contentType: a.contentType, fileExtension: a.fileExtension, url: a.url };
}

// The manifest body, as the exact string that gets signed and served. Built
// once; nothing downstream re-serializes it.
export function buildManifest(input: ManifestInput): string {
  if (!UUID_RE.test(input.id)) {
    throw new Error(`update id must be a lowercase UUID, got ${input.id}`);
  }
  if (!RUNTIME_VERSION_RE.test(input.runtimeVersion)) {
    throw new Error(`invalid runtime version ${input.runtimeVersion}`);
  }
  if (Number.isNaN(Date.parse(input.createdAt))) {
    throw new Error(`invalid createdAt ${input.createdAt}`);
  }
  return JSON.stringify({
    id: input.id,
    createdAt: input.createdAt,
    runtimeVersion: input.runtimeVersion,
    launchAsset: wireAsset(input.launch),
    assets: input.assets.map(wireAsset),
    metadata: {},
    // Constants.expoConfig in an updated app is read from here — the push
    // registration reads extra.eas.projectId through it — so it is the full
    // public app config, as `expo export` would embed it.
    extra: { expoClient: input.expoClient },
  });
}

export type DirectiveType = "rollBackToEmbedded" | "noUpdateAvailable";

export function buildDirective(type: DirectiveType, commitTime: string): string {
  if (type === "noUpdateAvailable") {
    return JSON.stringify({ type });
  }
  if (Number.isNaN(Date.parse(commitTime))) {
    throw new Error(`invalid commitTime ${commitTime}`);
  }
  // The client rolls back only to a directive newer than what it is running,
  // so commitTime is the moment of the rollback.
  return JSON.stringify({ type, parameters: { commitTime } });
}

export interface ParsedManifest {
  id: string;
  createdAt: string;
  runtimeVersion: string;
  launchAsset: { hash: string; key: string; contentType: string; fileExtension: string; url: string };
  assets: { hash: string; key: string; contentType: string; fileExtension: string; url: string }[];
  metadata: Record<string, unknown>;
  extra: { expoClient?: Record<string, unknown> };
}

// Everything the signer re-checks before it signs: a manifest that does not
// pass this is never signed, whatever the build job claims.
export function assertManifestShape(body: string, expect: { runtimeVersion: string; baseUrl?: string }): ParsedManifest {
  const m = JSON.parse(body) as ParsedManifest;
  const base = expect.baseUrl ?? UPDATES_BASE_URL;
  if (!UUID_RE.test(m.id)) {
    throw new Error("manifest id is not a UUID");
  }
  if (m.runtimeVersion !== expect.runtimeVersion) {
    throw new Error(`manifest runtimeVersion ${m.runtimeVersion} != ${expect.runtimeVersion}`);
  }
  for (const a of [m.launchAsset, ...m.assets]) {
    const hex = base64UrlToHex(a.hash);
    if (a.url !== `${base}/assets/${hex}`) {
      throw new Error(`asset url ${a.url} is not the content address of its hash`);
    }
  }
  if (m.launchAsset.contentType !== "application/javascript") {
    throw new Error("launch asset is not javascript");
  }
  // The public config (what an updated app reads back as Constants.expoConfig)
  // must be the prod one. Expo strips the code-signing fields from a public
  // config, so those are checked on the app config itself (tests/ota-config).
  const updates = (m.extra?.expoClient?.updates ?? {}) as Record<string, unknown>;
  const headers = (updates.requestHeaders ?? {}) as Record<string, unknown>;
  if (updates.enabled !== true || updates.url !== MANIFEST_URL || headers["expo-channel-name"] !== CHANNEL) {
    throw new Error("expoClient.updates is not the production configuration");
  }
  return m;
}

// ---------------------------------------------------------------------------
// Code signing (RSASSA-PKCS1-v1_5 / SHA-256 over the exact body bytes)
// ---------------------------------------------------------------------------

export function signatureHeader(signatureBase64: string, keyid: string = KEY_ID): string {
  return `sig="${signatureBase64}", keyid="${keyid}", alg="${ALG}"`;
}

export function signBody(body: string, privateKeyPem: string): string {
  const key = createPrivateKey(privateKeyPem);
  if (key.asymmetricKeyType !== "rsa") {
    throw new Error(`code-signing key must be RSA, got ${key.asymmetricKeyType}`);
  }
  const sig = sign("sha256", Buffer.from(body, "utf8"), key);
  return signatureHeader(sig.toString("base64"));
}

// The sf-dictionary of string items the app parses (expo-structured-headers).
export function parseSignatureHeader(value: string): { sig: string; keyid: string; alg: string } {
  const out: Record<string, string> = {};
  const re = /\s*([a-z][a-z0-9_*-]*)="((?:[^"\\]|\\.)*)"\s*(?:,|$)/gy;
  let m: RegExpExecArray | null;
  let consumed = 0;
  while ((m = re.exec(value)) !== null) {
    out[m[1]] = m[2];
    consumed = re.lastIndex;
    if (consumed >= value.length) {
      break;
    }
  }
  if (consumed !== value.length || out.sig === undefined) {
    throw new Error(`malformed expo-signature: ${value}`);
  }
  return { sig: out.sig, keyid: out.keyid ?? "root", alg: out.alg ?? ALG };
}

export function assertCodeSigningCertificate(certPem: string, now: Date = new Date()): X509Certificate {
  const cert = new X509Certificate(certPem);
  if (cert.publicKey.asymmetricKeyType !== "rsa") {
    throw new Error("code-signing certificate is not RSA");
  }
  // Node names the extended key usages `keyUsage` (newer releases also
  // `extKeyUsage`); either way it is the list of EKU OIDs.
  const eku = (cert as unknown as { extKeyUsage?: string[] }).extKeyUsage ?? cert.keyUsage ?? [];
  if (!eku.includes(CODE_SIGNING_EKU)) {
    throw new Error("code-signing certificate lacks the codeSigning extended key usage");
  }
  if (now < new Date(cert.validFrom) || now > new Date(cert.validTo)) {
    throw new Error(`code-signing certificate is not valid now (${cert.validFrom} – ${cert.validTo})`);
  }
  return cert;
}

// Verify exactly as the app does: the certificate compiled into the binary,
// key id "main", RSA-SHA256 over the body bytes as served.
export function verifyBody(body: string | Buffer, header: string, certPem: string, now: Date = new Date()): void {
  const cert = assertCodeSigningCertificate(certPem, now);
  const parsed = parseSignatureHeader(header);
  if (parsed.keyid !== KEY_ID) {
    throw new Error(`signature keyid ${parsed.keyid} != ${KEY_ID}`);
  }
  if (parsed.alg !== ALG) {
    throw new Error(`signature alg ${parsed.alg} != ${ALG}`);
  }
  const bytes = typeof body === "string" ? Buffer.from(body, "utf8") : body;
  if (!verify("sha256", bytes, cert.publicKey, Buffer.from(parsed.sig, "base64"))) {
    throw new Error("signature does not verify against the code-signing certificate");
  }
}

// The key handed to the signer must be the one the app trusts; a wrong key
// would produce updates every phone silently refuses.
export function assertKeyMatchesCertificate(privateKeyPem: string, certPem: string): void {
  const probe = "pollis-ota-key-check";
  verifyBody(probe, signBody(probe, privateKeyPem), certPem);
}

// ---------------------------------------------------------------------------
// multipart/mixed (what the Worker returns)
// ---------------------------------------------------------------------------

export interface Part {
  headers: Record<string, string>;
  body: Buffer;
}

export function parseMultipart(contentType: string, body: Buffer): Part[] {
  const m = /boundary=("?)([^";]+)\1/i.exec(contentType);
  if (!m || !contentType.toLowerCase().startsWith("multipart/mixed")) {
    throw new Error(`not multipart/mixed: ${contentType}`);
  }
  const delimiter = Buffer.from(`--${m[2]}`);
  const parts: Part[] = [];
  let pos = body.indexOf(delimiter);
  if (pos < 0) {
    throw new Error("no multipart boundary in body");
  }
  for (;;) {
    pos += delimiter.length;
    if (body.subarray(pos, pos + 2).toString() === "--") {
      break;
    }
    // Skip the CRLF after the delimiter line.
    pos += 2;
    const next = body.indexOf(delimiter, pos);
    if (next < 0) {
      throw new Error("unterminated multipart body");
    }
    // The part ends with the CRLF that precedes the next delimiter.
    const raw = body.subarray(pos, next - 2);
    const split = raw.indexOf("\r\n\r\n");
    if (split < 0) {
      throw new Error("multipart part without header block");
    }
    const headers: Record<string, string> = {};
    for (const line of raw.subarray(0, split).toString("utf8").split("\r\n")) {
      const colon = line.indexOf(":");
      if (colon > 0) {
        headers[line.slice(0, colon).trim().toLowerCase()] = line.slice(colon + 1).trim();
      }
    }
    parts.push({ headers, body: raw.subarray(split + 4) });
    pos = next;
  }
  return parts;
}

export function partName(part: Part): string | null {
  const m = /name="([^"]+)"/.exec(part.headers["content-disposition"] ?? "");
  return m ? m[1] : null;
}

// ---------------------------------------------------------------------------
// The publish plan — what the build job hands the signer
// ---------------------------------------------------------------------------

export type EntryKind = "manifest" | "directive";

export interface PlanEntry {
  platform: Platform;
  runtimeVersion: string;
  kind: EntryKind;
  // The exact bytes to sign and serve.
  body: string;
  // Filled by the signer.
  signature?: string;
  // Manifest entries only.
  updateId?: string;
  launchSha256?: string;
  assets?: { sha256: string; key: string; fileExtension: string; contentType: string }[];
}

export type PlanAction = "publish" | "republish" | "rollback";

export interface Plan {
  version: 1;
  action: PlanAction;
  groupId: string;
  createdAt: string;
  channel: string;
  baseUrl: string;
  // The source revision whose bytes these are (for a republish: the original's).
  commit: string;
  sourceDateEpoch: number;
  toolchain: { rustc: string; node: string; pnpm: string; runnerImage: string };
  // Republish only: the group being re-served.
  fromGroupId?: string;
  entries: PlanEntry[];
  // Every content-addressed file the entries reference (launch bundles and
  // assets), keyed by sha256 hex; the bytes travel in <plan dir>/files/<sha256>.
  files: Record<string, { contentType: string; size: number }>;
}

export function releaseTag(plan: Pick<Plan, "groupId">): string {
  return `mobile-ota-${plan.groupId}`;
}

export function pointerKey(channel: string, platform: Platform, runtimeVersion: string): string {
  return `current/${channel}/${platform}/${runtimeVersion}.json`;
}

export function pointerFor(entry: PlanEntry): { kind: EntryKind; body: string; signature: string } {
  if (entry.signature === undefined) {
    throw new Error(`${entry.platform}/${entry.runtimeVersion} is not signed`);
  }
  return { kind: entry.kind, body: entry.body, signature: entry.signature };
}

// The canonical asset list of one update: what the `ota-assets` leaf hashes.
export function assetListFile(entry: PlanEntry): string {
  const lines = (entry.assets ?? [])
    .map((a) => `${a.sha256}  ${a.key}${a.fileExtension}`)
    .sort();
  return lines.join("\n") + (lines.length > 0 ? "\n" : "");
}

// ---------------------------------------------------------------------------
// Binary transparency leaves (verifiable-log-builder BinaryRecord, unchanged)
// ---------------------------------------------------------------------------

// Field order is the frozen leaf encoding (verifiable-log-builder/src/binaries.rs);
// the builder re-serialises from its struct, but keeping the same order here
// makes the accumulator diffable against what the tree commits to.
export interface BinaryRecord {
  release_tag: string;
  commit: string;
  platform: string;
  arch: string;
  bundle: string;
  artifact_name: string;
  layer: "payload";
  payload_sha256: string;
  artifact_sha256: string;
  toolchain: { rustc: string; node: string; pnpm: string; runner_image: string; source_date_epoch: number };
  provenance_uri: string;
}

export interface AttestedFile {
  // The file name under which the bytes are attested and their provenance published.
  artifactName: string;
  bytes: Buffer;
  record: BinaryRecord;
}

export const PROVENANCE_HOST_PREFIX = "cdn.pollis.com/releases/mobile-ota";

// One update becomes three leaves (manifest, launch bundle, asset list); one
// directive becomes one. `arch` carries the runtime version: the native binary
// an update targets is its "architecture", and it keeps the fork key
// (tag, platform, arch, bundle, layer) unique when a rollback names several
// runtime versions of one platform.
export function attestedFiles(plan: Plan, launchBytes: (entry: PlanEntry) => Buffer): AttestedFile[] {
  const tag = releaseTag(plan);
  const files: AttestedFile[] = [];
  const toolchain = {
    rustc: plan.toolchain.rustc,
    node: plan.toolchain.node,
    pnpm: plan.toolchain.pnpm,
    runner_image: plan.toolchain.runnerImage,
    source_date_epoch: plan.sourceDateEpoch,
  };
  const add = (entry: PlanEntry, bundle: string, artifactName: string, bytes: Buffer): void => {
    const digest = sha256Hex(bytes);
    files.push({
      artifactName,
      bytes,
      record: {
        release_tag: tag,
        commit: plan.commit,
        platform: entry.platform,
        arch: entry.runtimeVersion,
        bundle,
        artifact_name: artifactName,
        layer: "payload",
        payload_sha256: digest,
        artifact_sha256: digest,
        toolchain,
        provenance_uri: `${PROVENANCE_HOST_PREFIX}/${tag}/${artifactName}.intoto.jsonl`,
      },
    });
  };
  for (const entry of plan.entries) {
    if (entry.kind === "manifest") {
      const id = entry.updateId as string;
      const stem = `pollis-ota-${id}-${entry.platform}`;
      add(entry, "ota-manifest", `${stem}.manifest.json`, Buffer.from(entry.body, "utf8"));
      add(entry, "ota-bundle", `${stem}.bundle.hbc`, launchBytes(entry));
      add(entry, "ota-assets", `${stem}.assets.sha256`, Buffer.from(assetListFile(entry), "utf8"));
    } else {
      const stem = `pollis-ota-${plan.groupId}-${entry.platform}-${entry.runtimeVersion}`;
      add(entry, "ota-directive", `${stem}.directive.json`, Buffer.from(entry.body, "utf8"));
    }
  }
  return files;
}

// Merge this group's leaves into the accumulator transparency-publish.yml reads.
// Idempotent: a group already present is left alone (re-appending would double
// its leaves), the same rule desktop-release.yml's attest-and-log applies.
export function mergeRecords(accumulator: BinaryRecord[], records: BinaryRecord[]): BinaryRecord[] | null {
  const tags = new Set(records.map((r) => r.release_tag));
  if (tags.size !== 1) {
    throw new Error("records must belong to exactly one release_tag");
  }
  const [tag] = [...tags];
  if (accumulator.some((r) => r.release_tag === tag)) {
    return null;
  }
  return [...accumulator, ...records];
}
