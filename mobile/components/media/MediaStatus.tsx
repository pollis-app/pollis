// A viewer page's decrypting / failed placeholder.

import { ActivityIndicator, View } from "react-native";
import { Text } from "../Text";
import { useTranslation } from "react-i18next";
import { semantic, type as ty, space } from "../../theme/tokens";

export function MediaStatus({
  state,
}: {
  // `removed`: the operator removed the file; `update`: it needs a newer app (#2109).
  state: "idle" | "loading" | "error" | "removed" | "update";
}) {
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
      {state === "removed" || state === "update" ? (
        <Text
          testID={state === "removed" ? "text-media-removed" : "text-media-update"}
          accessibilityLiveRegion="polite"
          style={[ty.secondary, { color: semantic.dim, textAlign: "center" }]}
        >
          {state === "removed" ? t("chat:attachment.removed") : t("chat:attachment.updateToOpen")}
        </Text>
      ) : null}
    </View>
  );
}
