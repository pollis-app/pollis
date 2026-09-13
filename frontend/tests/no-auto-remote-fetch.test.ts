/*
 * Nothing the renderer paints reaches a remote host on its own.
 *
 * Two distinct leaks, one property.
 *
 * 1. `MediaLinkUnfurl` gates remote media behind a click, but it carried an
 *    allowlist — `AUTO_LOAD_HOSTS = ["cdn.pollis.com"]` — whose members loaded
 *    the moment a message scrolled into view. The hostname comes out of MESSAGE
 *    TEXT, so the sender chooses it, and the check looked at the host alone:
 *    the path stayed the sender's, which makes the URL a per-recipient beacon
 *    that fires on read. And a webview `<img src>` is the renderer's own
 *    request, not `http_client`'s, so it cannot ride the relay overlay — the
 *    "first party" whose knowledge the exception waved away is precisely the
 *    party the overlay hides the address from.
 *
 * 2. The auto-updater is the other request the overlay cannot reach by itself
 *    (the plugin builds its own client in Rust). `bridge/updater.ts` must ask
 *    Rust what is allowed before every check rather than always going direct.
 *
 * Both are source-shape properties, invisible to a rendering test: the failure
 * is "somebody adds an auto-load host again" or "somebody drops the plan
 * lookup".
 *
 *   node --test frontend/tests/
 */

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

  // The gate is the whole component: `show` decides between the placeholder
  // button and the real <img>/<video>. It may depend on nothing but what the
  // reader has revealed.
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

  // Belt and braces: no literal first-party host anywhere in the executable
  // part of the file.
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
