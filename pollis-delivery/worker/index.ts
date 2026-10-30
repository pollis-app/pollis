// Cloudflare Worker front-door for the Pollis Delivery Service.
//
// One serialized Cloudflare Container behind a Durable Object (single writer to Turso, #419/#420).
//
// Secrets Store bindings are async, so `startAndWaitForPorts` resolves them into env vars at boot.
import {
  Container,
  type ContainerStartConfigOptions,
} from "@cloudflare/containers";
import { handleUpdates, isUpdatesPath } from "./updates";
import {
  DEV_KEY_HEADER,
  GateCounts,
  acceptedKeys,
  gate,
  gateModeFrom,
  type GateInputs,
  type GateMode,
} from "./dev-gate";
import { ENV_FINGERPRINT_KEY, envFingerprint, envVerdict } from "./container-env";

// WHERE the DS container physically runs (#658).
// A DO is placed near whoever first created it, permanently; `enam` matches the Turso primary.
// NOT A PERFORMANCE LEVER: re-placing dev under this hint changed nothing (#658).
const DS_LOCATION_HINT: DurableObjectLocationHint = "enam";

// `locationHint` applies only when an object is first created; existing objects never migrate.
// Fallback object name when `DS_SINGLETON_NAME` is unset (prod's object).
// RENAMING RE-PLACES THE CONTAINER: follow docs/deployments.md, or `max_instances: 1` blocks it.
const DS_SINGLETON_NAME_FALLBACK = "pollis-delivery-singleton";

// Secrets Store keys passed to the container as env vars; missing ones are skipped.
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
  // #720: gates GET /v1/retention/metrics; absent, the endpoint 404s.
  "POLLIS_DS_METRICS_TOKEN",
  // #707: authenticates push fan-out to Expo.
  "EXPO_TOKEN",
  // `<email>:<6 digits>` — the one account App Store / Play review signs in
  // with; that address gets this fixed code and no email (otp.rs ReviewLogin).
  "APP_REVIEW_LOGIN",
] as const;

// Non-secret tunables, forwarded only when set; checked by scripts/check-ds-config-chain.py.
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
  // Dev-only ops log (ops_log.rs).
  "POLLIS_DS_OPS_LOG",
] as const;

// Every header that can carry a client IP, all deleted before forwarding.
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

// The ONLY client identity the container sees: a keyed hash of the IP; inbound copies are deleted.
const CLIENT_BUCKET_HEADER = "x-pollis-client-bucket";

// Bytes of HMAC-SHA256 output kept (128 bits), matching the DS.
const CLIENT_BUCKET_BYTES = 16;

// How long a resolved dev-gate secret is reused before re-reading the store.
const GATE_SECRET_TTL_MS = 5 * 60 * 1000;

// A Secrets Store value, or undefined when unbound, unset or unreadable.
async function readSecret(binding: SecretStoreBinding | undefined): Promise<string | undefined> {
  if (!binding) {
    return undefined;
  }
  try {
    return (await binding.get()) || undefined;
  } catch {
    return undefined;
  }
}

interface SecretStoreBinding {
  get(): Promise<string>;
}

type Env = {
  POLLIS_DELIVERY: DurableObjectNamespace<PollisDelivery>;
  // Static (non-secret) container config, set as wrangler `vars`.
  PORT: string;
  POLLIS_DS_REQUIRE_AUTH: string;
  POLLIS_DS_WATERMARK_STALE_MONTHS?: string;
  // #1611: the published minimum-client-version policy the DS mirrors and enforces.
  POLLIS_CLIENT_POLICY_URL?: string;
  // The object hosting this environment's container; per environment so dev can be re-placed alone.
  DS_SINGLETON_NAME?: string;
  // Mobile OTA updates (#1250): prod only; dev answers /updates/ with 404.
  UPDATES?: R2Bucket;
  OTA_CHANNEL?: string;
  // The dev gate (#1242): dev only; the key is never forwarded to the container.
  DEV_GATE_MODE?: string;
  POLLIS_DEV_ACCESS_KEY?: SecretStoreBinding;
} & Record<(typeof SECRET_KEYS)[number], SecretStoreBinding | undefined> &
  Partial<Record<(typeof TUNABLE_VAR_KEYS)[number], string>>;

// The tunables this environment sets; unset keys are omitted.
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
  // Scale-to-zero after 10m (the library default, kept explicit); always-on is a cost call (#515).
  // NEVER BELOW THE OTP LIFETIME (#1142): sleep clears the in-memory lockout (otp_state_durability.rs).
  sleepAfter = "10m";
  // The DS reaches out to Turso, Resend, LiveKit and R2 — needs egress.
  enableInternet = true;

  // Per-instance in-memory HMAC key for CLIENT_BUCKET_HEADER; one object, so one bucket per client.
  private bucketKey?: Promise<CryptoKey>;

  // Dev gate state (dev-gate.ts): the resolved secrets, re-read every
  // GATE_SECRET_TTL_MS, and the verdict counts behind GET /__gate.
  private gateSecrets?: { at: number; keys: string[]; statsToken?: string };
  private gateCounts?: GateCounts;

  private async gateInputs(mode: GateMode): Promise<GateInputs> {
    const now = Date.now();
    if (!this.gateSecrets || now - this.gateSecrets.at > GATE_SECRET_TTL_MS) {
      const [key, statsToken] = await Promise.all([
        readSecret(this.env.POLLIS_DEV_ACCESS_KEY),
        readSecret(this.env.POLLIS_DS_METRICS_TOKEN),
      ]);
      this.gateSecrets = { at: now, keys: acceptedKeys(key), statsToken };
    }
    this.gateCounts ??= new GateCounts(this.ctx.storage);
    return {
      mode,
      keys: this.gateSecrets.keys,
      statsToken: this.gateSecrets.statsToken,
      counts: this.gateCounts,
    };
  }

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

  // Per Durable Object instance (each deploy restarts it), until a running container is seen with
  // the current environment: a container a rollout restarted with the previous start's environment
  // is restarted with the current one (container-env.ts). Concurrent requests share one check.
  private envSettled = false;
  private envCheck?: Promise<boolean>;

  // True once settled. A check that found nothing running, restarted the container or failed
  // leaves it unsettled, so the next request checks again (#1624).
  private async reconcileEnv(): Promise<boolean> {
    try {
      const running = this.ctx.container?.running ?? false;
      if (!running) {
        // "recheck", without paying for the secret reads.
        return false;
      }
      const current = await envFingerprint({ ...this.envVars, ...(await this.resolveSecretEnv()) });
      const recorded = await this.ctx.storage.get<string>(ENV_FINGERPRINT_KEY);
      const verdict = envVerdict(running, recorded, current);
      if (verdict === "restart") {
        await this.destroy();
      }
      return verdict === "settled";
    } catch {
      // The Worker logs nothing (no_client_ip_exposure); a failed check skips the restart this time.
      return false;
    }
  }

  private async ensureEnvReconciled(): Promise<void> {
    if (this.envSettled) {
      return;
    }
    this.envCheck ??= this.reconcileEnv().finally(() => {
      this.envCheck = undefined;
    });
    if (await this.envCheck) {
      this.envSettled = true;
    }
  }

  // The dev gate first, so a refused request never wakes the container; then bucket the client IP.
  override async fetch(request: Request): Promise<Response> {
    const gateMode = gateModeFrom(this.env.DEV_GATE_MODE);
    if (gateMode !== undefined) {
      const refused = await gate(request, await this.gateInputs(gateMode));
      if (refused) {
        return refused;
      }
    }
    const headers = new Headers(request.headers);
    // The container has no use for the dev key; it stops at the gate.
    headers.delete(DEV_KEY_HEADER);
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
    await this.ensureEnvReconciled();
    return super.fetch(new Request(request, { headers }));
  }

  // Non-secret config from wrangler `vars`; secrets come in startAndWaitForPorts.
  envVars = {
    PORT: this.env.PORT ?? "8788",
    POLLIS_DS_REQUIRE_AUTH: this.env.POLLIS_DS_REQUIRE_AUTH ?? "true",
    // #720: a wrangler `var` binds to the Worker, not the container, so forward it explicitly.
    ...(this.env.POLLIS_DS_WATERMARK_STALE_MONTHS
      ? { POLLIS_DS_WATERMARK_STALE_MONTHS: this.env.POLLIS_DS_WATERMARK_STALE_MONTHS }
      : {}),
    // #1611: forwarded like the watermark; unset leaves the minimum-version check off.
    ...(this.env.POLLIS_CLIENT_POLICY_URL
      ? { POLLIS_CLIENT_POLICY_URL: this.env.POLLIS_CLIENT_POLICY_URL }
      : {}),
    // Per-environment rate-limit tunables (TUNABLE_VAR_KEYS), only those set.
    ...tunableVarEnv(this.env),
  };

  // Resolve every Secrets Store binding into an env map, skipping unset ones.
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

  // Inject secrets before traffic; handles every overload (containerFetch uses the positional one).
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
    const envVars: Record<string, string> = {
      ...this.envVars,
      ...secretEnv,
      ...resolvedStart?.envVars,
    };
    // Positional form, preserving ports + cancellation.
    await super.startAndWaitForPorts(ports, resolvedCancellation, {
      ...resolvedStart,
      envVars,
    });
    // What this start passed, so the next deploy can tell a stale container (container-env.ts).
    await this.ctx.storage.put(ENV_FINGERPRINT_KEY, await envFingerprint(envVars));
  }
}

export default {
  // Forward everything to the container, except `/updates/` (updates.ts), answered here from R2.
  async fetch(request: Request, env: Env): Promise<Response> {
    if (isUpdatesPath(new URL(request.url).pathname)) {
      return handleUpdates(request, env);
    }
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
