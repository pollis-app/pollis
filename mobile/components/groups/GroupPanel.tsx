import { useEffect, useMemo, useState } from "react";
import { Pressable, ScrollView, Text, View } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { Divider, IconButton } from "../ui";
import { Icon } from "../icons";
import { semantic, type as ty, fonts, r, space, layout } from "../../theme/tokens";
import {
  useUserGroupsWithChannels,
  useGroupChannels,
  useGroupMembers,
  useGroupJoinRequests,
} from "../../hooks/queries";
import { appStore } from "../../stores/appStore";
import type { Channel } from "../../types";
import { ChannelRow } from "./ChannelRow";
import { GroupMenuSheet } from "./GroupMenuSheet";
import { CreateChannelSheet } from "./CreateChannelSheet";

// Re-reads of an empty roster (a just-created group's read can lag the
// create): a few, spaced out, then give up and show no count.
const ROSTER_RETRIES = 3;
const ROSTER_RETRY_MS = 1500;

// One group's channel sheet (Main.dc.html, the panel under the group strip):
// group header (name + chevron → group menu, member count) with Invite, a
// "Search <group>" button, then "Text channels" (+ create, for admins) and
// "This group" (Members, Group settings, Join requests). Shared by the Groups
// tab (under the pill strip) and the /group/[id] route (under a back bar), so
// both entry points render the same thing.
function GroupPanelImpl({
  groupId,
  selectedChannelId,
  onOpenChannel,
  top,
  testID,
  bottomInset = 0,
}: {
  groupId: string;
  // Highlighted row (iPad two-pane, where the chat stays visible).
  selectedChannelId?: string | null;
  onOpenChannel: (channel: Channel) => void;
  // Extra rows above the channel list (the tab's cross-group join requests
  // and pending invites).
  top?: React.ReactNode;
  testID?: string;
  // Extra bottom padding for the list when the panel runs to the screen's
  // bottom edge (the /group/[id] route, which has no tab bar to clear).
  bottomInset?: number;
}) {
  const { t } = useTranslation("mobile");
  const router = useRouter();
  const [menuOpen, setMenuOpen] = useState(false);
  const [createOpen, setCreateOpen] = useState(false);

  // Reuse the cached groups list for the group's metadata; the per-group
  // channel query is the fresher source once it has loaded.
  const { data: groups = [] } = useUserGroupsWithChannels();
  const group = groups.find((g) => g.id === groupId);
  const channelsQuery = useGroupChannels(groupId);
  const channels = channelsQuery.data ?? group?.channels ?? [];
  const membersQuery = useGroupMembers(groupId);
  // A member is always in their own group's roster, so an empty or missing
  // roster means it has not loaded yet (or the read lagged a just-created
  // group): show no count rather than a wrong "0 members".
  const members = membersQuery.data ?? [];
  const membersKnown = members.length > 0;
  const refetchMembers = membersQuery.refetch;
  const [rosterRetries, setRosterRetries] = useState(0);
  // The Groups tab reuses one panel across groups: each gets its own retries.
  useEffect(() => {
    setRosterRetries(0);
  }, [groupId]);
  useEffect(() => {
    if (membersKnown || membersQuery.isFetching || rosterRetries >= ROSTER_RETRIES) {
      return;
    }
    if (!membersQuery.isSuccess && !membersQuery.isError) {
      return;
    }
    const timer = setTimeout(() => {
      setRosterRetries((n) => n + 1);
      void refetchMembers();
    }, ROSTER_RETRY_MS);
    return () => clearTimeout(timer);
  }, [
    membersKnown,
    membersQuery.isFetching,
    membersQuery.isSuccess,
    membersQuery.isError,
    rosterRetries,
    refetchMembers,
  ]);
  const { data: joinRequests = [] } = useGroupJoinRequests(groupId);
  const unreadCounts = appStore.unreadCounts;
  const me = appStore.currentUser?.id;

  const myRole = members.find((m) => m.user_id === me)?.role ?? group?.current_user_role;
  const isAdmin = myRole === "admin" || myRole === "owner";
  const groupName = group?.name ?? t("group.common.fallbackName");
  const memberCountLabel = membersKnown
    ? t("group.detail.memberCount", { count: members.length })
    : null;
  const channelNames = useMemo(() => channels.map((c) => c.name.toLowerCase()), [channels]);

  return (
    <View
      testID={testID}
      style={{
        flex: 1,
        backgroundColor: semantic.panel,
        borderTopStartRadius: r.xl,
        borderTopEndRadius: r.xl,
        overflow: "hidden",
      }}
    >
      {/* Group header: the name opens the group menu. */}
      <View
        style={{
          flexDirection: "row",
          alignItems: "flex-start",
          gap: space.sm,
          paddingTop: space.lg,
          paddingBottom: space.md,
          paddingStart: space.xxl,
          paddingEnd: space.sm,
        }}
      >
        <Pressable
          testID="btn-group-menu"
          onPress={() => setMenuOpen(true)}
          accessibilityRole="button"
          accessibilityLabel={
            memberCountLabel
              ? `${t("group.panel.menuLabel", { name: groupName })}, ${memberCountLabel}`
              : t("group.panel.menuLabel", { name: groupName })
          }
          style={{ flex: 1, minWidth: 0, minHeight: layout.touchMin, justifyContent: "center", gap: 2 }}
        >
          <View style={{ flexDirection: "row", alignItems: "center", gap: space.xs }}>
            <Text
              accessibilityRole="header"
              numberOfLines={1}
              style={[ty.title, { flexShrink: 1 }]}
            >
              {groupName}
            </Text>
            <Icon.chevronDown size={18} color={semantic.dim} />
          </View>
          {memberCountLabel ? (
            <Text style={[ty.meta, { fontSize: 13 }]}>{memberCountLabel}</Text>
          ) : null}
        </Pressable>
        <IconButton
          testID="row-group-invite"
          filled
          accessibilityLabel={t("group.panel.invite")}
          icon={<Icon.userPlus size={20} color={semantic.text} />}
          onPress={() => router.push({ pathname: "/group/invite", params: { groupId } })}
        />
      </View>

      {/* Looks like a field; opens Search. */}
      <View style={{ paddingHorizontal: space.xxl, paddingBottom: space.lg }}>
        <Pressable
          testID="btn-search-group"
          accessibilityRole="button"
          accessibilityLabel={t("group.panel.search", { name: groupName })}
          onPress={() => router.push("/(tabs)/search")}
          style={{
            minHeight: layout.touchMin,
            borderRadius: r.sm,
            backgroundColor: semantic.raised,
            flexDirection: "row",
            alignItems: "center",
            gap: space.sm,
            paddingHorizontal: space.lg,
          }}
        >
          <Icon.search size={16} color={semantic.muted} />
          <Text numberOfLines={1} style={{ flex: 1, fontFamily: fonts.regular, fontSize: 15, color: semantic.muted }}>
            {t("group.panel.search", { name: groupName })}
          </Text>
        </Pressable>
      </View>
      <Divider />

      <ScrollView
        style={{ flex: 1 }}
        contentContainerStyle={{
          paddingTop: space.lg,
          paddingHorizontal: space.lg,
          paddingBottom: space.xxxl + bottomInset,
          gap: 2,
        }}
      >
        {top}

        <View
          style={{
            flexDirection: "row",
            alignItems: "center",
            justifyContent: "space-between",
            paddingStart: space.sm,
            minHeight: 32,
          }}
        >
          <Text accessibilityRole="header" style={ty.section}>
            {t("group.detail.textChannels")}
          </Text>
          {isAdmin ? (
            <IconButton
              testID="btn-create-channel"
              accessibilityLabel={t("group.panel.createChannel")}
              icon={<Icon.plus size={18} color={semantic.dim} />}
              onPress={() => setCreateOpen(true)}
            />
          ) : null}
        </View>

        {channelsQuery.isLoading && channels.length === 0 ? (
          <Text style={[ty.secondary, { paddingStart: space.md, paddingVertical: space.sm }]}>
            {t("group.detail.loadingChannels")}
          </Text>
        ) : null}
        {!channelsQuery.isLoading && channels.length === 0 ? (
          <Text style={[ty.secondary, { paddingStart: space.md, paddingVertical: space.sm }]}>
            {t("group.common.noChannels")}
          </Text>
        ) : null}
        {channels.map((c) => {
          const unread = unreadCounts[c.id] ?? 0;
          const selected = selectedChannelId === c.id;
          const color = selected ? semantic.accent : unread > 0 ? semantic.text : semantic.dim;
          const label = selected
            ? t("group.panel.channelSelected", { name: c.name })
            : unread > 0
              ? t("group.panel.channelUnread", { name: c.name, count: unread })
              : c.name;
          return (
            <ChannelRow
              key={c.id}
              testID={`row-channel-${c.id}`}
              icon={<Icon.hash size={16} color={color} />}
              name={c.name}
              selected={selected}
              unread={unread > 0}
              accessibilityLabel={label}
              onPress={() => onOpenChannel(c)}
            />
          );
        })}

        <Text
          accessibilityRole="header"
          style={[ty.section, { paddingStart: space.sm, paddingTop: space.xxl, paddingBottom: 4 }]}
        >
          {t("group.panel.thisGroup")}
        </Text>
        <ChannelRow
          testID="row-group-members"
          icon={<Icon.users size={16} color={semantic.dim} />}
          name={t("group.panel.members")}
          value={membersKnown ? String(members.length) : undefined}
          accessibilityLabel={
            memberCountLabel
              ? `${t("group.panel.members")}, ${memberCountLabel}`
              : t("group.panel.members")
          }
          onPress={() => router.push({ pathname: "/group/members", params: { groupId } })}
        />
        <ChannelRow
          testID="row-group-settings"
          icon={<Icon.sliders size={16} color={semantic.dim} />}
          name={t("group.panel.settings")}
          accessibilityLabel={t("group.panel.settings")}
          onPress={() => router.push({ pathname: "/group/settings", params: { groupId } })}
        />
        {joinRequests.length > 0 ? (
          <ChannelRow
            testID="row-group-requests"
            icon={<Icon.inbox size={16} color={semantic.text} />}
            name={t("group.panel.joinRequests")}
            count={joinRequests.length}
            dimWhenRead={false}
            accessibilityLabel={`${t("group.panel.joinRequests")}, ${t("channels:groups.joinRequestsPending", { count: joinRequests.length })}`}
            onPress={() => router.push({ pathname: "/group/requests", params: { groupId } })}
          />
        ) : null}
      </ScrollView>

      {menuOpen ? (
        <GroupMenuSheet
          groupId={groupId}
          groupName={groupName}
          memberCount={membersKnown ? members.length : undefined}
          pendingRequests={joinRequests.length}
          onClose={() => setMenuOpen(false)}
        />
      ) : null}
      {createOpen ? (
        <CreateChannelSheet
          groupId={groupId}
          existingNames={channelNames}
          onClose={() => setCreateOpen(false)}
          onCreated={onOpenChannel}
        />
      ) : null}
    </View>
  );
}

export const GroupPanel = observer(GroupPanelImpl);
