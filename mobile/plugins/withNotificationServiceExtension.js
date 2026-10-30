// Expo config plugin (#1599): the iOS Notification Service Extension that decrypts a push on the device.
//
// Adds the `PollisNotificationService` app-extension target (Swift, statically linking the Rust core
// from the same xcframework the app uses), embeds it in the app, and gives both targets the App
// Group `group.<bundle id>` and the keychain access group `<team>.com.pollis.shared` so the
// extension reads the app's data dir and keystore key. The team is spelled out (plugin option
// `teamId`) rather than `$(AppIdentifierPrefix)`: the store archive is built unsigned, where that
// variable is empty, and the ad-hoc codesign step reads the entitlements files unexpanded. The app also gets the Communication
// Notifications capability (the sender-style banner). Strings come from the shared catalogues
// (`mobile:push.*`, shared with the Android handler), written out as Localizable.strings at prebuild.
//
// `POLLIS_IOS_NSE=off` builds the app without any of it (no extension, no new entitlements, and the
// app does not advertise the `rich` push capability) — for a store build cut before the developer
// portal has the App Group and the extension's App ID. See mobile/CLAUDE.md "Notification Service
// Extension".
const fs = require("fs");
const path = require("path");
const plist = require("@expo/plist").default;
const {
  withDangerousMod,
  withEntitlementsPlist,
  withInfoPlist,
  withXcodeProject,
} = require("expo/config-plugins");

const TARGET = "PollisNotificationService";
const SOURCE_DIR = path.join(__dirname, "notification-service");
const SWIFT_SOURCES = ["NotificationService.swift", "LetterAvatar.swift"];
const BRIDGING_HEADER = `${TARGET}-Bridging-Header.h`;
const INFO_PLIST = `${TARGET}-Info.plist`;
const ENTITLEMENTS = `${TARGET}.entitlements`;
const KEYCHAIN_SUFFIX = "com.pollis.shared";
// Relative to ios/ (SRCROOT).
const XCFRAMEWORK = "../modules/pollis-native/PollisNativeFramework.xcframework";
const CORE_HEADERS = "$(SRCROOT)/../../pollis-core/include";
const LOCALES_DIR = path.join(__dirname, "..", "..", "frontend", "src", "i18n", "locales");
// Catalogue language → .lproj name. `zh` is Simplified Chinese (mobile/i18n/languages.ts).
const LPROJ = { en: "en", es: "es", fr: "fr", ru: "ru", uk: "uk", zh: "zh-Hans", ar: "ar" };
const STRING_KEYS = [
  "genericTitle",
  "genericBody",
  "nameOnlyBody",
  "groupTitle",
  "bodyAttachment",
  "bodyImage",
  "bodyVideo",
  "bodyVoice",
  "bodySensitive",
];
// i18next interpolation → positional format arguments, in the order NotificationService.swift passes them.
const PLACEHOLDERS = { sender: "%1$@", conversation: "%2$@" };

function isEnabled(env = process.env) {
  return env.POLLIS_IOS_NSE !== "off";
}

function identifiers(config, options) {
  const app = config.ios?.bundleIdentifier;
  if (!app) {
    throw new Error("withNotificationServiceExtension needs expo.ios.bundleIdentifier");
  }
  const teamId = options?.teamId;
  if (!/^[A-Z0-9]{10}$/.test(teamId ?? "")) {
    throw new Error(
      `withNotificationServiceExtension needs a 10-character teamId option, got ${JSON.stringify(teamId)}`,
    );
  }
  return {
    app,
    extension: `${app}.NotificationService`,
    group: `group.${app}`,
    keychain: `${teamId}.${KEYCHAIN_SUFFIX}`,
  };
}

function addUnique(list, value) {
  const out = Array.isArray(list) ? [...list] : [];
  if (!out.includes(value)) {
    out.push(value);
  }
  return out;
}

function sharedEntitlements(entitlements, { group, keychain }) {
  return {
    ...entitlements,
    "com.apple.security.application-groups": addUnique(
      entitlements["com.apple.security.application-groups"],
      group,
    ),
    // First entry = the default group for new keychain items; old items stay readable by the app.
    "keychain-access-groups": [
      keychain,
      ...(entitlements["keychain-access-groups"] ?? []).filter((g) => g !== keychain),
    ],
  };
}

function escapeStrings(value) {
  return value.replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\n/g, "\\n");
}

function toFormat(value) {
  return value
    .replace(/%/g, "%%")
    .replace(/\{\{\s*(\w+)\s*\}\}/g, (match, name) => {
      if (!PLACEHOLDERS[name]) {
        throw new Error(`mobile:push string uses unknown placeholder {{${name}}}`);
      }
      return PLACEHOLDERS[name];
    });
}

/** `<lproj>` → Localizable.strings text, from `mobile:push.*`; English fills any gap. */
function localizableStrings(localesDir = LOCALES_DIR) {
  const read = (lng) =>
    JSON.parse(fs.readFileSync(path.join(localesDir, lng, "mobile.json"), "utf8"))?.push ?? {};
  const en = read("en");
  const out = {};
  for (const [lng, lproj] of Object.entries(LPROJ)) {
    const table = { ...en, ...read(lng) };
    const lines = STRING_KEYS.map((key) => {
      if (typeof table[key] !== "string") {
        throw new Error(`mobile:push.${key} is missing from the ${lng} catalogue`);
      }
      return `"${key}" = "${escapeStrings(toFormat(table[key]))}";`;
    });
    out[lproj] = `/* Generated by plugins/withNotificationServiceExtension.js from frontend/src/i18n/locales/${lng}/mobile.json */\n${lines.join("\n")}\n`;
  }
  return out;
}

function extensionInfoPlist(config, { group, keychain }) {
  return {
    CFBundleDevelopmentRegion: "en",
    CFBundleDisplayName: TARGET,
    CFBundleExecutable: "$(EXECUTABLE_NAME)",
    CFBundleIdentifier: "$(PRODUCT_BUNDLE_IDENTIFIER)",
    CFBundleInfoDictionaryVersion: "6.0",
    CFBundleName: "$(PRODUCT_NAME)",
    CFBundlePackageType: "$(PRODUCT_BUNDLE_PACKAGE_TYPE)",
    // App Store Connect rejects an extension whose versions differ from its app's.
    CFBundleShortVersionString: config.version,
    CFBundleVersion: String(config.ios?.buildNumber ?? "1"),
    PollisAppGroup: group,
    // Read by pollis-core's iOS keystore (#1599).
    PollisKeychainAccessGroup: keychain,
    NSExtension: {
      NSExtensionPointIdentifier: "com.apple.usernotifications.service",
      NSExtensionPrincipalClass: "$(PRODUCT_MODULE_NAME).NotificationService",
    },
  };
}

function writeExtensionFiles(config, ids, iosDir) {
  const dir = path.join(iosDir, TARGET);
  fs.mkdirSync(dir, { recursive: true });
  for (const file of SWIFT_SOURCES) {
    fs.copyFileSync(path.join(SOURCE_DIR, file), path.join(dir, file));
  }
  fs.copyFileSync(path.join(SOURCE_DIR, BRIDGING_HEADER), path.join(dir, BRIDGING_HEADER));
  fs.writeFileSync(path.join(dir, INFO_PLIST), plist.build(extensionInfoPlist(config, ids)));
  fs.writeFileSync(path.join(dir, ENTITLEMENTS), plist.build(sharedEntitlements({}, ids)));
  for (const [lproj, text] of Object.entries(localizableStrings())) {
    fs.mkdirSync(path.join(dir, `${lproj}.lproj`), { recursive: true });
    fs.writeFileSync(path.join(dir, `${lproj}.lproj`, "Localizable.strings"), text);
  }
}

function put(project, isa, uuid, obj, comment) {
  const objects = project.hash.project.objects;
  objects[isa] = objects[isa] ?? {};
  objects[isa][uuid] = obj;
  objects[isa][`${uuid}_comment`] = comment;
}

function fileRef(project, { name, path: filePath, type, sourceTree = '"<group>"' }) {
  const uuid = project.generateUuid();
  put(
    project,
    "PBXFileReference",
    uuid,
    {
      isa: "PBXFileReference",
      lastKnownFileType: type,
      name: `"${name}"`,
      path: `"${filePath}"`,
      sourceTree,
    },
    name,
  );
  return uuid;
}

function buildFile(project, ref, name, phaseUuid, phaseIsa, phaseName) {
  const uuid = project.generateUuid();
  put(
    project,
    "PBXBuildFile",
    uuid,
    { isa: "PBXBuildFile", fileRef: ref, fileRef_comment: name },
    `${name} in ${phaseName}`,
  );
  project.hash.project.objects[phaseIsa][phaseUuid].files.push({
    value: uuid,
    comment: `${name} in ${phaseName}`,
  });
}

function mainTargetSettings(project) {
  const target = project.getFirstTarget().firstTarget;
  const list = project.pbxXCConfigurationList()[target.buildConfigurationList];
  const configs = project.pbxXCBuildConfigurationSection();
  const byName = {};
  for (const entry of list.buildConfigurations) {
    byName[entry.comment] = configs[entry.value].buildSettings;
  }
  return byName;
}

function addExtensionTarget(config, ids, project) {
  if (project.pbxTargetByName(TARGET) || project.pbxTargetByName(`"${TARGET}"`)) {
    return project;
  }
  const { extension } = ids;
  const objects = project.hash.project.objects;
  // xcode@4 addTarget assumes these sections exist.
  objects.PBXTargetDependency = objects.PBXTargetDependency ?? {};
  objects.PBXContainerItemProxy = objects.PBXContainerItemProxy ?? {};

  const appSettings = mainTargetSettings(project);
  const target = project.addTarget(TARGET, "app_extension", TARGET, extension);

  const sources = project.addBuildPhase([], "PBXSourcesBuildPhase", "Sources", target.uuid);
  const resources = project.addBuildPhase([], "PBXResourcesBuildPhase", "Resources", target.uuid);
  const frameworks = project.addBuildPhase([], "PBXFrameworksBuildPhase", "Frameworks", target.uuid);

  const swiftRefs = SWIFT_SOURCES.map((file) => ({
    file,
    ref: fileRef(project, { name: file, path: file, type: "sourcecode.swift" }),
  }));
  const headerRef = fileRef(project, {
    name: BRIDGING_HEADER,
    path: BRIDGING_HEADER,
    type: "sourcecode.c.h",
  });
  const plistRef = fileRef(project, { name: INFO_PLIST, path: INFO_PLIST, type: "text.plist.xml" });
  const entRef = fileRef(project, {
    name: ENTITLEMENTS,
    path: ENTITLEMENTS,
    type: "text.plist.entitlements",
  });
  const xcfRef = fileRef(project, {
    name: "PollisNativeFramework.xcframework",
    path: XCFRAMEWORK,
    type: "wrapper.xcframework",
    sourceTree: "SOURCE_ROOT",
  });

  const variantUuid = project.generateUuid();
  const localeRefs = Object.values(LPROJ).map((lproj) => ({
    value: fileRef(project, {
      name: lproj,
      path: `${lproj}.lproj/Localizable.strings`,
      type: "text.plist.strings",
    }),
    comment: lproj,
  }));
  put(
    project,
    "PBXVariantGroup",
    variantUuid,
    {
      isa: "PBXVariantGroup",
      children: localeRefs,
      name: "Localizable.strings",
      sourceTree: '"<group>"',
    },
    "Localizable.strings",
  );

  for (const { file, ref } of swiftRefs) {
    buildFile(project, ref, file, sources.uuid, "PBXSourcesBuildPhase", "Sources");
  }
  buildFile(project, variantUuid, "Localizable.strings", resources.uuid, "PBXResourcesBuildPhase", "Resources");
  buildFile(
    project,
    xcfRef,
    "PollisNativeFramework.xcframework",
    frameworks.uuid,
    "PBXFrameworksBuildPhase",
    "Frameworks",
  );

  const groupUuid = project.generateUuid();
  put(
    project,
    "PBXGroup",
    groupUuid,
    {
      isa: "PBXGroup",
      children: [
        ...swiftRefs.map(({ file, ref }) => ({ value: ref, comment: file })),
        { value: headerRef, comment: BRIDGING_HEADER },
        { value: plistRef, comment: INFO_PLIST },
        { value: entRef, comment: ENTITLEMENTS },
        { value: variantUuid, comment: "Localizable.strings" },
        { value: xcfRef, comment: "PollisNativeFramework.xcframework" },
      ],
      path: TARGET,
      sourceTree: '"<group>"',
    },
    TARGET,
  );
  const mainGroup = project.getFirstProject().firstProject.mainGroup;
  project.hash.project.objects.PBXGroup[mainGroup].children.push({ value: groupUuid, comment: TARGET });

  // Name the embed phase the way Xcode does (addTarget calls it "Copy Files").
  for (const [key, phase] of Object.entries(objects.PBXCopyFilesBuildPhase ?? {})) {
    if (typeof phase === "object" && phase.dstSubfolderSpec === 13 && phase.name === '"Copy Files"') {
      phase.name = '"Embed Foundation Extensions"';
      objects.PBXCopyFilesBuildPhase[`${key}_comment`] = "Embed Foundation Extensions";
    }
  }

  const known = project.getFirstProject().firstProject.knownRegions ?? [];
  for (const lproj of Object.values(LPROJ)) {
    const quoted = lproj.includes("-") ? `"${lproj}"` : lproj;
    if (!known.includes(lproj) && !known.includes(quoted)) {
      known.push(quoted);
    }
  }
  project.getFirstProject().firstProject.knownRegions = known;

  const deploymentTarget = config.ios?.deploymentTarget ?? "16.4";
  const configs = project.pbxXCBuildConfigurationSection();
  const list = project.pbxXCConfigurationList()[target.pbxNativeTarget.buildConfigurationList];
  for (const entry of list.buildConfigurations) {
    const settings = configs[entry.value].buildSettings;
    const app = appSettings[entry.comment] ?? {};
    const debug = entry.comment === "Debug";
    Object.assign(settings, {
      APPLICATION_EXTENSION_API_ONLY: "YES",
      CLANG_ENABLE_MODULES: "YES",
      CODE_SIGN_ENTITLEMENTS: `"${TARGET}/${ENTITLEMENTS}"`,
      CODE_SIGN_STYLE: app.CODE_SIGN_STYLE ?? "Automatic",
      CURRENT_PROJECT_VERSION: String(config.ios?.buildNumber ?? "1"),
      DEAD_CODE_STRIPPING: "YES",
      ENABLE_BITCODE: "NO",
      GENERATE_INFOPLIST_FILE: "NO",
      HEADER_SEARCH_PATHS: `"${CORE_HEADERS}"`,
      IPHONEOS_DEPLOYMENT_TARGET: deploymentTarget,
      MARKETING_VERSION: config.version,
      // The core's system dependencies (Security / SystemConfiguration for the keychain and reqwest).
      OTHER_LDFLAGS: [
        '"$(inherited)"',
        '"-framework"',
        "Security",
        '"-framework"',
        "SystemConfiguration",
        '"-framework"',
        "CoreFoundation",
        '"-lc++"',
        '"-lresolv"',
      ],
      SDKROOT: "iphoneos",
      SWIFT_OBJC_BRIDGING_HEADER: `"${TARGET}/${BRIDGING_HEADER}"`,
      SWIFT_OPTIMIZATION_LEVEL: debug ? '"-Onone"' : '"-O"',
      SWIFT_VERSION: "5.0",
      TARGETED_DEVICE_FAMILY: app.TARGETED_DEVICE_FAMILY ?? '"1,2"',
    });
    if (app.DEVELOPMENT_TEAM) {
      settings.DEVELOPMENT_TEAM = app.DEVELOPMENT_TEAM;
    }
  }
  return project;
}

const withNotificationServiceExtension = (config, options) => {
  if (!isEnabled()) {
    return config;
  }
  const ids = identifiers(config, options);
  // Read by lib/push/caps.ts from the copy embedded at the native build, which an OTA cannot change.
  config.extra = {
    ...config.extra,
    richPush: { ...config.extra?.richPush, ios: true },
  };

  config = withEntitlementsPlist(config, (c) => {
    c.modResults = {
      ...sharedEntitlements(c.modResults, ids),
      "com.apple.developer.usernotifications.communication": true,
    };
    return c;
  });
  config = withInfoPlist(config, (c) => {
    c.modResults.PollisAppGroup = ids.group;
    c.modResults.PollisKeychainAccessGroup = ids.keychain;
    c.modResults.NSUserActivityTypes = addUnique(c.modResults.NSUserActivityTypes, "INSendMessageIntent");
    return c;
  });
  config = withXcodeProject(config, (c) => {
    c.modResults = addExtensionTarget(c, ids, c.modResults);
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

module.exports = withNotificationServiceExtension;
module.exports.localizableStrings = localizableStrings;
module.exports.sharedEntitlements = sharedEntitlements;
module.exports.extensionInfoPlist = extensionInfoPlist;
module.exports.isEnabled = isEnabled;
module.exports.TARGET = TARGET;
module.exports.identifiers = identifiers;
