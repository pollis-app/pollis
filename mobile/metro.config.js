// Expo's defaults plus ONE watch folder: the shared translation catalogues (frontend/src/i18n/locales/). Nothing else in frontend/ is watched.

const { getDefaultConfig } = require("expo/metro-config");
const { execFileSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const config = getDefaultConfig(__dirname);

config.watchFolders = [
  ...(config.watchFolders ?? []),
  path.resolve(__dirname, "../frontend/src/i18n/locales"),
];

// The Open source licenses screen's data (#1832): generated from the lockfiles
// by scripts/oss-inventory.mjs before Metro crawls, so the list matches what this
// build ships. Regenerated only when an input is newer. If it cannot be made and
// no copy exists, `lib/ossNotices.ts`'s require fails the bundle rather than
// shipping without the notices.
function ensureOssNotices() {
  const repo = path.resolve(__dirname, "..");
  const out = path.join(__dirname, "generated/oss-notices.json");
  const texts = path.join(repo, "supply-chain/license-texts");
  const inputs = [
    path.join(repo, "Cargo.lock"),
    path.join(repo, "scripts/oss-inventory.mjs"),
    path.join(__dirname, "pnpm-lock.yaml"),
    path.join(__dirname, "package.json"),
    ...fs.readdirSync(texts).map((f) => path.join(texts, f)),
  ];
  const newest = Math.max(...inputs.map((f) => fs.statSync(f).mtimeMs));
  if (fs.existsSync(out) && fs.statSync(out).mtimeMs >= newest) {
    return;
  }
  try {
    fs.mkdirSync(path.dirname(out), { recursive: true });
    execFileSync("node", [path.join(repo, "scripts/oss-inventory.mjs"), "mobile", "--notices", out], {
      cwd: repo,
      stdio: ["ignore", "inherit", "inherit"],
    });
  } catch (err) {
    console.warn(`open source notices not generated: ${err.message}`);
  }
}

ensureOssNotices();

module.exports = config;
