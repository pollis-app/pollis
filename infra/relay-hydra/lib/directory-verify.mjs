// The client's verification path for the signed directory (§3 frozen contract),
// in one place. The reconciler signs; this verifies. scripts/verify-directory.mjs
// runs it against the live URL; test/directory-contract.test.mjs runs it offline
// with generated keys + tampering, proving the contract end to end.
//
// Byte-for-byte discipline: verify the Ed25519 signature over the EXACT bytes we
// base64-decode from payload_b64, THEN parse those bytes. No canonicalization.

import { createHash, createPublicKey, verify } from "node:crypto";

// SPKI DER prefix for an Ed25519 public key (RFC 8410); the raw 32-byte key
// follows. Lets us rebuild a KeyObject from the pinned raw public key.
const ED25519_SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");

export function publicKeyFromRaw(raw32) {
  if (raw32.length !== 32) {
    throw new Error(`expected 32-byte Ed25519 public key, got ${raw32.length}`);
  }
  const der = Buffer.concat([ED25519_SPKI_PREFIX, raw32]);
  return createPublicKey({ key: der, format: "der", type: "spki" });
}

export class DirectoryRejected extends Error {}

// Mirrors the client: REJECT (fail closed) on bad signature, version != 1,
// now >= expires_at, malformed JSON, or empty relays. Returns the Directory on
// success. `nowSeconds` is injectable so tests can exercise expiry.
export function verifyDirectory(envelopeText, pinnedPublicKeyB64, nowSeconds = Math.floor(Date.now() / 1000)) {
  let envelope;
  try {
    envelope = JSON.parse(envelopeText);
  } catch {
    throw new DirectoryRejected("malformed envelope JSON");
  }

  if (typeof envelope.payload_b64 !== "string" || typeof envelope.signature_b64 !== "string") {
    throw new DirectoryRejected("envelope missing payload_b64/signature_b64");
  }

  const payloadBytes = Buffer.from(envelope.payload_b64, "base64");
  const signature = Buffer.from(envelope.signature_b64, "base64");
  const publicKey = publicKeyFromRaw(Buffer.from(pinnedPublicKeyB64, "base64"));

  if (!verify(null, payloadBytes, publicKey, signature)) {
    throw new DirectoryRejected("bad signature");
  }

  let directory;
  try {
    directory = JSON.parse(payloadBytes.toString("utf8"));
  } catch {
    throw new DirectoryRejected("malformed payload JSON");
  }

  if (directory.version !== 1) {
    throw new DirectoryRejected(`unsupported version ${directory.version}`);
  }
  if (typeof directory.expires_at !== "number" || nowSeconds >= directory.expires_at) {
    throw new DirectoryRejected("expired");
  }
  if (!Array.isArray(directory.relays) || directory.relays.length === 0) {
    throw new DirectoryRejected("empty relays");
  }

  return directory;
}

// ─── Revocation anchoring (#813 Phase C) ────────────────────────────────────
//
// The §3 payload gained ONE additive, optional top-level field:
//
//   "revocation": { "seq": <int>, "path": "revocations.json", "count": <int> }
//
// `version` stays 1 and every existing field keeps its exact meaning, so an
// already-shipped client — which parses the payload with a struct that ignores
// unknown fields (pollis-core) or reads only version/expires_at/relays (this
// module) — is completely unaffected. That backward compatibility is not a hope;
// test/directory-contract.test.mjs pins it.
//
// The anchor is what makes revocation resistant to an on-path attacker. Both
// artifacts are signed by the same key and neither can be forged, but they can be
// PAIRED dishonestly: serve a fresh directory next to a stale-but-unexpired
// revocation list, and the newest revocation disappears. Carrying the sequence
// INSIDE the signed directory makes that pairing detectable — a client that
// accepted the directory has, by construction, also accepted a floor on how old
// the revocation list may be.

// The revocation anchor a directory carries, or null for a directory published
// before revocation existed (or by a pool that has not enabled it).
export function revocationAnchor(directory) {
  const anchor = directory?.revocation;
  if (anchor === null || typeof anchor !== "object" || Array.isArray(anchor)) {
    return null;
  }
  if (!Number.isInteger(anchor.seq) || anchor.seq < 0) {
    throw new DirectoryRejected("revocation anchor has a non-integer seq");
  }
  const path = typeof anchor.path === "string" && anchor.path.length > 0 ? anchor.path : "revocations.json";
  return { seq: anchor.seq, path, count: Number.isInteger(anchor.count) ? anchor.count : null };
}

// The client's fail-closed admission step: given a verified directory and a
// verified revocation list, return the relays that may actually be used.
//
// THROWS rather than returning a filtered set when the evidence is unusable:
//   - `list` is null/undefined      → could not evaluate revocation at all;
//   - the list has expired          → held evidence is no longer evidence;
//   - list.seq < the anchored seq   → a stale list paired with a fresh directory.
//
// Getting this backwards is the whole risk of the feature. Returning "all
// relays" when revocation cannot be evaluated turns the mechanism into
// decoration; the only safe answer to "I don't know" is the same as the answer
// to "revoked". The caller then has no usable relays, which in Prefer means a
// direct dial and in Strict a surfaced degrade — never a silent send over a
// relay that might be seized.
export function admitRelays(directory, list, nowSeconds = Math.floor(Date.now() / 1000)) {
  const anchor = revocationAnchor(directory);
  if (list === null || list === undefined) {
    throw new DirectoryRejected("no revocation list — cannot evaluate revocation (fail closed)");
  }
  if (nowSeconds >= list.expires_at) {
    throw new DirectoryRejected("revocation list expired (fail closed)");
  }
  if (anchor !== null && list.seq < anchor.seq) {
    throw new DirectoryRejected(
      `revocation list is behind the directory anchor (seq ${list.seq} < ${anchor.seq})`,
    );
  }
  const usable = directory.relays.filter((r) => !revokesRelay(list, r));
  if (usable.length === 0) {
    throw new DirectoryRejected("every advertised relay is revoked");
  }
  return usable;
}

// Kept local (rather than importing revocation-verify.mjs) so this module stays
// the single dependency the directory path needs; the matching rule is the same
// one pollis-relay's RevocationList::revokes implements.
function revokesRelay(list, relay) {
  if (typeof relay.addr === "string" && list.addrs.has(relay.addr.trim().toLowerCase())) {
    return true;
  }
  if (typeof relay.cert_b64 === "string" && relay.cert_b64.length > 0) {
    return list.certDigests.has(sha256B64(Buffer.from(relay.cert_b64, "base64")));
  }
  return false;
}

function sha256B64(bytes) {
  return createHash("sha256").update(bytes).digest("base64");
}
