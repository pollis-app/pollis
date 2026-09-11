import { useCallback } from "react";
import { View, Text } from "react-native";
import { useFocusEffect, useRouter, useLocalSearchParams } from "expo-router";
import { useTranslation } from "react-i18next";
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
import { upper } from "../../i18n";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

function GroupDetail() {
  const { t } = useTranslation("channels");
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

  // Being on this list means no conversation is open — clear the channel
  // selection so realtime `new_message` events for the channel the user just
  // left count as unread again (same rule as the tab lists).
  useFocusEffect(
    useCallback(() => {
      appStore.setSelectedChannelId(null);
    }, []),
  );

  const groupName = group?.name ?? t("mobile:group.common.fallbackName");
  const adminCount = members.filter(
    (m) => m.role === "admin" || m.role === "owner",
  ).length;
  const membersSub = [
    String(members.length),
    adminCount ? t("mobile:group.detail.adminCount", { count: adminCount }) : null,
  ]
    .filter(Boolean)
    .join(" · ");

  const onLeave = () => {
    if (!groupId) {
      return;
    }
    leaveGroup.mutate(groupId, {
      onSuccess: () => router.replace("/(tabs)/groups"),
    });
  };

  return (
    <Screen testID="screen-group">
      <Crumb
        segs={[
          { label: upper(t("nav:breadcrumb.groups")) },
          { label: groupName, leaf: true },
        ]}
        end={upper(
          t("mobile:group.detail.memberCount", { count: members.length || 0 }),
        )}
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
            {members.length > 3
              ? ` ${t("mobile:group.detail.moreMembers", { count: members.length - 3 })}`
              : ""}
          </Text>
        </View>

        <SectionTitle>{upper(t("mobile:group.detail.textChannels"))}</SectionTitle>
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
            {t("mobile:group.detail.loadingChannels")}
          </Text>
        ) : null}
        {channels.map((c) => (
          <ListRow
            key={c.id}
            testID={`row-channel-${c.id}`}
            minHeight={54}
            glyph={<Icon.hash color={semantic.mute} />}
            name={c.name}
            sub={c.description ?? undefined}
            onPress={() => {
              setSelectedGroupId(groupId);
              setSelectedChannelId(c.id);
              // Opening a conversation clears its unread count.
              appStore.markRead(c.id);
              router.push({
                pathname: "/chat/[id]",
                params: { id: c.id, kind: "channel", name: c.name },
              });
            }}
          />
        ))}

        <SectionTitle>{upper(t("mobile:group.detail.adminSection"))}</SectionTitle>
        <ListRow
          testID="row-group-members"
          minHeight={48}
          glyph={<Icon.people color={semantic.mute} />}
          name={t("group.members")}
          nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
          sub={membersSub}
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
          testID="row-group-invite"
          minHeight={48}
          glyph={<Icon.at color={semantic.mute} />}
          name={t("group.inviteMember")}
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
          testID="row-group-settings"
          minHeight={48}
          glyph={<Icon.gear color={semantic.mute} />}
          name={t("nav:breadcrumb.settings")}
          nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
          sub={t("mobile:group.detail.settingsSub")}
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
            testID="row-group-requests"
            minHeight={48}
            glyph={<Icon.inbox color={semantic.mute} />}
            name={t("group.joinRequests")}
            nameStyle={{ fontSize: 14, fontFamily: ty.body.fontFamily }}
            sub={t("nav:home.pending", { count: joinRequests.length })}
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

        <SectionTitle>{upper(t("mobile:group.common.danger"))}</SectionTitle>
        <ListRow
          testID="btn-leave-group"
          minHeight={48}
          glyph={<Icon.exit color={semantic.danger} />}
          name={
            leaveGroup.isPending ? t("leaveGroup.submitting") : t("group.leave")
          }
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
            {(leaveGroup.error as Error).message || t("leaveGroup.leaveFailed")}
          </Text>
        ) : null}
      </Body>

      <Ctx
        cr={upper(t("nav:breadcrumb.groups"))}
        name={groupName}
        actions={
          <CtxAct
            testID="btn-group-menu"
            accessibilityLabel={t("mobile:group.detail.menuLabel")}
            icon={<Icon.kebab color={semantic.ink2} />}
          />
        }
      />
    </Screen>
  );
}

export default observer(GroupDetail);
