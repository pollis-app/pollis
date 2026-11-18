// Regenerates src/generated/ (the TypeScript declarations of the
// `pollis:app@0.1.0` WIT world) from wit/app.wit with jco, and refreshes
// wit/ from the repo's canonical pollis-app-wit/. test/generated.test.ts
// fails if either is stale.
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(dirname(fileURLToPath(import.meta.url)));
const canonical = join(here, "..", "..", "..", "pollis-app-wit");

export function generate(outDir) {
  rmSync(outDir, { recursive: true, force: true });
  mkdirSync(outDir, { recursive: true });
  execFileSync(
    join(here, "node_modules", ".bin", "jco"),
    ["guest-types", "wit/app.wit", "--world-name", "app", "-o", outDir, "-q"],
    { cwd: here, stdio: ["ignore", "ignore", "inherit"] },
  );
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  copyFileSync(join(canonical, "wit", "app.wit"), join(here, "wit", "app.wit"));
  copyFileSync(join(canonical, "pollis-app.schema.json"), join(here, "wit", "pollis-app.schema.json"));
  generate(join(here, "src", "generated"));
  console.log("regenerated src/generated from wit/app.wit");
}
