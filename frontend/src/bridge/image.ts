/** `Image.fromBytes` over `@tauri-apps/api/image`, for `useBadge.ts`. */

import type { PollisImage } from "./window";

export const Image = {
  async fromBytes(bytes: Uint8Array): Promise<PollisImage> {
    const mod = await import("@tauri-apps/api/image");
    const img = await mod.Image.fromBytes(bytes);
    return img as unknown as PollisImage;
  },
};
