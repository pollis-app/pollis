// Version bookkeeping, derived — `mobile/package.json` `version` is the ONE
// place a human edits.
//
// Expo reads `app.json` first and hands it to this function as `config`, so
// everything else (plugins, icons, privacy manifests, permissions) still lives
// in `app.json` exactly as before. This file overrides only the three fields
// that used to be hand-bumped in lockstep and drifted the moment someone
// forgot one:
//
//   expo.version              user-facing, e.g. "1.0.0"
//   expo.ios.buildNumber      CFBundleVersion            (string)
//   expo.android.versionCode  Play versionCode           (integer)
//
// ## Why the build number is not just the version
//
// Both stores reject a REUSED build number, and they reject it after the
// upload, not before. A scheme where `buildNumber` is literally the version
// therefore makes the second upload of a version impossible — which is exactly
// the case you hit most: a TestFlight build, one fix, upload again, still
// 1.0.0 to users. So the version supplies the high digits and a separate
// counter supplies the low ones:
//
//   code = ((major * 100 + minor) * 100 + patch) * 100 + build
//
//   1.0.0            -> 1000000
//   1.0.0 (build 1)  -> 1000001      same user-facing version, uploadable
//   1.0.1            -> 1000100      still strictly greater
//   1.2.3 (build 4)  -> 1020304
//
// `build` comes from POLLIS_BUILD (default 0), so a re-upload of an unchanged
// version is `POLLIS_BUILD=1 pnpm expo prebuild -p ios` and nothing else moves.
//
// Android's `versionCode` must be a strictly increasing integer across every
// release ever, and this is monotonic in (major, minor, patch, build) as long
// as minor/patch/build each stay under 100. It is also well under Play's
// 2100000000 ceiling — major 20 is still only 20000000. If a component ever
// needs to exceed 99, widen the multipliers deliberately; do not let it wrap,
// because a versionCode that goes backwards is unrecoverable without a new
// package name.

const fs = require("fs");
const path = require("path");
const pkg = require("./package.json");

// ## Over-the-air JS updates (#1250)
//
// expo-updates is in every binary, but it is switched ON only for a build that
// says so: POLLIS_OTA=production, set by the store builds and the sideload APK
// release. Everything else (dev clients, the Maestro Release builds against
// api-dev, CI's debug-keystore APK) gets `enabled: false` and no URL at all, so
// it cannot ask the update server for anything — a dev build never takes a
// prod update, and there is no dev update server whose output could reach a
// prod build.
//
// A prod build REFUSES to configure without the code-signing certificate:
// expo-updates then rejects every manifest that is unsigned or signed by any
// other key. The private half never touches this repo or the update server;
// see scripts/generate-ota-signing-key.sh and mobile/CLAUDE.md "OTA updates".
const OTA_CHANNEL = "production";
// Served by the DS front-door Worker itself (pollis-delivery/worker/updates.ts),
// never forwarded to the DS container.
const OTA_UPDATE_URL = "https://api.pollis.com/updates/api/manifest";
const OTA_CERT_PATH = "./store/ota-code-signing.pem";
const OTA_PROD_DS = "https://api.pollis.com";
// Every install sends the same value: nothing in an update request identifies one.
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
  // The DS URL is only visible here when it is in the environment (the Expo CLI
  // loads mobile/.env; the native build phase that re-reads this file does not).
  // When it IS visible it must be prod: an api-dev bundle taking prod updates is
  // exactly the cross-over this switch exists to prevent.
  // The dev DS gate key (#1242) is inlined wherever the app reads it, so a prod
  // build must not be able to see one at all. Rust would never send it to prod,
  // but a key in a store bundle is a key handed to everyone.
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
    // Check on every launch, in the background; never hold the launch for it.
    // A downloaded update runs on the NEXT cold start. No UI.
    checkAutomatically: "ON_LOAD",
    fallbackToCacheTimeout: 0,
    // The update server serves whole assets only; asking for bsdiff patches
    // would add a request header the server has no use for.
    enableBsdiffPatchSupport: false,
    codeSigningCertificate: OTA_CERT_PATH,
    codeSigningMetadata: { keyid: "main", alg: "rsa-v1_5-sha256" },
    requestHeaders: {
      "expo-channel-name": OTA_CHANNEL,
      // expo-updates attaches an install-scoped random UUID (`EAS-Client-ID`,
      // stored on the device) to every manifest AND asset request — a stable
      // per-install identifier the update server has no use for, and which
      // Cloudflare's edge would see next to the client IP. Both platforms'
      // FileDownloader apply `requestHeaders` after setting it (iOS
      // `setValue`, Android OkHttp `header()` — both replace), so a constant
      // here overwrites it on every request.
      "EAS-Client-ID": OTA_NEUTRAL_CLIENT_ID,
      // The previous launch's fatal JS error text (up to 1 KiB) is otherwise
      // sent on the next manifest request; it is diagnostic data that could
      // carry anything the error message did. Also applied last, so blanked.
      "Expo-Fatal-Error": "",
    },
  };
}

// Guard the inputs rather than silently shipping a wrong number: a bad version
// string here becomes NaN in a store field, which fails late and confusingly.
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

  const certExists = fs.existsSync(path.join(__dirname, OTA_CERT_PATH));

  return {
    ...config,
    version: pkg.version,
    ios: { ...config.ios, buildNumber: String(code) },
    android: { ...config.android, versionCode: code },
    updates: updatesConfigFor(process.env, certExists),
  };
};

module.exports.updatesConfigFor = updatesConfigFor;
module.exports.OTA = { OTA_CHANNEL, OTA_UPDATE_URL, OTA_CERT_PATH, OTA_PROD_DS, OTA_NEUTRAL_CLIENT_ID };
