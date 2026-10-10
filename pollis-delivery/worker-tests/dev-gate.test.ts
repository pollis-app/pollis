/*
 * worker/dev-gate.ts — only our own clients reach the dev Delivery Service
 * (#1242).
 *
 * Pinned: an unset mode means no gate at all (prod); `report` forwards every
 * request and counts it; `enforce` 404s a missing or wrong key and forwards a
 * right one; /health and /version are always answered; `off` checks nothing;
 * a typo'd mode falls back to `report`, never to enforce; a rotation accepts
 * every listed key; /__gate needs the operator bearer and returns only counts
 * keyed by verdict × path class × UA family (no address, no per-request
 * record); prod's wrangler config binds neither the mode nor the key; and
 * index.ts runs the gate before the container forward and strips the key.
 *
 *   node --test pollis-delivery/worker-tests/*.test.ts   (or `pnpm test:worker` in pollis-delivery/)
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  DEV_KEY_HEADER,
  GateCounts,
  acceptedKeys,
  gate,
  gateModeFrom,
  pathClass,
  uaFamily,
  verdictFor,
  type CountStore,
  type GateInputs,
  type GateMode,
} from "../worker/dev-gate.ts";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const KEY = "k-0123456789abcdef0123456789abcdef";
const TOKEN = "operator-token";

function memoryStore(): CountStore & { data: Map<string, unknown> } {
  const data = new Map<string, unknown>();
  return {
    data,
    async get<T>(key: string) {
      return data.get(key) as T | undefined;
    },
    async put<T>(key: string, value: T) {
      data.set(key, structuredClone(value));
    },
  };
}

function inputs(mode: GateMode, keys = [KEY], store = memoryStore()): GateInputs {
  return { mode, keys, statsToken: TOKEN, counts: new GateCounts(store) };
}

function req(path: string, headers: Record<string, string> = {}): Request {
  return new Request(`https://api-dev.pollis.com${path}`, { method: "POST", headers });
}

async function stats(i: GateInputs): Promise<{ mode: string; key_configured: boolean; counts: Record<string, number> }> {
  const res = await gate(new Request("https://api-dev.pollis.com/__gate", { headers: { authorization: `Bearer ${TOKEN}` } }), i);
  assert.ok(res);
  assert.equal(res.status, 200);
  return res.json();
}

test("an unset mode is no gate at all; a typo is report, never enforce", () => {
  assert.equal(gateModeFrom(undefined), undefined);
  assert.equal(gateModeFrom("off"), "off");
  assert.equal(gateModeFrom("report"), "report");
  assert.equal(gateModeFrom(" ENFORCE "), "enforce");
  assert.equal(gateModeFrom("enfroce"), "report");
  assert.equal(gateModeFrom(""), "report");
});

test("a rotation accepts every listed key, and only those", async () => {
  const keys = acceptedKeys(" old-key , new-key ,");
  assert.deepEqual(keys, ["old-key", "new-key"]);
  assert.equal(await verdictFor("old-key", keys), "ok");
  assert.equal(await verdictFor("new-key", keys), "ok");
  assert.equal(await verdictFor("new-ke", keys), "bad");
  assert.equal(await verdictFor(null, keys), "missing");
  assert.equal(await verdictFor("", keys), "missing");
  // No key configured: nothing can be ok.
  assert.equal(await verdictFor("anything", acceptedKeys(undefined)), "bad");
});

test("report forwards everything and counts each verdict", async () => {
  const i = inputs("report");
  assert.equal(await gate(req("/v1/auth/request-otp", { [DEV_KEY_HEADER]: KEY }), i), undefined);
  assert.equal(await gate(req("/v1/auth/request-otp"), i), undefined);
  assert.equal(await gate(req("/v1/r2/presign", { [DEV_KEY_HEADER]: "wrong", "user-agent": "curl/8.7.1" }), i), undefined);
  const s = await stats(i);
  assert.equal(s.mode, "report");
  assert.equal(s.key_configured, true);
  assert.deepEqual(s.counts, {
    "ok auth none": 1,
    "missing auth none": 1,
    "bad r2 curl": 1,
  });
});

test("enforce 404s a missing or wrong key with an empty body, forwards the right one", async () => {
  const i = inputs("enforce");
  assert.equal(await gate(req("/v1/messages/send", { [DEV_KEY_HEADER]: KEY }), i), undefined);
  for (const headers of [{}, { [DEV_KEY_HEADER]: "wrong" }]) {
    const res = await gate(req("/v1/messages/send", headers), i);
    assert.ok(res);
    assert.equal(res.status, 404);
    assert.equal(await res.text(), "");
  }
});

test("/health and /version answer without a key, uncounted, even under enforce", async () => {
  const i = inputs("enforce");
  assert.equal(await gate(req("/health"), i), undefined);
  assert.equal(await gate(req("/version"), i), undefined);
  assert.deepEqual((await stats(i)).counts, {});
});

test("off checks and counts nothing", async () => {
  const i = inputs("off");
  assert.equal(await gate(req("/v1/auth/verify-otp"), i), undefined);
  assert.deepEqual((await stats(i)).counts, {});
});

test("/__gate is a 404 without the operator bearer", async () => {
  const i = inputs("report");
  for (const headers of [{}, { authorization: "Bearer nope" }, { authorization: TOKEN }, { [DEV_KEY_HEADER]: KEY }]) {
    const res = await gate(new Request("https://api-dev.pollis.com/__gate", { headers }), i);
    assert.ok(res);
    assert.equal(res.status, 404);
  }
  // No token resolvable → never served.
  const noToken = { ...inputs("report"), statsToken: undefined };
  const res = await gate(new Request("https://api-dev.pollis.com/__gate", { headers: { authorization: "Bearer " } }), noToken);
  assert.equal(res?.status, 404);
});

test("counts hold only verdict × path class × UA family, and survive a new object", async () => {
  const store = memoryStore();
  await gate(
    req("/v1/livekit/token", {
      "user-agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15",
      "cf-connecting-ip": "203.0.113.7",
      "x-forwarded-for": "203.0.113.7",
    }),
    inputs("report", [KEY], store),
  );
  const stored = JSON.stringify([...store.data.values()]);
  assert.ok(!stored.includes("203.0.113.7"), "an address reached the counts");
  assert.ok(!stored.includes("Macintosh"), "a full user agent reached the counts");
  // A fresh Durable Object reads the same store.
  assert.deepEqual((await stats(inputs("report", [KEY], store))).counts, { "missing livekit browser": 1 });
});

test("path classes and UA families are coarse", () => {
  assert.equal(pathClass("/v1/auth/verify-otp"), "auth");
  assert.equal(pathClass("/v1/r2/presign"), "r2");
  assert.equal(pathClass("/v1/livekit/token"), "livekit");
  assert.equal(pathClass("/v1/ops/otp-requests"), "ops");
  assert.equal(pathClass("/v1/config"), "ops");
  assert.equal(pathClass("/v1/retention/metrics"), "ops");
  assert.equal(pathClass("/v1/messages/send"), "api");
  assert.equal(pathClass("/wp-login.php"), "other");
  assert.equal(uaFamily(null), "none");
  assert.equal(uaFamily("Googlebot/2.1"), "bot");
  assert.equal(uaFamily("curl/8.7.1"), "curl");
  assert.equal(uaFamily("okhttp/4.12.0"), "mobile-http");
  assert.equal(uaFamily("python-requests/2.32"), "other");
});

// ── Config pins ───────────────────────────────────────────────────────────────

function jsonc(path: string): Record<string, unknown> {
  const raw = readFileSync(path, "utf8");
  const stripped = raw.replace(/("(?:[^"\\]|\\.)*")|\/\/[^\n]*/g, (_m, str: string | undefined) => str ?? "");
  return JSON.parse(stripped);
}

type Binding = { binding: string; secret_name: string };

test("prod binds neither the gate mode nor the key; dev binds both, enforced", () => {
  const prod = jsonc(join(root, "wrangler.prod.jsonc"));
  assert.equal((prod.vars as Record<string, string>).DEV_GATE_MODE, undefined);
  assert.ok(!(prod.secrets_store_secrets as Binding[]).some((b) => b.binding === "POLLIS_DEV_ACCESS_KEY"));
  const dev = jsonc(join(root, "wrangler.dev.jsonc"));
  assert.equal((dev.vars as Record<string, string>).DEV_GATE_MODE, "enforce");
  const key = (dev.secrets_store_secrets as Binding[]).find((b) => b.binding === "POLLIS_DEV_ACCESS_KEY");
  assert.equal(key?.secret_name, "DS_DEV_POLLIS_DEV_ACCESS_KEY");
});

test("index.ts gates before the container forward, strips the key, never forwards it", () => {
  const index = readFileSync(join(root, "worker", "index.ts"), "utf8");
  const body = index.slice(index.indexOf("override async fetch(request: Request)"));
  const gateAt = body.indexOf("await gate(request");
  const forwardAt = body.indexOf("super.fetch(");
  assert.ok(gateAt > 0 && gateAt < forwardAt, "the gate must run before super.fetch");
  assert.ok(body.slice(0, forwardAt).includes("headers.delete(DEV_KEY_HEADER)"));
  const secretKeys = /const SECRET_KEYS = \[(.*?)\] as const/s.exec(index)?.[1] ?? "";
  assert.ok(!secretKeys.includes("POLLIS_DEV_ACCESS_KEY"), "the container must never be given the dev key");
});
