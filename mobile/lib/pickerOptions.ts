import * as ImagePicker from "expo-image-picker";

/**
 * Every library pick asks iOS for the most compatible representation, so a HEIC photo (or an
 * HEVC video) arrives as JPEG (H.264). The default copies the original HEIC, which desktop
 * webviews on Linux and Windows cannot decode. iOS only; Android ignores it.
 */
export const COMPATIBLE_PICK = {
  preferredAssetRepresentationMode: ImagePicker.UIImagePickerPreferredAssetRepresentationMode.Compatible,
} as const;
