// A covered sensitive attachment (#1568): its blurhash under a crossed-out eye, purely visual. `SensitiveHideButton` covers a revealed one again.

import { View } from "react-native";
import { Image } from "expo-image";
import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { useThemeRerender } from "../theme";
import { AttachmentToolbar, TOOLBAR_GLYPH } from "./AttachmentToolbar";
import { semantic } from "../../theme/tokens";

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
          backgroundColor: semantic.raised,
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
    <AttachmentToolbar
      actions={[
        {
          key: "hide",
          testID: testID ?? "btn-sensitive-hide",
          accessibilityLabel: t("attachment.sensitiveHide", { filename }),
          icon: (color) => <Icon.eye size={TOOLBAR_GLYPH} color={color} />,
          onPress: onHide,
        },
      ]}
    />
  );
}
