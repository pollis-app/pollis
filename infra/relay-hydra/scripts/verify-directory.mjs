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

import { verifyDirectory, revocationAnchor, admitRelays } from "../lib/directory-verify.mjs";
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
  process.exit(0);
}

const revocationUrl = new URL(anchor.path, url).toString();
const revRes = await fetch(revocationUrl);
if (!revRes.ok) {
  console.error(`REJECTED — revocation list fetch failed: HTTP ${revRes.status} (${revocationUrl})`);
  process.exit(1);
}

let usable;
try {
  // The anchor is the seq floor: a list below it is a stale artifact paired with
  // a fresh directory, which is the on-path downgrade this check exists to catch.
  const list = verifyRevocations(await revRes.text(), pubKeyB64, undefined, anchor.seq);
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
process.exit(0);
