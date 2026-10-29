import { useCallback, useEffect, useMemo, useState } from "react";
import { Text, View } from "react-native";
import { useFocusEffect, useIsFocused, useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { Screen, Header, Body, Button } from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty, space } from "../../theme/tokens";
import {
  useUserGroupsWithChannels,
  usePendingGroupInvites,
  useAdminPendingJoinRequestCounts,
  markConversationRead,
} from "../../hooks/queries";
import { appStore } from "../../stores/appStore";
import { useLayoutClass } from "../../hooks/useLayoutClass";
import {
  TwoPane,
  DetailPlaceholder,
  DetailPane,
  PaneProvider,
  CenteredColumn,
} from "../../components/MasterDetail";
import { usePaneStack } from "../../components/pane/paneContext";
import { GroupPills } from "../../components/groups/GroupPills";
import { GroupPanel } from "../../components/groups/GroupPanel";
import { PendingRows } from "../../components/groups/PendingRows";
import { AddGroupSheet } from "../../components/groups/AddGroupSheet";
import { readLastGroupId, writeLastGroupId } from "../../components/groups/lastGroup";
import type { Channel } from "../../types";
import { ChatView } from "../chat/[id]";
import { useThemeRerender } from "../../components/theme";

// The Groups tab: group pills, then the selected group's channel panel.
function Groups() {
  useThemeRerender();
  const { t } = useTranslation("mobile");
  const router = useRouter();
  const { data: groups = [], isLoading, isError } = useUserGroupsWithChannels();
  const { data: invites = [] } = usePendingGroupInvites();
  const adminGroupIds = useMemo(
    () => groups.filter((g) => g.current_user_role === "admin").map((g) => g.id),
    [groups],
  );
  const pendingByGroup = useAdminPendingJoinRequestCounts(adminGroupIds);
  const selectedGroupId = appStore.selectedGroupId;
  const selectedChannelId = appStore.selectedChannelId;
  const unreadCounts = appStore.unreadCounts;
  const me = appStore.currentUser?.id ?? null;
  const [addOpen, setAddOpen] = useState(false);
  const isRegular = useLayoutClass() === "regular";
  // The right pane's page stack; a different selection starts it over.
  const pane = usePaneStack();
  const resetPane = pane.api.reset;
  useEffect(() => {
    resetPane();
  }, [selectedGroupId, selectedChannelId, resetPane]);

  const [storedGroupId, setStoredGroupId] = useState<string | null>(null);
  useEffect(() => {
    if (!me) {
      return;
    }
    let live = true;
    readLastGroupId(me).then((id) => {
      if (live) {
        setStoredGroupId(id);
      }
    });
    return () => {
      live = false;
    };
  }, [me]);

  const current =
    groups.find((g) => g.id === selectedGroupId) ??
    groups.find((g) => g.id === storedGroupId) ??
    groups[0] ??
    null;

  // On compact, this list means nothing is open: clear the selection so realtime counts unread again.
  useFocusEffect(
    useCallback(() => {
      if (!isRegular) {
        appStore.setSelectedChannelId(null);
      }
    }, [isRegular]),
  );

  // Regular width: auto-open the first channel, only while focused, and only once the list knows the group.
  const focused = useIsFocused();
  const autoChannel =
    isRegular &&
    focused &&
    !selectedChannelId &&
    current &&
    (selectedGroupId === null || selectedGroupId === current.id)
      ? (current.channels[0] ?? null)
      : null;
  const autoGroupId = current?.id ?? null;
  useEffect(() => {
    if (!autoChannel || !autoGroupId) {
      return;
    }
    if (appStore.selectedGroupId !== autoGroupId) {
      appStore.setSelectedGroupId(autoGroupId);
    }
    appStore.setSelectedChannelId(autoChannel.id);
    markConversationRead(autoChannel.id);
  }, [autoChannel, autoGroupId]);

  const selectGroup = (id: string) => {
    resetPane();
    if (id !== appStore.selectedGroupId) {
      appStore.setSelectedGroupId(id);
    }
    setStoredGroupId(id);
    if (me) {
      writeLastGroupId(me, id);
    }
  };

  const openChannel = (groupId: string, c: Channel) => {
    selectGroup(groupId);
    appStore.setSelectedChannelId(c.id);
    markConversationRead(c.id);
    resetPane();
    if (!isRegular) {
      router.push({
        pathname: "/chat/[id]",
        params: { id: c.id, kind: "channel", name: c.name },
      });
    }
  };

  const pills = groups.map((g) => ({
    id: g.id,
    name: g.name,
    unread: g.channels.some((c) => (unreadCounts[c.id] ?? 0) > 0),
  }));

  const pending = (
    <PendingRows groups={groups} pendingByGroup={pendingByGroup} invites={invites} />
  );

  let column: React.ReactNode;
  if (current) {
    column = (
      <View style={{ flex: 1 }}>
        <GroupPills
          groups={pills}
          selectedId={current.id}
          onSelect={selectGroup}
          onAdd={() => setAddOpen(true)}
        />
        <GroupPanel
          testID="panel-group"
          groupId={current.id}
          selectedChannelId={isRegular ? selectedChannelId : null}
          onOpenChannel={(c) => openChannel(current.id, c)}
          top={pending}
        />
      </View>
    );
  } else {
    column = (
      <>
        <Header variant="large" title={t("groups.title")} />
        <Body contentContainerStyle={{ paddingHorizontal: space.xxl, gap: space.xxl }}>
          {isLoading ? (
            <Text style={ty.secondary}>{t("groups.loading")}</Text>
          ) : null}
          {isError ? (
            <Text style={[ty.secondary, { color: semantic.text }]}>
              {t("channels:groups.loadFailed")}
            </Text>
          ) : null}
          {pending}
          {!isLoading && !isError ? (
            <View style={{ gap: space.sm }}>
              <Text accessibilityRole="header" style={ty.title}>
                {t("groups.emptyTitle")}
              </Text>
              <Text style={ty.secondary}>{t("groups.emptyBody")}</Text>
            </View>
          ) : null}
          {isLoading ? null : (
            <View style={{ gap: space.md }}>
              <Button
                testID="btn-create-group"
                variant="primary"
                full
                icon={<Icon.plus size={18} color={semantic.onAccent} />}
                onPress={() => router.push("/group/new")}
              >
                {t("groups.create")}
              </Button>
              <Button
                testID="btn-join-group"
                full
                icon={<Icon.search size={18} color={semantic.text} />}
                onPress={() => router.push("/group/discover")}
              >
                {t("groups.find")}
              </Button>
            </View>
          )}
        </Body>
      </>
    );
  }

  // Regular width with no group: one centred column, not a squeezed two-pane.
  let body: React.ReactNode = column;
  if (isRegular && current) {
    body = (
      <PaneProvider pane={pane}>
        <TwoPane
          list={column}
          detail={
            <DetailPane
              pane={pane}
              root={
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
          }
        />
      </PaneProvider>
    );
  } else if (isRegular) {
    body = <CenteredColumn>{column}</CenteredColumn>;
  }

  return (
    <Screen testID="screen-groups" aboveTabBar wide>
      {body}
      {addOpen ? <AddGroupSheet onClose={() => setAddOpen(false)} /> : null}
    </Screen>
  );
}

export default observer(Groups);
