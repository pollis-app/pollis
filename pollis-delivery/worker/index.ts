// Cloudflare Worker front-door for the Pollis Delivery Service.
//
// The DS is a stateless axum binary (pollis-delivery/Dockerfile) — all state
// lives in Turso/R2. Here it runs as a single-instance Cloudflare Container
// fronted by a Durable Object: the Worker forwards every HTTP request to the
// container on :8788, and the DO gives us exactly one serialized instance
// (Pollis's single-writer-to-Turso invariant, #419/#420).
//
// Secrets: the container reads its config as OS env vars (TURSO_URL, LIVEKIT_*,
// R2_*, …). Those come from Wrangler Secrets Store bindings, which are async
// (`.get()`), so they cannot live in the static `envVars`. Instead we override
// `startAndWaitForPorts` to resolve them at boot and inject them as per-instance
// env vars before the container serves any traffic. Doppler -> Secrets Store is
// the single source of truth (see the deploy workflows).
import {
  Container,
  getContainer,
  type ContainerStartConfigOptions,
} from "@cloudflare/containers";

// Keys synced from Doppler into this env's Secrets Store, each bound under the
// same name in wrangler config. Read at container start and passed through as
// OS env vars. Missing/optional keys are skipped so an absent dev-only secret
// (e.g. DEV_OTP in prod) never bricks startup.
const SECRET_KEYS = [
  "TURSO_URL",
  "TURSO_TOKEN",
  "LOG_DB_URL",
  "LOG_DB_ADMIN_TOKEN",
  "RESEND_API_KEY",
  "LIVEKIT_API_KEY",
  "LIVEKIT_API_SECRET",
  "LIVEKIT_URL",
  "R2_S3_ENDPOINT",
  "R2_ACCESS_KEY_ID",
  "R2_SECRET_KEY",
  "R2_BUCKET",
  "TURSO_PLATFORM_TOKEN",
  "TURSO_ORG",
  "TURSO_DB",
  "DEV_OTP",
  // #720: gates GET /v1/retention/metrics. Absent here means the DS never sees
  // it, the route stays default-closed, and the endpoint 404s however correctly
  // the secret is set in Doppler and bound in wrangler.
  "POLLIS_DS_METRICS_TOKEN",
] as const;

interface SecretStoreBinding {
  get(): Promise<string>;
}

type Env = {
  POLLIS_DELIVERY: DurableObjectNamespace<PollisDelivery>;
  // Static (non-secret) container config, set as wrangler `vars`.
  PORT: string;
  POLLIS_DS_REQUIRE_AUTH: string;
  POLLIS_DS_WATERMARK_STALE_MONTHS?: string;
} & Record<(typeof SECRET_KEYS)[number], SecretStoreBinding | undefined>;

// Derived from the base method so we don't depend on the (unexported)
// CancellationOptions / StartAndWaitForPortsOptions types.
type StartArgs = Parameters<Container<Env>["startAndWaitForPorts"]>;

export class PollisDelivery extends Container<Env> {
  // The axum DS listens here (Dockerfile EXPOSE 8788 / PORT default 8788).
  defaultPort = 8788;
  // Startup readiness gate — the DS serves /health.
  pingEndpoint = "/health";
  // Scale-to-zero pre-launch: the DO wakes the container on the next request,
  // so single-instance serialization is unaffected — only a cold boot cost.
  //
  // "10m" is EXACTLY the @cloudflare/containers default (DEFAULT_SLEEP_AFTER
  // in dist/lib/container.js as of the pinned 0.3.7). Consequences:
  //   - Deleting this line is a NO-OP — the base class re-applies the same 10m.
  //   - There is no "never sleep" value: parseTimeExpression accepts only
  //     `<n>[smh]` or a bare seconds count, so always-on is not expressible via
  //     sleepAfter at any value. The only lever is a large FINITE duration
  //     (e.g. "24h"), which just widens the warm window at a running cost.
  // So going always-on is a COST decision, not a code fix (#515). We keep the
  // value EXPLICIT — matching the default — to document intent and to survive a
  // library default change. Re-measure the cold start before paying: procedure
  // in docs/deployments.md, DS section. (#695)
  sleepAfter = "10m";
  // The DS reaches out to Turso, Resend, LiveKit and R2 — needs egress.
  enableInternet = true;

  // Non-secret config baked at deploy time (from wrangler `vars`). Secret env
  // vars are injected in startAndWaitForPorts below (they need async .get()).
  envVars = {
    PORT: this.env.PORT ?? "8788",
    POLLIS_DS_REQUIRE_AUTH: this.env.POLLIS_DS_REQUIRE_AUTH ?? "true",
    // #720: the device-staleness window. Forwarded explicitly — a wrangler `var`
    // is bound to the WORKER, not to the container, so without this line the DS
    // silently falls back to its 6-month code default no matter what the config
    // says. Omitted from the map when unset so that fallback stays intact.
    ...(this.env.POLLIS_DS_WATERMARK_STALE_MONTHS
      ? { POLLIS_DS_WATERMARK_STALE_MONTHS: this.env.POLLIS_DS_WATERMARK_STALE_MONTHS }
      : {}),
  };

  // Resolve every Secrets Store binding into a plain env map. Optional/unset
  // secrets (a dev-only key in prod, or an absent optional) are skipped so a
  // missing binding never bricks boot.
  private async resolveSecretEnv(): Promise<Record<string, string>> {
    const out: Record<string, string> = {};
    for (const key of SECRET_KEYS) {
      const binding = this.env[key];
      if (!binding) {
        continue;
      }
      try {
        const value = await binding.get();
        if (value) {
          out[key] = value;
        }
      } catch {
        // Optional/unset secret for this env — skip.
      }
    }
    return out;
  }

  // Inject the resolved secrets as per-instance env vars before the container
  // accepts traffic. The default fetch path reaches here via containerFetch,
  // which calls the POSITIONAL form `startAndWaitForPorts(port, {abort})`, so we
  // must parse all overload shapes (mirroring the base) and preserve ports +
  // cancellation. Runs on every (re)start, including the scale-to-zero wake.
  //
  // NB: per-call startOptions.envVars REPLACES the class `envVars` in the base
  // (it does not merge), so we re-merge `this.envVars` (PORT etc.) ourselves.
  override async startAndWaitForPorts(
    portsOrArgs?: StartArgs[0],
    cancellationOptions?: StartArgs[1],
    startOptions?: StartArgs[2],
  ): Promise<void> {
    let ports: number | number[] | undefined;
    let resolvedCancellation: StartArgs[1];
    let resolvedStart: ContainerStartConfigOptions | undefined;
    if (
      typeof portsOrArgs === "object" &&
      portsOrArgs !== null &&
      !Array.isArray(portsOrArgs)
    ) {
      ports = portsOrArgs.ports;
      resolvedCancellation = portsOrArgs.cancellationOptions;
      resolvedStart = portsOrArgs.startOptions;
    } else {
      ports = portsOrArgs;
      resolvedCancellation = cancellationOptions;
      resolvedStart = startOptions;
    }

    const secretEnv = await this.resolveSecretEnv();
    // Positional (overload 2) form — avoids the object-form typing friction and
    // preserves ports + cancellation from whichever shape the caller used.
    await super.startAndWaitForPorts(ports, resolvedCancellation, {
      ...resolvedStart,
      envVars: {
        ...this.envVars,
        ...secretEnv,
        ...resolvedStart?.envVars,
      },
    });
  }
}

export default {
  // Forward everything to the single serialized container instance. No
  // per-route allowlist (the nginx-vhost rot this migration kills, #515) —
  // the app owns its routing.
  async fetch(request: Request, env: Env): Promise<Response> {
    return getContainer(env.POLLIS_DELIVERY, "pollis-delivery-singleton").fetch(
      request,
    );
  },
};
