// Conversation info (#826): member roster + shared media for a channel or DM.

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
import { SafetyNumberRows } from "../../components/direct/SafetyNumberRows";
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
import { useThemeRerender } from "../../components/theme";

// Each tile decrypts its own bytes, so the grid is capped.
const MEDIA_LIMIT = 30;

function ConversationInfo() {
  useThemeRerender();
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
  // Fall back to the selected group (the chat screen keeps it selected).
  const groupId =
    kind === "channel"
      ? (params.groupId ?? appStore.selectedGroupId)
      : null;
  const currentUser = appStore.currentUser;
  const { width } = useWindowDimensions();
  const openMedia = useOpenMediaViewer(conversationId, kind);

  const { data: groupMembers = [] } = useGroupMembers(groupId);
  const dmChannel = useDMChannel(kind === "dm" ? conversationId : null);

  // Role then alphabetical (no presence source on mobile).
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

  const { data: messagesData } = useMessages(conversationId, kind);
  const attachments = useMemo(() => {
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
                  // Truncated in the middle so "· you" never wraps alone.
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

        {kind === "dm" ? (
          <SafetyNumberRows
            peers={(dmChannel.data?.members ?? []).filter(
              (m) => m.user_id !== currentUser?.id,
            )}
          />
        ) : null}

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

        <View style={{ paddingTop: space.xxxl }}>
          <ExportArchive conversationId={conversationId} />
        </View>
      </Body>
    </Screen>
  );
}

export default observer(ConversationInfo);
