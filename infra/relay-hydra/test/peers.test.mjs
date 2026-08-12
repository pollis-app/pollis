// Peer-relay publication (#813 wave 3): parsing the relays' parked-peer reports,
// aggregating them pool-wide, and withholding revoked peers. No AWS, no network.
//
// Run: node --test   (from infra/relay-hydra/)

import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { parsePeerReport, aggregatePeers, excludeRevokedPeers, MAX_DIRECTORY_PEERS } from "../reconciler/peers.mjs";

const RELAY_A = "203.0.113.7:9444";
const RELAY_B = "203.0.113.8:9444";

function certB64(name) {
  return Buffer.from(name).toString("base64");
}

function digestB64(name) {
  return createHash("sha256").update(Buffer.from(name)).digest("base64");
}

function body(...names) {
  return JSON.stringify({ peers: names.map((n) => ({ cert_b64: certB64(n) })) });
}

// ── Parsing one relay's report ───────────────────────────────────────────────

test("parses a well-formed parked-peer report", () => {
  assert.deepEqual(parsePeerReport(body("p1", "p2")), [
    { cert_b64: certB64("p1") },
    { cert_b64: certB64("p2") },
  ]);
});

test("an unreadable report contributes NO peers and never throws", () => {
  // A relay build from before peer parking answers 404 (handled by the caller);
  // anything else unreadable must degrade to "this node has no peers", because a
  // shorter path is exactly where the pool was before peers existed.
  for (const bad of ["", "not json", "[]", "null", '"a string"', "{}", '{"peers":"nope"}', '{"peers":null}']) {
    assert.deepEqual(parsePeerReport(bad), [], `parsed ${JSON.stringify(bad)} as peers`);
  }
});

test("one unreadable entry is skipped, the rest of the report survives", () => {
  const raw = JSON.stringify({
    peers: [{ cert_b64: certB64("good") }, { cert_b64: "" }, { cert_b64: 7 }, null, ["x"], { nope: 1 }],
  });
  assert.deepEqual(parsePeerReport(raw), [{ cert_b64: certB64("good") }]);
});

test("cert spellings are canonicalized, so one peer cannot become two entries", () => {
  // Two base64 spellings of the same DER must collapse — otherwise a peer could
  // also dodge a cert_sha256_b64 revocation just by re-spelling its cert.
  const canonical = certB64("p1");
  const padded = `  ${canonical}  `;
  assert.deepEqual(parsePeerReport(JSON.stringify({ peers: [{ cert_b64: padded }] })), [
    { cert_b64: canonical },
  ]);
});

// ── Aggregating across the pool ──────────────────────────────────────────────

test("a peer parked at several relays collapses to one entry listing them all", () => {
  const peers = aggregatePeers([
    { addr: RELAY_B, peers: parsePeerReport(body("p1")) },
    { addr: RELAY_A, peers: parsePeerReport(body("p1", "p2")) },
  ]);
  assert.equal(peers.length, 2);
  const p1 = peers.find((p) => p.cert_b64 === certB64("p1"));
  assert.deepEqual(p1.parked_at, [RELAY_A, RELAY_B], "parked_at must union across relays, sorted");
  const p2 = peers.find((p) => p.cert_b64 === certB64("p2"));
  assert.deepEqual(p2.parked_at, [RELAY_A]);
});

test("an unchanged pool aggregates to byte-identical bytes", () => {
  // The directory is re-signed every couple of minutes; a set that reshuffles per
  // cycle would churn CloudFront and make real membership changes unreadable in a
  // diff. Ordering is therefore total, not incidental.
  const reports = [
    { addr: RELAY_A, peers: parsePeerReport(body("p2", "p1")) },
    { addr: RELAY_B, peers: parsePeerReport(body("p1", "p2")) },
  ];
  const shuffled = [reports[1], reports[0]];
  assert.equal(JSON.stringify(aggregatePeers(reports)), JSON.stringify(aggregatePeers(shuffled)));
});

test("a report from a relay with no address is ignored", () => {
  // parked_at has to name a relay clients can dial, so an entry we cannot attribute
  // to an advertised relay is not publishable.
  assert.deepEqual(aggregatePeers([{ addr: "", peers: parsePeerReport(body("p1")) }]), []);
  assert.deepEqual(aggregatePeers([{ peers: parsePeerReport(body("p1")) }]), []);
  assert.deepEqual(aggregatePeers([]), []);
  assert.deepEqual(aggregatePeers(undefined), []);
});

test("the published peer set is capped, deterministically", () => {
  // The directory is downloaded by every overlay-on client on every refresh, so
  // the artifact's size cannot grow without bound with volunteer count.
  const many = Array.from({ length: MAX_DIRECTORY_PEERS + 25 }, (_, i) => `peer-${i}`);
  const peers = aggregatePeers([{ addr: RELAY_A, peers: parsePeerReport(body(...many)) }]);
  assert.equal(peers.length, MAX_DIRECTORY_PEERS);
  // Same input, same output — the cap must not sample randomly.
  const again = aggregatePeers([{ addr: RELAY_A, peers: parsePeerReport(body(...many.slice().reverse())) }]);
  assert.deepEqual(peers, again);
});

// ── Revocation ───────────────────────────────────────────────────────────────

test("a revoked peer is excluded by the reconciler", () => {
  const peers = aggregatePeers([{ addr: RELAY_A, peers: parsePeerReport(body("p1", "p2")) }]);
  const { peers: kept, excluded } = excludeRevokedPeers(peers, [
    { cert_sha256_b64: digestB64("p1"), reason: "seized" },
  ]);
  assert.equal(excluded, 1);
  assert.deepEqual(kept.map((p) => p.cert_b64), [certB64("p2")]);
});

test("an empty revocation set changes nothing", () => {
  const peers = aggregatePeers([{ addr: RELAY_A, peers: parsePeerReport(body("p1")) }]);
  assert.deepEqual(excludeRevokedPeers(peers, []), { peers, excluded: 0 });
  assert.deepEqual(excludeRevokedPeers(peers, undefined), { peers, excluded: 0 });
});

test("an ADDRESS revocation never sweeps peers out by accident", () => {
  // A peer has no address of its own; the addresses in the revocation set belong
  // to first-party nodes. Only the cert selector may take a peer out.
  const peers = aggregatePeers([{ addr: RELAY_A, peers: parsePeerReport(body("p1")) }]);
  assert.deepEqual(excludeRevokedPeers(peers, [{ addr: RELAY_A }]), { peers, excluded: 0 });
});
