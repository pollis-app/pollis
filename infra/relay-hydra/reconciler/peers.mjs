// Peer-hosted relay publication — the PRODUCER half (#813 wave 3, contract §3).
//
// Peers sit behind NAT and park with a first-party relay, so they reach clients only via the signed directory.
// Relays report their parked peers on the health port; this aggregates them into the directory's `peers`.
// A peer entry discloses only its (public) leaf cert and the relays it is parked at (§5.2/§11.7).
// Unlike revocation, peers fail open: an unreadable report contributes no peers and never fails the cycle.
// Pure functions; tested by test/peers.test.mjs and test/directory-contract.test.mjs.

import { revokesRelay } from "./revocation.mjs";

// The HTTP path each relay serves its parked-peer report on, alongside /version
// on the same health port. A relay that predates peer parking answers 404, which
// this module reads as "no peers" — so the rollout needs no flag and no ordering.
export const PEERS_PATH = "/peers";

// Upper bound on how many peers one directory advertises.
// Every client downloads the signed directory, and each entry is a DER leaf; capped after a stable sort.
//
// The scaling path when this bites is sharding the peer set per client (each
// client gets a signed subset), not raising this number.
export const MAX_DIRECTORY_PEERS = 64;

// Parse one relay's parked-peer report body into `[{ cert_b64 }]`.
//
// Accepted shape (the whole body):
//   { "peers": [ { "cert_b64": "<base64 DER of the peer's relay leaf>" }, ... ] }
//
// Returns [] for anything unreadable; unreadable entries are skipped, not fatal.
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
// One peer at several relays collapses to one entry; output is sorted so re-signing is byte-stable.
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
// Belt and braces with the client's own revocation check (pollis-core net::revocation).
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
