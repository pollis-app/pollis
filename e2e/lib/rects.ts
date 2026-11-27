// Layout geometry for specs that check where things paint relative to each other.

import { expect, type Locator } from "@playwright/test";

export type Rect = { left: number; right: number; top: number; bottom: number };

/** The element's border box, as edges. */
export const rectOf = async (locator: Locator): Promise<Rect> => {
  const b = await locator.boundingBox();
  expect(b).not.toBeNull();
  return { left: b!.x, right: b!.x + b!.width, top: b!.y, bottom: b!.y + b!.height };
};

/** True when the two boxes share any area; touching edges do not count. */
export const intersects = (a: Rect, b: Rect) =>
  a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom;
