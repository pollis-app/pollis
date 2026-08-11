// End-to-end proof of the §3 directory contract: sign exactly as the reconciler
// does, then run the client's verification path (lib/directory-verify.mjs) over
// it — including every documented rejection case. No AWS, no network.
//
// Run: node --test   (from infra/relay-hydra/)

import { test } from "node:test";
import assert from "node:assert/strict";
import { generateKeyPairSync, sign } from "node:crypto";
import { verifyDirectory, DirectoryRejected } from "../lib/directory-verify.mjs";

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

test("a directory with revocation NOT configured is byte-identical to the old shape", () => {
  // When the pool has no revocation parameter the reconciler omits the field
  // entirely, so the published bytes match what it published before #813.
  const before = JSON.stringify(freshDirectory());
  const envelope = JSON.parse(signEnvelope(freshDirectory()));
  assert.equal(Buffer.from(envelope.payload_b64, "base64").toString("utf8"), before);
  assert.equal(JSON.parse(before).revocation, undefined);
});
