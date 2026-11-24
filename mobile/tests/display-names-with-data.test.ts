// Display names arrive with the user data they belong to (#2085): lists never look them up in a
// second read, so a row is named once instead of handle-then-name.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (p: string) => readFileSync(join(root, p), "utf8");

for (const screen of ["app/(tabs)/direct.tsx", "app/dm/requests.tsx", "app/group/members.tsx"]) {
  test(`${screen} names people from its own rows, with no second profile read`, () => {
    const src = read(screen);
    assert.doesNotMatch(src, /useUserProfiles/);
  });
}

test("the rows carry the display name the screens read", () => {
  const dms = read("hooks/queries/useDMChannels.ts");
  assert.match(dms, /preferred_name\?: string \| null;/);
  assert.match(dms, /user2_preferred_name: other\?\.preferred_name \?\? undefined,/);
  assert.match(read("app/(tabs)/direct.tsx"), /preferred_name: d\.user2_preferred_name/);
  assert.match(read("app/dm/requests.tsx"), /preferred_name: d\.user2_preferred_name/);
  const members = read("app/group/members.tsx");
  assert.match(members, /preferred_name: m\.display_name/);
  assert.match(members, /preferred_name: inv\.invitee_preferred_name/);
});

test("the reaction sheet reads profiles only for whoever the roster cannot name", () => {
  const sheet = read("components/chat/ReactionBreakdownSheet.tsx");
  assert.match(sheet, /roster\.get\(id\)\?\.preferred_name === undefined/);
  assert.match(sheet, /useUserProfiles\(unnamed\)/);
  const chat = read("app/chat/[id].tsx");
  assert.match(chat, /preferred_name: m\.display_name \?\? null/);
  assert.match(chat, /preferred_name: m\.preferred_name \?\? null/);
});
