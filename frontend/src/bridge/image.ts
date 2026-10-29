/**
 * Image bridge — `Image.fromBytes`, used by `useBadge.ts` for the Windows
 * taskbar icon.
 */

import type { PollisImage } from "./window";

export const Image = {
  async fromBytes(bytes: Uint8Array): Promise<PollisImage> {
    const mod = await import("@tauri-apps/api/image");
    const img = await mod.Image.fromBytes(bytes);
    return img as unknown as PollisImage;
  },
};
