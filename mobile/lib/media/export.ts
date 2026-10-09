// Save and share for the media viewer (#1248) — mobile's counterpart to
// desktop's lightbox Download button (`save_media_to_path`). Desktop writes
// to a path the user picks; a phone has no such path, so images and video go
// to the photo library and any file goes through the share sheet.
//
// Both take a NAMED local copy (`resolveNamedMediaUri`): the library and the
// share sheet type a file by its extension, and the decrypted cache file has
// none. Nothing here downloads anything — the bytes are the ones the viewer
// already decrypted for display.

import * as MediaLibrary from "expo-media-library";
import * as Sharing from "expo-sharing";

export type SaveResult = "saved" | "denied";

/**
 * Save an image or video to the photo library, asking for ADD-ONLY access on
 * demand. The app never reads the library: on iOS that is the
 * NSPhotoLibraryAddUsageDescription prompt, and on Android 13+ saving needs
 * no runtime permission at all (`[]` asks for none of the read grants).
 * Resolves "denied" when the user has said no, so the caller can point them
 * at Settings.
 */
export async function saveToPhotoLibrary(uri: string): Promise<SaveResult> {
  const permission = await MediaLibrary.requestPermissionsAsync(true, []);
  if (!permission.granted) {
    return "denied";
  }
  await MediaLibrary.Asset.create(uri);
  return "saved";
}

/**
 * Hand a file to the system share sheet. Resolves false when the device has
 * no share target at all.
 */
export async function shareFile(
  uri: string,
  mimeType: string,
  dialogTitle: string,
): Promise<boolean> {
  if (!(await Sharing.isAvailableAsync())) {
    return false;
  }
  await Sharing.shareAsync(uri, { mimeType, dialogTitle });
  return true;
}
