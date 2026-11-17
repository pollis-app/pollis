/* Helpers extracted from copy-pasted code (#1731): each keeps the exact output its copies produced. */

import test from "node:test";
import assert from "node:assert/strict";

import { fieldControlClass, fieldControlStyle } from "../src/components/ui/fieldStyles.ts";
import { deriveSlug, resolveSlug } from "../src/utils/urlRouting.ts";
import { detachChannel } from "../src/utils/detachChannel.ts";

// The control radius buttons use, and the shared focus ring (#1964).
const RING =
  "font-mono text-body rounded-control focus:outline-none focus-visible:ring-2 focus-visible:ring-accent focus-visible:ring-offset-2 focus-visible:ring-offset-bg transition-all bg-surface text-fg border-2";

test("TextInput / TextArea class: the border tracks error, then focus", () => {
  assert.equal(
    fieldControlClass("placeholder-leading-1", undefined, false),
    `w-full py-2 placeholder-leading-1 ${RING} border-line-strong`,
  );
  assert.equal(
    fieldControlClass("resize-none", undefined, true),
    `w-full py-2 resize-none ${RING} border-accent`,
  );
  // An error wins over focus.
  assert.equal(
    fieldControlClass("resize-none", "bad", true),
    `w-full py-2 resize-none ${RING} border-danger`,
  );
});

test("TextInput / TextArea style: chevron padding only when focused and enabled; disabled dims", () => {
  assert.deepEqual(fieldControlStyle(false, false), {
    paddingInlineStart: "0.75rem",
    paddingInlineEnd: "0.75rem",
    outline: "none",
    opacity: 1,
    cursor: undefined,
  });
  assert.equal(fieldControlStyle(true, false).paddingInlineStart, "1.5rem");
  const disabled = fieldControlStyle(true, true);
  assert.equal(disabled.paddingInlineStart, "0.75rem");
  assert.equal(disabled.opacity, 0.5);
  assert.equal(disabled.cursor, "not-allowed");
});

test("create forms submit the derived slug until the user edits it", () => {
  assert.equal(resolveSlug("My Group", "", false), deriveSlug("My Group"));
  assert.equal(resolveSlug("My Group", "stale", false), "my-group");
  assert.equal(resolveSlug("My Group", " custom ", true), "custom");
  assert.equal(resolveSlug("   ", "", false), "");
});

test("detachChannel drops the handler and clears the field", () => {
  let calls = 0;
  const channel = { onmessage: (_ev: number) => { calls++; } };
  assert.equal(detachChannel(channel), null);
  (channel.onmessage as (ev: number) => void)(1);
  assert.equal(calls, 0, "a late event is ignored");
  assert.equal(detachChannel(null), null);
});
