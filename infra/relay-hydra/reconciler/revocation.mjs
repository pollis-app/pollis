// Live relay revocation — the PRODUCER half (#813 Phase C).
//
// A relay is trusted while it sits in the ~1h-TTL directory; dropping it from relays[] only helps clients that re-fetch.
//
// THE MECHANISM. A second signed artifact — `revocations.json` — published beside
// the directory, signed by the SAME Ed25519 key, with a much shorter TTL
// (default 5 min) and re-signed on every 2-minute reconcile. Clients and relays
// must hold a fresh one to use any relay at all, so the exposure window is the
// REVOCATION TTL, not the directory TTL. Long-lived membership, short-lived
// safety: exactly the certificate/CRL split, and for the same reason — the
// directory's long TTL is a deliberate availability property (a wedged reconciler
// must not take the pool down) and must not be traded away for freshness.
//
// seq is the parameter's SSM Version (server-side, monotonic); clients reject lower. Never DELETE the parameter: it resets to 1.
// The directory anchors it (`revocation: { seq, path, count }`), so a fresh directory cannot pair with a stale list.
// Pure functions (index.mjs cannot be imported under test), covered by test/revocation-contract.test.mjs.

import { createHash } from "node:crypto";

export const REVOCATION_TYPE = "pollis-relay-revocations";
export const REVOCATION_VERSION = 1;

// Parse the operator-authored revocation parameter.
//
// Accepted value shape (the whole parameter):
//   { "revoked": [ <entry>, ... ] }
// with each entry carrying at least one selector:
//   { "addr": "203.0.113.7:9444" }      exact advertised endpoint
//   { "ip": "203.0.113.7" }             sugar — expanded to ip:relayPort
//   { "cert_sha256_b64": "<32-byte digest, base64>" }
// plus optional, purely descriptive `reason` and `revoked_at`.
//
// THROWS on anything it cannot fully understand: half a revocation reads as "these relays are fine".
export function parseRevocationSet(raw, relayPort) {
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch (err) {
    throw new Error(`revocation parameter is not valid JSON: ${err?.message ?? err}`);
  }
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error("revocation parameter must be a JSON object");
  }
  const list = parsed.revoked ?? [];
  if (!Array.isArray(list)) {
    throw new Error("revocation parameter's `revoked` must be an array");
  }

  const revoked = [];
  for (const [i, entry] of list.entries()) {
    if (entry === null || typeof entry !== "object" || Array.isArray(entry)) {
      throw new Error(`revoked[${i}] is not an object`);
    }
    const out = {};
    const addr = nonEmpty(entry.addr);
    const ip = nonEmpty(entry.ip);
    const digest = nonEmpty(entry.cert_sha256_b64);

    if (addr !== null && ip !== null) {
      throw new Error(`revoked[${i}] sets both addr and ip — pick one`);
    }
    if (addr !== null) {
      out.addr = normalizeAddr(addr);
    } else if (ip !== null) {
      out.addr = normalizeAddr(`${ip}:${relayPort}`);
    }
    if (out.addr !== undefined && !out.addr.includes(":")) {
      throw new Error(`revoked[${i}] addr ${JSON.stringify(out.addr)} is not host:port`);
    }
    if (digest !== null) {
      // A digest that is not a 32-byte SHA-256 can never match a real cert, so
      // it is an unenforceable revocation, not a harmless one.
      const bytes = Buffer.from(digest, "base64");
      if (bytes.length !== 32) {
        throw new Error(`revoked[${i}] cert_sha256_b64 is not a 32-byte SHA-256`);
      }
      out.cert_sha256_b64 = bytes.toString("base64");
    }
    if (out.addr === undefined && out.cert_sha256_b64 === undefined) {
      throw new Error(`revoked[${i}] has no selector (need addr, ip or cert_sha256_b64)`);
    }
    if (nonEmpty(entry.reason) !== null) {
      out.reason = nonEmpty(entry.reason);
    }
    if (Number.isInteger(entry.revoked_at)) {
      out.revoked_at = entry.revoked_at;
    }
    revoked.push(out);
  }
  return { revoked };
}

// Does this revocation set cover a relay we were about to advertise?
// `relay` is a directory entry candidate: { addr, cert_b64 }.
export function revokesRelay(revoked, relay) {
  const addr = typeof relay.addr === "string" ? normalizeAddr(relay.addr) : null;
  const digest =
    typeof relay.cert_b64 === "string" && relay.cert_b64.length > 0
      ? certDigestB64(Buffer.from(relay.cert_b64, "base64"))
      : null;
  return revoked.some(
    (e) =>
      (e.addr !== undefined && e.addr === addr) ||
      (e.cert_sha256_b64 !== undefined && e.cert_sha256_b64 === digest),
  );
}

// The exact object the reconciler signs into revocations.json.
export function buildRevocationPayload({ seq, revoked, issuedAt, ttlSeconds }) {
  if (!Number.isInteger(seq) || seq < 0) {
    throw new Error(`revocation seq must be a non-negative integer, got ${seq}`);
  }
  return {
    version: REVOCATION_VERSION,
    type: REVOCATION_TYPE,
    seq,
    issued_at: issuedAt,
    expires_at: issuedAt + ttlSeconds,
    revoked,
  };
}

// The additive, optional anchor the directory carries. Returns null when
// revocation is not configured for this pool, in which case the directory is
// byte-identical to the pre-#813 shape.
export function directoryAnchor({ seq, path, count }) {
  if (seq === null || seq === undefined) {
    return null;
  }
  return { seq, path, count };
}

// How long the directory itself should be valid for.
// While ANY relay is revoked, cut the TTL to `activeTtl`: the only lever for already-shipped clients (fail-closed).
export function directoryTtlFor({ revokedCount, baseTtl, activeTtl }) {
  if (revokedCount > 0) {
    return Math.min(baseTtl, activeTtl);
  }
  return baseTtl;
}

// base64(SHA-256(DER)) — the canonical cert_sha256_b64 form.
export function certDigestB64(certDer) {
  return createHash("sha256").update(certDer).digest("base64");
}

function normalizeAddr(addr) {
  return String(addr).trim().toLowerCase();
}

function nonEmpty(v) {
  if (typeof v !== "string") {
    return null;
  }
  const trimmed = v.trim();
  return trimmed.length === 0 ? null : trimmed;
}
