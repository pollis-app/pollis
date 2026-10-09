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
  // `<email>:<6 digits>` — the one account App Store / Play review signs in
  // with; that address gets this fixed code and no email (otp.rs ReviewLogin).
  "APP_REVIEW_LOGIN",
] as const;

// Non-secret per-environment tunables, set (or not) as wrangler `vars` and
// forwarded to the container only when set — an unset key leaves the DS on its
// compiled-in default (ratelimit.rs `RateLimitConfig::from_env`). Every per-IP
// rate-limit tier is listed so any of them can be tuned per environment with a
// config change alone; today only dev sets the two OTP tiers (the mobile e2e
// suites sign up many accounts from one IP) and the ops log. Checked against
// ds-config-manifest.json `optional_container_vars` by
// scripts/check-ds-config-chain.py.
const TUNABLE_VAR_KEYS = [
  "RL_REQUEST_OTP_MAX",
  "RL_REQUEST_OTP_WINDOW_SECS",
  "RL_VERIFY_OTP_MAX",
  "RL_VERIFY_OTP_WINDOW_SECS",
  "RL_WRITE_MAX",
  "RL_WRITE_WINDOW_SECS",
  "RL_READ_MAX",
  "RL_READ_WINDOW_SECS",
  "RL_PROBE_MAX",
  "RL_PROBE_WINDOW_SECS",
  "RL_GET_MAX",
  "RL_GET_WINDOW_SECS",
  "RL_INVITE_REDEEM_MAX",
  "RL_INVITE_REDEEM_WINDOW_SECS",
  // Dev-only request-otp / rate-limit ops log (ops_log.rs). Only
  // wrangler.dev.jsonc sets it; unset (prod) means no recorder exists.
  "POLLIS_DS_OPS_LOG",
] as const;

// Every request header through which a client IP can reach the container. The
// Durable Object deletes ALL of them before forwarding (see `fetch` below), so
// the DS process never receives a plain client address — it receives only
// CLIENT_BUCKET_HEADER. Listed exhaustively, including headers Cloudflare adds
// only under optional zone settings (Pseudo IPv4, the "Add True-Client-IP"
// managed transform), so toggling one in the dashboard cannot leak an address.
// tests/no_client_ip_exposure.rs asserts this list is complete.
const CLIENT_IP_HEADERS = [
  "cf-connecting-ip",
  "cf-connecting-ipv6",
  "cf-pseudo-ipv4",
  "true-client-ip",
  "x-real-ip",
  "x-forwarded-for",
  "forwarded",
] as const;

// The ONLY client identity the container sees: a per-instance keyed hash of the
// client IP, which the DS rate limiter keys on (ratelimit.rs). Always deleted
// from the inbound request first, so a client cannot choose its own bucket.
const CLIENT_BUCKET_HEADER = "x-pollis-client-bucket";

// Bytes of HMAC-SHA256 output kept in the bucket — matches the DS's own
// truncation; 128 bits makes two clients sharing a budget negligible.
const CLIENT_BUCKET_BYTES = 16;

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
} & Record<(typeof SECRET_KEYS)[number], SecretStoreBinding | undefined> &
  Partial<Record<(typeof TUNABLE_VAR_KEYS)[number], string>>;

// The tunables this environment actually sets, ready to spread into envVars.
// Unset/empty keys are omitted so the DS default applies.
function tunableVarEnv(env: Env): Record<string, string> {
  const out: Record<string, string> = {};
  for (const key of TUNABLE_VAR_KEYS) {
    const value = env[key];
    if (value) {
      out[key] = value;
    }
  }
  return out;
}

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
  //
  // DO NOT LOWER THIS BELOW THE OTP LIFETIME (#1142). The DS holds the OTP
  // failed-guess counter and mailbox lockout in memory, so sleeping clears them.
  // That is safe only because the silence needed to trigger a sleep (10m) also
  // expires the code being attacked (OTP_TTL_SECS, 600s) — the reset cannot
  // outlive the secret it protects. Lower it and an attacker earns a fresh
  // 5-guess budget against one LIVE code every sleep window just by pausing.
  // `tests/otp_state_durability.rs` reads this line and fails if the inequality
  // inverts; raising it (a cost decision) is always safe.
  sleepAfter = "10m";
  // The DS reaches out to Turso, Resend, LiveKit and R2 — needs egress.
  enableInternet = true;

  // HMAC key for CLIENT_BUCKET_HEADER: random, non-extractable, generated once
  // per Durable Object instance and held only in its memory. A per-instance key
  // is enough — and needs no secret to provision — because every request goes
  // through this ONE object (the DS is a single serialized instance), so one
  // client always lands in one bucket for as long as the object lives. When the
  // object is recreated the key changes, which resets per-IP counters exactly as
  // a container restart already does.
  private bucketKey?: Promise<CryptoKey>;

  // hex(HMAC-SHA256(bucketKey, ip)[..CLIENT_BUCKET_BYTES]).
  private async clientBucket(ip: string): Promise<string> {
    this.bucketKey ??= crypto.subtle.generateKey(
      { name: "HMAC", hash: "SHA-256" },
      false,
      ["sign"],
    ) as Promise<CryptoKey>;
    const mac = await crypto.subtle.sign(
      "HMAC",
      await this.bucketKey,
      new TextEncoder().encode(ip),
    );
    return [...new Uint8Array(mac, 0, CLIENT_BUCKET_BYTES)]
      .map((b) => b.toString(16).padStart(2, "0"))
      .join("");
  }

  // Replace every client-IP header with the keyed bucket before the request
  // reaches the container. `CF-Connecting-IP` is set by Cloudflare's edge and
  // cannot be forged through it; the first `X-Forwarded-For` hop is a fallback
  // that never applies in practice behind Cloudflare.
  override async fetch(request: Request): Promise<Response> {
    const headers = new Headers(request.headers);
    const ip =
      headers.get("cf-connecting-ip")?.trim() ||
      headers.get("x-forwarded-for")?.split(",")[0]?.trim() ||
      "";
    for (const name of CLIENT_IP_HEADERS) {
      headers.delete(name);
    }
    headers.delete(CLIENT_BUCKET_HEADER);
    if (ip) {
      headers.set(CLIENT_BUCKET_HEADER, await this.clientBucket(ip));
    }
    return super.fetch(new Request(request, { headers }));
  }

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
    // Per-environment rate-limit tunables (TUNABLE_VAR_KEYS), only those set.
    ...tunableVarEnv(this.env),
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
