// External liveness probe for the LiveKit stack (livekit/DEPLOY.md → "External probe").
//
// Every five minutes: GET https://rtc.pollis.com/ must answer 200 "OK" through
// nginx → LiveKit, and a STUN Binding Request over TLS to turn.pollis.com:443
// must come back as a Binding Success Response from LiveKit's TURN listener.
// Two consecutive failing runs alert and dispatch livekit-restart.yml through
// the GitHub API. GET / on the Worker reports the last result (no request data
// is read), so status.pollis.com (archon) can show the voice server.
//
// Observes only our own servers; it never sees a client, so it has no IP to
// keep — and the wrangler config still pins Workers Logs and Logpush off.
import { DurableObject } from "cloudflare:workers";
import { connect } from "cloudflare:sockets";
import {
  checkStunResponse,
  decide,
  stunBindingRequest,
  type CheckResult,
  type ProbeResult,
  type ProbeState,
} from "./logic";

export interface Env {
  PROBE_STATE: DurableObjectNamespace<ProbeStateObject>;
  RTC_URL: string;
  TURN_HOST: string;
  TURN_PORT: string;
  GITHUB_REPO: string;
  RESTART_WORKFLOW: string;
  // Secrets (`wrangler secret put`): a fine-grained token with Actions
  // read/write on GITHUB_REPO only, and an optional incoming-webhook URL.
  GITHUB_DISPATCH_TOKEN?: string;
  ALERT_WEBHOOK_URL?: string;
}

const CHECK_TIMEOUT_MS = 10_000;
const STATE_KEY = "state";

// The probe's memory: one SQLite-backed object, so a bare `wrangler deploy`
// from a checkout needs no namespace id to be created first.
export class ProbeStateObject extends DurableObject<Env> {
  async load(): Promise<ProbeState | null> {
    return (await this.ctx.storage.get<ProbeState>(STATE_KEY)) ?? null;
  }

  async save(state: ProbeState): Promise<void> {
    await this.ctx.storage.put(STATE_KEY, state);
  }
}

async function checkRtc(url: string): Promise<CheckResult> {
  try {
    const res = await fetch(url, {
      signal: AbortSignal.timeout(CHECK_TIMEOUT_MS),
      headers: { "user-agent": "pollis-livekit-probe" },
    });
    const body = (await res.text()).trim();
    if (res.status === 200 && body === "OK") {
      return { ok: true, detail: "200 OK" };
    }
    return { ok: false, detail: `${res.status} ${body.slice(0, 32)}` };
  } catch (e) {
    return { ok: false, detail: `fetch failed: ${(e as Error).name}` };
  }
}

async function checkTurn(hostname: string, port: number): Promise<CheckResult> {
  const txid = crypto.getRandomValues(new Uint8Array(12));
  const timer = new Promise<CheckResult>((resolve) =>
    setTimeout(() => resolve({ ok: false, detail: "TURNS timeout" }), CHECK_TIMEOUT_MS),
  );
  let socket: Socket;
  try {
    socket = connect({ hostname, port }, { secureTransport: "on", allowHalfOpen: false });
  } catch (e) {
    return { ok: false, detail: `TURNS connect failed: ${(e as Error).name}` };
  }
  const run = (async (): Promise<CheckResult> => {
    await socket.opened;
    const writer = socket.writable.getWriter();
    await writer.write(stunBindingRequest(txid));
    writer.releaseLock();
    const reader = socket.readable.getReader();
    let buf = new Uint8Array(0);
    while (buf.length < 20) {
      const { value, done } = await reader.read();
      if (done) {
        break;
      }
      const next = new Uint8Array(buf.length + value.length);
      next.set(buf);
      next.set(value, buf.length);
      buf = next;
    }
    reader.releaseLock();
    return checkStunResponse(buf, txid);
  })().catch((e: Error) => ({ ok: false, detail: `TURNS failed: ${e.name}` }));
  const result = await Promise.race([run, timer]);
  try {
    await socket.close();
  } catch {
    // The check already has its verdict.
  }
  return result;
}

async function alert(env: Env, message: string): Promise<void> {
  if (!env.ALERT_WEBHOOK_URL) {
    return;
  }
  // `content` is what Discord-style hooks read, `text` what Slack-style ones do.
  await fetch(env.ALERT_WEBHOOK_URL, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ content: message, text: message }),
    signal: AbortSignal.timeout(CHECK_TIMEOUT_MS),
  }).catch(() => undefined);
}

async function dispatchRestart(env: Env): Promise<string> {
  if (!env.GITHUB_DISPATCH_TOKEN) {
    return "no GITHUB_DISPATCH_TOKEN — restart not dispatched";
  }
  const url = `https://api.github.com/repos/${env.GITHUB_REPO}/actions/workflows/${env.RESTART_WORKFLOW}/dispatches`;
  const res = await fetch(url, {
    method: "POST",
    headers: {
      accept: "application/vnd.github+json",
      authorization: `Bearer ${env.GITHUB_DISPATCH_TOKEN}`,
      "content-type": "application/json",
      "user-agent": "pollis-livekit-probe",
      "x-github-api-version": "2022-11-28",
    },
    body: JSON.stringify({ ref: "main", inputs: {} }),
    signal: AbortSignal.timeout(CHECK_TIMEOUT_MS),
  }).catch(() => null);
  if (res === null) {
    return "dispatch request failed";
  }
  return res.status === 204 ? "restart dispatched" : `dispatch answered ${res.status}`;
}

async function runProbe(env: Env): Promise<void> {
  const [rtc, turn] = await Promise.all([
    checkRtc(env.RTC_URL),
    checkTurn(env.TURN_HOST, Number(env.TURN_PORT)),
  ]);
  const result: ProbeResult = { at: Date.now(), rtc, turn };
  const stub = env.PROBE_STATE.get(env.PROBE_STATE.idFromName("singleton"));
  const prev = await stub.load();
  const decision = decide(prev, result);
  await stub.save(decision.state);
  if (decision.dispatchRestart) {
    const outcome = await dispatchRestart(env);
    if (decision.alert) {
      decision.alert = `${decision.alert} (${outcome})`;
    }
  }
  if (decision.alert) {
    await alert(env, decision.alert);
  }
}

export default {
  async scheduled(_controller: ScheduledController, env: Env, ctx: ExecutionContext): Promise<void> {
    ctx.waitUntil(runProbe(env));
  },

  // Public, read-only summary for the status page. Reads nothing from the
  // request and never echoes it.
  async fetch(_request: Request, env: Env): Promise<Response> {
    const stub = env.PROBE_STATE.get(env.PROBE_STATE.idFromName("singleton"));
    const state = await stub.load();
    const body = state
      ? {
          ok: !state.down,
          since: state.since,
          checked_at: state.last.at,
          rtc: state.last.rtc,
          turn: state.last.turn,
        }
      : { ok: null, checked_at: null };
    return new Response(JSON.stringify(body), {
      status: state?.down ? 503 : 200,
      headers: {
        "content-type": "application/json",
        "cache-control": "no-store",
        "access-control-allow-origin": "*",
      },
    });
  },
} satisfies ExportedHandler<Env>;
