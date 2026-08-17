import { useState } from "react";
import { View, Text, FlatList, Pressable } from "react-native";
import { useRouter } from "expo-router";
import { Screen, Crumb, Ctx, Chip } from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import {
  useSavedMessages,
  useUnsaveMessage,
  useResolvePermalink,
  type SavedMessage,
} from "../../hooks/queries";
import { useConversationRoute } from "../../hooks/useConversationRoute";
import { EmojiText } from "../../components/emoji/EmojiText";
import { PERMALINK_MISS_COPY } from "../../lib/permalinks";

function timeAgo(iso: string): string {
  const then = new Date(iso).getTime();
  if (!Number.isFinite(then)) {
    return "";
  }
  const s = Math.max(0, Math.floor((Date.now() - then) / 1000));
  if (s < 60) {
    return "now";
  }
  if (s < 3600) {
    return `${Math.floor(s / 60)}m`;
  }
  if (s < 86400) {
    return `${Math.floor(s / 3600)}h`;
  }
  return `${Math.floor(s / 86400)}d`;
}

/**
 * Saved messages (#887). Rows open the message's conversation via
 * `resolve_message_permalink`; a miss shows desktop's exact non-oracle copy
 * and navigates nowhere.
 */
export default function SavedScreen() {
  const router = useRouter();
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
    router.push({
      pathname: "/chat/[id]",
      params: {
        id: item.conversation_id,
        kind: route.kind,
        ...(route.name ? { name: route.name } : {}),
      },
    });
  };

  const renderRow = ({ item }: { item: SavedMessage }) => (
    <Pressable
      testID={`row-saved-${item.message_id}`}
      accessibilityRole="button"
      accessibilityLabel="Open saved message"
      onPress={() => void openItem(item)}
      style={{
        flexDirection: "row",
        alignItems: "center",
        gap: 12,
        paddingHorizontal: 18,
        paddingVertical: 10,
        borderBottomWidth: 1,
        borderBottomColor: semantic.hairSoft,
      }}
    >
      <Icon.bookmark color={semantic.accent} />
      <View style={{ flex: 1, minWidth: 0 }}>
        {item.available ? (
          <EmojiText
            text={item.content || "(no text)"}
            numberOfLines={2}
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 14,
              lineHeight: 20,
              color: semantic.ink,
            }}
          />
        ) : (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              fontStyle: "italic",
              color: semantic.mute,
            }}
            testID={`saved-unavailable-${item.message_id}`}
          >
            {PERMALINK_MISS_COPY}
          </Text>
        )}
        <Text
          style={{
            fontFamily: ty.body.fontFamily,
            fontSize: 11,
            color: semantic.mute,
            marginTop: 2,
          }}
        >
          saved {timeAgo(item.saved_at)} ago
        </Text>
      </View>
      <Chip
        testID={`btn-unsave-${item.message_id}`}
        accessibilityLabel="Unsave"
        onPress={() => unsave.mutate(item.message_id)}
      >
        Unsave
      </Chip>
    </Pressable>
  );

  return (
    <Screen testID="screen-saved">
      <Crumb segs={[{ label: "SELF" }, { label: "Saved", leaf: true }]} />
      {unresolved ? (
        <Text
          testID="saved-unresolved-notice"
          style={{
            fontFamily: ty.body.fontFamily,
            fontSize: 13,
            color: semantic.danger,
            paddingHorizontal: 18,
            paddingTop: 10,
          }}
        >
          {PERMALINK_MISS_COPY}
        </Text>
      ) : null}
      <FlatList
        testID="list-saved"
        data={saved}
        keyExtractor={(item) => item.message_id}
        renderItem={renderRow}
        style={{ flex: 1 }}
        ListEmptyComponent={
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingTop: 12,
            }}
          >
            {isLoading
              ? "Loading…"
              : "No saved messages. Use the long-press action on a message to save it."}
          </Text>
        }
      />
      <Ctx cr="SELF" name="Saved" />
    </Screen>
  );
}
