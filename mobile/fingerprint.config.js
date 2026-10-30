// Widens the OTA runtime-version fingerprint (#1250) to pollis-core; see mobile/CLAUDE.md "OTA updates". It also
// normalises node_modules files that GRADLE REWRITES IN PLACE (#1427).
//      @react-native-masked-view/masked-view's build.gradle strips
//      `package="org.reactnative.maskedview"` from its own
//      android/src/main/AndroidManifest.xml whenever Gradle configures it. A
//      store build's machine has the rewritten file; a fresh install (the
//      Linux OTA job) has the pristine one, and the package is hashed as a
//      whole dir, so one commit got two runtime versions and OTAs never
//      reached Play binaries. Hashing both as the rewritten form makes them
//      agree and keeps every binary already built (with the rewrite) valid.

const { execFileSync } = require("child_process");
const crypto = require("crypto");
const fs = require("fs");
const path = require("path");

const REPO_ROOT = path.resolve(__dirname, "..");

// Files outside any crate that change what the Rust core compiles to.
const WORKSPACE_FILES = ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo/config.toml"];

function isNonBuildPath(rel) {
  const parts = rel.split("/");
  if (parts.some((p) => p === "tests" || p === "benches" || p === "fuzz" || p === "examples")) {
    return true;
  }
  return rel.endsWith(".md");
}

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
  // Generated bindings also depend on the ubrn version.
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

// Build output tools write INTO installed packages (ubrn's cargo target/, Kotlin's .kotlin/ session state, #1429).
const NODE_MODULES_BUILD_OUTPUT = [
  "node_modules/uniffi-bindgen-react-native/target/**/*",
  "node_modules/uniffi-bindgen-react-native/Cargo.lock",
  "node_modules/**/.kotlin/**/*",
];

// Drop `updates` from the hashed Expo config.
function stripUpdatesBlock(source, chunk) {
  if (source.type !== "contents" || source.id !== "expoConfig" || chunk == null) {
    return chunk;
  }
  const config = JSON.parse(chunk.toString());
  delete config.updates;
  // The install channel (#1443) comes from the native build's env, which the OTA job never has.
  if (config.extra && typeof config.extra === "object") {
    delete config.extra.installChannel;
  }
  return JSON.stringify(config);
}

// Files a dependency's build.gradle rewrites inside node_modules (#1427), with the exact edit.
const GRADLE_REWRITTEN_FILES = {
  // masked-view android/build.gradle, AGP >= 7: manifestContent.replaceAll(PACKAGE_PROP, '')
  "node_modules/@react-native-masked-view/masked-view/android/src/main/AndroidManifest.xml": (text) =>
    text.split('package="org.reactnative.maskedview"').join(""),
};

// Idempotent, so a rewritten file hashes the same as a pristine one.
function normaliseGradleRewrite(filePath, text) {
  const rewrite = GRADLE_REWRITTEN_FILES[filePath];
  if (rewrite == null) {
    return text;
  }
  return rewrite(text);
}

// File contents arrive in 1 KiB chunks, concurrently across files: buffer a
// rewritten file whole so the edit never straddles a chunk boundary.
const pendingRewrites = new Map();

function fileHookTransform(source, chunk, isEndOfFile, encoding) {
  if (source.type !== "file" || !Object.hasOwn(GRADLE_REWRITTEN_FILES, source.filePath)) {
    return stripUpdatesBlock(source, chunk, isEndOfFile, encoding);
  }
  const parts = pendingRewrites.get(source.filePath) ?? [];
  if (chunk != null) {
    parts.push(Buffer.from(chunk));
  }
  if (!isEndOfFile) {
    pendingRewrites.set(source.filePath, parts);
    return null;
  }
  pendingRewrites.delete(source.filePath);
  const text = Buffer.concat(parts).toString("utf8");
  return Buffer.from(normaliseGradleRewrite(source.filePath, text), "utf8");
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
  ignorePaths: [...GENERATED_NATIVE_BRIDGE, ...NODE_MODULES_BUILD_OUTPUT],
  extraSources: [
    {
      type: "contents",
      id: "pollis-core",
      contents: rustCoreDigest().contents,
      reasons: ["pollis-core (uniffi bridge)"],
    },
    // The NSE's Swift (#1599): fingerprint hashes the plugin's .js, not the files it copies in.
    {
      type: "dir",
      filePath: "plugins/notification-service",
      reasons: ["iOS Notification Service Extension sources"],
    },
    {
      type: "contents",
      id: "ota-code-signing-certificate",
      contents: otaCertificateDigest(),
      reasons: ["OTA code-signing certificate"],
    },
  ],
  fileHookTransform,
};

module.exports.rustCoreDigest = rustCoreDigest;
module.exports.rustCrateClosure = rustCrateClosure;
module.exports.GENERATED_NATIVE_BRIDGE = GENERATED_NATIVE_BRIDGE;
module.exports.otaCertificateDigest = otaCertificateDigest;
module.exports.GRADLE_REWRITTEN_FILES = GRADLE_REWRITTEN_FILES;
module.exports.NODE_MODULES_BUILD_OUTPUT = NODE_MODULES_BUILD_OUTPUT;
module.exports.normaliseGradleRewrite = normaliseGradleRewrite;
