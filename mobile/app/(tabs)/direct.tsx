import { View, Text } from "react-native";
import { useRouter } from "expo-router";
import {
  Screen,
  Crumb,
  Body,
  ListRow,
  Avatar,
  Button,
  BottomAction,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import { useDMChannels } from "../../hooks/queries";
import { useAppStore } from "../../stores/appStore";

export default function Direct() {
  const router = useRouter();
  const { data: dms = [], isLoading, isError } = useDMChannels();
  const setSelectedConversationId = useAppStore(
    (s) => s.setSelectedConversationId,
  );

  return (
    <Screen>
      <Crumb segs={[{ label: "DIRECT", leaf: true }]} end={String(dms.length)} />
      <Body>
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
                router.push(`/chat/${d.id}`);
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
        <Button full icon={<Icon.plus color={semantic.ink} />}>
          NEW DIRECT MESSAGE
        </Button>
      </BottomAction>
    </Screen>
  );
}
