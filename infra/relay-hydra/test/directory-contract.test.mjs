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
