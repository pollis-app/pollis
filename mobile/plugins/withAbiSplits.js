// Expo config plugin: per-ABI APK splits, off unless POLLIS_ABI_SPLITS=true. No x86: pollis-core is not built for it.
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
module.exports.addAbiSplits = addAbiSplits;
