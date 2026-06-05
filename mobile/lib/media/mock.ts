// Dev-only mock for the `get_media_path` command (issue #346).
//
// The Rust side of the file-path transport doesn't exist yet — when it
// does, it'll decrypt R2 bytes to a sandbox file and return the path.
// Until then this mock writes a placeholder image into the same dest dir
// and returns its `file://` URI, so the full mobile pipeline (resolve →
// expo-image render → unlink-on-unmount) can be exercised end-to-end
// against mock message data exactly as the real command will drive it.
//
// Registered via the bridge's mock registry, which always wins over the
// native bridge — so this is safe to leave installed in dev even after
// the real command lands, and trivially removed for production.
//
//   import { registerMediaMock } from "../lib/media/mock";
//   registerMediaMock(); // e.g. in a dev-only effect in app/_layout

import * as FileSystem from "expo-file-system/legacy";
import { registerMockCommand } from "../native";

// A small opaque PNG (amber square) — enough to confirm a real file was
// written, read back by expo-image, and unlinked. Not representative of
// any actual attachment; the real command returns decrypted user media.
const PLACEHOLDER_PNG_BASE64 =
  "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAAGklEQVR42mP4tT/qPyWYYdSA" +
  "UQNGDRguBgAAHKMSLu6egtEAAAAASUVORK5CYII=";

interface GetMediaPathArgs {
  contentHash: string;
  destDir: string;
}

export function registerMediaMock(): () => void {
  return registerMockCommand("get_media_path", async (args) => {
    const { contentHash, destDir } = (args ?? {}) as Partial<GetMediaPathArgs>;
    if (!contentHash || !destDir) {
      throw new Error("mock get_media_path: missing contentHash/destDir");
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
