import { View, Text } from "react-native";
import { useRouter } from "expo-router";
import {
  Screen,
  Crumb,
  Body,
  SectionTitle,
  ListRow,
  Chip,
  Button,
  BottomAction,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import {
  useUserGroupsWithChannels,
  usePendingGroupInvites,
  useAcceptGroupInvite,
  useDeclineGroupInvite,
} from "../../hooks/queries";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

function Groups() {
  const router = useRouter();
  const { data: groups = [], isLoading, isError } = useUserGroupsWithChannels();
  const { data: invites = [] } = usePendingGroupInvites();
  const acceptInvite = useAcceptGroupInvite();
  const declineInvite = useDeclineGroupInvite();
  const setSelectedGroupId = appStore.setSelectedGroupId;
  const setSelectedChannelId = appStore.setSelectedChannelId;

  const totalChannels = groups.reduce((acc, g) => acc + g.channels.length, 0);

  return (
    <Screen>
      <Crumb segs={[{ label: "GROUPS", leaf: true }]} end={String(totalChannels)} />
      <Body>
        {invites.length > 0 ? (
          <View>
            <SectionTitle>PENDING INVITES</SectionTitle>
            {invites.map((inv) => (
              <ListRow
                key={inv.id}
                glyph={<Icon.inbox color={semantic.accent} />}
                name={inv.group_name}
                sub={`from @${inv.inviter_username ?? "someone"}`}
                end={
                  <View style={{ flexDirection: "row", gap: 6 }}>
                    <Chip
                      onPress={() => declineInvite.mutate(inv.id)}
                    >
                      Decline
                    </Chip>
                    <Chip
                      variant="on"
                      onPress={() => acceptInvite.mutate(inv.id)}
                    >
                      {acceptInvite.isPending ? "…" : "Accept"}
                    </Chip>
                  </View>
                }
              />
            ))}
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
            Loading groups…
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
            Couldn't load groups.
          </Text>
        ) : null}
        {!isLoading && !isError && groups.length === 0 ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingTop: 12,
            }}
          >
            No groups yet. Create one to get started.
          </Text>
        ) : null}
        {groups.map((g) => (
          <View key={g.id}>
            <SectionTitle right={<Icon.fwd color={semantic.mute} />}>
              {g.name.toUpperCase()}
            </SectionTitle>
            {g.channels.length === 0 ? (
              <Text
                style={{
                  fontFamily: ty.body.fontFamily,
                  fontSize: 12,
                  color: semantic.mute,
                  paddingHorizontal: 18,
                  paddingVertical: 6,
                }}
              >
                No channels.
              </Text>
            ) : null}
            {g.channels.map((c) => (
              <ListRow
                key={c.id}
                glyph={<Icon.hash color={semantic.mute} />}
                name={c.name}
                sub={c.description ?? undefined}
                onPress={() => {
                  setSelectedGroupId(g.id);
                  setSelectedChannelId(c.id);
                  router.push({
                    pathname: "/chat/[id]",
                    params: { id: c.id, kind: "channel", name: c.name },
                  });
                }}
              />
            ))}
          </View>
        ))}
      </Body>
      <BottomAction>
        <Button
          full
          icon={<Icon.plus color={semantic.ink} />}
          onPress={() => router.push("/group/new")}
        >
          New Group
        </Button>
        <Button
          variant="subtle"
          full
          icon={<Icon.search color={semantic.ink} />}
          onPress={() => router.push("/group/discover")}
        >
          Join Group
        </Button>
      </BottomAction>
    </Screen>
  );
}

export default observer(Groups);
