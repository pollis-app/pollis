import { View, Text, Platform } from "react-native";
import { observer } from "mobx-react-lite";
import { useTranslation } from "react-i18next";
import { useRouter, type Href } from "expo-router";
import type { TFunction } from "i18next";
import { semantic, fonts, layout } from "../theme/tokens";
import { useTheme } from "./theme";
import { Icon } from "./icons";
import { useBottomInset, PressInPressable } from "./ui";
import { appStore } from "../stores/appStore";
import { emitTabReselect } from "../lib/tabReselect";
import {
  useDMChannels,
  useDMRequests,
  usePendingGroupInvites,
  useUserGroupsWithChannels,
} from "../hooks/queries";
import { badgeText, badgeTotal, tabA11yLabel, tabBadges } from "../lib/tabBadges";

// Groups / Direct / Link / Self. `tab-<name>` and `badge-<name>` are load-bearing in e2e. The shield (#1550) is a shortcut, never a tab.
const ICON = 22;
const TABS: {
  name: string;
  label: (t: TFunction) => string;
  glyph: (c: string) => React.ReactNode;
  push?: Href;
  a11yLabel?: (t: TFunction) => string;
}[] = [
  { name: "groups", label: (t) => t("tabs.groups"), glyph: (c) => <Icon.users size={ICON} color={c} /> },
  {
    name: "direct",
    label: (t) => t("tabs.direct"),
    glyph: (c) => <Icon.messageCircle size={ICON} color={c} />,
  },
  {
    name: "link-device",
    label: (t) => t("tabs.signInDevice"),
    a11yLabel: (t) => t("self.scanDevice.heading"),
    glyph: (c) => <Icon.shield size={ICON} color={c} />,
    push: { pathname: "/self/link-device", params: { from: "tab" } },
  },
  { name: "self", label: (t) => t("tabs.self"), glyph: (c) => <Icon.user size={ICON} color={c} /> },
];

export const TabBar = observer(function TabBar({ state, navigation }: any) {
  useTheme();
  const { t } = useTranslation("mobile");
  const router = useRouter();
  const bottomInset = useBottomInset();
  // Per-tab badge (#1561, lib/tabBadges.ts). Read `unreadCounts` directly: a MobX action call from render would not subscribe.
  const unreadCounts = appStore.unreadCounts;
  const { data: groups = [] } = useUserGroupsWithChannels();
  const { data: dms = [] } = useDMChannels();
  const { data: requests = [] } = useDMRequests();
  const { data: invites = [] } = usePendingGroupInvites();
  const badgeByTab = tabBadges({
    unreadCounts,
    channelIds: groups.flatMap((g) => g.channels.map((c) => c.id)),
    dmIds: dms.map((d) => d.id),
    requestCount: requests.length,
    inviteCount: invites.length,
  });
  return (
    <View
      // "tabbar" exists only on iOS; Android crashes on it.
      accessibilityRole={Platform.OS === "ios" ? "tabbar" : "tablist"}
      style={{
        flexDirection: "row",
        height: layout.tabBar + bottomInset,
        paddingBottom: bottomInset,
        borderTopWidth: 1,
        borderTopColor: semantic.hairSoft,
        backgroundColor: semantic.panel,
      }}
    >
      {TABS.map((tab) => {
        const route = tab.push
          ? undefined
          : state.routes.find((r: { name: string }) => r.name === tab.name);
        const focused = !!route && state.routes[state.index]?.key === route.key;
        const color = focused ? semantic.accent : semantic.dim;
        const label = tab.label(t);
        const spoken = tab.a11yLabel?.(t) ?? label;
        const badge = badgeByTab[tab.name];
        const count = badgeTotal(badge);
        const onPress = () => {
          if (tab.push) {
            router.push(tab.push);
            return;
          }
          if (!route) {
            return;
          }
          if (focused) {
            emitTabReselect(tab.name);
            return;
          }
          navigation.navigate(tab.name);
        };
        return (
          // Touch-down (#1529).
          <PressInPressable
            key={tab.name}
            pressIn
            onPress={onPress}
            testID={`tab-${tab.name}`}
            accessibilityRole={tab.push ? "button" : "tab"}
            accessibilityLabel={tabA11yLabel(t, tab.name, spoken, badge)}
            accessibilityState={tab.push ? undefined : { selected: focused }}
            style={{
              flex: 1,
              alignItems: "center",
              justifyContent: "flex-start",
              paddingTop: 10,
              gap: 3,
            }}
          >
            {focused ? (
              <View
                style={{
                  position: "absolute",
                  top: 0,
                  alignSelf: "center",
                  width: 26,
                  height: 3,
                  borderRadius: 1.5,
                  backgroundColor: semantic.accent,
                }}
              />
            ) : null}
            <View>
              {tab.glyph(color)}
              {count > 0 ? (
                <View
                  testID={`badge-${tab.name}`}
                  style={{
                    position: "absolute",
                    top: -5,
                    end: -11,
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
                    {badgeText(count)}
                  </Text>
                </View>
              ) : null}
            </View>
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
          </PressInPressable>
        );
      })}
    </View>
  );
});
