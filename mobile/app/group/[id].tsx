import { View, Text } from "react-native";
import { useRouter, useLocalSearchParams } from "expo-router";
import {
  Screen,
  Crumb,
  Body,
  SectionTitle,
  ListRow,
  Avatar,
  Chip,
  Ctx,
  CtxAct,
} from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import {
  useGroupChannels,
  useUserGroupsWithChannels,
  useGroupMembers,
  useLeaveGroup,
  useGroupJoinRequests,
} from "../../hooks/queries";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

function GroupDetail() {
  const router = useRouter();
  const { id } = useLocalSearchParams<{ id: string }>();
  const groupId = id ?? null;

  // Reuse the cached groups list to find the group's metadata without
  // hitting Turso again. Falls back to "Group" when the cache hasn't
  // hydrated yet (deep-link / fresh launch).
  const { data: groups = [] } = useUserGroupsWithChannels();
  const group = groups.find((g) => g.id === groupId);

  const { data: channels = [], isLoading: channelsLoading } =
    useGroupChannels(groupId);
  const { data: members = [] } = useGroupMembers(groupId);
  const { data: joinRequests = [] } = useGroupJoinRequests(groupId);
  const leaveGroup = useLeaveGroup();

  const setSelectedGroupId = appStore.setSelectedGroupId;
  const setSelectedChannelId = appStore.setSelectedChannelId;

  const groupName = group?.name ?? "Group";
  const adminCount = members.filter(
    (m) => m.role === "admin" || m.role === "owner",
  ).length;

  const onLeave = () => {
    if (!groupId) {
      return;
    }
    leaveGroup.mutate(groupId, {
      onSuccess: () => router.replace("/(tabs)/groups"),
    });
  };

  return (
    <Screen>
      <Crumb
        segs={[{ label: "GROUPS" }, { label: groupName, leaf: true }]}
        end={`${members.length || 0} MEMBERS`}
      />
      <Body>
        <View
          style={{
            flexDirection: "row",
            alignItems: "center",
            gap: 10,
            paddingHorizontal: 18,
            paddingTop: 8,
            paddingBottom: 16,
          }}
        >
          <View style={{ flexDirection: "row" }}>
            {members.slice(0, 3).map((m, i) => (
              <Avatar
                key={m.user_id}
                label={(m.username || m.user_id || "us").slice(0, 2)}
                size="sm"
                variant={i === 0 ? "amber" : "default"}
                style={{ marginRight: i < 2 ? -8 : 0 }}
              />
            ))}
          </View>
          <Text
            style={{
              flex: 1,
              fontFamily: ty.body.fontFamily,
              fontSize: 12,
              color: semantic.mute,
            }}
          >
            {members
              .slice(0, 3)
              .map((m) => m.username || m.user_id.slice(0, 6))
              .join(", ")}
            {members.length > 3 ? ` +${members.length - 3}` : ""}
          </Text>
        </View>

        <SectionTitle>TEXT CHANNELS</SectionTitle>
        {channelsLoading && channels.length === 0 ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingVertical: 8,
            }}
          >
            Loading channels…
          </Text>
        ) : null}
        {channels.map((c) => (
          <ListRow
            key={c.id}
            minHeight={54}
            glyph={<Icon.hash color={semantic.mute} />}
            name={c.name}
            sub={c.description ?? undefined}
            onPress={() => {
              setSelectedGroupId(groupId);
              setSelectedChannelId(c.id);
              router.push({
                pathname: "/chat/[id]",
                params: { id: c.id, kind: "channel", name: c.name },
              });
            }}
          />
        ))}

        <SectionTitle>ADMIN</SectionTitle>
        <ListRow
          minHeight={48}
          glyph={<Icon.people color={semantic.mute} />}
          name="Members"
          nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
          sub={`${members.length}${adminCount ? ` · ${adminCount} admin` : ""}`}
          onPress={() =>
            groupId &&
            router.push({
              pathname: "/group/members",
              params: { groupId },
            })
          }
          end={<Icon.fwd color={semantic.mute} />}
        />
        <ListRow
          minHeight={48}
          glyph={<Icon.at color={semantic.mute} />}
          name="Invite a member"
          nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
          onPress={() =>
            groupId &&
            router.push({
              pathname: "/group/invite",
              params: { groupId },
            })
          }
          end={<Icon.fwd color={semantic.mute} />}
        />
        <ListRow
          minHeight={48}
          glyph={<Icon.gear color={semantic.mute} />}
          name="Settings"
          nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
          sub="Rename, channels, danger zone"
          onPress={() =>
            groupId &&
            router.push({
              pathname: "/group/settings",
              params: { groupId },
            })
          }
          end={<Icon.fwd color={semantic.mute} />}
        />
        {joinRequests.length > 0 ? (
          <ListRow
            minHeight={48}
            glyph={<Icon.inbox color={semantic.mute} />}
            name="Join requests"
            nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
            sub={`${joinRequests.length} pending`}
            onPress={() =>
              groupId &&
              router.push({
                pathname: "/group/requests",
                params: { groupId },
              })
            }
            end={<Icon.fwd color={semantic.mute} />}
          />
        ) : null}

        <SectionTitle>DANGER</SectionTitle>
        <ListRow
          minHeight={48}
          glyph={<Icon.exit color={semantic.danger} />}
          name={leaveGroup.isPending ? "Leaving…" : "Leave group"}
          nameStyle={{
            fontSize: 14,
            fontFamily: ty.body.fontFamily,
            color: semantic.danger,
          }}
          onPress={onLeave}
        />
        {leaveGroup.isError ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 12,
              color: semantic.danger,
              paddingHorizontal: 18,
              paddingTop: 6,
            }}
          >
            {(leaveGroup.error as Error).message || "Couldn't leave the group."}
          </Text>
        ) : null}
      </Body>

      <Ctx
        cr="GROUPS"
        name={groupName}
        actions={<CtxAct icon={<Icon.kebab color={semantic.ink2} />} />}
      />
    </Screen>
  );
}

export default observer(GroupDetail);
