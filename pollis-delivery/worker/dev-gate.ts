// The dev Delivery Service gate (#1242): only our own clients, which send
// `X-Pollis-Dev-Key`, reach api-dev; anything else gets a bare 404. Inert on
// prod: only wrangler.dev.jsonc sets `DEV_GATE_MODE` (off | report | enforce; an
// unknown value is `report`, so a rollback typo cannot take dev offline).
// Counts hold no client IP. Modes, rotation and reading `/__gate`:
// docs/deployments.md → "Dev gate".
//
// No imports, so worker-tests/ can load this file under plain Node.

export type GateMode = "off" | "report" | "enforce";
export type Verdict = "ok" | "missing" | "bad";

export const DEV_KEY_HEADER = "x-pollis-dev-key";
export const GATE_STATS_PATH = "/__gate";

// The deploy polls /version; /health is the readiness probe.
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

// More than one only during a rotation.
export function acceptedKeys(raw: string | undefined): string[] {
  return (raw ?? "")
    .split(",")
    .map((k) => k.trim())
    .filter((k) => k.length > 0);
}

async function digest(value: string): Promise<Uint8Array> {
  return new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value)));
}

// Constant-time over SHA-256 digests, so neither length nor content leaks.
// Not `crypto.subtle.timingSafeEqual` (Workers-only), so the tests run under Node.
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

// Coarse on purpose: which part of dev, not what was asked for.
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

// pollis-core sends no User-Agent, so our own traffic is `none`.
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

// The Durable Object passes its own storage so counts survive an idle sleep.
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
// The caller strips DEV_KEY_HEADER before forwarding.
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
