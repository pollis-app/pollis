#!/usr/bin/env node
/**
 * oss-inventory.mjs — what open source each Pollis product ships, and under
 * which licences (#1832).
 *
 * One generator for all four products, so a customer gets the same two
 * documents for each, built the same way:
 *
 *   SBOM     CycloneDX 1.6 JSON: every third-party Rust crate and npm package
 *            in the product, with licence, purl, hash (crates) and the
 *            dependency edges between them.
 *   notices  The attribution data the apps render on their "Open source
 *            licenses" page: name, version, licence and the licence text each
 *            package ships.
 *
 * The scope is what reaches the product, not the whole repo:
 *   Rust  `cargo tree -e normal,no-proc-macro` from the product's root crates,
 *         with the features and target triples that product is built with, so
 *         build scripts, proc-macros, dev-dependencies and other targets' crates
 *         are left out. The lockfile is `--locked`, as every release build is.
 *   JS    the production `dependencies` of the product's package.json, walked
 *         through node_modules with Node's own resolution, so the result is
 *         the installed tree (which `--frozen-lockfile` pins to pnpm-lock).
 * What it cannot see is C/C++ vendored inside -sys crates and the native
 * libraries React Native and Expo modules pull in through CocoaPods / Gradle;
 * those are covered by the licence of the crate or package that vendors them.
 *
 * Modes:
 *   node scripts/oss-inventory.mjs <product> [--version V] [--sbom F] [--notices F]
 *                                             [--summary F] [--out-dir D]
 *        product: desktop | mobile | cli | ds. --out-dir writes the SBOM
 *        under its published name plus summary.json, the index
 *        pollis.com/open-source reads (scripts/publish-oss-evidence.sh uploads
 *        them to cdn.pollis.com/releases/oss/<product>/<version>/). --root DIR
 *        describes the checkout at DIR instead of this one.
 *   node scripts/oss-inventory.mjs check-licenses
 *        JS licence gate: every npm package any product ships carries a licence
 *        on the allow-list (deny.toml's, plus supply-chain/js-licenses.json).
 *        Rust is gated by `cargo deny check licenses`.
 */

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readdirSync, realpathSync, statSync, writeFileSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

// TOOL: this checkout's licence data (supply-chain/, deny.toml).
// ROOT: the source tree being described (lockfiles, node_modules, git), which
// `--root` points at another checkout to describe an older release with
// today's licence data (.github/workflows/oss-evidence.yml).
const TOOL = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const ROOT = resolve(process.argv.includes("--root") ? process.argv[process.argv.indexOf("--root") + 1] : TOOL);
const SCRIPT_VERSION = "1";

// The crates and npm projects that make up each product, and how each is built.
// Keep these in step with the release recipes: src-tauri + the capture helpers
// (desktop-release.yml), pollis-core --no-default-features
// (mobile/modules/pollis-native/ubrn.config.yaml), pollis-tui (cli-release.yml),
// pollis-delivery + its front-door Worker (delivery-deploy-*.yml).
export const PRODUCTS = {
  desktop: {
    name: "Pollis Desktop",
    rust: {
      roots: ["pollis", "pollis-capture-linux", "pollis-capture-macos"],
      features: [],
      targets: ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-pc-windows-msvc"],
    },
    js: [{ dir: "frontend" }],
  },
  mobile: {
    name: "Pollis Mobile",
    rust: {
      roots: ["pollis-core"],
      features: ["--no-default-features"],
      targets: [
        "aarch64-linux-android",
        "armv7-linux-androideabi",
        "x86_64-linux-android",
        "aarch64-apple-ios",
      ],
    },
    js: [{ dir: "mobile" }],
  },
  cli: {
    name: "Pollis CLI",
    rust: {
      roots: ["pollis-tui"],
      features: [],
      targets: ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin", "x86_64-pc-windows-msvc"],
    },
    js: [],
  },
  ds: {
    name: "Pollis Delivery Service",
    rust: {
      roots: ["pollis-delivery"],
      features: [],
      targets: ["x86_64-unknown-linux-gnu"],
    },
    js: [{ dir: "pollis-delivery" }],
  },
};

// First-party code: our workspace crates and our own npm projects. They are the
// product, not a dependency of it, so they are walked through but never listed.
const FIRST_PARTY_CRATE = /^(pollis|verifiable-log)(-|$)/;

const LICENSE_FILE = /^(licen[cs]e|copying|notice|unlicense|copyright)([-._].*)?$/i;
const MAX_LICENSE_BYTES = 128 * 1024;

const args = process.argv.slice(2);
function arg(name) {
  const i = args.indexOf(name);
  if (i === -1) {
    return undefined;
  }
  return args[i + 1];
}

function die(msg) {
  console.error(`oss-inventory: ${msg}`);
  process.exit(1);
}

function sha256(data) {
  return createHash("sha256").update(data).digest("hex");
}

// Cargo's pre-SPDX `MIT/Apache-2.0` (and npm's `MIT / Apache-2.0`) mean OR.
export function normaliseLicense(expr) {
  if (!expr) {
    return null;
  }
  return expr.trim().replace(/\s*\/\s*/g, " OR ");
}

function spdxIds(ast) {
  if (ast.id) {
    return ast.id.split(" WITH ");
  }
  return (ast.or ?? ast.and).flatMap(spdxIds);
}

// The standard text of each licence in the expression, for a package that
// ships no licence file (supply-chain/license-texts/README.md).
function fallbackTexts(c) {
  const ast = c.license ? parseSpdx(c.license) : null;
  const texts = [];
  for (const id of ast ? spdxIds(ast) : []) {
    const f = join(TOOL, "supply-chain/license-texts", `${id}.txt`);
    if (existsSync(f)) {
      texts.push(`${id}\n\n${readFileSync(f, "utf8").trim()}`);
    }
  }
  if (texts.length === 0) {
    die(`${c.ecosystem}:${c.name}@${c.version} ships no licence file and "${c.license}" has no text in supply-chain/license-texts/`);
  }
  return texts;
}

function sortedBy(list, key) {
  return [...list].sort((a, b) => (key(a) < key(b) ? -1 : key(a) > key(b) ? 1 : 0));
}

// ---------------------------------------------------------------------------
// Licence files
// ---------------------------------------------------------------------------

function licenseTexts(dir, extraFile) {
  const texts = [];
  const seen = new Set();
  const add = (path) => {
    if (seen.has(path) || !existsSync(path)) {
      return;
    }
    seen.add(path);
    const st = statSync(path);
    if (!st.isFile() || st.size === 0) {
      return;
    }
    const raw = readFileSync(path);
    const text = raw.subarray(0, MAX_LICENSE_BYTES).toString("utf8").replace(/\r\n?/g, "\n").trim();
    if (text) {
      texts.push(text);
    }
  };
  if (extraFile) {
    add(resolve(dir, extraFile));
  }
  let entries = [];
  try {
    entries = readdirSync(dir).sort();
  } catch {
    return texts;
  }
  for (const name of entries) {
    if (LICENSE_FILE.test(name)) {
      add(join(dir, name));
    }
  }
  // Crates with several licences sometimes keep them in LICENSES/ (REUSE layout).
  for (const sub of ["LICENSES", "licenses"]) {
    const d = join(dir, sub);
    if (existsSync(d) && statSync(d).isDirectory()) {
      for (const name of readdirSync(d).sort()) {
        add(join(d, name));
      }
    }
  }
  return texts;
}

// ---------------------------------------------------------------------------
// Rust: cargo tree per target, joined to cargo metadata for licence + source
// ---------------------------------------------------------------------------

let cargoMetadataCache;
function cargoMetadata() {
  if (!cargoMetadataCache) {
    const out = execFileSync("cargo", ["metadata", "--format-version", "1", "--locked"], {
      cwd: ROOT,
      maxBuffer: 256 * 1024 * 1024,
      stdio: ["ignore", "pipe", "inherit"],
    });
    cargoMetadataCache = JSON.parse(out.toString("utf8"));
  }
  return cargoMetadataCache;
}

// Cargo.lock checksums, keyed `name@version`. Only registry crates have one.
function cargoLockChecksums() {
  const lock = readFileSync(join(ROOT, "Cargo.lock"), "utf8");
  const sums = new Map();
  for (const block of lock.split(/\n\[\[package\]\]\n/)) {
    const name = /^name = "([^"]+)"/m.exec(block)?.[1];
    const version = /^version = "([^"]+)"/m.exec(block)?.[1];
    const checksum = /^checksum = "([0-9a-f]{64})"/m.exec(block)?.[1];
    if (name && version && checksum) {
      sums.set(`${name}@${version}`, checksum);
    }
  }
  return sums;
}

// `{p}` renders as `name v1.2.3`, `name v1.2.3 (/path)` or
// `name v1.2.3 (https://github.com/o/r?rev=…#short)`, and `(*)` marks a subtree
// already printed above.
function parseTreeLine(line) {
  const m = /^(\d+)(\S+) v(\S+)(?: \(([^)]*)\))?/.exec(line);
  if (!m) {
    return null;
  }
  return { depth: Number(m[1]), name: m[2], version: m[3], where: m[4] ?? null };
}

function rustGraph(spec) {
  const meta = cargoMetadata();
  const byNameVersion = new Map();
  for (const p of meta.packages) {
    const key = `${p.name}@${p.version}`;
    const prev = byNameVersion.get(key);
    // Two sources at one name+version is possible in principle; prefer the
    // registry copy, which is what a purl without qualifiers names.
    if (!prev || (p.source ?? "").startsWith("registry+")) {
      byNameVersion.set(key, p);
    }
  }
  const nodes = new Set();
  const edges = new Set();
  for (const target of spec.targets) {
    const cmd = ["tree", "--locked", "-e", "normal,no-proc-macro", "--prefix", "depth", "--format", "{p}", "--target", target, ...spec.features];
    for (const r of spec.roots) {
      cmd.push("-p", r);
    }
    const out = execFileSync("cargo", cmd, { cwd: ROOT, maxBuffer: 64 * 1024 * 1024, stdio: ["ignore", "pipe", "inherit"] }).toString("utf8");
    const stack = [];
    for (const line of out.split("\n")) {
      const n = parseTreeLine(line);
      if (!n) {
        continue;
      }
      const key = `${n.name}@${n.version}`;
      stack.length = n.depth;
      stack.push(key);
      nodes.add(key);
      if (n.depth > 0) {
        edges.add(`${stack[n.depth - 1]}>${key}`);
      }
    }
  }
  const sums = cargoLockChecksums();
  const components = new Map();
  for (const key of nodes) {
    const p = byNameVersion.get(key);
    if (!p) {
      die(`cargo tree listed ${key}, which cargo metadata does not know`);
    }
    const firstParty = FIRST_PARTY_CRATE.test(p.name) && (p.source == null);
    const dir = dirname(p.manifest_path);
    let purl = `pkg:cargo/${p.name}@${p.version}`;
    const source = p.source ?? "";
    if (source.startsWith("git+")) {
      const url = source.slice(4).replace(/#.*$/, "");
      purl += `?vcs_url=${encodeURIComponent(`git+${url}`)}`;
    }
    components.set(key, {
      key,
      ecosystem: "cargo",
      name: p.name,
      version: p.version,
      purl,
      firstParty,
      license: normaliseLicense(p.license) ?? (p.license_file ? `LicenseRef-file-${basename(p.license_file)}` : null),
      sha256: sums.get(key) ?? null,
      homepage: p.repository ?? p.homepage ?? null,
      texts: firstParty ? [] : licenseTexts(dir, p.license_file),
    });
  }
  return { components, edges };
}

// ---------------------------------------------------------------------------
// JS: Node module resolution over the installed tree
// ---------------------------------------------------------------------------

function findPackageDir(name, fromDir) {
  let d = fromDir;
  for (;;) {
    if (basename(d) !== "node_modules") {
      const candidate = join(d, "node_modules", name);
      if (existsSync(join(candidate, "package.json"))) {
        return realpathSync(candidate);
      }
    }
    const up = dirname(d);
    if (up === d) {
      return null;
    }
    d = up;
  }
}

// npm's licence field has three historical shapes; normalise to one string.
function npmLicense(pkg) {
  const l = pkg.license ?? pkg.licenses;
  if (typeof l === "string") {
    return l;
  }
  if (Array.isArray(l)) {
    const ids = l.map((x) => (typeof x === "string" ? x : x?.type)).filter(Boolean);
    return ids.length > 1 ? `(${ids.join(" OR ")})` : ids[0] ?? null;
  }
  if (l && typeof l === "object" && l.type) {
    return l.type;
  }
  return null;
}

function npmPurl(name, version) {
  const enc = name.startsWith("@") ? `%40${name.slice(1)}` : name;
  return `pkg:npm/${enc}@${version}`;
}

export function jsGraph(projects) {
  const components = new Map();
  const edges = new Set();
  const missing = [];
  const visited = new Map();

  const visit = (dir, isRoot, onlyRuntime) => {
    if (visited.has(dir)) {
      return visited.get(dir);
    }
    const pkg = JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
    // Anything outside node_modules (a workspace project, a `link:` module) is ours.
    const firstParty = isRoot || !dir.split(/[\\/]/).includes("node_modules");
    const key = firstParty ? `first-party:${dir}` : `${pkg.name}@${pkg.version}`;
    visited.set(dir, key);
    if (!firstParty && !components.has(key)) {
      components.set(key, {
        key,
        ecosystem: "npm",
        name: pkg.name,
        version: pkg.version,
        purl: npmPurl(pkg.name, pkg.version),
        firstParty: false,
        license: normaliseLicense(npmLicense(pkg)),
        sha256: null,
        homepage: typeof pkg.repository === "string" ? pkg.repository : pkg.repository?.url ?? pkg.homepage ?? null,
        texts: licenseTexts(dir),
      });
    }
    const deps = [
      ...Object.keys(pkg.dependencies ?? {}).map((n) => [n, false]),
      ...Object.keys(pkg.optionalDependencies ?? {}).map((n) => [n, true]),
    ];
    // Peers are what the package runs against; pnpm links the resolved peer into
    // its node_modules context, so include the ones that resolve.
    if (!onlyRuntime) {
      for (const n of Object.keys(pkg.peerDependencies ?? {})) {
        deps.push([n, true]);
      }
    }
    for (const [name, optional] of deps) {
      const childDir = findPackageDir(name, dir);
      if (!childDir) {
        if (!optional) {
          missing.push(`${pkg.name ?? dir} -> ${name}`);
        }
        continue;
      }
      const childKey = visit(childDir, false, false);
      if (!firstParty && !childKey.startsWith("first-party:")) {
        edges.add(`${key}>${childKey}`);
      }
    }
    return key;
  };

  for (const p of projects) {
    visit(realpathSync(resolve(ROOT, p.dir)), true, true);
  }
  if (missing.length > 0) {
    die(`npm dependencies not installed (run pnpm install first):\n  ${missing.slice(0, 20).join("\n  ")}`);
  }
  return { components, edges };
}

// ---------------------------------------------------------------------------
// Inventory
// ---------------------------------------------------------------------------

export function inventory(productId) {
  const spec = PRODUCTS[productId];
  if (!spec) {
    die(`unknown product "${productId}" (one of: ${Object.keys(PRODUCTS).join(", ")})`);
  }
  const rust = rustGraph(spec.rust);
  const js = spec.js.length > 0 ? jsGraph(spec.js) : { components: new Map(), edges: new Set() };
  const all = new Map([...rust.components, ...js.components]);
  const components = sortedBy(
    [...all.values()].filter((c) => !c.firstParty),
    (c) => `${c.ecosystem}:${c.name}@${c.version}`,
  );
  const thirdParty = new Set(components.map((c) => c.key));
  const deps = new Map();
  for (const e of [...rust.edges, ...js.edges]) {
    const [from, to] = e.split(">");
    if (thirdParty.has(from) && thirdParty.has(to)) {
      if (!deps.has(from)) {
        deps.set(from, new Set());
      }
      deps.get(from).add(to);
    }
  }
  return { productId, spec, components, deps };
}

function sourceDateIso() {
  const epoch = process.env.SOURCE_DATE_EPOCH;
  if (epoch) {
    return new Date(Number(epoch) * 1000).toISOString().replace(/\.\d{3}Z$/, "Z");
  }
  try {
    const iso = execFileSync("git", ["log", "-1", "--format=%cI"], { cwd: ROOT }).toString().trim();
    return new Date(iso).toISOString().replace(/\.\d{3}Z$/, "Z");
  } catch {
    return new Date().toISOString().replace(/\.\d{3}Z$/, "Z");
  }
}

// Deterministic UUID (RFC 4122 layout) from a digest, so the same inputs give
// the same SBOM byte for byte.
function uuidFrom(hex) {
  const h = hex.slice(0, 32).split("");
  h[12] = "5";
  h[16] = ((parseInt(h[16], 16) & 0x3) | 0x8).toString(16);
  const s = h.join("");
  return `${s.slice(0, 8)}-${s.slice(8, 12)}-${s.slice(12, 16)}-${s.slice(16, 20)}-${s.slice(20)}`;
}

function cdxLicenses(expr) {
  if (!expr) {
    return undefined;
  }
  if (parseSpdx(expr)) {
    return [{ expression: expr }];
  }
  return [{ license: { name: expr } }];
}

export function cyclonedx(inv, version) {
  const timestamp = sourceDateIso();
  const productRef = `pollis-${inv.productId}`;
  const digest = sha256(JSON.stringify([inv.productId, version, inv.components.map((c) => [c.purl, c.license, c.sha256])]));
  const components = inv.components.map((c) => {
    const out = {
      type: "library",
      "bom-ref": c.purl,
      name: c.name,
      version: c.version,
      purl: c.purl,
    };
    const licenses = cdxLicenses(c.license);
    if (licenses) {
      out.licenses = licenses;
    }
    if (c.sha256) {
      out.hashes = [{ alg: "SHA-256", content: c.sha256 }];
    }
    if (c.homepage) {
      const url = c.homepage.replace(/^git\+/, "");
      if (/^https?:\/\//.test(url)) {
        out.externalReferences = [{ type: "vcs", url }];
      }
    }
    return out;
  });
  const refOf = new Map(inv.components.map((c) => [c.key, c.purl]));
  const dependsOnFromRoot = inv.components.filter((c) => ![...inv.deps.values()].some((s) => s.has(c.key))).map((c) => c.purl);
  const dependencies = [
    { ref: productRef, dependsOn: dependsOnFromRoot },
    ...inv.components.map((c) => ({
      ref: c.purl,
      dependsOn: [...(inv.deps.get(c.key) ?? [])].map((k) => refOf.get(k)).sort(),
    })),
  ];
  return {
    bomFormat: "CycloneDX",
    specVersion: "1.6",
    serialNumber: `urn:uuid:${uuidFrom(digest)}`,
    version: 1,
    metadata: {
      timestamp,
      lifecycles: [{ phase: "build" }],
      tools: {
        components: [{ type: "application", name: "pollis oss-inventory", version: SCRIPT_VERSION }],
      },
      component: {
        type: "application",
        "bom-ref": productRef,
        name: inv.spec.name,
        version,
        supplier: { name: "Pollis", url: ["https://pollis.com"] },
      },
    },
    components,
    dependencies,
  };
}

// ---------------------------------------------------------------------------
// Summary: the small index pollis.com/open-source reads for each product
// ---------------------------------------------------------------------------

export const CDN_BASE = "https://cdn.pollis.com/releases/oss";

export function evidenceNames(productId, version) {
  const stem = `pollis-${productId}-${version}`;
  return { sbom: `${stem}.cdx.json` };
}

export function summary(inv, version) {
  const names = evidenceNames(inv.productId, version);
  const base = `${CDN_BASE}/${inv.productId}/${version}`;
  const licenses = new Map();
  for (const c of inv.components) {
    const l = c.license ?? "UNKNOWN";
    licenses.set(l, (licenses.get(l) ?? 0) + 1);
  }
  return {
    schema: 1,
    product: inv.productId,
    name: inv.spec.name,
    version,
    generated: sourceDateIso(),
    sbom: `${base}/${names.sbom}`,
    components: {
      total: inv.components.length,
      cargo: inv.components.filter((c) => c.ecosystem === "cargo").length,
      npm: inv.components.filter((c) => c.ecosystem === "npm").length,
    },
    licenses: sortedBy([...licenses].map(([license, count]) => ({ license, count })), (x) => `${String(1e6 - x.count).padStart(7, "0")}${x.license}`),
  };
}

// ---------------------------------------------------------------------------
// Notices (what the apps render)
// ---------------------------------------------------------------------------

export function notices(inv) {
  const texts = [];
  const index = new Map();
  const packages = inv.components.map((c) => {
    const own = c.texts.length > 0 ? c.texts : fallbackTexts(c);
    const ids = own.map((t) => {
      if (!index.has(t)) {
        index.set(t, texts.length);
        texts.push(t);
      }
      return index.get(t);
    });
    return { name: c.name, version: c.version, ecosystem: c.ecosystem, license: c.license ?? "UNKNOWN", texts: ids };
  });
  return { schema: 1, product: inv.productId, packages, texts };
}

// ---------------------------------------------------------------------------
// SPDX expressions + the JS licence gate
// ---------------------------------------------------------------------------

// Returns an AST, or null when the string is not a valid SPDX expression.
export function parseSpdx(expr) {
  const tokens = expr.replace(/\(/g, " ( ").replace(/\)/g, " ) ").trim().split(/\s+/);
  let i = 0;
  const peek = () => tokens[i];
  const id = /^(LicenseRef-[A-Za-z0-9.-]+|[A-Za-z0-9][A-Za-z0-9.+-]*)$/;
  const primary = () => {
    const t = tokens[i];
    if (t === "(") {
      i += 1;
      const e = orExpr();
      if (tokens[i] !== ")") {
        throw new Error("unbalanced");
      }
      i += 1;
      return e;
    }
    if (!t || !id.test(t) || ["AND", "OR", "WITH"].includes(t.toUpperCase())) {
      throw new Error(`unexpected ${t}`);
    }
    i += 1;
    if (peek()?.toUpperCase() === "WITH") {
      i += 1;
      const exc = tokens[i];
      if (!exc || !id.test(exc)) {
        throw new Error("bad exception");
      }
      i += 1;
      return { id: `${t} WITH ${exc}` };
    }
    return { id: t };
  };
  const andExpr = () => {
    let left = primary();
    while (peek()?.toUpperCase() === "AND") {
      i += 1;
      left = { and: [left, primary()] };
    }
    return left;
  };
  const orExpr = () => {
    let left = andExpr();
    while (peek()?.toUpperCase() === "OR") {
      i += 1;
      left = { or: [left, andExpr()] };
    }
    return left;
  };
  try {
    const ast = orExpr();
    return i === tokens.length ? ast : null;
  } catch {
    return null;
  }
}

export function spdxAllowed(ast, allowed) {
  if (ast.id) {
    return allowed.has(ast.id);
  }
  if (ast.or) {
    return ast.or.some((x) => spdxAllowed(x, allowed));
  }
  return ast.and.every((x) => spdxAllowed(x, allowed));
}

// deny.toml's [licenses] allow-list, so Rust and JS answer to one list.
function denyAllowList() {
  const deny = readFileSync(join(TOOL, "deny.toml"), "utf8");
  const section = /^\[licenses\]\s*$([\s\S]*?)^\[/m.exec(deny)?.[1];
  const block = section && /^allow\s*=\s*\[([\s\S]*?)^\]/m.exec(section)?.[1];
  if (!block) {
    die("could not find [licenses] allow = [...] in deny.toml");
  }
  return block
    .split("\n")
    .map((l) => l.replace(/#.*$/, ""))
    .flatMap((l) => [...l.matchAll(/"([^"]+)"/g)].map((m) => m[1]));
}

function checkLicenses() {
  const cfg = JSON.parse(readFileSync(join(TOOL, "supply-chain/js-licenses.json"), "utf8"));
  const allowed = new Set([...denyAllowList(), ...cfg.allow.map((a) => a.license)]);
  const overrides = new Map(cfg.packages.map((p) => [p.package, p]));
  const used = new Set();
  const failures = [];
  let checked = 0;
  for (const productId of Object.keys(PRODUCTS)) {
    if (PRODUCTS[productId].js.length === 0) {
      continue;
    }
    const { components } = jsGraph(PRODUCTS[productId].js);
    for (const c of components.values()) {
      checked += 1;
      const o = overrides.get(`${c.name}@${c.version}`) ?? overrides.get(c.name);
      let expr = c.license;
      if (o) {
        used.add(o.package);
        if (o.license_found && o.license_found !== c.license) {
          failures.push(`${productId}: ${c.name}@${c.version} now declares "${c.license}", but supply-chain/js-licenses.json reviewed "${o.license_found}" — re-review the override`);
          continue;
        }
        expr = o.license;
      }
      const ast = expr ? parseSpdx(expr) : null;
      if (!ast) {
        failures.push(`${productId}: ${c.name}@${c.version} has no valid SPDX licence ("${c.license ?? ""}") — add a reviewed entry to supply-chain/js-licenses.json "packages"`);
        continue;
      }
      if (!spdxAllowed(ast, allowed)) {
        failures.push(`${productId}: ${c.name}@${c.version} is "${expr}", not on the allow-list (deny.toml [licenses] + supply-chain/js-licenses.json)`);
      }
    }
  }
  for (const p of cfg.packages) {
    if (!used.has(p.package)) {
      failures.push(`supply-chain/js-licenses.json: override for "${p.package}" matches no shipped package any more — remove it`);
    }
  }
  const unique = [...new Set(failures)];
  if (unique.length > 0) {
    for (const f of unique) {
      console.error(`::error::${f}`);
    }
    die(`${unique.length} licence problem(s)`);
  }
  console.log(`JS licences OK: ${checked} package instances across ${Object.keys(PRODUCTS).filter((p) => PRODUCTS[p].js.length).join(", ")}`);
}

// ---------------------------------------------------------------------------

function writeJson(path, value) {
  mkdirSync(dirname(resolve(path)), { recursive: true });
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`);
}

function main() {
  const mode = args[0];
  if (mode === "check-licenses") {
    checkLicenses();
    return;
  }
  if (!mode || !PRODUCTS[mode]) {
    die(`usage: oss-inventory.mjs <${Object.keys(PRODUCTS).join("|")}> [--version V] [--sbom F] [--notices F] [--summary F] [--out-dir D] | check-licenses`);
  }
  const version = arg("--version") ?? "0.0.0-dev";
  const inv = inventory(mode);
  const sbomPath = arg("--sbom");
  const noticesPath = arg("--notices");
  const summaryPath = arg("--summary");
  // --out-dir writes both published files under their published names.
  const outDir = arg("--out-dir");
  const names = evidenceNames(mode, version);
  const sbomOut = sbomPath ?? (outDir && join(outDir, names.sbom));
  const summaryOut = summaryPath ?? (outDir && join(outDir, "summary.json"));
  if (sbomOut) {
    writeJson(sbomOut, cyclonedx(inv, version));
  }
  if (summaryOut) {
    writeJson(summaryOut, summary(inv, version));
  }
  if (noticesPath) {
    writeJson(noticesPath, notices(inv));
  }
  const missingTexts = inv.components.filter((c) => c.texts.length === 0).length;
  console.error(`${PRODUCTS[mode].name}: ${inv.components.length} third-party packages (${missingTexts} ship no licence file)`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main();
}
