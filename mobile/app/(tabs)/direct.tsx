import { View, Text } from "react-native";
import { useRouter } from "expo-router";
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
} from "../../hooks/queries";
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
  const setSelectedConversationId = appStore.setSelectedConversationId;
  const selectedConversationId = appStore.selectedConversationId;
  // On regular (iPad) width the list is the left column of a two-pane
  // master-detail; on compact it is the whole screen with push navigation.
  const isRegular = useLayoutClass() === "regular";
  const selectedDm = dms.find((d) => d.id === selectedConversationId);
  const selectedHandle = selectedDm?.user2_identifier || undefined;

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
                    <Chip
                      testID={`btn-accept-request-${d.id}`}
                      accessibilityLabel="Accept request"
                      variant="on"
                      onPress={() => acceptRequest.mutate(d.id)}
                    >
                      {acceptRequest.isPending ? "…" : "Accept"}
                    </Chip>
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
          return (
            <ListRow
              key={d.id}
              testID={`row-dm-${d.id}`}
              minHeight={64}
              selected={isRegular && selectedConversationId === d.id}
              onPress={() => {
                setSelectedConversationId(d.id);
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
