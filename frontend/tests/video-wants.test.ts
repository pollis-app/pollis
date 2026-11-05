/* Tiles fold into one demand per track (the max of what is on screen), hidden once none
   shows it, and Rust hears about a change once — never the same value twice. */
import test from "node:test";
import assert from "node:assert/strict";

import { VideoWantsRegistry, type WantsReport } from "../src/screenshare/videoWantsRegistry.ts";

function registry() {
  const sent: Array<[string, WantsReport]> = [];
  const r = new VideoWantsRegistry(async (trackKey, wants) => {
    sent.push([trackKey, wants]);
  });
  return { r, sent };
}

test("a tile's report is sent; a second tile raises the fold to the max of both", () => {
  const { r, sent } = registry();
  const a = r.register("t");
  r.report("t", a, { width: 320, height: 180, fps: 15 });
  r.flush();
  assert.deepEqual(sent, [["t", { width: 320, height: 180, fps: 15 }]]);

  const b = r.register("t");
  r.report("t", b, { width: 1280, height: 720, fps: 60 });
  r.flush();
  assert.deepEqual(sent.at(-1), ["t", { width: 1280, height: 720, fps: 60 }]);

  // The small tile going away changes nothing the big one does not already want.
  r.unregister("t", a);
  r.flush();
  assert.equal(sent.length, 2);
});

test("a hidden tile counts as nothing; the last tile leaving sends hidden once", () => {
  const { r, sent } = registry();
  const a = r.register("t");
  r.report("t", a, { width: 640, height: 360, fps: 30 });
  r.flush();
  r.report("t", a, null);
  r.flush();
  assert.deepEqual(sent.at(-1), ["t", null]);
  r.flush();
  assert.equal(sent.length, 2, "an unchanged fold is not re-sent");

  r.unregister("t", a);
  r.flush();
  assert.equal(sent.length, 2, "already hidden: leaving sends nothing more");

  // A tile that reappears for the same key reports afresh, even with the same value.
  const b = r.register("t");
  r.report("t", b, { width: 640, height: 360, fps: 30 });
  r.flush();
  assert.deepEqual(sent.at(-1), ["t", { width: 640, height: 360, fps: 30 }]);
});

test("a registered tile that has not reported yet is hidden, and reports are coalesced", async () => {
  const { r, sent } = registry();
  const a = r.register("t");
  assert.equal(r.aggregate("t"), null);
  r.report("t", a, { width: 100, height: 100, fps: 15 });
  r.report("t", a, { width: 200, height: 200, fps: 15 });
  r.report("t", a, { width: 300, height: 300, fps: 15 });
  assert.equal(sent.length, 0, "nothing goes out synchronously");
  await new Promise((done) => setTimeout(done, 80));
  assert.deepEqual(sent, [["t", { width: 300, height: 300, fps: 15 }]]);
});

test("tracks are independent", () => {
  const { r, sent } = registry();
  const a = r.register("a");
  const b = r.register("b");
  r.report("a", a, { width: 10, height: 10, fps: 15 });
  r.report("b", b, { width: 20, height: 20, fps: 30 });
  r.flush();
  assert.deepEqual(
    sent.sort(),
    [
      ["a", { width: 10, height: 10, fps: 15 }],
      ["b", { width: 20, height: 20, fps: 30 }],
    ],
  );
});
