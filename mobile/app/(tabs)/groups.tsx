import { useCallback, useEffect, useMemo, useState } from "react";
import { Text, View } from "react-native";
import { useFocusEffect, useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { Screen, Header, Body, Button } from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty, space } from "../../theme/tokens";
import {
  useUserGroupsWithChannels,
  usePendingGroupInvites,
  useAdminPendingJoinRequestCounts,
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

// The Groups tab (Main.dc.html): a strip of group pills, then the selected
// group's channel panel. Picking a pill switches the group in place; the
// choice is remembered across launches.
function Groups() {
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
  // On regular (iPad) width the strip + panel are the left column of a
  // two-pane master-detail; on compact they are the whole screen.
  const isRegular = useLayoutClass() === "regular";
  // The right pane's own page stack (channel info, members, settings, a
  // thread…) on regular width. A different group or conversation starts it
  // over — whoever changed the selection (this list, Search, a deep link).
  const pane = usePaneStack();
  const resetPane = pane.api.reset;
  useEffect(() => {
    resetPane();
  }, [selectedGroupId, selectedChannelId, resetPane]);

  // The group last picked here, from a previous launch.
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

  // The shown group: the app-wide selection (set by opening a channel or a
  // group anywhere), else the remembered one, else the first.
  const current =
    groups.find((g) => g.id === selectedGroupId) ??
    groups.find((g) => g.id === storedGroupId) ??
    groups[0] ??
    null;

  // On compact, being on this list means no conversation is open — clear the
  // selection so realtime `new_message` events for the conversation the user
  // just left count as unread again. On regular the detail pane keeps the
  // conversation visible, so the selection (and unread suppression) stands.
  useFocusEffect(
    useCallback(() => {
      if (!isRegular) {
        appStore.setSelectedChannelId(null);
      }
    }, [isRegular]),
  );

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
    // Opening a conversation clears its unread count (desktop does the same
    // in its Channel page).
    appStore.markRead(c.id);
    // Re-opening the channel already shown closes any page over it.
    resetPane();
    // On regular the right pane updates in place; on compact push the chat.
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

  // Regular width: list + conversation side by side while there is a group to
  // pick from; with none (or still loading) the empty state is one centred
  // column — a two-pane would squeeze its call to action into the list column
  // beside a "Select a conversation" that has nothing to select.
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
