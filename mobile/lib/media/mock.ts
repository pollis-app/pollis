// Dev-only mock for the `get_media_path` command (issue #346).
//
// The real command exists — the `get_media_path` arm in
// `pollis-core/src/bridge.rs` decrypts R2 bytes to a sandbox file and returns
// its `file://` path. This mock writes a placeholder image into the same dest
// dir and returns the same shape, so the mobile pipeline (resolve →
// expo-image render → unlink-on-unmount) can be exercised against mock
// message data without R2 credentials or a real attachment.
//
// The real arm requires `r2Key` (the R2 object key) as well as `contentHash`
// and `destDir` (see `get_media_path` in `pollis-core/src/bridge.rs`), so the
// mock requires it too — a mock that accepts a call the real command would
// reject lets a caller drop `r2Key` and still pass tests. We don't fetch R2,
// so the key is only validated for presence, not used to key the placeholder.
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
