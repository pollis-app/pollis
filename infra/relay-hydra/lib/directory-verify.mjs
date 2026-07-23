// The client's verification path for the signed directory (§3 frozen contract),
// in one place. The reconciler signs; this verifies. scripts/verify-directory.mjs
// runs it against the live URL; test/directory-contract.test.mjs runs it offline
// with generated keys + tampering, proving the contract end to end.
//
// Byte-for-byte discipline: verify the Ed25519 signature over the EXACT bytes we
// base64-decode from payload_b64, THEN parse those bytes. No canonicalization.

import { createPublicKey, verify } from "node:crypto";

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
