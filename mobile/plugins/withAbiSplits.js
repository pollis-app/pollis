// Expo config plugin: optional per-ABI APK splits in android/app/build.gradle.
//
// Off unless POLLIS_ABI_SPLITS=true (gradle property or env), so every existing
// build — the debug loop, CI's mobile-core-check APK, the Play .aab (bundles
// ignore `splits` anyway) — is unchanged. The sideload release
// (mobile-apk-release.yml) turns it on: pollis-core's native library is
// 24-34 MB per ABI and Android ships native libs uncompressed, so one universal
// APK carrying all three ABIs is ~210 MB against ~90 MB for the arm64-v8a
// split nearly every phone needs.
//
// The ABI list is the one modules/pollis-native/ubrn.config.yaml builds the Rust
// core for. 32-bit x86 is deliberately absent: pollis-core is not built for it,
// so an x86 APK would install and then crash loading the core.
const { withAppBuildGradle } = require('expo/config-plugins');

const MARKER = 'POLLIS_ABI_SPLITS';

const ABI_SPLITS = `    // Injected by plugins/withAbiSplits.js. Per-ABI APKs only when
    // POLLIS_ABI_SPLITS=true; the ABIs are pollis-core's (ubrn.config.yaml).
    splits {
        abi {
            enable((project.findProperty('POLLIS_ABI_SPLITS') ?: System.getenv('POLLIS_ABI_SPLITS') ?: 'false').toString().toBoolean())
            reset()
            include 'arm64-v8a', 'armeabi-v7a', 'x86_64'
            universalApk false
        }
    }
`;

function addAbiSplits(gradle) {
  if (gradle.includes(MARKER)) {
    return gradle;
  }
  const androidRe = /^(android\s*\{\n)/m;
  if (!androidRe.test(gradle)) {
    throw new Error('withAbiSplits: could not find `android {` in android/app/build.gradle');
  }
  return gradle.replace(androidRe, `$1${ABI_SPLITS}`);
}

const withAbiSplits = (config) => {
  return withAppBuildGradle(config, (config) => {
    if (config.modResults.language !== 'groovy') {
      throw new Error('withAbiSplits: android/app/build.gradle is not groovy — plugin needs updating');
    }
    config.modResults.contents = addAbiSplits(config.modResults.contents);
    return config;
  });
};

module.exports = withAbiSplits;
// Exported for unit-style verification without running prebuild.
module.exports.addAbiSplits = addAbiSplits;
