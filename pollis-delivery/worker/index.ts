// Cloudflare Worker front-door for the Pollis Delivery Service: a
// single-instance Container behind a Durable Object, so there is exactly one
// serialized DS instance (single-writer-to-Turso, #419/#420). Secrets Store
// bindings are async, so they are resolved in `startAndWaitForPorts` rather
// than the static `envVars`. See docs/deployments.md.
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

// Matches the `aws-us-east-1` Turso primary, so placement is a reviewable
// declaration rather than whoever sent the first request. Honoured only when the
// object is first created. NOT a performance lever: tested in #658, no effect on
// the container (docs/deployments.md).
const DS_LOCATION_HINT: DurableObjectLocationHint = "enam";

// Prod's long-standing object, for a config that sets no `DS_SINGLETON_NAME`.
// CHANGING AN ENVIRONMENT'S NAME RE-PLACES ITS CONTAINER and must follow the
// staged procedure in docs/deployments.md: a bare rename creates a second object
// that `max_instances: 1` never lets start, so every request 500s.
const DS_SINGLETON_NAME_FALLBACK = "pollis-delivery-singleton";

// Secrets Store keys passed to the container as OS env vars. Missing keys are
// skipped so an absent dev-only secret never bricks startup.
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
  // Gates the operator endpoints (#720); absent here, they 404.
  "POLLIS_DS_METRICS_TOKEN",
  // Push fan-out auth (#707); Expo rejects unauthenticated sends under
  // Enhanced Security.
  "EXPO_TOKEN",
  // `<email>:<6 digits>` — the one account App Store / Play review signs in
  // with; that address gets this fixed code and no email (otp.rs ReviewLogin).
  "APP_REVIEW_LOGIN",
] as const;

// Non-secret wrangler `vars`, forwarded only when set so an unset key keeps the
// DS default. Checked against ds-config-manifest.json `optional_container_vars`
// by scripts/check-ds-config-chain.py.
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

// Every header through which a client IP can reach the container, all deleted
// before forwarding. Exhaustive, including headers only optional zone settings
// add, so a dashboard toggle cannot leak an address. tests/no_client_ip_exposure.rs
// asserts the list is complete.
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
// IP. Always deleted from the inbound request, so a client cannot pick its bucket.
const CLIENT_BUCKET_HEADER = "x-pollis-client-bucket";

// 128 bits, matching the DS's own truncation.
const CLIENT_BUCKET_BYTES = 16;

// A rotated dev-gate key is honoured within this window.
const GATE_SECRET_TTL_MS = 5 * 60 * 1000;

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
  PORT: string;
  POLLIS_DS_REQUIRE_AUTH: string;
  POLLIS_DS_WATERMARK_STALE_MONTHS?: string;
  // Per environment, so re-placing dev never re-places prod.
  DS_SINGLETON_NAME?: string;
  // Mobile OTA updates (#1250, updates.ts). Prod only.
  UPDATES?: R2Bucket;
  OTA_CHANNEL?: string;
  // The dev gate (#1242, dev-gate.ts). Dev only; never forwarded.
  DEV_GATE_MODE?: string;
  POLLIS_DEV_ACCESS_KEY?: SecretStoreBinding;
} & Record<(typeof SECRET_KEYS)[number], SecretStoreBinding | undefined> &
  Partial<Record<(typeof TUNABLE_VAR_KEYS)[number], string>>;

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
  defaultPort = 8788;
  pingEndpoint = "/health";
  // The library default, kept explicit; always-on is a cost decision, not
  // expressible here (#515, #695).
  //
  // DO NOT LOWER THIS BELOW THE OTP LIFETIME (#1142). Sleeping clears the
  // in-memory OTP guess counter and lockout; that is safe only because the
  // silence needed to sleep also expires the code under attack.
  // `tests/otp_state_durability.rs` reads this line and fails if it inverts.
  sleepAfter = "10m";
  enableInternet = true;

  // Random, non-extractable, per object instance. Enough because every request
  // goes through this one object; recreating it resets per-IP counters, as a
  // container restart already does.
  private bucketKey?: Promise<CryptoKey>;

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

  // The dev gate first, so a refused request never wakes the container. Then
  // every client-IP header is replaced by the keyed bucket. `CF-Connecting-IP`
  // cannot be forged through Cloudflare's edge.
  override async fetch(request: Request): Promise<Response> {
    const gateMode = gateModeFrom(this.env.DEV_GATE_MODE);
    if (gateMode !== undefined) {
      const refused = await gate(request, await this.gateInputs(gateMode));
      if (refused) {
        return refused;
      }
    }
    const headers = new Headers(request.headers);
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
    return super.fetch(new Request(request, { headers }));
  }

  envVars = {
    PORT: this.env.PORT ?? "8788",
    POLLIS_DS_REQUIRE_AUTH: this.env.POLLIS_DS_REQUIRE_AUTH ?? "true",
    // A wrangler `var` binds to the Worker, not the container, so it must be
    // forwarded explicitly (#720).
    ...(this.env.POLLIS_DS_WATERMARK_STALE_MONTHS
      ? { POLLIS_DS_WATERMARK_STALE_MONTHS: this.env.POLLIS_DS_WATERMARK_STALE_MONTHS }
      : {}),
    ...tunableVarEnv(this.env),
  };

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
        // Optional/unset secret for this env.
      }
    }
    return out;
  }

  // Inject the resolved secrets before the container accepts traffic, on every
  // (re)start. containerFetch calls the positional overload, so all shapes are
  // parsed as the base does. Per-call `envVars` REPLACES the class `envVars`, so
  // they are re-merged here.
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
  // Forward everything to the container, which owns its routing (#515), except
  // `/updates/` (updates.ts), answered here from R2.
  async fetch(request: Request, env: Env): Promise<Response> {
    if (isUpdatesPath(new URL(request.url).pathname)) {
      return handleUpdates(request, env);
    }
    // `getContainer` gives no way to pass the placement hint.
    const id = env.POLLIS_DELIVERY.idFromName(
      env.DS_SINGLETON_NAME ?? DS_SINGLETON_NAME_FALLBACK,
    );
    return env.POLLIS_DELIVERY.get(id, {
      locationHint: DS_LOCATION_HINT,
    }).fetch(request);
  },
};
