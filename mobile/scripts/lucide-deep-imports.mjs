#!/usr/bin/env node
// Rewrites a barrel import from "lucide-react-native" in components/icons.tsx into one deep
// import per icon ("lucide-react-native/icons/<file>"), #2187. Metro does not tree-shake, so the
// barrel puts all ~1,800 icon modules in the bundle and runs each one's factory at first import.
// The file for each name comes from the barrel itself: an alias such as `MoreVertical` lives in
// `ellipsis-vertical`, not `more-vertical`. Run after adding an icon to the barrel import there
// by hand, or write the deep import directly (tests/lucide-deep-imports.test.ts checks either way).
//
//     node scripts/lucide-deep-imports.mjs

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const MOBILE = join(dirname(fileURLToPath(import.meta.url)), "..");
const BARREL = join(MOBILE, "node_modules/lucide-react-native/dist/esm/lucide-react-native.mjs");
const ICONS = join(MOBILE, "components/icons.tsx");

/** Every name the barrel exports, mapped to its icon file (without extension). */
export function barrelFiles(barrelSource) {
  const fileOf = new Map();
  for (const m of barrelSource.matchAll(/export \{([^}]*)\} from '\.\/icons\/([^']+)\.mjs';/g)) {
    for (const part of m[1].split(",")) {
      fileOf.set(part.trim().replace(/^default as /, ""), m[2]);
    }
  }
  return fileOf;
}

function main() {
  const fileOf = barrelFiles(readFileSync(BARREL, "utf8"));
  const src = readFileSync(ICONS, "utf8");
  const re = /import \{([^}]*)\} from "lucide-react-native";/;
  const m = src.match(re);
  if (!m) {
    console.log("no barrel import in components/icons.tsx");
    return;
  }
  const specs = m[1].split(",").map((s) => s.trim()).filter(Boolean);
  const types = specs.filter((s) => s.startsWith("type "));
  const lines = specs
    .filter((s) => !s.startsWith("type "))
    .map((spec) => {
      const [imported, local] = spec.split(/\s+as\s+/);
      const file = fileOf.get(imported);
      if (!file) {
        throw new Error(`lucide-react-native exports no icon named ${imported}`);
      }
      return `import ${local ?? imported} from "lucide-react-native/icons/${file}";`;
    });
  if (types.length) {
    lines.push(`import type { ${types.map((t) => t.replace(/^type /, "")).join(", ")} } from "lucide-react-native";`);
  }
  writeFileSync(ICONS, src.replace(re, lines.join("\n")));
  console.log(`rewrote ${lines.length} lucide imports`);
}

if (process.argv[1] && process.argv[1] === fileURLToPath(import.meta.url)) {
  main();
}
