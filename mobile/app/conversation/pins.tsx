// Pinned messages (#1315): the conversation's pins, newest first; a row jumps back to the message.

import { useState } from "react";
import { View, FlatList, Pressable } from "react-native";
import { Text } from "../../components/Text";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { useNav, useRouteParams } from "../../components/pane/paneContext";
import { Screen, Header, Chip, EmptyState } from "../../components/ui";
import { ErrorText } from "../../components/self/SettingsField";
import { Icon } from "../../components/icons";
import { MessageBodyInline } from "../../components/chat/MessageBody";
import { MessageLinkPreview } from "../../components/chat/MessageLinkPreview";
import { dayLabel, timeLabel } from "../../components/chat/dates";
import { fonts, semantic, type as ty, space, groupedRowCorners } from "../../theme/tokens";
import {
  usePinnedMessages,
  useTogglePinnedMessage,
  useResolvePermalink,
  useDMRequests,
  type PinnedMessage,
} from "../../hooks/queries";
import { useMessageBodyContext } from "../../hooks/useMessageBodyContext";
import { authorName } from "../../lib/authorName";
import { aliasName } from "../../lib/groupAlias";
import { useGroupAliases } from "../../hooks/queries/useGroupAliases";
import { pinLinkPreview, pinPreview } from "../../lib/pins";
import { linkA11yActions, runLinkA11yAction } from "../../lib/openLink";
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
  // A channel's pins name authors as its group shows them (#2003); a DM's, by main profile.
  const kind = params.kind === "channel" || params.kind === "dm" ? params.kind : null;
  const groupId = kind === "channel" ? appStore.selectedGroupId ?? null : null;
  const aliases = useGroupAliases(groupId);
  // A pin's body draws as the timeline draws it (#2127): mentions, links, the sender's preview.
  const body = useMessageBodyContext(kind, conversationId, groupId);
  // No sender previews in an unaccepted DM request, as in its timeline (#1987).
  const { data: dmRequests } = useDMRequests();
  const isRequest =
    kind === "dm" && !!conversationId && (dmRequests ?? []).some((d) => d.id === conversationId);
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
    // Only ever opened from its conversation, right under this screen.
    nav.back();
  };

  const renderRow = ({ item, index }: { item: PinnedMessage; index: number }) => {
    const author =
      aliasName(aliases, item.sender_id, authorName(item.sender_id, item.sender_username ?? undefined, currentUser, item.sender_display_name)) ??
      t("chat:list.unknownAuthor");
    const sent = new Date(item.sent_at).getTime();
    const when = Number.isFinite(sent) ? `${dayLabel(sent)} · ${timeLabel(sent)}` : "";
    const preview = pinPreview(item.content);
    const linkPreview = isRequest ? null : pinLinkPreview(item);
    const spoken = preview.text.trim() || preview.filenames.join(", ");
    // The row is one accessible element, so its links are offered as actions, as a timeline row's are.
    const linkActions = linkA11yActions(preview.text, (host) => t("mobile:link.openLabel", { host }));
    const first = index === 0;
    return (
      <View
        style={{
          marginHorizontal: space.xxl,
          backgroundColor: semantic.raised,
          ...groupedRowCorners(index, pins.length - 1),
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
          accessibilityActions={linkActions.map(({ name, label }) => ({ name, label }))}
          onAccessibilityAction={(e) => {
            runLinkA11yAction(e.nativeEvent.actionName, linkActions);
          }}
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
            <Text testID={`text-pin-${item.message_id}`} numberOfLines={3} style={[ty.body, { color: semantic.text }]}>
              <MessageBodyInline
                text={preview.text}
                mentionNames={body.mentionNames}
                selfName={body.selfName}
                mentionLabels={body.mentionLabels}
                botMentions={body.botMentions}
              />
            </Text>
          ) : null}
          {preview.text && linkPreview ? (
            <MessageLinkPreview text={preview.text} preview={linkPreview} />
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
          isLoading ? (
            <Text testID="pins-loading" style={[ty.secondary, { paddingHorizontal: space.xxl }]}>
              {t("common:states.loading")}
            </Text>
          ) : (
            <EmptyState testID={isError ? "pins-unavailable" : "pins-empty"} icon="pin">
              {isError ? t("pins.unavailable") : t("pins.empty")}
            </EmptyState>
          )
        }
      />
    </Screen>
  );
}

export default observer(PinnedMessagesScreen);
