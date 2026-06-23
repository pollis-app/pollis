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

function Direct() {
  const router = useRouter();
  const { data: dms = [], isLoading, isError } = useDMChannels();
  const { data: requests = [] } = useDMRequests();
  const acceptRequest = useAcceptDMRequest();
  const setSelectedConversationId = appStore.setSelectedConversationId;

  return (
    <Screen>
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
              minHeight={64}
              onPress={() => {
                setSelectedConversationId(d.id);
                router.push({
                  pathname: "/chat/[id]",
                  params: { id: d.id, kind: "dm", name: handle },
                });
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
          full
          icon={<Icon.plus color={semantic.ink} />}
          onPress={() => router.push("/dm/new")}
        >
          NEW DIRECT MESSAGE
        </Button>
      </BottomAction>
    </Screen>
  );
}

export default observer(Direct);
