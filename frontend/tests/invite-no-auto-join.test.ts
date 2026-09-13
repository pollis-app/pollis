/*
 * #1094: opening an invite deep link must not JOIN anything.
 *
 * `pollis://invite/<token>` is reachable by anyone who can put a link in front
 * of the user — an SMS, another app, a web page with an `<a href="pollis://…">`
 * — and joining a group discloses their username and device identity to every
 * member of it. Both landing screens used to redeem on mount, so one tap was
 * enough.
 *
 * These are source-shape guards rather than render tests, because the thing that
 * must stay true is structural: the redeem call is reachable only from a press
 * handler, never from a mount effect. A render test of today's component would
 * pass again the moment someone re-adds an effect.
 *
 *   node --test frontend/tests/
 */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const read = (p: string) => readFileSync(join(root, p), "utf8");

test("desktop: the invite landing page does not auto-redeem", () => {
  const landing = read("frontend/src/pages/InviteLinkLandingPage.tsx");
  const form = read("frontend/src/pages/JoinByInvite.tsx");

  // `autoRedeem` was the prop that fired the mutation on mount. It is gone, and
  // staying gone is the invariant — a re-added prop is the regression.
  assert.ok(!landing.includes("autoRedeem"), "the landing page must not pass autoRedeem");
  assert.ok(!form.includes("autoRedeem"), "JoinByInvite must not accept autoRedeem");

  // And nothing in the form redeems from an effect.
  assert.ok(
    !/useEffect\([^)]*\)[\s\S]{0,400}?mutateAsync/.test(form),
    "JoinByInvite must not redeem from a mount effect"
  );
});

test("mobile: the invite deep link asks before joining", () => {
  const src = read("mobile/app/invite/[token].tsx");

  // There is an explicit press target, and a confirm phase to host it.
  assert.ok(src.includes('testID="btn-invite-join"'), "a Join press target must exist");
  assert.ok(src.includes('"confirm"'), "a confirm phase must exist");

  // The redeem call must sit in the press handler, not the mount effect. Check
  // by position: the effect's dependency array comes before `const join`, and
  // the only redeem call must come after it.
  const joinAt = src.indexOf("const join");
  const redeemAt = src.indexOf("redeem_group_invite_link");
  assert.ok(joinAt > 0, "a join handler must exist");
  assert.ok(redeemAt > joinAt, "redeem_group_invite_link must be called from the press handler");
  assert.equal(
    src.split("redeem_group_invite_link").length - 1,
    1,
    "exactly one redeem call site, so there is no second path that skips the prompt"
  );
});
