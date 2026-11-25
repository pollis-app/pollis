// Expo config plugin (#2100): the iOS share extension, so other apps' share sheets can send text,
// links, photos, videos and files into Pollis chats.
//
// Adds the `PollisShare` app-extension target (Swift + SwiftUI in `plugins/share-extension/`,
// statically linking the Rust core from the app's xcframework, as the Notification Service
// Extension does) and embeds it in the app. The extension unlocks with the app's PIN or biometric
// key, shows its own chat picker and sends in place through the core's headless share session
// (`pollis-core/include/pollis_share.h`); nothing is handed to the app.
//
// Privacy rules this plugin holds (pinned by tests/share-extension.test.ts):
// - The share sheet shows Pollis, the app, and never a contact, group or chat. Those suggestions
//   need an extension that declares `IntentsSupported: INSendMessageIntent`; this one never does,
//   so the communication-notification intents the NSE donates (#1599) stay out of the share sheet.
// - The activation rule is the dictionary form with fixed counts (text, one web URL, up to 10
//   images, movies and files), never TRUEPREDICATE: Pollis is offered only for what it can send.
//
// Entitlements: the App Group (the core's data dir) and two keychain groups, the shared
// `<team>.com.pollis.shared` first (the keystore key) and the app-only `<team>.com.pollis.app`
// last, which holds the biometric device unlock key (#1685). Unlike the NSE, which runs with nobody
// present and must never get the app-only group, this extension runs in front of the user, and that
// key only opens behind Face ID / Touch ID (`.biometryCurrentSet`), exactly as in the app.
//
// `POLLIS_IOS_SHARE=off` builds the app without any of it (no target, no entitlement changes), for
// a store build cut before the developer portal has the `com.pollis.ios.Share` App ID. See
// mobile/CLAUDE.md "Share extension".
const fs = require("fs");
const path = require("path");
const plist = require("@expo/plist").default;
const { withDangerousMod, withXcodeProject } = require("expo/config-plugins");
const { LOCALES_DIR, LPROJ, mobileSection, escapeStrings } = require("./iosLocales");
const { addExtensionTarget } = require("./iosExtensionTarget");
const nse = require("./withNotificationServiceExtension");
const { appOnlyGroup } = require("./withDeviceUnlock");

const TARGET = "PollisShare";
const SOURCE_DIR = path.join(__dirname, "share-extension");
// Compiled into the extension from the NSE's sources, not forked.
const SHARED_SOURCES = { "LetterAvatar.swift": path.join(__dirname, "notification-service", "LetterAvatar.swift") };
const OWN_SOURCES = [
  "ShareCore.swift",
  "ShareItems.swift",
  "ShareModel.swift",
  "ShareStrings.swift",
  "BiometricKey.swift",
  "ShareIntake.swift",
  "ShareView.swift",
  "ShareViewController.swift",
];
const SWIFT_SOURCES = [...OWN_SOURCES, ...Object.keys(SHARED_SOURCES)];
const BRIDGING_HEADER = `${TARGET}-Bridging-Header.h`;
const INFO_PLIST = `${TARGET}-Info.plist`;
const ENTITLEMENTS = `${TARGET}.entitlements`;
const MAX_ITEMS = 10;
// i18next interpolation → a positional string argument, in order of appearance.
const PLACEHOLDER = /\{\{\s*(\w+)\s*\}\}/g;

// Off with the NSE too: without it the app has no App Group, so its data is not where the
// extension looks.
function isEnabled(env = process.env) {
  return env.POLLIS_IOS_SHARE !== "off" && nse.isEnabled(env);
}

function identifiers(config, options) {
  const base = nse.identifiers(config, options);
  return {
    app: base.app,
    extension: `${base.app}.Share`,
    group: base.group,
    keychain: base.keychain,
    appOnlyKeychain: appOnlyGroup(options),
  };
}

/** The extension's entitlements: the App Group; the shared keychain group first, the app-only last. */
function extensionEntitlements({ group, keychain, appOnlyKeychain }) {
  return {
    "com.apple.security.application-groups": [group],
    "keychain-access-groups": [keychain, appOnlyKeychain],
  };
}

/** The activation rule: what Pollis can send, with counts. The dictionary form only. */
function activationRule() {
  return {
    NSExtensionActivationSupportsText: true,
    NSExtensionActivationSupportsWebURLWithMaxCount: 1,
    NSExtensionActivationSupportsImageWithMaxCount: MAX_ITEMS,
    NSExtensionActivationSupportsMovieWithMaxCount: MAX_ITEMS,
    NSExtensionActivationSupportsFileWithMaxCount: MAX_ITEMS,
  };
}

function extensionInfoPlist(config, ids, localesDir = LOCALES_DIR) {
  const permissions = mobileSection("en", "permissions", localesDir);
  return {
    CFBundleDevelopmentRegion: "en",
    // What the share sheet shows under the app icon.
    CFBundleDisplayName: "Pollis",
    CFBundleExecutable: "$(EXECUTABLE_NAME)",
    CFBundleIdentifier: "$(PRODUCT_BUNDLE_IDENTIFIER)",
    CFBundleInfoDictionaryVersion: "6.0",
    CFBundleName: "$(PRODUCT_NAME)",
    CFBundlePackageType: "$(PRODUCT_BUNDLE_PACKAGE_TYPE)",
    // App Store Connect rejects an extension whose versions differ from its app's.
    CFBundleShortVersionString: config.version,
    CFBundleVersion: String(config.ios?.buildNumber ?? "1"),
    // The biometric key is read behind Face ID from this process.
    NSFaceIDUsageDescription: permissions.faceId,
    PollisAppGroup: ids.group,
    // Read by pollis-core's iOS keystore (#1599).
    PollisKeychainAccessGroup: ids.keychain,
    // Where the app keeps the biometric device unlock key (#1685).
    PollisAppOnlyKeychainGroup: ids.appOnlyKeychain,
    NSExtension: {
      NSExtensionPointIdentifier: "com.apple.share-services",
      NSExtensionPrincipalClass: "$(PRODUCT_MODULE_NAME).ShareViewController",
      NSExtensionAttributes: {
        NSExtensionActivationRule: activationRule(),
      },
    },
  };
}

function toFormat(value) {
  let n = 0;
  return value.replace(/%/g, "%%").replace(PLACEHOLDER, () => {
    n += 1;
    return `%${n}$@`;
  });
}

/** `<lproj>` → Localizable.strings text, from `mobile:share.*`; English fills any gap. */
function localizableStrings(localesDir = LOCALES_DIR) {
  const read = (lng) => mobileSection(lng, "share", localesDir);
  const en = read("en");
  const keys = Object.keys(en).sort();
  if (keys.length === 0) {
    throw new Error("mobile:share is missing from the en catalogue");
  }
  const out = {};
  for (const [lng, lproj] of Object.entries(LPROJ)) {
    const table = { ...en, ...read(lng) };
    const lines = keys.map((key) => `"${key}" = "${escapeStrings(toFormat(table[key]))}";`);
    out[lproj] = `/* Generated by plugins/withShareExtension.js from frontend/src/i18n/locales/${lng}/mobile.json */\n${lines.join("\n")}\n`;
  }
  return out;
}

function writeExtensionFiles(config, ids, iosDir) {
  const dir = path.join(iosDir, TARGET);
  fs.mkdirSync(dir, { recursive: true });
  for (const file of OWN_SOURCES) {
    fs.copyFileSync(path.join(SOURCE_DIR, file), path.join(dir, file));
  }
  for (const [file, from] of Object.entries(SHARED_SOURCES)) {
    fs.copyFileSync(from, path.join(dir, file));
  }
  fs.copyFileSync(path.join(SOURCE_DIR, BRIDGING_HEADER), path.join(dir, BRIDGING_HEADER));
  fs.writeFileSync(path.join(dir, INFO_PLIST), plist.build(extensionInfoPlist(config, ids)));
  fs.writeFileSync(path.join(dir, ENTITLEMENTS), plist.build(extensionEntitlements(ids)));
  for (const [lproj, text] of Object.entries(localizableStrings())) {
    fs.mkdirSync(path.join(dir, `${lproj}.lproj`), { recursive: true });
    fs.writeFileSync(path.join(dir, `${lproj}.lproj`, "Localizable.strings"), text);
  }
}

const withShareExtension = (config, options) => {
  if (!isEnabled()) {
    return config;
  }
  const ids = identifiers(config, options);
  // The app's own entitlements are untouched: it already has the App Group (the NSE plugin) and
  // the app-only group (withDeviceUnlock).
  config = withXcodeProject(config, (c) => {
    c.modResults = addExtensionTarget(c, c.modResults, {
      target: TARGET,
      bundleId: ids.extension,
      swiftSources: SWIFT_SOURCES,
      bridgingHeader: BRIDGING_HEADER,
      infoPlist: INFO_PLIST,
      entitlements: ENTITLEMENTS,
      frameworks: ["AVFoundation", "LocalAuthentication"],
    });
    return c;
  });
  config = withDangerousMod(config, [
    "ios",
    (c) => {
      writeExtensionFiles(c, ids, c.modRequest.platformProjectRoot);
      return c;
    },
  ]);
  return config;
};

module.exports = withShareExtension;
module.exports.identifiers = identifiers;
module.exports.extensionEntitlements = extensionEntitlements;
module.exports.extensionInfoPlist = extensionInfoPlist;
module.exports.activationRule = activationRule;
module.exports.localizableStrings = localizableStrings;
module.exports.isEnabled = isEnabled;
module.exports.TARGET = TARGET;
module.exports.SWIFT_SOURCES = SWIFT_SOURCES;
