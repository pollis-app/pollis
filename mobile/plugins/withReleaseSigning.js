// Expo config plugin: wire a release signingConfig into android/app/build.gradle
// at prebuild time, without EAS. Reads the upload keystore from gradle/env
// properties (POLLIS_UPLOAD_STORE_FILE / _STORE_PASSWORD / _KEY_ALIAS /
// _KEY_PASSWORD — put them in ~/.gradle/gradle.properties, see
// scripts/generate-upload-keystore.sh) and falls back to the Expo template's
// debug keystore when unset, so CI release builds keep working unchanged.
const { withAppBuildGradle } = require('expo/config-plugins');

const RELEASE_SIGNING_CONFIG = `        release {
            // Injected by plugins/withReleaseSigning.js (upload keystore from
            // gradle/env properties; debug keystore fallback when unset).
            def pollisStoreFile = project.findProperty('POLLIS_UPLOAD_STORE_FILE') ?: System.getenv('POLLIS_UPLOAD_STORE_FILE')
            if (pollisStoreFile) {
                storeFile file(pollisStoreFile)
                storePassword project.findProperty('POLLIS_UPLOAD_STORE_PASSWORD') ?: System.getenv('POLLIS_UPLOAD_STORE_PASSWORD')
                keyAlias project.findProperty('POLLIS_UPLOAD_KEY_ALIAS') ?: System.getenv('POLLIS_UPLOAD_KEY_ALIAS')
                keyPassword project.findProperty('POLLIS_UPLOAD_KEY_PASSWORD') ?: System.getenv('POLLIS_UPLOAD_KEY_PASSWORD')
            } else {
                storeFile file('debug.keystore')
                storePassword 'android'
                keyAlias 'androiddebugkey'
                keyPassword 'android'
            }
        }
`;

function addReleaseSigning(gradle) {
  // Idempotency: if the block is already present, leave the file alone.
  if (gradle.includes('POLLIS_UPLOAD_STORE_FILE')) {
    return gradle;
  }

  // 1. Add a `release` entry inside `signingConfigs { ... }` (inserting it
  //    directly after the opening brace keeps this independent of whatever
  //    else the template puts in there).
  const signingConfigsRe = /(signingConfigs\s*\{\n)/;
  if (!signingConfigsRe.test(gradle)) {
    throw new Error(
      'withReleaseSigning: could not find `signingConfigs {` in android/app/build.gradle'
    );
  }
  let next = gradle.replace(signingConfigsRe, `$1${RELEASE_SIGNING_CONFIG}`);

  // 2. Point buildTypes.release at signingConfigs.release. The Expo template
  //    ships `signingConfig signingConfigs.debug` inside the release build
  //    type; rewrite only the occurrence inside `buildTypes { ... release {`.
  const buildTypesIdx = next.indexOf('buildTypes');
  if (buildTypesIdx === -1) {
    throw new Error(
      'withReleaseSigning: could not find `buildTypes` in android/app/build.gradle'
    );
  }
  const releaseIdx = next.indexOf('release {', buildTypesIdx);
  if (releaseIdx === -1) {
    throw new Error(
      'withReleaseSigning: could not find the release build type in android/app/build.gradle'
    );
  }
  const debugRefIdx = next.indexOf('signingConfig signingConfigs.debug', releaseIdx);
  if (debugRefIdx === -1) {
    // Already pointing somewhere else (e.g. a re-run on a hand-edited file);
    // the signingConfigs.release block above is still in place.
    return next;
  }
  next =
    next.slice(0, debugRefIdx) +
    'signingConfig signingConfigs.release' +
    next.slice(debugRefIdx + 'signingConfig signingConfigs.debug'.length);
  return next;
}

const withReleaseSigning = (config) => {
  return withAppBuildGradle(config, (config) => {
    if (config.modResults.language !== 'groovy') {
      throw new Error(
        'withReleaseSigning: android/app/build.gradle is not groovy — plugin needs updating'
      );
    }
    config.modResults.contents = addReleaseSigning(config.modResults.contents);
    return config;
  });
};

module.exports = withReleaseSigning;
// Exported for unit-style verification without running prebuild.
module.exports.addReleaseSigning = addReleaseSigning;
