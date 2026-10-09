// What the OTA runtime version is a hash OF (#1250).
//
// `runtimeVersion.policy: "fingerprint"` (app.json) makes the runtime version a
// hash of the app's native layer, computed by @expo/fingerprint — locally, with
// no EAS involved: expo-updates' own build phase (Xcode script / Gradle plugin)
// writes it into the binary, and mobile/scripts/ota-publish.mjs computes the
// same hash with the same function before it builds a manifest. An update is
// only ever served to a binary whose fingerprint equals the update's runtime
// version, so a JS update can never land on native code it was not built for.
//
// Out of the box that fingerprint does NOT cover the part of this app most
// likely to change: pollis-core, the Rust core behind the uniffi bridge. Its
// generated C++/TS lives in gitignored directories of modules/pollis-native,
// and the Rust sources are outside the Expo project entirely. So this file:
//
//   1. hashes every git-tracked file of pollis-core and of each workspace crate
//      it reaches through `path =` dependencies (computed, not listed, so a new
//      crate dependency is covered the day it is added), plus the workspace
//      Cargo files and the pinned toolchain — any change there means a new
//      store build before any update can target it;
//   2. ignores the ubrn-GENERATED files in modules/pollis-native (they exist on
//      a machine that ran `ubrn build` and not on one that did not, and are a
//      pure function of (1) plus the ubrn version, which is hashed too) so the
//      store build and the publish job agree;
//   3. skips the version fields (package.json `version` / POLLIS_BUILD — they
//      move without the native code moving) and the `updates` block (whether a
//      build has OTA switched on is not native-compatibility information, and
//      the native build phase re-reads the config without the Expo CLI's env);
//   4. hashes the OTA code-signing CERTIFICATE (store/ota-code-signing.pem).
//      A binary only accepts updates signed for the certificate compiled into
//      it, so a rotated certificate must mean a new runtime version: old
//      binaries keep their own pointers (signed by the old key) instead of
//      sharing one with binaries that trust a different key.
//
// tests/ota-fingerprint.test.ts pins all three.

const { execFileSync } = require("child_process");
const crypto = require("crypto");
const fs = require("fs");
const path = require("path");

const REPO_ROOT = path.resolve(__dirname, "..");

// Files outside any crate that change what the Rust core compiles to.
const WORKSPACE_FILES = ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo/config.toml"];

// Paths inside a crate that never reach the compiled library.
function isNonBuildPath(rel) {
  const parts = rel.split("/");
  if (parts.some((p) => p === "tests" || p === "benches" || p === "fuzz" || p === "examples")) {
    return true;
  }
  return rel.endsWith(".md");
}

// pollis-core plus the transitive closure of its `path = "../x"` dependencies.
function rustCrateClosure() {
  const seen = new Set();
  const queue = ["pollis-core"];
  while (queue.length > 0) {
    const crate = queue.shift();
    if (seen.has(crate)) {
      continue;
    }
    seen.add(crate);
    const manifest = fs.readFileSync(path.join(REPO_ROOT, crate, "Cargo.toml"), "utf8");
    for (const m of manifest.matchAll(/path\s*=\s*"\.\.\/([A-Za-z0-9_.-]+)"/g)) {
      queue.push(m[1]);
    }
  }
  return [...seen].sort();
}

function gitTrackedFiles(dirs) {
  const out = execFileSync("git", ["ls-files", "-z", "--", ...dirs], {
    cwd: REPO_ROOT,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
  return out.split("\0").filter(Boolean).sort();
}

function rustCoreDigest() {
  const crates = rustCrateClosure();
  const files = [
    ...gitTrackedFiles(crates).filter((rel) => !isNonBuildPath(rel.split("/").slice(1).join("/"))),
    ...WORKSPACE_FILES.filter((rel) => fs.existsSync(path.join(REPO_ROOT, rel))),
  ];
  const lines = files.map((rel) => {
    const bytes = fs.readFileSync(path.join(REPO_ROOT, rel));
    return `${crypto.createHash("sha256").update(bytes).digest("hex")}  ${rel}`;
  });
  // The generated bindings are a function of the Rust sources AND of the ubrn
  // version that generated them; the npm half is pinned in package.json.
  const pkg = JSON.parse(fs.readFileSync(path.join(__dirname, "package.json"), "utf8"));
  lines.push(`uniffi-bindgen-react-native ${pkg.dependencies["uniffi-bindgen-react-native"]}`);
  lines.push(`@ubjs/core ${pkg.dependencies["@ubjs/core"]}`);
  return { crates, contents: lines.join("\n") + "\n" };
}

// ubrn output: present only where `ubrn build` ran (mobile/.gitignore lists it).
const GENERATED_NATIVE_BRIDGE = [
  "modules/pollis-native/cpp/**/*",
  "modules/pollis-native/src/generated/**/*",
  "modules/pollis-native/src/index.tsx",
  "modules/pollis-native/src/NativePollisNative.ts",
  "modules/pollis-native/android/CMakeLists.txt",
  "modules/pollis-native/android/android/**/*",
  "modules/pollis-native/android/build/**/*",
  "modules/pollis-native/android/.cxx/**/*",
  "modules/pollis-native/PollisNativeFramework.xcframework/**/*",
];

// Drop `updates` from the hashed Expo config (see 3. above). Everything else in
// the config — plugins, permissions, entitlements — stays in.
function stripUpdatesBlock(source, chunk) {
  if (source.type !== "contents" || source.id !== "expoConfig" || chunk == null) {
    return chunk;
  }
  const config = JSON.parse(chunk.toString());
  delete config.updates;
  return JSON.stringify(config);
}

// The certificate's bytes, or "absent" before the owner has generated it.
function otaCertificateDigest() {
  const cert = path.join(__dirname, "store", "ota-code-signing.pem");
  if (!fs.existsSync(cert)) {
    return "absent\n";
  }
  return `${crypto.createHash("sha256").update(fs.readFileSync(cert)).digest("hex")}\n`;
}

/** @type {import('expo/fingerprint').Config} */
module.exports = {
  sourceSkips: ["ExpoConfigVersions", "PackageJsonAndroidAndIosScriptsIfNotContainRun"],
  ignorePaths: GENERATED_NATIVE_BRIDGE,
  extraSources: [
    {
      type: "contents",
      id: "pollis-core",
      contents: rustCoreDigest().contents,
      reasons: ["pollis-core (uniffi bridge)"],
    },
    {
      type: "contents",
      id: "ota-code-signing-certificate",
      contents: otaCertificateDigest(),
      reasons: ["OTA code-signing certificate"],
    },
  ],
  fileHookTransform: stripUpdatesBlock,
};

module.exports.rustCoreDigest = rustCoreDigest;
module.exports.rustCrateClosure = rustCrateClosure;
module.exports.GENERATED_NATIVE_BRIDGE = GENERATED_NATIVE_BRIDGE;
module.exports.otaCertificateDigest = otaCertificateDigest;
