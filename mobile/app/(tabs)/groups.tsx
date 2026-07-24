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
import { useLayoutClass } from "../../hooks/useLayoutClass";
import { TwoPane, DetailPlaceholder } from "../../components/MasterDetail";
import { ChatView } from "../chat/[id]";

function Groups() {
  const router = useRouter();
  const { data: groups = [], isLoading, isError } = useUserGroupsWithChannels();
  const { data: invites = [] } = usePendingGroupInvites();
  const acceptInvite = useAcceptGroupInvite();
  const declineInvite = useDeclineGroupInvite();
  const setSelectedGroupId = appStore.setSelectedGroupId;
  const setSelectedChannelId = appStore.setSelectedChannelId;
  const selectedGroupId = appStore.selectedGroupId;
  const selectedChannelId = appStore.selectedChannelId;
  // On regular (iPad) width the list is the left column of a two-pane
  // master-detail; on compact it is the whole screen with push navigation.
  const isRegular = useLayoutClass() === "regular";

  const totalChannels = groups.reduce((acc, g) => acc + g.channels.length, 0);

  // The single-column content — rendered as the whole screen on compact, or as
  // the left list column of the two-pane on regular. Byte-for-byte identical on
  // compact save for the row onPress, which only skips the push on regular.
  const listColumn = (
    <>
      <Crumb segs={[{ label: "GROUPS", leaf: true }]} end={String(totalChannels)} />
      <Body>
        {invites.length > 0 ? (
          <View>
            <SectionTitle>PENDING INVITES</SectionTitle>
            {invites.map((inv) => (
              <ListRow
                key={inv.id}
                testID={`row-invite-${inv.id}`}
                glyph={<Icon.inbox color={semantic.accent} />}
                name={inv.group_name}
                sub={`from @${inv.inviter_username ?? "someone"}`}
                end={
                  <View style={{ flexDirection: "row", gap: 6 }}>
                    <Chip
                      testID={`btn-decline-invite-${inv.id}`}
                      accessibilityLabel="Decline invite"
                      onPress={() => declineInvite.mutate(inv.id)}
                    >
                      Decline
                    </Chip>
                    <Chip
                      testID={`btn-accept-invite-${inv.id}`}
                      accessibilityLabel="Accept invite"
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
                testID={`row-channel-${c.id}`}
                selected={isRegular && selectedChannelId === c.id}
                glyph={<Icon.hash color={semantic.mute} />}
                name={c.name}
                sub={c.description ?? undefined}
                onPress={() => {
                  setSelectedGroupId(g.id);
                  setSelectedChannelId(c.id);
                  // On regular the right pane updates in place; on compact push
                  // the channel chat as today.
                  if (!isRegular) {
                    router.push({
                      pathname: "/chat/[id]",
                      params: { id: c.id, kind: "channel", name: c.name },
                    });
                  }
                }}
              />
            ))}
          </View>
        ))}
      </Body>
      <BottomAction>
        <Button
          testID="btn-create-group"
          full
          icon={<Icon.plus color={semantic.ink} />}
          onPress={() => router.push("/group/new")}
        >
          New Group
        </Button>
        <Button
          testID="btn-join-group"
          variant="subtle"
          full
          icon={<Icon.search color={semantic.ink} />}
          onPress={() => router.push("/group/discover")}
        >
          Join Group
        </Button>
      </BottomAction>
    </>
  );

  return (
    <Screen testID="screen-groups">
      {isRegular ? (
        <TwoPane
          list={listColumn}
          detail={
            selectedChannelId ? (
              <ChatView
                conversationId={selectedChannelId}
                kind="channel"
                groupId={selectedGroupId ?? undefined}
                embedded
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

export default observer(Groups);
