/* plugins/withReleaseSigning.js: each key reads only its own POLLIS_<KEY>_*, only upload falls back to debug, idempotent, migrates the legacy block. */

import test from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { addReleaseSigning } = require("../plugins/withReleaseSigning.js");

const TEMPLATE = `android {
    signingConfigs {
        debug {
            storeFile file('debug.keystore')
        }
    }
    buildTypes {
        debug {
            signingConfig signingConfigs.debug
        }
        release {
            signingConfig signingConfigs.debug
        }
    }
}
`;

function count(haystack: string, needle: string): number {
  return haystack.split(needle).length - 1;
}

test("injects one release signingConfig and points the release build type at it", () => {
  const out = addReleaseSigning(TEMPLATE);
  assert.equal(count(out, "        release {\n            // Injected by plugins/withReleaseSigning.js"), 1);
  assert.equal(count(out, "signingConfig signingConfigs.release"), 1);
  assert.equal(count(out, "signingConfig signingConfigs.debug"), 1);
});

test("each key reads only its own prefix, and only the upload key falls back to debug", () => {
  const out = addReleaseSigning(TEMPLATE);
  assert.match(out, /pollisKey in \['UPLOAD', 'SIDELOAD'\]/);
  assert.match(out, /"POLLIS_" \+ pollisKey \+ "_" \+ name/);
  assert.match(out, /else if \(pollisKey == 'UPLOAD'\) \{\n\s+storeFile file\('debug\.keystore'\)/);
  assert.match(out, /refusing to fall back to the debug key/);
  assert.equal(out.includes("POLLIS_UPLOAD_STORE_FILE"), false);
});

test("re-running is a no-op", () => {
  const once = addReleaseSigning(TEMPLATE);
  assert.equal(addReleaseSigning(once), once);
});

test("a build.gradle from the previous plugin version is migrated in place", () => {
  const legacyBlock = `        release {
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
  const legacy = TEMPLATE.replace("    signingConfigs {\n", `    signingConfigs {\n${legacyBlock}`).replace(
    "        release {\n            signingConfig signingConfigs.debug",
    "        release {\n            signingConfig signingConfigs.release",
  );
  const out = addReleaseSigning(legacy);
  assert.equal(out, addReleaseSigning(TEMPLATE));
});
