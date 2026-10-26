// Pinned messages (#99; mobile #1315) — the conversation's pins, newest
// first. Mobile counterpart of desktop's right-panel PinnedPanel: the chat
// header's pin button pushes it with { id, kind, name } (on iPad, inside the
// detail pane).
//
// Same semantics as desktop, because the rules live in pollis-core and the
// DS: every member sees the same list, any member may unpin, and a row shows
// the message as it was when it was pinned (a sealed snapshot — a later edit
// or delete does not change it). A device still waiting for the
// conversation's pin key gets desktop's "not available yet" copy.
//
// Tapping a row goes back to the conversation scrolled to the message: the
// same local `resolve_message_permalink` the permalink route and Saved use
// (a miss shows their non-oracle copy and goes nowhere), then the jump rides
// `messageJumpStore`.

import { useState } from "react";
import { View, Text, FlatList, Pressable } from "react-native";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { useNav, useRouteParams } from "../../components/pane/paneContext";
import { Screen, Header, Chip } from "../../components/ui";
import { ErrorText } from "../../components/self/SettingsField";
import { Icon } from "../../components/icons";
import { EmojiText } from "../../components/emoji/EmojiText";
import { dayLabel, timeLabel } from "../../components/chat/dates";
import { fonts, semantic, type as ty, r, space } from "../../theme/tokens";
import {
  usePinnedMessages,
  useTogglePinnedMessage,
  useResolvePermalink,
  type PinnedMessage,
} from "../../hooks/queries";
import { authorName } from "../../lib/authorName";
import { pinPreview } from "../../lib/pins";
import { permalinkMissCopy } from "../../lib/permalinks";
import { messageJumpStore } from "../../stores/messageJumpStore";
import { appStore } from "../../stores/appStore";
import { useThemeRerender } from "../../components/theme";

function PinnedMessagesScreen() {
  useThemeRerender();
  const nav = useNav();
  const { t } = useTranslation("nav");
  const params = useRouteParams<{ id?: string; kind?: string; name?: string }>();
  const conversationId = params.id ?? null;
  const name = typeof params.name === "string" ? params.name : undefined;
  const currentUser = appStore.currentUser;
  const { data: pins = [], isLoading, isError } = usePinnedMessages(conversationId, {
    fresh: true,
  });
  const togglePin = useTogglePinnedMessage();
  const resolvePermalink = useResolvePermalink();
  const [unresolved, setUnresolved] = useState(false);

  const openPin = async (pin: PinnedMessage) => {
    setUnresolved(false);
    const target = await resolvePermalink(pin.conversation_id, pin.message_id);
    if (!target.found) {
      setUnresolved(true);
      return;
    }
    messageJumpStore.request(pin.conversation_id, pin.message_id);
    // The list is only ever opened from its conversation, which is the
    // screen (or pane page) right under this one.
    nav.back();
  };

  const renderRow = ({ item, index }: { item: PinnedMessage; index: number }) => {
    const author =
      authorName(item.sender_id, item.sender_username ?? undefined, currentUser) ??
      t("chat:list.unknownAuthor");
    const sent = new Date(item.sent_at).getTime();
    const when = Number.isFinite(sent) ? `${dayLabel(sent)} · ${timeLabel(sent)}` : "";
    const preview = pinPreview(item.content);
    const spoken = preview.text.trim() || preview.filenames.join(", ");
    const first = index === 0;
    const last = index === pins.length - 1;
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
          testID={`row-pin-${item.message_id}`}
          accessibilityRole="button"
          accessibilityLabel={t("mobile:pins.rowLabel", { author, when, text: spoken })}
          accessibilityHint={t("mobile:pins.openA11y")}
          onPress={() => void openPin(item)}
          style={({ pressed }) => ({
            flex: 1,
            minWidth: 0,
            gap: 2,
            minHeight: 64,
            paddingVertical: space.md,
            paddingStart: space.xxl,
            paddingEnd: space.md,
            opacity: pressed ? 0.7 : 1,
          })}
        >
          <View style={{ flexDirection: "row", alignItems: "baseline", gap: space.sm }}>
            <Text
              numberOfLines={1}
              style={[ty.secondary, { flexShrink: 1, fontFamily: fonts.semibold, color: semantic.text }]}
            >
              {author}
            </Text>
            <Text style={ty.meta}>{when}</Text>
          </View>
          {preview.text ? (
            <EmojiText text={preview.text} numberOfLines={3} style={ty.body} />
          ) : null}
          {preview.filenames.map((filename, fi) => (
            <View
              key={`${filename}-${fi}`}
              style={{ flexDirection: "row", alignItems: "center", gap: space.xs }}
            >
              <Icon.attach size={14} color={semantic.dim} />
              <Text numberOfLines={1} style={[ty.secondary, { flexShrink: 1, color: semantic.dim }]}>
                {filename}
              </Text>
            </View>
          ))}
        </Pressable>
        <Chip
          variant="outline"
          testID={`btn-unpin-${item.message_id}`}
          disabled={togglePin.isPending}
          accessibilityLabel={t("mobile:pins.unpinA11y", { author })}
          onPress={() =>
            togglePin.mutate({
              conversationId: item.conversation_id,
              messageId: item.message_id,
              isPinned: true,
            })
          }
        >
          {t("pins.unpin")}
        </Chip>
      </View>
    );
  };

  const emptyCopy = isLoading
    ? t("common:states.loading")
    : isError
      ? t("pins.unavailable")
      : t("pins.empty");

  return (
    <Screen testID="screen-pins">
      <Header
        title={t("pins.heading")}
        subtitle={name}
        backTo={name}
        onBack={nav.onBack}
      />
      {unresolved ? (
        <View style={{ paddingHorizontal: space.xxl, paddingTop: space.lg }}>
          <ErrorText testID="pins-unresolved-notice">{permalinkMissCopy()}</ErrorText>
        </View>
      ) : null}
      <FlatList
        testID="list-pins"
        data={isError ? [] : pins}
        keyExtractor={(item) => item.id}
        renderItem={renderRow}
        style={{ flex: 1 }}
        contentContainerStyle={{ paddingTop: space.xxl, paddingBottom: space.xxxl }}
        ListEmptyComponent={
          <Text
            testID={isError ? "pins-unavailable" : isLoading ? "pins-loading" : "pins-empty"}
            style={[ty.secondary, { paddingHorizontal: space.xxl }]}
          >
            {emptyCopy}
          </Text>
        }
      />
    </Screen>
  );
}

export default observer(PinnedMessagesScreen);
