// The client's verification path for the signed **relay revocation list**
// (#813 Phase C), in one place — the sibling of lib/directory-verify.mjs.
// The reconciler signs; this verifies. scripts/verify-directory.mjs runs it
// against the live URL; test/revocation-contract.test.mjs runs it offline with
// generated keys, forgeries, replays and cross-artifact confusion.
//
// WHY A SECOND ARTIFACT AT ALL. The directory is the *availability* artifact: it
// carries a ~1h TTL on purpose, so a wedged reconciler does not take the pool
// down with it. That same hour is how long a seized relay stays trusted. You
// cannot fix that by shortening the directory TTL without trading the
// availability property away — so revocation gets its own artifact with its own,
// much shorter, life (default 5 min, re-signed every 2-min reconcile). Long-lived
// membership, short-lived safety: the same split as a certificate and its CRL.
//
// Byte-for-byte discipline, identical to the directory: verify the Ed25519
// signature over the EXACT bytes we base64-decode from payload_b64, THEN parse
// those bytes. No canonicalization.

import { createHash, verify } from "node:crypto";
import { publicKeyFromRaw } from "./directory-verify.mjs";

// The `type` discriminator every revocation payload carries. The directory and
// the revocation list are signed by the SAME Ed25519 key, so without a
// self-naming tag a genuinely-signed directory could be replayed at the
// revocation endpoint (and vice versa) and the signature check would pass.
export const REVOCATION_TYPE = "pollis-relay-revocations";
export const REVOCATION_VERSION = 1;

export class RevocationRejected extends Error {}

// Verify a revocation envelope. Mirrors pollis-relay's `verify_revocations` and
// pollis-core's directory verifier exactly.
//
// `seqFloor` is the lowest sequence number the caller accepts — pass
// max(highest seq already seen, the seq the signed directory anchored). Anything
// below it is a REPLAY: a genuinely-signed, not-yet-expired, but OLDER list that
// an on-path attacker serves to hide the newest revocation.
//
// Returns { seq, issued_at, expires_at, addrs:Set<string>, certDigests:Set<string>, count }.
// REJECTS (throws RevocationRejected) on: malformed JSON, bad signature,
// version != 1, wrong type, expired, seq below the floor, or any entry this
// build cannot enforce.
export function verifyRevocations(
  envelopeText,
  pinnedPublicKeyB64,
  nowSeconds = Math.floor(Date.now() / 1000),
  seqFloor = 0,
) {
  let envelope;
  try {
    envelope = JSON.parse(envelopeText);
  } catch {
    throw new RevocationRejected("malformed envelope JSON");
  }

  if (typeof envelope.payload_b64 !== "string" || typeof envelope.signature_b64 !== "string") {
    throw new RevocationRejected("envelope missing payload_b64/signature_b64");
  }

  const payloadBytes = Buffer.from(envelope.payload_b64, "base64");
  const signature = Buffer.from(envelope.signature_b64, "base64");
  const publicKey = publicKeyFromRaw(Buffer.from(pinnedPublicKeyB64, "base64"));

  if (!verify(null, payloadBytes, publicKey, signature)) {
    throw new RevocationRejected("bad signature");
  }

  let payload;
  try {
    payload = JSON.parse(payloadBytes.toString("utf8"));
  } catch {
    throw new RevocationRejected("malformed payload JSON");
  }

  if (payload.version !== REVOCATION_VERSION) {
    throw new RevocationRejected(`unsupported revocation version ${payload.version}`);
  }
  if (payload.type !== REVOCATION_TYPE) {
    throw new RevocationRejected(`wrong artifact type ${JSON.stringify(payload.type)}`);
  }
  if (typeof payload.expires_at !== "number" || nowSeconds >= payload.expires_at) {
    throw new RevocationRejected("expired");
  }
  if (!Number.isInteger(payload.seq) || payload.seq < 0) {
    throw new RevocationRejected("seq is not a non-negative integer");
  }
  if (payload.seq < seqFloor) {
    throw new RevocationRejected(`rolled back (seq ${payload.seq} < required ${seqFloor})`);
  }
  if (!Array.isArray(payload.revoked)) {
    throw new RevocationRejected("revoked is not an array");
  }

  const addrs = new Set();
  const certDigests = new Set();
  for (const entry of payload.revoked) {
    if (entry === null || typeof entry !== "object" || Array.isArray(entry)) {
      throw new RevocationRejected("revocation entry is not an object");
    }
    const addr = nonEmpty(entry.addr);
    const digest = nonEmpty(entry.cert_sha256_b64);
    // An entry this build cannot act on fails the WHOLE list closed rather than
    // being skipped. Skipping would silently downgrade a revocation published by
    // a newer producer (a selector we do not know yet) into an admission —
    // exactly the bypass revocation exists to prevent.
    if (addr === null && digest === null) {
      throw new RevocationRejected("revocation entry carries no addr and no cert_sha256_b64");
    }
    if (addr !== null) {
      addrs.add(normalizeAddr(addr));
    }
    if (digest !== null) {
      // A digest that is not 32 bytes can never match a real SHA-256, so it is
      // an unenforceable revocation, not a harmless one.
      const raw = Buffer.from(digest, "base64");
      if (raw.length !== 32) {
        throw new RevocationRejected("cert_sha256_b64 is not a 32-byte SHA-256");
      }
      certDigests.add(raw.toString("base64"));
    }
  }

  return {
    seq: payload.seq,
    issued_at: payload.issued_at,
    expires_at: payload.expires_at,
    addrs,
    certDigests,
    count: addrs.size + certDigests.size,
  };
}

// Does `list` revoke this relay? Either selector matching is enough.
//
// The ADDRESS covers the shared-pool-identity case: every hydra node currently
// presents the same pinned QUIC leaf, so a cert-keyed revocation would take the
// whole pool down at once. The CERT DIGEST covers per-node and peer-hosted
// identities (design §7), where the leaf really is the relay's name and the
// address is not.
export function revokes(list, { addr, cert_b64 }) {
  if (typeof addr === "string" && list.addrs.has(normalizeAddr(addr))) {
    return true;
  }
  if (typeof cert_b64 === "string" && cert_b64.length > 0) {
    return list.certDigests.has(certDigestB64(Buffer.from(cert_b64, "base64")));
  }
  return false;
}

// base64(SHA-256(DER)) — the cert_sha256_b64 selector's canonical form. Handy for
// operators minting a revocation entry from a node's advertised cert_b64.
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
