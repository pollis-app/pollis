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

const pkg = require("./package.json");

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

  return {
    ...config,
    version: pkg.version,
    ios: { ...config.ios, buildNumber: String(code) },
    android: { ...config.android, versionCode: code },
  };
};
