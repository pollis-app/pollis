// The placeholder a media viewer page shows while its file is being
// decrypted, or when it could not be.

import { ActivityIndicator, Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { semantic, type as ty, space } from "../../theme/tokens";

export function MediaStatus({ state }: { state: "idle" | "loading" | "error" }) {
  const { t } = useTranslation("mobile");
  return (
    <View
      style={{
        flex: 1,
        alignItems: "center",
        justifyContent: "center",
        padding: space.xxl,
      }}
    >
      {state === "loading" ? (
        <ActivityIndicator
          color={semantic.dim}
          accessibilityLabel={t("common:states.loading")}
        />
      ) : null}
      {state === "error" ? (
        <Text
          testID="text-media-unavailable"
          accessibilityLiveRegion="polite"
          style={[ty.secondary, { color: semantic.dim, textAlign: "center" }]}
        >
          {t("media.unavailable")}
        </Text>
      ) : null}
    </View>
  );
}
