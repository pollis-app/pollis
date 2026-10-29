#!/usr/bin/env node
/**
 * The component inventory in `.codesight/wiki/ui.md`, generated (#933).
 *
 * Owns exactly one region of the article, fenced by the markers below; the rest is prose.
 *
 *   node scripts/ui-inventory.mjs           # rewrite the inventory in place
 *   node scripts/ui-inventory.mjs --check   # exit 1 if it is out of date
 *
 * Text after an entry's backticked path is a hand-written note, carried forward verbatim.
 */

import { readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join, relative, basename, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = fileURLToPath(new URL("..", import.meta.url));
const SRC = join(ROOT, "frontend/src");
const ARTICLE = join(ROOT, ".codesight/wiki/ui.md");

const BEGIN = "<!-- BEGIN GENERATED: component inventory (scripts/ui-inventory.mjs) -->";
const END = "<!-- END GENERATED: component inventory -->";

function walk(dir) {
  const out = [];
  for (const entry of readdirSync(dir).sort()) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) {
      out.push(...walk(full));
    } else if (entry.endsWith(".tsx")) {
      out.push(full);
    }
  }
  return out;
}

/** The name this file is about: the export matching the filename, else the filename. */
function componentName(source, file) {
  const stem = basename(file, ".tsx");
  const exported = new Set();
  const patterns = [
    /export\s+(?:const|function|class)\s+([A-Za-z_$][\w$]*)/g,
    /export\s+default\s+(?:function\s+)?([A-Z][\w$]*)/g,
  ];
  for (const re of patterns) {
    let m;
    while ((m = re.exec(source)) !== null) {
      exported.add(m[1]);
    }
  }
  if (exported.has(stem)) {
    return stem;
  }
  // `Channel.tsx` exporting `ChannelPage`, `DM.tsx` exporting `DMPage`, …
  const suffixed = [...exported].find(
    (n) => n.startsWith(stem) || stem.startsWith(n),
  );
  if (suffixed) {
    return suffixed;
  }
  const firstPascal = [...exported].find((n) => /^[A-Z]/.test(n));
  return firstPascal ?? stem;
}

/** Top-level fields of the `<Name>Props` type only (a file's first *Props is often a sub-component's). */
function propNames(source, name) {
  const re = new RegExp(
    `(?:interface|type)\\s+${name}Props\\b[^{]*\\{`,
    "m",
  );
  const start = source.match(re);
  if (!start) {
    return [];
  }
  const open = source.indexOf("{", start.index);
  let depth = 0;
  let end = open;
  for (let i = open; i < source.length; i++) {
    if (source[i] === "{") {
      depth++;
    } else if (source[i] === "}") {
      depth--;
      if (depth === 0) {
        end = i;
        break;
      }
    }
  }
  const body = source.slice(open + 1, end);

  // Strip comments and nested braces so only this type's own fields remain.
  const flat = body
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/\/\/[^\n]*/g, "")
    .replace(/\{[^{}]*\}/g, "{}");

  const fields = [];
  for (const line of flat.split("\n")) {
    const m = line.match(/^\s*(?:readonly\s+)?([A-Za-z_$][\w$]*)\s*\??\s*:/);
    if (m && !fields.includes(m[1])) {
      fields.push(m[1]);
    }
  }
  return fields;
}

/** Directory heading a file belongs under, relative to `frontend/src`. */
function groupKey(file) {
  const dir = relative(SRC, dirname(file)).split("\\").join("/");
  return dir === "" ? "(root)" : dir;
}

export function collect() {
  const groups = new Map();
  for (const file of walk(SRC)) {
    const source = readFileSync(file, "utf8");
    const name = componentName(source, file);
    const entry = {
      name,
      path: relative(ROOT, file).split("\\").join("/"),
      props: propNames(source, name),
    };
    const key = groupKey(file);
    if (!groups.has(key)) {
      groups.set(key, []);
    }
    groups.get(key).push(entry);
  }
  for (const list of groups.values()) {
    list.sort((a, b) => a.name.localeCompare(b.name));
  }
  return new Map([...groups].sort(([a], [b]) => a.localeCompare(b)));
}

/** Hand-written notes from the existing block, keyed by component name. */
export function existingNotes(article) {
  const notes = new Map();
  const block = between(article);
  if (block === null) {
    return notes;
  }
  for (const line of block.split("\n")) {
    const m = line.match(/^- \*\*(.+?)\*\* — .*?`[^`]+`(.*)$/);
    if (m && m[2].trim()) {
      notes.set(m[1], m[2]);
    }
  }
  return notes;
}

function between(article) {
  const start = article.indexOf(BEGIN);
  const end = article.indexOf(END);
  if (start === -1 || end === -1) {
    return null;
  }
  return article.slice(start + BEGIN.length, end);
}

export function renderBlock(groups, notes) {
  const total = [...groups.values()].reduce((n, l) => n + l.length, 0);
  const lines = [
    "",
    `**${total} \`.tsx\` files** under \`frontend/src\`, by directory. Regenerate with`,
    "`node scripts/ui-inventory.mjs`; `--check` fails if this is stale.",
    "",
  ];
  for (const [dir, entries] of groups) {
    lines.push(`### \`${dir}\` (${entries.length})`, "");
    for (const e of entries) {
      const props = e.props.length ? ` — props: ${e.props.join(", ")}` : "";
      const note = notes.get(e.name) ?? "";
      lines.push(`- **${e.name}**${props} — \`${e.path}\`${note}`);
    }
    lines.push("");
  }
  return lines.join("\n");
}

export function generate(article) {
  const start = article.indexOf(BEGIN);
  const end = article.indexOf(END);
  if (start === -1 || end === -1) {
    throw new Error(
      `${ARTICLE} is missing the generated-inventory markers:\n  ${BEGIN}\n  ${END}`,
    );
  }
  const block = renderBlock(collect(), existingNotes(article));
  return article.slice(0, start + BEGIN.length) + block + article.slice(end);
}

function main() {
  const check = process.argv.includes("--check");
  const current = readFileSync(ARTICLE, "utf8");
  const next = generate(current);

  if (current === next) {
    console.log("OK — .codesight/wiki/ui.md's component inventory matches the source tree.");
    return 0;
  }
  if (check) {
    console.error(
      "STALE — .codesight/wiki/ui.md's component inventory no longer matches\n" +
        "frontend/src. Run `node scripts/ui-inventory.mjs` and commit the result.",
    );
    return 1;
  }
  writeFileSync(ARTICLE, next);
  console.log("Regenerated .codesight/wiki/ui.md's component inventory.");
  return 0;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  process.exit(main());
}
