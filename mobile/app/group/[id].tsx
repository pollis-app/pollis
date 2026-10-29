import { useCallback, useEffect } from "react";
import { useFocusEffect, useRouter, useLocalSearchParams, type Href } from "expo-router";
import { observer } from "mobx-react-lite";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { Screen, Header } from "../../components/ui";
import { GroupPanel } from "../../components/groups/GroupPanel";
import { appStore } from "../../stores/appStore";
import { markConversationRead } from "../../hooks/queries";
import { useIsRegular } from "../../hooks/useLayoutClass";
import { useThemeRerender } from "../../components/theme";

// A single group's page on phones, reached by route.
function GroupDetail() {
  const router = useRouter();
  const { id } = useLocalSearchParams<{ id: string }>();
  const groupId = id ?? null;
  const insets = useSafeAreaInsets();

  // On this list no conversation is open: clear the selection so realtime counts unread again.
  useFocusEffect(
    useCallback(() => {
      appStore.setSelectedChannelId(null);
    }, []),
  );

  // No bottom safe-area edge: the panel pads its own list by the inset.
  return (
    <Screen testID="screen-group" aboveTabBar wide>
      {/* Back-only bar; with nothing to pop it goes to the Groups tab. */}
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
            markConversationRead(c.id);
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

// Regular width shows a group in the Groups tab's two-pane, never this page.
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
  useThemeRerender();
  const regular = useIsRegular();
  return regular ? <GroupRedirect /> : <GroupDetail />;
}

export default observer(GroupRoute);
