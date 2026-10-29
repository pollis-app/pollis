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
import { useDMChannels, useUserGroupsWithChannels } from "../hooks/queries";

// Groups / Direct / Link / Self (Main.dc.html): 22pt icon over a
// 12pt label, set 10pt below the bar's top hairline; active = accent icon and
// label, a bolder label, and a short rounded accent bar at the top of the
// cell (so where you are is not told by colour alone); inactive = dim. The
// `tab-<name>` testIDs and the `badge-<name>` anchors are load-bearing in
// e2e flows.
//
// The shield (#1550) is not a tab: it is a shortcut that pushes "Scan to
// sign in a device" (the same screen as Self → Security) onto the root
// stack, so back returns to whichever tab you were on. It is never the
// selected cell and is announced as a button. Search moved into Self.
const ICON = 22;
const TABS: {
  name: string;
  label: (t: TFunction) => string;
  glyph: (c: string) => React.ReactNode;
  // A shortcut entry: pushes this route instead of switching tabs.
  push?: Href;
  // Spoken label, when the visible one is shortened for the bar.
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
  // Home-indicator inset on iOS; on Android the nav-bar inset, floored so
  // the labels never sit on the glass edge (see useBottomInset).
  const bottomInset = useBottomInset();
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
      // "tabbar" exists only on iOS; Android throws "Invalid accessibility
      // role value: tabbar" and crashes the moment the tab bar mounts.
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
        // Matched by route name, not index: the shortcut cell has no route.
        const route = tab.push
          ? undefined
          : state.routes.find((r: { name: string }) => r.name === tab.name);
        const focused = !!route && state.routes[state.index]?.key === route.key;
        const color = focused ? semantic.accent : semantic.dim;
        const label = tab.label(t);
        const spoken = tab.a11yLabel?.(t) ?? label;
        const count = badgeByTab[tab.name] ?? 0;
        const onPress = () => {
          if (tab.push) {
            router.push(tab.push);
            return;
          }
          if (!route) {
            return;
          }
          // A second tap on the focused tab lets it react (Self ends a search).
          if (focused) {
            emitTabReselect(tab.name);
            return;
          }
          navigation.navigate(tab.name);
        };
        return (
          // Switching tabs, and opening the shortcut's page, are free to
          // undo, so they act on touch-down (#1529).
          <PressInPressable
            key={tab.name}
            pressIn
            onPress={onPress}
            testID={`tab-${tab.name}`}
            accessibilityRole={tab.push ? "button" : "tab"}
            accessibilityLabel={
              count > 0 ? t("tabs.withUnread", { label: spoken, count }) : spoken
            }
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
          </PressInPressable>
        );
      })}
    </View>
  );
});
