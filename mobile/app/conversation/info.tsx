// Conversation info — member roster + shared media for a channel or DM.
// Mobile counterpart of desktop's right-hand context panel (#826): Discord's
// member list over Messenger's shared-media grid. The chat screen's members
// button and channel menu navigate here with { id, kind, groupId?, name? }.

import { useMemo } from "react";
import { View, Text, Pressable, useWindowDimensions } from "react-native";
import { useNav, useRouteParams } from "../../components/pane/paneContext";
import { useTranslation } from "react-i18next";
import {
  Screen,
  Header,
  Body,
  SectionTitle,
  ListRow,
  Avatar,
  Group,
} from "../../components/ui";
import { ExportArchive } from "../../components/ExportArchive";
import { fonts, semantic, space, type as ty } from "../../theme/tokens";
import {
  useDMChannel,
  useGroupMembers,
  useMessages,
  flattenPages,
  sortMembersByRole,
  type ConversationKind,
} from "../../hooks/queries";
import { MediaImage } from "../../components/Media";
import { useOpenMediaViewer } from "../../hooks/useOpenMediaViewer";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

// Cap on grid tiles, matching desktop's MembersPanel (#826): each tile
// resolves its own bytes through the media transport, so an unbounded grid
// on a media-heavy conversation would fan out into hundreds of decrypt
// round-trips the moment the screen opens.
const MEDIA_LIMIT = 30;

function ConversationInfo() {
  const router = useNav();
  const { t } = useTranslation("mobile");
  const params = useRouteParams<{
    id?: string;
    kind?: string;
    groupId?: string;
    name?: string;
  }>();
  const conversationId = params.id ?? null;
  const kind: ConversationKind | null =
    params.kind === "channel" || params.kind === "dm" ? params.kind : null;
  // For a channel the roster lives on the group; fall back to the selected
  // group when the opener didn't pass it (the chat screen keeps it selected).
  const groupId =
    kind === "channel"
      ? (params.groupId ?? appStore.selectedGroupId)
      : null;
  const currentUser = appStore.currentUser;
  const { width } = useWindowDimensions();
  const openMedia = useOpenMediaViewer(conversationId, kind);

  const { data: groupMembers = [] } = useGroupMembers(groupId);
  const dmChannel = useDMChannel(kind === "dm" ? conversationId : null);

  // Role-then-alphabetical, mirroring desktop's members panel intent —
  // desktop orders online-first, but mobile has no presence source yet, so
  // presence ordering awaits one.
  const roster = useMemo(() => {
    if (kind === "channel") {
      return sortMembersByRole(groupMembers).map((m) => ({
        userId: m.user_id,
        handle: m.username ?? m.user_id.slice(0, 8),
        role: m.role,
      }));
    }
    const members = dmChannel.data?.members ?? [];
    return [...members]
      .sort((a, b) =>
        (a.username ?? a.user_id).localeCompare(b.username ?? b.user_id),
      )
      .map((m) => ({
        userId: m.user_id,
        handle: m.username ?? m.user_id.slice(0, 8),
        role: null as string | null,
      }));
  }, [kind, groupMembers, dmChannel.data]);

  // Shared media comes from the same message cache the chat screen fills —
  // the most recent image attachments this device holds, newest first,
  // capped at MEDIA_LIMIT.
  const { data: messagesData } = useMessages(conversationId, kind);
  const attachments = useMemo(() => {
    // `flattenPages` yields newest-first, so a forward walk leads the grid
    // with the most recent media.
    const messages = flattenPages(messagesData);
    const seen = new Set<string>();
    const out: NonNullable<(typeof messages)[number]["attachments"]> = [];
    for (let i = 0; i < messages.length && out.length < MEDIA_LIMIT; i++) {
      const message = messages[i];
      if (message.deleted_at) {
        continue;
      }
      for (const attachment of message.attachments ?? []) {
        if (out.length >= MEDIA_LIMIT || seen.has(attachment.id)) {
          continue;
        }
        if (!attachment.content_type.startsWith("image/")) {
          continue;
        }
        seen.add(attachment.id);
        out.push(attachment);
      }
    }
    return out;
  }, [messagesData]);

  const title = params.name ?? t("conversationInfo.fallbackTitle");
  // Three-column grid: screen width minus the 16pt side gutters and the two
  // inter-tile gaps.
  const tile = Math.floor((width - 16 * 2 - 6 * 2) / 3);

  return (
    <Screen testID="screen-conversation-info" aboveTabBar={router.inPane}>
      <Header onBack={router.onBack}
        title={title}
        subtitle={
          roster.length > 0
            ? t("group.detail.memberCount", { count: roster.length })
            : undefined
        }
        backTo={params.name ?? undefined}
      />
      <Body>
        <SectionTitle>{t("channels:group.members")}</SectionTitle>
        {roster.length === 0 ? (
          <Text
            style={[
              ty.secondary,
              { color: semantic.muted, paddingHorizontal: 20, paddingVertical: 8 },
            ]}
          >
            {t("common:states.loading")}
          </Text>
        ) : (
          <Group style={{ marginHorizontal: 16 }}>
            {roster.map((m) => {
              const isMe = m.userId === currentUser?.id;
              const memberName = isMe
                ? t("conversationInfo.memberSelf", { handle: m.handle })
                : `@${m.handle}`;
              const roleLabel =
                m.role === "owner"
                  ? t("conversationInfo.roleOwner")
                  : m.role === "admin"
                    ? t("conversationInfo.roleAdmin")
                    : undefined;
              return (
                <ListRow
                  key={m.userId}
                  testID={`row-member-${m.userId}`}
                  minHeight={56}
                  glyph={
                    <Avatar
                      label={m.handle}
                      size="sm"
                      variant={isMe ? "self" : "default"}
                    />
                  }
                  // One line, truncated in the middle: a long handle gives
                  // way so the "· you" suffix never wraps onto a line alone.
                  name={
                    <Text
                      numberOfLines={1}
                      ellipsizeMode="middle"
                      style={{
                        fontFamily: fonts.medium,
                        fontSize: 16,
                        color: semantic.text,
                      }}
                    >
                      {memberName}
                    </Text>
                  }
                  sub={roleLabel}
                  accessibilityLabel={
                    roleLabel ? `${memberName}, ${roleLabel}` : memberName
                  }
                  chevron={!isMe}
                  onPress={
                    isMe
                      ? undefined
                      : () =>
                          router.push({
                            pathname: "/user/[id]",
                            params: { id: m.userId },
                          })
                  }
                />
              );
            })}
          </Group>
        )}

        <SectionTitle>{t("nav:media.heading")}</SectionTitle>
        {attachments.length === 0 ? (
          <Text
            style={[
              ty.secondary,
              { color: semantic.muted, paddingHorizontal: 20, paddingVertical: 8 },
            ]}
          >
            {t("nav:media.empty")}
          </Text>
        ) : (
          <View
            style={{
              flexDirection: "row",
              flexWrap: "wrap",
              gap: 6,
              paddingHorizontal: 16,
              paddingVertical: 4,
            }}
          >
            {/* Each tile opens the full-screen viewer (#1248), as desktop's
                media grid opens its lightbox. */}
            {attachments.map((a) => (
              <Pressable
                key={a.id}
                testID={`btn-media-tile-${a.id}`}
                accessibilityRole="button"
                accessibilityLabel={t("chat:attachment.viewLabel", { filename: a.filename })}
                onPress={() => openMedia(a)}
              >
                <MediaImage attachment={a} style={{ width: tile, height: tile }} />
              </Pressable>
            ))}
          </View>
        )}

        {/* The export block's heading has no top padding of its own; give it
            the same gap every other section gets above its title. */}
        <View style={{ paddingTop: space.xxxl }}>
          <ExportArchive conversationId={conversationId} />
        </View>
      </Body>
    </Screen>
  );
}

export default observer(ConversationInfo);
