// Conversation info — member roster + shared media for a channel or DM.
// Mobile counterpart of desktop's right-hand context panel (#826): Discord's
// member list over Messenger's shared-media grid. The chat screen's kebab
// navigates here with { id, kind, groupId?, name? }.

import { useMemo } from "react";
import { View, Text, useWindowDimensions } from "react-native";
import { useRouter, useLocalSearchParams } from "expo-router";
import {
  Screen,
  Crumb,
  Body,
  SectionTitle,
  ListRow,
  Avatar,
  Ctx,
  BottomAction,
  Button,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import {
  useDMChannel,
  useGroupMembers,
  useMessages,
  flattenPages,
  sortMembersByRole,
  type ConversationKind,
} from "../../hooks/queries";
import { MediaImage } from "../../components/Media";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

// Cap on grid tiles, matching desktop's MembersPanel (#826): each tile
// resolves its own bytes through the media transport, so an unbounded grid
// on a media-heavy conversation would fan out into hundreds of decrypt
// round-trips the moment the screen opens.
const MEDIA_LIMIT = 30;

function ConversationInfo() {
  const router = useRouter();
  const params = useLocalSearchParams<{
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

  const ctxLabel = kind === "dm" ? "DIRECT" : "CHANNEL";
  const title = params.name ?? "Conversation";
  // Three-column grid: screen width minus the Body's horizontal padding and
  // the two inter-tile gaps.
  const tile = Math.floor((width - 18 * 2 - 6 * 2) / 3);

  return (
    <Screen testID="screen-conversation-info">
      <Crumb
        segs={[{ label: ctxLabel }, { label: "Info", leaf: true }]}
        end={roster.length > 0 ? String(roster.length) : undefined}
      />
      <Body>
        <SectionTitle>{`MEMBERS${roster.length > 0 ? ` · ${roster.length}` : ""}`}</SectionTitle>
        {roster.length === 0 ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingVertical: 8,
            }}
          >
            Loading…
          </Text>
        ) : null}
        {roster.map((m) => {
          const isMe = m.userId === currentUser?.id;
          return (
            <ListRow
              key={m.userId}
              testID={`row-member-${m.userId}`}
              minHeight={54}
              glyph={<Avatar label={m.handle.slice(0, 2)} />}
              name={`@${m.handle}${isMe ? " · you" : ""}`}
              nameStyle={{ fontSize: 14 }}
              sub={
                m.role === "owner"
                  ? "Owner"
                  : m.role === "admin"
                    ? "Admin"
                    : undefined
              }
              onPress={
                isMe
                  ? undefined
                  : () =>
                      router.push({
                        pathname: "/user/[id]",
                        params: { id: m.userId },
                      })
              }
              end={!isMe ? <Icon.fwd color={semantic.mute} /> : undefined}
            />
          );
        })}

        <SectionTitle>SHARED MEDIA</SectionTitle>
        {attachments.length === 0 ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingVertical: 8,
            }}
          >
            No media shared yet.
          </Text>
        ) : (
          <View
            style={{
              flexDirection: "row",
              flexWrap: "wrap",
              gap: 6,
              paddingHorizontal: 18,
              paddingVertical: 8,
            }}
          >
            {attachments.map((a) => (
              <MediaImage
                key={a.id}
                attachment={a}
                style={{ width: tile, height: tile }}
              />
            ))}
          </View>
        )}
      </Body>
      <Ctx cr={ctxLabel} name={`${title} · Info`} />
      <BottomAction>
        <Button
          full
          testID="btn-back-to-conversation"
          variant="subtle"
          onPress={() => router.back()}
          icon={<Icon.back color={semantic.ink} />}
        >
          Back to conversation
        </Button>
      </BottomAction>
    </Screen>
  );
}

export default observer(ConversationInfo);
