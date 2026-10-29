// Dev-only mock for `get_media_path` (#346): writes a placeholder and, like the real arm, requires `r2Key`.

import * as FileSystem from "expo-file-system/legacy";
import { registerMockCommand } from "../native";

// A small opaque PNG (amber square).
const PLACEHOLDER_PNG_BASE64 =
  "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAAGklEQVR42mP4tT/qPyWYYdSA" +
  "UQNGDRguBgAAHKMSLu6egtEAAAAASUVORK5CYII=";

interface GetMediaPathArgs {
  r2Key: string;
  contentHash: string;
  destDir: string;
}

export function registerMediaMock(): () => void {
  return registerMockCommand("get_media_path", async (args) => {
    const { r2Key, contentHash, destDir } = (args ??
      {}) as Partial<GetMediaPathArgs>;
    if (!r2Key || !contentHash || !destDir) {
      throw new Error("mock get_media_path: missing r2Key/contentHash/destDir");
    }
    await FileSystem.makeDirectoryAsync(destDir, { intermediates: true }).catch(
      () => {
        // Already exists — fine.
      },
    );
    const uri = `${destDir}${contentHash}.png`;
    await FileSystem.writeAsStringAsync(uri, PLACEHOLDER_PNG_BASE64, {
      encoding: "base64",
    });
    return uri;
  });
}
