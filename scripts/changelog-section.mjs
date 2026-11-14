#!/usr/bin/env node
/**
 * One release's entry from CHANGELOG.md, for the desktop release (#1916):
 * the text becomes the `notes` field of the updater manifests (the app's
 * Update page shows it) and the top of the GitHub release body.
 *
 *   node scripts/changelog-section.mjs v1.19.12            # reads ./CHANGELOG.md
 *   node scripts/changelog-section.mjs v1.19.12 path/to.md
 *
 * Prints the body of the `## v1.19.12` entry, without its heading. A release
 * must never fail on its notes, so this ALWAYS exits 0: no entry, a renamed
 * heading, a missing file or a missing argument all print nothing.
 * Run the tests: node --test scripts/test-changelog-section.mjs
 */

import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const VERSION = /^v?(\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.+-]+)?)$/;

/** The version a `## ` heading names, or null for a range ("v1.9.8 – v1.11.4") or any other heading. */
function headingVersion(line) {
  const heading = line.match(/^##\s+(\S+)(?:\s+(.*))?$/);
  if (!heading) {
    return null;
  }
  const version = heading[1].match(VERSION);
  if (!version) {
    return null;
  }
  // After the version, only a date may follow: "— 2026-11-14". A second version makes it a range.
  const rest = (heading[2] ?? "").replace(/^[—–-]\s*/, "").trim();
  if (/^v?\d+\.\d+/.test(rest)) {
    return null;
  }
  return version[1];
}

/** The body of `tag`'s entry in `markdown`, trimmed; "" when there is none. */
export function changelogSection(markdown, tag) {
  const wanted = String(tag ?? "").trim().match(VERSION);
  if (!wanted || typeof markdown !== "string") {
    return "";
  }
  const lines = markdown.replace(/\r\n/g, "\n").split("\n");
  const start = lines.findIndex((line) => headingVersion(line) === wanted[1]);
  if (start === -1) {
    return "";
  }
  const body = [];
  for (const line of lines.slice(start + 1)) {
    // The next entry, a top-level heading, or the rule that closes the list of entries.
    if (/^#{1,2}\s/.test(line) || /^---+\s*$/.test(line)) {
      break;
    }
    body.push(line.trimEnd());
  }
  return body.join("\n").trim();
}

function main(argv) {
  const [tag, file = "CHANGELOG.md"] = argv;
  let section = "";
  try {
    section = changelogSection(readFileSync(file, "utf8"), tag);
  } catch {
    section = "";
  }
  if (section) {
    process.stdout.write(`${section}\n`);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main(process.argv.slice(2));
}
