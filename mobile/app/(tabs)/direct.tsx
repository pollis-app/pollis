import { useCallback, useMemo } from "react";
import { View, Text } from "react-native";
import { useFocusEffect, useRouter } from "expo-router";
import {
  Screen,
  Crumb,
  Body,
  SectionTitle,
  ListRow,
  Avatar,
  Chip,
  Button,
  BottomAction,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import {
  useDMChannels,
  useDMRequests,
  useAcceptDMRequest,
  useBlockUser,
  useLastMessages,
  previewText,
} from "../../hooks/queries";
import { timeAgoShort } from "../../lib/timeAgo";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";
import { useLayoutClass } from "../../hooks/useLayoutClass";
import { TwoPane, DetailPlaceholder } from "../../components/MasterDetail";
import { ChatView } from "../chat/[id]";

function Direct() {
  const router = useRouter();
  const { data: dms = [], isLoading, isError } = useDMChannels();
  const { data: requests = [] } = useDMRequests();
  const acceptRequest = useAcceptDMRequest();
  // "Decline" for a DM request is blocking the sender — mirrors desktop's
  // RequestsPage / dm-request bar, which offer exactly Accept and Block
  // (there is no decline command in pollis-core).
  const blockUser = useBlockUser();
  const setSelectedConversationId = appStore.setSelectedConversationId;
  const selectedConversationId = appStore.selectedConversationId;
  const unreadCounts = appStore.unreadCounts;
  // On regular (iPad) width the list is the left column of a two-pane
  // master-detail; on compact it is the whole screen with push navigation.
  const isRegular = useLayoutClass() === "regular";
  const selectedDm = dms.find((d) => d.id === selectedConversationId);
  const selectedHandle = selectedDm?.user2_identifier || undefined;

  // One batched preview fetch for every DM row (desktop #874/#936) — never
  // one call per row. A conversation with no locally-ingested messages is
  // absent from the map and its row simply renders without a preview.
  const dmIds = useMemo(() => dms.map((d) => d.id), [dms]);
  const { data: lastMessages = {} } = useLastMessages(dmIds);

  // On compact, being on this list means no conversation is open — clear the
  // selection so realtime `new_message` events for the conversation the user
  // just left count as unread again. On regular the detail pane keeps the
  // conversation visible, so the selection (and unread suppression) stands.
  useFocusEffect(
    useCallback(() => {
      if (!isRegular) {
        appStore.setSelectedConversationId(null);
      }
    }, [isRegular]),
  );

  // The single-column content — rendered as the whole screen on compact, or as
  // the left list column of the two-pane on regular. Byte-for-byte identical on
  // compact save for the row onPress, which only skips the push on regular.
  const listColumn = (
    <>
      <Crumb segs={[{ label: "DIRECT", leaf: true }]} end={String(dms.length)} />
      <Body>
        {requests.length > 0 ? (
          <View>
            <SectionTitle>PENDING REQUESTS</SectionTitle>
            {requests.map((d) => {
              const handle = d.user2_identifier || "user";
              return (
                <ListRow
                  key={d.id}
                  testID={`row-request-${d.id}`}
                  minHeight={64}
                  glyph={<Avatar label={handle.slice(0, 2)} />}
                  name={
                    <Text
                      style={{
                        fontFamily: ty.rowN.fontFamily,
                        fontSize: 15,
                        color: semantic.ink,
                      }}
                    >
                      @{handle}
                    </Text>
                  }
                  sub="wants to message you"
                  end={
                    <View style={{ flexDirection: "row", gap: 6 }}>
                      <Chip
                        testID={`btn-block-request-${d.id}`}
                        accessibilityLabel="Block sender"
                        onPress={() => {
                          if (d.user2_id) {
                            blockUser.mutate(d.user2_id);
                          }
                        }}
                      >
                        {blockUser.isPending ? "…" : "Block"}
                      </Chip>
                      <Chip
                        testID={`btn-accept-request-${d.id}`}
                        accessibilityLabel="Accept request"
                        variant="on"
                        onPress={() => acceptRequest.mutate(d.id)}
                      >
                        {acceptRequest.isPending ? "…" : "Accept"}
                      </Chip>
                    </View>
                  }
                />
              );
            })}
          </View>
        ) : null}
        {isLoading ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingTop: 12,
            }}
          >
            Loading conversations…
          </Text>
        ) : null}
        {isError ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.danger,
              paddingHorizontal: 18,
              paddingTop: 12,
            }}
          >
            Couldn't load conversations.
          </Text>
        ) : null}
        {!isLoading && !isError && dms.length === 0 ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingTop: 12,
            }}
          >
            No direct messages yet.
          </Text>
        ) : null}
        {dms.map((d) => {
          const handle = d.user2_identifier || "user";
          const label = handle.slice(0, 2);
          const last = lastMessages[d.id];
          const preview = previewText(last);
          const unread = unreadCounts[d.id] ?? 0;
          return (
            <ListRow
              key={d.id}
              testID={`row-dm-${d.id}`}
              minHeight={64}
              selected={isRegular && selectedConversationId === d.id}
              onPress={() => {
                setSelectedConversationId(d.id);
                // Opening a conversation clears its unread count (desktop
                // does the same in its DM page).
                appStore.markRead(d.id);
                // On regular the right pane updates in place; on compact push
                // the conversation as today.
                if (!isRegular) {
                  router.push({
                    pathname: "/chat/[id]",
                    params: { id: d.id, kind: "dm", name: handle },
                  });
                }
              }}
              glyph={<Avatar label={label} />}
              name={
                <Text
                  style={{
                    fontFamily: ty.rowN.fontFamily,
                    fontSize: 15,
                    color: semantic.ink,
                  }}
                >
                  @{handle}
                </Text>
              }
              sub={preview ?? undefined}
              end={
                <>
                  {last ? (
                    <Text style={[ty.label, { fontSize: 10 }]}>
                      {timeAgoShort(last.created_at)}
                    </Text>
                  ) : null}
                  {unread > 0 ? (
                    <View
                      testID={`unread-${d.id}`}
                      style={{
                        width: 7,
                        height: 7,
                        borderRadius: 4,
                        backgroundColor: semantic.accent,
                      }}
                    />
                  ) : null}
                </>
              }
            />
          );
        })}
      </Body>
      <BottomAction>
        <Button
          testID="btn-new-dm"
          full
          icon={<Icon.plus color={semantic.ink} />}
          onPress={() => router.push("/dm/new")}
        >
          NEW DIRECT MESSAGE
        </Button>
      </BottomAction>
    </>
  );

  return (
    <Screen testID="screen-direct">
      {isRegular ? (
        <TwoPane
          list={listColumn}
          detail={
            selectedConversationId ? (
              <ChatView
                conversationId={selectedConversationId}
                kind="dm"
                embedded
                name={selectedHandle}
              />
            ) : (
              <DetailPlaceholder />
            )
          }
        />
      ) : (
        listColumn
      )}
    </Screen>
  );
}

export default observer(Direct);
