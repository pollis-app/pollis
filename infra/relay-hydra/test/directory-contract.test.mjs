// End-to-end proof of the §3 directory contract: sign exactly as the reconciler
// does, then run the client's verification path (lib/directory-verify.mjs) over
// it — including every documented rejection case. No AWS, no network.
//
// Run: node --test   (from infra/relay-hydra/)

import { test } from "node:test";
import assert from "node:assert/strict";
import { generateKeyPairSync, sign } from "node:crypto";
import { verifyDirectory, DirectoryRejected, directoryPeers, admitPeers } from "../lib/directory-verify.mjs";
import { createHash } from "node:crypto";

// A fresh signing keypair, and its raw 32-byte public key as base64 — exactly
// the POLLIS_OVERLAY_DIRECTORY_KEY the client pins.
const { publicKey, privateKey } = generateKeyPairSync("ed25519");
const pubRawB64 = publicKey.export({ format: "der", type: "spki" }).subarray(-32).toString("base64");

// Sign a Directory object the way reconciler/index.mjs does: sign the exact UTF-8
// bytes we base64 into payload_b64.
function signEnvelope(directory, key = privateKey) {
  const payloadBytes = Buffer.from(JSON.stringify(directory), "utf8");
  const signature = sign(null, payloadBytes, key);
  return JSON.stringify({
    payload_b64: payloadBytes.toString("base64"),
    signature_b64: signature.toString("base64"),
  });
}

const now = 1_737_600_000;
function freshDirectory(overrides = {}) {
  return {
    version: 1,
    issued_at: now,
    expires_at: now + 3600,
    relays: [
      { addr: "203.0.113.7:9444", region: "us-west-2", cert_b64: "ZHVtbXktY2VydA==" },
    ],
    ...overrides,
  };
}

test("valid directory verifies and parses", () => {
  const dir = verifyDirectory(signEnvelope(freshDirectory()), pubRawB64, now);
  assert.equal(dir.version, 1);
  assert.equal(dir.relays.length, 1);
  assert.equal(dir.relays[0].addr, "203.0.113.7:9444");
});

test("byte-for-byte: verifier decodes the exact signed bytes", () => {
  // Reproduce the client's decode and confirm it round-trips the signer's bytes.
  const envelope = JSON.parse(signEnvelope(freshDirectory()));
  const decoded = Buffer.from(envelope.payload_b64, "base64").toString("utf8");
  assert.equal(decoded, JSON.stringify(freshDirectory()));
});

test("rejects a tampered payload", () => {
  const envelope = JSON.parse(signEnvelope(freshDirectory()));
  // Flip the signed payload but keep the old signature.
  const tampered = freshDirectory({ relays: [{ addr: "evil.example:9444", region: "us-west-2", cert_b64: "eA==" }] });
  envelope.payload_b64 = Buffer.from(JSON.stringify(tampered), "utf8").toString("base64");
  assert.throws(() => verifyDirectory(JSON.stringify(envelope), pubRawB64, now), DirectoryRejected);
});

test("rejects a signature from the wrong key", () => {
  const { privateKey: attacker } = generateKeyPairSync("ed25519");
  assert.throws(() => verifyDirectory(signEnvelope(freshDirectory(), attacker), pubRawB64, now), /bad signature/);
});

test("rejects an expired directory", () => {
  const env = signEnvelope(freshDirectory({ expires_at: now - 1 }));
  assert.throws(() => verifyDirectory(env, pubRawB64, now), /expired/);
});

test("rejects version != 1", () => {
  const env = signEnvelope(freshDirectory({ version: 2 }));
  assert.throws(() => verifyDirectory(env, pubRawB64, now), /unsupported version/);
});

test("rejects empty relays", () => {
  const env = signEnvelope(freshDirectory({ relays: [] }));
  assert.throws(() => verifyDirectory(env, pubRawB64, now), /empty relays/);
});

test("rejects malformed envelope JSON", () => {
  assert.throws(() => verifyDirectory("{not json", pubRawB64, now), /malformed envelope/);
});

// ── Backward compatibility of the FROZEN §3 contract (#813 Phase C) ──────────
//
// The revocation work added ONE optional top-level field to the payload. The
// §3 contract is shared with already-shipped client binaries we cannot update,
// so the tests below are the guarantee that they keep working — not a nice-to-
// have. If any of them ever fails, the change must not ship.

test("OLD CLIENT: a revocation-carrying directory still verifies unchanged", () => {
  // This is verbatim the pre-#813 verification path — the one baked into shipped
  // builds. It reads version/expires_at/relays and nothing else.
  const dir = verifyDirectory(
    signEnvelope(freshDirectory({ revocation: { seq: 42, path: "revocations.json", count: 1 } })),
    pubRawB64,
    now,
  );
  assert.equal(dir.version, 1, "version stays 1 — bumping it would make every shipped client reject");
  assert.equal(dir.relays.length, 1);
  assert.equal(dir.relays[0].addr, "203.0.113.7:9444");
  assert.equal(dir.relays[0].cert_b64, "ZHVtbXktY2VydA==");
});

test("OLD CLIENT: relay entries gained no new required fields", () => {
  // A shipped client deserializes each entry into a fixed struct. Revocation is
  // carried at the TOP level precisely so entries stay exactly as they were.
  const dir = verifyDirectory(
    signEnvelope(freshDirectory({ revocation: { seq: 42, path: "revocations.json", count: 0 } })),
    pubRawB64,
    now,
  );
  assert.deepEqual(Object.keys(dir.relays[0]).sort(), ["addr", "cert_b64", "region"]);
});

test("OLD CLIENT: unknown future payload fields are ignored, not rejected", () => {
  // The same property, generalized: additive extension is the ONLY way this
  // contract may grow.
  const dir = verifyDirectory(
    signEnvelope(freshDirectory({ some_field_from_2027: { nested: true } })),
    pubRawB64,
    now,
  );
  assert.equal(dir.relays.length, 1);
});

// ── Peer-hosted relays: the SECOND additive field (#813 wave 3) ──────────────
//
// Everything above is the guarantee phase C established for `revocation`. These
// re-establish it for `peers`, because "we did it right last time" is not a test.

const PEER_CERT_B64 = Buffer.from("peer-leaf-der").toString("base64");
const PEER_DIGEST_B64 = createHash("sha256").update(Buffer.from("peer-leaf-der")).digest("base64");

function withPeers(overrides = {}) {
  return freshDirectory({
    peers: [{ cert_b64: PEER_CERT_B64, parked_at: ["203.0.113.7:9444"] }],
    ...overrides,
  });
}

test("OLD CLIENT: a peers-carrying directory still verifies unchanged", () => {
  // Verbatim the pre-#813 verification path — the one baked into shipped builds.
  const dir = verifyDirectory(signEnvelope(withPeers()), pubRawB64, now);
  assert.equal(dir.version, 1, "version stays 1 — bumping it would make every shipped client reject");
  assert.equal(dir.relays.length, 1);
  assert.equal(dir.relays[0].addr, "203.0.113.7:9444");
  assert.equal(dir.relays[0].cert_b64, "ZHVtbXktY2VydA==");
});

test("OLD CLIENT: peers ride at the TOP level, so relay entries gain no new fields", () => {
  const dir = verifyDirectory(signEnvelope(withPeers()), pubRawB64, now);
  assert.deepEqual(Object.keys(dir.relays[0]).sort(), ["addr", "cert_b64", "region"]);
});

test("OLD CLIENT: both additive fields together still verify", () => {
  const dir = verifyDirectory(
    signEnvelope(withPeers({ revocation: { seq: 42, path: "revocations.json", count: 0 } })),
    pubRawB64,
    now,
  );
  assert.equal(dir.version, 1);
  assert.equal(dir.relays.length, 1);
});

test("a directory with no parked peers is byte-identical to the old shape", () => {
  // The reconciler omits the key entirely when there is nothing to say, so a pool
  // with no consenting devices publishes exactly what it published before #813.
  const before = JSON.stringify(freshDirectory());
  const envelope = JSON.parse(signEnvelope(freshDirectory()));
  assert.equal(Buffer.from(envelope.payload_b64, "base64").toString("utf8"), before);
  assert.equal(JSON.parse(before).peers, undefined);
});

test("absent peers means NO peers, never unknown", () => {
  assert.deepEqual(directoryPeers(verifyDirectory(signEnvelope(freshDirectory()), pubRawB64, now)), []);
});

test("a peer parked at no relay in this directory is dropped as unreachable", () => {
  // A peer never listens for inbound connections — it is reachable only through a
  // relay that holds its parked link, so one naming no live relay is unusable.
  const dir = verifyDirectory(
    signEnvelope(freshDirectory({ peers: [{ cert_b64: PEER_CERT_B64, parked_at: ["198.51.100.9:9444"] }] })),
    pubRawB64,
    now,
  );
  assert.deepEqual(directoryPeers(dir), []);
  const dirNoParking = verifyDirectory(
    signEnvelope(freshDirectory({ peers: [{ cert_b64: PEER_CERT_B64 }] })),
    pubRawB64,
    now,
  );
  assert.deepEqual(directoryPeers(dirNoParking), []);
});

test("a revoked peer is rejected CLIENT-side even when the directory still lists it", () => {
  // Belt and braces: the reconciler excludes revoked peers at publication, but a
  // client holding an older directory must refuse the same peer on its own.
  const dir = verifyDirectory(signEnvelope(withPeers({ revocation: { seq: 3, count: 1 } })), pubRawB64, now);
  assert.equal(directoryPeers(dir).length, 1, "the directory does still advertise it");

  const list = { seq: 3, expires_at: now + 300, addrs: new Set(), certDigests: new Set([PEER_DIGEST_B64]) };
  assert.deepEqual(admitPeers(dir, list, now), [], "a revoked peer was admitted");

  // ...and an unrelated revocation leaves it usable.
  const other = { seq: 3, expires_at: now + 300, addrs: new Set(), certDigests: new Set(["Zm9v"]) };
  assert.equal(admitPeers(dir, other, now).length, 1);
});

test("peers fail closed when revocation cannot be evaluated", () => {
  const dir = verifyDirectory(signEnvelope(withPeers({ revocation: { seq: 7, count: 1 } })), pubRawB64, now);
  const fresh = { seq: 7, expires_at: now + 300, addrs: new Set(), certDigests: new Set() };
  assert.equal(admitPeers(dir, fresh, now).length, 1);
  // No list at all, an expired one, and one behind the anchor all mean "cannot
  // evaluate", which resolves identically to "revoked".
  assert.deepEqual(admitPeers(dir, null, now), []);
  assert.deepEqual(admitPeers(dir, { ...fresh, expires_at: now }, now), []);
  assert.deepEqual(admitPeers(dir, { ...fresh, seq: 6 }, now), []);
});

test("the deploy-order carve-out is as narrow for peers as it is for relays", () => {
  // A client that ships before the reconciler publishes revocations.json must not
  // fail closed fleet-wide on day one — so while the directory anchors NOTHING (or
  // anchors count == 0), a missing list means "no revocations known".
  const noAnchor = verifyDirectory(signEnvelope(withPeers()), pubRawB64, now);
  assert.equal(admitPeers(noAnchor, null, now).length, 1);
  const zeroCount = verifyDirectory(signEnvelope(withPeers({ revocation: { seq: 5, count: 0 } })), pubRawB64, now);
  assert.equal(admitPeers(zeroCount, null, now).length, 1);
  // ...and an anchor whose count this build cannot read is ambiguity, not
  // permission: it enforces.
  const unreadableCount = verifyDirectory(
    signEnvelope(withPeers({ revocation: { seq: 5, count: "lots" } })),
    pubRawB64,
    now,
  );
  assert.deepEqual(admitPeers(unreadableCount, null, now), []);
});

test("a directory with revocation NOT configured is byte-identical to the old shape", () => {
  // When the pool has no revocation parameter the reconciler omits the field
  // entirely, so the published bytes match what it published before #813.
  const before = JSON.stringify(freshDirectory());
  const envelope = JSON.parse(signEnvelope(freshDirectory()));
  assert.equal(Buffer.from(envelope.payload_b64, "base64").toString("utf8"), before);
  assert.equal(JSON.parse(before).revocation, undefined);
});
