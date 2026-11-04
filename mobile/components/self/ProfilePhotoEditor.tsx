import { useState } from "react";
import { ActivityIndicator, Linking, Pressable, Text, View } from "react-native";
import { Image } from "expo-image";
import * as ImagePicker from "expo-image-picker";
import { useTranslation } from "react-i18next";
import { Button, Group, ListRow } from "../ui";
import { UserAvatar } from "../UserAvatar";
import { SheetOverlay, afterSheetClose } from "../chat/SheetOverlay";
import { ErrorText } from "./SettingsField";
import { Icon } from "../icons";
import { semantic, type as ty, space } from "../../theme/tokens";
import { useRemoveAvatar, useSetAvatar } from "../../hooks/queries";
import { avatarHash } from "../../lib/avatarKey";

const SIZE = 96;

// What the picker returns is a `file://` URI; core reads a path.
function uriToPath(uri: string): string {
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
};

/** Set, change or remove your own profile photo (Self → Account details). */
export function ProfilePhotoEditor({
  userId,
  avatarKey,
  label,
}: {
  userId: string | null | undefined;
  avatarKey: string | null | undefined;
  label: string;
}) {
  const { t } = useTranslation("mobile");
  const setAvatar = useSetAvatar();
  const removeAvatar = useRemoveAvatar();
  const [sheetOpen, setSheetOpen] = useState(false);
  // The picked photo, shown at once while it uploads.
  const [preview, setPreview] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<"saved" | "removed" | null>(null);

  const hasPhoto = avatarHash(avatarKey) !== null;
  const busy = setAvatar.isPending || removeAvatar.isPending;

  const upload = (uri: string) => {
    setPreview(uri);
    setError(null);
    setDone(null);
    setAvatar.mutate(uriToPath(uri), {
      onSuccess: () => setDone("saved"),
      onError: () => setError(t("self.userSettings.photo.failed")),
      onSettled: () => setPreview(null),
    });
  };

  const fromLibrary = async () => {
    const result = await ImagePicker.launchImageLibraryAsync(PICK);
    if (!result.canceled && result.assets.length > 0) {
      upload(result.assets[0].uri);
    }
  };

  const fromCamera = async () => {
    const perm = await ImagePicker.requestCameraPermissionsAsync();
    if (!perm.granted) {
      setError(t("self.userSettings.photo.cameraDenied"));
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
      upload(result.assets[0].uri);
    }
  };

  const remove = () => {
    setError(null);
    setDone(null);
    removeAvatar.mutate(undefined, {
      onSuccess: () => setDone("removed"),
      onError: () => setError(t("self.userSettings.photo.removeFailed")),
    });
  };

  const choose = (fn: () => void) => {
    setSheetOpen(false);
    afterSheetClose(fn);
  };

  const status = setAvatar.isPending
    ? t("self.userSettings.photo.saving")
    : removeAvatar.isPending
      ? t("self.userSettings.photo.removing")
      : done === "saved"
        ? t("self.userSettings.photo.saved")
        : done === "removed"
          ? t("self.userSettings.photo.removed")
          : null;

  return (
    <View style={{ alignItems: "center", gap: space.lg }}>
      <Pressable
        testID="btn-avatar-edit"
        accessibilityRole="button"
        accessibilityLabel={hasPhoto ? t("self.userSettings.photo.change") : t("self.userSettings.photo.add")}
        accessibilityState={{ disabled: busy, busy }}
        disabled={busy}
        onPress={() => setSheetOpen(true)}
        style={({ pressed }) => ({ opacity: pressed && !busy ? 0.85 : 1 })}
      >
        {preview ? (
          <View
            testID="avatar-self-settings-preview"
            style={{
              width: SIZE,
              height: SIZE,
              borderRadius: SIZE / 2,
              overflow: "hidden",
              backgroundColor: semantic.high,
            }}
          >
            <Image source={{ uri: preview }} style={{ width: SIZE, height: SIZE }} contentFit="cover" transition={0} />
            <View
              style={{
                position: "absolute",
                top: 0,
                bottom: 0,
                start: 0,
                end: 0,
                alignItems: "center",
                justifyContent: "center",
                backgroundColor: semantic.backdrop,
              }}
            >
              <ActivityIndicator color={semantic.text} />
            </View>
          </View>
        ) : (
          <UserAvatar
            testID="avatar-self-settings"
            userId={userId}
            avatarKey={avatarKey}
            label={label}
            size={SIZE}
            variant="self"
          />
        )}
      </Pressable>
      <Button
        testID="btn-avatar-change"
        variant="subtle"
        disabled={busy}
        loading={busy}
        icon={<Icon.camera size={18} color={semantic.text} />}
        onPress={() => setSheetOpen(true)}
      >
        {hasPhoto ? t("self.userSettings.photo.change") : t("self.userSettings.photo.add")}
      </Button>
      <Text style={[ty.meta, { textAlign: "center" }]}>{t("self.userSettings.photo.hint")}</Text>
      {status ? (
        <View style={{ flexDirection: "row", alignItems: "center", gap: space.xs }}>
          {done && !busy ? <Icon.check size={16} color={semantic.accent} /> : null}
          <Text
            testID={done && !busy ? `text-avatar-${done}` : "text-avatar-busy"}
            accessibilityRole={done && !busy ? "alert" : undefined}
            style={[ty.secondary, { color: semantic.text }]}
          >
            {status}
          </Text>
        </View>
      ) : null}
      {error ? <ErrorText>{error}</ErrorText> : null}

      {sheetOpen ? (
        <SheetOverlay
          title={t("self.userSettings.photo.heading")}
          onClose={() => setSheetOpen(false)}
          testID="sheet-avatar"
        >
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
          {hasPhoto ? (
            <Group surface="high" style={{ marginTop: space.lg }}>
              <ListRow
                testID="btn-avatar-remove"
                glyph={<Icon.trash size={22} color={semantic.danger} />}
                name={t("self.userSettings.photo.remove")}
                onPress={() => choose(remove)}
              />
            </Group>
          ) : null}
        </SheetOverlay>
      ) : null}
    </View>
  );
}
