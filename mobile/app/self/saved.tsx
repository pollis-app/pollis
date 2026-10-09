import { useState } from "react";
import { View, Text, FlatList, Pressable } from "react-native";
import { useOpenConversation } from "../../hooks/useOpenConversation";
import { useTranslation } from "react-i18next";
import { Screen, Header, Chip } from "../../components/ui";
import { ErrorText } from "../../components/self/SettingsField";
import { Icon } from "../../components/icons";
import { semantic, type as ty, r, space } from "../../theme/tokens";
import i18n from "../../i18n";
import {
  useSavedMessages,
  useUnsaveMessage,
  useResolvePermalink,
  type SavedMessage,
} from "../../hooks/queries";
import { useConversationRoute } from "../../hooks/useConversationRoute";
import { EmojiText } from "../../components/emoji/EmojiText";
import { permalinkMissCopy } from "../../lib/permalinks";

function timeAgo(iso: string): string {
  const then = new Date(iso).getTime();
  if (!Number.isFinite(then)) {
    return "";
  }
  const s = Math.max(0, Math.floor((Date.now() - then) / 1000));
  if (s < 60) {
    return i18n.t("common:timeAgo.seconds", { count: s });
  }
  if (s < 3600) {
    return i18n.t("common:timeAgo.minutes", { count: Math.floor(s / 60) });
  }
  if (s < 86400) {
    return i18n.t("common:timeAgo.hours", { count: Math.floor(s / 3600) });
  }
  return i18n.t("common:timeAgo.days", { count: Math.floor(s / 86400) });
}

/**
 * Saved messages (#887). Rows open the message's conversation via
 * `resolve_message_permalink`; a miss shows desktop's exact non-oracle copy
 * and navigates nowhere.
 */
export default function SavedScreen() {
  const { t } = useTranslation("saved");
  // Phones push the conversation; iPad opens it in its tab's two-pane.
  const { openConversation } = useOpenConversation();
  const { data: saved = [], isLoading } = useSavedMessages();
  const unsave = useUnsaveMessage();
  const resolvePermalink = useResolvePermalink();
  const routeFor = useConversationRoute();
  const [unresolved, setUnresolved] = useState(false);

  const openItem = async (item: SavedMessage) => {
    setUnresolved(false);
    const target = await resolvePermalink(
      item.conversation_id,
      item.message_id,
    );
    if (!target.found) {
      setUnresolved(true);
      return;
    }
    const route = routeFor(item.conversation_id);
    openConversation({
      id: item.conversation_id,
      kind: route.kind,
      name: route.name ?? undefined,
    });
  };

  const renderRow = ({ item, index }: { item: SavedMessage; index: number }) => {
    const savedAgo = t("mobile:self.saved.savedAgo", { time: timeAgo(item.saved_at) });
    const preview = item.available ? item.content || t("row.noText") : t("row.unavailable");
    const first = index === 0;
    const last = index === saved.length - 1;
    return (
      <View
        style={{
          marginHorizontal: space.xxl,
          backgroundColor: semantic.raised,
          borderTopLeftRadius: first ? r.lg : 0,
          borderTopRightRadius: first ? r.lg : 0,
          borderBottomLeftRadius: last ? r.lg : 0,
          borderBottomRightRadius: last ? r.lg : 0,
          borderTopWidth: first ? 0 : 1,
          borderTopColor: semantic.hairSoft,
          flexDirection: "row",
          alignItems: "center",
          paddingEnd: space.lg,
        }}
      >
        <Pressable
          testID={`row-saved-${item.message_id}`}
          accessibilityRole="button"
          accessibilityLabel={`${preview}, ${savedAgo}`}
          accessibilityHint={t("mobile:self.saved.openA11y")}
          onPress={() => void openItem(item)}
          style={({ pressed }) => ({
            flex: 1,
            minWidth: 0,
            flexDirection: "row",
            alignItems: "center",
            gap: space.lg,
            minHeight: 64,
            paddingVertical: space.md,
            paddingStart: space.xxl,
            opacity: pressed ? 0.7 : 1,
          })}
        >
          <Icon.bookmark size={20} color={semantic.dim} />
          <View style={{ flex: 1, minWidth: 0, gap: 2 }}>
            {item.available ? (
              <EmojiText
                text={item.content || t("row.noText")}
                numberOfLines={2}
                style={ty.body}
              />
            ) : (
              <Text
                style={[ty.body, { fontStyle: "italic", color: semantic.dim }]}
                testID={`saved-unavailable-${item.message_id}`}
              >
                {t("row.unavailable")}
              </Text>
            )}
            <Text style={ty.meta}>{savedAgo}</Text>
          </View>
        </Pressable>
        <Chip
          variant="outline"
          testID={`btn-unsave-${item.message_id}`}
          accessibilityLabel={t("mobile:self.saved.unsave")}
          onPress={() => unsave.mutate(item.message_id)}
        >
          {t("mobile:self.saved.unsave")}
        </Chip>
      </View>
    );
  };

  return (
    <Screen testID="screen-saved">
      <Header title={t("mobile:self.hub.savedMessages")} backTo={t("mobile:self.title")} />
      {unresolved ? (
        <View style={{ paddingHorizontal: space.xxl, paddingTop: space.lg }}>
          <ErrorText testID="saved-unresolved-notice">{permalinkMissCopy()}</ErrorText>
        </View>
      ) : null}
      <FlatList
        testID="list-saved"
        data={saved}
        keyExtractor={(item) => item.message_id}
        renderItem={renderRow}
        style={{ flex: 1 }}
        contentContainerStyle={{ paddingTop: space.xxl, paddingBottom: space.xxxl }}
        ListEmptyComponent={
          <Text style={[ty.secondary, { paddingHorizontal: space.xxl }]}>
            {isLoading
              ? t("common:states.loading")
              : t("mobile:self.saved.empty")}
          </Text>
        }
      />
    </Screen>
  );
}
