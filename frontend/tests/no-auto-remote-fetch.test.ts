/* Nothing painted reaches a remote host on its own: no auto-load hosts in `MediaLinkUnfurl` (sender-chosen beacons), and the updater asks Rust's plan. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const UNFURL = fileURLToPath(
  new URL("../src/components/Message/MediaLinkUnfurl.tsx", import.meta.url),
);
const UPDATER = fileURLToPath(new URL("../src/bridge/updater.ts", import.meta.url));

test("no host loads remote media without a click", () => {
  const source = readFileSync(UNFURL, "utf8");

  // `show` may depend only on what the reader revealed.
  const show = source.match(/const show = (.+);/);
  assert.ok(show, "MediaLinkUnfurl must still compute a `show` gate");
  assert.equal(
    show[1],
    "revealed.has(link.url)",
    "`show` must be exactly the reader's own reveal — any additional disjunct " +
      "is an auto-load exception, and the sender picks the URL it is tested against",
  );

  for (const banned of ["AUTO_LOAD", "isFirstParty"]) {
    assert.ok(
      !new RegExp(`^(?!\\s*(//|\\*)).*\\b${banned}\\b`, "m").test(source),
      `${banned} is an auto-load allowlist; remote media is click-to-load for every host`,
    );
  }

  const code = source
    .split("\n")
    .filter((line) => !/^\s*(\/\/|\*|\/\*)/.test(line))
    .join("\n");
  assert.ok(
    !code.includes("pollis.com"),
    "no host may be special-cased in the unfurl path",
  );
});

test("the update check asks Rust whether it may go direct", () => {
  const source = readFileSync(UPDATER, "utf8");
  assert.ok(
    source.includes('invoke<UpdateCheckPlan>("get_update_check_plan")'),
    "bridge/updater.ts must consult pollis-core's overlay plan before checking",
  );
  assert.ok(
    /plan\.kind === "blocked"/.test(source) && /return null/.test(source),
    "a blocked plan (strict overlay, no circuit) must skip the check entirely " +
      "rather than fall back to a direct fetch",
  );
  assert.ok(
    /proxy: plan\.url/.test(source),
    "a proxy plan must be handed to the plugin so both the manifest fetch and " +
      "the artifact download ride the shim",
  );
});
