#!/usr/bin/env node
// Fetch the live signed directory (and, when the directory anchors one, the
// signed revocation list) and run the client's verification path against them
// (§7 acceptance: prove the published artifacts validate exactly).
//
// Usage:
//   node scripts/verify-directory.mjs <directory-url> <POLLIS_OVERLAY_DIRECTORY_KEY>
//
// Exits 0 and prints the usable relay set on success; non-zero with the rejection
// reason on failure — exactly what the client does (fail closed). A directory
// whose relays are all revoked, or whose revocation list is missing/stale, is a
// FAILURE here, not a warning: that is precisely the state in which a client has
// nothing it may safely dial.

import { verifyDirectory, revocationAnchor, admitRelays, directoryPeers, admitPeers } from "../lib/directory-verify.mjs";
import { verifyRevocations } from "../lib/revocation-verify.mjs";

const [, , url, pubKeyB64] = process.argv;
if (!url || !pubKeyB64) {
  console.error("usage: verify-directory.mjs <directory-url> <public-key-b64>");
  process.exit(2);
}

const res = await fetch(url);
if (!res.ok) {
  console.error(`fetch failed: HTTP ${res.status}`);
  process.exit(1);
}
const text = await res.text();

let dir;
try {
  dir = verifyDirectory(text, pubKeyB64);
} catch (err) {
  console.error(`REJECTED — ${err.message}`);
  process.exit(1);
}
console.log(`OK — directory valid. version=${dir.version} expires_at=${dir.expires_at} relays=${dir.relays.length}`);

// Revocation (#813). A pool that has not enabled it publishes no anchor, and the
// pre-#813 output above is the whole answer.
let anchor;
try {
  anchor = revocationAnchor(dir);
} catch (err) {
  console.error(`REJECTED — ${err.message}`);
  process.exit(1);
}
if (anchor === null) {
  console.log("  (no revocation anchor — this pool has revocation disabled)");
  for (const r of dir.relays) {
    console.log(`  ${r.region}\t${r.addr}\tcert_b64=${r.cert_b64.slice(0, 16)}…`);
  }
  reportPeers(dir, null);
  process.exit(0);
}

const revocationUrl = new URL(anchor.path, url).toString();
const revRes = await fetch(revocationUrl);
if (!revRes.ok) {
  console.error(`REJECTED — revocation list fetch failed: HTTP ${revRes.status} (${revocationUrl})`);
  process.exit(1);
}

let usable;
let list;
try {
  // The anchor is the seq floor: a list below it is a stale artifact paired with
  // a fresh directory, which is the on-path downgrade this check exists to catch.
  list = verifyRevocations(await revRes.text(), pubKeyB64, undefined, anchor.seq);
  console.log(`OK — revocation list valid. seq=${list.seq} expires_at=${list.expires_at} revoked=${list.count}`);
  usable = admitRelays(dir, list);
} catch (err) {
  console.error(`REJECTED — ${err.message}`);
  process.exit(1);
}

for (const r of usable) {
  console.log(`  ${r.region}\t${r.addr}\tcert_b64=${r.cert_b64.slice(0, 16)}…`);
}
const suppressed = dir.relays.length - usable.length;
if (suppressed > 0) {
  console.log(`  (${suppressed} advertised relay(s) suppressed by revocation)`);
}

reportPeers(dir, list);
process.exit(0);

// Peer-hosted relays (#813 wave 3). Advisory: peers are middle hops only, so an
// empty set means shorter paths, never an unusable pool — this is NOT a reason to
// exit non-zero. `admitPeers` runs the same revocation rule the shipping client
// runs, so a revoked or unevaluable peer shows up here as suppressed.
function reportPeers(directory, list) {
  const advertised = directoryPeers(directory);
  if (advertised.length === 0) {
    console.log("  (no peer-hosted relays advertised — paths are first-party only)");
    return;
  }
  const usablePeers = admitPeers(directory, list);
  console.log(`OK — ${usablePeers.length}/${advertised.length} peer relay(s) usable as middle hops.`);
  for (const p of usablePeers) {
    console.log(`  peer\tparked_at=${p.parked_at.join(",")}\tcert_b64=${p.cert_b64.slice(0, 16)}…`);
  }
}
