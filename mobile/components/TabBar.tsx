import { View, Pressable, Text } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { observer } from "mobx-react-lite";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { semantic, fonts, layout } from "../theme/tokens";
import { useTheme } from "./theme";
import { Icon } from "./icons";
import { appStore } from "../stores/appStore";
import { useDMChannels, useUserGroupsWithChannels } from "../hooks/queries";

// Groups / Direct / Search / Self (Main.dc.html): 24pt icon over a 12pt
// label; active = accent, inactive = dim. The `tab-<name>` testIDs and the
// `badge-<name>` anchors are load-bearing in e2e flows.
const TABS: {
  name: string;
  label: (t: TFunction) => string;
  glyph: (c: string) => React.ReactNode;
}[] = [
  { name: "groups", label: (t) => t("tabs.groups"), glyph: (c) => <Icon.users size={24} color={c} /> },
  {
    name: "direct",
    label: (t) => t("tabs.direct"),
    glyph: (c) => <Icon.messageCircle size={24} color={c} />,
  },
  { name: "search", label: (t) => t("tabs.search"), glyph: (c) => <Icon.search size={24} color={c} /> },
  { name: "self", label: (t) => t("tabs.self"), glyph: (c) => <Icon.user size={24} color={c} /> },
];

export const TabBar = observer(function TabBar({ state, navigation }: any) {
  useTheme();
  const { t } = useTranslation("mobile");
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
      accessibilityRole="tabbar"
      style={{
        flexDirection: "row",
        height: layout.tabBar + insets.bottom,
        paddingBottom: insets.bottom,
        borderTopWidth: 1,
        borderTopColor: semantic.hairSoft,
        backgroundColor: semantic.panel,
      }}
    >
      {TABS.map((tab, i) => {
        const focused = state.index === i;
        const color = focused ? semantic.accent : semantic.dim;
        const label = tab.label(t);
        const count = badgeByTab[tab.name] ?? 0;
        return (
          <Pressable
            key={tab.name}
            onPress={() => navigation.navigate(tab.name)}
            testID={`tab-${tab.name}`}
            accessibilityRole="tab"
            accessibilityLabel={
              count > 0 ? t("tabs.withUnread", { label, count }) : label
            }
            accessibilityState={{ selected: focused }}
            style={{
              flex: 1,
              alignItems: "center",
              justifyContent: "center",
              gap: 3,
            }}
          >
            <View>
              {tab.glyph(color)}
              {count > 0 ? (
                <View
                  testID={`badge-${tab.name}`}
                  style={{
                    position: "absolute",
                    top: -4,
                    end: -10,
                    minWidth: 18,
                    height: 18,
                    paddingHorizontal: 4,
                    borderRadius: 9,
                    borderWidth: 2,
                    borderColor: semantic.panel,
                    backgroundColor: semantic.accent,
                    alignItems: "center",
                    justifyContent: "center",
                  }}
                >
                  <Text
                    maxFontSizeMultiplier={1.2}
                    style={{
                      fontFamily: fonts.bold,
                      fontSize: 11,
                      lineHeight: 13,
                      color: semantic.onAccent,
                    }}
                  >
                    {count > 99 ? "99+" : count}
                  </Text>
                </View>
              ) : null}
            </View>
            {/* The bar's height is fixed, so the label caps its scaling. */}
            <Text
              numberOfLines={1}
              maxFontSizeMultiplier={1.3}
              style={{
                fontFamily: focused ? fonts.semibold : fonts.medium,
                fontSize: 12,
                color,
              }}
            >
              {label}
            </Text>
          </Pressable>
        );
      })}
    </View>
  );
});
