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
// WIRE FORMAT: NOT HERE. This is a transparent pass-through — it forwards the request body and returns
// archon's status and body unchanged. The archon request/response shape is asserted in exactly one place,
// `archonAdapter()` in website/assistant.js, and adding a second copy here is precisely how the two would
// drift. This file cares about auth, limits and validation; it does not care what an answer looks like.
//
// It also does not log, store, or cache question text. A privacy-first product proxying its users'
// questions should retain nothing by default; response caching would cut cost but is a separate,
// deliberate decision rather than something to slip in here.

// Where archon actually lives. Overridable per-environment (preview vs production) via a Pages var,
// so a preview deployment can be pointed at a staging archon without a code change.
const ARCHON_BASE_DEFAULT = "https://archon.pollis.com";

// Hard ceiling on the upstream round trip. Must stay UNDER the browser's own ARCHON_TIMEOUT_MS in
// website/assistant.js (4s) — if this were the slower of the two, the browser would abandon the request
// first and the user would wait the full client timeout to be told nothing, while this Function kept a
// paid generation running that nobody would ever read.
const ARCHON_TIMEOUT_MS = 3500;

// Longest question accepted. Long enough for a real multi-sentence question, short enough that the
// prompt cost per request has a fixed ceiling. Rejected rather than truncated: silently answering a
// different question than the one asked is worse than saying no.
const MAX_QUERY_CHARS = 1000;

// Longest raw body accepted, before parsing. Guards the JSON parser itself — MAX_QUERY_CHARS only
// applies once a body has already been read and parsed.
const MAX_BODY_BYTES = 4096;

// The bindings this Function cannot run without. Named here so the failure message can say exactly
// what an operator has to configure, rather than making them read the source.
const ASSISTANT_BINDINGS = [
  // Cloudflare Access service token — the credential that reaches archon.
  "ARCHON_ACCESS_CLIENT_ID",
  "ARCHON_ACCESS_CLIENT_SECRET",
];

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
  // The rate limiter is a binding object rather than a string, so it is checked by shape.
  if (!env.ASSISTANT_RATE_LIMIT || typeof env.ASSISTANT_RATE_LIMIT.limit !== "function") {
    missing.push("ASSISTANT_RATE_LIMIT");
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

  // Rate limit per client IP, before the body is read — a flood should cost as little as possible.
  // CF-Connecting-IP is set by the edge and cannot be spoofed by the client.
  const ip = request.headers.get("CF-Connecting-IP") || "unknown";
  const { success } = await env.ASSISTANT_RATE_LIMIT.limit({ key: ip });
  if (!success) {
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
  // no unexpected field a caller invented can reach archon.
  const base = env.ARCHON_BASE || ARCHON_BASE_DEFAULT;
  let upstream;
  try {
    upstream = await fetch(base + "/query", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        // The Access service token. This pair is the ONLY reason this request gets past Access, and
        // it exists nowhere the browser can see.
        "CF-Access-Client-Id": env.ARCHON_ACCESS_CLIENT_ID,
        "CF-Access-Client-Secret": env.ARCHON_ACCESS_CLIENT_SECRET,
      },
      body: JSON.stringify({ query }),
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

  // Pass the upstream body through untouched — the wire contract is asserted in assistant.js, not here.
  // Only the status and the body travel; upstream headers are deliberately not forwarded, so nothing
  // archon sets (cookies, cache directives, Access artifacts) leaks to the browser.
  const body = await upstream.text();
  return new Response(body, {
    status: upstream.status,
    headers: {
      "Content-Type": "application/json; charset=utf-8",
      "Cache-Control": "no-store",
    },
  });
}
