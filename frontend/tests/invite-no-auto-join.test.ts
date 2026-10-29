/* #1094: an invite deep link must not join on arrival; source guards that redeem is reachable only from a press handler. */
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

  // `autoRedeem` fired on mount; it must stay gone.
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

  // By position: the one redeem call comes after `const join`, past the effect.
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
