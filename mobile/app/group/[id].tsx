import { useCallback } from "react";
import { useFocusEffect, useRouter, useLocalSearchParams } from "expo-router";
import { observer } from "mobx-react-lite";
import { Screen, Header } from "../../components/ui";
import { GroupPanel } from "../../components/groups/GroupPanel";
import { appStore } from "../../stores/appStore";

// A single group's page, reached by route (new group, invite links, deep
// links, search results). Renders the same panel the Groups tab shows under
// its pill strip, under a top back bar.
function GroupDetail() {
  const router = useRouter();
  const { id } = useLocalSearchParams<{ id: string }>();
  const groupId = id ?? null;

  // Being on this list means no conversation is open — clear the channel
  // selection so realtime `new_message` events for the channel the user just
  // left count as unread again (same rule as the tab lists).
  useFocusEffect(
    useCallback(() => {
      appStore.setSelectedChannelId(null);
    }, []),
  );

  return (
    <Screen testID="screen-group">
      {/* Back-only bar: the group panel below carries the title. With
          nothing to pop (cold deep link / invite) back goes to the Groups tab. */}
      <Header
        bordered={false}
        onBack={() => {
          if (router.canGoBack()) {
            router.back();
          } else {
            router.replace("/(tabs)/groups");
          }
        }}
      />
      {groupId ? (
        <GroupPanel
          groupId={groupId}
          onOpenChannel={(c) => {
            appStore.setSelectedGroupId(groupId);
            appStore.setSelectedChannelId(c.id);
            // Opening a conversation clears its unread count.
            appStore.markRead(c.id);
            router.push({
              pathname: "/chat/[id]",
              params: { id: c.id, kind: "channel", name: c.name },
            });
          }}
        />
      ) : null}
    </Screen>
  );
}

export default observer(GroupDetail);
