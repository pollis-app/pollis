/* Swipe to reply (#1270): the curve, the ranges, the preview, and the wiring Maestro cannot see. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  SWIPE_REPLY,
  arrowProgress,
  dragDistance,
  isArmed,
  panRanges,
  replyDirection,
  towardStart,
} from "../lib/swipeReply.ts";
import { REPLY_SNIPPET_MAX, replySnippet, replyTargets } from "../lib/replyPreview.ts";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (p: string) => readFileSync(join(ROOT, p), "utf8");

test("the swipe goes toward the start edge: left in LTR, right in RTL", () => {
  assert.equal(replyDirection(false), -1);
  assert.equal(replyDirection(true), 1);
  // A drag the other way never moves the row.
  assert.equal(towardStart(40, -1), 0);
  assert.equal(towardStart(-40, -1), 40);
  assert.equal(towardStart(40, 1), 40);
  assert.equal(towardStart(-40, 1), 0);
});

test("the row follows 1:1 to the threshold, then resists, then stops", () => {
  const { threshold, max } = SWIPE_REPLY;
  assert.equal(dragDistance(0), 0);
  assert.equal(dragDistance(threshold / 2), threshold / 2);
  assert.equal(dragDistance(threshold), threshold);
  const past = dragDistance(threshold + 40);
  assert.ok(past > threshold && past < threshold + 40, "past the threshold the row lags the finger");
  assert.equal(dragDistance(10_000), max);
  let last = -1;
  for (let x = 0; x <= 400; x += 4) {
    const d = dragDistance(x);
    assert.ok(d >= last);
    last = d;
  }
});

test("the reply arms exactly at the threshold, and the arrow is full there", () => {
  const { threshold } = SWIPE_REPLY;
  assert.equal(isArmed(threshold - 1), false);
  assert.equal(isArmed(threshold), true);
  assert.equal(arrowProgress(0), 0);
  assert.equal(arrowProgress(threshold / 2), 0.5);
  assert.equal(arrowProgress(threshold), 1);
  assert.equal(arrowProgress(SWIPE_REPLY.max), 1);
  // The double tap's nudge shows the arrow without reaching "armed".
  assert.ok(arrowProgress(SWIPE_REPLY.nudge) > 0 && arrowProgress(SWIPE_REPLY.nudge) < 1);
});

test("the pan claims only travel toward the start edge, never a scroll", () => {
  const { activateX, failY } = SWIPE_REPLY;
  const ltr = panRanges(-1);
  // Any rightward travel (the back gestures) fails it.
  assert.equal(ltr.activeOffsetX[0], -activateX);
  assert.ok(ltr.activeOffsetX[1] > 1000);
  assert.equal(ltr.failOffsetX[1], activateX);
  assert.ok(ltr.failOffsetX[0] < -1000);
  assert.deepEqual(ltr.failOffsetY, [-failY, failY]);
  const rtl = panRanges(1);
  assert.equal(rtl.activeOffsetX[1], activateX);
  assert.equal(rtl.failOffsetX[0], -activateX);
  // A wobbly scroll gives the touch back before a swipe can start.
  assert.ok(failY <= activateX);
});

test("a reply shows one line of the original: text, else a file name", () => {
  assert.equal(replySnippet({ id: "a", content: "hi  there\nfriend", attachments: [] }), "hi there friend");
  const long = "x".repeat(REPLY_SNIPPET_MAX + 20);
  const cut = replySnippet({ id: "a", content: long, attachments: [] });
  assert.equal(cut, `${"x".repeat(REPLY_SNIPPET_MAX)}…`);
  assert.equal(
    replySnippet({ id: "a", content: "  ", attachments: [{ filename: "cat.jpg" }] }),
    "cat.jpg",
  );
  assert.equal(replySnippet({ id: "a", content: "", attachments: [] }), "");
});

test("reply targets are indexed in one pass, and only the replied-to ones", () => {
  const msgs = [
    { id: "1", content: "root", attachments: [], reply_to_id: null },
    { id: "2", content: "answer", attachments: [], reply_to_id: "1" },
    { id: "3", content: "orphan", attachments: [], reply_to_id: "gone" },
    { id: "4", content: "plain", attachments: [] },
  ];
  const map = replyTargets(msgs);
  assert.deepEqual([...map.keys()], ["1"]);
  assert.equal(replyTargets([{ id: "x", content: "", attachments: [] }]).size, 0);
});

const swipe = read("components/chat/SwipeToReply.tsx");
const row = read("components/chat/MessageRow.tsx");
const chat = read("app/chat/[id].tsx");

test("the row moves on the UI thread; JS runs only when a reply fires or the armed state flips", () => {
  assert.match(swipe, /from "react-native-gesture-handler"/);
  assert.match(swipe, /from "react-native-reanimated"/);
  assert.doesNotMatch(swipe, /Animated\.timing|new Animated\.Value|PanResponder/);
  const onUpdate = swipe.slice(swipe.indexOf(".onUpdate("), swipe.indexOf(".onEnd("));
  assert.ok(onUpdate.length > 0);
  // No JS call per frame: only the haptic, when armed flips.
  assert.doesNotMatch(onUpdate, /runOnJS/, "no JS call per frame");
  assert.equal(onUpdate.match(/scheduleOnRN/g)?.length ?? 0, 1);
  assert.match(onUpdate, /if \(event\) \{\s*scheduleOnRN\(buzz, event\);/);
  // Reset in onEnd: onFinalize would cancel the double tap's nudge.
  assert.doesNotMatch(swipe, /\.onFinalize\(/);
  assert.match(swipe, /numberOfTaps\(2\)/);
  assert.match(swipe, /Gesture\.Race\(pan, doubleTap\)/);
});

test("rows reply only once settled, and every reply path is wired", () => {
  assert.match(row, /<SwipeToReply onReply=\{onReply\}[ >]/);
  assert.match(row, /name: "reply", label: t\("actions\.reply"\)/);
  assert.match(chat, /onReply=\{settled \? \(\) => startReply\(m\) : undefined\}/);
  assert.match(chat, /replyToId: replyTarget\?\.id/);
  assert.match(read("components/chat/MessageActionsSheet.tsx"), /testID="btn-reply"/);
});

test("a reply never outlives its conversation or survives an edit", () => {
  assert.match(
    chat,
    /useEffect\(\(\) => \{\s*setReplyTarget\(null\);\s*setHighlightId\(null\);\s*\}, \[conversationId\]\);/,
  );
  const onEdit = chat.slice(chat.indexOf("onEdit={() => {"));
  assert.match(onEdit.slice(0, 120), /setReplyTarget\(null\)/);
  const startReply = chat.slice(chat.indexOf("const startReply"));
  assert.match(startReply.slice(0, 200), /setEditTarget\(null\)/);
});

test("the reply arrow mirrors in RTL", () => {
  assert.match(read("components/icons.tsx"), /reply: mirrored\(Reply\)/);
});
