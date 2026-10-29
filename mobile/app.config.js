// Derives the store version fields from `package.json` `version` and POLLIS_BUILD; see mobile/CLAUDE.md "Store builds".

const fs = require("fs");
const path = require("path");
const pkg = require("./package.json");

// OTA updates (#1250): on only with POLLIS_OTA=production, and then only with the code-signing certificate.
const OTA_CHANNEL = "production";
const OTA_UPDATE_URL = "https://api.pollis.com/updates/api/manifest";
const OTA_CERT_PATH = "./store/ota-code-signing.pem";
const OTA_PROD_DS = "https://api.pollis.com";
// Every install sends the same value: nothing in a request identifies one.
const OTA_NEUTRAL_CLIENT_ID = "00000000-0000-0000-0000-000000000000";

function updatesConfigFor(env, certExists) {
  const mode = env.POLLIS_OTA;
  if (mode === undefined || mode === "" || mode === "off") {
    return { enabled: false };
  }
  if (mode !== OTA_CHANNEL) {
    throw new Error(
      `POLLIS_OTA must be "${OTA_CHANNEL}" or "off" (or unset), got ${JSON.stringify(mode)}`,
    );
  }
  if (!certExists) {
    throw new Error(
      `POLLIS_OTA=${OTA_CHANNEL} but the OTA code-signing certificate ${OTA_CERT_PATH} is missing. ` +
        "A prod build must not ship expo-updates without it. Run scripts/generate-ota-signing-key.sh " +
        "(owner only) and commit the certificate, or build with POLLIS_OTA unset.",
    );
  }
  // A prod build must not see the dev DS gate key (#1242): it is inlined wherever the app reads it.
  if (env.EXPO_PUBLIC_POLLIS_DEV_ACCESS_KEY) {
    throw new Error(
      `POLLIS_OTA=${OTA_CHANNEL} with EXPO_PUBLIC_POLLIS_DEV_ACCESS_KEY set: a prod build must not carry the dev DS key. ` +
        "Clear it (mobile/.env and the environment) and build again.",
    );
  }
  const ds = env.EXPO_PUBLIC_POLLIS_DELIVERY_URL;
  if (ds !== undefined && ds !== OTA_PROD_DS) {
    throw new Error(
      `POLLIS_OTA=${OTA_CHANNEL} needs EXPO_PUBLIC_POLLIS_DELIVERY_URL=${OTA_PROD_DS}, got ${JSON.stringify(ds)}`,
    );
  }
  return {
    enabled: true,
    url: OTA_UPDATE_URL,
    // Never hold the launch; a downloaded update runs on the NEXT cold start.
    checkAutomatically: "ON_LOAD",
    fallbackToCacheTimeout: 0,
    enableBsdiffPatchSupport: false,
    codeSigningCertificate: OTA_CERT_PATH,
    codeSigningMetadata: { keyid: "main", alg: "rsa-v1_5-sha256" },
    requestHeaders: {
      "expo-channel-name": OTA_CHANNEL,
      // Overwrites expo-updates' per-install UUID (requestHeaders apply last on both platforms).
      "EAS-Client-ID": OTA_NEUTRAL_CLIENT_ID,
      // Blank the previous launch's fatal JS error text, which could carry anything.
      "Expo-Fatal-Error": "",
    },
  };
}

// Without both R2 origins every image renders "Image unavailable"; refuse to configure.
const MEDIA_ENV = ["EXPO_PUBLIC_R2_ENDPOINT", "EXPO_PUBLIC_R2_PUBLIC_URL"];

function assertMediaEnv(env) {
  if (!env.EXPO_PUBLIC_POLLIS_DELIVERY_URL) {
    return;
  }
  const missing = MEDIA_ENV.filter((name) => !(env[name] ?? "").trim());
  if (missing.length > 0) {
    throw new Error(
      `EXPO_PUBLIC_POLLIS_DELIVERY_URL is set but ${missing.join(" and ")} ${missing.length > 1 ? "are" : "is"} not: ` +
        "this build could not load a single attachment. Set them for the same environment as the DS " +
        "(dev: doppler secrets get R2_S3_ENDPOINT / R2_PUBLIC_URL -p pollis -c dev_personal --plain; see mobile/.env.example).",
    );
  }
}

// Install channel (#1443), from the NATIVE build step's env; an OTA cannot change it (lib/buildInfo.ts).
const INSTALL_CHANNELS = ["store", "sideload", "dev"];

function installChannelFor(env) {
  const key = env.POLLIS_RELEASE_KEY;
  if (key !== undefined && key !== "" && key !== "upload" && key !== "sideload") {
    throw new Error(
      `POLLIS_RELEASE_KEY must be "upload" or "sideload" (or unset), got ${JSON.stringify(key)}`,
    );
  }
  if (key === "sideload") {
    return "sideload";
  }
  if (env.POLLIS_OTA === OTA_CHANNEL) {
    return "store";
  }
  return "dev";
}

function versionCodeFrom(version, build) {
  const m = /^(\d+)\.(\d+)\.(\d+)/.exec(version);
  if (!m) {
    throw new Error(
      `mobile/package.json "version" must be MAJOR.MINOR.PATCH, got ${JSON.stringify(version)}`,
    );
  }
  const [major, minor, patch] = m.slice(1, 4).map(Number);
  for (const [name, value] of [
    ["minor", minor],
    ["patch", patch],
    ["build", build],
  ]) {
    if (value > 99) {
      throw new Error(
        `${name}=${value} exceeds 99 and would overflow into the next field of the version code; widen the multipliers in app.config.js deliberately`,
      );
    }
  }
  return ((major * 100 + minor) * 100 + patch) * 100 + build;
}

module.exports = ({ config }) => {
  const build = Number(process.env.POLLIS_BUILD ?? 0);
  if (!Number.isInteger(build) || build < 0) {
    throw new Error(
      `POLLIS_BUILD must be a non-negative integer, got ${JSON.stringify(process.env.POLLIS_BUILD)}`,
    );
  }
  const code = versionCodeFrom(pkg.version, build);

  assertMediaEnv(process.env);
  const certExists = fs.existsSync(path.join(__dirname, OTA_CERT_PATH));

  return {
    ...config,
    version: pkg.version,
    ios: { ...config.ios, buildNumber: String(code) },
    android: { ...config.android, versionCode: code },
    updates: updatesConfigFor(process.env, certExists),
    extra: { ...config.extra, installChannel: installChannelFor(process.env) },
  };
};

module.exports.updatesConfigFor = updatesConfigFor;
module.exports.assertMediaEnv = assertMediaEnv;
module.exports.installChannelFor = installChannelFor;
module.exports.INSTALL_CHANNELS = INSTALL_CHANNELS;
module.exports.OTA = { OTA_CHANNEL, OTA_UPDATE_URL, OTA_CERT_PATH, OTA_PROD_DS, OTA_NEUTRAL_CLIENT_ID };
