import { View, Pressable, Text } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { observer } from "mobx-react-lite";
import { palette, semantic, fonts } from "../theme/tokens";
import { useTheme } from "./theme";
import { Icon } from "./icons";
import { appStore } from "../stores/appStore";
import { useDMChannels, useUserGroupsWithChannels } from "../hooks/queries";

const TABS: {
  name: string;
  label: string;
  glyph: (c: string) => React.ReactNode;
}[] = [
  {
    name: "groups",
    label: "Groups",
    glyph: (c) => <Icon.diamond size={16} color={c} />,
  },
  { name: "direct", label: "Direct", glyph: (c) => <Icon.at size={16} color={c} /> },
  {
    name: "search",
    label: "Search",
    glyph: (c) => <Icon.search size={16} color={c} />,
  },
  { name: "self", label: "Self", glyph: (c) => <Icon.user size={16} color={c} /> },
];

export const TabBar = observer(function TabBar({ state, navigation }: any) {
  useTheme();
  const insets = useSafeAreaInsets();
  // Per-tab unread badge: sum the store's unread counts over the ids each tab
  // lists. The same cached queries the tabs render from split the id space
  // into channels (Groups) and DM conversations (Direct). Read `unreadCounts`
  // directly (not via a store method — MobX actions run untracked, so a
  // method call from render would not subscribe this observer).
  const unreadCounts = appStore.unreadCounts;
  const { data: groups = [] } = useUserGroupsWithChannels();
  const { data: dms = [] } = useDMChannels();
  const sumUnread = (items: { id: string }[]) =>
    items.reduce((sum, x) => sum + (unreadCounts[x.id] ?? 0), 0);
  const badgeByTab: Record<string, number> = {
    groups: sumUnread(groups.flatMap((g) => g.channels)),
    direct: sumUnread(dms),
  };
  return (
    <View
      style={{
        flexDirection: "row",
        height: 70 + insets.bottom,
        paddingBottom: insets.bottom,
        borderTopWidth: 1,
        borderTopColor: semantic.hair,
        backgroundColor: palette.bg,
      }}
    >
      {TABS.map((tab, i) => {
        const focused = state.index === i;
        const color = focused ? semantic.accent : semantic.mute;
        return (
          <Pressable
            key={tab.name}
            onPress={() => navigation.navigate(tab.name)}
            testID={`tab-${tab.name}`}
            accessibilityRole="tab"
            accessibilityLabel={tab.label}
            accessibilityState={{ selected: focused }}
            style={{
              flex: 1,
              alignItems: "center",
              justifyContent: "center",
              gap: 4,
            }}
          >
            {focused && (
              <View
                style={{
                  position: "absolute",
                  top: 0,
                  width: 22,
                  height: 2,
                  backgroundColor: semantic.accent,
                }}
              />
            )}
            <View
              style={{
                width: 22,
                height: 22,
                alignItems: "center",
                justifyContent: "center",
              }}
            >
              {tab.glyph(color)}
              {(badgeByTab[tab.name] ?? 0) > 0 ? (
                <View
                  testID={`badge-${tab.name}`}
                  style={{
                    position: "absolute",
                    top: -4,
                    right: -10,
                    minWidth: 15,
                    height: 15,
                    paddingHorizontal: 3,
                    borderRadius: 8,
                    backgroundColor: semantic.accent,
                    alignItems: "center",
                    justifyContent: "center",
                  }}
                >
                  <Text
                    style={{
                      fontFamily: fonts.sora600,
                      fontSize: 9,
                      color: palette.bg,
                    }}
                  >
                    {Math.min(badgeByTab[tab.name], 99)}
                  </Text>
                </View>
              ) : null}
            </View>
            <Text
              style={{
                fontFamily: fonts.sora500,
                fontSize: 10,
                letterSpacing: 1.4,
                textTransform: "uppercase",
                color,
              }}
            >
              {tab.label}
            </Text>
          </Pressable>
        );
      })}
    </View>
  );
});
