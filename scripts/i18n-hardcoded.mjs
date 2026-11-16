#!/usr/bin/env node
/**
 * Hardcoded-copy lint (#1724): user-facing string literals in the desktop
 * renderer and the mobile app that never went through `t()`.
 *
 * Parses every scanned `.tsx` with the TypeScript compiler's parser (syntax
 * only, no type check) and reports:
 *   - JSX text children that contain a letter;
 *   - string literals (including template literals and the branches of `?:`,
 *     `||` and `??`) in the props in ATTRIBUTES, and in JSX child expressions;
 *   - the same in the message arguments of the CALLS (error setters, alerts).
 *
 * The reviewed exemptions (brand names, dev-only files) and the ratchet
 * baseline live in scripts/i18n-hardcoded.json. A violation not in the
 * baseline fails; a baseline entry that no longer occurs fails too, so a fix
 * shrinks the baseline in the same PR (`--update-baseline` rewrites it).
 *
 * Run through `node scripts/i18n-check.mjs`, or directly:
 *   node scripts/i18n-hardcoded.mjs                    check against the baseline
 *   node scripts/i18n-hardcoded.mjs --list [path...]   print every violation
 *   node scripts/i18n-hardcoded.mjs --update-baseline  rewrite the baseline
 */

import { readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export const CONFIG_PATH = join(ROOT, "scripts/i18n-hardcoded.json");

// Scanned trees, per app. Only `.tsx`: that is where JSX copy lives.
export const APPS = {
  desktop: ["frontend/src"],
  mobile: ["mobile/app", "mobile/components", "mobile/hooks"],
};

// Props whose value a person reads or hears.
export const ATTRIBUTES = new Set([
  "placeholder",
  "title",
  "aria-label",
  "alt",
  "accessibilityLabel",
  "accessibilityHint",
  "label",
]);

// Calls whose arguments are shown to the user: error/notice setters and native alerts.
const SETTER = /^set[A-Z]\w*(Error|Message|Notice|Warning|Hint)$/;
const CALLS = new Set(["setError", "Alert.alert", "Alert.prompt"]);

const LETTER = /\p{L}/u;
const ENTITY = /&(#\d+|#x[0-9a-f]+|[a-z]+);/gi;

let tsModule;
/** The TypeScript compiler, from whichever app's node_modules is installed (CI installs one of them). */
export function typescript() {
  if (tsModule) {
    return tsModule;
  }
  for (const pkg of ["frontend/package.json", "mobile/package.json", "package.json"]) {
    try {
      tsModule = createRequire(join(ROOT, pkg))("typescript");
      return tsModule;
    } catch {
      // Try the next install.
    }
  }
  throw new Error(
    "i18n-hardcoded: the `typescript` package is not installed — run `pnpm install --filter ./frontend...` (or install mobile/)",
  );
}

function tsxFiles(dir, acc = []) {
  let entries;
  try {
    entries = readdirSync(dir);
  } catch {
    return acc;
  }
  for (const entry of entries) {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) {
      if (!["node_modules", "__mocks__", "locales", "generated"].includes(entry)) {
        tsxFiles(path, acc);
      }
    } else if (entry.endsWith(".tsx") && !/\.(test|spec|stories)\.tsx$/.test(entry)) {
      acc.push(path);
    }
  }
  return acc;
}

/** Collapse whitespace so a reflowed JSX line keeps its baseline entry. */
function normalise(text) {
  return text.replace(/\s+/g, " ").trim();
}

function calleeName(ts, expr) {
  if (ts.isIdentifier(expr)) {
    return expr.text;
  }
  if (ts.isPropertyAccessExpression(expr) && ts.isIdentifier(expr.expression)) {
    return `${expr.expression.text}.${expr.name.text}`;
  }
  return null;
}

/**
 * String values an expression can evaluate to directly: literals, templates
 * (with `{…}` for each substitution), and both sides of `?:`, `||`, `??`, `&&`.
 * A call (`t(...)`), identifier or member access yields nothing.
 */
function literalStrings(ts, node, out = []) {
  if (!node) {
    return out;
  }
  if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) {
    out.push({ node, text: node.text });
  } else if (ts.isTemplateExpression(node)) {
    const text = node.head.text + node.templateSpans.map((s) => `{…}${s.literal.text}`).join("");
    out.push({ node, text });
  } else if (ts.isParenthesizedExpression(node) || ts.isAsExpression(node)) {
    literalStrings(ts, node.expression, out);
  } else if (ts.isConditionalExpression(node)) {
    literalStrings(ts, node.whenTrue, out);
    literalStrings(ts, node.whenFalse, out);
  } else if (ts.isBinaryExpression(node)) {
    const op = node.operatorToken.kind;
    const K = ts.SyntaxKind;
    if (op === K.BarBarToken || op === K.QuestionQuestionToken) {
      literalStrings(ts, node.left, out);
      literalStrings(ts, node.right, out);
    } else if (op === K.AmpersandAmpersandToken) {
      literalStrings(ts, node.right, out);
    } else if (op === K.PlusToken) {
      // "Signed in as " + name: concatenated copy is still copy (and untranslatable as written).
      const left = literalStrings(ts, node.left);
      const right = literalStrings(ts, node.right);
      if (left.length || right.length) {
        const side = (parts) => (parts.length ? parts.map((p) => p.text).join("{…}") : "{…}");
        out.push({ node, text: side(left) + side(right) });
      }
    }
  }
  return out;
}

/** Every user-facing literal in one file: `{ line, kind, text }`. */
export function scanSource(source, fileName = "file.tsx") {
  const ts = typescript();
  const sf = ts.createSourceFile(fileName, source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const found = [];
  const add = (node, kind, raw) => {
    const text = normalise(raw);
    // Symbols and punctuation only (`/`, `•`, `&lt;`) are not copy: an HTML entity's name is no letter.
    if (!LETTER.test(text.replace(ENTITY, ""))) {
      return;
    }
    const { line } = sf.getLineAndCharacterOfPosition(node.getStart(sf));
    found.push({ line: line + 1, kind, text });
  };

  const visit = (node) => {
    if (ts.isJsxText(node)) {
      add(node, "jsx-text", node.text);
    } else if (ts.isJsxAttribute(node)) {
      const name = node.name.getText(sf);
      if (ATTRIBUTES.has(name) && node.initializer) {
        if (ts.isStringLiteral(node.initializer)) {
          add(node.initializer, `prop ${name}`, node.initializer.text);
        } else if (ts.isJsxExpression(node.initializer)) {
          for (const s of literalStrings(ts, node.initializer.expression)) {
            add(s.node, `prop ${name}`, s.text);
          }
        }
      }
      // Attribute values are fully handled above; nothing below them is JSX copy.
      return;
    } else if (ts.isJsxExpression(node) && node.parent && (ts.isJsxElement(node.parent) || ts.isJsxFragment(node.parent))) {
      for (const s of literalStrings(ts, node.expression)) {
        add(s.node, "jsx-expression", s.text);
      }
    } else if (ts.isCallExpression(node)) {
      const name = calleeName(ts, node.expression);
      if (name && (CALLS.has(name) || SETTER.test(name))) {
        for (const arg of node.arguments) {
          for (const s of literalStrings(ts, arg)) {
            add(s.node, `call ${name}`, s.text);
          }
        }
      }
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
  return found;
}

export function loadConfig(path = CONFIG_PATH) {
  return JSON.parse(readFileSync(path, "utf8"));
}

/**
 * Scan every app tree. Returns `{ violations, files, unusedAllow, missingIgnored }`;
 * `violations` are `{ app, file, line, kind, text }` after the config's exemptions.
 */
export function scan(config = loadConfig(), root = ROOT) {
  const allow = new Set(Object.keys(config.allow ?? {}));
  const usedAllow = new Set();
  const ignored = Object.keys(config.ignore_files ?? {});
  const seenIgnored = new Set();
  const violations = [];
  let files = 0;
  for (const [app, dirs] of Object.entries(APPS)) {
    for (const file of dirs.flatMap((d) => tsxFiles(join(root, d)))) {
      const rel = relative(root, file);
      if (ignored.includes(rel)) {
        seenIgnored.add(rel);
        continue;
      }
      files += 1;
      for (const v of scanSource(readFileSync(file, "utf8"), rel)) {
        if (allow.has(v.text)) {
          usedAllow.add(v.text);
        } else {
          violations.push({ app, file: rel, ...v });
        }
      }
    }
  }
  return {
    violations,
    files,
    unusedAllow: [...allow].filter((t) => !usedAllow.has(t)),
    missingIgnored: ignored.filter((f) => !seenIgnored.has(f)),
  };
}

/** `{ file: { text: count } }`, sorted, for the baseline. */
export function tally(violations) {
  const out = {};
  for (const v of [...violations].sort((a, b) => a.file.localeCompare(b.file) || a.text.localeCompare(b.text))) {
    out[v.file] ??= {};
    out[v.file][v.text] = (out[v.file][v.text] ?? 0) + 1;
  }
  return out;
}

/**
 * Compare a scan against the baseline. `added` are violations beyond the
 * baseline's count for that file and text (the LAST occurrences, by line);
 * `stale` are baseline entries that occur fewer times than recorded.
 */
export function compare(violations, baseline) {
  const now = tally(violations);
  const added = [];
  const stale = [];
  const byKey = new Map();
  for (const v of violations) {
    const key = `${v.file}\0${v.text}`;
    byKey.set(key, [...(byKey.get(key) ?? []), v]);
  }
  for (const [key, list] of byKey) {
    const [file, text] = key.split("\0");
    const allowed = baseline[file]?.[text] ?? 0;
    if (list.length > allowed) {
      added.push(...list.slice(allowed));
    }
  }
  for (const [file, texts] of Object.entries(baseline)) {
    for (const [text, count] of Object.entries(texts)) {
      const have = now[file]?.[text] ?? 0;
      if (have < count) {
        stale.push({ file, text, count, have });
      }
    }
  }
  added.sort((a, b) => a.file.localeCompare(b.file) || a.line - b.line);
  return { added, stale };
}

export function countByApp(baseline) {
  const out = Object.fromEntries(Object.keys(APPS).map((a) => [a, 0]));
  for (const [file, texts] of Object.entries(baseline)) {
    const app = Object.entries(APPS).find(([, dirs]) => dirs.some((d) => file.startsWith(`${d}/`)))?.[0];
    if (app) {
      out[app] += Object.values(texts).reduce((a, b) => a + b, 0);
    }
  }
  return out;
}

/**
 * The check `i18n-check.mjs` runs. `fail(message)` records a failure; returns
 * nothing. Prints the per-app baseline size.
 */
export function check({ fail, log = console.log } = {}) {
  const config = loadConfig();
  const { violations, files, unusedAllow, missingIgnored } = scan(config);
  if (files === 0) {
    fail("hardcoded-copy lint scanned no files — has a directory in APPS (scripts/i18n-hardcoded.mjs) moved?");
    return;
  }
  const baseline = config.baseline ?? {};
  const { added, stale } = compare(violations, baseline);
  const sizes = countByApp(baseline);
  log(
    `  ${files} files scanned · baseline ${Object.entries(sizes).map(([a, n]) => `${a} ${n}`).join(", ")}`,
  );
  for (const v of added) {
    fail(
      `${v.file}:${v.line}: hardcoded ${v.kind}: ${JSON.stringify(v.text)} — move it into a catalogue and render it with t() (or, for a brand name, add it to "allow" in scripts/i18n-hardcoded.json with a reason)`,
    );
  }
  for (const text of unusedAllow) {
    fail(`scripts/i18n-hardcoded.json: allow entry ${JSON.stringify(text)} matches nothing any more — remove it`);
  }
  for (const file of missingIgnored) {
    fail(`scripts/i18n-hardcoded.json: ignore_files entry ${file} is not a scanned .tsx file — remove it or fix the path`);
  }
  if (stale.length) {
    fail(
      `${stale.length} baseline entr${stale.length === 1 ? "y is" : "ies are"} fixed — shrink the baseline: node scripts/i18n-hardcoded.mjs --update-baseline\n` +
        stale.slice(0, 10).map((s) => `      ${s.file}: ${JSON.stringify(s.text)} (${s.count} → ${s.have})`).join("\n"),
    );
  }
}

function main() {
  const args = process.argv.slice(2);
  const config = loadConfig();
  if (args.includes("--update-baseline")) {
    const { violations } = scan(config);
    config.baseline = tally(violations);
    writeFileSync(CONFIG_PATH, `${JSON.stringify(config, null, 2)}\n`);
    const sizes = countByApp(config.baseline);
    console.log(`baseline rewritten: ${Object.entries(sizes).map(([a, n]) => `${a} ${n}`).join(", ")}`);
    return;
  }
  if (args.includes("--list")) {
    const filters = args.filter((a) => a !== "--list");
    const { violations } = scan(config);
    for (const v of violations.filter((v) => !filters.length || filters.some((f) => v.file.startsWith(f)))) {
      console.log(`${v.file}:${v.line}\t${v.kind}\t${JSON.stringify(v.text)}`);
    }
    return;
  }
  let failures = 0;
  check({
    fail: (m) => {
      failures += 1;
      console.error(`  ✗ ${m}`);
    },
  });
  console.log(failures === 0 ? "OK — no new hardcoded copy." : `FAILED — ${failures} problem(s).`);
  process.exit(failures === 0 ? 0 : 1);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main();
}
