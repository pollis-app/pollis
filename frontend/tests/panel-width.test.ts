/*
 * The bounds a dragged side panel is held to (#985).
 *
 * The gesture needs a browser; the arithmetic behind it does not, and the
 * arithmetic is where the rules actually live — minimum, collapse, the maximum
 * against the window, and what happens when BOTH panels are open and competing
 * for the same pixels. `panelWidth.ts` exists as a pure module so those can be
 * asked here rather than only through a pointer.
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";

import {
  MIN_CONTENT_PX,
  MIN_PANEL_PX,
  clampPanelPx,
  maxPanelPx,
  parseStoredWidth,
  resolveDrag,
} from "../src/components/Layout/panelWidth.ts";

test("a panel on its own may grow to the window minus the content floor", () => {
  // The headline rule: one panel open, so nothing but the centre column is
  // competing with it.
  assert.equal(maxPanelPx(1400, 0), 1400 - MIN_CONTENT_PX);
  assert.equal(maxPanelPx(800, 0), 750);
});

test("the other panel's width comes off the maximum", () => {
  // The combined rule: each panel is bounded by the room the OTHER one
  // actually occupies, so the two together always leave the centre column its
  // floor — no fixed "half the window each" split, which would stop someone
  // widening one panel while the other is a stub.
  assert.equal(maxPanelPx(1400, 300), 1050);

  const windowPx = 1000;
  const sidebar = maxPanelPx(windowPx, 260);
  assert.equal(sidebar + 260 + MIN_CONTENT_PX, windowPx, "content keeps its floor");
});

test("opening the second panel pulls the first one back inside the window", () => {
  // The case the combined rule exists for. A sidebar can be dragged very wide
  // while the right panel is CLOSED — nothing is competing with it then — and
  // opening the panel afterwards would otherwise leave no conversation at all.
  // This is the pass `panelWidthStore.clampToWindow` runs when either panel
  // opens or the window changes, spelled with the same two functions.
  const windowPx = 1200;
  const rightPanel = 300;
  const sidebar = clampPanelPx(1000, maxPanelPx(windowPx, rightPanel));

  assert.equal(sidebar, 850);
  assert.equal(sidebar + rightPanel + MIN_CONTENT_PX, windowPx);
});

test("a window too small for both leaves each panel at the minimum", () => {
  // The floor wins over the content rule only when nothing can satisfy
  // everyone: two usable panels beat two zero-width ones, and the user can
  // still collapse either with `mod+b`.
  const windowPx = 200;
  const sidebar = clampPanelPx(400, maxPanelPx(windowPx, 120));
  assert.equal(sidebar, MIN_PANEL_PX);
});

test("a window too small for everyone still leaves a panel usable", () => {
  // Never below the minimum: a cramped window should not silently collapse a
  // panel the user opened on purpose.
  assert.equal(maxPanelPx(120, 100), MIN_PANEL_PX);
  assert.equal(maxPanelPx(0, 0), MIN_PANEL_PX);
});

test("dragging below the minimum collapses rather than shrinking", () => {
  assert.deepEqual(resolveDrag(MIN_PANEL_PX - 1, 900), { kind: "collapse" });
  assert.deepEqual(resolveDrag(0, 900), { kind: "collapse" });
  assert.deepEqual(resolveDrag(-40, 900), { kind: "collapse" });
  // Dragging fast enough to leave the window entirely is still a collapse and
  // never a NaN width.
  assert.deepEqual(resolveDrag(Number.NaN, 900), { kind: "collapse" });
});

test("the minimum itself is a width, not a collapse", () => {
  // The boundary matters: 50px is the narrowest a panel may be, so landing
  // exactly on it is a legitimate answer and must not snap shut.
  assert.deepEqual(resolveDrag(MIN_PANEL_PX, 900), { kind: "width", px: MIN_PANEL_PX });
});

test("a drag past the maximum stops at the maximum", () => {
  assert.deepEqual(resolveDrag(5000, 900), { kind: "width", px: 900 });
  // Sub-pixel pointer positions are rounded, not carried into the DOM.
  assert.deepEqual(resolveDrag(240.6, 900), { kind: "width", px: 241 });
});

test("clamping never collapses", () => {
  // A window resize is not a gesture aimed at this panel, so a stored width
  // that no longer fits is pulled to the bounds and left open.
  assert.equal(clampPanelPx(900, 300), 300);
  assert.equal(clampPanelPx(10, 300), MIN_PANEL_PX);
  assert.equal(clampPanelPx(200, 300), 200);
});

test("clamping a shrinking window is idempotent", () => {
  // Two `resize` events in a row must not walk the width down further.
  const once = clampPanelPx(900, 420);
  assert.equal(clampPanelPx(once, 420), once);
});

test("a stored width survives a round trip, and junk does not", () => {
  assert.equal(parseStoredWidth("240"), 240);
  assert.equal(parseStoredWidth("240.7"), 241);
  // null means "no answer on this device" — the panel falls back to the
  // `--side-w` token, which is why every rejection returns null and not a
  // number.
  assert.equal(parseStoredWidth(null), null);
  assert.equal(parseStoredWidth(""), null);
  assert.equal(parseStoredWidth("wide"), null);
  assert.equal(parseStoredWidth("Infinity"), null);
  // A width below the minimum can only come from a hand-edited or stale
  // entry; the panel is collapsed or defaulted, never a 3px sliver.
  assert.equal(parseStoredWidth("0"), null);
  assert.equal(parseStoredWidth("-300"), null);
  assert.equal(parseStoredWidth(String(MIN_PANEL_PX)), MIN_PANEL_PX);
});
