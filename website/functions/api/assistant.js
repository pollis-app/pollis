// Cloudflare Pages Function — the site's own origin, standing in front of archon (#765).
//
// THE PROBLEM IT SOLVES. archon.pollis.com is gated by Cloudflare Access, so a browser could never
// reach it: every request 302s to the Access login and the CORS preflight is refused at the edge with
// 403. The assistant's remote path has therefore never worked in production — every answer ever served
// came from the on-device fallback, which is extractive by design and cannot synthesize an answer to a
// question nobody pre-wrote.
//
// WHY A PROXY RATHER THAN OPENING archon UP. The obvious fix — an Access bypass on /query — makes an
// unauthenticated AI endpoint reachable by the whole internet, which is an abuse and spend surface with
// no ceiling. This keeps archon FULLY Access-gated and gives exactly one caller a way in: this Function,
// holding an Access **service token** that lives server-side and never reaches a browser. The browser
// talks only to pollis.com, so there is no cross-origin request and no CORS to configure.
//
// The blast radius of a compromise here is a leaked service token, which is revocable from the Access
// dashboard without touching the site. The blast radius of the bypass alternative is an open model
// endpoint that cannot be revoked without breaking the feature.
//
// FAIL CLOSED, DELIBERATELY. This Function refuses to run unless BOTH a service token and a rate limiter
// are bound. A proxy to a paid model backend with no rate limit is not a degraded feature, it is an
// unmetered bill, so "configured" is not something an operator can half-do: missing either binding is a
// 503 and the client falls straight through to the on-device answer. See ASSISTANT_BINDINGS below.
//
// WIRE FORMAT: TRANSLATED HERE, ON PURPOSE. The browser speaks Pollis's own stable shape
// ({query} -> {answer}); archon speaks {question, history} -> {answer, usage}. Something has to
// translate, and it belongs server-side where it ships with the deploy rather than in a cached browser
// bundle.
//
// The shapes were DISCOVERED, not specified — the contract previously assumed in assistant.js
// (POST /query with {query}) was wrong in every particular and would have 405'd. Verified live against
// archon on 2026-08-08.
//
// `usage` is deliberately dropped: it carries per-request token counts, which are internal cost data and
// no business of a browser.
//
// It also does not log, store, or cache question text. A privacy-first product proxying its users'
// questions should retain nothing by default; response caching would cut cost but is a separate,
// deliberate decision rather than something to slip in here.

// Where archon actually lives. Overridable per-environment (preview vs production) via a Pages var,
// so a preview deployment can be pointed at a staging archon without a code change.
const ARCHON_BASE_DEFAULT = "https://archon.pollis.com";

// Hard ceiling on the upstream round trip. Must stay UNDER the browser's own ARCHON_TIMEOUT_MS in
// website/assistant.js — if this were the slower of the two, the browser would abandon the request first
// and the user would wait the full client timeout to be told nothing, while this Function kept a paid
// generation running that nobody would ever read.
//
// 18s because archon MEASURES at 6-8s per answer (2026-08-08: 6.2s / 6.6s / 7.5s), and warming its
// prompt cache does not help — the time is generation, not lookup. The previous 3.5s here and 4s in the
// browser were guesses made when the endpoint had never been called, and would have aborted every single
// request before it could answer.
const ARCHON_TIMEOUT_MS = 18000;

// Longest question accepted. Long enough for a real multi-sentence question, short enough that the
// prompt cost per request has a fixed ceiling. Rejected rather than truncated: silently answering a
// different question than the one asked is worse than saying no.
const MAX_QUERY_CHARS = 1000;

// Longest raw body accepted, before parsing. Guards the JSON parser itself — MAX_QUERY_CHARS only
// applies once a body has already been read and parsed.
const MAX_BODY_BYTES = 4096;

// ── Rate limiting: PER VISITOR, and it has to happen HERE ─────────────────────────────────────────
//
// archon rate-limits per client. Behind this proxy it cannot: every visitor arrives on Cloudflare's
// egress, so archon would see the whole public internet as ONE client sharing one bucket — the first
// few visitors each minute would consume the budget and everyone else, including the ops dashboard,
// would be locked out. Proxying MOVES the responsibility for per-visitor limiting to the proxy. This is
// that limiter.
//
// Budgets match archon's own: 3/min and 100/day. At the per-minute rate the daily budget is reachable
// in about half an hour of sustained asking, which is far past any genuine reader of a FAQ.
const PER_MINUTE = 3;
const PER_DAY = 100;

// A SECOND ceiling, keyed on IP alone. The fingerprint makes the key finer so that two people behind one
// NAT do not share a budget — but it is client-supplied and therefore forgeable, and a forgeable
// component in a rate-limit key is an evasion lever: rotate the fingerprint, get a fresh budget. So the
// IP-only bucket is the real ceiling and cannot be evaded from a single address; the finer bucket only
// ever hands out a SUBSET of it. Set well above PER_DAY so ordinary shared connections (offices, campus
// wifi, mobile CGNAT) are unaffected.
const PER_DAY_PER_IP = 400;

// KV is eventually consistent, so a burst of simultaneous requests can read the same counter and each
// write back the same increment — the effective limit can overshoot slightly under a deliberate flood.
// That is an accepted trade: the purpose here is bounding cost, not enforcing a security boundary, and
// the alternative (a Durable Object per visitor) is a great deal of machinery for a FAQ. The IP ceiling
// above bounds how far an overshoot can go.
const MINUTE_TTL_SECS = 120;
const DAY_TTL_SECS = 172800;

// The bindings this Function cannot run without. Named here so the failure message can say exactly
// what an operator has to configure, rather than making them read the source.
const ASSISTANT_BINDINGS = [
  // Cloudflare Access service token — the credential that reaches archon.
  "ARCHON_ACCESS_CLIENT_ID",
  "ARCHON_ACCESS_CLIENT_SECRET",
];

// Stable, non-reversible bucket id. The visitor's IP is hashed rather than stored: a KV namespace full
// of raw IP addresses next to question traffic is a log of who asked something, which is exactly what a
// privacy-first product should not keep. A hash is enough to count against.
async function bucketKey(parts) {
  const data = new TextEncoder().encode(parts.join("\u0000"));
  const digest = await crypto.subtle.digest("SHA-256", data);
  return [...new Uint8Array(digest)].slice(0, 16).map((b) => b.toString(16).padStart(2, "0")).join("");
}

// Increment one counter and report whether it is now over its cap. Read-modify-write on KV; see the
// eventual-consistency note above.
async function overLimit(kv, key, cap, ttl) {
  const current = Number(await kv.get(key)) || 0;
  if (current >= cap) {
    return true;
  }
  await kv.put(key, String(current + 1), { expirationTtl: ttl });
  return false;
}

// JSON response with no-store: an answer is per-user and must not be held by any shared cache.
function json(body, status) {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "Content-Type": "application/json; charset=utf-8",
      "Cache-Control": "no-store",
    },
  });
}

// Which required bindings are absent. Returns names, never values.
function missingBindings(env) {
  const missing = [];
  for (const name of ASSISTANT_BINDINGS) {
    if (!env[name]) {
      missing.push(name);
    }
  }
  // The KV namespace backing the rate limiter is a binding object, so it is checked by shape.
  if (!env.ASSISTANT_KV || typeof env.ASSISTANT_KV.get !== "function" || typeof env.ASSISTANT_KV.put !== "function") {
    missing.push("ASSISTANT_KV");
  }
  return missing;
}

export async function onRequestPost(context) {
  const { request, env } = context;

  // Fail closed before anything else: no token or no limiter means no proxying, full stop.
  const missing = missingBindings(env);
  if (missing.length > 0) {
    // The names are configuration keys, not secrets, and naming them is the difference between a
    // five-second fix and an afternoon. Values are never echoed.
    console.error("assistant proxy disabled — unbound: " + missing.join(", "));
    return json({ error: "assistant_unconfigured" }, 503);
  }

  // Rate limit before the body is read — a flood should cost as little as possible.
  //
  // CF-Connecting-IP is set by the edge and cannot be forged by the client. The fingerprint is
  // client-supplied and freely forgeable, which is why it only ever narrows a bucket and never widens
  // one: see PER_DAY_PER_IP.
  const ip = request.headers.get("CF-Connecting-IP") || "unknown";
  const fingerprint = (request.headers.get("X-Pollis-Assistant-Fingerprint") || "none").slice(0, 128);
  const now = Date.now();
  const minuteWindow = Math.floor(now / 60000);
  const dayWindow = Math.floor(now / 86400000);

  const visitor = await bucketKey([ip, fingerprint]);
  const address = await bucketKey([ip]);

  // The IP ceiling is checked FIRST: it is the one an attacker cannot rotate around, so it should be
  // what stops them, and it should not be reachable only after the finer buckets have been consumed.
  if (
    (await overLimit(env.ASSISTANT_KV, `d:${dayWindow}:ip:${address}`, PER_DAY_PER_IP, DAY_TTL_SECS)) ||
    (await overLimit(env.ASSISTANT_KV, `m:${minuteWindow}:v:${visitor}`, PER_MINUTE, MINUTE_TTL_SECS)) ||
    (await overLimit(env.ASSISTANT_KV, `d:${dayWindow}:v:${visitor}`, PER_DAY, DAY_TTL_SECS))
  ) {
    return json({ error: "rate_limited" }, 429);
  }

  // Reject an oversized body on the declared length before reading it.
  const declared = Number(request.headers.get("Content-Length") || "0");
  if (declared > MAX_BODY_BYTES) {
    return json({ error: "body_too_large" }, 413);
  }

  let raw;
  try {
    raw = await request.text();
  } catch {
    return json({ error: "unreadable_body" }, 400);
  }
  // Re-check after reading: Content-Length is a claim, the body is the fact.
  if (raw.length > MAX_BODY_BYTES) {
    return json({ error: "body_too_large" }, 413);
  }

  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return json({ error: "invalid_json" }, 400);
  }

  const query = parsed && typeof parsed.query === "string" ? parsed.query.trim() : "";
  if (query.length === 0) {
    return json({ error: "missing_query" }, 400);
  }
  if (query.length > MAX_QUERY_CHARS) {
    return json({ error: "query_too_long" }, 413);
  }

  // Rebuild the upstream body from the VALIDATED query rather than forwarding the caller's bytes, so
  // no unexpected field a caller invented can reach archon. `history` is always empty: the website
  // assistant is single-shot, and forwarding a caller-supplied conversation would let anyone inflate
  // the prompt (and the bill) at will.
  const base = env.ARCHON_BASE || ARCHON_BASE_DEFAULT;
  let upstream;
  try {
    upstream = await fetch(base + "/api/ask", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        // The Access service token. This pair is the ONLY reason this request gets past Access, and
        // it exists nowhere the browser can see.
        "CF-Access-Client-Id": env.ARCHON_ACCESS_CLIENT_ID,
        "CF-Access-Client-Secret": env.ARCHON_ACCESS_CLIENT_SECRET,
      },
      body: JSON.stringify({ question: query, history: [] }),
      signal: AbortSignal.timeout(ARCHON_TIMEOUT_MS),
    });
  } catch {
    // Timeout, DNS, TLS, connection reset — all the same to the caller: no answer, use the fallback.
    return json({ error: "upstream_unreachable" }, 502);
  }

  // A 302 here means Access rejected the service token (an expired, revoked, or wrong-audience token
  // redirects to the login page rather than returning 401). Surfacing that as its own status makes a
  // broken token look different in logs from archon simply being down.
  if (upstream.status === 302 || upstream.status === 301) {
    console.error("assistant proxy: Access rejected the service token (redirect to login)");
    return json({ error: "upstream_auth_failed" }, 502);
  }

  // A non-2xx upstream is reported as a plain 502 rather than mirrored: archon's status codes are its
  // own business, and the browser only needs "no answer, use the fallback".
  if (!upstream.ok) {
    return json({ error: "upstream_error" }, 502);
  }

  let answer;
  try {
    const parsed = JSON.parse(await upstream.text());
    answer = parsed && typeof parsed.answer === "string" ? parsed.answer : "";
  } catch {
    return json({ error: "upstream_malformed" }, 502);
  }
  if (answer.trim().length === 0) {
    return json({ error: "upstream_empty" }, 502);
  }

  // Only the answer travels. `usage` (token counts) is internal cost data; upstream headers are not
  // forwarded either, so nothing archon sets — cookies, cache directives, Access artifacts — can leak
  // to the browser.
  //
  // No `sources`: archon does not return citations. The on-device fallback does, so a remote answer is
  // uncited where a local one is not — recorded here because the panel's disclaimer points users at the
  // linked pages as authoritative, and for remote answers there are none to link.
  return json({ answer }, 200);
}
