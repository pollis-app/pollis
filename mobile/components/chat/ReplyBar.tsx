import { View, Text, Pressable } from "react-native";
import { Trans, useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { fonts, layout, semantic, type as ty } from "../../theme/tokens";

/** Sits on top of the composer while a reply is written (#1270); tapping the quote scrolls to the original. */
export function ReplyBar({
  name,
  snippet,
  onPressQuote,
  onDismiss,
}: {
  name: string;
  snippet: string;
  onPressQuote?: () => void;
  onDismiss: () => void;
}) {
  const { t } = useTranslation("chat");
  return (
    <View
      testID="bar-reply"
      style={{
        flexDirection: "row",
        alignItems: "center",
        gap: 8,
        paddingStart: 16,
        paddingEnd: 8,
        paddingVertical: 4,
        borderTopWidth: 1,
        borderTopColor: semantic.hair,
        backgroundColor: semantic.panel,
      }}
    >
      <Icon.reply size={16} color={semantic.dim} />
      <Pressable
        onPress={onPressQuote}
        disabled={!onPressQuote}
        accessibilityRole="button"
        accessibilityLabel={`${t("mobile:chat.replyingToLabel", { name })}, ${snippet}`}
        accessibilityHint={onPressQuote ? t("replyBar.scrollToMessage") : undefined}
        style={{ flex: 1, minWidth: 0, minHeight: layout.touchMin, justifyContent: "center" }}
      >
        <Text numberOfLines={1} style={[ty.meta, { color: semantic.dim }]}>
          <Trans
            t={t}
            i18nKey="mobile:chat.replyingTo"
            values={{ name }}
            components={{
              name: <Text style={{ fontFamily: fonts.semibold, color: semantic.accent }} />,
            }}
          />
        </Text>
        <Text numberOfLines={1} style={[ty.secondary, { color: semantic.text }]}>
          {snippet}
        </Text>
      </Pressable>
      <Pressable
        testID="btn-reply-dismiss"
        onPress={onDismiss}
        accessibilityRole="button"
        accessibilityLabel={t("replyBar.dismiss")}
        style={({ pressed }) => ({
          width: layout.touchMin,
          height: layout.touchMin,
          borderRadius: layout.touchMin / 2,
          alignItems: "center",
          justifyContent: "center",
          backgroundColor: pressed ? semantic.high : "transparent",
        })}
      >
        <Icon.close size={20} color={semantic.dim} />
      </Pressable>
    </View>
  );
}
