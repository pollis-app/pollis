import { Pressable, Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Avatar } from "../ui";
import { semantic, type as ty } from "../../theme/tokens";
import { activeLocale } from "../../i18n";
import type { SearchMessageResult } from "../../hooks/queries/useSearch";
import { HighlightedSnippet } from "./HighlightedSnippet";

/**
 * One message hit: who and when, where it lives (channel · group or DM, plus
 * "in a thread" / "attachment"), and the highlighted snippet. The same three
 * lines desktop's SearchView shows, in the mobile row idiom.
 */
export function SearchResultRow({
  result,
  conversationLabel,
  onPress,
}: {
  result: SearchMessageResult;
  conversationLabel: string | null;
  onPress: () => void;
}) {
  const { t } = useTranslation("search");
  const sender = result.sender_username ?? result.sender_id;
  const where = [
    conversationLabel,
    result.thread_id ? t("view.inThread") : null,
    result.has_attachment ? t("view.hasAttachment") : null,
  ]
    .filter(Boolean)
    .join(" · ");
  const when = new Date(result.sent_at).toLocaleString(activeLocale(), {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });

  return (
    <Pressable
      testID={`row-message-${result.message_id}`}
      accessibilityRole="button"
      accessibilityLabel={`${sender}: ${result.snippet.text || result.content}`}
      onPress={onPress}
      style={({ pressed }) => ({
        flexDirection: "row",
        gap: 12,
        paddingHorizontal: 18,
        paddingVertical: 12,
        borderBottomWidth: 1,
        borderBottomColor: semantic.hairSoft,
        backgroundColor: pressed ? semantic.fieldBg : "transparent",
      })}
    >
      <Avatar label={sender.slice(0, 2)} size="sm" />
      <View style={{ flex: 1, gap: 3 }}>
        <View style={{ flexDirection: "row", alignItems: "baseline", gap: 8 }}>
          <Text
            numberOfLines={1}
            style={{ flex: 1, fontFamily: ty.rowN.fontFamily, fontSize: 13, color: semantic.accent }}
          >
            {sender}
          </Text>
          <Text style={[ty.label, { letterSpacing: 0.6 }]}>{when}</Text>
        </View>
        {where ? (
          <Text numberOfLines={1} style={[ty.rowSub, { marginBottom: 6 }]}>
            {where}
          </Text>
        ) : null}
        <HighlightedSnippet snippet={result.snippet.text ? result.snippet : { text: result.content, highlights: [] }} />
      </View>
    </Pressable>
  );
}
