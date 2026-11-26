/* #2100: the iOS share extension plugin. What it must never do is as important as what it does:
 * the share sheet shows Pollis, the app, and never a contact, group or chat. */

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { supportedLanguageCodes } from "../i18n/languages.ts";

const SHIPPED_LPROJ = supportedLanguageCodes().map((c) => (c === "zh" ? "zh-Hans" : c)).sort();

const require = createRequire(import.meta.url);
const plugin = require("../plugins/withShareExtension.js");
const nse = require("../plugins/withNotificationServiceExtension.js");
const { foldEmbedPhases } = require("../plugins/iosExtensionTarget.js");
const appJson = require("../app.json");

const config = { ios: { bundleIdentifier: "com.pollis.ios", buildNumber: "1002300" }, version: "1.0.23" };
const ids = plugin.identifiers(config, { teamId: "8JT3WH45NZ" });
const info = plugin.extensionInfoPlist(config, ids);
const read = (p: string) => fs.readFileSync(path.join(import.meta.dirname, p), "utf8");

test("the identifiers are what the developer portal must hold", () => {
  assert.deepEqual(ids, {
    app: "com.pollis.ios",
    extension: "com.pollis.ios.Share",
    group: "group.com.pollis.ios",
    keychain: "8JT3WH45NZ.com.pollis.shared",
    appOnlyKeychain: "8JT3WH45NZ.com.pollis.app",
  });
  assert.throws(() => plugin.identifiers(config, {}), /teamId/);
});

test("the share sheet never lists people or chats: no IntentsSupported, no INSendMessageIntent", () => {
  const ext = info.NSExtension;
  assert.equal(ext.NSExtensionPointIdentifier, "com.apple.share-services");
  assert.equal(ext.NSExtensionPrincipalClass, "$(PRODUCT_MODULE_NAME).ShareViewController");
  const text = JSON.stringify(info);
  assert.ok(!text.includes("IntentsSupported"), "no IntentsSupported anywhere in the plist");
  assert.ok(!text.includes("INSendMessageIntent"), "no INSendMessageIntent anywhere in the plist");
  assert.ok(!("NSUserActivityTypes" in info), "the extension declares no activity types");
  // Nor anywhere in the Swift or the plugin source.
  for (const file of [...plugin.SWIFT_SOURCES.filter((f: string) => f !== "LetterAvatar.swift").map((f: string) => `../plugins/share-extension/${f}`), "../plugins/withShareExtension.js"]) {
    const src = read(file).replace(/^\s*\/\/.*$/gm, "");
    assert.ok(!/IntentsSupported|INSendMessageIntent|import Intents/.test(src), `${file} declares no share-sheet intents`);
  }
});

test("the activation rule is the dictionary form, with fixed counts and nothing else", () => {
  const rule = info.NSExtension.NSExtensionAttributes.NSExtensionActivationRule;
  assert.equal(typeof rule, "object");
  assert.deepEqual(rule, {
    NSExtensionActivationSupportsText: true,
    NSExtensionActivationSupportsWebURLWithMaxCount: 1,
    NSExtensionActivationSupportsImageWithMaxCount: 10,
    NSExtensionActivationSupportsMovieWithMaxCount: 10,
    NSExtensionActivationSupportsFileWithMaxCount: 10,
  });
  assert.ok(!JSON.stringify(info).includes("TRUEPREDICATE"));
  assert.ok(!read("../plugins/withShareExtension.js").replace(/^\s*\/\/.*$/gm, "").includes("TRUEPREDICATE"));
});

test("entitlements: the share extension reads the app-only group, the NSE never does", () => {
  assert.deepEqual(plugin.extensionEntitlements(ids), {
    "com.apple.security.application-groups": ["group.com.pollis.ios"],
    "keychain-access-groups": ["8JT3WH45NZ.com.pollis.shared", "8JT3WH45NZ.com.pollis.app"],
  });
  const nseIds = nse.identifiers(config, { teamId: "8JT3WH45NZ" });
  const nseEnt = nse.sharedEntitlements({}, nseIds);
  assert.ok(!JSON.stringify(nseEnt).includes("com.pollis.app"), "the NSE stays out of the app-only group");
  assert.ok(!JSON.stringify(plugin.extensionEntitlements(ids)).includes("$("), "no unexpanded build variable");
  assert.equal(info.PollisAppGroup, "group.com.pollis.ios");
  assert.equal(info.PollisKeychainAccessGroup, "8JT3WH45NZ.com.pollis.shared");
  assert.equal(info.PollisAppOnlyKeychainGroup, "8JT3WH45NZ.com.pollis.app");
  assert.ok(info.NSFaceIDUsageDescription.length > 0, "Face ID is described for the key read");
});

test("versions equal the app's and the share sheet shows the name Pollis", () => {
  assert.equal(info.CFBundleShortVersionString, "1.0.23");
  assert.equal(info.CFBundleVersion, "1002300");
  assert.equal(info.CFBundleDisplayName, "Pollis");
});

test("every locale gets every share string, with positional placeholders", () => {
  const tables = plugin.localizableStrings();
  assert.deepEqual(Object.keys(tables).sort(), SHIPPED_LPROJ);
  const en = JSON.parse(read("../../frontend/src/i18n/locales/en/mobile.json")).share;
  for (const [lproj, text] of Object.entries(tables) as [string, string][]) {
    for (const key of Object.keys(en)) {
      assert.match(text, new RegExp(`^"${key}" = ".+";$`, "m"), `${lproj} ${key}`);
    }
    assert.ok(!text.includes("{{"), `${lproj} has no i18next placeholder left`);
  }
  assert.match(tables.en, /"pinIncorrect_one" = "[^"]*%1\$@[^"]*";/);
  assert.match(tables.en, /"pinIncorrect_other" = "[^"]*%1\$@[^"]*";/);
});

test("the Swift reads only keys the plugin writes", () => {
  const written = new Set([...plugin.localizableStrings().en.matchAll(/^"(\w+)" =/gm)].map((m) => m[1]));
  const swift = plugin.SWIFT_SOURCES.filter((f: string) => f !== "LetterAvatar.swift")
    .map((f: string) => read(`../plugins/share-extension/${f}`))
    .join("\n");
  const plain = [...swift.matchAll(/ShareStrings\.t\("(\w+)"/g)].map((m) => m[1]);
  const plurals = [...swift.matchAll(/ShareStrings\.plural\("(\w+)"/g)].flatMap((m) => [`${m[1]}_one`, `${m[1]}_other`]);
  assert.ok(plain.length > 10, "the Swift uses the catalogue");
  for (const key of [...plain, ...plurals]) {
    assert.ok(written.has(key), `mobile:share.${key} is written`);
  }
});

test("the extension compiles the NSE's avatar renderer instead of a copy", () => {
  assert.ok(plugin.SWIFT_SOURCES.includes("LetterAvatar.swift"));
  assert.ok(!fs.existsSync(path.join(import.meta.dirname, "../plugins/share-extension/LetterAvatar.swift")));
});

test("POLLIS_IOS_SHARE=off, or the NSE off, leaves the config alone", () => {
  assert.equal(plugin.isEnabled({ POLLIS_IOS_SHARE: "off" }), false);
  assert.equal(plugin.isEnabled({ POLLIS_IOS_NSE: "off" }), false, "no App Group without the NSE");
  assert.equal(plugin.isEnabled({}), true);
  const before = { ios: { bundleIdentifier: "com.pollis.ios" }, version: "1.0.0" };
  const prev = process.env.POLLIS_IOS_SHARE;
  process.env.POLLIS_IOS_SHARE = "off";
  try {
    const out = plugin({ ...before }, { teamId: "8JT3WH45NZ" });
    assert.deepEqual(out, before, "no mods, no entitlements, no target");
  } finally {
    if (prev === undefined) {
      delete process.env.POLLIS_IOS_SHARE;
    } else {
      process.env.POLLIS_IOS_SHARE = prev;
    }
  }
});

test("app.json registers it after device unlock, with the team, and keeps the app's activity types", () => {
  const plugins = appJson.expo.plugins.map((p: unknown) => (Array.isArray(p) ? p[0] : p));
  const share = plugins.indexOf("./plugins/withShareExtension");
  assert.ok(share > plugins.indexOf("./plugins/withDeviceUnlock"), "after withDeviceUnlock");
  assert.equal(appJson.expo.plugins[share][1].teamId, "8JT3WH45NZ");
  // The share plugin never touches the app's Info.plist: its activity types stay the NSE's.
  assert.ok(!read("../plugins/withShareExtension.js").includes("withInfoPlist"));
  assert.ok(!read("../plugins/withShareExtension.js").includes("withEntitlementsPlist"));
});

test("a second extension's embed phase folds into the first", () => {
  const phase = (name: string, files: string[]) => ({ dstSubfolderSpec: 13, name, files: files.map((value) => ({ value })) });
  const phases: Record<string, unknown> = {
    P1: phase('"Embed Foundation Extensions"', ["nse"]),
    P1_comment: "Embed Foundation Extensions",
    P2: phase('"Copy Files"', ["share"]),
    P2_comment: "Copy Files",
  };
  const app = { buildPhases: [{ value: "S" }, { value: "P1" }, { value: "P2" }] };
  const project = {
    hash: { project: { objects: { PBXCopyFilesBuildPhase: phases } } },
    getFirstTarget: () => ({ firstTarget: app }),
  };
  foldEmbedPhases(project);
  assert.deepEqual(app.buildPhases.map((p) => p.value), ["S", "P1"]);
  assert.deepEqual((phases.P1 as { files: { value: string }[] }).files.map((f) => f.value), ["nse", "share"]);
  assert.ok(!("P2" in phases));
  // Alone, the phase is only renamed, as the NSE plugin always did.
  const solo: Record<string, unknown> = { Q: phase('"Copy Files"', ["nse"]), Q_comment: "Copy Files" };
  const soloApp = { buildPhases: [{ value: "Q" }] };
  foldEmbedPhases({ hash: { project: { objects: { PBXCopyFilesBuildPhase: solo } } }, getFirstTarget: () => ({ firstTarget: soloApp }) });
  assert.equal((solo.Q as { name: string }).name, '"Embed Foundation Extensions"');
  assert.equal(solo.Q_comment, "Embed Foundation Extensions");
});
