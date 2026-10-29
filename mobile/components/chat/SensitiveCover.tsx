// What a sensitive attachment (#1568) shows while covered: its blurhash with
// one crossed-out eye centred on it — the same in a message row, the viewer
// and the Vault grid, and desktop's `SensitiveCover`. With no blurhash (a
// video whose poster could not be read) it is a plain surface with the same
// eye, never the media. Purely visual: it fills its parent, which owns the
// reveal.
//
// `SensitiveHideButton` is the matching top-right eye on a revealed one: one
// tap covers it again. It is an open eye — what is true now, the media is
// showing — as the composer's toggle shows.

import { Pressable, View } from "react-native";
import { Image } from "expo-image";
import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { useThemeRerender } from "../theme";
import { r, semantic } from "../../theme/tokens";

const FILL = { position: "absolute", top: 0, bottom: 0, start: 0, end: 0 } as const;

export function SensitiveCover({ blurhash, testID }: { blurhash?: string; testID?: string }) {
  useThemeRerender();
  return (
    <View
      testID={testID}
      pointerEvents="none"
      style={[FILL, { backgroundColor: semantic.high, alignItems: "center", justifyContent: "center" }]}
    >
      {blurhash ? (
        <Image placeholder={{ blurhash }} placeholderContentFit="cover" style={FILL} />
      ) : null}
      <View
        style={{
          width: 44,
          height: 44,
          borderRadius: 22,
          alignItems: "center",
          justifyContent: "center",
          backgroundColor: semantic.bg,
        }}
      >
        <Icon.eyeOff size={20} color={semantic.text} />
      </View>
    </View>
  );
}

export function SensitiveHideButton({
  filename,
  onHide,
  testID,
}: {
  filename: string;
  onHide: () => void;
  testID?: string;
}) {
  useThemeRerender();
  const { t } = useTranslation("chat");
  return (
    <Pressable
      testID={testID}
      onPress={onHide}
      accessibilityRole="button"
      accessibilityLabel={t("attachment.sensitiveHide", { filename })}
      hitSlop={8}
      style={{
        position: "absolute",
        top: 6,
        end: 6,
        width: 32,
        height: 32,
        borderRadius: r.sm,
        alignItems: "center",
        justifyContent: "center",
        backgroundColor: semantic.bg,
      }}
    >
      <Icon.eye size={16} color={semantic.text} />
    </Pressable>
  );
}
