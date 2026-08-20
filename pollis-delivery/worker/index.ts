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
  type ContainerStartConfigOptions,
} from "@cloudflare/containers";

// WHERE the DS container physically runs (#658).
//
// A Durable Object addressed by name is placed in the region nearest whoever
// FIRST instantiated it, and that placement is PERMANENT for the life of that
// object ID. `getContainer(binding, name)` — which this used to call — is exactly
// `binding.idFromName(name)` + `binding.get(id)` with no placement argument
// (@cloudflare/containers 0.3.7, dist/lib/utils.js), so placement was never
// configured: it was decided by whichever request happened to arrive first.
//
// `enam` (eastern North America) matches the `aws-us-east-1` Turso primary that
// BOTH environments use. It is declared so placement is at least a reviewable
// property rather than silently inherited from whoever sent the first request.
//
// DO NOT TREAT THIS AS A PERFORMANCE LEVER — it was tested and it is not one.
//
// Dev is much slower than prod on identical code and images, measured from the
// SAME edge (IAD, confirmed via cf-ray): `/version` ~280ms vs ~47ms with no
// database involved at all, and ~1.4s vs ~65ms on a signed request. The dev Turso
// database answers in ~20ms queried directly, so the database is not the cause.
// The hypothesis was that dev's container sits far from it, and #658 tested that
// directly by creating a FRESH object under this hint (via the staged
// max_instances migration in docs/deployments.md).
//
// RESULT: no change whatsoever — ~280ms and ~1.4s after re-placement. A
// `locationHint` evidently does not govern where a CONTAINER-backed durable
// object's container is scheduled, whatever it does for the object itself. The
// dev/prod gap is still unexplained; that is deferred to the hosting-strategy
// spike on #658. Keep the hint (it is free and correct as a declaration), but do
// not reach for it expecting latency to move.
const DS_LOCATION_HINT: DurableObjectLocationHint = "enam";

// `locationHint` is honoured ONLY when the object is first created — an existing
// object never migrates. So the hint below does NOT move either environment's
// current container; it decides where the object lands the next time one is
// created from scratch (new account, new namespace, a `migrations` class change,
// or a deliberate rename per the procedure below). That is the point: it stops
// prod's placement from being re-rolled by chance.
//
// The object name comes from the per-environment `DS_SINGLETON_NAME` var so an
// environment can be re-placed on its own. This is the fallback for a config that
// sets no var — prod's long-standing object.
//
// CHANGING AN ENVIRONMENT'S NAME RE-PLACES ITS CONTAINER, and must follow the
// staged procedure in docs/deployments.md. A bare rename was tried on 2026-08-06
// and took dev down:
//
//   Failed to start container: Maximum number of running container instances
//   exceeded. Try again later, or try configuring a higher value for max_instances
//
// A rename creates a SECOND durable object while `max_instances: 1` permits only
// one container instance. The outgoing object still held it, so the incoming one
// could never start and every request 500'd until traffic was routed back. The DO
// is stateless (no `ctx.storage` in this file; all DS state is in Turso) — the
// blocker is the instance cap, not data. Hence: raise the cap, switch the name,
// let the orphan idle past `sleepAfter` and release, then lower the cap again.
const DS_SINGLETON_NAME_FALLBACK = "pollis-delivery-singleton";

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
  "DEV_OTP",
  // #720: gates GET /v1/retention/metrics. Absent here means the DS never sees
  // it, the route stays default-closed, and the endpoint 404s however correctly
  // the secret is set in Doppler and bound in wrangler.
  "POLLIS_DS_METRICS_TOKEN",
  // #707: authenticates the content-free push fan-out to Expo. Absent
  // here means the DS sends unauthenticated, which Expo rejects outright
  // once Enhanced Security for Push Notifications is enabled.
  "EXPO_TOKEN",
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
  // Which durable object hosts this environment's container — PER ENVIRONMENT,
  // and deliberately so. The name determines object identity, and identity
  // determines placement, so a name shared across environments means dev cannot
  // be re-placed without also re-placing prod on its next deploy. That coupling
  // is exactly how a routine dev migration would become a production outage.
  // Absent → DS_SINGLETON_NAME_FALLBACK, which is prod's existing object.
  DS_SINGLETON_NAME?: string;
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
    // Same resolution getContainer performed (idFromName + get), plus the
    // placement hint it gives no way to pass. See DS_LOCATION_HINT above.
    const id = env.POLLIS_DELIVERY.idFromName(
      env.DS_SINGLETON_NAME ?? DS_SINGLETON_NAME_FALLBACK,
    );
    return env.POLLIS_DELIVERY.get(id, {
      locationHint: DS_LOCATION_HINT,
    }).fetch(request);
  },
};
