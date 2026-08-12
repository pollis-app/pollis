// End-to-end proof of the live relay revocation contract (#813 Phase C): sign
// exactly as the reconciler does, then run the client's verification path over
// it — including every fail-closed case. No AWS, no network.
//
// The tests here are deliberately weighted toward the ways revocation can go
// WRONG, because the failure modes are asymmetric: a revocation that does not
// take effect is a silent security hole, and one that fires when it should not
// is a denial of service against our own pool. "It works on the happy path" is
// not coverage for this feature.
//
// Run: node --test   (from infra/relay-hydra/)

import { test } from "node:test";
import assert from "node:assert/strict";
import { generateKeyPairSync, sign } from "node:crypto";
import { verifyDirectory, admitRelays, revocationAnchor, DirectoryRejected } from "../lib/directory-verify.mjs";
import { verifyRevocations, revokes, certDigestB64, RevocationRejected, REVOCATION_TYPE } from "../lib/revocation-verify.mjs";
import {
  parseRevocationSet,
  revokesRelay,
  buildRevocationPayload,
  directoryAnchor,
  directoryTtlFor,
} from "../reconciler/revocation.mjs";

const { publicKey, privateKey } = generateKeyPairSync("ed25519");
const pubRawB64 = publicKey.export({ format: "der", type: "spki" }).subarray(-32).toString("base64");

const now = 1_737_600_000;
const RELAY_PORT = 9444;

// The pool's shared QUIC leaf, as it appears in the directory (base64 of DER).
const CERT_B64 = Buffer.from("pool leaf DER").toString("base64");
const OTHER_CERT_B64 = Buffer.from("some other leaf DER").toString("base64");

// Sign an object the way reconciler/index.mjs signEnvelope() does: the exact
// UTF-8 bytes that get base64'd into payload_b64.
function signEnvelope(payload, key = privateKey) {
  const payloadBytes = Buffer.from(JSON.stringify(payload), "utf8");
  const signature = sign(null, payloadBytes, key);
  return JSON.stringify({
    payload_b64: payloadBytes.toString("base64"),
    signature_b64: signature.toString("base64"),
  });
}

// The full producer path: operator parameter value + SSM version -> signed list.
function publishRevocations(paramValue, seq, { ttl = 300, key = privateKey } = {}) {
  const { revoked } = parseRevocationSet(paramValue, RELAY_PORT);
  return signEnvelope(
    buildRevocationPayload({ seq, revoked, issuedAt: now, ttlSeconds: ttl }),
    key,
  );
}

function freshDirectory(overrides = {}, revocation = null) {
  const dir = {
    version: 1,
    issued_at: now,
    expires_at: now + 3600,
    relays: [
      { addr: "203.0.113.7:9444", region: "us-west-2", cert_b64: CERT_B64 },
      { addr: "203.0.113.8:9444", region: "us-east-2", cert_b64: CERT_B64 },
    ],
    ...overrides,
  };
  if (revocation !== null) {
    dir.revocation = revocation;
  }
  return dir;
}

// ── The producer: parsing the operator-authored parameter ────────────────────

test("producer: parses addr / ip / cert selectors and normalizes them", () => {
  const { revoked } = parseRevocationSet(
    JSON.stringify({
      revoked: [
        { addr: " 203.0.113.7:9444 ", reason: "seized" },
        { ip: "198.51.100.4" },
        { cert_sha256_b64: certDigestB64(Buffer.from(CERT_B64, "base64")) },
      ],
    }),
    RELAY_PORT,
  );
  assert.equal(revoked.length, 3);
  assert.equal(revoked[0].addr, "203.0.113.7:9444");
  assert.equal(revoked[0].reason, "seized");
  // `ip` is sugar: expanded to the pool's relay port.
  assert.equal(revoked[1].addr, "198.51.100.4:9444");
  assert.equal(revoked[2].cert_sha256_b64.length, 44);
});

test("producer: an empty revocation set is valid and normal", () => {
  assert.deepEqual(parseRevocationSet(JSON.stringify({ revoked: [] }), RELAY_PORT), { revoked: [] });
  // A parameter seeded as {} is the same thing.
  assert.deepEqual(parseRevocationSet("{}", RELAY_PORT), { revoked: [] });
});

test("producer: refuses anything it cannot fully understand", () => {
  const bad = [
    "not json",
    "[]",
    JSON.stringify({ revoked: "everything" }),
    // No selector at all — an entry that would revoke nothing.
    JSON.stringify({ revoked: [{ reason: "seized" }] }),
    // Ambiguous: two address selectors.
    JSON.stringify({ revoked: [{ addr: "1.2.3.4:9444", ip: "1.2.3.4" }] }),
    // Not host:port.
    JSON.stringify({ revoked: [{ addr: "1.2.3.4" }] }),
    // Not a 32-byte SHA-256.
    JSON.stringify({ revoked: [{ cert_sha256_b64: "QUJD" }] }),
  ];
  for (const value of bad) {
    assert.throws(() => parseRevocationSet(value, RELAY_PORT), Error, `expected ${value} to throw`);
  }
});

test("producer: revoked relays are dropped from the directory candidate set", () => {
  const { revoked } = parseRevocationSet(
    JSON.stringify({ revoked: [{ addr: "203.0.113.7:9444" }] }),
    RELAY_PORT,
  );
  assert.equal(revokesRelay(revoked, { addr: "203.0.113.7:9444", cert_b64: CERT_B64 }), true);
  // The rest of the pool shares the SAME leaf and must survive — revoking by
  // cert here would take every node down at once.
  assert.equal(revokesRelay(revoked, { addr: "203.0.113.8:9444", cert_b64: CERT_B64 }), false);
});

test("producer: the directory TTL is cut while anything is revoked", () => {
  // This is the only lever that reaches ALREADY-SHIPPED clients, which know
  // nothing about revocations.json.
  assert.equal(directoryTtlFor({ revokedCount: 0, baseTtl: 3600, activeTtl: 300 }), 3600);
  assert.equal(directoryTtlFor({ revokedCount: 1, baseTtl: 3600, activeTtl: 300 }), 300);
  // Never LENGTHENS the TTL.
  assert.equal(directoryTtlFor({ revokedCount: 1, baseTtl: 120, activeTtl: 300 }), 120);
});

test("producer: no anchor at all when revocation is not configured", () => {
  assert.equal(directoryAnchor({ seq: null, path: "revocations.json", count: 0 }), null);
  assert.deepEqual(directoryAnchor({ seq: 4, path: "revocations.json", count: 1 }), {
    seq: 4,
    path: "revocations.json",
    count: 1,
  });
});

// ── The consumer: verifying the signed revocation list ───────────────────────

test("valid revocation list verifies and parses", () => {
  const env = publishRevocations(JSON.stringify({ revoked: [{ addr: "203.0.113.7:9444" }] }), 12);
  const list = verifyRevocations(env, pubRawB64, now, 0);
  assert.equal(list.seq, 12);
  assert.equal(list.count, 1);
  assert.equal(revokes(list, { addr: "203.0.113.7:9444", cert_b64: CERT_B64 }), true);
  assert.equal(revokes(list, { addr: "203.0.113.8:9444", cert_b64: CERT_B64 }), false);
});

test("byte-for-byte: verifier decodes the exact signed bytes", () => {
  const payload = buildRevocationPayload({ seq: 3, revoked: [], issuedAt: now, ttlSeconds: 300 });
  const envelope = JSON.parse(signEnvelope(payload));
  assert.equal(Buffer.from(envelope.payload_b64, "base64").toString("utf8"), JSON.stringify(payload));
});

test("an empty revocation list is valid — it is the steady state", () => {
  const list = verifyRevocations(publishRevocations("{}", 1), pubRawB64, now, 0);
  assert.equal(list.count, 0);
  assert.equal(revokes(list, { addr: "203.0.113.7:9444", cert_b64: CERT_B64 }), false);
});

test("a cert-keyed revocation follows the relay wherever it moves", () => {
  const env = publishRevocations(
    JSON.stringify({ revoked: [{ cert_sha256_b64: certDigestB64(Buffer.from(CERT_B64, "base64")) }] }),
    5,
  );
  const list = verifyRevocations(env, pubRawB64, now, 0);
  assert.equal(revokes(list, { addr: "198.51.100.99:9444", cert_b64: CERT_B64 }), true);
  assert.equal(revokes(list, { addr: "198.51.100.99:9444", cert_b64: OTHER_CERT_B64 }), false);
});

// ── Fail-closed: forgery, tampering, replay, confusion ───────────────────────

test("rejects a FORGED revocation list (wrong signing key)", () => {
  const { privateKey: attacker } = generateKeyPairSync("ed25519");
  const forged = publishRevocations("{}", 99, { key: attacker });
  assert.throws(() => verifyRevocations(forged, pubRawB64, now, 0), /bad signature/);
});

test("rejects a TAMPERED list — stripping a revocation breaks the signature", () => {
  const env = JSON.parse(
    publishRevocations(JSON.stringify({ revoked: [{ addr: "203.0.113.7:9444" }] }), 12),
  );
  // Swap in a payload revoking nothing, keeping the genuine signature.
  const stripped = buildRevocationPayload({ seq: 12, revoked: [], issuedAt: now, ttlSeconds: 300 });
  env.payload_b64 = Buffer.from(JSON.stringify(stripped), "utf8").toString("base64");
  assert.throws(() => verifyRevocations(JSON.stringify(env), pubRawB64, now, 0), /bad signature/);
});

test("rejects an EXPIRED revocation list", () => {
  const env = publishRevocations("{}", 2, { ttl: 300 });
  // Exactly at expiry is already expired (>=, matching the directory rule).
  assert.throws(() => verifyRevocations(env, pubRawB64, now + 300, 0), /expired/);
});

test("rejects a REPLAYED older list below the caller's floor", () => {
  const older = publishRevocations("{}", 7);
  assert.throws(() => verifyRevocations(older, pubRawB64, now, 9), /rolled back/);
  // At or above the floor it is fine.
  assert.equal(verifyRevocations(older, pubRawB64, now, 7).seq, 7);
});

test("a signed DIRECTORY cannot pose as a revocation list, and vice versa", () => {
  // Same key, same envelope shape — only the self-naming `type` tag stops this.
  const directoryEnvelope = signEnvelope(freshDirectory());
  assert.throws(() => verifyRevocations(directoryEnvelope, pubRawB64, now, 0), RevocationRejected);

  const revocationEnvelope = publishRevocations("{}", 1);
  // The directory verifier rejects it too — no `relays`.
  assert.throws(() => verifyDirectory(revocationEnvelope, pubRawB64, now), DirectoryRejected);
});

test("rejects version != 1 and a wrong type tag", () => {
  const wrongVersion = signEnvelope({
    version: 2,
    type: REVOCATION_TYPE,
    seq: 1,
    issued_at: now,
    expires_at: now + 300,
    revoked: [],
  });
  assert.throws(() => verifyRevocations(wrongVersion, pubRawB64, now, 0), /unsupported revocation version/);

  const wrongType = signEnvelope({
    version: 1,
    type: "something-else",
    seq: 1,
    issued_at: now,
    expires_at: now + 300,
    revoked: [],
  });
  assert.throws(() => verifyRevocations(wrongType, pubRawB64, now, 0), /wrong artifact type/);
});

test("an entry with no selector this build understands fails the WHOLE list closed", () => {
  // A revocation published by a NEWER producer, keyed on something we do not
  // know. Skipping it would silently downgrade a real revocation to an
  // admission — the exact bypass this feature exists to prevent.
  const env = signEnvelope({
    version: 1,
    type: REVOCATION_TYPE,
    seq: 4,
    issued_at: now,
    expires_at: now + 300,
    revoked: [{ region: "us-west-2", reason: "seized" }],
  });
  assert.throws(() => verifyRevocations(env, pubRawB64, now, 0), /carries no addr and no cert_sha256_b64/);
});

test("a malformed cert digest fails the whole list closed", () => {
  const env = signEnvelope({
    version: 1,
    type: REVOCATION_TYPE,
    seq: 4,
    issued_at: now,
    expires_at: now + 300,
    revoked: [{ cert_sha256_b64: "QUJD" }],
  });
  assert.throws(() => verifyRevocations(env, pubRawB64, now, 0), /not a 32-byte SHA-256/);
});

test("rejects malformed envelope JSON", () => {
  assert.throws(() => verifyRevocations("{not json", pubRawB64, now, 0), /malformed envelope/);
});

// ── The client's admission step: directory x revocation list ─────────────────

test("admitRelays returns the surviving relays and drops the revoked one", () => {
  const env = publishRevocations(JSON.stringify({ revoked: [{ addr: "203.0.113.7:9444" }] }), 12);
  const list = verifyRevocations(env, pubRawB64, now, 0);
  const dir = verifyDirectory(
    signEnvelope(freshDirectory({}, { seq: 12, path: "revocations.json", count: 1 })),
    pubRawB64,
    now,
  );
  const usable = admitRelays(dir, list, now);
  assert.deepEqual(usable.map((r) => r.addr), ["203.0.113.8:9444"]);
});

test("admitRelays FAILS CLOSED with no revocation list at all", () => {
  const dir = verifyDirectory(
    signEnvelope(freshDirectory({}, { seq: 12, path: "revocations.json", count: 0 })),
    pubRawB64,
    now,
  );
  // A client that cannot evaluate revocation must not fall back to trusting the
  // directory alone — that is the whole mechanism, undone.
  assert.throws(() => admitRelays(dir, null, now), /cannot evaluate revocation/);
});

test("admitRelays FAILS CLOSED on an expired list, without it being re-fetched", () => {
  const list = verifyRevocations(publishRevocations("{}", 12, { ttl: 300 }), pubRawB64, now, 0);
  const dir = verifyDirectory(
    signEnvelope(freshDirectory({}, { seq: 12, path: "revocations.json", count: 0 })),
    pubRawB64,
    now,
  );
  // The directory is still valid for another 55 minutes; the revocation list is
  // not. Held evidence stops being evidence — this is the property that bounds
  // exposure to the REVOCATION ttl instead of the directory ttl.
  assert.doesNotThrow(() => admitRelays(dir, list, now + 299));
  assert.throws(() => admitRelays(dir, list, now + 300), /revocation list expired/);
});

test("admitRelays FAILS CLOSED when a fresh directory is paired with a stale list", () => {
  // The on-path downgrade: both artifacts are genuinely signed and unexpired,
  // but they are from different moments. The anchor inside the SIGNED directory
  // is what makes the mismatch detectable.
  const stale = verifyRevocations(publishRevocations("{}", 11), pubRawB64, now, 0);
  const dir = verifyDirectory(
    signEnvelope(freshDirectory({}, { seq: 12, path: "revocations.json", count: 1 })),
    pubRawB64,
    now,
  );
  assert.throws(() => admitRelays(dir, stale, now), /behind the directory anchor/);
});

test("admitRelays FAILS CLOSED when every advertised relay is revoked", () => {
  const env = publishRevocations(
    JSON.stringify({ revoked: [{ addr: "203.0.113.7:9444" }, { addr: "203.0.113.8:9444" }] }),
    13,
  );
  const list = verifyRevocations(env, pubRawB64, now, 0);
  const dir = verifyDirectory(
    signEnvelope(freshDirectory({}, { seq: 13, path: "revocations.json", count: 2 })),
    pubRawB64,
    now,
  );
  assert.throws(() => admitRelays(dir, list, now), /every advertised relay is revoked/);
});

test("an unanchored directory still requires a fresh revocation list", () => {
  // A directory published before revocation existed (or by a pool with it
  // switched off) lowers the sequence floor to 0 — it does NOT waive the
  // requirement to hold a fresh list.
  const dir = verifyDirectory(signEnvelope(freshDirectory()), pubRawB64, now);
  assert.equal(revocationAnchor(dir), null);
  assert.throws(() => admitRelays(dir, null, now), /cannot evaluate revocation/);
  const list = verifyRevocations(publishRevocations("{}", 1), pubRawB64, now, 0);
  assert.equal(admitRelays(dir, list, now).length, 2);
});

test("a malformed anchor is rejected rather than ignored", () => {
  const dir = verifyDirectory(
    signEnvelope(freshDirectory({}, { seq: "twelve", path: "revocations.json" })),
    pubRawB64,
    now,
  );
  assert.throws(() => revocationAnchor(dir), /non-integer seq/);
});
