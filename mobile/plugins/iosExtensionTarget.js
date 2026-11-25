// What the iOS app-extension config plugins share (#1599 notification service, #2100 share): the
// pbxproj surgery that adds a Swift extension target statically linking the Rust core from the
// app's xcframework, embeds it in the app, and localizes it. Each plugin passes its own target name,
// bundle id, sources and build settings; nothing here holds copy or entitlements.
const { LPROJ, addKnownRegions } = require("./iosLocales");

// Relative to ios/ (SRCROOT).
const XCFRAMEWORK = "../modules/pollis-native/PollisNativeFramework.xcframework";
const CORE_HEADERS = "$(SRCROOT)/../../pollis-core/include";
const EMBED_PHASE = '"Embed Foundation Extensions"';

function addUnique(list, value) {
  const out = Array.isArray(list) ? [...list] : [];
  if (!out.includes(value)) {
    out.push(value);
  }
  return out;
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

/**
 * Name the embed phase the way Xcode does (addTarget calls it "Copy Files"), and keep ONE embed
 * phase on the app: a second extension's phase is folded into the first, so the app target never
 * carries two phases of the same name.
 */
function foldEmbedPhases(project) {
  const objects = project.hash.project.objects;
  const phases = objects.PBXCopyFilesBuildPhase ?? {};
  const app = project.getFirstTarget().firstTarget;
  const embeds = app.buildPhases
    .map((p) => p.value)
    .filter((uuid) => typeof phases[uuid] === "object" && phases[uuid].dstSubfolderSpec === 13);
  for (const uuid of embeds) {
    if (phases[uuid].name === '"Copy Files"') {
      phases[uuid].name = EMBED_PHASE;
      phases[`${uuid}_comment`] = "Embed Foundation Extensions";
    }
  }
  if (embeds.length < 2) {
    return;
  }
  const [keep, ...rest] = embeds;
  const buildFiles = objects.PBXBuildFile ?? {};
  const relabel = (comment) =>
    typeof comment === "string" ? comment.replace(/ in Copy Files$/, " in Embed Foundation Extensions") : comment;
  for (const uuid of rest) {
    for (const file of phases[uuid].files) {
      file.comment = relabel(file.comment);
      if (`${file.value}_comment` in buildFiles) {
        buildFiles[`${file.value}_comment`] = relabel(buildFiles[`${file.value}_comment`]);
      }
    }
    phases[keep].files.push(...phases[uuid].files);
    app.buildPhases = app.buildPhases.filter((p) => p.value !== uuid);
    delete phases[uuid];
    delete phases[`${uuid}_comment`];
  }
}

/**
 * Add an app-extension target that statically links the core. `spec`:
 * `{ target, bundleId, swiftSources, bridgingHeader, infoPlist, entitlements, frameworks }`, where
 * `frameworks` are the system frameworks the Swift needs beyond the core's own.
 */
function addExtensionTarget(config, project, spec) {
  const { target: TARGET, bundleId, swiftSources, bridgingHeader, infoPlist, entitlements } = spec;
  if (project.pbxTargetByName(TARGET) || project.pbxTargetByName(`"${TARGET}"`)) {
    return project;
  }
  const objects = project.hash.project.objects;
  // xcode@4 addTarget assumes these sections exist.
  objects.PBXTargetDependency = objects.PBXTargetDependency ?? {};
  objects.PBXContainerItemProxy = objects.PBXContainerItemProxy ?? {};

  const appSettings = mainTargetSettings(project);
  const target = project.addTarget(TARGET, "app_extension", TARGET, bundleId);

  const sources = project.addBuildPhase([], "PBXSourcesBuildPhase", "Sources", target.uuid);
  const resources = project.addBuildPhase([], "PBXResourcesBuildPhase", "Resources", target.uuid);
  const frameworks = project.addBuildPhase([], "PBXFrameworksBuildPhase", "Frameworks", target.uuid);

  const swiftRefs = swiftSources.map((file) => ({
    file,
    ref: fileRef(project, { name: file, path: file, type: "sourcecode.swift" }),
  }));
  const headerRef = fileRef(project, {
    name: bridgingHeader,
    path: bridgingHeader,
    type: "sourcecode.c.h",
  });
  const plistRef = fileRef(project, { name: infoPlist, path: infoPlist, type: "text.plist.xml" });
  const entRef = fileRef(project, {
    name: entitlements,
    path: entitlements,
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
        { value: headerRef, comment: bridgingHeader },
        { value: plistRef, comment: infoPlist },
        { value: entRef, comment: entitlements },
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

  foldEmbedPhases(project);
  addKnownRegions(project);

  const deploymentTarget = config.ios?.deploymentTarget ?? "16.4";
  const configs = project.pbxXCBuildConfigurationSection();
  const list = project.pbxXCConfigurationList()[target.pbxNativeTarget.buildConfigurationList];
  const extraFrameworks = (spec.frameworks ?? []).flatMap((f) => ['"-framework"', f]);
  for (const entry of list.buildConfigurations) {
    const settings = configs[entry.value].buildSettings;
    const app = appSettings[entry.comment] ?? {};
    const debug = entry.comment === "Debug";
    Object.assign(settings, {
      APPLICATION_EXTENSION_API_ONLY: "YES",
      CLANG_ENABLE_MODULES: "YES",
      CODE_SIGN_ENTITLEMENTS: `"${TARGET}/${entitlements}"`,
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
        ...extraFrameworks,
        '"-lc++"',
        '"-lresolv"',
      ],
      SDKROOT: "iphoneos",
      SWIFT_OBJC_BRIDGING_HEADER: `"${TARGET}/${bridgingHeader}"`,
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

module.exports = { addExtensionTarget, addUnique, foldEmbedPhases, XCFRAMEWORK, CORE_HEADERS };
