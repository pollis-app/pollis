/**
 * Image bridge — mirrors `@tauri-apps/api/image`'s `Image.fromBytes`.
 *
 * Used today only by `useBadge.ts` to load the Windows taskbar icon variant
 * and pass it to `window.setIcon`. Under Tauri, this delegates to the real
 * `Image.fromBytes` so Tauri's native setIcon path works. Under Electron,
 * we keep the bytes on the returned object; `setIcon` forwards them to the
 * preload bridge.
 */

import { hasElectron } from "./runtime";
import type { PollisImage } from "./window";

export const Image = {
  async fromBytes(bytes: Uint8Array): Promise<PollisImage> {
    if (hasElectron()) {
      return { bytes };
    }
    const mod = await import("@tauri-apps/api/image");
    const img = await mod.Image.fromBytes(bytes);
    // Tauri's Image is structurally compatible — its `rgba()`/`size()`
    // methods aren't needed by our consumers.
    return img as unknown as PollisImage;
  },
};
