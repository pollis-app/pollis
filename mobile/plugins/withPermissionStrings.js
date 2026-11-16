// Expo config plugin (#1724): the iOS permission prompts in every catalogue language.
//
// iOS shows an NS*UsageDescription in the system permission sheet, in the language iOS picks for
// the app (the device's, or the per-app choice in iOS Settings) — never the in-app picker's, which
// iOS cannot see. The text comes from `mobile:permissions.*` in the shared catalogues and is
// written as `<lproj>/InfoPlist.strings` at prebuild, with English filling any gap. The base
// Info.plist keeps the English set in app.json (the plugin options and `ios.infoPlist`), which
// tests/permission-strings.test.ts holds equal to the `en` catalogue.
//
// Android needs nothing: its runtime permission dialog is written and localized by the OS, and
// the app's own rationale copy (camera access, saving to photos) is already in the catalogues.
const path = require("path");
const { IOSConfig, withXcodeProject } = require("expo/config-plugins");
const { LOCALES_DIR, LPROJ, mobileSection, escapeStrings, addKnownRegions } = require("./iosLocales");

// Info.plist key → `mobile:permissions.*` key.
const KEYS = {
  NSCameraUsageDescription: "camera",
  NSPhotoLibraryUsageDescription: "photoLibrary",
  NSPhotoLibraryAddUsageDescription: "photoLibraryAdd",
  NSFaceIDUsageDescription: "faceId",
};

/** `<lproj>` → { Info.plist key → text }, unescaped. */
function permissionStrings(localesDir = LOCALES_DIR) {
  const en = mobileSection("en", "permissions", localesDir);
  const out = {};
  for (const [lng, lproj] of Object.entries(LPROJ)) {
    const table = { ...en, ...mobileSection(lng, "permissions", localesDir) };
    out[lproj] = {};
    for (const [plistKey, key] of Object.entries(KEYS)) {
      if (typeof table[key] !== "string" || !table[key].trim()) {
        throw new Error(`mobile:permissions.${key} is missing from the ${lng} catalogue`);
      }
      out[lproj][plistKey] = table[key];
    }
  }
  return out;
}

const withPermissionStrings = (config) =>
  withXcodeProject(config, async (c) => {
    const { projectRoot, platformProjectRoot } = c.modRequest;
    const projectName = IOSConfig.XcodeUtils.getProjectName(projectRoot);
    const localesMap = {};
    for (const [lproj, strings] of Object.entries(permissionStrings())) {
      // writeStringsFile quotes the value but does not escape it.
      localesMap[lproj] = Object.fromEntries(Object.entries(strings).map(([k, v]) => [k, escapeStrings(v)]));
    }
    c.modResults = await IOSConfig.Locales.writeStringsFile({
      localesMap,
      supportingDirectory: path.join(platformProjectRoot, projectName, "Supporting"),
      fileName: "InfoPlist.strings",
      projectName,
      project: c.modResults,
    });
    addKnownRegions(c.modResults);
    return c;
  });

module.exports = withPermissionStrings;
module.exports.permissionStrings = permissionStrings;
module.exports.KEYS = KEYS;
