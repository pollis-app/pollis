// Peer-hosted relay publication — the PRODUCER half (#813 wave 3, contract §3).
//
// THE PROBLEM. A consenting device runs a relay node, but it never listens for
// inbound connections: it sits behind NAT/CGNAT and opens an OUTBOUND link to a
// first-party relay, which parks it. So nothing about a peer is discoverable by
// scanning, and a client cannot be told about one by the peer itself — a peer
// learned over an unsigned channel is exactly how you feed a target a hand-picked
// set of relays. Peers must therefore reach clients through the ONE artifact they
// already pin a key for: the signed directory.
//
// THE MECHANISM. Every first-party relay knows which peers are parked with it —
// that is a live socket in its own process, and it is the only place the fact
// exists. The relays surface those parked peer identities on the health port the
// reconciler already polls; this module aggregates them across the pool into the
// directory's additive `peers` array.
//
// WHAT IS DISCLOSED, AND WHY IT IS NOTHING NEW. A peer entry carries its relay
// leaf cert and the first-party relays it is parked at. Nothing else: no id, no
// region, no address, no count of anything, and nothing about the clients a relay
// is serving (§5.2/§11.7). The cert is public by construction — a client cannot
// pin a hop it has not been given, so publishing it is what makes the peer usable
// at all, and the peer presents it in TLS to anything that connects.
//
// FAILURE DIRECTION IS THE OPPOSITE OF REVOCATION'S. A revocation set we can only
// half-read must never be signed (half a revocation reads as "these relays are
// fine"). Peers are an AVAILABILITY field, not a safety one: fewer peers means
// shorter paths, which is the state the pool was in before this shipped. So an
// unreadable peer report contributes NO peers and never fails the cycle — the
// same "fail toward the safe shape" rule, pointing the other way because the safe
// shape is the other one.
//
// Pure functions, no AWS, no network — index.mjs injects the fetched reports.
// (index.mjs cannot be imported under test: its AWS SDK deps only exist in the
// Lambda runtime.) Tested by test/peers.test.mjs and, for the frozen §3 contract
// it feeds, test/directory-contract.test.mjs.

import { revokesRelay } from "./revocation.mjs";

// The HTTP path each relay serves its parked-peer report on, alongside /version
// on the same health port. A relay that predates peer parking answers 404, which
// this module reads as "no peers" — so the rollout needs no flag and no ordering.
export const PEERS_PATH = "/peers";

// Upper bound on how many peers one directory advertises.
//
// The directory is a SIGNED artifact every overlay-on client downloads on every
// refresh, and a peer entry is a whole DER leaf (~0.5-1 KB base64). Unbounded
// volunteer growth would therefore turn a ~1 KB artifact into a megabyte one that
// every client pays for on an hourly tick, to offer a choice of middle hop it uses
// one of. Capping keeps the artifact small; the cap is applied AFTER a stable sort
// so the published set does not churn between cycles for no reason.
//
// The scaling path when this bites is sharding the peer set per client (each
// client gets a signed subset), not raising this number.
export const MAX_DIRECTORY_PEERS = 64;

// Parse one relay's parked-peer report body into `[{ cert_b64 }]`.
//
// Accepted shape (the whole body):
//   { "peers": [ { "cert_b64": "<base64 DER of the peer's relay leaf>" }, ... ] }
//
// Returns [] for anything it cannot read — a body that is not JSON, not an
// object, or whose `peers` is not an array. Individual entries that are not
// readable are SKIPPED rather than failing the report: one malformed entry costs
// one middle-hop candidate, and dropping the whole report over it would cost the
// rest for no gain. See the module header for why this direction is the safe one
// here and the unsafe one in revocation.mjs.
export function parsePeerReport(body) {
  let parsed;
  try {
    parsed = JSON.parse(body);
  } catch {
    return [];
  }
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
    return [];
  }
  if (!Array.isArray(parsed.peers)) {
    return [];
  }
  const out = [];
  for (const entry of parsed.peers) {
    if (entry === null || typeof entry !== "object" || Array.isArray(entry)) {
      continue;
    }
    const certB64 = normalizeCert(entry.cert_b64);
    if (certB64 === null) {
      continue;
    }
    out.push({ cert_b64: certB64 });
  }
  return out;
}

// Merge per-relay reports into the directory's `peers` array.
//
// `reports` is `[{ addr, peers: [{ cert_b64 }] }]` — one entry per first-party
// relay that is itself going into the directory, `addr` being the address the
// directory advertises for it (so `parked_at` and `relays[].addr` are the same
// namespace, which is what lets a client intersect them).
//
// One peer parked at several relays collapses to ONE entry whose `parked_at`
// lists them all. Output is sorted (peers by cert, each `parked_at` by address)
// so an unchanged pool re-signs byte-identical bytes rather than reshuffling the
// artifact every two minutes.
export function aggregatePeers(reports) {
  const byCert = new Map();
  for (const report of reports ?? []) {
    const addr = typeof report?.addr === "string" ? report.addr.trim() : "";
    if (addr.length === 0) {
      continue;
    }
    for (const peer of report.peers ?? []) {
      const certB64 = normalizeCert(peer?.cert_b64);
      if (certB64 === null) {
        continue;
      }
      const parked = byCert.get(certB64) ?? new Set();
      parked.add(addr);
      byCert.set(certB64, parked);
    }
  }
  return [...byCert.entries()]
    .map(([cert_b64, parked]) => ({ cert_b64, parked_at: [...parked].sort() }))
    .sort((a, b) => (a.cert_b64 < b.cert_b64 ? -1 : a.cert_b64 > b.cert_b64 ? 1 : 0))
    .slice(0, MAX_DIRECTORY_PEERS);
}

// Drop every peer the operator has revoked.
//
// Belt and braces with the client, which runs the same revocation list over the
// peers it reads out of the directory (`pollis-core`'s net::revocation). Both
// halves are wanted: excluding here means a revoked peer stops being offered to
// anyone within one reconcile, and rejecting there means a client holding an older
// directory does not use it either.
//
// Peers are matched on the `cert_sha256_b64` selector only — a peer has no
// address to revoke, which is precisely the case phase C added that selector for.
export function excludeRevokedPeers(peers, revoked) {
  if (!Array.isArray(revoked) || revoked.length === 0) {
    return { peers: peers ?? [], excluded: 0 };
  }
  const kept = (peers ?? []).filter((peer) => !revokesRelay(revoked, { cert_b64: peer.cert_b64 }));
  return { peers: kept, excluded: (peers ?? []).length - kept.length };
}

// A base64 string that decodes to at least one byte, re-encoded canonically so
// two spellings of the same DER cannot become two directory entries (and cannot
// dodge a `cert_sha256_b64` revocation by re-spelling). Anything else is null.
function normalizeCert(value) {
  if (typeof value !== "string") {
    return null;
  }
  const trimmed = value.trim();
  if (trimmed.length === 0) {
    return null;
  }
  const bytes = Buffer.from(trimmed, "base64");
  if (bytes.length === 0) {
    return null;
  }
  return bytes.toString("base64");
}
