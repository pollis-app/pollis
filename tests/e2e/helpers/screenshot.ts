// Pixel-stable screenshot writer.
//
// Why this exists: `page.screenshot({ path })` always writes the file,
// which means git sees every screenshot as modified after every run
// even when nothing about the rendered page changed. PNG re-encoding
// (compression dictionary state, IDAT chunk boundaries) can produce
// different bytes for identical pixels, so a plain byte-compare isn't
// reliable either.
//
// Approach: take the screenshot to a buffer, decode it + the existing
// file via pngjs, compare the raw RGBA pixel buffers, and only write
// to disk when they differ. Result: `git status` only flags screenshots
// where the visual output actually changed.

import * as fs from "node:fs";
import type { Page } from "@playwright/test";
import { PNG } from "pngjs";

/**
 * Take a screenshot of `page` and write it to `filepath` only if its
 * decoded pixels differ from whatever is already on disk. If the file
 * doesn't exist yet, write unconditionally.
 *
 * No-op for the (rare) case where the new screenshot is byte-identical
 * to the old — pngjs decoding is the expensive path, so the fast
 * happy-path is the cheap `Buffer.equals` short-circuit.
 */
export async function saveScreenshotIfChanged(
  page: Page,
  filepath: string,
): Promise<void> {
  const fresh = await page.screenshot();

  let existing: Buffer | null = null;
  try {
    existing = fs.readFileSync(filepath);
  } catch {
    // ENOENT (new file) — fall through, write unconditionally.
  }

  if (existing !== null) {
    // Fast path: identical bytes. Chromium's encoder is mostly
    // deterministic so this hits often.
    if (existing.equals(fresh)) {
      return;
    }
    // Slow path: bytes differ, but the pixels might still match.
    if (pixelsEqual(existing, fresh)) {
      return;
    }
  }

  fs.writeFileSync(filepath, fresh);
}

function pixelsEqual(a: Buffer, b: Buffer): boolean {
  try {
    const decA = PNG.sync.read(a);
    const decB = PNG.sync.read(b);
    if (decA.width !== decB.width || decA.height !== decB.height) {
      return false;
    }
    return decA.data.equals(decB.data);
  } catch {
    // A decode failure means we can't safely claim equality —
    // pessimistically treat as changed so the file gets refreshed.
    return false;
  }
}
