#!/usr/bin/env node
/**
 * The component inventory in `.codesight/wiki/ui.md`, generated (#933).
 *
 * The article had always claimed to be regenerated — "treat the filesystem as
 * the authority and regenerate rather than patch when it disagrees" — while no
 * generator existed anywhere in the repo. So the instruction could not be
 * followed, the count drifted by a third (111 claimed, 147 real), and each
 * person who noticed hand-patched the number instead, which is the loop that
 * produces a doc nobody trusts.
 *
 * This is that generator. It owns exactly one region of the article, fenced by
 * the markers below; every other section is prose and is never touched.
 *
 *   node scripts/ui-inventory.mjs           # rewrite the inventory in place
 *   node scripts/ui-inventory.mjs --check   # exit 1 if it is out of date
 *
 * ## Hand-written notes survive
 *
 * Some entries carry a sentence that no extractor could produce ("Memoised via
 * `observer()`", "Not a loading state"). Those are the most valuable lines in
 * the section, so the generator preserves them: everything a line carries
 * AFTER its backticked path is a note, and it is copied forward verbatim onto
 * the regenerated line for the same component. Delete the component and the
 * note goes with it; rename it and the note is dropped, which is the correct
 * prompt to rewrite it.
 *
 * ## What it extracts, and what it deliberately does not
 *
 * Component name and props come from a light regex scan, not a TypeScript AST.
 * The inventory is a NAVIGATION AID — its job is to get a reader to the right
 * file — so "the name is close enough to grep and the path is exact" is the
 * bar, and a full parser would be a dependency and a maintenance burden for no
 * extra navigational value. Props are listed when a `<Name>Props` type is
 * declared in the file and omitted otherwise; the article says as much.
 */

import { readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join, relative, basename, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = fileURLToPath(new URL("..", import.meta.url));
const SRC = join(ROOT, "frontend/src");
const ARTICLE = join(ROOT, ".codesight/wiki/ui.md");

const BEGIN = "<!-- BEGIN GENERATED: component inventory (scripts/ui-inventory.mjs) -->";
const END = "<!-- END GENERATED: component inventory -->";

// ── Extraction ─────────────────────────────────────────────────────────────

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

/**
 * The name this file is about.
 *
 * An export whose name matches the filename wins, which is the repo's
 * convention and is what stops the old inventory's failure mode — it listed
 * `ALL_ALGORITHMS` for `DotMatrix.tsx` and a sub-component's props for
 * `VoiceSettingsPage.tsx`, because it took whatever was exported first.
 * Falling back to the filename keeps the entry useful even then.
 */
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

/**
 * Top-level field names of the component's props type.
 *
 * Only a type NAMED for the component (`<Name>Props`) counts. A file's first
 * `*Props` type is very often a private sub-component's, which is how the old
 * inventory ended up attributing `DeviceSelect`'s props to the whole voice
 * settings page.
 */
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

// ── Rendering ──────────────────────────────────────────────────────────────

/**
 * Hand-written notes from the existing block, keyed by component name.
 *
 * A note is whatever follows the backticked path on the line. Anything before
 * the path is regenerated and therefore not a place to write prose.
 */
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

// ── CLI ────────────────────────────────────────────────────────────────────

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
