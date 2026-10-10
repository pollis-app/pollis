// The dev Delivery Service gate (#1242): only our own clients reach api-dev.
//
// api-dev.pollis.com is public, and dev is deliberately soft: `DEV_OTP` signs
// any mailbox in with one fixed code, request-otp is unlimited, and it brokers
// dev R2 and LiveKit tokens on the SAME SFU prod uses. Anyone holding only the
// hostname could have all of that. So every dev client (pollis-core, on desktop,
// CLI and mobile) sends `X-Pollis-Dev-Key`, and the dev Worker checks it before
// a request reaches the container. A request without a valid key gets a bare
// 404, so dev looks like nothing is there.
//
// INERT ON PROD, STRUCTURALLY. The gate runs only when `DEV_GATE_MODE` is set,
// and only wrangler.dev.jsonc sets it (and binds the key). Prod sets neither, so
// `gateModeFrom` returns undefined, nothing is checked, nothing is counted, and
// even `/__gate` is forwarded to the container like any other path.
// worker-tests/dev-gate.test.ts pins that prod config binds neither.
//
//   DEV_GATE_MODE=off      configured but checking nothing (rollback)
//   DEV_GATE_MODE=report   lets everything through, counts the verdicts
//   DEV_GATE_MODE=enforce  404s every request without a valid key
//
// An unrecognised value is treated as `report`: a typo while rolling back must
// not take dev offline, and `/__gate` reports the mode actually in force.
//
// NO CLIENT IP, ANYWHERE. Report mode counts `{verdict} × {path class} × {UA
// family}` and nothing else: no address, no rate-limit bucket, no full user
// agent, no timestamp per request. The counts are read on `GET /__gate` with the
// operator bearer (`POLLIS_DS_METRICS_TOKEN`), which the Worker already binds.
//
// The key is NOT a strong secret. It is inlined in every dev mobile build and
// sits in plain text in local env files, so it stops people who only have the
// URL (scanners, crawlers, a hostname seen in a screenshot or a CT log), not
// anyone holding a dev build. Rotation is the answer to a leak: the binding may
// hold a comma-separated list, so set "old,new", rebuild the clients, then
// "new".
//
// No imports, so worker-tests/ can load this file under plain Node.

export type GateMode = "off" | "report" | "enforce";
export type Verdict = "ok" | "missing" | "bad";

export const DEV_KEY_HEADER = "x-pollis-dev-key";
export const GATE_STATS_PATH = "/__gate";

// Answered without a key: the deploy workflow polls /version for the new SHA,
// and /health is the readiness probe. Neither does anything a stranger can use.
export const EXEMPT_PATHS: ReadonlySet<string> = new Set(["/health", "/version"]);

// Unset (prod) → undefined → the gate does not exist.
export function gateModeFrom(raw: string | undefined): GateMode | undefined {
  if (raw === undefined) {
    return undefined;
  }
  const mode = raw.trim().toLowerCase();
  if (mode === "off" || mode === "enforce") {
    return mode;
  }
  return "report";
}

// Every key currently accepted. More than one only during a rotation.
export function acceptedKeys(raw: string | undefined): string[] {
  return (raw ?? "")
    .split(",")
    .map((k) => k.trim())
    .filter((k) => k.length > 0);
}

async function digest(value: string): Promise<Uint8Array> {
  return new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value)));
}

// Constant-time over SHA-256 digests, so neither the length nor the content of
// the presented value leaks through timing. Written out rather than using the
// Workers-only `crypto.subtle.timingSafeEqual` so the tests run under Node.
export async function sameSecret(presented: string, expected: string): Promise<boolean> {
  const [a, b] = await Promise.all([digest(presented), digest(expected)]);
  let diff = 0;
  for (let i = 0; i < a.length; i++) {
    diff |= a[i] ^ b[i];
  }
  return diff === 0;
}

export async function verdictFor(presented: string | null, keys: string[]): Promise<Verdict> {
  if (presented === null || presented === "") {
    return "missing";
  }
  // Every key is compared, even after a match, so a rotation does not make the
  // check's timing depend on which key was sent.
  let ok = false;
  for (const key of keys) {
    ok = (await sameSecret(presented, key)) || ok;
  }
  return ok ? "ok" : "bad";
}

// Coarse, so a count says which part of dev a stranger was after without
// recording what they asked for.
export function pathClass(pathname: string): string {
  if (pathname.startsWith("/v1/auth/")) {
    return "auth";
  }
  if (pathname.startsWith("/v1/r2/")) {
    return "r2";
  }
  if (pathname.startsWith("/v1/livekit/")) {
    return "livekit";
  }
  if (
    pathname.startsWith("/v1/ops/") ||
    pathname === "/v1/config" ||
    pathname.startsWith("/v1/retention/")
  ) {
    return "ops";
  }
  if (pathname.startsWith("/v1/")) {
    return "api";
  }
  return "other";
}

// pollis-core's reqwest client sends no User-Agent at all, so our own traffic
// is `none`; a family is enough to tell it from browsers, curl and crawlers.
export function uaFamily(ua: string | null): string {
  if (!ua) {
    return "none";
  }
  const s = ua.toLowerCase();
  if (/bot|crawl|spider|scan/.test(s)) {
    return "bot";
  }
  if (s.startsWith("curl/")) {
    return "curl";
  }
  if (s.startsWith("mozilla/")) {
    return "browser";
  }
  if (s.includes("okhttp") || s.includes("cfnetwork") || s.includes("dalvik")) {
    return "mobile-http";
  }
  return "other";
}

// Where the counts live. The Durable Object passes its own storage so they
// survive the object idling out between a soak run and reading /__gate.
export interface CountStore {
  get<T>(key: string): Promise<T | undefined>;
  put<T>(key: string, value: T): Promise<void>;
}

const COUNTS_KEY = "dev-gate-counts";

interface StoredCounts {
  since: string;
  counts: Record<string, number>;
}

export class GateCounts {
  private loaded?: Promise<StoredCounts>;
  private readonly store: CountStore;

  // A plain assignment, not a parameter property: Node's type stripping (the
  // worker tests) does not accept those.
  constructor(store: CountStore) {
    this.store = store;
  }

  private load(): Promise<StoredCounts> {
    this.loaded ??= this.store
      .get<StoredCounts>(COUNTS_KEY)
      .then((c) => c ?? { since: new Date().toISOString(), counts: {} });
    return this.loaded;
  }

  async record(verdict: Verdict, pathname: string, ua: string | null): Promise<void> {
    const state = await this.load();
    const key = `${verdict} ${pathClass(pathname)} ${uaFamily(ua)}`;
    state.counts[key] = (state.counts[key] ?? 0) + 1;
    await this.store.put(COUNTS_KEY, state);
  }

  async snapshot(): Promise<StoredCounts> {
    const state = await this.load();
    return { since: state.since, counts: { ...state.counts } };
  }
}

export interface GateInputs {
  mode: GateMode;
  keys: string[];
  // The operator bearer for /__gate; undefined when it cannot be resolved.
  statsToken: string | undefined;
  counts: GateCounts;
}

const NOT_FOUND = () => new Response(null, { status: 404 });

// Returns the response to send instead of forwarding, or undefined to forward.
// The caller strips DEV_KEY_HEADER before forwarding; the container never needs
// it.
export async function gate(request: Request, inputs: GateInputs): Promise<Response | undefined> {
  const { pathname } = new URL(request.url);

  if (pathname === GATE_STATS_PATH) {
    const auth = request.headers.get("authorization") ?? "";
    const presented = auth.startsWith("Bearer ") ? auth.slice("Bearer ".length) : "";
    if (!inputs.statsToken || !presented || !(await sameSecret(presented, inputs.statsToken))) {
      return NOT_FOUND();
    }
    const snap = await inputs.counts.snapshot();
    return Response.json({
      mode: inputs.mode,
      key_configured: inputs.keys.length > 0,
      since: snap.since,
      counts: snap.counts,
    });
  }

  if (inputs.mode === "off" || EXEMPT_PATHS.has(pathname)) {
    return undefined;
  }

  const verdict = await verdictFor(request.headers.get(DEV_KEY_HEADER), inputs.keys);
  await inputs.counts.record(verdict, pathname, request.headers.get("user-agent"));
  if (inputs.mode === "enforce" && verdict !== "ok") {
    return NOT_FOUND();
  }
  return undefined;
}
