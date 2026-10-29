// Client verification of the signed directory (§3 frozen contract); tested offline by test/directory-contract.test.mjs.
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

// The artifact name a directory identifies itself by — the mirror of
// REVOCATION_TYPE in revocation-verify.mjs, and of DIRECTORY_TYPE in
// pollis-core's net::directory.
//
// One key signs both artifacts, so a signature proves only who signed, never
// WHICH artifact was signed: an on-path attacker can serve the revocation list
// where the directory is expected and both signatures verify.
//
// The guard is ONE-SIDED: a wrong `type` is rejected, an absent one accepted (requiring it would make deploy order an outage).
export const DIRECTORY_TYPE = "pollis-relay-directory";

// Mirrors the client: REJECT (fail closed) on bad signature, version != 1, a
// `type` that is present and not DIRECTORY_TYPE, now >= expires_at, malformed
// JSON, or empty relays. Returns the Directory on success. `nowSeconds` is
// injectable so tests can exercise expiry.
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
  // Cross-artifact confusion guard — see DIRECTORY_TYPE. Before the range checks
  // so a foreign artifact is rejected as what it is, not as "expired".
  if (directory.type !== undefined && directory.type !== null && directory.type !== DIRECTORY_TYPE) {
    throw new DirectoryRejected(`wrong artifact type ${JSON.stringify(directory.type)}`);
  }
  if (typeof directory.expires_at !== "number" || nowSeconds >= directory.expires_at) {
    throw new DirectoryRejected("expired");
  }
  if (!Array.isArray(directory.relays) || directory.relays.length === 0) {
    throw new DirectoryRejected("empty relays");
  }

  return directory;
}

// Revocation anchoring (#813 Phase C); `version` stays 1.
//
// The §3 payload gained ONE additive, optional top-level field:
//
//   "revocation": { "seq": <int>, "path": "revocations.json", "count": <int> }
// Carrying the seq INSIDE the signed directory stops a fresh directory being paired with a stale list.

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
// The only safe answer to "I don't know" is the same as the answer to "revoked".
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

// Peer-hosted relays (#813 wave 3); `version` stays 1.
//
// The §3 payload gained a SECOND additive, optional top-level field:
//
//   "peers": [ { "cert_b64": "<peer relay leaf DER>", "parked_at": ["ip:port"] } ]
// ABSENT MEANS NONE. There is no "unknown" state to represent: a directory with
// no `peers` key is a directory with no peer relays, which is the shape the pool
// published before this existed. Hence an array with an empty default rather than
// a nullable field — the fail-closed answer and the default are the same value.

// Usable peer entries, normalized; unusable ones are DROPPED (a middle hop only costs a shorter path).
export function directoryPeers(directory) {
  if (!Array.isArray(directory?.peers)) {
    return [];
  }
  const out = [];
  for (const entry of directory.peers) {
    if (entry === null || typeof entry !== "object" || Array.isArray(entry)) {
      continue;
    }
    if (typeof entry.cert_b64 !== "string" || entry.cert_b64.length === 0) {
      continue;
    }
    const parkedAt = Array.isArray(entry.parked_at)
      ? entry.parked_at.filter((a) => typeof a === "string" && a.length > 0)
      : [];
    // A peer never listens for inbound connections, so one parked at no relay in
    // this directory is not reachable by any circuit we could build.
    const reachable = parkedAt.filter((a) => directory.relays.some((r) => r.addr === a));
    if (reachable.length === 0) {
      continue;
    }
    out.push({ cert_b64: entry.cert_b64, parked_at: reachable });
  }
  return out;
}

// The peers a client may actually use: the reachable ones, minus anything the
// revocation list covers. Belt and braces with the reconciler, which excludes a
// revoked peer at publication — this is what protects a client still holding an
// older directory.
//
// Mirrors pollis-core's RelayAdmission, incl. its carve-out: with no anchor (or count 0), unusable evidence means no revocations.
export function admitPeers(directory, list, nowSeconds = Math.floor(Date.now() / 1000)) {
  const peers = directoryPeers(directory);
  if (peers.length === 0) {
    return [];
  }
  const anchor = revocationAnchor(directory);
  const required = anchor !== null && anchor.count !== 0;
  const evaluable =
    list !== null &&
    list !== undefined &&
    nowSeconds < list.expires_at &&
    (anchor === null || list.seq >= anchor.seq);
  if (!evaluable) {
    return required ? [] : peers;
  }
  return peers.filter((p) => !list.certDigests.has(sha256B64(Buffer.from(p.cert_b64, "base64"))));
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
