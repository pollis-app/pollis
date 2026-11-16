// What the iOS config plugins that localize native strings share (#1599 push, #1724 permission prompts):
// where the catalogues are, the .lproj each catalogue language becomes, .strings escaping, and the
// project's known regions. The catalogues are the single source; nothing here holds copy.
const fs = require("fs");
const path = require("path");

const LOCALES_DIR = path.join(__dirname, "..", "..", "frontend", "src", "i18n", "locales");
// Catalogue language → .lproj name. `zh` is Simplified Chinese (mobile/i18n/languages.ts).
const LPROJ = { en: "en", es: "es", fr: "fr", ru: "ru", uk: "uk", zh: "zh-Hans", ar: "ar" };

/** One section of a language's `mobile` catalogue (`push`, `permissions`), or `{}`. */
function mobileSection(lng, section, localesDir = LOCALES_DIR) {
  return JSON.parse(fs.readFileSync(path.join(localesDir, lng, "mobile.json"), "utf8"))?.[section] ?? {};
}

function escapeStrings(value) {
  return value.replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\n/g, "\\n");
}

/** Add every LPROJ region to the Xcode project's knownRegions (idempotent). */
function addKnownRegions(project) {
  const known = project.getFirstProject().firstProject.knownRegions ?? [];
  for (const lproj of Object.values(LPROJ)) {
    const quoted = lproj.includes("-") ? `"${lproj}"` : lproj;
    if (!known.includes(lproj) && !known.includes(quoted)) {
      known.push(quoted);
    }
  }
  project.getFirstProject().firstProject.knownRegions = known;
  return project;
}

module.exports = { LOCALES_DIR, LPROJ, mobileSection, escapeStrings, addKnownRegions };
