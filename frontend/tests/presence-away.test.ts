/* #2110: three presence states from LiveKit rooms. Online while any shared room shows the user in
   use, away while every room that shows them shows them not in use, offline while none does. A
   client that predates the `active` attribute is in use (missing = active), so older builds still
   show online. Peers see this desktop away once its window is hidden or idle, not on a blur. */
import test from "node:test";
import assert from "node:assert/strict";

import { presenceStore, statusFromRooms } from "../src/stores/presenceStore.ts";
import { UserActivityStore } from "../src/stores/userActivityStore.ts";

test("the rule: no room is offline, any room in use is online, else away", () => {
  assert.equal(statusFromRooms(undefined), "offline");
  assert.equal(statusFromRooms({}), "offline");
  assert.equal(statusFromRooms({ dm: true }), "online");
  assert.equal(statusFromRooms({ dm: false }), "away");
  assert.equal(statusFromRooms({ dm: false, group: true }), "online");
  assert.equal(statusFromRooms({ dm: false, group: false }), "away");
});

test("a presence event without `active` (an older core or caller) means in use", () => {
  presenceStore.setPresent("u-old", "room-1", true);
  assert.equal(presenceStore.status("u-old"), "online");
  presenceStore.setPresent("u-old", "room-1", false);
  assert.equal(presenceStore.status("u-old"), "offline");
});

test("away in one room, online in another: online; leaving the active room: away; leaving both: offline", () => {
  presenceStore.setPresent("u-bob", "dm", true, false);
  assert.equal(presenceStore.status("u-bob"), "away");
  assert.equal(presenceStore.isPresent("u-bob"), true);
  presenceStore.setPresent("u-bob", "group", true, true);
  assert.equal(presenceStore.status("u-bob"), "online");
  presenceStore.setPresent("u-bob", "group", false);
  assert.equal(presenceStore.status("u-bob"), "away");
  presenceStore.resetRoom("dm");
  assert.equal(presenceStore.status("u-bob"), "offline");
  assert.equal(presenceStore.isPresent("u-bob"), false);
});

test("an attribute change in place flips the user without leaving the room", () => {
  presenceStore.setPresent("u-carol", "dm", true, true);
  presenceStore.setPresent("u-carol", "dm", true, false);
  assert.equal(presenceStore.status("u-carol"), "away");
  presenceStore.setPresent("u-carol", "dm", true, true);
  assert.equal(presenceStore.status("u-carol"), "online");
  presenceStore.resetRoom("dm");
});

test("you are always online on your own device", () => {
  presenceStore.setSelfUserId("u-me");
  presenceStore.setPresent("u-me", "dm", true, false);
  assert.equal(presenceStore.status("u-me"), "online");
  presenceStore.setSelfUserId(null);
  presenceStore.resetRoom("dm");
});

test("peers see away once the window is hidden or idle, not on a blur alone", () => {
  const s = new UserActivityStore();
  assert.equal(s.presenceActive, true);
  s.setFocused(false);
  assert.equal(s.presenceActive, true, "a blur is not away");
  assert.equal(s.isActive, false, "but the push hold (#2056) still lets go at once");
  s.setIdle(true);
  assert.equal(s.presenceActive, false, "blurred past the idle threshold is away");
  s.setFocused(true);
  s.noteInput(Date.now());
  assert.equal(s.presenceActive, true, "focus is input: back at once");
  s.setVisible(false);
  assert.equal(s.presenceActive, false, "a hidden window is away");
});
