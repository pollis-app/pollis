// #2085: a message row names its sender by display name, else handle, else id.
import test from "node:test";
import assert from "node:assert/strict";

import { senderName } from "../src/utils/senderName.ts";

test("the display name wins over the handle", () => {
  assert.equal(senderName({ sender_id: "u", sender_username: "nicholas.dinh31_wnkd", sender_display_name: "Nick" }), "Nick");
});

test("without a display name the handle stands, then the id", () => {
  assert.equal(senderName({ sender_id: "u", sender_username: "nick", sender_display_name: null }), "nick");
  assert.equal(senderName({ sender_id: "u", sender_username: "nick", sender_display_name: "" }), "nick");
  assert.equal(senderName({ sender_id: "u" }), "u");
});
