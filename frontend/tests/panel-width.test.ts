/* Dragged side-panel bounds (#985), via the pure `panelWidth.ts`. */

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
  assert.equal(maxPanelPx(1400, 0), 1400 - MIN_CONTENT_PX);
  assert.equal(maxPanelPx(800, 0), 750);
});

test("the other panel's width comes off the maximum", () => {
  // Each panel is bounded by the room the OTHER one occupies.
  assert.equal(maxPanelPx(1400, 300), 1050);

  const windowPx = 1000;
  const sidebar = maxPanelPx(windowPx, 260);
  assert.equal(sidebar + 260 + MIN_CONTENT_PX, windowPx, "content keeps its floor");
});

test("opening the second panel pulls the first one back inside the window", () => {
  // A wide sidebar plus a newly opened right panel must still leave content room (`clampToWindow`).
  const windowPx = 1200;
  const rightPanel = 300;
  const sidebar = clampPanelPx(1000, maxPanelPx(windowPx, rightPanel));

  assert.equal(sidebar, 850);
  assert.equal(sidebar + rightPanel + MIN_CONTENT_PX, windowPx);
});

test("a window too small for both leaves each panel at the minimum", () => {
  // The panel floor wins only when nothing can satisfy everyone.
  const windowPx = 200;
  const sidebar = clampPanelPx(400, maxPanelPx(windowPx, 120));
  assert.equal(sidebar, MIN_PANEL_PX);
});

test("a window too small for everyone still leaves a panel usable", () => {
  assert.equal(maxPanelPx(120, 100), MIN_PANEL_PX);
  assert.equal(maxPanelPx(0, 0), MIN_PANEL_PX);
});

test("dragging below the minimum collapses rather than shrinking", () => {
  assert.deepEqual(resolveDrag(MIN_PANEL_PX - 1, 900), { kind: "collapse" });
  assert.deepEqual(resolveDrag(0, 900), { kind: "collapse" });
  assert.deepEqual(resolveDrag(-40, 900), { kind: "collapse" });
  assert.deepEqual(resolveDrag(Number.NaN, 900), { kind: "collapse" });
});

test("the minimum itself is a width, not a collapse", () => {
  // Exactly the minimum is valid, not a collapse.
  assert.deepEqual(resolveDrag(MIN_PANEL_PX, 900), { kind: "width", px: MIN_PANEL_PX });
});

test("a drag past the maximum stops at the maximum", () => {
  assert.deepEqual(resolveDrag(5000, 900), { kind: "width", px: 900 });
  // Sub-pixel pointer positions are rounded, not carried into the DOM.
  assert.deepEqual(resolveDrag(240.6, 900), { kind: "width", px: 241 });
});

test("clamping never collapses", () => {
  // A resize clamps a stored width but keeps the panel open.
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
  // null = no device answer, falling back to `--side-w`.
  assert.equal(parseStoredWidth(null), null);
  assert.equal(parseStoredWidth(""), null);
  assert.equal(parseStoredWidth("wide"), null);
  assert.equal(parseStoredWidth("Infinity"), null);
  assert.equal(parseStoredWidth("0"), null);
  assert.equal(parseStoredWidth("-300"), null);
  assert.equal(parseStoredWidth(String(MIN_PANEL_PX)), MIN_PANEL_PX);
});
