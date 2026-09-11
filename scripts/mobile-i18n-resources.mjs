#!/usr/bin/env node
/**
 * Generate `mobile/i18n/resources.ts`.
 *
 * Metro has no `import.meta.glob`, so the catalogue table the desktop builds
 * at bundle time (`frontend/src/i18n/resources.ts`) has to be a static list of
 * requires on mobile — one per `<locale>/<namespace>.json`. Hand-maintaining
 * a hundred lines that must match a directory listing is how a locale ships
 * on desktop and silently renders English on mobile, so the file is generated
 * from the listing and `scripts/i18n-check.mjs` fails when it is stale.
 *
 *   node scripts/mobile-i18n-resources.mjs          # rewrite
 *   node scripts/mobile-i18n-resources.mjs --check  # exit 1 if stale
 */

import { readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export const LOCALES_DIR = join(ROOT, "frontend/src/i18n/locales");
export const OUTPUT_PATH = join(ROOT, "mobile/i18n/resources.ts");

/** `{ en: ["auth", "chat", …], … }`, both levels sorted. */
export function listCatalogues(localesDir = LOCALES_DIR) {
  const out = {};
  for (const code of readdirSync(localesDir).sort()) {
    if (!statSync(join(localesDir, code)).isDirectory()) {
      continue;
    }
    out[code] = readdirSync(join(localesDir, code))
      .filter((f) => f.endsWith(".json"))
      .map((f) => f.replace(/\.json$/, ""))
      .sort();
  }
  return out;
}

function identifier(code, ns) {
  return `${code}_${ns}`.replace(/[^A-Za-z0-9_]/g, "_");
}

export function render(catalogues) {
  const lines = [
    "// GENERATED FILE — DO NOT EDIT BY HAND.",
    "//",
    "// Produced by `scripts/mobile-i18n-resources.mjs` from the directory listing",
    "// of frontend/src/i18n/locales/ — the catalogues are SHARED with desktop (see",
    "// metro.config.js). Regenerate after adding a locale or a namespace:",
    "//",
    "//     node scripts/mobile-i18n-resources.mjs",
    "//",
    "",
  ];
  for (const [code, namespaces] of Object.entries(catalogues)) {
    for (const ns of namespaces) {
      lines.push(
        `import ${identifier(code, ns)} from "../../frontend/src/i18n/locales/${code}/${ns}.json";`,
      );
    }
  }
  lines.push("");
  lines.push("type Catalogue = Record<string, unknown>;");
  lines.push("");
  lines.push("export const RESOURCES: Record<string, Record<string, Catalogue>> = {");
  for (const [code, namespaces] of Object.entries(catalogues)) {
    lines.push(`  ${JSON.stringify(code)}: {`);
    for (const ns of namespaces) {
      lines.push(`    ${JSON.stringify(ns)}: ${identifier(code, ns)},`);
    }
    lines.push("  },");
  }
  lines.push("};");
  lines.push("");
  return lines.join("\n");
}

function main() {
  const rendered = render(listCatalogues());
  if (process.argv.includes("--check")) {
    let current = "";
    try {
      current = readFileSync(OUTPUT_PATH, "utf8");
    } catch {
      // Missing counts as stale.
    }
    if (current !== rendered) {
      console.error(
        `${OUTPUT_PATH} is stale — run \`node scripts/mobile-i18n-resources.mjs\` and commit the result.`,
      );
      process.exit(1);
    }
    console.log("mobile/i18n/resources.ts is up to date.");
    return;
  }
  writeFileSync(OUTPUT_PATH, rendered);
  console.log(`wrote ${OUTPUT_PATH}`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main();
}
