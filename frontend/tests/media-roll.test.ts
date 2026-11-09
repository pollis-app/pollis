/* The media spotlight's roll (#1788): one message's images and videos, in mobile's order, wrapping as mobile's pager does. */

import test from "node:test";
import assert from "node:assert/strict";

import { isRollMedia, spotlightRoll, stepRoll } from "../src/utils/mediaRoll.ts";
import { collectViewerItems } from "../../mobile/lib/media/viewer.ts";

const att = (id: string, content_type: string) => ({ id, content_type });

const MIXED = [
  att("a", "image/jpeg"),
  att("doc", "application/pdf"),
  att("b", "video/webm"),
  att("song", "audio/mpeg"),
  att("c", "image/png"),
];

test("images and videos walk together in attachment order; audio and files are left out", () => {
  const { items, index } = spotlightRoll(MIXED, MIXED[2]);
  assert.deepEqual(items.map((a) => a.id), ["a", "b", "c"]);
  assert.equal(index, 1);
});

test("audio and files open alone", () => {
  for (const opened of [MIXED[1], MIXED[3]]) {
    const { items, index } = spotlightRoll(MIXED, opened);
    assert.deepEqual(items.map((a) => a.id), [opened.id]);
    assert.equal(index, 0);
  }
});

test("without the message, or opened from outside it, the spotlight holds just the one item", () => {
  const stray = att("z", "image/gif");
  assert.deepEqual(spotlightRoll(undefined, stray), { items: [stray], index: 0 });
  assert.deepEqual(spotlightRoll(MIXED, stray), { items: [stray], index: 0 });
});

test("a repeated id is listed once", () => {
  const twice = [att("a", "image/jpeg"), att("a", "image/jpeg"), att("b", "image/jpeg")];
  assert.deepEqual(spotlightRoll(twice, twice[2]).items.map((a) => a.id), ["a", "b"]);
});

test("stepping wraps at both ends, as mobile's pager does", () => {
  assert.equal(stepRoll(2, 1, 3), 0);
  assert.equal(stepRoll(0, -1, 3), 2);
  assert.equal(stepRoll(1, 1, 3), 2);
  assert.equal(stepRoll(0, 1, 1), 0);
});

test("the roll for one message is mobile's roll for the same message", () => {
  const message = { id: "m", created_at: 1, attachments: MIXED };
  for (const opened of MIXED) {
    // The mobile type wants full attachments; only id and content_type are read.
    const mobile = collectViewerItems([message] as never, opened.id);
    const desktop = spotlightRoll(MIXED, opened);
    assert.deepEqual(
      desktop.items.map((a) => a.id),
      mobile.items.map((a) => a.id),
      `rolls differ when opening ${opened.id}`,
    );
    assert.equal(desktop.index, mobile.index, `start differs when opening ${opened.id}`);
  }
  assert.equal(isRollMedia("image/heic"), true);
  assert.equal(isRollMedia("audio/ogg"), false);
});
