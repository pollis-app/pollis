import { useEffect, useMemo, useState } from "react";
import { View } from "react-native";
import { ScrollView } from "react-native-gesture-handler";
import { Text } from "../Text";
import { useRouter } from "expo-router";
import { useNav } from "../pane/paneContext";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { Button, Divider, IconButton, PressInPressable, EmptyState } from "../ui";
import { Icon } from "../icons";
import { semantic, type as ty, fonts, r, space, layout, typeSize } from "../../theme/tokens";
import {
  useUserGroupsWithChannels,
  useGroupChannels,
  useGroupMembers,
  useGroupJoinRequests,
  useMutedChannels,
  usePrefetchConversation,
  useMoveCategory,
  useMoveChannel,
} from "../../hooks/queries";
import { appStore } from "../../stores/appStore";
import type { Channel, ChannelCategory } from "../../types";
import { ChannelRow } from "./ChannelRow";
import { GroupChannelList } from "./GroupChannelList";
import { CategorySheet, type CategorySheetMode } from "./CategorySheet";
import { GroupMenuSheet } from "./GroupMenuSheet";
import { CreateChannelSheet } from "./CreateChannelSheet";
import { useThemeRerender } from "../theme";

// Re-reads of an empty roster (a just-created group's read can lag).
const ROSTER_RETRIES = 3;
const ROSTER_RETRY_MS = 1500;

// One group's channel sheet, shared by the Groups tab and the /group/[id] route.
function GroupPanelImpl({
  groupId,
  selectedChannelId,
  onOpenChannel,
  top,
  testID,
  bottomInset = 0,
}: {
  groupId: string;
  selectedChannelId?: string | null;
  onOpenChannel: (channel: Channel) => void;
  // Extra rows above the channel list.
  top?: React.ReactNode;
  testID?: string;
  // For the /group/[id] route, which has no tab bar to clear.
  bottomInset?: number;
}) {
  useThemeRerender();
  const { t } = useTranslation("mobile");
  const router = useNav();
  const rootRouter = useRouter();
  const prefetch = usePrefetchConversation();
  const [menuOpen, setMenuOpen] = useState(false);
  // `null` closed; `{ category: null }` the group's own "+"; a category's "+" names it (#2031).
  const [createIn, setCreateIn] = useState<{ category: ChannelCategory | null } | null>(null);
  // Edit mode (#2031): admins arrange the list; nothing opens a channel meanwhile.
  const [editing, setEditing] = useState(false);
  const [categorySheet, setCategorySheet] = useState<CategorySheetMode | null>(null);
  const [dragging, setDragging] = useState(false);
  const moveChannel = useMoveChannel();
  const moveCategory = useMoveCategory();
  useEffect(() => {
    setEditing(false);
  }, [groupId]);

  const { data: groups = [] } = useUserGroupsWithChannels();
  const group = groups.find((g) => g.id === groupId);
  const channelsQuery = useGroupChannels(groupId);
  const channels = channelsQuery.data ?? group?.channels ?? [];
  const membersQuery = useGroupMembers(groupId);
  // An empty roster means not loaded yet: show no count rather than "0 members".
  const members = membersQuery.data ?? [];
  const membersKnown = members.length > 0;
  const refetchMembers = membersQuery.refetch;
  const [rosterRetries, setRosterRetries] = useState(0);
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
  // The testID gains `-muted` so Maestro can see the state.
  const { data: mutedChannels } = useMutedChannels();
  const me = appStore.currentUser?.id;

  const myRole = members.find((m) => m.user_id === me)?.role ?? group?.current_user_role;
  const isAdmin = myRole === "admin" || myRole === "owner";
  const groupName = group?.name ?? t("group.common.fallbackName");
  const memberCountLabel = membersKnown
    ? t("group.detail.memberCount", { count: members.length })
    : null;
  const channelNames = useMemo(() => channels.map((c) => c.name.toLowerCase()), [channels]);
  const categories = group?.categories ?? [];
  const allChannels = group?.allChannels ?? channels;
  const arranging = editing && isAdmin;

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
        {/* The fixed header acts on touch-down (#1529); channel rows stay on release. */}
        <PressInPressable
          pressIn
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
          {/* Reserved while members load, so the header doesn't grow when the count lands (#1733). */}
          <Text style={[ty.meta, { fontSize: typeSize.section }]}>{memberCountLabel ?? "\u00a0"}</Text>
        </PressInPressable>
        {isAdmin ? (
          <>
            <Button
              testID={arranging ? "btn-edit-channels-done" : "btn-edit-channels"}
              variant={arranging ? "primary" : "subtle"}
              accessibilityLabel={arranging ? t("channels:categories.done") : t("channels:categories.editChannels")}
              onPress={() => setEditing(!arranging)}
            >
              {arranging ? t("channels:categories.done") : t("channels:categories.edit")}
            </Button>
            <IconButton
              testID="btn-create-channel"
              filled
              accessibilityLabel={t("group.panel.createChannel")}
              icon={<Icon.plus size={20} color={semantic.text} />}
              onPress={() => setCreateIn({ category: null })}
            />
          </>
        ) : null}
        <IconButton
          testID="row-group-invite"
          pressIn
          filled
          accessibilityLabel={t("group.panel.invite")}
          icon={<Icon.userPlus size={20} color={semantic.text} />}
          onPress={() => router.push({ pathname: "/group/invite", params: { groupId } })}
        />
      </View>

      <View style={{ paddingHorizontal: space.xxl, paddingBottom: space.lg }}>
        <PressInPressable
          pressIn
          testID="btn-search-group"
          accessibilityRole="button"
          accessibilityLabel={t("group.panel.search", { name: groupName })}
          onPress={() =>
            rootRouter.navigate({
              pathname: "/(tabs)/self",
              params: { search: "1", searchGroup: groupId },
            })
          }
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
          <Text numberOfLines={1} style={{ flex: 1, fontFamily: fonts.regular, fontSize: typeSize.button, color: semantic.muted }}>
            {t("group.panel.search", { name: groupName })}
          </Text>
        </PressInPressable>
      </View>
      <Divider />

      <ScrollView
        style={{ flex: 1 }}
        scrollEnabled={!dragging}
        contentContainerStyle={{
          paddingTop: space.lg,
          paddingHorizontal: space.lg,
          paddingBottom: space.xxxl + bottomInset,
          gap: 2,
        }}
      >
        {top}

        {channelsQuery.isLoading && channels.length === 0 ? (
          <Text style={[ty.secondary, { paddingStart: space.md, paddingVertical: space.sm }]}>
            {t("group.detail.loadingChannels")}
          </Text>
        ) : null}
        {!channelsQuery.isLoading && channels.length === 0 && categories.length === 0 && !arranging ? (
          <EmptyState testID="group-panel-no-channels">{t("group.common.noChannels")}</EmptyState>
        ) : null}
        <GroupChannelList
          channels={channels}
          allChannels={allChannels}
          categories={categories}
          editing={arranging}
          selectedChannelId={selectedChannelId}
          mutedChannels={mutedChannels}
          onOpenChannel={onOpenChannel}
          onPrefetch={(id) => prefetch(id, "channel")}
          onNewChannelIn={(category) => setCreateIn({ category })}
          onCategoryMenu={(category, channelCount) => setCategorySheet({ kind: "menu", category, channelCount })}
          onNewCategory={() => setCategorySheet({ kind: "create" })}
          onMoveChannel={(channelId, categoryId, position) =>
            moveChannel.mutate({ groupId, channelId, categoryId, position })
          }
          onMoveCategory={(categoryId, position) => moveCategory.mutate({ groupId, categoryId, position })}
          onDraggingChange={setDragging}
        />
        {moveChannel.isError || moveCategory.isError ? (
          <Text testID="text-arrange-error" style={[ty.secondary, { color: semantic.danger, paddingStart: space.sm, paddingTop: space.sm }]}>
            {t("channels:categories.moveFailed")}
          </Text>
        ) : null}

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
      {createIn ? (
        <CreateChannelSheet
          groupId={groupId}
          existingNames={channelNames}
          category={createIn.category}
          onClose={() => setCreateIn(null)}
          onCreated={onOpenChannel}
        />
      ) : null}
      {categorySheet ? (
        <CategorySheet groupId={groupId} mode={categorySheet} onClose={() => setCategorySheet(null)} />
      ) : null}
    </View>
  );
}

export const GroupPanel = observer(GroupPanelImpl);
