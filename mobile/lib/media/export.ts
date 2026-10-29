// Save and share for the media viewer (#1248): images and video go to the photo
// library, any file to the share sheet. Both take the NAMED local copy
// (`resolveNamedMediaUri`), since both type a file by its extension.

import * as MediaLibrary from "expo-media-library";
import * as Sharing from "expo-sharing";

export type SaveResult = "saved" | "denied";

/**
 * Asks for ADD-ONLY access on demand: the app never reads the library (`[]`
 * requests none of the read grants). "denied" lets the caller point at Settings.
 */
export async function saveToPhotoLibrary(uri: string): Promise<SaveResult> {
  const permission = await MediaLibrary.requestPermissionsAsync(true, []);
  if (!permission.granted) {
    return "denied";
  }
  await MediaLibrary.Asset.create(uri);
  return "saved";
}

/** False when the device has no share target. */
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
