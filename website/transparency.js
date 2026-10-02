// Key Transparency explorer — verifies a conversation's commit chain IN THIS
// BROWSER against the static, signed log published at verify.pollis.com/v1/.
//
// This is a port of `pollis-verify group` (verifiable-log-serve/src/group.rs,
// `verify_group_via` + `verify_group_in_bundle_at`) and must reach the same
// verdict for the same log. No server computes anything: the page fetches the
// same static files the CLI fetches and checks them itself.
//
//   1. Version gate on /v1/index.json (refuse a log format we do not understand).
//   2. Pin: /v1/public_key.json must carry a key in PINNED_KEYS below, compared on
//      the id recomputed from the served key bytes, never the served id.
//   3. Fetch /v1/sth/latest.json and /v1/entries.json, select this conversation's
//      commits by re-deriving its windowed pseudonym in each leaf's window (#701),
//      and fetch an inclusion proof for each selected entry only.
//   4. Verify the STH's ML-DSA-44 signature, every inclusion proof against it, and
//      replay the commit-log invariant (no fork, no epoch regression, a successor
//      lineage opens at epoch 0) across every window under the real id.
//
// The only thing trusted is the pinned key. ML-DSA-44 and SHA-256 come from
// @noble/post-quantum and @noble/hashes, loaded through the import map in
// transparency.html with a pinned version and an SRI hash per module file.
//
// Every remote value is escaped through esc() before it is inserted into HTML.

import { ml_dsa44 } from "@noble/post-quantum/ml-dsa.js";
import { sha256 } from "@noble/hashes/sha2.js";

// ── Configuration ──────────────────────────────────────────────────────────
// The static transparency log. Production only; there is deliberately no
// override, so a crafted link cannot point the page at another host.
const LOG_BASE = "https://verify.pollis.com";

// The keys this page trusts. Mirrors PINNED_LOG_PUBLIC_KEYS in
// pollis-core/src/commands/transparency.rs and PINNED_KEYS in artifacts.js, and
// MUST stay byte-identical to both (scripts/check-pinned-log-key.py enforces it).
//
// `notAfter` is epoch milliseconds; null means the current key. Past that instant
// a retired key stops being accepted here, exactly as on the artifacts page.
const PINNED_KEYS = [
  {
    keyId: "6cbd4b2aed5c4bf1",
    algorithm: "ML-DSA-44",
    publicKey:
      "56ab128f3f10107382802e69d3de8659d0127c711feb9c849f5b213c6f2d0af3b5fe41f581b202b385906fc42e4421747e84939054d160c551536131e41508a82b1f3ff0a07bcc4cee5e2eae8e85155d5c9e0dbc6e7683811649fb9e3b1f18c7ed070dbf61f2a058915b33f8ad3edcd135dd18770053e5ac971b13d17d95e16e98f47a852d600c47cbc0349354af2898803cfec7112660076d20027cb67870e18fb25ee327a36743fa812ccf93ba0769ddbd3d42ab40849ac8c98357b64eaf1ffc242abb12fddef4d8cdfa02448b4d99546b448e589657f898a47c6f30ddd88edd3f4456470e0a151e5fd601750c8b0489d3471897cfa78e0d7a00d938dfe876ef243117c972e041fdb00aa7af30d34184153cfd7b1e3b481dc562bbfc82bc20fe8ac4d9845f41de49fc33b6f94494df7088b06c7cb9ae35db86ac0fd293ca403046cec46ca9b12c755670d3d9b14c300b11ec292cd5e37d9f9e5e5d1729222a33bf1e13440f44dbf1b4d4104c612db4e269760868be5ff99f9ed269625fa4f39e21713a14293285e95f8a8e8cecd9db8e6a70c36340280322eab3490270ac640f706a23e81d79111dead641eaf7b926582ed0b0422f9addc0091d731a4fe1b9079be8bd75df23f5f9bf287beab7f67f763e04f0245bf9c705136d04eb8391fb4b4f12bfba44ae49bb6f32ddb0d539e59cd0159120b2fb1718f57e12a846638dbe0b650bfcd5a6cc74cd315b49136ea4e13d431a7f3a4c38fc783a82ca2b4c44a2f379c8aa9704d4639de3f94466662c97fbbd834db97a90405c382b5039803f4e4ed5c6b57487c8d23ad9e4d319df3466c49ef1e1cef526ddad1db5fa14f3b067b40580e068582dc428e21dbdc3df848e8e00fe1181f8e0d1409ab9a8757aef008b67191f4368f37cbd587ff65acdf07adbb989d09cc3318e346ca71c029557f2c523c204defab472b3dcb09bfbb95d5d1665a360a00faeb09b660f13fdc00f7b53fbfeaa58f87a208ad4551bcbe4307bf4d8451e027f4cc33cd55700016795c3164b1bc90d9dd1737b49d2e9e4b190128d2e62a44a80c1375c616aa2871ae7ad4a914102551380a8f8edb68c2df02bdf52607a7432ea7026f6a1efcdb37ecc11ecf1623ec6979e5d65c2812a997121010cd5fd9a98b9ed34edc17b667bfd37ef2be6dfe67fbdde03fa95bb80d0e1c7336263042ef44c4f9d28f1bf959bdc24c09cf8269378705022ff476fce91dbba6c8ffec00b27572eaa4835b59948d7a625ccc84ff4ac062176f4972f5131a961b17c7ff0010d2f2f3f8c12b7bf05fb9771d64a24fdab058f4bf3a155ade6a496b9a09d43a7673b5d8fb6519e01bf911ca78cc23f95943f63db72883d522fe24d4b7c7a26c7fd43b4f6f7496acf9ea2cab2e3cd6fc274964b576084c820bae79dbaa331d11751ec718660cd8e7847b7bacf31180803f681fb349b96338c98c791f74bc95e0d37b2810632159bc3175fed2e16038d45d35e4628250e8c9fb66c5bb2238f6456901f657e9655d3d5a09ff4952a0b9eb9c614f78c27626a136ef281f7099f68e898628530ef690851c179ef6a02448d498e49b2c362c839832100f4a9bf4abf17d496c71bfb5263da345d952b275f04707b31b9f6575da6dd2be799b90cc615f52ec32b4833a7e619d7f34f91f16edc38bc0a869c7211473f3ab90255446e0b7efbb2b97e8111d43b039ec0469b020f38925aad61e229836c96fad5bf3c3cad8f2c1c8b56cd819e8972d108dbfa8cd518177feaa7f4e0b547584a9a5d39ad4f1e8010cfead998ec18991cb89031a11c03cbd1ee7e0a1436da10ef154db13d4850c687c0a668215c9c8b7b1c",
    notAfter: null,
  },
];

// Served-bundle wire format this verifier understands (bundle::FORMAT_VERSION)
// and the oldest one whose commit leaves it can decode (MIN_LEAF_FORMAT_VERSION).
const FORMAT_VERSION = 2;
const MIN_LEAF_FORMAT_VERSION = 2;

// The mls-commit-log tenant and its pseudonym scheme (commit_log.rs).
const TENANT = "mls-commit-log";
const PSEUDONYM_WINDOW_SIZE = 1024n;
const CONVERSATION_PSEUDONYM_DOMAIN = "pollis-verifiable-log:commit-pseudonym:conversation:v1";

// Domain tag prepended to the signed STH message (sth.rs STH_CONTEXT_COMMIT_LOG).
// It is part of the MESSAGE; the FIPS 204 context string is empty, because the
// Rust signer uses ml-dsa's `Signer` impl, which signs with an empty context.
const STH_CONTEXT_COMMIT_LOG = "pollis-verifiable-log:sth:v2";
const STH_PUB_LEN = 1312;
const STH_SIG_LEN = 2420;

// How many inclusion proofs to fetch at once.
const PROOF_FETCH_PARALLELISM = 6;

const utf8 = new TextEncoder();

// ── Byte helpers ────────────────────────────────────────────────────────────
// Strict hex decode (either case, like Rust's `hex` crate). Returns null on any
// malformed input so callers decide whether that is fatal.
function hexToBytes(s) {
  if (typeof s !== "string" || s.length % 2 !== 0 || !/^[0-9a-fA-F]*$/.test(s)) {
    return null;
  }
  const out = new Uint8Array(s.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = parseInt(s.substr(i * 2, 2), 16);
  }
  return out;
}

function bytesToHex(b) {
  let s = "";
  for (let i = 0; i < b.length; i++) {
    s += b[i].toString(16).padStart(2, "0");
  }
  return s;
}

function concatBytes(parts) {
  let len = 0;
  parts.forEach(function (p) {
    len += p.length;
  });
  const out = new Uint8Array(len);
  let off = 0;
  parts.forEach(function (p) {
    out.set(p, off);
    off += p.length;
  });
  return out;
}

function u64Bytes(value, littleEndian) {
  const out = new Uint8Array(8);
  new DataView(out.buffer).setBigUint64(0, BigInt(value), littleEndian);
  return out;
}

function u32be(value) {
  const out = new Uint8Array(4);
  new DataView(out.buffer).setUint32(0, value, false);
  return out;
}

function equalBytes(a, b) {
  if (a.length !== b.length) {
    return false;
  }
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) {
      return false;
    }
  }
  return true;
}

// ── Wire validation (mirrors the serde shapes the CLI deserializes) ────────
function isU64(v) {
  return Number.isSafeInteger(v) && v >= 0;
}

function isI64(v) {
  return Number.isSafeInteger(v);
}

function isObject(v) {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}

function isOptionalString(v) {
  return v === undefined || v === null || typeof v === "string";
}

function parsePublicKeyDoc(doc) {
  if (!isObject(doc) || typeof doc.public_key !== "string") {
    return null;
  }
  const keys = doc.keys === undefined ? [] : doc.keys;
  if (!Array.isArray(keys)) {
    return null;
  }
  for (const k of keys) {
    const okNotAfter = k && (k.not_after === undefined || k.not_after === null || isU64(k.not_after));
    if (
      !isObject(k) ||
      typeof k.key_id !== "string" ||
      typeof k.algorithm !== "string" ||
      typeof k.public_key !== "string" ||
      !okNotAfter
    ) {
      return null;
    }
  }
  return {
    public_key: doc.public_key,
    keys: keys.map(function (k) {
      return {
        key_id: k.key_id,
        public_key: k.public_key,
        not_after: k.not_after === undefined || k.not_after === null ? null : k.not_after,
      };
    }),
  };
}

function parseSth(s) {
  if (
    !isObject(s) ||
    !isU64(s.tree_size) ||
    typeof s.root_hash !== "string" ||
    !isU64(s.timestamp) ||
    typeof s.signature !== "string" ||
    !isOptionalString(s.key_id)
  ) {
    return null;
  }
  return {
    tree_size: s.tree_size,
    root_hash: s.root_hash,
    timestamp: s.timestamp,
    signature: s.signature,
    key_id: typeof s.key_id === "string" ? s.key_id : null,
  };
}

function parseEntries(list) {
  if (!Array.isArray(list)) {
    return null;
  }
  const out = [];
  for (const e of list) {
    if (!isObject(e) || typeof e.tenant !== "string") {
      return null;
    }
    const data = hexToBytes(e.data);
    if (data === null) {
      return null;
    }
    out.push({ tenant: e.tenant, data: data });
  }
  return out;
}

function parseInclusionProof(p) {
  if (!isObject(p) || !isU64(p.leaf_index) || !isU64(p.tree_size) || !Array.isArray(p.audit_path)) {
    return null;
  }
  if (!p.audit_path.every(function (h) { return typeof h === "string"; })) {
    return null;
  }
  return { leaf_index: p.leaf_index, tree_size: p.tree_size, audit_path: p.audit_path };
}

// CommitLeaf::decode — compact JSON of the leaf. Returns null if it does not
// decode, which (as in Rust) simply excludes the entry from selection.
const leafDecoder = new TextDecoder("utf-8", { fatal: true });
function decodeCommitLeaf(bytes) {
  let v;
  try {
    v = JSON.parse(leafDecoder.decode(bytes));
  } catch (e) {
    return null;
  }
  const generation = v && v.generation === undefined ? 0 : v && v.generation;
  if (
    !isObject(v) ||
    typeof v.conversation_pseudonym !== "string" ||
    !isU64(generation) ||
    !isU64(v.epoch) ||
    typeof v.sender_pseudonym !== "string" ||
    !isI64(v.seq) ||
    typeof v.commit_sha256 !== "string"
  ) {
    return null;
  }
  return {
    conversation_pseudonym: v.conversation_pseudonym,
    generation: generation,
    epoch: v.epoch,
    sender_pseudonym: v.sender_pseudonym,
    seq: v.seq,
    commit_sha256: v.commit_sha256,
  };
}

// ── Keys (bundle.rs PublicKeyDoc / pinned.rs) ───────────────────────────────
// key_id_for: first 8 bytes of SHA-256 over the encoded key, lowercase hex.
function keyIdFor(keyBytes) {
  return bytesToHex(sha256(keyBytes).subarray(0, 8));
}

function parseVerifyingKey(hex) {
  const b = hexToBytes(hex);
  if (b === null || b.length !== STH_PUB_LEN) {
    return null;
  }
  return b;
}

// PublicKeyDoc::active_keys + verifying_candidates: unexpired entries of `keys`,
// or `public_key` alone when `keys` is empty; undecodable keys are dropped.
function verifyingCandidates(doc, nowMs) {
  let active;
  if (doc.keys.length === 0) {
    active = [{ key_id: "", public_key: doc.public_key, not_after: null }];
  } else {
    active = doc.keys.filter(function (k) {
      return k.not_after === null || nowMs <= k.not_after;
    });
  }
  const out = [];
  active.forEach(function (k) {
    const vk = parseVerifyingKey(k.public_key);
    if (vk !== null) {
      out.push({ keyId: k.key_id, key: vk, notAfter: k.not_after });
    }
  });
  return out;
}

// PublicKeyDoc::overlap_keys: every published key other than the active one.
function overlapKeys(doc) {
  const active = doc.public_key.toLowerCase();
  return doc.keys.filter(function (k) {
    return k.public_key.toLowerCase() !== active;
  });
}

// Bundle::key_candidates for the verification-side bundle group.rs builds: the
// active key (its id recomputed) followed by the overlap keys, expiry applied.
function bundleKeyCandidates(doc, nowMs) {
  const activeVk = parseVerifyingKey(doc.public_key);
  const rebuilt = {
    public_key: doc.public_key,
    keys: [{ key_id: activeVk ? keyIdFor(activeVk) : "", public_key: doc.public_key, not_after: null }].concat(
      overlapKeys(doc)
    ),
  };
  return verifyingCandidates(rebuilt, nowMs);
}

// The pinned keys still inside their overlap window.
function livePinnedKeys(nowMs) {
  return PINNED_KEYS.filter(function (k) {
    return k.notAfter === null || nowMs <= k.notAfter;
  });
}

// pinned.rs require_pinned: keep only the served keys that are pinned, matched on
// the id recomputed from the served bytes, and return that pinned-only document.
// Everything downstream verifies against it, so a key the server merely lists
// beside the pinned one can never sign a head this page accepts.
function requirePinned(doc, nowMs) {
  const pinnedIds = new Set();
  livePinnedKeys(nowMs).forEach(function (k) {
    const vk = parseVerifyingKey(k.publicKey);
    if (vk !== null) {
      pinnedIds.add(keyIdFor(vk));
    }
  });
  const served = verifyingCandidates(doc, nowMs);
  const kept = served.filter(function (c) {
    return pinnedIds.has(keyIdFor(c.key));
  });
  if (kept.length === 0) {
    throw new Error(
      "malformed bundle: served public_key.json does not match the pinned log key — refusing to trust the served log"
    );
  }
  const keys = kept.map(function (c) {
    return { key_id: keyIdFor(c.key), public_key: bytesToHex(c.key), not_after: c.notAfter };
  });
  return { public_key: keys[0].public_key, keys: keys };
}

// ── STH (sth.rs) ────────────────────────────────────────────────────────────
// CONTEXT || tree_size (u64 BE) || root (32) || timestamp (u64 BE).
function sthVerifyWith(sth, key) {
  const root = hexToBytes(sth.root_hash);
  const sig = hexToBytes(sth.signature);
  if (root === null || root.length !== 32 || sig === null || sig.length !== STH_SIG_LEN) {
    return false;
  }
  const message = concatBytes([
    utf8.encode(STH_CONTEXT_COMMIT_LOG),
    u64Bytes(sth.tree_size, false),
    root,
    u64Bytes(sth.timestamp, false),
  ]);
  try {
    return ml_dsa44.verify(sig, message, key) === true;
  } catch (e) {
    return false;
  }
}

// Sth::verify_any: try the key the (unsigned) key_id hints at first, then every
// candidate. The hint can only reorder attempts, never change the verdict.
function sthVerifyAny(sth, candidates) {
  if (sth.key_id !== null) {
    const hinted = candidates.find(function (c) {
      return c.keyId === sth.key_id;
    });
    if (hinted && sthVerifyWith(sth, hinted.key)) {
      return true;
    }
  }
  return candidates.some(function (c) {
    return sthVerifyWith(sth, c.key);
  });
}

// ── Merkle (hash.rs / merkle.rs / proof.rs, RFC 6962 + RFC 9162) ────────────
function nodeHash(left, right) {
  return sha256(concatBytes([new Uint8Array([1]), left, right]));
}

// Entry::leaf_hash: SHA-256(0x00 || len(tenant) u32 BE || tenant || data).
function entryLeafHash(entry) {
  const tenant = utf8.encode(entry.tenant);
  return sha256(concatBytes([new Uint8Array([0]), u32be(tenant.length), tenant, entry.data]));
}

function verifyInclusion(leafHash, leafIndex, treeSize, path, root) {
  if (leafIndex >= treeSize) {
    return false;
  }
  let fln = BigInt(leafIndex);
  let sn = BigInt(treeSize) - 1n;
  let r = leafHash;
  for (const p of path) {
    if (sn === 0n) {
      return false;
    }
    if ((fln & 1n) === 1n || fln === sn) {
      r = nodeHash(p, r);
      if ((fln & 1n) === 0n) {
        while ((fln & 1n) === 0n && fln !== 0n) {
          fln >>= 1n;
          sn >>= 1n;
        }
      }
    } else {
      r = nodeHash(r, p);
    }
    fln >>= 1n;
    sn >>= 1n;
  }
  return sn === 0n && equalBytes(r, root);
}

function verifyInclusionProof(entry, proof, sth) {
  if (proof.tree_size !== sth.tree_size) {
    return false;
  }
  const root = hexToBytes(sth.root_hash);
  if (root === null || root.length !== 32) {
    return false;
  }
  const path = [];
  for (const h of proof.audit_path) {
    const b = hexToBytes(h);
    if (b === null || b.length !== 32) {
      return false;
    }
    path.push(b);
  }
  return verifyInclusion(entryLeafHash(entry), proof.leaf_index, proof.tree_size, path, root);
}

// ── Commit-log tenant (commit_log.rs) ───────────────────────────────────────
function windowForSeq(seq) {
  return BigInt(Math.max(seq, 0)) / PSEUDONYM_WINDOW_SIZE;
}

// derive_conversation_pseudonym: SHA-256 over length-framed parts, each length
// a u64 LITTLE-endian: len(domain) || domain || len(id) || id || 8 || window LE.
function deriveConversationPseudonym(conversationId, window) {
  const domain = utf8.encode(CONVERSATION_PSEUDONYM_DOMAIN);
  const id = utf8.encode(conversationId);
  const win = u64Bytes(window, true);
  return bytesToHex(
    sha256(
      concatBytes([
        u64Bytes(domain.length, true),
        domain,
        u64Bytes(id.length, true),
        id,
        u64Bytes(win.length, true),
        win,
      ])
    )
  );
}

// CommitLogInvariant::check against the leaves already accepted (all regrouped
// under one key, so every prior leaf is in the same group). Returns the
// violation message, or null if the candidate is accepted.
function commitLogViolation(accepted, cand) {
  const prefix = "tenant `" + TENANT + "` invariant violated: ";
  const conv = cand.conversation_pseudonym;
  let maxGeneration = null;
  for (const prev of accepted) {
    maxGeneration = maxGeneration === null ? prev.generation : Math.max(maxGeneration, prev.generation);
    if (prev.generation === cand.generation && prev.epoch === cand.epoch) {
      return (
        prefix +
        "fork in conversation `" + conv + "` at generation " + cand.generation +
        " epoch " + cand.epoch + ": seq " + cand.seq + " conflicts with seq " + prev.seq
      );
    }
    const prevAhead =
      prev.generation > cand.generation || (prev.generation === cand.generation && prev.epoch > cand.epoch);
    if (prevAhead) {
      return (
        prefix +
        "epoch regression in conversation `" + conv + "`: seq " + cand.seq +
        " is generation " + cand.generation + " epoch " + cand.epoch +
        " but seq " + prev.seq + " already reached generation " + prev.generation +
        " epoch " + prev.epoch
      );
    }
  }
  if (maxGeneration !== null && cand.generation > maxGeneration && cand.epoch !== 0) {
    return (
      prefix +
      "conversation `" + conv + "` opens generation " + cand.generation + " at epoch " + cand.epoch +
      " (seq " + cand.seq + "): a successor lineage must start at epoch 0"
    );
  }
  return null;
}

// ── Fetching ────────────────────────────────────────────────────────────────
// `no-cache` makes the browser revalidate, so a stale cached head or entry list
// is never verified as if it were current.
function fetchText(url) {
  return fetch(url, { cache: "no-cache" }).then(
    function (resp) {
      if (!resp.ok) {
        throw new Error("http error: GET " + url + ": status code " + resp.status);
      }
      return resp.text();
    },
    function (err) {
      throw new Error("http error: GET " + url + ": " + (err && err.message ? err.message : "network error"));
    }
  );
}

function fetchJson(url) {
  return fetchText(url).then(function (body) {
    try {
      return JSON.parse(body);
    } catch (e) {
      throw new Error("http error: parse " + url + ": " + e.message);
    }
  });
}

// Run `worker` over `items` with at most `limit` in flight.
function mapLimit(items, limit, worker) {
  const results = new Array(items.length);
  let next = 0;
  function run() {
    if (next >= items.length) {
      return Promise.resolve();
    }
    const i = next++;
    return worker(items[i], i).then(function (r) {
      results[i] = r;
      return run();
    });
  }
  const runners = [];
  for (let k = 0; k < Math.min(limit, items.length); k++) {
    runners.push(run());
  }
  return Promise.all(runners).then(function () {
    return results;
  });
}

// ── The verifier ────────────────────────────────────────────────────────────
// verify_group_via + verify_group_in_bundle_at. Rejects only when a prerequisite
// cannot be fetched, parsed, or trusted; every verification failure is folded
// into the returned report as chain_valid = false with a violation.
async function verifyGroup(baseUrl, conversationId, onProgress) {
  const progress = onProgress || function () {};
  const base = baseUrl.replace(/\/+$/, "");
  const nowMs = Date.now();

  progress("Fetching the log index…");
  const indexBody = await fetchText(base + "/v1/index.json");
  let manifest;
  try {
    manifest = JSON.parse(indexBody);
  } catch (e) {
    throw new Error("http error: read manifest format_version: " + e.message);
  }
  const formatVersion = isObject(manifest) && manifest.format_version !== undefined ? manifest.format_version : 0;
  if (!isU64(formatVersion)) {
    throw new Error("http error: read manifest format_version: not an unsigned integer");
  }
  if (formatVersion > FORMAT_VERSION) {
    throw new Error(
      "log format v" + formatVersion + " is newer than this verifier understands (up to v" + FORMAT_VERSION +
        ") — this page is too old for this log; reload it"
    );
  }
  if (formatVersion < MIN_LEAF_FORMAT_VERSION) {
    throw new Error(
      "log format v" + formatVersion + " is older than this verifier can interpret per-conversation (needs v" +
        MIN_LEAF_FORMAT_VERSION + "+)"
    );
  }

  progress("Checking the log's public key against the pinned key…");
  const pkUrl = base + "/v1/public_key.json";
  let pkDoc = parsePublicKeyDoc(await fetchJson(pkUrl));
  if (pkDoc === null) {
    throw new Error("http error: parse " + pkUrl + ": unexpected shape");
  }
  // Rebind to the pinned-only subset before anything is verified against it.
  pkDoc = requirePinned(pkDoc, nowMs);

  progress("Fetching the signed tree head and log entries…");
  const sthUrl = base + "/v1/sth/latest.json";
  const entriesUrl = base + "/v1/entries.json";
  const fetched = await Promise.all([fetchJson(sthUrl), fetchJson(entriesUrl)]);
  const sth = parseSth(fetched[0]);
  if (sth === null) {
    throw new Error("http error: parse " + sthUrl + ": unexpected shape");
  }
  const entries = parseEntries(fetched[1]);
  if (entries === null) {
    throw new Error("http error: parse " + entriesUrl + ": unexpected shape");
  }

  // Membership: decode each commit-log leaf and match its pseudonym against the
  // one re-derived for this conversation in that leaf's own window.
  progress("Finding this conversation's commits among " + entries.length + " log entries…");
  const pseudonymByWindow = new Map();
  const selected = [];
  entries.forEach(function (e, i) {
    if (e.tenant !== TENANT) {
      return;
    }
    const leaf = decodeCommitLeaf(e.data);
    if (leaf === null) {
      return;
    }
    const w = windowForSeq(leaf.seq);
    if (!pseudonymByWindow.has(w)) {
      pseudonymByWindow.set(w, deriveConversationPseudonym(conversationId, w));
    }
    if (leaf.conversation_pseudonym === pseudonymByWindow.get(w)) {
      selected.push({ index: i, leaf: leaf });
    }
  });
  // Stable sort by seq, as Rust's sort_by_key.
  selected.sort(function (a, b) {
    return a.leaf.seq - b.leaf.seq;
  });

  // Inclusion proofs for the selected entries only. A failed fetch omits the
  // proof and the entry is reported not-included. Proofs are keyed by the
  // leaf_index they carry, and only those for the latest head are kept.
  progress("Checking " + selected.length + " inclusion proof" + (selected.length === 1 ? "" : "s") + "…");
  const proofs = await mapLimit(selected, PROOF_FETCH_PARALLELISM, function (s) {
    const url = base + "/v1/proof/inclusion/" + sth.tree_size + "/" + s.index + ".json";
    return fetchJson(url).then(parseInclusionProof, function () {
      return null;
    });
  });
  const inclusionByIndex = new Map();
  proofs.forEach(function (p) {
    if (p !== null && p.tree_size === sth.tree_size) {
      inclusionByIndex.set(p.leaf_index, p);
    }
  });

  progress("Verifying the signature and replaying the commit chain…");
  const violations = [];

  // 1. Trust anchor.
  const sthSigOk = sthVerifyAny(sth, bundleKeyCandidates(pkDoc, nowMs));
  if (!sthSigOk) {
    violations.push("STH signature is invalid — published head is not trustworthy");
  }

  // 3. Inclusion.
  const commits = [];
  let allIncluded = true;
  selected.forEach(function (s) {
    const proof = inclusionByIndex.get(s.index);
    const included = proof !== undefined ? verifyInclusionProof(entries[s.index], proof, sth) : false;
    if (!included) {
      allIncluded = false;
      violations.push(
        "commit seq " + s.leaf.seq + " (generation " + s.leaf.generation + ", epoch " + s.leaf.epoch +
          ") is not provably included in the signed log"
      );
    }
    commits.push({
      generation: s.leaf.generation,
      epoch: s.leaf.epoch,
      seq: s.leaf.seq,
      sender_pseudonym: s.leaf.sender_pseudonym,
      commit_sha256: s.leaf.commit_sha256,
      included: included,
    });
  });

  // 4. Invariant, regrouped under the real conversation id so it spans windows.
  //    A rejected leaf is not appended, exactly like VerifiableLog::append.
  const accepted = [];
  let invariantOk = true;
  selected.forEach(function (s) {
    const canonical = {
      conversation_pseudonym: conversationId,
      generation: s.leaf.generation,
      epoch: s.leaf.epoch,
      seq: s.leaf.seq,
    };
    const violation = commitLogViolation(accepted, canonical);
    if (violation !== null) {
      invariantOk = false;
      violations.push(violation);
    } else {
      accepted.push(canonical);
    }
  });

  return {
    group_id: conversationId,
    found: selected.length > 0,
    sth_tree_size: sth.tree_size,
    root_hex: sth.root_hash,
    commits: commits,
    chain_valid: sthSigOk && allIncluded && invariantOk,
    violations: violations,
  };
}

export { verifyGroup };

// ── DOM helpers ─────────────────────────────────────────────────────────────
const form = document.getElementById("kt-form");
const input = document.getElementById("kt-group");
const submit = document.getElementById("kt-submit");
const result = document.getElementById("kt-result");

function show(html) {
  result.innerHTML = html;
  result.classList.add("is-visible");
}

// Escape text for safe insertion into HTML (all remote values are untrusted).
function esc(s) {
  return String(s)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function shortHash(s) {
  if (!s || s.length <= 14) {
    return s || "";
  }
  return s.slice(0, 8) + "…" + s.slice(-6);
}

// ── Rendering ─────────────────────────────────────────────────────────────--
function renderProgress(id, message) {
  show(
    '<span class="kt-badge kt-badge--info">Verifying ' + esc(id) + "…</span>" +
      '<p class="kt-note">' + esc(message) + "</p>"
  );
}

function renderError(message) {
  show(
    '<span class="kt-badge kt-badge--fail">Could not verify</span>' +
      '<p class="kt-note">The check could not run against <code>' +
      esc(LOG_BASE) +
      "/v1/</code>: " +
      esc(message) +
      "</p>" +
      '<p class="kt-note">Nothing was verified. You can run the same check with ' +
      "<code>pollis-verify group</code>.</p>"
  );
}

function renderViolations(violations) {
  if (!violations || violations.length === 0) {
    return "";
  }
  let html = '<div class="kt-violations"><h3>Violations</h3><ul>';
  violations.forEach(function (v) {
    html += "<li>" + esc(v) + "</li>";
  });
  return html + "</ul></div>";
}

function renderReport(report) {
  if (!report.found) {
    show(
      '<span class="kt-badge kt-badge--info">Not found</span>' +
        '<p class="kt-note">No commits were found for <code>' +
        esc(report.group_id) +
        "</code> in the transparency log. Double-check the conversation id.</p>" +
        renderViolations(report.violations)
    );
    return;
  }

  const pass = report.chain_valid;
  let html = "";

  html +=
    '<span class="kt-badge ' +
    (pass ? "kt-badge--pass" : "kt-badge--fail") +
    '">' +
    (pass ? "✓ Chain valid" : "✗ Chain INVALID") +
    "</span>";

  html +=
    '<div class="kt-meta">' +
    "<div>group: " +
    esc(report.group_id) +
    "</div>" +
    "<div>signed tree size: " +
    esc(report.sth_tree_size) +
    "</div>" +
    "<div>root: " +
    esc(report.root_hex) +
    "</div>" +
    "</div>";

  html += renderViolations(report.violations);

  // Commit timeline.
  html += '<ul class="kt-timeline">';
  report.commits.forEach(function (c) {
    const included = c.included;
    const lineage = c.generation > 0 ? "generation " + c.generation + " · " : "";
    html +=
      '<li class="kt-commit' +
      (included ? "" : " kt-commit--missing") +
      '">' +
      '<div class="kt-commit-head">' +
      '<span class="kt-epoch">' +
      esc(lineage) +
      "epoch " +
      esc(c.epoch) +
      "</span>" +
      '<span class="kt-inc ' +
      (included ? "kt-inc--ok" : "kt-inc--no") +
      '">' +
      (included ? "included ✓" : "NOT INCLUDED ✗") +
      "</span>" +
      "</div>" +
      '<div class="kt-commit-detail">seq ' +
      esc(c.seq) +
      " · sender " +
      esc(shortHash(c.sender_pseudonym)) +
      " · commit " +
      esc(shortHash(c.commit_sha256)) +
      "</div>" +
      "</li>";
  });
  html += "</ul>";

  if (pass) {
    html +=
      '<p class="kt-note">Verified in your browser: this conversation’s commit history is ' +
      "append-only and fork-free, and every commit is provably included in the log signed " +
      "by the pinned key.</p>";
  }

  show(html);
}

// ── Submit ──────────────────────────────────────────────────────────────────
form.addEventListener("submit", function (e) {
  e.preventDefault();
  const id = input.value.trim();
  if (!id) {
    return;
  }

  submit.disabled = true;
  renderProgress(id, "Starting…");

  verifyGroup(LOG_BASE, id, function (message) {
    renderProgress(id, message);
  })
    .then(function (report) {
      renderReport(report);
    })
    .catch(function (err) {
      renderError(err && err.message ? err.message : "Network error.");
    })
    .finally(function () {
      submit.disabled = false;
    });
});
