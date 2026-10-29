// Save and share for the media viewer (#1248), from a NAMED copy: the library and share sheet type a file by extension.

import * as MediaLibrary from "expo-media-library";
import * as Sharing from "expo-sharing";

export type SaveResult = "saved" | "denied";

/** Save to the photo library with ADD-ONLY access; "denied" lets the caller point at Settings. */
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
