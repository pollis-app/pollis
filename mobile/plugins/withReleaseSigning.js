// Expo config plugin: a release signingConfig in android/app/build.gradle. Each key reads only its own POLLIS_<KEY>_* (gradle properties beat env).
const { withAppBuildGradle } = require('expo/config-plugins');

const MARKER = 'POLLIS_RELEASE_KEY';

const RELEASE_SIGNING_CONFIG = `        release {
            // Injected by plugins/withReleaseSigning.js. POLLIS_RELEASE_KEY picks
            // the key (upload | sideload); each reads only its own POLLIS_<KEY>_*
            // gradle/env properties. Only the upload key falls back to debug.
            def pollisKey = (project.findProperty('POLLIS_RELEASE_KEY') ?: System.getenv('POLLIS_RELEASE_KEY') ?: 'upload').toString().toUpperCase()
            if (!(pollisKey in ['UPLOAD', 'SIDELOAD'])) {
                throw new GradleException("POLLIS_RELEASE_KEY must be upload or sideload, got " + pollisKey)
            }
            def pollisProp = { String name -> project.findProperty("POLLIS_" + pollisKey + "_" + name) ?: System.getenv("POLLIS_" + pollisKey + "_" + name) }
            def pollisStoreFile = pollisProp('STORE_FILE')
            if (pollisStoreFile) {
                storeFile file(pollisStoreFile)
                storePassword pollisProp('STORE_PASSWORD')
                keyAlias pollisProp('KEY_ALIAS')
                keyPassword pollisProp('KEY_PASSWORD')
            } else if (pollisKey == 'UPLOAD') {
                storeFile file('debug.keystore')
                storePassword 'android'
                keyAlias 'androiddebugkey'
                keyPassword 'android'
            } else {
                throw new GradleException("POLLIS_RELEASE_KEY=" + pollisKey.toLowerCase() + " but POLLIS_" + pollisKey + "_STORE_FILE is unset; refusing to fall back to the debug key")
            }
        }
`;

// The block an older version injected; swapped for the current one, never duplicated.
const LEGACY_RELEASE_SIGNING_CONFIG = `        release {
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
  if (gradle.includes(MARKER)) {
    return gradle;
  }
  if (gradle.includes(LEGACY_RELEASE_SIGNING_CONFIG)) {
    return gradle.replace(LEGACY_RELEASE_SIGNING_CONFIG, RELEASE_SIGNING_CONFIG);
  }
  if (gradle.includes('POLLIS_UPLOAD_STORE_FILE')) {
    throw new Error(
      'withReleaseSigning: android/app/build.gradle carries a hand-edited release signing block; run `expo prebuild -p android --clean`'
    );
  }

  // 1. Add a `release` entry right after `signingConfigs {`.
  const signingConfigsRe = /(signingConfigs\s*\{\n)/;
  if (!signingConfigsRe.test(gradle)) {
    throw new Error(
      'withReleaseSigning: could not find `signingConfigs {` in android/app/build.gradle'
    );
  }
  let next = gradle.replace(signingConfigsRe, `$1${RELEASE_SIGNING_CONFIG}`);

  // 2. Point buildTypes.release at signingConfigs.release.
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
module.exports.addReleaseSigning = addReleaseSigning;
