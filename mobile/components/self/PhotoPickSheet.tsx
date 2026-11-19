import { Linking } from "react-native";
import * as ImagePicker from "expo-image-picker";
import { useTranslation } from "react-i18next";
import { Group, ListRow } from "../ui";
import { SheetOverlay, afterSheetClose } from "../chat/SheetOverlay";
import { Icon } from "../icons";
import { semantic, space } from "../../theme/tokens";
import { COMPATIBLE_PICK } from "../../lib/pickerOptions";

/** What the picker returns is a `file://` URI; core reads a path. */
export function uriToPath(uri: string): string {
  if (uri.startsWith("file://")) {
    return decodeURIComponent(uri.slice("file://".length));
  }
  return uri;
}

// Square crop in the system editor; core re-encodes and strips EXIF on upload.
const PICK: ImagePicker.ImagePickerOptions = {
  mediaTypes: "images",
  allowsEditing: true,
  aspect: [1, 1],
  quality: 0.9,
  ...COMPATIBLE_PICK,
};

/**
 * The "library / camera / remove" sheet behind a profile photo: the main one
 * (`ProfilePhotoEditor`) and a group alias's (#2003). `onPicked` gets the square
 * photo's URI; `remove` adds the destructive row with its own label.
 */
export function PhotoPickSheet({
  title,
  onClose,
  onPicked,
  onCameraDenied,
  remove,
}: {
  title: string;
  onClose: () => void;
  onPicked: (uri: string) => void;
  onCameraDenied: () => void;
  remove?: { label: string; onPress: () => void };
}) {
  const { t } = useTranslation("mobile");

  const fromLibrary = async () => {
    const result = await ImagePicker.launchImageLibraryAsync(PICK);
    if (!result.canceled && result.assets.length > 0) {
      onPicked(result.assets[0].uri);
    }
  };

  const fromCamera = async () => {
    const perm = await ImagePicker.requestCameraPermissionsAsync();
    if (!perm.granted) {
      onCameraDenied();
      if (!perm.canAskAgain) {
        void Linking.openSettings();
      }
      return;
    }
    const result = await ImagePicker.launchCameraAsync({
      ...PICK,
      cameraType: ImagePicker.CameraType.front,
    });
    if (!result.canceled && result.assets.length > 0) {
      onPicked(result.assets[0].uri);
    }
  };

  const choose = (fn: () => void) => {
    onClose();
    afterSheetClose(fn);
  };

  return (
    <SheetOverlay title={title} onClose={onClose} testID="sheet-avatar">
      <Group surface="high">
        <ListRow
          testID="btn-avatar-library"
          glyph={<Icon.images size={22} color={semantic.dim} />}
          name={t("self.userSettings.photo.library")}
          onPress={() => choose(() => void fromLibrary())}
        />
        <ListRow
          testID="btn-avatar-camera"
          glyph={<Icon.camera size={22} color={semantic.dim} />}
          name={t("self.userSettings.photo.camera")}
          onPress={() => choose(() => void fromCamera())}
        />
      </Group>
      {remove ? (
        <Group surface="high" style={{ marginTop: space.lg }}>
          <ListRow
            testID="btn-avatar-remove"
            glyph={<Icon.trash size={22} color={semantic.danger} />}
            name={remove.label}
            onPress={() => choose(remove.onPress)}
          />
        </Group>
      ) : null}
    </SheetOverlay>
  );
}
