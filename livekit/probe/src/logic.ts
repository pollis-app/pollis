// Pure decision logic for the LiveKit external probe: given the previous state
// and this run's results, what to store, whether to alert, whether to dispatch
// the restart workflow. No I/O, so `node --test` covers it without a runtime.

export interface CheckResult {
  ok: boolean;
  // Short, IP-free description ("200 OK", "timeout", "STUN type 0x0111").
  detail: string;
}

export interface ProbeResult {
  at: number;
  rtc: CheckResult;
  turn: CheckResult;
}

export interface ProbeState {
  // Two consecutive failing runs (ten minutes at the 5-minute cron) flip this;
  // one run recovers it. A single missed run is noise, not an outage.
  down: boolean;
  failures: number;
  // When the current outage was first observed (first failing run), or null.
  since: number | null;
  lastDispatchAt: number | null;
  lastAlertAt: number | null;
  last: ProbeResult;
}

export interface Decision {
  state: ProbeState;
  alert: string | null;
  dispatchRestart: boolean;
}

export const FAILURES_BEFORE_DOWN = 2;
// A restart takes ~1-2 minutes end to end; re-dispatching every five minutes
// would only queue runs behind each other (the workflow's concurrency group).
export const DISPATCH_COOLDOWN_MS = 30 * 60 * 1000;
export const REMIND_EVERY_MS = 60 * 60 * 1000;

export function isOk(r: ProbeResult): boolean {
  return r.rtc.ok && r.turn.ok;
}

function describe(r: ProbeResult): string {
  return `rtc: ${r.rtc.detail}; turn: ${r.turn.detail}`;
}

function minutes(ms: number): string {
  return `${Math.round(ms / 60000)} min`;
}

export function decide(prev: ProbeState | null, result: ProbeResult): Decision {
  const now = result.at;
  if (isOk(result)) {
    const alert =
      prev?.down && prev.since !== null
        ? `LiveKit recovered after ${minutes(now - prev.since)} (${describe(result)})`
        : null;
    return {
      state: {
        down: false,
        failures: 0,
        since: null,
        lastDispatchAt: prev?.lastDispatchAt ?? null,
        lastAlertAt: alert ? now : (prev?.lastAlertAt ?? null),
        last: result,
      },
      alert,
      dispatchRestart: false,
    };
  }

  const failures = (prev?.failures ?? 0) + 1;
  const since = prev?.since ?? now;
  const down = failures >= FAILURES_BEFORE_DOWN;
  const wasDown = prev?.down ?? false;

  let alert: string | null = null;
  if (down && !wasDown) {
    alert = `LiveKit DOWN for ${minutes(now - since)} (${describe(result)}) — dispatching livekit-restart.yml`;
  } else if (down && prev?.lastAlertAt !== null && prev !== null && now - prev.lastAlertAt >= REMIND_EVERY_MS) {
    alert = `LiveKit still down, ${minutes(now - since)} (${describe(result)})`;
  }

  const lastDispatchAt = prev?.lastDispatchAt ?? null;
  const dispatchRestart = down && (lastDispatchAt === null || now - lastDispatchAt >= DISPATCH_COOLDOWN_MS);

  return {
    state: {
      down,
      failures,
      since,
      lastDispatchAt: dispatchRestart ? now : lastDispatchAt,
      lastAlertAt: alert ? now : (prev?.lastAlertAt ?? null),
      last: result,
    },
    alert,
    dispatchRestart,
  };
}

// STUN Binding Request (RFC 5389 §6): type 0x0001, zero-length body, magic
// cookie, 12-byte transaction id. LiveKit's TURN listener answers one without
// credentials, so a Binding Success Response proves the TLS session reached
// LiveKit's TURN server through nginx's SNI demux — not merely that something
// on 443 completed a handshake.
export const STUN_MAGIC_COOKIE = [0x21, 0x12, 0xa4, 0x42] as const;

export function stunBindingRequest(transactionId: Uint8Array): Uint8Array {
  if (transactionId.length !== 12) {
    throw new Error("transaction id must be 12 bytes");
  }
  const msg = new Uint8Array(20);
  msg[0] = 0x00;
  msg[1] = 0x01;
  msg[2] = 0x00;
  msg[3] = 0x00;
  msg.set(STUN_MAGIC_COOKIE, 4);
  msg.set(transactionId, 8);
  return msg;
}

// Accepts a Binding Success Response (0x0101) echoing our transaction id.
export function checkStunResponse(buf: Uint8Array, transactionId: Uint8Array): CheckResult {
  if (buf.length < 20) {
    return { ok: false, detail: `short STUN reply (${buf.length} bytes)` };
  }
  const type = (buf[0] << 8) | buf[1];
  for (let i = 0; i < 4; i++) {
    if (buf[4 + i] !== STUN_MAGIC_COOKIE[i]) {
      return { ok: false, detail: "reply is not STUN (bad magic cookie)" };
    }
  }
  for (let i = 0; i < 12; i++) {
    if (buf[8 + i] !== transactionId[i]) {
      return { ok: false, detail: "STUN transaction id mismatch" };
    }
  }
  if (type !== 0x0101) {
    return { ok: false, detail: `STUN type 0x${type.toString(16).padStart(4, "0")}` };
  }
  return { ok: true, detail: "STUN binding over TURNS ok" };
}
