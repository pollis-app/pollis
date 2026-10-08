import { useCallback, useEffect } from "react";
import { useFocusEffect, useRouter, useLocalSearchParams, type Href } from "expo-router";
import { observer } from "mobx-react-lite";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { Screen, Header } from "../../components/ui";
import { GroupPanel } from "../../components/groups/GroupPanel";
import { appStore } from "../../stores/appStore";
import { useIsRegular } from "../../hooks/useLayoutClass";

// A single group's page on phones, reached by route (invite links, deep
// links, search results). Creating a group lands on the Groups tab instead. Renders the same panel the Groups tab shows under
// its pill strip, under a top back bar.
function GroupDetail() {
  const router = useRouter();
  const { id } = useLocalSearchParams<{ id: string }>();
  const groupId = id ?? null;
  const insets = useSafeAreaInsets();

  // Being on this list means no conversation is open — clear the channel
  // selection so realtime `new_message` events for the channel the user just
  // left count as unread again (same rule as the tab lists).
  useFocusEffect(
    useCallback(() => {
      appStore.setSelectedChannelId(null);
    }, []),
  );

  // No bottom safe-area edge: the panel runs to the screen's bottom edge
  // (no black strip under it) and pads its own list by the inset instead.
  return (
    <Screen testID="screen-group" aboveTabBar wide>
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
          bottomInset={insets.bottom}
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

// Regular width (iPad) shows a group in the Groups tab's two-pane, never as
// this standalone page: select it and return to the tab. Call sites use
// useOpenConversation().openGroup to go there directly; this is the net for
// anything that still lands here (e.g. a cold deep link).
function GroupRedirect() {
  const router = useRouter();
  const { id } = useLocalSearchParams<{ id: string }>();
  useEffect(() => {
    if (id && id !== appStore.selectedGroupId) {
      appStore.setSelectedGroupId(id);
    }
    router.dismissTo("/(tabs)/groups" as Href);
    // Once per group id.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id]);
  return <Screen testID="screen-group-redirect" wide>{null}</Screen>;
}

function GroupRoute() {
  const regular = useIsRegular();
  return regular ? <GroupRedirect /> : <GroupDetail />;
}

export default observer(GroupRoute);
