import { Pressable, Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Avatar } from "../ui";
import { semantic, type as ty, fonts, space } from "../../theme/tokens";
import { activeLocale } from "../../i18n";
import type { SearchMessageResult } from "../../hooks/queries/useSearch";
import { HighlightedSnippet } from "./HighlightedSnippet";

/**
 * One message hit: who and when, where it lives (channel · group or DM, plus
 * "in a thread" / "attachment"), and the highlighted snippet. The same three
 * lines desktop's SearchView shows, in the mobile row idiom. Sits inside a
 * <Group>, which draws the separators; one control, one spoken label.
 */
export function SearchResultRow({
  result,
  isSelf = false,
  conversationLabel,
  onPress,
}: {
  result: SearchMessageResult;
  // The reader's own message: the amber "self" avatar, as in the chat.
  isSelf?: boolean;
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

  const snippet = result.snippet.text
    ? result.snippet
    : { text: result.content, highlights: [] };

  return (
    <Pressable
      testID={`row-message-${result.message_id}`}
      accessibilityRole="button"
      accessibilityLabel={[sender, snippet.text, where, when]
        .filter(Boolean)
        .join(", ")}
      onPress={onPress}
      style={({ pressed }) => ({
        flexDirection: "row",
        gap: space.lg,
        minHeight: 64,
        paddingVertical: space.lg,
        paddingStart: space.xxl,
        paddingEnd: space.xl,
        backgroundColor: pressed ? semantic.high : "transparent",
      })}
    >
      {/* The chat timeline's avatar: 40pt, amber for the reader's own. */}
      <Avatar label={sender} size="md" variant={isSelf ? "self" : "default"} />
      <View style={{ flex: 1, minWidth: 0, gap: 2 }}>
        <View
          style={{
            flexDirection: "row",
            alignItems: "baseline",
            gap: space.sm,
          }}
        >
          <Text
            numberOfLines={1}
            style={{
              flex: 1,
              fontFamily: fonts.semibold,
              fontSize: 15,
              color: semantic.text,
            }}
          >
            {sender}
          </Text>
          <Text style={ty.meta}>{when}</Text>
        </View>
        {where ? (
          <Text numberOfLines={1} style={[ty.meta, { marginBottom: 4 }]}>
            {where}
          </Text>
        ) : null}
        <HighlightedSnippet snippet={snippet} />
      </View>
    </Pressable>
  );
}
