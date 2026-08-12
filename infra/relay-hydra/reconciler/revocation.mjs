// Live relay revocation — the PRODUCER half (#813 Phase C).
//
// THE PROBLEM. A relay is trusted for exactly as long as its entry sits in the
// signed directory, and the directory carries a ~1 hour TTL. So a seized or
// compromised node stays valid for up to an hour after we know it is bad, and
// nothing we do to the directory shortens that for a client that already holds
// one. Dropping the node from `relays[]` (which this module also does) only helps
// clients that re-fetch.
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
// THE SEQUENCE NUMBER IS SSM'S. `seq` is the revocation parameter's SSM
// `Version`, not an operator-authored field. Parameter Store increments it on
// every PutParameter, server-side, monotonically — so it is impossible for an
// operator to forget to bump it, impossible for two edits to collide on the same
// value, and it needs no extra state anywhere. Clients keep a high-water mark and
// reject anything lower, which is what stops a genuinely-signed but OLDER list
// being replayed to un-revoke a relay. (Corollary, in the runbook: never DELETE
// the parameter — recreating it resets Version to 1, which every client will
// reject as a rollback. Empty its `revoked` array instead.)
//
// THE DIRECTORY ANCHORS IT. The directory carries
// `revocation: { seq, path, count }` — one additive, optional field, with
// `version` still 1. That binds revocation freshness to directory freshness: an
// on-path attacker cannot pair a fresh directory with a stale-but-unexpired
// revocation list, because the directory it must serve states the minimum
// sequence. The field is invisible to already-shipped clients, which ignore
// unknown payload fields (see test/directory-contract.test.mjs).
//
// Pure functions, no AWS, no network — index.mjs injects the raw parameter value
// and the node set. This is the logic that must not misfire, so it lives here
// where it is unit-testable (test/revocation-contract.test.mjs); index.mjs cannot
// be imported under test (its AWS SDK deps only exist in the Lambda runtime).

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
// THROWS on anything it cannot fully understand. A revocation set we can only
// partially parse must never be signed: half a revocation reads to every client
// as "these relays are fine", which is the one outcome this feature must not
// produce. The caller treats a throw as "publish nothing this cycle" and lets the
// live artifacts expire, which fails closed on a 5-minute clock.
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
//
// While ANY relay is revoked, cut the directory TTL to `activeTtl` (default
// 5 min). This is the only lever that helps ALREADY-SHIPPED clients, which know
// nothing about revocations.json: a revoked node leaves `relays[]` on the next
// reconcile, and a short TTL means an old client's cached directory stops being
// usable in minutes rather than an hour. It is a deliberate fail-closed trade —
// if the reconciler wedges DURING an active revocation the pool goes dark in
// minutes instead of an hour, which is the correct direction while a node is
// known-compromised.
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
