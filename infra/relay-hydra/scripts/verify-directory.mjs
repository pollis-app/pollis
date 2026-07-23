#!/usr/bin/env node
// Fetch the live signed directory and run the client's verification path against
// it (§7 acceptance: prove the published directory validates exactly).
//
// Usage:
//   node scripts/verify-directory.mjs <directory-url> <POLLIS_OVERLAY_DIRECTORY_KEY>
//
// Exits 0 and prints the relay set on success; non-zero with the rejection reason
// on failure — exactly what the client does (fail closed).

import { verifyDirectory } from "../lib/directory-verify.mjs";

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

try {
  const dir = verifyDirectory(text, pubKeyB64);
  console.log(`OK — directory valid. version=${dir.version} expires_at=${dir.expires_at} relays=${dir.relays.length}`);
  for (const r of dir.relays) {
    console.log(`  ${r.region}\t${r.addr}\tcert_b64=${r.cert_b64.slice(0, 16)}…`);
  }
  process.exit(0);
} catch (err) {
  console.error(`REJECTED — ${err.message}`);
  process.exit(1);
}
